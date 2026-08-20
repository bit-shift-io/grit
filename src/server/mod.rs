//! Axum web server subsystem: shared state, routing, and sync loop.

pub mod registry;
pub mod static_files;
pub mod websocket;

use std::path::PathBuf;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::CorsLayer;

use crate::server::registry::{TabRegistry, WebState};

/// Shared application state for the Axum server.
#[derive(Clone)]
pub struct AppState {
    pub registry: TabRegistry,
    pub broadcast: broadcast::Sender<WebState>,
}

impl AppState {
    pub fn new(registry: TabRegistry) -> Self {
        let (broadcast, _) = broadcast::channel(128);
        Self { registry, broadcast }
    }
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub tab_count: usize,
    pub current_branch: String,
    pub change_count: usize,
}

/// Builds the Axum router with health check and WebSocket endpoints.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(websocket::ws_handler))
        .route("/files", get(files_handler))
        .route("/", get(static_files::serve_static))
        .route("/{*path}", get(static_files::serve_static))
        .with_state(state)
        .layer(CorsLayer::permissive())
}

#[derive(Deserialize)]
struct FilesQuery {
    tab: usize,
    path: String,
}

async fn files_handler(
    State(app): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<FilesQuery>,
) -> (StatusCode, Json<crate::git::types::FilePair>) {
    let repo_path = app.registry.repo_path_for(query.tab);
    let Some(repo_path) = repo_path else {
        return (
            StatusCode::NOT_FOUND,
            Json(crate::git::types::FilePair {
                original: "no repository tabs open".to_string(),
                current: String::new(),
            }),
        );
    };
    let file_path = query.path.clone();
    let result =
        tokio::task::spawn_blocking(move || crate::git::get_file_pair(&repo_path, &file_path)).await;
    match result {
        Ok(Ok(pair)) => (StatusCode::OK, Json(pair)),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::git::types::FilePair {
                original: e.to_string(),
                current: String::new(),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::git::types::FilePair {
                original: format!("files task panicked: {e}"),
                current: String::new(),
            }),
        ),
    }
}

async fn health_handler(State(app): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let state = app.registry.snapshot();
    let active_tab = state.tabs.get(state.active);
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            tab_count: state.tabs.len(),
            current_branch: active_tab
                .map(|t| t.state.current_branch.clone())
                .unwrap_or_default(),
            change_count: active_tab.map(|t| t.state.changes.len()).unwrap_or(0),
        }),
    )
}

/// Recomputes the state of one repository tab on a blocking task.
pub async fn refresh_tab(app: &AppState, tab_id: usize) {
    let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
        return;
    };
    let result =
        tokio::task::spawn_blocking(move || crate::git::get_repository_status(&repo_path)).await;
    match result {
        Ok(Ok(state)) => app.registry.update_state(tab_id, state),
        Ok(Err(e)) => tracing::error!("repository status refresh failed for tab {tab_id}: {e}"),
        Err(e) => tracing::error!("repository status task panicked: {e}"),
    }
}

/// Recomputes the state of every open repository tab.
pub async fn refresh_all(app: &AppState) {
    let tabs = app.registry.snapshot().tabs;
    for tab in tabs {
        refresh_tab(app, tab.id).await;
    }
}

/// Listens for file-watcher refresh events and registry changes, re-broadcasting
/// the latest `WebState` snapshot after each.
pub async fn sync_loop(app: AppState, mut refresh_rx: mpsc::UnboundedReceiver<()>) {
    let mut registry_rx = app.registry.subscribe();
    let _ = app.broadcast.send(app.registry.snapshot());
    loop {
        tokio::select! {
            _ = refresh_rx.recv() => {
                refresh_all(&app).await;
                let _ = app.broadcast.send(app.registry.snapshot());
            }
            changed = registry_rx.changed() => {
                if changed.is_ok() {
                    let _ = app.broadcast.send(app.registry.snapshot());
                }
            }
        }
    }
}

/// Runs the Axum server on the given listener until it fails.
pub fn run_server(
    listener: tokio::net::TcpListener,
    app: AppState,
    refresh_rx: mpsc::UnboundedReceiver<()>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::spawn(sync_loop(app.clone(), refresh_rx));
        if let Err(e) = axum::serve(listener, build_router(app).into_make_service()).await {
            tracing::error!("server error: {e}");
        }
    })
}

