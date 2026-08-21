//! Project Actions subsystem: automatic discovery and fire-and-forget
//! launching of repository scripts/executables.
//!
//! Scans only the repository root, `scripts/`, and `tools/` (non-recursive).
//! Launches are detached (`spawn` + drop): Grit never tracks, waits on, or
//! captures launched processes.
//!
//! This module is deliberately self-contained. To disable the feature at
//! runtime set [`ENABLED`] to `false`; to excise it entirely delete this
//! file plus the few `actions::` call sites in `git/mod.rs` and the UI.

use std::path::Path;

use crate::git::types::ScriptEntry;

/// Runtime kill-switch. `false` makes [`discover`] return an empty list and
/// [`launch`] refuse every request — no scripts are surfaced or run.
pub const ENABLED: bool = true;

/// Repo-relative directories scanned for executables. `""` is the root.
const SCAN_DIRS: [&str; 3] = ["", "scripts", "tools"];

/// Upper bound on surfaced scripts, keeping the UI sane on messy repos.
const MAX_SCRIPTS: usize = 32;

/// Discovers executable files in the repository's scan directories.
/// Results are sorted by relative path for a stable dropdown order.
pub fn discover(repo_path: &Path) -> Vec<ScriptEntry> {
    if !ENABLED {
        return Vec::new();
    }

    let mut found: Vec<ScriptEntry> = Vec::new();
    for dir in SCAN_DIRS {
        let dir_path = if dir.is_empty() {
            repo_path.to_path_buf()
        } else {
            repo_path.join(dir)
        };
        let Ok(entries) = std::fs::read_dir(&dir_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_executable_file(&path) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let rel_path = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            found.push(ScriptEntry { name, rel_path });
            if found.len() >= MAX_SCRIPTS {
                found.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
                return found;
            }
        }
    }

    found.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    found
}

