//! Axum web server subsystem: shared state, routing, and sync loop.

pub mod static_files;
pub mod websocket;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc, RwLock};
use tower_http::cors::CorsLayer;

use crate::git::types::RepoState;

/// Shared application state for the Axum server.
#[derive(Clone)]
pub struct AppState {
    pub repo_path: PathBuf,
    pub state: Arc<RwLock<RepoState>>,
    pub broadcast: broadcast::Sender<RepoState>,
}

impl AppState {
    pub fn new(repo_path: PathBuf) -> Self {
        let (broadcast, _) = broadcast::channel(128);
        Self {
            repo_path,
            state: Arc::new(RwLock::new(RepoState::default())),
            broadcast,
        }
    }
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub repo_path: String,
    pub current_branch: String,
    pub change_count: usize,
}

/// Builds the Axum router with health check and WebSocket endpoints.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(websocket::ws_handler))
        .route("/", get(static_files::serve_static))
        .route("/{*path}", get(static_files::serve_static))
        .with_state(state)
        .layer(CorsLayer::permissive())
}

async fn health_handler(State(app): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    let repo = app.state.read().await;
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok",
            repo_path: app.repo_path.display().to_string(),
            current_branch: repo.current_branch.clone(),
            change_count: repo.changes.len(),
        }),
    )
}

/// Recomputes repository state on a blocking task and broadcasts it.
pub async fn refresh_state(app: &AppState) {
    let path = app.repo_path.clone();
    let result =
        tokio::task::spawn_blocking(move || crate::git::get_repository_status(&path)).await;
    match result {
        Ok(Ok(state)) => {
            *app.state.write().await = state.clone();
            let _ = app.broadcast.send(state);
        }
        Ok(Err(e)) => tracing::error!("repository status refresh failed: {e}"),
        Err(e) => tracing::error!("repository status task panicked: {e}"),
    }
}

/// Listens for file-watcher refresh events and re-broadcasts state after each.
pub async fn sync_loop(app: AppState, mut refresh_rx: mpsc::UnboundedReceiver<()>) {
    while refresh_rx.recv().await.is_some() {
        refresh_state(&app).await;
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

/// Boots shared daemon infrastructure: app state, watcher, and initial refresh.
pub async fn boot(repo_path: PathBuf) -> (AppState, mpsc::UnboundedReceiver<()>) {
    let app = AppState::new(repo_path.clone());
    let (refresh_tx, refresh_rx) = mpsc::unbounded_channel::<()>();

    let watcher = match crate::git::watcher::spawn_watcher(repo_path, refresh_tx) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("file watcher failed to start: {e}");
            None
        }
    };

    // Keep the watcher alive for the lifetime of the process.
    tokio::spawn(async move {
        let _keep_alive = watcher;
        std::future::pending::<()>().await;
    });

    refresh_state(&app).await;
    (app, refresh_rx)
}

/// Boots the full headless daemon: watcher, state sync, and HTTP server.
pub async fn run(repo_path: PathBuf, port: u16) {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to bind 127.0.0.1:{port}: {e}");
            return;
        }
    };

    tracing::info!("Grit web daemon listening on http://127.0.0.1:{port}");
    let (app, refresh_rx) = boot(repo_path).await;
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
    >) -> RepoState {
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
        mut pred: impl FnMut(&RepoState) -> bool,
    ) -> RepoState {
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
        let app = AppState::new(dir.path().to_path_buf());
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
        assert_eq!(json["change_count"], 0);
    }

    #[tokio::test]
    async fn ws_route_requires_websocket_upgrade() {
        let dir = tempfile::tempdir().unwrap();
        let app = AppState::new(dir.path().to_path_buf());
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
    async fn full_daemon_streams_watcher_updates_and_dispatching_actions() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (app, refresh_rx) = boot(dir.path().to_path_buf()).await;
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;

        let initial = recv_state(&mut ws).await;
        assert_eq!(initial.current_branch, "main");
        assert_eq!(initial.changes.len(), 1);
        assert_eq!(initial.changes[0].path, "a.txt");

        ws.send(Message::Text(r#"{"Stage":"a.txt"}"#.into()))
            .await
            .unwrap();
        let staged = recv_state_until(&mut ws, |s| {
            s.changes.iter().any(|c| c.path == "a.txt" && c.is_staged)
        })
        .await;
        assert_eq!(staged.changes.len(), 1);

        ws.send(Message::Text(r#"{"Commit":"first commit"}"#.into()))
            .await
            .unwrap();
        let committed = recv_state_until(&mut ws, |s| !s.history.is_empty()).await;
        assert_eq!(committed.history[0].message, "first commit");
        assert_eq!(committed.changes.len(), 0);

        std::fs::write(dir.path().join("b.txt"), "data").unwrap();
        let updated = recv_state_until(&mut ws, |s| {
            s.changes.iter().any(|c| c.path == "b.txt")
        })
        .await;
        assert_eq!(updated.changes.len(), 1);
        assert_eq!(updated.changes[0].path, "b.txt");
    }
}