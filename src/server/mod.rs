//! Axum web server subsystem: shared state, routing, and sync loop.

pub mod registry;
pub mod static_files;
pub mod websocket;
mod handlers;
pub(crate) use handlers::*;

use std::path::PathBuf;
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::CorsLayer;

use crate::git::sync::{check_remote_sync, SyncOutcome};
use crate::git::types::RepoState;
use crate::server::registry::{TabRegistry, WebState};

/// Capacity of the state-broadcast channel fanning frames out to every
/// connected client (slow clients lag rather than block publishers).
const BROADCAST_CAPACITY: usize = 128;

/// Per-operation deadline for the raw-TCP daemon probe on `/health`.
#[cfg(any(test, feature = "desktop"))]
const DAEMON_PROBE_TIMEOUT_MS: u64 = 100;

/// Listen backlog for the TCP listener (`socket.listen`).
const LISTEN_BACKLOG: u32 = 1024;

/// Gap between remote-sync probe passes. Uniform rather than a backoff ladder:
/// the first client connect is a strong enough signal that front-loaded retries
/// buy nothing.
const SYNC_TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a tab may keep answering "unreachable" before the probe gives up on
/// it. Bounds the offline-at-startup case: 20 attempts, then silence until a
/// manual Fetch/Pull re-arms the tab.
const SYNC_GIVE_UP_AFTER: std::time::Duration = std::time::Duration::from_secs(600);

/// Per-tab start offset within a pass, so N tabs do not open N connections in
/// the same millisecond. Capped at a handful of slots: a wide spread would
/// stretch the pass for no benefit.
const SYNC_STAGGER_SLOTS: usize = 8;
const SYNC_STAGGER_STEP: std::time::Duration = std::time::Duration::from_millis(250);

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
    /// Set once a client has asked for the remote-sync probe. The ticker is
    /// spawned at boot but stays dormant until this flips: a daemon can boot
    /// scheduled or headless with no network, and the user opening a client is
    /// the signal that connectivity is worth spending.
    sync_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Wakes the ticker immediately when the probe is first requested, so the
    /// badge is not a whole `SYNC_TICK` away.
    sync_signal: Arc<tokio::sync::Notify>,
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
            sync_requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sync_signal: Arc::default(),
        }
    }

    /// Releases the sync ticker, returning true only for the first caller.
    ///
    /// `swap`, not load-then-store: several browser tabs reloading at once would
    /// each decide they were first and kick duplicate fan-outs. The return value
    /// is the whole trigger contract — only the caller that sees `true` has a
    /// pass to start.
    ///
    /// Deliberately one-way. The probe answers once per tab per session, so
    /// there is nothing to un-request.
    pub fn request_sync_check(&self) -> bool {
        let first = !self
            .sync_requested
            .swap(true, std::sync::atomic::Ordering::SeqCst);
        if first {
            self.sync_signal.notify_one();
        }
        first
    }

    /// True once some client has asked for the sync probe.
    pub fn sync_requested(&self) -> bool {
        self.sync_requested
            .load(std::sync::atomic::Ordering::SeqCst)
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
        .route("/", get(static_files::serve_static))
        .route("/{*path}", get(static_files::serve_static))
        .with_state(state)
        .layer(CorsLayer::permissive())
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
        // `out_of_date` is owned by the remote sync checker, not by a status
        // collection, so carry it across instead of clobbering it with the
        // `false` that `get_repository_status` has to assume.
        Ok(Ok(mut state)) => {
            state.out_of_date = app
                .registry
                .snapshot()
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .is_some_and(|tab| tab.state.out_of_date);
            app.registry.update_state(tab_id, state)
        }
        Ok(Err(e)) => tracing::error!("repository status refresh failed for tab {tab_id}: {e}"),
        Err(e) => tracing::error!("repository status task panicked: {e}"),
    }
}

