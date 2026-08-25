//! Project Actions subsystem: automatic discovery and fire-and-forget
//! launching of repository scripts/executables.
//!
//! Scans only the repository root, `scripts/`, and `tools/` (non-recursive).
//! Launches are detached (`spawn` + drop): Grit never tracks, waits on, or
//! captures launched processes.
//!
//! This module is deliberately self-contained; to excise the feature
//! entirely, delete this file plus the few `actions::` call sites in
//! `git/mod.rs` and the UI.

use std::path::Path;

use crate::git::types::ScriptEntry;

/// Subdirectory names scanned for executables, matched **case-insensitively**
/// against actual root-level directories (`Scripts/`, `TOOLS/`, ... all work).
/// The repository root itself is always scanned too.
const SCAN_DIR_NAMES: [&str; 2] = ["scripts", "tools"];

/// Upper bound on surfaced scripts, keeping the UI sane on messy repos.
const MAX_SCRIPTS: usize = 32;

/// Maximum `/proc` parent-chain hops when hunting for the ancestor terminal.
const PROC_ANCESTOR_WALK_LIMIT: u32 = 16;

/// `ENOEXEC`: the kernel refused to exec the file (no shebang, wrong
/// format) — retry through `/bin/sh` so shebang-less scripts still run.
const ENOEXEC: i32 = 8;

/// Discovers executable files in the repository's scan directories.
/// Results are sorted by relative path for a stable dropdown order.
pub fn discover(repo_path: &Path) -> Vec<ScriptEntry> {
    let mut found: Vec<ScriptEntry> = Vec::new();
    scan_dir(repo_path, "", &mut found);

    // Pick up root-level scripts/tools directories whatever their casing;
    // rel paths keep the real names so launches resolve on case-sensitive
    // filesystems.
    let mut dirs: Vec<std::path::PathBuf> = std::fs::read_dir(repo_path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .map(|name| {
                        let lower = name.to_string_lossy().to_lowercase();
                        SCAN_DIR_NAMES.contains(&lower.as_str())
                    })
                    .unwrap_or(false)
        })
        .collect();
    dirs.sort();
    for dir in dirs {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        scan_dir(&dir, &name, &mut found);
    }

    found.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    found.truncate(MAX_SCRIPTS);
    found
}

/// Collects executable files directly inside `dir`, recording them with
/// `prefix`-relative paths (`""` = repo root).
fn scan_dir(dir: &Path, prefix: &str, out: &mut Vec<ScriptEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
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
        let rel_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        out.push(ScriptEntry { name, rel_path });
    }
}

