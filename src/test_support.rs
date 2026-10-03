//! Shared test helpers for the git, server, and websocket test suites.
//!
//! Compiled only under `cfg(test)`; registered in `main.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::Message;

use crate::server::registry::{TabRegistry, WebState};
use crate::server::AppState;

pub type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

const CONNECT_ATTEMPTS: usize = 100;
const CONNECT_POLL: Duration = Duration::from_millis(50);
const RECV_TIMEOUT: Duration = Duration::from_secs(20);

/// Creates a throwaway repository with a deterministic identity so commits
/// made by tests are reproducible.
pub fn init_repo(dir: &Path) {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test User"]);
}

/// Stages everything and commits with the given message.
pub fn commit_all(dir: &Path, message: &str) {
    for args in [
        vec!["add", "-A"],
        vec!["commit", "-q", "-m", message],
    ] {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
    }
}

/// Single-tab AppState rooted at `dir`, the standard fixture for server tests.
pub fn app_for(path: &Path) -> AppState {
    AppState::new(TabRegistry::with_single_tab(
        0,
        "repo".to_string(),
        path.to_path_buf(),
    ))
}

/// Connects to the test server, tolerating the startup window before axum
/// begins accepting connections.
pub async fn connect_with_retry(url: &str) -> WsStream {
    for _ in 0..CONNECT_ATTEMPTS {
        if let Ok((ws, _)) = tokio_tungstenite::connect_async(url).await {
            return ws;
        }
        tokio::time::sleep(CONNECT_POLL).await;
    }
    panic!("failed to connect to {url}");
}

/// Awaits the next state broadcast and decodes it.
pub async fn recv_state(ws: &mut WsStream) -> WebState {
    let msg = tokio::time::timeout(RECV_TIMEOUT, ws.next())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for ws message"))
        .expect("stream closed")
        .expect("ws error");
    match msg {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected text state message, got {other:?}"),
    }
}

/// Awaits broadcasts until one satisfies `pred`, returning that state.
pub async fn recv_state_until(
    ws: &mut WsStream,
    mut pred: impl FnMut(&WebState) -> bool,
) -> WebState {
    // Generous ceiling: the full suite runs many git-spawning tests
    // concurrently and can starve an individual exchange for seconds.
    tokio::time::timeout(RECV_TIMEOUT, async {
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

/// Work repo with one commit plus a bare local-path origin, wired up so `main`
/// tracks `origin/main` — no `push` against the remote, only a local `fetch`.
///
/// Returns the work tempdir (the bare origin lives inside it), the bare repo
/// path, and a closure that advances the origin's `main` without the work repo
/// fetching it: the state a colleague's push leaves behind.
pub fn repo_with_remote() -> (tempfile::TempDir, PathBuf, impl Fn(&Path)) {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
    commit_all(dir.path(), "initial");

    let bare = dir.path().join("origin.git");
    let git = |cwd: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    git(dir.path(), &["init", "-q", "--bare", "origin.git"]);
    git(
        &bare,
        &[
            "fetch",
            "-q",
            dir.path().to_str().unwrap(),
            "refs/heads/main:refs/heads/main",
        ],
    );
    git(
        dir.path(),
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git(dir.path(), &["fetch", "-q", "origin"]);
    git(dir.path(), &["config", "branch.main.remote", "origin"]);
    git(dir.path(), &["config", "branch.main.merge", "refs/heads/main"]);

    let remote = bare.clone();
    let advance = move |work: &Path| {
        std::fs::write(work.join("a.txt"), "v2\n").unwrap();
        commit_all(work, "second");
        std::process::Command::new("git")
            .args([
                "fetch",
                "-q",
                work.to_str().unwrap(),
                "refs/heads/main:refs/heads/main",
            ])
            .current_dir(&remote)
            .output()
            .unwrap();
    };

    (dir, bare, advance)
}

/// Polls `pred` against the registry snapshot until it holds, or panics after
/// `timeout`. The async counterpart to asserting on a background task's
/// eventual effect.
pub async fn wait_for_snapshot(
    registry: &TabRegistry,
    what: &str,
    timeout: Duration,
    pred: impl Fn(&WebState) -> bool,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let snapshot = registry.snapshot();
        if pred(&snapshot) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
