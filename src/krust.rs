//! Optional krust web-terminal auto-launch.
//!
//! The Grit web UI embeds an iframe pointing at krust (a separate Rust web
//! terminal project) on localhost:3000. When Grit runs as the daemon and
//! krust is NOT already running, this module locates a `krust` binary on
//! `$KRUST_BIN` / `PATH` and spawns it detached so the terminal dock buttons
//! just work. Krust being absent, broken, or slow to start is never fatal to
//! Grit — failures are logged and ignored.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The port the web UI expects krust on (krust's default; `KRUST_PORT` in
/// krust overrides its own side, but this probe follows krust's default).
const KRUST_PORT: u16 = 3000;

/// True when something answers on `127.0.0.1:{KRUST_PORT}`, i.e. krust (or
/// some other service) is already bound there — in which case we won't spawn.
async fn krust_is_up() -> bool {
    use tokio::net::TcpStream;
    tokio::time::timeout(
        Duration::from_millis(400),
        TcpStream::connect(format!("127.0.0.1:{KRUST_PORT}")),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// Locate a `krust` executable: `$KRUST_BIN` wins, then a PATH search.
fn find_krust_binary() -> Option<PathBuf> {
    finding_krust_binary(std::env::var("KRUST_BIN").ok(), std::env::var_os("PATH"))
}

fn finding_krust_binary(krust_bin: Option<String>, path_var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(explicit) = krust_bin {
        let p = PathBuf::from(&explicit);
        if p.is_file() {
            return Some(p);
        }
    }
    path_var.and_then(|paths| {
        for dir in std::env::split_paths(&paths) {
            for name in ["krust", "krust.exe"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    })
}

/// Best-effort: spawn krust when it is installed but not running. Errors and
/// absences log and are swallowed.
pub async fn ensure_krust() {
    if krust_is_up().await {
        tracing::debug!("krust already running on port {KRUST_PORT}");
        return;
    }
    let Some(bin) = find_krust_binary() else {
        tracing::info!(
            "krust not found (set KRUST_BIN or put it on PATH); terminal dock buttons stay hidden"
        );
        return;
    };
    match Command::new(&bin).spawn() {
        Ok(_child) => tracing::info!("launched krust ({}) for terminal dock", bin.display()),
        Err(e) => tracing::warn!("failed to spawn krust ({}): {e}", bin.display()),
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
    fn krust_bin_env_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let explicit = fake_bin(dir.path(), "krust");
        let on_path = dir.path().join("other").join("krust");
        std::fs::create_dir_all(on_path.parent().unwrap()).unwrap();
        std::fs::write(&on_path, "#!/bin/sh\n").unwrap();
        assert_eq!(
            finding_krust_binary(Some(explicit.to_string_lossy().into()), None),
            Some(explicit)
        );
    }

    #[test]
    fn falls_back_to_path_search() {
        let dir = tempfile::tempdir().unwrap();
        let found = fake_bin(dir.path(), "krust");
        assert_eq!(
            finding_krust_binary(None, Some(dir.path().as_os_str().to_os_string())),
            Some(found)
        );
    }

    #[test]
    fn missing_binary_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            finding_krust_binary(Some(dir.path().join("nope").to_string_lossy().into()), None),
            None
        );
    }
}