//! Optional folio file-explorer auto-launch.
//!
//! The Grit web UI embeds an iframe of the files dock pointing at folio (a
//! separate Rust web file-explorer project) on localhost:4000. When Grit runs
//! as the daemon and folio is NOT already running, this module locates a
//! `folio` binary on `$FOLIO_BIN` / `PATH` and spawns it detached so the files
//! dock just works. Folio being absent, broken, or slow to start is never
//! fatal to Grit — failures are logged and ignored.
//!
//! Folio binds one root per process and serves it from `/info`, but the web UI
//! always opens `?dir=<repo>` in the iframe; with folio's `?dir` start param
//! that rebinds the dock to the active repository without restarting folio.
//! The root passed at spawn is only a fallback for a directly-opened folio.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The port the web UI expects folio on (folio's default; `--port` on the CLI
/// overrides its own side, but this probe follows folio's default).
const FOLIO_PORT: u16 = 4000;

/// True when something answers on `127.0.0.1:{FOLIO_PORT}`, i.e. folio (or
/// some other service) is already bound there — in which case we won't spawn.
async fn folio_is_up() -> bool {
    use tokio::net::TcpStream;
    tokio::time::timeout(
        Duration::from_millis(100),
        TcpStream::connect(format!("127.0.0.1:{FOLIO_PORT}")),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Locate a `folio` executable: `$FOLIO_BIN` wins, then a PATH search.
fn find_folio_binary() -> Option<PathBuf> {
    finding_folio_binary(std::env::var("FOLIO_BIN").ok(), std::env::var_os("PATH"))
}

fn finding_folio_binary(
    folio_bin: Option<String>,
    path_var: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(explicit) = folio_bin {
        let p = PathBuf::from(&explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    path_var.and_then(|paths| {
        for dir in std::env::split_paths(&paths) {
            for name in ["folio", "folio.exe"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    })
}

/// Best-effort: spawn folio when it is installed but not running. The `root`
/// is the served fallback directory (usually the first repo, else home);
/// per-repo roots come from the iframe `?dir=` param. Errors and absences log
/// and are swallowed.
pub async fn ensure_folio(root: Option<&Path>) {
    if folio_is_up().await {
        tracing::debug!("folio already running on port {FOLIO_PORT}");
        return;
    }
    let Some(bin) = find_folio_binary() else {
        tracing::info!(
            "folio not found (set FOLIO_BIN or put it on PATH); the files dock stays empty"
        );
        return;
    };
    let root = match root {
        Some(root) if root.is_dir() => root.to_path_buf(),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")),
    };
    match Command::new(&bin)
        .args(["--root", root.to_str().unwrap_or(".")])
        .args(["--port", &FOLIO_PORT.to_string()])
        .spawn()
    {
        Ok(_child) => {
            tracing::info!(
                "launched folio ({}) for the files dock, root {}",
                bin.display(),
                root.display()
            );
        }
        Err(e) => tracing::warn!("failed to spawn folio ({}): {e}", bin.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bin(root: &std::path::Path, name: &str) -> PathBuf {
        let p = root.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        p
    }

    #[test]
    fn folio_bin_env_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = fake_bin(dir.path(), "folio");
        let on_path = dir.path().join("other").join("folio");
        std::fs::create_dir_all(on_path.parent().unwrap()).unwrap();
        std::fs::write(&on_path, "#!/bin/sh\n").unwrap();
        assert_eq!(
            finding_folio_binary(Some(explicit.to_string_lossy().into()), None),
            Some(explicit)
        );
    }

    #[test]
    fn falls_back_to_path_search() {
        let dir = tempfile::tempdir().unwrap();
        let found = fake_bin(dir.path(), "folio");
        assert_eq!(
            finding_folio_binary(None, Some(dir.path().as_os_str().to_os_string())),
            Some(found)
        );
    }

    #[test]
    fn missing_binary_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            finding_folio_binary(Some(dir.path().join("nope").to_string_lossy().into()), None),
            None
        );
    }
}