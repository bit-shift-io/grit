//! HTTP handlers: tab-scoped git queries, diffs, health, and the
//! folder-browser picker used by the "+ new tab" form.

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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

/// Turns a path into a display string, abbreviating the home directory to `~`.
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

/// Default directory the folder picker starts in (and where a bare,
/// relative folder name is resolved): `~/Projects` / `~/projects` when it
/// exists, otherwise `$HOME`. `None` when neither is available.
pub(crate) fn default_browse_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let projects = home.as_ref().and_then(|h| {
        ["projects", "Projects"]
            .iter()
            .map(|name| h.join(name))
            .find(|p| p.is_dir())
    });
    projects.or_else(|| home.filter(|p| p.is_dir()))
}

/// Lists subdirectories of a folder so the web UI can offer a path picker.
pub(crate) async fn browse_handler(
    axum::extract::Query(query): axum::extract::Query<BrowseQuery>,
) -> Json<BrowseResponse> {
    let requested = query.path.as_deref().map(expand_tilde);
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    let dir = requested
        .filter(|p| p.is_dir())
        .or_else(default_browse_dir)
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
        dir.parent().map(shorten_path)
    };

    Json(BrowseResponse {
        current: shorten_path(&dir),
        parent,
        entries,
    })
}

/// Builds the settings snapshot off the runtime thread: it reads up to three
/// config files from disk.
async fn settings_response(app: &AppState) -> crate::settings::SettingsResponse {
    let paths = app.settings_paths.clone();
    let control = app.control.clone();
    let baseline = app.baseline.clone();
    let ports = app.ports;
    match tokio::task::spawn_blocking(move || {
        crate::settings::snapshot(&paths, &control, &baseline, ports)
    })
    .await
    {
        Ok(response) => response,
        Err(e) => {
            // Only reachable if the task panicked; read inline rather than
            // failing a settings page that is otherwise fine.
            tracing::error!("settings snapshot task panicked: {e}");
            crate::settings::snapshot(
                &app.settings_paths,
                &app.control,
                &app.baseline,
                app.ports,
            )
        }
    }
}

/// Rejects a mutating request whose browser Origin is not allowed.
///
/// CORS is not a guard on its own: the browser still *sends* a "simple"
/// cross-site request and the effect happens — only the reply is hidden. So
/// every route that changes something checks the origin itself, exactly as the
/// WebSocket upgrade does. Native clients send no Origin and are unaffected.
fn mutation_origin_ok(app: &AppState, headers: &axum::http::HeaderMap) -> bool {
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    super::websocket::origin_is_allowed_for(
        origin,
        crate::settings::is_lan(&app.control.effective()),
    )
}

/// Current LAN settings for grit, folio and krust.
pub(crate) async fn settings_get_handler(
    State(app): State<AppState>,
) -> Json<crate::settings::SettingsResponse> {
    Json(settings_response(&app).await)
}

