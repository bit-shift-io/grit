//! HTTP handlers: tab-scoped git queries, diffs, and health.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::AppState;


#[derive(Serialize)]
pub struct HealthResponse {
    pub tab_count: usize,
    pub current_branch: String,
    pub change_count: usize,
}

#[derive(Deserialize)]
pub(crate) struct FilesQuery {
    tab: usize,
    path: String,
}

/// Shared shape of the tab-scoped detail endpoints: resolve the tab's
/// repository, run one blocking git call off-thread, and map each failure
/// tier onto (status code, message) for the caller's fallback payload.
async fn tab_scoped_git_call<T, F>(
    app: &AppState,
    tab: usize,
    op_name: &str,
    call: F,
) -> Result<T, (StatusCode, String)>
where
    F: FnOnce(std::path::PathBuf) -> Result<T, crate::git::GitError> + Send + 'static,
    T: serde::Serialize + Send + 'static,
{
    let Some(repo_path) = app.registry.repo_path_for(tab) else {
        return Err((
            StatusCode::NOT_FOUND,
            "no repository tabs open".to_string(),
        ));
    };
    match tokio::task::spawn_blocking(move || call(repo_path)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{op_name} task panicked: {e}"),
        )),
    }
}

pub(crate) async fn files_handler(
    State(app): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<FilesQuery>,
) -> (StatusCode, Json<crate::git::types::FilePair>) {
    let file_path = query.path.clone();
    match tab_scoped_git_call(&app, query.tab, "files", move |repo_path| {
        crate::git::get_file_pair(&repo_path, &file_path)
    })
    .await
    {
        Ok(pair) => (StatusCode::OK, Json(pair)),
        Err((status, message)) => (
            status,
            Json(crate::git::types::FilePair {
                original: message,
                current: String::new(),
            }),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct CommitQuery {
    tab: usize,
    hash: String,
}

pub(crate) async fn commit_handler(
    State(app): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<CommitQuery>,
) -> (StatusCode, Json<crate::git::types::CommitSummary>) {
    let hash = query.hash.clone();
    match tab_scoped_git_call(&app, query.tab, "commit", move |repo_path| {
        crate::git::get_commit_summary(&repo_path, &hash)
    })
    .await
    {
        Ok(summary) => (StatusCode::OK, Json(summary)),
        Err((status, message)) => (
            status,
            Json(crate::git::types::CommitSummary::error(message)),
        ),
    }
}

/// Lists the repository's tracked + untracked files as a flat tree.
pub(crate) async fn health_handler(State(app): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let state = app.registry.snapshot();
    let active_tab = state.tabs.get(state.active);
    (
        StatusCode::OK,
        Json(HealthResponse {
            tab_count: state.tabs.len(),
            current_branch: active_tab
                .map(|t| t.state.current_branch.clone())
                .unwrap_or_default(),
            change_count: active_tab.map(|t| t.state.changes.len()).unwrap_or(0),
        }),
    )
}



#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::server::build_router;
    use crate::server::registry::TabRegistry;
    use crate::test_support::{app_for, init_repo};

    #[tokio::test]
    async fn health_endpoint_reports_ok() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["tab_count"], 1);
        assert_eq!(json["change_count"], 0);
    }

    #[tokio::test]
    async fn files_endpoint_returns_file_pair() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::fs::write(dir.path().join("a.txt"), "v2\n").unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/files?tab=0&path=a.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let pair: crate::git::types::FilePair = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(pair.original, "v1\n", "got: {pair:?}");
        assert_eq!(pair.current, "v2\n", "got: {pair:?}");
    }

    #[tokio::test]
    async fn files_endpoint_scopes_diff_to_named_tab() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        for dir in [&dir1, &dir2] {
            init_repo(&dir.path().to_path_buf());
            std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
            std::process::Command::new("git")
                .args(["add", "-A"])
                .current_dir(dir.path())
                .output()
                .unwrap();
            std::process::Command::new("git")
                .args(["commit", "-q", "-m", "init"])
                .current_dir(dir.path())
                .output()
                .unwrap();
        }
        std::fs::write(dir1.path().join("f.txt"), "dir1\n").unwrap();
        std::fs::write(dir2.path().join("f.txt"), "dir2\n").unwrap();

        let registry = crate::server::registry::TabRegistry::new();
        registry.set(crate::server::registry::WebState {
            active: 0,
            tabs: vec![
                crate::server::registry::WebTab {
                    id: 0,
                    name: "one".to_string(),
                    repo_path: dir1.path().display().to_string(),
                    state: crate::git::types::RepoState::default(),
                    log: Vec::new(),
                },
                crate::server::registry::WebTab {
                    id: 1,
                    name: "two".to_string(),
                    repo_path: dir2.path().display().to_string(),
                    state: crate::git::types::RepoState::default(),
                    log: Vec::new(),
                },
            ],
        });
        let app = AppState::new(registry);
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/files?tab=1&path=f.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let pair: crate::git::types::FilePair = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(pair.original, "one\n", "got: {pair:?}");
        assert_eq!(pair.current, "dir2\n", "got: {pair:?}");
    }

    #[tokio::test]
    async fn commit_endpoint_returns_summary() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-q", "-m", "first commit"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let hash = crate::git::get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/commit?tab=0&hash={hash}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let summary: crate::git::types::CommitSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(summary.message, "first commit");
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 1);
        assert_eq!(summary.deletions, 0);
    }

}