/// Launches a discovered script inside a **terminal window** so interactive
/// menu/TUI scripts get a real TTY and their output stays visible. The
/// window is kept open after the script exits showing its status. Grit
/// never waits on or tracks the launched process beyond reaping the
/// short-lived spawner child.
///
/// Terminal selection order (unix): `$TERMINAL`, then well-known emulators;
/// macOS uses Terminal.app, Windows opens a console via `start`. When no
/// terminal can be spawned (or `GRIT_NO_TERMINAL=1`, used by tests), the
/// script falls back to a direct detached spawn with inherited stdio.
pub fn launch(repo_path: &Path, rel_path: &str) -> Result<(), String> {
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

    let spawned = if terminal_disabled() {
        spawn_direct(&canonical, &canonical_repo)
    } else {
        spawn_terminal(&canonical, &canonical_repo).or_else(|e| {
            tracing::warn!(
                "no usable terminal emulator ({e}); running {rel_path} detached in Grit's \
                 own stdio instead. Set $TERMINAL to your terminal to fix this."
            );
            spawn_direct(&canonical, &canonical_repo)
        })
    };
    let mut child = spawned.map_err(|e| format!("failed to launch {rel_path}: {e}"))?;

    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Test hook: when set, never spawn a terminal emulator.
fn terminal_disabled() -> bool {
    std::env::var_os("GRIT_NO_TERMINAL").is_some()
}

/// Shell snippet that runs the script.
fn keep_open_payload(script: &Path) -> String {
    let script = script.display();
    format!("\"{script}\"")
}

/// Locates a program on `PATH` without shelling out.
fn find_in_path(program: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Direct detached spawn (pre-terminal behavior): inherits stdout/stderr,
/// nulls stdin, detaches the process group, falls back to `/bin/sh` for
/// shebang-less scripts.
fn spawn_direct(script: &Path, cwd: &Path) -> std::io::Result<std::process::Child> {
    base_command(script, cwd)
        .spawn()
        .or_else(|e| {
            if cfg!(unix) && e.raw_os_error() == Some(ENOEXEC) {
                base_command(Path::new("/bin/sh"), cwd).arg(script).spawn()
            } else {
                Err(e)
            }
        })
}

/// Well-known terminal emulators and the flags each expects before the
/// command. Single source of truth for every probe path below.
#[cfg(not(target_os = "macos"))]
const TERMINAL_FLAGS: &[(&str, &[&str])] = &[
    ("x-terminal-emulator", &["-e"]),
    ("gnome-terminal", &["--"]),
    ("ptyxis", &["--"]),
    ("kgx", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-x"]),
    ("alacritty", &["-e"]),
    ("kitty", &[]),
    ("tilix", &["-e"]),
    ("ghostty", &["-e"]),
    ("foot", &[]),
    ("st", &[]),
    ("urxvt", &["-e"]),
    ("xterm", &["-e"]),
    ("terminator", &["-x"]),
    ("mate-terminal", &["-e"]),
    ("lxterminal", &["-e"]),
    ("qterminal", &["-e"]),
    ("wezterm", &["start", "--"]),
];

#[cfg(not(target_os = "macos"))]
fn flags_for(program: &str) -> Option<&'static [&'static str]> {
    TERMINAL_FLAGS
        .iter()
        .find(|(name, _)| *name == program)
        .map(|(_, flags)| *flags)
}

/// Normalizes process names to entries in [`TERMINAL_FLAGS`]: strips `.exe`
/// and folds server variants like `gnome-terminal-server` (whose 15-char
/// comm name is truncated to `gnome-terminal-`) onto their base program.
#[cfg(not(target_os = "macos"))]
fn normalize_terminal_name(name: &str) -> &str {
    let name = name.strip_suffix(".exe").unwrap_or(name);
    let name = name.strip_suffix('-').unwrap_or(name);
    match name {
        "gnome-terminal-server" => "gnome-terminal",
        other => other,
    }
}

/// The terminal Grit itself is running inside, discovered by walking the
/// `/proc` parent chain. Launching it again opens a new window of exactly
/// the emulator the user chose to work in — no guessing required.
#[cfg(all(unix, not(target_os = "macos")))]
fn ancestor_terminal() -> Option<(String, Vec<String>)> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut pid = std::process::id();
    for _ in 0..PROC_ANCESTOR_WALK_LIMIT {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let comm_end = stat.rfind(')')?;
        let ppid: u32 = stat[comm_end + 2..]
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        if let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) {
            if let Some(arg0) = cmdline.split(|&b| b == 0).find(|s| !s.is_empty()) {
                let exe = Path::new(OsStr::from_bytes(arg0));
                if let Some(name) = exe.file_name().and_then(|n| n.to_str()) {
                    let normalized = normalize_terminal_name(name);
                    if let Some(flags) = flags_for(normalized) {
                        return Some((
                            normalized.to_string(),
                            flags.iter().map(|f| f.to_string()).collect(),
                        ));
                    }
                }
            }
        }
        pid = ppid;
    }
    None
}

/// Terminals preferred by the current desktop environment, so the right
/// emulator wins when several are installed.
#[cfg(not(target_os = "macos"))]
fn desktop_preferences() -> Vec<(&'static str, &'static [&'static str])> {
    let de = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if de.contains("gnome") || de.contains("unity") || de.contains("cinnamon") {
        vec![
            ("gnome-terminal", &["--"]),
            ("ptyxis", &["--"]),
            ("kgx", &["--"]),
        ]
    } else if de.contains("kde") || de.contains("plasma") {
        vec![("konsole", &["-e"])]
    } else if de.contains("xfce") {
        vec![("xfce4-terminal", &["-x"])]
    } else if de.contains("mate") {
        vec![("mate-terminal", &["-e"])]
    } else if de.contains("lxqt") {
        vec![("qterminal", &["-e"])]
    } else {
        Vec::new()
    }
}

/// The launchable binary name advertised by a TerminalEmulator entry:
/// `TryExec` when present, else the first token of `Exec` (basename only).
/// Flatpak-wrapped launchers return None — the wrapper does not accept a
/// plain `sh -c` command line.
#[cfg(not(target_os = "macos"))]
fn primary_binary(entry: &freedesktop_desktop_entry::DesktopEntry) -> Option<String> {
    if entry.exec().is_some_and(|e| e.starts_with("flatpak run")) {
        return None;
    }
    let bin = entry
        .try_exec()
        .or_else(|| entry.exec().and_then(|e| e.split_whitespace().next()))?;
    Some(
        Path::new(bin)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| bin.to_string()),
    )
}