/// Boots shared daemon infrastructure: app state, watchers, and initial refresh.
pub async fn boot(registry: TabRegistry) -> (AppState, mpsc::UnboundedReceiver<()>) {
    let app = AppState::new(registry);
    let (refresh_tx, refresh_rx) = mpsc::unbounded_channel::<()>();

    for tab in app.registry.snapshot().tabs {
        let repo_path = PathBuf::from(&tab.repo_path);
        let watcher = match crate::git::watcher::spawn_watcher(repo_path, refresh_tx.clone()) {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!("file watcher failed to start for {}: {e}", tab.repo_path);
                None
            }
        };

        // Keep the watcher alive for the lifetime of the process.
        if let Some(watcher) = watcher {
            tokio::spawn(async move {
                let _keep_alive = watcher;
                std::future::pending::<()>().await;
            });
        }
    }

    refresh_all(&app).await;
    (app, refresh_rx)
}

/// Boots the full headless daemon: watcher, state sync, and HTTP server.
pub async fn run(registry: TabRegistry, port: u16) {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to bind 127.0.0.1:{port}: {e}");
            return;
        }
    };

    tracing::info!("Grit web daemon listening on http://127.0.0.1:{port}");
    let (app, refresh_rx) = boot(registry).await;
    let handle = run_server(listener, app, refresh_rx);
    handle.await.ok();
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures_util::stream::StreamExt;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    use tower::ServiceExt;

    use super::*;
    use crate::server::registry::TabRegistry;

    fn init_repo(dir: &PathBuf) {
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    fn app_for(path: &PathBuf) -> AppState {
        AppState::new(TabRegistry::with_single_tab(
            0,
            "repo".to_string(),
            path.clone(),
        ))
    }

    async fn connect_with_retry(url: &str) -> tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    > {
        for _ in 0..40 {
            if let Ok((ws, _)) = tokio_tungstenite::connect_async(url).await {
                return ws;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("failed to connect to {url}");
    }

    async fn recv_state(ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >) -> WebState {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for ws message"))
            .expect("stream closed")
            .expect("ws error");
        match msg {
            Message::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("expected text state message, got {other:?}"),
        }
    }

    async fn recv_state_until(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        mut pred: impl FnMut(&WebState) -> bool,
    ) -> WebState {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let state = recv_state(ws).await;
                if pred(&state) {
                    return state;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for matching state"))
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
        assert_eq!(json["status"], "ok");
        assert_eq!(json["tab_count"], 1);
        assert_eq!(json["change_count"], 0);
    }

    #[tokio::test]
    async fn ws_route_requires_websocket_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_for(&dir.path().to_path_buf());
        let router = build_router(app);

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/ws")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
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
                },
                crate::server::registry::WebTab {
                    id: 1,
                    name: "two".to_string(),
                    repo_path: dir2.path().display().to_string(),
                    state: crate::git::types::RepoState::default(),
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
    async fn full_daemon_streams_watcher_updates_and_dispatching_actions() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = TabRegistry::with_single_tab(
            0,
            "repo".to_string(),
            dir.path().to_path_buf(),
        );
        let (app, refresh_rx) = boot(registry).await;
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;

        let initial = recv_state(&mut ws).await;
        assert_eq!(initial.tabs[0].state.current_branch, "main");
        assert_eq!(initial.tabs[0].state.changes.len(), 1);
        assert_eq!(initial.tabs[0].state.changes[0].path, "a.txt");

        ws.send(Message::Text(r#"{"tab":0,"action":{"Stage":"a.txt"}}"#.into()))
            .await
            .unwrap();
        let staged = recv_state_until(&mut ws, |s| {
            s.tabs[0]
                .state
                .changes
                .iter()
                .any(|c| c.path == "a.txt" && c.is_staged)
        })
        .await;
        assert_eq!(staged.tabs[0].state.changes.len(), 1);

        ws.send(Message::Text(r#"{"tab":0,"action":{"Commit":"first commit"}}"#.into()))
            .await
            .unwrap();
        let committed = recv_state_until(&mut ws, |s| !s.tabs[0].state.history.is_empty()).await;
        assert_eq!(committed.tabs[0].state.history[0].message, "first commit");
        assert_eq!(committed.tabs[0].state.changes.len(), 0);

        std::fs::write(dir.path().join("b.txt"), "data").unwrap();
        let updated = recv_state_until(&mut ws, |s| {
            s.tabs[0]
                .state
                .changes
                .iter()
                .any(|c| c.path == "b.txt")
        })
        .await;
        assert_eq!(updated.tabs[0].state.changes.len(), 1);
        assert_eq!(updated.tabs[0].state.changes[0].path, "b.txt");
    }
}