/// Recomputes the state of every open repository tab concurrently.
pub async fn refresh_all(app: &AppState) {
    let tabs = app.registry.snapshot().tabs;
    let futures: Vec<_> = tabs
        .iter()
        .map(|tab| refresh_tab(app, tab.id))
        .collect();
    futures_util::future::join_all(futures).await;
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

/// One remote-sync probe for a single tab. `None` means the tab spent its
/// give-up ceiling and must be settled without a verdict.
async fn probe_tab_sync(tab_id: usize, repo_path: PathBuf) -> Option<SyncOutcome> {
    // Git calls block; the ticker runs on the async side and must never hold a
    // runtime worker (AGENTS.md rule 2).
    match tokio::task::spawn_blocking(move || check_remote_sync(&repo_path)).await {
        Ok(Ok(out_of_date)) => Some(if out_of_date {
            SyncOutcome::OutOfDate
        } else {
            SyncOutcome::InSync
        }),
        // A transport failure is the only inconclusive answer: leave the tab
        // unsettled so the next tick tries again.
        Ok(Err(e)) if e.is_retryable_transport_failure() => {
            tracing::debug!("tab {tab_id} sync probe inconclusive: {e}");
            None
        }
        // No upstream, a deleted branch, anything local: settled, and the badge
        // stays off.
        Ok(Err(e)) => {
            tracing::debug!("tab {tab_id} has nothing to compare against: {e}");
            Some(SyncOutcome::NoRemote)
        }
        Err(e) => {
            tracing::error!("tab {tab_id} sync probe task panicked: {e}");
            Some(SyncOutcome::NoRemote)
        }
    }
}

/// One pass over every unsettled tab, concurrently. A settled tab is skipped
/// outright — this filter is what keeps the ticker from spawning an `ls-remote`
/// every 30s forever.
async fn sync_pass(app: &AppState, budget: &mut SyncBudget, now: std::time::Instant) {
    let tabs = app.registry.snapshot().tabs;
    let live: std::collections::HashSet<usize> = tabs.iter().map(|t| t.id).collect();
    budget.forget_missing(&live);

    let outstanding: Vec<_> = tabs
        .iter()
        .filter(|tab| !app.registry.is_settled(tab.id))
        .collect();
    if outstanding.is_empty() {
        // Every tab has its answer; the filter below is what keeps this loop
        // from spawning an `ls-remote` every 30s forever.
        tracing::debug!(
            "sync pass: nothing to probe, {} tab(s) already settled: {:?}",
            app.registry.settled_tabs().len(),
            app.registry.settled_tabs()
        );
        return;
    }

    let probes: Vec<_> = outstanding
        .into_iter()
        .map(|tab| {
            let started = budget.started_at(tab.id, now);
            let path = PathBuf::from(&tab.repo_path);
            let stagger = SYNC_STAGGER_STEP * (tab.id % SYNC_STAGGER_SLOTS) as u32;
            let expired = now.duration_since(started) >= budget.give_up_after;
            async move {
                if expired {
                    // Ceiling spent. Settle it so the loop stops here; the badge
                    // stays off and a manual Fetch/Pull re-arms the tab.
                    return (tab.id, SyncPass::GaveUp);
                }
                // Spread the passes out so N tabs do not hit one remote at once.
                tokio::time::sleep(stagger).await;
                (tab.id, SyncPass::from(probe_tab_sync(tab.id, path).await))
            }
        })
        .collect();

    for (tab_id, pass) in futures_util::future::join_all(probes).await {
        match pass {
            SyncPass::GaveUp => {
                tracing::debug!("tab {tab_id} gave up on the sync probe");
                app.registry.mark_settled(tab_id);
            }
            SyncPass::Inconclusive => {}
            SyncPass::Answered(outcome) => {
                if outcome.is_settled() {
                    app.registry.mark_settled(tab_id);
                }
                if let Some(state) = sync_badge_state(app, tab_id, outcome.out_of_date()) {
                    app.registry.update_state(tab_id, state);
                }
            }
        }
    }
}

/// The tab's state with `out_of_date` set, or `None` when it already holds that
/// value — a no-op publish would churn the revision (and every client's render)
/// once per tick for nothing.
fn sync_badge_state(app: &AppState, tab_id: usize, out_of_date: bool) -> Option<RepoState> {
    let state = app
        .registry
        .snapshot()
        .tabs
        .into_iter()
        .find(|tab| tab.id == tab_id)?
        .state;
    (state.out_of_date != out_of_date).then_some(RepoState {
        out_of_date,
        ..state
    })
}

/// What one tab's probe produced this pass.
enum SyncPass {
    /// A conclusive answer; settle unless it was `Unreachable`.
    Answered(SyncOutcome),
    /// No answer this time: stay unsettled and try again next tick.
    Inconclusive,
    /// The give-up ceiling is spent; settle without a verdict.
    GaveUp,
}

impl From<Option<SyncOutcome>> for SyncPass {
    fn from(outcome: Option<SyncOutcome>) -> Self {
        match outcome {
            Some(outcome) => SyncPass::Answered(outcome),
            None => SyncPass::Inconclusive,
        }
    }
}

/// Per-tab retry bookkeeping: when each unsettled tab was first seen, so the
/// give-up ceiling can be measured from the tab's own start rather than from
/// daemon boot.
struct SyncBudget {
    started: std::collections::HashMap<usize, std::time::Instant>,
    give_up_after: std::time::Duration,
}

impl SyncBudget {
    fn new(give_up_after: std::time::Duration) -> Self {
        SyncBudget {
            started: std::collections::HashMap::new(),
            give_up_after,
        }
    }

    fn started_at(&mut self, tab_id: usize, now: std::time::Instant) -> std::time::Instant {
        *self.started.entry(tab_id).or_insert(now)
    }

    /// Drops bookkeeping for tabs that no longer exist.
    fn forget_missing(&mut self, live: &std::collections::HashSet<usize>) {
        self.started.retain(|id, _| live.contains(id));
    }
}

/// Probes every open repository for out-of-date-ness until each tab reaches a
/// conclusive answer, once per tab per session.
///
/// Dormant until [`AppState::request_sync_check`]. One task for all tabs, in
/// the spirit of `watch_reconciler`: the loop must never fan out into a task per
/// tab, because tabs close and would leak the spinners.
pub async fn sync_ticker(app: AppState) {
    sync_ticker_with(app, SYNC_TICK, SYNC_GIVE_UP_AFTER).await
}

/// [`sync_ticker`] with the cadence and ceiling injected, so tests need not wait
/// 30 seconds to observe a pass.
pub(crate) async fn sync_ticker_with(
    app: AppState,
    tick: std::time::Duration,
    give_up_after: std::time::Duration,
) {
    let mut ticker = tokio::time::interval(tick);
    // The default `Burst` fires immediately for every tick missed while a pass
    // was blocked on a slow `spawn_blocking`, which would hammer the remote
    // with back-to-back probes.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut budget = SyncBudget::new(give_up_after);

    loop {
        // Armed before the select, so a request landing between passes cannot be
        // missed: `Notify` keeps the permit for the next waiter.
        let requested = app.sync_signal.notified();
        tokio::select! {
            _ = ticker.tick() => {}
            _ = requested => {}
        }
        if !app.sync_requested() {
            continue;
        }
        sync_pass(&app, &mut budget, std::time::Instant::now()).await;
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

    // Remote-sync probe. Dormant until the first client connects, which is the
    // signal that spending a round trip on the remote is worthwhile.
    tokio::spawn(sync_ticker(app.clone()));

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
    // All tabs refresh concurrently to minimize startup latency.
    tokio::spawn({
        let app = app.clone();
        async move {
            let tabs = app.registry.snapshot().tabs;
            let futures: Vec<_> = tabs
                .iter()
                .map(|tab| refresh_tab(&app, tab.id))
                .collect();
            futures_util::future::join_all(futures).await;
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
    // Files dock fallback root: the first open repository, else home.
    let default_root = app
        .registry
        .snapshot()
        .tabs
        .first()
        .map(|t| t.repo_path.clone())
        .filter(|p| !p.is_empty());
    let handle = run_server(listener, app, refresh_rx);
    // Spin up folio (the file explorer our dock embeds) and krust (the web
    // terminal) if they're installed but not already running — fire-and-forget,
    // never fatal. Per-repo roots come from the iframe `?dir=` param.
    tokio::spawn(async move {
        crate::folio::ensure_folio(default_root.as_deref().map(std::path::Path::new)).await;
        crate::krust::ensure_krust().await;
    });
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
        repo_with_remote, wait_for_snapshot,
    };

// --- remote sync ticker ---

    fn app_for_repo(path: &std::path::Path) -> AppState {
        let app = app_for(path);
        app.request_sync_check();
        app
    }

    fn out_of_date(app: &AppState) -> bool {
        app.registry
            .snapshot()
            .tabs
            .first()
            .is_some_and(|t| t.state.out_of_date)
    }

    #[tokio::test]
    async fn sync_ticker_stays_dormant_until_a_client_asks() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let app = app_for(dir.path());

        tokio::spawn(sync_ticker_with(
            app.clone(),
            std::time::Duration::from_millis(20),
            std::time::Duration::from_secs(600),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        assert!(
            !app.registry.is_settled(0),
            "an unrequested probe would spend the remote's bandwidth uninvited"
        );
    }

    #[tokio::test]
    async fn sync_ticker_retries_an_unreachable_tab_until_it_answers() {
        let (dir, _bare, advance) = repo_with_remote();
        std::process::Command::new("git")
            .args(["remote", "set-url", "origin", "/nonexistent/grit-ticker"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let app = app_for_repo(dir.path());

        tokio::spawn(sync_ticker_with(
            app.clone(),
            std::time::Duration::from_millis(60),
            std::time::Duration::from_secs(600),
        ));

        // Offline for a few passes: the tab must stay unsettled, not settle on
        // the first failure, or it could never badge once the network returns.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(!app.registry.is_settled(0), "gave up while still offline");
        assert!(!out_of_date(&app), "an unreachable remote must not alarm");

        // The remote moves on while we are still offline, *then* the URL is
        // repaired. Doing it in this order removes the race: a probe issued
        // the instant the URL is fixed can no longer catch the remote
        // mid-update and wrongly conclude in-sync.
        advance(dir.path());
        std::process::Command::new("git")
            .args([
                "remote",
                "set-url",
                "origin",
                dir.path().join("origin.git").to_str().unwrap(),
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        wait_for_snapshot(&app.registry, "the badge after connectivity returned", std::time::Duration::from_secs(10), |_| {
            out_of_date(&app)
        })
        .await;
        assert!(app.registry.is_settled(0));
    }

    #[tokio::test]
    async fn sync_ticker_never_reprobes_a_settled_tab() {
        let (dir, _bare, advance) = repo_with_remote();
        let app = app_for_repo(dir.path());

        tokio::spawn(sync_ticker_with(
            app.clone(),
            std::time::Duration::from_millis(40),
            std::time::Duration::from_secs(600),
        ));
        wait_for_snapshot(&app.registry, "the in-sync tab to settle", std::time::Duration::from_secs(10), |_| {
            app.registry.is_settled(0)
        })
        .await;
        assert!(!out_of_date(&app));

        // A settled tab is in sync *for this session*. If the ticker kept
        // probing, the badge would now flip — it must not.
        advance(dir.path());
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;

        assert!(
            !out_of_date(&app),
            "a settled tab was probed again: out_of_date flipped after the remote moved"
        );
    }

    #[tokio::test]
    async fn sync_ticker_settles_a_local_only_repo_on_the_first_pass() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let app = app_for_repo(dir.path());

        tokio::spawn(sync_ticker_with(
            app.clone(),
            std::time::Duration::from_millis(40),
            std::time::Duration::from_secs(600),
        ));
        wait_for_snapshot(&app.registry, "the local-only tab to settle", std::time::Duration::from_secs(10), |_| {
            app.registry.is_settled(0)
        })
        .await;

        assert!(!out_of_date(&app));
    }

    #[tokio::test]
    async fn sync_ticker_gives_up_after_the_ceiling() {
        let (dir, bare, advance) = repo_with_remote();
        std::process::Command::new("git")
            .args(["remote", "set-url", "origin", "/nonexistent/grit-ticker"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let app = app_for_repo(dir.path());

        // A ceiling far shorter than the tick, so the give-up rule — not the
        // answer — is what ends the retries.
        tokio::spawn(sync_ticker_with(
            app.clone(),
            std::time::Duration::from_millis(40),
            std::time::Duration::from_millis(60),
        ));
        wait_for_snapshot(&app.registry, "the tab to give up", std::time::Duration::from_secs(10), |_| {
            app.registry.is_settled(0)
        })
        .await;

        // Connectivity returns, but the ceiling is spent: no badge, ever.
        std::process::Command::new("git")
            .args(["remote", "set-url", "origin", bare.to_str().unwrap()])
            .current_dir(dir.path())
            .output()
            .unwrap();
        advance(dir.path());
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        assert!(
            !out_of_date(&app),
            "a tab past the ceiling must not keep probing the remote"
        );
    }

    #[tokio::test]
    async fn sync_ticker_probes_every_unsettled_tab() {
        let first = repo_with_remote();
        let second = repo_with_remote();
        let registry = TabRegistry::new();
        registry.set(WebState {
            active: 0,
            tabs: vec![
                crate::server::registry::WebTab {
                    id: 0,
                    name: "a".to_string(),
                    repo_path: first.0.path().display().to_string(),
                    state: Default::default(),
                    log: Vec::new(),
                },
                crate::server::registry::WebTab {
                    id: 1,
                    name: "b".to_string(),
                    repo_path: second.0.path().display().to_string(),
                    state: Default::default(),
                    log: Vec::new(),
                },
            ],
            revision: 0,
        });
        let app = AppState::with_watcher_resets(registry, mpsc::unbounded_channel().0);
        app.request_sync_check();
        // Second repo is stale against its remote.
        (second.2)(second.0.path());

        tokio::spawn(sync_ticker_with(
            app.clone(),
            std::time::Duration::from_millis(40),
            std::time::Duration::from_secs(600),
        ));
        wait_for_snapshot(&app.registry, "both tabs to settle", std::time::Duration::from_secs(10), |_| {
            let ids = app.registry.settled_tabs();
            ids == vec![0, 1]
        })
        .await;

        let state = app.registry.snapshot();
        assert!(
            !state.tabs[0].state.out_of_date,
            "the in-sync tab must show no badge"
        );
        assert!(
            state.tabs[1].state.out_of_date,
            "the tab whose remote moved must show a badge"
        );
    }

    #[tokio::test]
    async fn a_manual_fetch_clears_the_badge_on_the_next_pass() {
        let (dir, _bare, advance) = repo_with_remote();
        let app = app_for_repo(dir.path());
        // The remote moves before the first pass, so the badge is earned
        // legitimately rather than injected by hand.
        advance(dir.path());

        tokio::spawn(sync_ticker_with(
            app.clone(),
            std::time::Duration::from_millis(40),
            std::time::Duration::from_secs(600),
        ));
        wait_for_snapshot(&app.registry, "the badge to appear", std::time::Duration::from_secs(10), |_| {
            out_of_date(&app)
        })
        .await;

        // What a Fetch does on success: re-arm the tab. The remote the badge is
        // complaining about is the same one just fetched, so the next pass has
        // to find it in sync and take the badge back down.
        app.registry.reset_settled(0);
        advance(dir.path());
        std::process::Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        wait_for_snapshot(&app.registry, "the badge to clear", std::time::Duration::from_secs(10), |_| {
            !out_of_date(&app) && app.registry.is_settled(0)
        })
        .await;

        assert!(
            !out_of_date(&app),
            "a fetched tab must not keep showing the out-of-date badge"
        );
    }

    #[tokio::test]
    async fn refresh_tab_preserves_out_of_date() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    commit_all(dir.path(), "initial");
    let app = app_for(dir.path());

    // The sync checker owns this flag, but `refresh_tab` rebuilds RepoState
    // from scratch on every watcher event — it must carry the flag over or
    // the badge blinks off the first time anyone edits a file.
    app.registry.update_state(
        0,
        crate::git::types::RepoState {
            out_of_date: true,
            ..Default::default()
        },
    );

    refresh_tab(&app, 0).await;

    let state = app.registry.snapshot();
    assert!(
        state.tabs[0].state.out_of_date,
        "refresh_tab wiped the out-of-date flag"
    );
}

#[tokio::test]
async fn sync_loop_idles_when_refresh_channel_closes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let app = app_for(dir.path());

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
                revision: 0,
            });
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), bcast.recv()).await;
        assert!(received.is_ok(), "sync_loop stopped responding to changes");
    }

    #[tokio::test]
    async fn daemon_probe_detects_running_and_closed_ports() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let (_refresh_tx, refresh_rx) = mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app_for(dir.path()), refresh_rx);

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
        let app = app_for(dir.path());
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
        init_repo(dir.path());
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
        init_repo(dir.path());

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
        init_repo(dir.path());

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
        init_repo(seed.path());
        std::fs::write(seed.path().join("a.txt"), "v1\n").unwrap();
        commit_all(seed.path(), "seed");
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