/// Terminals advertised by installed `.desktop` files (Categories contains
/// `TerminalEmulator`), in freedesktop priority order. This reflects what
/// the system actually registered as terminal apps — including terminals
/// our static list has never heard of. Returns (flag-known, flag-unknown).
#[cfg(not(target_os = "macos"))]
fn desktop_file_terminals() -> (Vec<String>, Vec<String>) {
    use freedesktop_desktop_entry::{default_paths, DesktopEntry, Iter};

    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for path in Iter::new(default_paths()) {
        let Ok(entry) = DesktopEntry::from_path(path, None as Option<&[String]>) else {
            continue;
        };
        if entry.hidden() || entry.no_display() {
            continue;
        }
        let Some(categories) = entry.categories() else {
            continue;
        };
        if !categories.iter().any(|c| *c == "TerminalEmulator") {
            continue;
        }
        let Some(binary) = primary_binary(&entry) else {
            continue;
        };

        let target = if flags_for(&binary).is_some() {
            &mut known
        } else {
            &mut unknown
        };
        if !target.contains(&binary) {
            target.push(binary);
        }
    }
    (known, unknown)
}

#[cfg(not(target_os = "macos"))]
fn spawn_terminal(script: &Path, cwd: &Path) -> std::io::Result<std::process::Child> {
    use std::process::Command;

    let payload = keep_open_payload(script);
    let (attempts, positional_fallbacks) = build_terminal_probe_list();

    for (program, flags) in attempts {
        let Some(exe) = find_in_path(&program) else {
            continue;
        };
        let mut cmd = Command::new(exe);
        cmd.args(flags).args(["sh", "-c", &payload]).current_dir(cwd);
        match cmd.spawn() {
            Ok(child) => {
                tracing::info!("launched script in terminal window via {program}");
                return Ok(child);
            }
            Err(e) => tracing::debug!("terminal {program} failed: {e}"),
        }
    }

    for program in &positional_fallbacks {
        // Last resort: terminals we have no flag table entry for get the
        // command passed positionally — the most common convention.
        let Some(exe) = find_in_path(program) else {
            continue;
        };
        match Command::new(exe)
            .args(["sh", "-c", &payload])
            .current_dir(cwd)
            .spawn()
        {
            Ok(child) => {
                tracing::info!("launched script in terminal window via {program}");
                return Ok(child);
            }
            Err(e) => tracing::debug!("terminal {program} failed: {e}"),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no terminal emulator found",
    ))
}

/// Builds the ordered terminal-probe list. Probe order: explicit
/// `$TERMINAL`, the terminal Grit runs inside, freedesktop launchers,
/// installed `.desktop` entries, DE preferences, then the full known list.
/// Each entry is (program, flags preceding the command); duplicates are
/// dropped with first occurrence winning.
///
/// Returns `(flagged_probes, positional_fallbacks)` — unknown desktop
/// entries that lack a flag table entry are tried last without flags.
#[cfg(not(target_os = "macos"))]
fn build_terminal_probe_list() -> (Vec<(String, Vec<String>)>, Vec<String>) {
    let mut attempts: Vec<(String, Vec<String>)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    if let Some(term) = std::env::var_os("TERMINAL") {
        let name = term.to_string_lossy().into_owned();
        seen.push(name.clone());
        attempts.push((name, vec!["-e".into()]));
    }
    if let Some(mapped) = term_program_candidate() {
        seen.push(mapped.0.clone());
        attempts.push(mapped);
    }
    if let Some(ancestor) = ancestor_terminal() {
        tracing::debug!("detected host terminal: {}", ancestor.0);
        seen.push(ancestor.0.clone());
        attempts.push(ancestor);
    }
    let (desktop_known, desktop_unknown) = desktop_file_terminals();
    for program in &desktop_known {
        if let Some(flags) = flags_for(program) {
            if !seen.iter().any(|s| s == program) {
                seen.push(program.clone());
                attempts.push((
                    program.clone(),
                    flags.iter().map(|f| f.to_string()).collect(),
                ));
            }
        }
    }
    for (program, flags) in std::iter::once(("xdg-terminal-exec", &[] as &[&str]))
        .chain(std::iter::once(("xdg-terminal", &[] as &[&str])))
        .chain(desktop_preferences())
        .chain(TERMINAL_FLAGS.iter().copied())
    {
        if seen.iter().any(|s| s == program) {
            continue;
        }
        seen.push(program.to_string());
        attempts.push((
            program.to_string(),
            flags.iter().map(|f| f.to_string()).collect(),
        ));
    }

    (attempts, desktop_unknown)
}

