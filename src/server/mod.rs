//! Axum web server subsystem: shared state, routing, and sync loop.

pub mod registry;
pub mod static_files;
pub mod websocket;
mod handlers;
pub(crate) use handlers::*;

use std::path::PathBuf;

use axum::routing::get;
use axum::Router;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::CorsLayer;

use crate::server::registry::{TabRegistry, WebState};

/// Capacity of the state-broadcast channel fanning frames out to every
/// connected client (slow clients lag rather than block publishers).
const BROADCAST_CAPACITY: usize = 128;

/// Per-operation deadline for the raw-TCP daemon probe on `/health`.
#[cfg(any(test, feature = "desktop"))]
const DAEMON_PROBE_TIMEOUT_MS: u64 = 500;

/// Listen backlog for the TCP listener (`socket.listen`).
const LISTEN_BACKLOG: u32 = 1024;

/// Shared application state for the Axum server.
#[derive(Clone)]
pub struct AppState {
    pub registry: TabRegistry,
    pub broadcast: broadcast::Sender<WebState>,
    /// Repo paths whose filesystem watchers must be dropped and respawned,
    /// sent after a Reclone replaces the directory on disk. The receiving
    /// end is owned by `watch_reconciler` when booted through [`boot`];
    /// sending into a dropped receiver is a harmless no-op.
    watcher_resets: mpsc::UnboundedSender<PathBuf>,
}

impl AppState {
    /// Test constructor; watcher resets are a daemon concern and simply go
    /// nowhere here. Production paths go through [`boot`] / `with_watcher_resets`.
    #[cfg(any(test, feature = "desktop"))]
    #[allow(dead_code)]
    pub fn new(registry: TabRegistry) -> Self {
        Self::with_watcher_resets(registry, mpsc::unbounded_channel().0)
    }

    pub(crate) fn with_watcher_resets(
        registry: TabRegistry,
        watcher_resets: mpsc::UnboundedSender<PathBuf>,
    ) -> Self {
        let (broadcast, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            registry,
            broadcast,
            watcher_resets,
        }
    }
}

/// Builds the Axum router with health check and WebSocket endpoints.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ws", get(websocket::ws_handler))
        .route("/files", get(files_handler))
        .route("/commit", get(commit_handler))
        .route("/browse", get(browse_handler))
        .route("/filetree", get(filetree_handler))
        .route("/filecontent", get(filecontent_handler))
        .route("/filesearch", get(filesearch_handler))
        .route("/apps", get(apps_handler))
        .route("/", get(static_files::serve_static))
        .route("/{*path}", get(static_files::serve_static))
        .with_state(state)
        .layer(CorsLayer::permissive())
}


/// Expands a leading `~` in a user-supplied path to the home directory.

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
                Some(()) => refresh_and_broadcast_if_quiet(&app).await,
                None => refresh_open = false,
            },
            changed = registry_rx.changed() => {
                if changed.is_err() {
                    // Registry senders are gone; nothing can change anymore.
                    std::future::pending::<()>().await;
                }
                let _ = app.broadcast.send(app.registry.snapshot());
            }
        }
    }
}

