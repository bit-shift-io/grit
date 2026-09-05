//! HTTP handlers: tab-scoped git queries, file browsing, and health.

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::git::types::expand_tilde;
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

#[derive(Deserialize)]
pub(crate) struct FileTreeQuery {
    tab: usize,
    #[serde(default)]
    path: String,
}

#[derive(Deserialize)]
pub(crate) struct FileContentQuery {
    tab: usize,
    path: String,
    #[serde(default)]
    raw: bool,
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
pub(crate) async fn filetree_handler(
    State(app): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<FileTreeQuery>,
) -> (StatusCode, Json<Vec<crate::git::types::FileTreeEntry>>) {
    let dir = query.path;
    match tab_scoped_git_call(&app, query.tab, "filetree", move |repo_path| {
        crate::git::list_dir(&repo_path, &dir)
    })
    .await
    {
        Ok(entries) => (StatusCode::OK, Json(entries)),
        Err((_status, message)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(vec![crate::git::types::FileTreeEntry {
                name: message,
                path: String::new(),
                is_dir: false,
                depth: 0,
            }]),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct FileSearchQuery {
    tab: usize,
    q: String,
}

pub(crate) async fn filesearch_handler(
    State(app): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<FileSearchQuery>,
) -> (StatusCode, Json<Vec<crate::git::types::FileTreeEntry>>) {
    let q = query.q.clone();
    match tab_scoped_git_call(&app, query.tab, "filesearch", move |repo_path| {
        crate::git::search_files(&repo_path, &q, 200)
    })
    .await
    {
        Ok(entries) => (StatusCode::OK, Json(entries)),
        Err((_status, message)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(vec![crate::git::types::FileTreeEntry {
                name: message,
                path: String::new(),
                is_dir: false,
                depth: 0,
            }]),
        ),
    }
}

/// Returns a single file's preview content (text, image flag, binary flag),
/// or the raw bytes when `raw=true` (used by the browser to render images).
pub(crate) async fn filecontent_handler(
    State(app): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<FileContentQuery>,
) -> AxumResponse {
    if query.raw {
        let Some(repo_path) = app.registry.repo_path_for(query.tab) else {
            return (
                StatusCode::NOT_FOUND,
                "no repository tabs open".to_string(),
            )
                .into_response();
        };
        let Some(full) = crate::git::safe_join(&repo_path, &query.path) else {
            return (
                StatusCode::BAD_REQUEST,
                "path escapes the repository".to_string(),
            )
                .into_response();
        };
        match std::fs::read(&full) {
            Ok(bytes) => {
                let mime = infer_mime(&query.path);
                ([(axum::http::header::CONTENT_TYPE, mime)], bytes)
                    .into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read {}: {e}", query.path),
            )
                .into_response(),
        }
    } else {
        let path = query.path.clone();
        let content = match tab_scoped_git_call(
            &app,
            query.tab,
            "filecontent",
            move |repo_path| {
                Ok::<crate::git::types::FileContent, crate::git::GitError>(
                    crate::git::get_file_content(&repo_path, &path),
                )
            },
        )
        .await
        {
            Ok(c) => c,
            Err((_status, message)) => crate::git::types::FileContent {
                path: query.path,
                size: 0,
                is_binary: false,
                is_image: false,
                content: String::new(),
                error: message,
            },
        };
        Json(content).into_response()
    }
}

#[derive(Deserialize)]
pub(crate) struct AppsQuery {
    path: String,
}

pub(crate) async fn apps_handler(
    axum::extract::Query(query): axum::extract::Query<AppsQuery>,
) -> (StatusCode, Json<Vec<crate::git::AppEntry>>) {
    let mime = crate::git::mime_for_path(&query.path);
    let apps = crate::git::list_apps_for_mime(mime);
    (StatusCode::OK, Json(apps))
}

/// Best-effort content type for a raw file response.
fn infer_mime(path: &str) -> &'static str {
    crate::git::mime_for_path(path)
}

/// Renders a path with `$HOME` abbreviated to `~` for friendlier display.
fn shorten_path(path: &std::path::Path) -> String {
    shorten_path_with(std::env::var("HOME").ok().as_deref(), path)
}

fn shorten_path_with(home: Option<&str>, path: &std::path::Path) -> String {
    if let Some(home) = home {
        let home = PathBuf::from(home);
        if path == home.as_path() {
            return "~".to_string();
        }
        if let Ok(rest) = path.strip_prefix(&home) {
            let rest = rest.display().to_string();
            if !rest.is_empty() {
                return format!("~/{rest}");
            }
        }
    }
    path.display().to_string()
}

#[derive(Deserialize)]
pub(crate) struct BrowseQuery {
    path: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct BrowseEntry {
    name: String,
    path: String,
}

#[derive(Serialize)]
pub(crate) struct BrowseResponse {
    current: String,
    parent: Option<String>,
    entries: Vec<BrowseEntry>,
}

/// Lists subdirectories of a folder so the web UI can offer a path picker.
pub(crate) async fn browse_handler(
    axum::extract::Query(query): axum::extract::Query<BrowseQuery>,
) -> Json<BrowseResponse> {
    let requested = query.path.as_deref().map(expand_tilde);
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let projects_dir = home
        .as_ref()
        .and_then(|h| {
            for name in ["projects", "Projects"] {
                let p = h.join(name);
                if p.is_dir() {
                    return Some(p);
                }
            }
            None
        });
    let dir = requested
        .filter(|p| p.is_dir())
        .or(projects_dir)
        .or_else(|| home.clone().filter(|p| p.is_dir()))
        .unwrap_or_else(|| PathBuf::from("/"));

    let mut entries = Vec::new();
    if let Ok(read) = std::fs::read_dir(&dir) {
        let mut dirs: Vec<_> = read
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .collect();
        dirs.sort_by_key(|e| e.file_name());
        for e in dirs {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            entries.push(BrowseEntry {
                name,
                path: shorten_path(&e.path()),
            });
        }
    }

    let parent = if Some(&dir) == home.as_ref() {
        None
    } else {
        dir.parent().map(|p| shorten_path(p))
    };

    Json(BrowseResponse {
        current: shorten_path(&dir),
        parent,
        entries,
    })
}

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
    use std::path::Path;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::server::build_router;
    use crate::server::registry::TabRegistry;
    use crate::test_support::{app_for, init_repo};

    #[test]
    fn shorten_path_abbreviates_home() {
        let home = "/home/bronson";
        assert_eq!(
            shorten_path_with(Some(home), Path::new("/home/bronson/projects/grit")),
            "~/projects/grit"
        );
        assert_eq!(shorten_path_with(Some(home), Path::new("/home/bronson")), "~");
        assert_eq!(
            shorten_path_with(Some(home), Path::new("/var/log")),
            "/var/log"
        );
        assert_eq!(
            shorten_path_with(None, Path::new("/home/bronson")),
            "/home/bronson"
        );
    }

    #[tokio::test]
    async fn browse_endpoint_lists_directories_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("file.txt"), "x").unwrap();

        let app = AppState::new(TabRegistry::new());
        let router = build_router(app);

        let uri = format!("/browse?path={}", dir.path().display());
        let response = router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["current"], dir.path().display().to_string());
        let names: Vec<&str> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"subdir"), "got: {names:?}");
        assert!(!names.contains(&"file.txt"), "got: {names:?}");
    }

    #[tokio::test]
    async fn browse_endpoint_falls_back_to_projects_then_home() {
        let app = AppState::new(TabRegistry::new());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/browse?path=/nonexistent/does-not-exist")
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
        let current = json["current"].as_str().unwrap();
        // Falls back to ~/projects/Projects if exists, then $HOME, then /
        assert!(
            current == "~/projects"
                || current == "~/Projects"
                || current == std::env::var("HOME").unwrap_or_default()
                || current == "/",
            "got: {current}"
        );
    }

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

    #[tokio::test]
    async fn filetree_endpoint_lists_root_entries() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/sub/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        // Root listing: only immediate children
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/filetree?tab=0&path=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let entries: Vec<crate::git::types::FileTreeEntry> =
            serde_json::from_slice(&bytes).unwrap();

        let paths: Vec<(&str, bool)> = entries.iter().map(|e| (e.path.as_str(), e.is_dir)).collect();
        assert!(paths.contains(&("Cargo.toml", false)), "got: {paths:?}");
        assert!(paths.contains(&("src", true)), "got: {paths:?}");
        assert_eq!(paths.len(), 2, "root should have exactly 2 entries, got: {paths:?}");
    }

    #[tokio::test]
    async fn filetree_endpoint_lists_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/sub/lib.rs"), "pub fn f() {}\n").unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/filetree?tab=0&path=src")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let entries: Vec<crate::git::types::FileTreeEntry> =
            serde_json::from_slice(&bytes).unwrap();

        let paths: Vec<(&str, bool)> = entries.iter().map(|e| (e.path.as_str(), e.is_dir)).collect();
        assert!(paths.contains(&("src/main.rs", false)), "got: {paths:?}");
        assert!(paths.contains(&("src/sub", true)), "got: {paths:?}");
        assert_eq!(paths.len(), 2, "src/ should have exactly 2 entries, got: {paths:?}");
    }

    #[tokio::test]
    async fn filetree_endpoint_prevents_path_traversal_absolute() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/filetree?tab=0&path=/etc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let entries: Vec<crate::git::types::FileTreeEntry> =
            serde_json::from_slice(&bytes).unwrap();
        assert!(entries.is_empty(), "absolute path should return empty");
    }

    #[tokio::test]
    async fn filetree_endpoint_prevents_path_traversal_parent() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/filetree?tab=0&path=../etc")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let entries: Vec<crate::git::types::FileTreeEntry> =
            serde_json::from_slice(&bytes).unwrap();
        assert!(entries.is_empty(), "parent traversal should return empty");
    }

    #[tokio::test]
    async fn filecontent_endpoint_returns_text_and_flags_binary_images() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::write(dir.path().join("readme.txt"), "hello world\n").unwrap();
        std::fs::write(dir.path().join("note.md"), "# Title\n").unwrap();
        let _ = image_placeholder(&dir.path().join("pic.png"));

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let text = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/filecontent?tab=0&path=readme.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(text.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(text.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let tc: crate::git::types::FileContent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(tc.content, "hello world\n");
        assert!(!tc.is_binary);
        assert!(!tc.is_image);

        let img = router
            .oneshot(
                Request::builder()
                    .uri("/filecontent?tab=0&path=pic.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(img.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let ic: crate::git::types::FileContent = serde_json::from_slice(&bytes).unwrap();
        assert!(ic.is_image);
        assert_eq!(ic.content, "");
    }

    #[tokio::test]
    async fn filecontent_raw_serves_image_bytes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        let file = dir.path().join("pic.png");
        image_placeholder(&file).unwrap();
        let expected = std::fs::read(&file).unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/filecontent?tab=0&path=pic.png&raw=true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "image/png"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(bytes.as_ref(), expected.as_slice());
    }

    #[tokio::test]
    async fn filecontent_raw_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        let marker = dir.path().parent().unwrap().join("grit-traversal-probe.txt");
        std::fs::write(&marker, "must never leak").unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        for uri in [
            "/filecontent?tab=0&path=..%2Fgrit-traversal-probe.txt&raw=true",
            "/filecontent?tab=0&path=%2Fetc%2Fpasswd&raw=true",
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert!(
                response.status().is_client_error() || response.status().is_server_error(),
                "expected path traversal {uri} to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn filesearch_endpoint_finds_files_across_directories() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::create_dir_all(dir.path().join("src/sub")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("src/sub/lib.rs"), "pub fn f() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Hello\n").unwrap();

        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        // Search for ".rs" — should find both main.rs and sub/lib.rs
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/filesearch?tab=0&q=.rs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let entries: Vec<crate::git::types::FileTreeEntry> =
            serde_json::from_slice(&bytes).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"src/main.rs"), "got: {paths:?}");
        assert!(paths.contains(&"src/sub/lib.rs"), "got: {paths:?}");
        assert!(!paths.contains(&"Cargo.toml"), "should not match .toml: {paths:?}");
    }

    fn image_placeholder(path: &std::path::Path) -> std::io::Result<()> {
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
            0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
            0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
            0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
            0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        std::fs::write(path, PNG)
    }

}