/// Maps `TERM_PROGRAM` (set by terminals for their child processes) to a
/// fresh instance of that same terminal — i.e. the one the user chose.
#[cfg(not(target_os = "macos"))]
fn term_program_candidate() -> Option<(String, Vec<String>)> {
    let name = std::env::var("TERM_PROGRAM").ok()?;
    let (program, flags): (&str, &[&str]) = match name.as_str() {
        "ghostty" => ("ghostty", &["-e"]),
        "WezTerm" => ("wezterm", &["start", "--"]),
        "kitty" => ("kitty", &[]),
        "alacritty" => ("alacritty", &["-e"]),
        _ => return None,
    };
    Some((
        program.to_string(),
        flags.iter().map(|f| f.to_string()).collect(),
    ))
}

#[cfg(target_os = "macos")]
fn spawn_terminal(script: &Path, cwd: &Path) -> std::io::Result<std::process::Child> {
    use std::process::Command;

    // Terminal.app starts in $HOME, so cd explicitly. Escape for the
    // AppleScript string literal.
    let inner = format!(
        "cd '{}'; {}",
        cwd.display(),
        script.display()
    );
    let escaped = inner.replace('\\', "\\\\").replace('"', "\\\"");
    Command::new("osascript")
        .args([
            "-e",
            &format!("tell application \"Terminal\" to do script \"{escaped}\""),
        ])
        .spawn()
}

