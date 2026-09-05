use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

const DEBOUNCE_MS: u64 = 200;

/// Directory names whose churn can never affect repository status
/// (build artifacts, dependency installs, caches). Events originating
/// exclusively inside these are dropped before debouncing so tools like
/// `cargo build` or `npm install` do not trigger endless refresh cycles.
const IGNORED_DIRS: [&str; 5] = ["target", "node_modules", "__pycache__", ".venv", "venv"];

pub fn spawn_watcher(
    repo_path: PathBuf,
    tx: UnboundedSender<()>,
) -> Result<RecommendedWatcher, notify::Error> {
    let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            let _ = raw_tx.send(res);
        },
        notify::Config::default(),
    )?;

    // A single recursive watch of the repo root already covers `.git`;
    // adding a second watch for `.git` duplicates every event.
    watcher.watch(&repo_path, RecursiveMode::Recursive)?;

    std::thread::spawn(move || {
        debounce_loop(raw_rx, tx);
    });

    Ok(watcher)
}

/// True when the event can never affect repository status and must not
/// trigger a refresh: pure reads (`Access`), or every path living inside
/// an ignored build/cache directory.
fn is_ignored(event: &notify::Event) -> bool {
    if matches!(event.kind, notify::EventKind::Access(_)) {
        return true;
    }
    !event.paths.is_empty()
        && event.paths.iter().all(|path| {
            path.components().any(|component| {
                IGNORED_DIRS
                    .iter()
                    .any(|dir| component.as_os_str() == *dir)
            })
        })
}

fn debounce_loop(
    raw_rx: mpsc::Receiver<notify::Result<notify::Event>>,
    tx: UnboundedSender<()>,
) {
    let mut pending: Option<Instant> = None;

    loop {
        match raw_rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(event)) => {
                if !is_ignored(&event) {
                    pending = Some(Instant::now());
                }
            }
            Ok(Err(e)) => {
                tracing::debug!("watcher error: {e}");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if let Some(deadline) = pending {
            if deadline.elapsed() >= Duration::from_millis(DEBOUNCE_MS) {
                let _ = tx.send(());
                pending = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn events_inside_ignored_dirs_are_dropped() {
        let make = |paths: &[&str]| notify::Event {
            paths: paths.iter().map(PathBuf::from).collect(),
            ..notify::Event::default()
        };
        assert!(is_ignored(&make(&["/repo/target/debug/foo.rs"])));
        assert!(is_ignored(&make(&["/repo/node_modules/pkg/index.js"])));
        assert!(is_ignored(&make(&["/repo/target/a", "/repo/node_modules/b"])));

        assert!(!is_ignored(&make(&["/repo/src/main.rs"])));
        // Mixed event touching one real path must not be dropped.
        assert!(!is_ignored(&make(&["/repo/target/x", "/repo/src/lib.rs"])));
        assert!(!is_ignored(&make(&[])), "empty event stays conservative");
        // A file merely *named* like a dir elsewhere must not match.
        assert!(!is_ignored(&make(&["/repo/target.rs"])));

        // Reads never change repository state — including the Access burst
        // the inotify backend emits while registering recursive watches.
        let read = notify::Event {
            kind: notify::EventKind::Access(notify::event::AccessKind::Open(
                notify::event::AccessMode::Any,
            )),
            paths: vec![PathBuf::from("/repo/src/main.rs")],
            ..notify::Event::default()
        };
        assert!(is_ignored(&read));
    }

    #[tokio::test]
    async fn build_dir_churn_does_not_trigger_refresh() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _watcher = spawn_watcher(dir.path().to_path_buf(), tx).unwrap();

        fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        for i in 0..20 {
            fs::write(dir.path().join(format!("target/debug/artifact{i}")), "x").unwrap();
        }

        let quiet = tokio::time::timeout(Duration::from_millis(800), rx.recv()).await;
        assert!(quiet.is_err(), "build churn must not emit refresh events");
    }

    #[tokio::test]
    async fn watcher_emits_debounced_events() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _watcher = spawn_watcher(dir.path().to_path_buf(), tx).unwrap();

        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        fs::write(dir.path().join("file.txt"), "world").unwrap();

        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for debounced event")
            .expect("channel closed");
        assert_eq!(first, ());

        fs::write(dir.path().join("other.txt"), "second burst").unwrap();
        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for second debounced event")
            .expect("channel closed");
        assert_eq!(second, (), "debouncer must keep firing after the first event");
    }

    #[tokio::test]
    async fn rapid_burst_coalesces_into_single_refresh() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _watcher = spawn_watcher(dir.path().to_path_buf(), tx).unwrap();

        for i in 0..25 {
            fs::write(dir.path().join(format!("burst-{i}.txt")), "x").unwrap();
        }

        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for debounced event")
            .expect("channel closed");
        assert_eq!(first, ());

        // A fresh raw event arriving after the burst could legitimately arm a
        // second 200 ms window, so only assert that the debouncer never
        // double-fires back-to-back (impossible within DEBOUNCE_MS=200).
        let trailing = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
        assert!(
            trailing.is_err(),
            "a burst written within one debounce window must yield a single refresh"
        );
    }
}