/// Refreshes every tab and re-broadcasts, unless a mutation landed while
/// the refresh was running: that mutation already broadcast its own fresh
/// frame, so publishing here would trail a close with a stale frame.
async fn refresh_and_broadcast_if_quiet(app: &AppState) {
    let before = app.registry.revision();
    refresh_all(app).await;
    if app.registry.revision() == before {
        let _ = app.broadcast.send(app.registry.snapshot());
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
/// never stream filesystem updates to connected clients. Reset requests
/// (e.g. after Reclone deleted and re-created a repository) drop the old
/// watcher so the next pass respawns one on the new directory inodes.
async fn watch_reconciler(
    app: AppState,
    refresh_tx: mpsc::UnboundedSender<()>,
    mut reset_rx: mpsc::UnboundedReceiver<PathBuf>,
) {
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

        tokio::select! {
            changed = registry_rx.changed() => {
                if changed.is_err() {
                    // Registry senders are gone; nothing can change anymore.
                    break;
                }
            }
            Some(reset) = reset_rx.recv() => {
                let key = std::fs::canonicalize(&reset).unwrap_or(reset);
                // Drop the stale watch; the loop above respawns it because
                // the canonical path is still in `wanted`.
                watchers.remove(&key);
            }
        }
    }
}

/// Boots shared daemon infrastructure: app state, watchers, and initial refresh.
pub async fn boot(registry: TabRegistry) -> (AppState, mpsc::UnboundedReceiver<()>) {
    let (reset_tx, reset_rx) = mpsc::unbounded_channel::<PathBuf>();
    let app = AppState::with_watcher_resets(registry, reset_tx);
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
    tokio::spawn(watch_reconciler(app.clone(), refresh_tx.clone(), reset_rx));

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
#[cfg(any(test, feature = "desktop"))]
pub async fn is_daemon_running(port: u16) -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let connect = tokio::net::TcpStream::connect(("127.0.0.1", port));
    let Ok(Ok(mut stream)) =
        tokio::time::timeout(std::time::Duration::from_millis(DAEMON_PROBE_TIMEOUT_MS), connect)
            .await
    else {
        return false;
    };
    let request = format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(
        std::time::Duration::from_millis(DAEMON_PROBE_TIMEOUT_MS),
        stream.read(&mut buf),
    )
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
            // Loud on stderr too: a silent exit here looks exactly like "the
            // web UI never comes up" from the browser side.
            eprintln!("error: failed to bind 127.0.0.1:{port}: {e}");
            eprintln!("       is another Grit daemon already running on this port?");
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
    socket.listen(LISTEN_BACKLOG)
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
    use crate::test_support::{
        app_for, commit_all, connect_with_retry, init_repo, recv_state, recv_state_until,
    };

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

        // The close echo itself proves delivery; a fixed window here raced
        // under suite-wide load because nothing else broadcasts once idle.
        let emptied = recv_state_until(&mut ws, |s| s.tabs.is_empty()).await;
        assert!(emptied.tabs.is_empty());

        // Every later broadcast within the hold window must stay empty.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(700);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(120), ws.next()).await {
                Ok(Some(Ok(Message::Text(txt)))) => {
                    let s: WebState = serde_json::from_str(&txt).unwrap();
                    assert!(
                        s.tabs.is_empty(),
                        "broadcast after closing the last tab must be empty, got {txt}"
                    );
                }
                Ok(None) => break,
                _ => {}
            }
        }
    }

    /// Reclone deletes and re-creates the repository directory, so the
    /// daemon must drop the stale watch and respawn one over the fresh
    /// clone — otherwise the tab silently stops streaming updates.
    #[tokio::test]
    async fn reclone_respawns_the_filesystem_watcher() {
        // Isolate config persistence so the test never touches the real
        // user configuration file.
        let cfg_dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", cfg_dir.path());

        // Bare origin seeded with one commit, then a working clone of it.
        let origin = tempfile::tempdir().unwrap();
        let bare = origin.path().join("origin.git");
        std::process::Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&bare)
            .output()
            .unwrap();
        let seed = tempfile::tempdir().unwrap();
        init_repo(&seed.path().to_path_buf());
        std::fs::write(seed.path().join("a.txt"), "v1\n").unwrap();
        commit_all(&seed.path().to_path_buf(), "seed");
        std::process::Command::new("git")
            .args(["push", "-q", bare.to_str().unwrap(), "main"])
            .current_dir(seed.path())
            .output()
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let repo_path_string = dir.path().display().to_string();
        std::process::Command::new("git")
            .args(["clone", "-q", bare.to_str().unwrap(), "."])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = TabRegistry::with_single_tab(0, "repo".to_string(), dir.path().to_path_buf());
        let (app, refresh_rx) = boot(registry).await;
        let _server = run_server(listener, app.clone(), refresh_rx);

        let mut ws = connect_with_retry(&format!("ws://{addr}/ws")).await;
        recv_state_until(&mut ws, |s| {
            !s.tabs.is_empty() && s.tabs[0].state.current_branch == "main"
        })
        .await;

        // Prove the original watcher is alive before pulling the ground out.
        std::fs::write(dir.path().join("junk.txt"), "junk").unwrap();
        recv_state_until(&mut ws, |s| {
            !s.tabs.is_empty()
                && s.tabs[0]
                    .state
                    .changes
                    .iter()
                    .any(|c| c.path == "junk.txt")
        })
        .await;

        ws.send(Message::Text(r#"{"tab":0,"action":"Reclone"}"#.into()))
            .await
            .unwrap();

        // The clean frame can only follow the delete + fresh clone; the
        // pre-reclone dirty state above rules out a stale broadcast match.
        recv_state_until(&mut ws, |s| {
            !s.tabs.is_empty() && s.tabs[0].state.changes.is_empty()
        })
        .await;

        // The decisive assertion: filesystem events must still arrive after
        // the directory was replaced. Re-write until observed because the
        // watcher respawn races the clone finishing.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "filesystem change after Reclone was never broadcast: watcher was not respawned"
            );
            std::fs::write(dir.path().join("late.txt"), "data").unwrap();
            let pred = |s: &WebState| {
                s.tabs.iter().any(|t| {
                    t.repo_path == repo_path_string
                        && t.state.changes.iter().any(|c| c.path == "late.txt")
                })
            };
            match tokio::time::timeout(
                std::time::Duration::from_millis(400),
                recv_state_until(&mut ws, pred),
            )
            .await
            {
                Ok(_) => break,
                Err(_) => continue,
            }
        }
    }
}