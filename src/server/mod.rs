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
        .route("/commit", get(commit_handler))
        .route("/browse", get(browse_handler))
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

#[derive(Deserialize)]
struct CommitQuery {
    tab: usize,
    hash: String,
}

async fn commit_handler(
    State(app): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<CommitQuery>,
) -> (StatusCode, Json<crate::git::types::CommitSummary>) {
    let repo_path = app.registry.repo_path_for(query.tab);
    let Some(repo_path) = repo_path else {
        return (
            StatusCode::NOT_FOUND,
            Json(crate::git::types::CommitSummary {
                message: "no repository tabs open".to_string(),
                author: String::new(),
                timestamp: 0,
                files_changed: 0,
                insertions: 0,
                deletions: 0,
                files: Vec::new(),
            }),
        );
    };
    let hash = query.hash.clone();
    let result =
        tokio::task::spawn_blocking(move || crate::git::get_commit_summary(&repo_path, &hash)).await;
    match result {
        Ok(Ok(summary)) => (StatusCode::OK, Json(summary)),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::git::types::CommitSummary {
                message: e.to_string(),
                author: String::new(),
                timestamp: 0,
                files_changed: 0,
                insertions: 0,
                deletions: 0,
                files: Vec::new(),
            }),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::git::types::CommitSummary {
                message: format!("commit task panicked: {e}"),
                author: String::new(),
                timestamp: 0,
                files_changed: 0,
                insertions: 0,
                deletions: 0,
                files: Vec::new(),
            }),
        ),
    }
}

/// Expands a leading `~` in a user-supplied path to the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_with(std::env::var("HOME").ok().as_deref(), path)
}