#[cfg(windows)]
fn spawn_terminal(script: &Path, cwd: &Path) -> std::io::Result<std::process::Child> {
    use std::process::Command;

    // `start` opens a new console window; `/C` closes it afterwards.
    Command::new("cmd")
        .args(["/C", "start", "", "cmd", "/C"])
        .arg(script)
        .current_dir(cwd)
        .spawn()
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
    fn scan_dirs_match_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("Scripts")).unwrap();
        make_exec(&dir.path().join("Scripts/build.sh"));
        fs::create_dir(dir.path().join("TOOLS")).unwrap();
        make_exec(&dir.path().join("TOOLS/fix.sh"));
        fs::create_dir(dir.path().join("Tools")).unwrap();
        make_exec(&dir.path().join("Tools/lint.sh"));

        let found = discover(dir.path());
        let rels: Vec<&str> = found.iter().map(|s| s.rel_path.as_str()).collect();
        assert_eq!(
            rels,
            vec!["Scripts/build.sh", "TOOLS/fix.sh", "Tools/lint.sh"]
        );
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

    /// Pins launches to the direct-spawn path so tests never open real
    /// terminal windows, even on developer machines that have them.
    fn disable_terminals() {
        std::env::set_var("GRIT_NO_TERMINAL", "1");
    }

    #[test]
    fn keep_open_payload_runs_script() {
        let payload = keep_open_payload(Path::new("/repo/scripts/menu.sh"));
        assert_eq!(payload, "\"/repo/scripts/menu.sh\"");
    }

    #[cfg(unix)]
    #[test]
    fn find_in_path_locates_shell() {
        assert!(find_in_path("sh").is_some());
        assert!(find_in_path("definitely-not-a-real-binary-xyz").is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn term_program_maps_to_the_current_terminal() {
        std::env::set_var("TERM_PROGRAM", "ghostty");
        let (program, flags) = term_program_candidate().unwrap();
        assert_eq!(program, "ghostty");
        assert_eq!(flags, vec!["-e".to_string()]);

        std::env::set_var("TERM_PROGRAM", "vscode");
        assert!(term_program_candidate().is_none(), "unmapped names are skipped");
        std::env::remove_var("TERM_PROGRAM");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn terminal_names_normalize_to_flag_table_entries() {
        assert_eq!(normalize_terminal_name("gnome-terminal-server"), "gnome-terminal");
        assert_eq!(normalize_terminal_name("gnome-terminal-"), "gnome-terminal");
        assert_eq!(normalize_terminal_name("kitty.exe"), "kitty");
        assert_eq!(normalize_terminal_name("konsole"), "konsole");
        assert!(flags_for("gnome-terminal").is_some());
        assert!(flags_for("kgx").is_some());
        assert!(flags_for("bash").is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn probe_list_leads_with_explicit_terminal_and_dedupes() {
        std::env::set_var("TERMINAL", "foot");
        let (attempts, unknown) = build_terminal_probe_list();
        std::env::remove_var("TERMINAL");

        assert_eq!(attempts[0].0, "foot");
        assert_eq!(attempts[0].1, vec!["-e".to_string()]);

        // First occurrence wins: no program may be probed twice.
        for (i, (program, _)) in attempts.iter().enumerate() {
            assert!(
                !attempts[..i].iter().any(|(p, _)| p == program),
                "duplicate probe entry: {program}"
            );
        }
        assert!(
            unknown
                .iter()
                .all(|p| !attempts.iter().any(|(a, _)| a == p)),
            "positional fallbacks must not duplicate flagged probes"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn desktop_preferences_follow_xdg_current_desktop() {
        std::env::set_var("XDG_CURRENT_DESKTOP", "ubuntu:GNOME");
        let prefs = desktop_preferences();
        assert_eq!(prefs[0].0, "gnome-terminal");

        std::env::set_var("XDG_CURRENT_DESKTOP", "KDE");
        assert_eq!(desktop_preferences()[0].0, "konsole");

        std::env::remove_var("XDG_CURRENT_DESKTOP");
        assert!(desktop_preferences().is_empty());
    }

    #[cfg(not(target_os = "macos"))]
    fn decode_entry(content: &str) -> freedesktop_desktop_entry::DesktopEntry {
        use freedesktop_desktop_entry::DesktopEntry;
        DesktopEntry::from_str(
            "/tmp/fake/org.example.Term.desktop",
            content,
            None as Option<&[String]>,
        )
        .expect("fixture decodes")
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn primary_binary_prefers_try_exec_and_basenames() {
        let entry = decode_entry(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=T\n\
             Exec=/usr/local/bin/weird-term --flag\n\
             TryExec=weird-term\n\
             Categories=TerminalEmulator;\n",
        );
        assert_eq!(primary_binary(&entry).as_deref(), Some("weird-term"));

        let entry = decode_entry(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=T\n\
             Exec=ptyxis --new-window\n\
             Categories=TerminalEmulator;System;\n",
        );
        assert_eq!(primary_binary(&entry).as_deref(), Some("ptyxis"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn primary_binary_skips_flatpak_wrappers() {
        let entry = decode_entry(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=T\n\
             Exec=flatpak run org.some.Terminal\n\
             Categories=TerminalEmulator;\n",
        );
        assert_eq!(primary_binary(&entry), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn desktop_file_scan_picks_up_installed_terminals() {
        // Only assertable when this machine actually has Ptyxis installed.
        if !Path::new("/usr/share/applications/org.gnome.Ptyxis.desktop").exists() {
            return;
        }
        let (known, _) = desktop_file_terminals();
        assert!(
            known.iter().any(|b| b == "ptyxis"),
            "expected ptyxis among {known:?}"
        );
    }

    #[test]
    fn launch_runs_script_to_completion_detached() {
        disable_terminals();
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
        for _ in 0..150 {
            if dir.path().join("marker").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(dir.path().join("marker").exists(), "script side effect missing");
    }

    #[test]
    fn launch_runs_shebangless_script_via_sh_fallback() {
        disable_terminals();
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("plain");
        fs::write(
            &script_path,
            format!("touch {}/marker\nsleep 30\n", dir.path().display()),
        )
        .unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        launch(dir.path(), "plain").expect("sh fallback must launch shebangless scripts");

        for _ in 0..150 {
            if dir.path().join("marker").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
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