/// Applies the requested LAN toggles: writes each service's config file, and
/// moves grit's own listener without a restart.
pub(crate) async fn settings_post_handler(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(update): Json<crate::settings::SettingsUpdate>,
) -> Response {
    if !mutation_origin_ok(&app, &headers) {
        return (StatusCode::FORBIDDEN, "forbidden origin").into_response();
    }
    let paths = app.settings_paths.clone();
    let control = app.control.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::settings::apply(&paths, &update, &control)
    })
    .await;
    let status = match result {
        Ok(Ok(())) => StatusCode::OK,
        Ok(Err(e)) => {
            tracing::error!("failed to apply settings: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
        Err(e) => {
            tracing::error!("settings apply task panicked: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, Json(settings_response(&app).await)).into_response()
}

/// Restarts krust and folio so their freshly written config takes effect.
///
/// Reported per service: restarting krust kills every live terminal shell,
/// while restarting folio only blinks the files dock.
///
/// Takes a JSON body on purpose. Without one this is a "simple" request that a
/// browser will send cross-site without a preflight, which would let any page
/// the user has open kill their terminals; requiring a JSON content type means
/// the CORS preflight runs first.
pub(crate) async fn settings_restart_handler(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(_body): Json<serde_json::Value>,
) -> Response {
    if !mutation_origin_ok(&app, &headers) {
        return (StatusCode::FORBIDDEN, "forbidden origin").into_response();
    }
    let mut results = serde_json::Map::new();
    for service in crate::settings::Service::RESTARTABLE {
        let (ok, error) = match crate::settings::restart_service(&app, service).await {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        };
        results.insert(
            service.key().to_string(),
            serde_json::json!({ "ok": ok, "error": error }),
        );
    }
    Json(serde_json::Value::Object(results)).into_response()
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
    async fn health_endpoint_reports_ok() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_for(dir.path());
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
        init_repo(dir.path());
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

        let app = app_for(dir.path());
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
            init_repo(dir.path());
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
            revision: 0,
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
        init_repo(dir.path());
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

        let app = app_for(dir.path());
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

    async fn oneshot_json(router: axum::Router, uri: &str) -> serde_json::Value {
        use tower::ServiceExt;
        let response = router
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn oneshot_post_json(router: axum::Router, uri: &str, body: &str) -> serde_json::Value {
        use tower::ServiceExt;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "POST {uri}");
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn settings_post_rejects_a_foreign_origin() {
        // CORS only hides the *reply*: a page in the user's browser can still
        // fire a simple cross-site POST and have the effect happen. The
        // mutating routes therefore check the origin themselves.
        let paths = crate::settings::SettingsPaths {
            grit: None,
            krust: None,
            folio: None,
        };
        let app = AppState::new(TabRegistry::new()).with_settings(paths);
        let response = build_router(app)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/settings")
                    .header("content-type", "application/json")
                    .header("origin", "http://evil.example")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn settings_get_defaults_to_loopback_everywhere() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::settings::SettingsPaths {
            grit: Some(dir.path().join("grit.json")),
            krust: Some(dir.path().join("krust.json")),
            folio: Some(dir.path().join("folio.json")),
        };
        let app = AppState::new(TabRegistry::new()).with_settings(paths);

        let body = oneshot_json(build_router(app), "/settings").await;
        assert_eq!(body["grit"]["lan"], false);
        assert_eq!(body["krust"]["host"], "127.0.0.1");
        assert_eq!(body["krust"]["pending_restart"], false);
        assert_eq!(body["ports"]["krust"], 3000);
    }

    #[tokio::test]
    async fn settings_post_writes_the_config_and_reports_lan() {
        let dir = tempfile::tempdir().unwrap();
        let krust_path = dir.path().join("krust.json");
        std::fs::write(&krust_path, br#"{"port":3000,"allowed_origins":[]}"#).unwrap();
        let paths = crate::settings::SettingsPaths {
            grit: None,
            krust: Some(krust_path.clone()),
            folio: None,
        };
        let app = AppState::new(TabRegistry::new()).with_settings(paths);

        let body = oneshot_post_json(
            build_router(app),
            "/settings",
            r#"{"krust":true}"#,
        )
        .await;
        assert_eq!(body["krust"]["lan"], true);
        assert_eq!(body["krust"]["pending_restart"], true);
        assert_eq!(crate::settings::read_host(&krust_path), "0.0.0.0");
        // Fields grit does not own survive the write.
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&krust_path).unwrap()).unwrap();
        assert_eq!(value["port"], 3000);
    }

    #[tokio::test]
    async fn settings_post_on_grit_requests_a_live_rebind() {
        let dir = tempfile::tempdir().unwrap();
        let grit_path = dir.path().join("grit.json");
        let paths = crate::settings::SettingsPaths {
            grit: Some(grit_path.clone()),
            krust: None,
            folio: None,
        };
        let app = AppState::new(TabRegistry::new()).with_settings(paths);
        let control = app.control.clone();

        let body = oneshot_post_json(build_router(app), "/settings", r#"{"grit":true}"#).await;

        assert_eq!(control.desired(), "0.0.0.0");
        assert_eq!(body["grit"]["lan"], true);
        assert_eq!(crate::settings::read_host(&grit_path), "0.0.0.0");
    }
}