fn expand_tilde_with(home: Option<&str>, path: &str) -> PathBuf {
    if path == "~" {
        return home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = home {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
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
struct BrowseQuery {
    path: Option<String>,
}

#[derive(Serialize)]
struct BrowseEntry {
    name: String,
    path: String,
}

#[derive(Serialize)]
struct BrowseResponse {
    current: String,
    parent: Option<String>,
    entries: Vec<BrowseEntry>,
}

/// Lists subdirectories of a folder so the web UI can offer a path picker.
async fn browse_handler(
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
    if repo_path.as_os_str().is_empty() {
        return;
    }
    let path = PathBuf::from(&repo_path);
    if !path.exists() || !path.join(".git").exists() {
        tracing::warn!("skipping refresh for tab {} with invalid repo path: {}", tab_id, repo_path.display());
        return;
    }
    let result =
        tokio::task::spawn_blocking(move || crate::git::get_repository_status(&path)).await;
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
    // Once every watcher is gone the channel closes and recv() would return
    // None instantly forever; disable that arm instead of busy-spinning.
    let mut refresh_open = true;
    loop {
        tokio::select! {
            res = refresh_rx.recv(), if refresh_open => match res {
                Some(()) => {
                    let before = app.registry.revision();
                    refresh_all(&app).await;
                    // A mutation landing mid-refresh already broadcast its
                    // own fresh frame; only publish when nothing changed
                    // during the run, so no stale frame can trail a close.
                    if app.registry.revision() == before {
                        let _ = app.broadcast.send(app.registry.snapshot());
                    }
                }
                None => refresh_open = false,
            },
            changed = registry_rx.changed() => {
                if changed.is_ok() {
                    let _ = app.broadcast.send(app.registry.snapshot());
                } else {
                    // Registry senders are gone; nothing can change anymore.
                    std::future::pending::<()>().await;
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

/// Keeps exactly one filesystem watcher alive per unique repository path
/// among the open tabs, spawning and retiring them as tabs are opened or
/// closed through any client (web UI, desktop GUI, or config restore).
///
/// Watchers cannot be one-shot at boot: tabs opened later would otherwise
/// never stream filesystem updates to connected clients.
async fn watch_reconciler(app: AppState, refresh_tx: mpsc::UnboundedSender<()>) {
    let mut watchers: std::collections::HashMap<PathBuf, notify::RecommendedWatcher> =
        std::collections::HashMap::new();
    let mut registry_rx = app.registry.subscribe();

    loop {
        // Collect the canonical paths that should currently be watched.
        let mut wanted: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for tab in app.registry.snapshot().tabs {
            if tab.repo_path.is_empty() {
                continue;
            }
            let path = PathBuf::from(&tab.repo_path);
            if !path.exists() || !path.join(".git").exists() {
                tracing::warn!("not watching tab {}: invalid repo path {}", tab.id, tab.repo_path);
                continue;
            }
            let key = std::fs::canonicalize(&path).unwrap_or(path);
            wanted.insert(key);
        }

        // Retire watchers whose repository no longer has an open tab;
        // dropping a watcher also ends its debouncer thread.
        watchers.retain(|path, _| wanted.contains(path));

        // Spawn watchers for newly opened repositories. A fresh watcher is
        // followed by a refresh kick so the tab's state is computed and
        // broadcast even before its first filesystem event arrives.
        for path in &wanted {
            if watchers.contains_key(path) {
                continue;
            }
            match crate::git::watcher::spawn_watcher(path.clone(), refresh_tx.clone()) {
                Ok(w) => {
                    tracing::debug!("watching {}", path.display());
                    watchers.insert(path.clone(), w);
                    let _ = refresh_tx.send(());
                }
                Err(e) => {
                    tracing::warn!("file watcher failed to start for {}: {e}", path.display())
                }
            }
        }

        if registry_rx.changed().await.is_err() {
            break;
        }
    }
}

/// Boots shared daemon infrastructure: app state, watchers, and initial refresh.
pub async fn boot(registry: TabRegistry) -> (AppState, mpsc::UnboundedReceiver<()>) {
    let app = AppState::new(registry);
    let (refresh_tx, refresh_rx) = mpsc::unbounded_channel::<()>();

    // Restore tabs from persistent storage only if registry is empty.
    if app.registry.snapshot().tabs.is_empty() {
        let restored = crate::shared_config::restore_web_state();
        // Rewrite the config so tabs pruned for dead paths vanish from disk too.
        crate::shared_config::persist_web_state(&restored);
        if !restored.tabs.is_empty() {
            app.registry
                .raise_next_id_floor(restored.tabs.iter().map(|t| t.id));
            app.registry.set(restored);
        }
    }

    // One watcher per open repository, kept in sync with the registry for
    // the lifetime of the process.
    tokio::spawn(watch_reconciler(app.clone(), refresh_tx.clone()));

    // Persist tabs on registry changes.
    let mut persist_rx = app.registry.subscribe();
    tokio::spawn(async move {
        while persist_rx.changed().await.is_ok() {
            crate::shared_config::persist_web_state(&persist_rx.borrow());
        }
    });

    // Refresh statuses in the background so clients can connect while git
    // commands are still running; each finished tab's update_state publish
    // flows through sync_loop to every client, so tabs appear one by one.
    tokio::spawn({
        let app = app.clone();
        async move {
            for tab in app.registry.snapshot().tabs {
                refresh_tab(&app, tab.id).await;
            }
        }
    });

    (app, refresh_rx)
}

/// Returns true when a Grit daemon answers /health on this port.
///
/// Sends a minimal HTTP/1.1 request over a raw TCP connection so no HTTP
/// client dependency is needed; verifies the 200 status line to avoid
/// mistaking unrelated local services for a Grit daemon.
pub async fn is_daemon_running(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let connect = tokio::net::TcpStream::connect(("127.0.0.1", port));
    let Ok(Ok(mut stream)) =
        tokio::time::timeout(std::time::Duration::from_millis(500), connect).await
    else {
        return false;
    };
    let request = format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(std::time::Duration::from_millis(500), stream.read(&mut buf))
        .await
        .map(|r| r.unwrap_or(0))
        .unwrap_or(0);
    buf[..n].starts_with(b"HTTP/1.1 200") || buf[..n].starts_with(b"HTTP/1.0 200")
}

/// Boots the full headless daemon: watcher, state sync, and HTTP server.
pub async fn run(registry: TabRegistry, port: u16) {
    let listener = match create_listener(port).await {
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

/// SO_REUSEADDR lets an immediate close-and-restart rebind the port even
/// while old client sockets still linger in TIME_WAIT.
pub(crate) async fn create_listener(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(1024)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

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

    #[test]
    fn expand_tilde_resolves_home_prefixes() {
        let home = "/home/bronson";
        assert_eq!(
            expand_tilde_with(Some(home), "~/projects/grit"),
            PathBuf::from("/home/bronson/projects/grit")
        );
        assert_eq!(expand_tilde_with(Some(home), "~"), PathBuf::from(home));
        assert_eq!(
            expand_tilde_with(Some(home), "/usr/local"),
            PathBuf::from("/usr/local")
        );
        assert_eq!(
            expand_tilde_with(None, "~/projects"),
            PathBuf::from("~/projects")
        );
        assert_eq!(
            expand_tilde_with(Some(home), "~other/x"),
            PathBuf::from("~other/x")
        );
    }

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
        assert_eq!(json["status"], "ok");
        assert_eq!(json["tab_count"], 1);
        assert_eq!(json["change_count"], 0);
    }

    #[tokio::test]
    async fn sync_loop_idles_when_refresh_channel_closes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        let app = app_for(&dir.path().to_path_buf());

        // Simulate every watcher dying: the refresh channel closes up-front.
        let (refresh_tx, refresh_rx) = mpsc::unbounded_channel::<()>();
        drop(refresh_tx);

        let mut bcast = app.broadcast.subscribe();
        tokio::spawn(sync_loop(app.clone(), refresh_rx));

        // A busy-spinning loop would starve this single-threaded runtime and
        // hang the test before the sleep ever completes.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The loop must still re-broadcast registry changes while idling.
        app.registry
            .set(crate::server::registry::WebState {
                active: 0,
                tabs: vec![],
            });
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), bcast.recv()).await;
        assert!(received.is_ok(), "sync_loop stopped responding to changes");
    }

    #[tokio::test]
    async fn daemon_probe_detects_running_and_closed_ports() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());
        let (_refresh_tx, refresh_rx) = mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app_for(&dir.path().to_path_buf()), refresh_rx);

        assert!(is_daemon_running(port).await);

        // A bound-then-dropped port must not answer.
        let dead = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);
        assert!(!is_daemon_running(dead_port).await);
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
    async fn full_daemon_streams_watcher_updates_and_dispatching_actions() {
        // Isolate config persistence so the test never touches the real
        // user configuration file.
        let cfg_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", cfg_dir.path());

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

        // The initial refresh now runs in the background; wait until the
        // first tab has been fully computed before asserting on it.
        let initial =
            recv_state_until(&mut ws, |s| !s.tabs.is_empty() && s.tabs[0].state.current_branch == "main")
                .await;
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

    #[tokio::test]
    async fn tabs_opened_after_boot_stream_filesystem_updates() {
        // Isolate config persistence so the test never touches the real
        // user configuration file.
        let cfg_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", cfg_dir.path());

        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Fresh machine: nothing is open (and thus watched) at boot.
        let (app, refresh_rx) = boot(TabRegistry::new()).await;
        let _server = run_server(listener, app.clone(), refresh_rx);

        let mut ws = connect_with_retry(&format!("ws://{addr}/ws")).await;
        let _initial = recv_state(&mut ws).await;

        // Open the repository through the normal web-UI NewTab flow.
        // Match on our repo path: a concurrent test sharing the process
        // env may leak foreign tabs into the restored config.
        let repo_path_string = dir.path().display().to_string();
        ws.send(Message::Text(
            format!(
                r#"{{"tab":null,"action":{{"NewTab":"{{\"name\":\"\",\"path\":\"{}\"}}"}}}}"#,
                repo_path_string
            )
            .into(),
        ))
        .await
        .unwrap();
        recv_state_until(&mut ws, |s| {
            s.tabs.iter().any(|t| t.repo_path == repo_path_string)
        })
        .await;

        // A file written after the tab exists must surface on its own.
        // Re-write until observed: the watcher is spawned asynchronously
        // after the registry change, so one single write could race it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "filesystem change in a post-boot tab was never broadcast"
            );
            std::fs::write(dir.path().join("late.txt"), "data").unwrap();
            let pred = |s: &WebState| {
                s.tabs
                    .iter()
                    .any(|t| t.repo_path == repo_path_string && t.state.changes.iter().any(|c| c.path == "late.txt"))
            };
            match tokio::time::timeout(
                std::time::Duration::from_millis(400),
                recv_state_until(&mut ws, pred),
            )
            .await
            {
                Ok(state) => {
                    let mine = state
                        .tabs
                        .iter()
                        .find(|t| t.repo_path == repo_path_string)
                        .expect("opened tab present");
                    assert!(
                        mine.state.changes.iter().any(|c| c.path == "late.txt"),
                        "got: {mine:?}"
                    );
                    break;
                }
                Err(_) => continue,
            }
        }
    }

    #[tokio::test]
    async fn close_last_tab_broadcasts_empty_and_stays_empty() {
        // Isolate config persistence so the test never touches the real
        // user configuration file.
        let cfg_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", cfg_dir.path());

        let dir = tempfile::tempdir().unwrap();
        init_repo(&dir.path().to_path_buf());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = TabRegistry::with_single_tab(
            0,
            "repo".to_string(),
            dir.path().to_path_buf(),
        );
        let (app, refresh_rx) = boot(registry).await;
        let _server = run_server(listener, app.clone(), refresh_rx);

        let mut ws = connect_with_retry(&format!("ws://{addr}/ws")).await;
        // Let the boot-time refresh finish so no late refresh broadcast can
        // arrive after the tab is closed below.
        let _initial = recv_state_until(&mut ws, |s| {
            !s.tabs.is_empty() && s.tabs[0].state.current_branch == "main"
        })
        .await;

        ws.send(Message::Text(r#"{"tab":0,"action":"CloseTab"}"#.into()))
            .await
            .unwrap();

        // Every broadcast within the hold window must have no tabs.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(700);
        let mut seen_empty = false;
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(120), ws.next()).await {
                Ok(Some(Ok(Message::Text(txt)))) => {
                    let s: WebState = serde_json::from_str(&txt).unwrap();
                    assert!(
                        s.tabs.is_empty(),
                        "broadcast after closing the last tab must be empty, got {txt}"
                    );
                    seen_empty = true;
                }
                Ok(None) => break,
                _ => {}
            }
        }
        assert!(seen_empty, "expected at least one post-close broadcast");
    }
}