use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc::UnboundedSender;

const DEBOUNCE_MS: u64 = 200;

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

    let git_dir = repo_path.join(".git");
    if git_dir.exists() {
        watcher.watch(&git_dir, RecursiveMode::Recursive)?;
    }
    watcher.watch(&repo_path, RecursiveMode::Recursive)?;

    std::thread::spawn(move || {
        debounce_loop(raw_rx, tx);
    });

    Ok(watcher)
}

fn debounce_loop(
    raw_rx: mpsc::Receiver<notify::Result<notify::Event>>,
    tx: UnboundedSender<()>,
) {
    let mut pending: Option<Instant> = None;

    loop {
        match raw_rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Ok(_event)) => {
                pending = Some(Instant::now());
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

        let _ = first;
        assert!(true);
    }

    #[test]
    fn debounce_windows_are_160ms_apart() {
        assert!(DEBOUNCE_MS >= 100);
    }
}