/// Launches a discovered script as a detached child process. Standard
/// output/error are inherited from Grit itself, so script output lands in
/// the terminal (or journal when daemonized) instead of vanishing; stdin is
/// disconnected. A background thread reaps the child so nothing lingers as
/// a zombie — beyond that, Grit neither waits nor tracks.
pub fn launch(repo_path: &Path, rel_path: &str) -> Result<(), String> {
    if !ENABLED {
        return Err("Project actions are disabled".to_string());
    }
    if rel_path.starts_with('/') || rel_path.contains("..") {
        return Err(format!("refusing suspicious path: {rel_path}"));
    }

    let canonical_repo = repo_path
        .canonicalize()
        .map_err(|e| format!("repository root unavailable: {e}"))?;
    let candidate = canonical_repo.join(rel_path);
    let canonical = candidate
        .canonicalize()
        .map_err(|_| format!("script not found: {rel_path}"))?;
    // Containment: the resolved target must stay inside the repository.
    if !canonical.starts_with(&canonical_repo) {
        return Err(format!("refusing path outside repository: {rel_path}"));
    }
    if !is_executable_file(&canonical) {
        return Err(format!("not an executable file: {rel_path}"));
    }

    let spawned = base_command(&canonical, &canonical_repo)
        .spawn()
        .or_else(|e| {
            // Plain shell scripts without a shebang fail exec with ENOEXEC;
            // fall back to /bin/sh so they still launch.
            if cfg!(unix) && e.raw_os_error() == Some(8) {
                base_command(Path::new("/bin/sh"), &canonical_repo)
                    .arg(&canonical)
                    .spawn()
            } else {
                Err(e)
            }
        });
    let mut child = spawned.map_err(|e| format!("failed to launch {rel_path}: {e}"))?;

    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Builds the detached-launch command for a program run inside `cwd`.
fn base_command(program: &Path, cwd: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    cmd
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(ext.to_ascii_lowercase().as_str(), "bat" | "cmd" | "ps1" | "exe")
        && path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn make_exec(path: &Path) {
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn make_plain(path: &Path) {
        fs::write(path, "data").unwrap();
    }

    #[test]
    fn discovers_scripts_in_scan_dirs_only() {
        let dir = tempfile::tempdir().unwrap();
        make_exec(&dir.path().join("run.sh"));
        fs::create_dir(dir.path().join("scripts")).unwrap();
        make_exec(&dir.path().join("scripts/build.sh"));
        fs::create_dir(dir.path().join("tools")).unwrap();
        make_exec(&dir.path().join("tools/fix.sh"));
        fs::create_dir(dir.path().join("other")).unwrap();
        make_exec(&dir.path().join("other/nope.sh"));

        let found = discover(dir.path());
        let rels: Vec<&str> = found.iter().map(|s| s.rel_path.as_str()).collect();
        assert_eq!(rels, vec!["run.sh", "scripts/build.sh", "tools/fix.sh"]);
    }

    #[test]
    fn skips_non_executable_hidden_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("scripts")).unwrap();
        make_plain(&dir.path().join("scripts/plain.txt"));
        make_exec(&dir.path().join("scripts/.hidden.sh"));
        fs::create_dir_all(dir.path().join("scripts/nested")).unwrap();
        make_exec(&dir.path().join("scripts/nested/deep.sh"));

        let found = discover(dir.path());
        assert!(found.is_empty(), "got: {found:?}");
    }

    #[test]
    fn missing_directories_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        make_exec(&dir.path().join("only.sh"));
        let found = discover(dir.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "only.sh");
    }

    #[test]
    fn discovery_is_capped_at_max_scripts() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("scripts")).unwrap();
        for i in 0..40 {
            make_exec(&dir.path().join(format!("scripts/s{i:02}.sh")));
        }
        let found = discover(dir.path());
        assert_eq!(found.len(), MAX_SCRIPTS);
        assert!(
            found.windows(2).all(|w| w[0].rel_path <= w[1].rel_path),
            "results must stay sorted after capping"
        );
    }

    #[test]
    fn launch_runs_script_to_completion_detached() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("scripts")).unwrap();
        let script_path = dir.path().join("scripts/marker.sh");
        fs::write(
            &script_path,
            format!("#!/bin/sh\ntouch {}/marker\nsleep 30\n", dir.path().display()),
        )
        .unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        launch(dir.path(), "scripts/marker.sh").expect("launch must succeed");

        // Fire-and-forget: the marker appears while the child is still
        // sleeping; Grit has already returned without tracking it.
        for _ in 0..50 {
            if dir.path().join("marker").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(dir.path().join("marker").exists(), "script side effect missing");
    }

    #[test]
    fn launch_runs_shebangless_script_via_sh_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("plain");
        fs::write(
            &script_path,
            format!("touch {}/marker\nsleep 30\n", dir.path().display()),
        )
        .unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        launch(dir.path(), "plain").expect("sh fallback must launch shebangless scripts");

        for _ in 0..50 {
            if dir.path().join("marker").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(dir.path().join("marker").exists());
    }

    #[test]
    fn launch_rejects_escapes_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("scripts")).unwrap();

        assert!(launch(dir.path(), "../outside.sh").is_err());
        assert!(launch(dir.path(), "/etc/passwd").is_err());
        assert!(launch(dir.path(), "scripts/missing.sh").is_err());

        let outside = tempfile::tempdir().unwrap();
        make_exec(&outside.path().join("evil.sh"));
        let link = dir.path().join("link.sh");
        std::os::unix::fs::symlink(outside.path().join("evil.sh"), &link).unwrap();
        assert!(
            launch(dir.path(), "link.sh").is_err(),
            "symlink escape must be refused"
        );
    }

    #[test]
    fn launch_refuses_non_executables() {
        let dir = tempfile::tempdir().unwrap();
        make_plain(&dir.path().join("plain.txt"));
        assert!(launch(dir.path(), "plain.txt").is_err());
    }
}
