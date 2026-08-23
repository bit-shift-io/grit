pub mod types;
pub mod watcher;

pub use types::{
    CommitInfo, CommitSummary, FileChange, FilePair, FileStat, GitAction, GitStatus, LogEntry,
    LogStatus, RepoState,
};

use std::cell::{Cell, RefCell};
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitError {
    pub message: String,
    pub stderr: String,
    pub stdout: String,
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let combined = format!("{}{}", self.stderr.trim(), self.stdout.trim());
        if combined.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.message, combined)
        }
    }
}

impl std::error::Error for GitError {}

/// Per-entry cap so a chatty `push`/`clone` cannot flood the web UI log.
const MAX_LOG_OUTPUT_BYTES: usize = 64 * 1024;

thread_local! {
    /// Only set while [`execute_action_logged`] is on the stack, so status
    /// refreshes and diff reads never pollute the action log.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static PENDING_LOG: RefCell<Vec<LogEntry>> = const { RefCell::new(Vec::new()) };
}

/// Appends to the in-flight log when recording is active; a no-op otherwise.
fn record_entry(entry: LogEntry) {
    if RECORDING.with(Cell::get) {
        PENDING_LOG.with(|pending| pending.borrow_mut().push(entry));
    }
}

/// Records a command-free synthetic entry (used where no `run()` happened).
fn record_synthetic(command: &str, output: impl Into<String>, status: LogStatus) {
    record_entry(LogEntry {
        seq: 0,
        command: command.to_string(),
        output: output.into(),
        status,
        started_ms: epoch_millis(),
        duration_ms: 0,
    });
}

fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Renders a `Command` back into its shell form for the log, e.g.
/// `git add -- src/main.rs`.
fn describe_command(cmd: &Command) -> String {
    let mut line = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        line.push(' ');
        line.push_str(&arg.to_string_lossy());
    }
    line
}

fn truncate_output(mut out: String) -> String {
    if out.len() > MAX_LOG_OUTPUT_BYTES {
        let mut cut = MAX_LOG_OUTPUT_BYTES;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n… output truncated …");
    }
    out
}

fn git_command(repo_path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    cmd.env("LC_ALL", "C");
    cmd
}

fn run(cmd: &mut Command) -> Result<String, GitError> {
    let command_line = describe_command(cmd);
    let started_ms = epoch_millis();
    let started = Instant::now();

    let output = match cmd.output() {
        Ok(output) => output,
        Err(e) => {
            record_synthetic(
                &command_line,
                format!("failed to execute git: {e}"),
                LogStatus::Failed,
            );
            return Err(GitError {
                message: format!("failed to execute git: {e}"),
                stderr: String::new(),
                stdout: String::new(),
            });
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let duration_ms = started.elapsed().as_millis() as u64;

    // Terminal-like transcript: both streams verbatim, errors last.
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr.trim_end());
    }

    let status = if output.status.success() {
        LogStatus::Success
    } else {
        LogStatus::Failed
    };
    record_entry(LogEntry {
        seq: 0,
        command: command_line,
        output: truncate_output(combined),
        status,
        started_ms,
        duration_ms,
    });

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(GitError {
            message: "git command failed".to_string(),
            stderr,
            stdout,
        })
    }
}

/// Shell-style preview of what an action will do. Broadcast immediately
/// when a client action arrives so the log shows the command was entered
/// even while it is still running; replaced by the real per-command
/// entries once execution finishes.
pub fn placeholder_command(action: &GitAction) -> String {
    match action {
        GitAction::Stage(path) => format!("git add -- {path}"),
        GitAction::Unstage(path) => format!("git reset HEAD -- {path}"),
        GitAction::Discard(path) => format!("git restore --staged --worktree -- {path}"),
        GitAction::Commit(message) => format!("git commit -m \"{message}\""),
        GitAction::CommitAll(message) => format!("git add -A && git commit -m \"{message}\""),
        GitAction::CommitAllPush(message) => {
            format!("git add -A && git commit -m \"{message}\" && git push")
        }
        GitAction::DiscardAll => "git reset --hard HEAD".to_string(),
        GitAction::Push => "git push".to_string(),
        GitAction::Pull => "git pull".to_string(),
        GitAction::Fetch => "git fetch".to_string(),
        GitAction::CheckoutBranch(branch) => format!("git checkout {branch}"),
        GitAction::Revert(hash) => format!("git revert --no-edit {hash}"),
        GitAction::CreateBranch(name, from) => format!("git checkout -b {name} {from}"),
        GitAction::CreateTag(name, target) => format!("git tag {name} {target}"),
        GitAction::DeleteTag(name) => format!("git tag -d {name}"),
        GitAction::DeleteBranch(name) => format!("git branch -d {name}"),
        GitAction::Nuke => "git fetch origin && git reset --hard origin/<branch> && git clean -fdx"
            .to_string(),
        GitAction::RunScript(rel_path) => format!("./{rel_path}"),
        GitAction::NewTab(_) | GitAction::CloseTab => String::new(),
    }
}

pub fn get_repository_status(repo_path: &Path) -> Result<RepoState, GitError> {
    let current_branch = get_current_branch(repo_path)?;
    let branches = list_branches(repo_path)?;
    let changes = list_changes(repo_path)?;
    let history = get_history(repo_path)?;

    Ok(RepoState {
        current_branch,
        branches,
        changes,
        history,
        scripts: crate::actions::discover(repo_path),
    })
}

fn get_current_branch(repo_path: &Path) -> Result<String, GitError> {
    let branch = run(git_command(repo_path).args(["symbolic-ref", "--short", "HEAD"]))?;
    Ok(branch.trim().to_string())
}

fn list_branches(repo_path: &Path) -> Result<Vec<String>, GitError> {
    let output = run(git_command(repo_path).args(["branch", "--format=%(refname:short)"]))?;
    Ok(output
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

fn list_changes(repo_path: &Path) -> Result<Vec<FileChange>, GitError> {
    let mut changes = Vec::new();

    let staged = run(
        git_command(repo_path)
            .args(["diff", "--name-status", "--cached", "--diff-filter=ACMRD"]),
    )?;
    for line in staged.lines() {
        if let Some((status, path)) = parse_status_line(line) {
            changes.push(FileChange {
                path: path.to_string(),
                status: if status == "R" {
                    GitStatus::Renamed
                } else if status == "D" {
                    GitStatus::Deleted
                } else {
                    GitStatus::Staged
                },
                is_staged: true,
            });
        }
    }

    let unstaged = run(
        git_command(repo_path)
            .args(["diff", "--name-status", "--diff-filter=ACMRD"]),
    )?;
    for line in unstaged.lines() {
        if let Some((status, path)) = parse_status_line(line) {
            changes.push(FileChange {
                path: path.to_string(),
                status: if status == "R" {
                    GitStatus::Renamed
                } else if status == "D" {
                    GitStatus::Deleted
                } else {
                    GitStatus::Modified
                },
                is_staged: false,
            });
        }
    }

    let untracked = run(
        git_command(repo_path)
            .args(["ls-files", "--others", "--exclude-standard"]),
    )?;
    for path in untracked.lines() {
        let path = path.trim();
        if !path.is_empty() {
            changes.push(FileChange {
                path: path.to_string(),
                status: GitStatus::Untracked,
                is_staged: false,
            });
        }
    }

    Ok(changes)
}

fn parse_status_line(line: &str) -> Option<(&str, &str)> {
    let mut parts = line.splitn(2, '\t');
    let status = parts.next()?.trim();
    let path = parts.next()?.trim();
    if path.is_empty() {
        return None;
    }
    Some((status, path))
}

fn get_history(repo_path: &Path) -> Result<Vec<CommitInfo>, GitError> {
    let output = match run(
        git_command(repo_path).args([
            "log",
            "--format=%H%x09%an%x09%ct%x09%s",
            "-n",
            "50",
        ]),
    ) {
        Ok(output) => output,
        Err(e) if e.stderr.contains("does not have any commits") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut history = Vec::new();
    for line in output.lines() {
        let mut parts = line.splitn(4, '\t');
        let hash = parts.next().unwrap_or_default().trim();
        let author = parts.next().unwrap_or_default().trim();
        let timestamp = parts.next().unwrap_or_default().trim();
        let message = parts.next().unwrap_or_default();

        if hash.is_empty() {
            continue;
        }

        history.push(CommitInfo {
            hash: hash.to_string(),
            author: author.to_string(),
            message: message.to_string(),
            timestamp: timestamp.parse().unwrap_or(0),
        });
    }
    Ok(history)
}

pub fn get_file_diff(repo_path: &Path, path: &str) -> Result<String, GitError> {
    let diff = run(git_command(repo_path).args(["diff", "HEAD", "--", path]));

    match diff {
        Ok(output) => {
            if !output.trim().is_empty() {
                return Ok(output);
            }
        }
        Err(_) => {}
    }

    let staged = run(git_command(repo_path).args(["diff", "--cached", "--", path]));
    if let Ok(output) = staged {
        if !output.trim().is_empty() {
            return Ok(output);
        }
    }

    let unstaged = run(git_command(repo_path).args(["diff", "--", path]));
    if let Ok(output) = unstaged {
        if !output.trim().is_empty() {
            return Ok(output);
        }
    }

    let untracked = run(
        git_command(repo_path).args(["ls-files", "--others", "--exclude-standard", "--", path]),
    );
    if let Ok(output) = untracked {
        if output.trim() == path {
            let full_path = repo_path.join(path);
            match std::fs::read_to_string(&full_path) {
                Ok(content) => {
                    let mut diff = format!(
                        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n"
                    );
                    for line in content.lines() {
                        diff.push_str(&format!("+{line}\n"));
                    }
                    return Ok(diff);
                }
                Err(e) => {
                    return Err(GitError {
                        message: format!("failed to read untracked file {path}: {e}"),
                        stderr: String::new(),
                        stdout: String::new(),
                    });
                }
            }
        }
    }

    Ok(String::new())
}

pub fn get_file_pair(repo_path: &Path, path: &str) -> Result<FilePair, GitError> {
    let original = run(git_command(repo_path).args(["show", &format!("HEAD:{path}")]))
        .unwrap_or_default();
    let current = std::fs::read_to_string(repo_path.join(path)).unwrap_or_default();
    Ok(FilePair { original, current })
}

pub fn get_commit_summary(repo_path: &Path, hash: &str) -> Result<CommitSummary, GitError> {
    let meta = run(
        git_command(repo_path).args(["show", "-s", "--format=%an%x09%ct%x09%B", hash]),
    )?;
    let mut lines = meta.lines();
    let header = lines.next().unwrap_or_default();
    let mut parts = header.splitn(3, '\t');
    let author = parts.next().unwrap_or_default().to_string();
    let timestamp: i64 = parts.next().unwrap_or_default().trim().parse().unwrap_or(0);
    let subject = parts.next().unwrap_or_default().to_string();

    let mut body: Vec<&str> = lines.collect();
    while body.last().map(|l| l.is_empty()).unwrap_or(false) {
        body.pop();
    }
    let mut message = subject;
    if !body.is_empty() {
        message.push('\n');
        message.push_str(&body.join("\n"));
    }

    let mut files_changed = 0;
    let mut insertions = 0;
    let mut deletions = 0;
    if let Ok(stat) =
        run(git_command(repo_path).args(["show", "--format=", "--shortstat", hash]))
    {
        if let Some(line) = stat.lines().find(|l| l.contains("changed")) {
            for part in line.split(',') {
                let part = part.trim();
                let num: i64 = part
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if part.contains("insertion") {
                    insertions = num;
                } else if part.contains("deletion") {
                    deletions = num;
                } else {
                    files_changed = num;
                }
            }
        }
    }

    let mut files = Vec::new();
    let name_status = run(git_command(repo_path).args(["show", "--format=", "--name-status", hash]))
        .unwrap_or_default();
    let numstat = run(git_command(repo_path).args(["show", "--format=", "--numstat", hash]))
        .unwrap_or_default();
    let name_lines: Vec<&str> = name_status.lines().filter(|l| !l.trim().is_empty()).collect();
    let num_lines: Vec<&str> = numstat.lines().filter(|l| !l.trim().is_empty()).collect();
    for (i, line) in name_lines.iter().enumerate() {
        let mut fields = line.splitn(3, '\t');
        let status_letter = fields
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .next()
            .unwrap_or('M');
        let path = fields.last().unwrap_or_default().trim().to_string();
        if path.is_empty() {
            continue;
        }
        let status = match status_letter {
            'A' => "Added",
            'D' => "Deleted",
            'M' => "Modified",
            'R' => "Renamed",
            'C' => "Copied",
            'T' => "Type Changed",
            _ => "Changed",
        };
        let mut insertions = 0;
        let mut deletions = 0;
        if let Some(num) = num_lines.get(i) {
            let mut nf = num.splitn(3, '\t');
            insertions = nf.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            deletions = nf.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        }
        files.push(FileStat {
            status: status.to_string(),
            path,
            insertions,
            deletions,
        });
    }

    Ok(CommitSummary {
        message,
        author,
        timestamp,
        files_changed,
        insertions,
        deletions,
        files,
    })
}

pub fn execute_action(repo_path: &Path, action: GitAction) -> Result<(), GitError> {
    match action {
        GitAction::Stage(path) => {
            run(git_command(repo_path).args(["add", "--", path.as_str()]))?;
        }
        GitAction::Unstage(path) => {
            run(git_command(repo_path).args(["reset", "HEAD", "--", path.as_str()]))?;
        }
        GitAction::Discard(path) => {
            run(git_command(repo_path).args([
                "restore",
                "--staged",
                "--worktree",
                "--",
                path.as_str(),
            ]))?;
        }
        GitAction::Commit(message) => {
            run(git_command(repo_path).args(["commit", "-m", message.as_str()]))?;
        }
        GitAction::CommitAll(message) => {
            run(git_command(repo_path).args(["add", "-A"]))?;
            run(git_command(repo_path).args(["commit", "-m", message.as_str()]))?;
        }
        GitAction::CommitAllPush(message) => {
            run(git_command(repo_path).args(["add", "-A"]))?;
            run(git_command(repo_path).args(["commit", "-m", message.as_str()]))?;
            run(git_command(repo_path).args(["push"]))?;
        }
        GitAction::DiscardAll => {
            run(git_command(repo_path).args(["reset", "--hard", "HEAD"]))?;
        }
        GitAction::Push => {
            run(git_command(repo_path).args(["push"]))?;
        }
        GitAction::Pull => {
            run(git_command(repo_path).args(["pull"]))?;
        }
        GitAction::Fetch => {
            run(git_command(repo_path).args(["fetch"]))?;
        }
        GitAction::CheckoutBranch(branch) => {
            run(git_command(repo_path).args(["checkout", branch.as_str()]))?;
        }
        GitAction::Revert(hash) => {
            run(git_command(repo_path).args(["revert", "--no-edit", hash.as_str()]))?;
        }
        GitAction::CreateBranch(name, from) => {
            run(git_command(repo_path).args([
                "checkout",
                "-b",
                name.as_str(),
                from.as_str(),
            ]))?;
        }
        GitAction::CreateTag(name, target) => {
            run(git_command(repo_path).args(["tag", name.as_str(), target.as_str()]))?;
        }
        GitAction::DeleteTag(name) => {
            run(git_command(repo_path).args(["tag", "-d", name.as_str()]))?;
        }
        GitAction::DeleteBranch(name) => {
            run(git_command(repo_path).args(["branch", "-d", name.as_str()]))?;
        }
        GitAction::Nuke => {
            nuke_repo(repo_path)?;
        }
        GitAction::RunScript(rel_path) => {
            match crate::actions::launch(repo_path, &rel_path) {
                Ok(()) => record_synthetic(
                    &format!("./{rel_path}"),
                    "launched in a separate terminal window",
                    LogStatus::Success,
                ),
                Err(message) => {
                    record_synthetic(&format!("./{rel_path}"), message.clone(), LogStatus::Failed);
                    return Err(GitError {
                        message,
                        stderr: String::new(),
                        stdout: String::new(),
                    });
                }
            }
        }
        GitAction::NewTab(_) | GitAction::CloseTab => {}
    }
    Ok(())
}

/// Runs an action while capturing every executed git command and its
/// output. Returns the action result plus the transcript in execution
/// order; entries are produced even when the action fails mid-way.
/// Must be called from a single thread (it uses a thread-local buffer).
pub fn execute_action_logged(
    repo_path: &Path,
    action: GitAction,
) -> (Result<(), GitError>, Vec<LogEntry>) {
    RECORDING.with(|r| r.set(true));
    PENDING_LOG.with(|p| p.borrow_mut().clear());
    let result = execute_action(repo_path, action);
    let log = PENDING_LOG.with(|p| std::mem::take(&mut *p.borrow_mut()));
    RECORDING.with(|r| r.set(false));
    (result, log)
}

fn nuke_repo(repo_path: &Path) -> Result<(), GitError> {
    let branch = get_current_branch(repo_path)?;

    let fetched = run(git_command(repo_path).args(["fetch", "origin"]));
    match fetched {
        Ok(_) => {
            run(git_command(repo_path).args([
                "reset",
                "--hard",
                &format!("origin/{branch}"),
            ]))?;
        }
        Err(e) => {
            let missing_remote = e.stderr.contains("does not appear to be a git repository")
                || e.stderr.contains("Could not read from remote")
                || e.stderr.contains("No such remote")
                || e.stderr.contains("Could not resolve host");
            if missing_remote {
                run(git_command(repo_path).args(["reset", "--hard", "HEAD"]))?;
            } else {
                return Err(e);
            }
        }
    }

    run(git_command(repo_path).args(["clean", "-fdx"]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command as OsCommand;

    fn init_repo(dir: &Path) {
        OsCommand::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .output()
            .unwrap();
        OsCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        OsCommand::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    fn commit_all(dir: &Path, message: &str) {
        OsCommand::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .unwrap();
        OsCommand::new("git")
            .args(["commit", "-q", "-m", message])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    #[test]
    fn status_reports_discovered_scripts() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::create_dir(dir.path().join("scripts")).unwrap();
        fs::write(dir.path().join("scripts/build.sh"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(
            dir.path().join("scripts/build.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert_eq!(state.scripts.len(), 1);
        assert_eq!(state.scripts[0].rel_path, "scripts/build.sh");
        assert_eq!(state.scripts[0].name, "build.sh");
    }

    #[test]
    fn status_reports_branch_and_clean_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        commit_all(dir.path(), "initial");

        let state = get_repository_status(dir.path()).unwrap();
        assert_eq!(state.current_branch, "main");
        assert!(state.changes.is_empty());
        assert_eq!(state.history.len(), 1);
        assert_eq!(state.history[0].message, "initial");
        assert!(state.branches.contains(&"main".to_string()));
    }

    #[test]
    fn status_reports_untracked_and_modified() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("tracked.txt"), "v1").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("tracked.txt"), "v2").unwrap();
        fs::write(dir.path().join("new.txt"), "new").unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert!(state
            .changes
            .iter()
            .any(|c| c.path == "new.txt" && c.status == GitStatus::Untracked));
        assert!(state
            .changes
            .iter()
            .any(|c| c.path == "tracked.txt" && c.status == GitStatus::Modified && !c.is_staged));
    }

    #[test]
    fn stage_unstage_and_commit_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("file.txt"), "world").unwrap();

        execute_action(dir.path(), GitAction::Stage("file.txt".to_string())).unwrap();
        let state = get_repository_status(dir.path()).unwrap();
        assert!(state
            .changes
            .iter()
            .any(|c| c.path == "file.txt" && c.is_staged));

        execute_action(dir.path(), GitAction::Unstage("file.txt".to_string())).unwrap();
        let state = get_repository_status(dir.path()).unwrap();
        assert!(!state
            .changes
            .iter()
            .any(|c| c.path == "file.txt" && c.is_staged));

        execute_action(dir.path(), GitAction::Stage("file.txt".to_string())).unwrap();
        execute_action(dir.path(), GitAction::Commit("second".to_string())).unwrap();
        let state = get_repository_status(dir.path()).unwrap();
        assert_eq!(state.history.len(), 2);
        assert_eq!(state.history[0].message, "second");
    }

    #[test]
    fn discard_file_restores_tracked_content() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("a.txt"), "v2\n").unwrap();
        execute_action(dir.path(), GitAction::Stage("a.txt".to_string())).unwrap();

        execute_action(dir.path(), GitAction::Discard("a.txt".to_string())).unwrap();

        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "v1\n"
        );
        let state = get_repository_status(dir.path()).unwrap();
        assert!(
            !state.changes.iter().any(|c| c.path == "a.txt"),
            "got: {:?}",
            state.changes
        );
    }

    #[test]
    fn commit_all_stages_and_commits_everything() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("tracked.txt"), "v1\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("tracked.txt"), "v2\n").unwrap();
        fs::write(dir.path().join("new.txt"), "new\n").unwrap();

        execute_action(dir.path(), GitAction::CommitAll("all the things".to_string())).unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert!(state.changes.is_empty(), "got changes: {:?}", state.changes);
        assert_eq!(state.history[0].message, "all the things");
    }

    #[test]
    fn discard_all_resets_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("a.txt"), "v2\n").unwrap();
        fs::write(dir.path().join("b.txt"), "new\n").unwrap();
        execute_action(dir.path(), GitAction::Stage("a.txt".to_string())).unwrap();

        execute_action(dir.path(), GitAction::DiscardAll).unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "v1\n"
        );
        assert!(dir.path().join("b.txt").exists(), "untracked kept");
        assert!(state
            .changes
            .iter()
            .all(|c| c.path == "b.txt"), "got: {:?}", state.changes);
    }

    #[test]
    fn commit_all_push_commits_and_pushes_to_origin() {
        let origin = tempfile::tempdir().unwrap();
        OsCommand::new("git")
            .args(["init", "-q", "--bare"])
            .current_dir(origin.path())
            .output()
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        OsCommand::new("git")
            .args(["clone", "-q", origin.path().to_str().unwrap(), "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        OsCommand::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        OsCommand::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        commit_all(dir.path(), "initial");
        OsCommand::new("git")
            .args(["push", "-u", "origin", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("a.txt"), "v2\n").unwrap();

        execute_action(dir.path(), GitAction::CommitAllPush("ship it".to_string())).unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert!(state.changes.is_empty(), "got changes: {:?}", state.changes);
        let remote_log = OsCommand::new("git")
            .args(["-C", origin.path().to_str().unwrap(), "log", "--oneline", "-1"])
            .output()
            .unwrap();
        let remote_log = String::from_utf8(remote_log.stdout).unwrap();
        assert!(remote_log.contains("ship it"), "got: {remote_log}");
    }

    #[test]
    fn status_handles_repo_with_no_commits() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("new.txt"), "new").unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert_eq!(state.current_branch, "main");
        assert!(state.history.is_empty());
        assert!(state
            .changes
            .iter()
            .any(|c| c.path == "new.txt" && c.status == GitStatus::Untracked));
    }

    #[test]
    fn error_captures_git_output() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let err = execute_action(
            dir.path(),
            GitAction::Commit("no changes".to_string()),
        )
        .unwrap_err();
        let display = err.to_string();
        assert!(display.contains("nothing to commit"), "got: {display}");
    }

    #[test]
    fn execute_action_logged_captures_commands_and_output() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");

        let (result, log) =
            execute_action_logged(dir.path(), GitAction::Stage("file.txt".to_string()));
        assert!(result.is_ok());
        assert_eq!(log.len(), 1, "got: {log:?}");
        assert_eq!(log[0].command, "git add -- file.txt");
        assert_eq!(log[0].status, LogStatus::Success);
    }

    #[test]
    fn execute_action_logged_captures_multi_command_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();

        // CommitAllPush runs add + commit (+ push); push fails with no remote.
        let (result, log) =
            execute_action_logged(dir.path(), GitAction::CommitAllPush("ship".to_string()));
        assert!(result.is_err());
        let commands: Vec<&str> = log.iter().map(|e| e.command.as_str()).collect();
        assert!(
            commands.iter().any(|c| c.starts_with("git add -A")),
            "got: {commands:?}"
        );
        assert!(
            commands.iter().any(|c| c.contains("git commit")),
            "got: {commands:?}"
        );
        assert!(
            log.iter()
                .any(|e| e.status == LogStatus::Failed && !e.output.is_empty()),
            "failed entries must carry git's own output: {log:?}"
        );
        assert!(
            log.last().map(|e| e.status == LogStatus::Failed).unwrap_or(false),
            "transcript must end with the failing command: {log:?}"
        );
    }

    #[test]
    fn failed_commit_log_carries_git_stderr() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let (_, log) =
            execute_action_logged(dir.path(), GitAction::Commit("empty".to_string()));
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].status, LogStatus::Failed);
        assert!(
            log[0].output.contains("nothing to commit"),
            "got: {:?}",
            log[0].output
        );
    }

    #[test]
    fn placeholder_command_previews_real_invocations() {
        assert_eq!(
            placeholder_command(&GitAction::Pull),
            "git pull"
        );
        assert_eq!(
            placeholder_command(&GitAction::Stage("a b.txt".to_string())),
            "git add -- a b.txt"
        );
        assert!(placeholder_command(&GitAction::CloseTab).is_empty());
    }

    #[test]
    fn checkout_branch_switches() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        commit_all(dir.path(), "initial");
        OsCommand::new("git")
            .args(["branch", "feature"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        execute_action(
            dir.path(),
            GitAction::CheckoutBranch("feature".to_string()),
        )
        .unwrap();
        let state = get_repository_status(dir.path()).unwrap();
        assert_eq!(state.current_branch, "feature");
    }

    #[test]
    fn get_file_diff_reports_unstaged_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("file.txt"), "world\n").unwrap();

        let diff = get_file_diff(dir.path(), "file.txt").unwrap();
        assert!(diff.contains("-hello"), "got: {diff}");
        assert!(diff.contains("+world"), "got: {diff}");
    }

    #[test]
    fn get_file_diff_reports_staged_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("file.txt"), "staged\n").unwrap();
        execute_action(dir.path(), GitAction::Stage("file.txt".to_string())).unwrap();

        let diff = get_file_diff(dir.path(), "file.txt").unwrap();
        assert!(diff.contains("-hello"), "got: {diff}");
        assert!(diff.contains("+staged"), "got: {diff}");
    }

    #[test]
    fn get_file_diff_reports_untracked_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("new.txt"), "brand new\n").unwrap();

        let diff = get_file_diff(dir.path(), "new.txt").unwrap();
        assert!(diff.contains("new.txt"), "got: {diff}");
        assert!(diff.contains("+brand new"), "got: {diff}");
    }

    #[test]
    fn get_file_pair_returns_original_and_current() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("file.txt"), "world\n").unwrap();

        let pair = get_file_pair(dir.path(), "file.txt").unwrap();
        assert_eq!(pair.original, "hello\n");
        assert_eq!(pair.current, "world\n");
    }

    #[test]
    fn get_file_pair_returns_empty_original_for_untracked() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("new.txt"), "brand new\n").unwrap();

        let pair = get_file_pair(dir.path(), "new.txt").unwrap();
        assert_eq!(pair.original, "");
        assert_eq!(pair.current, "brand new\n");
    }

    #[test]
    fn create_branch_from_commit_and_switches() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        let hash = get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();

        execute_action(
            dir.path(),
            GitAction::CreateBranch("feature".to_string(), hash),
        )
        .unwrap();

        let output = Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "feature");
    }

    #[test]
    fn get_commit_summary_lists_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("keep.txt"), "a\n").unwrap();
        fs::write(dir.path().join("gone.txt"), "b\n").unwrap();
        commit_all(dir.path(), "first");
        fs::write(dir.path().join("keep.txt"), "a\nb\nc\n").unwrap();
        fs::write(dir.path().join("new.txt"), "x\ny\n").unwrap();
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        commit_all(dir.path(), "second");
        let hash = get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();

        let summary = get_commit_summary(dir.path(), &hash).unwrap();
        assert_eq!(summary.files.len(), 3);
        let keep = summary.files.iter().find(|f| f.path == "keep.txt").unwrap();
        assert_eq!(keep.status, "Modified");
        assert_eq!(keep.insertions, 2);
        assert_eq!(keep.deletions, 0);
        let new = summary.files.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!(new.status, "Added");
        assert_eq!(new.insertions, 2);
        let gone = summary.files.iter().find(|f| f.path == "gone.txt").unwrap();
        assert_eq!(gone.status, "Deleted");
        assert_eq!(gone.deletions, 1);
    }

    #[test]
    fn get_commit_summary_reports_stats() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "line1\nline2\n").unwrap();
        commit_all(dir.path(), "initial commit");
        let hash = get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();

        let summary = get_commit_summary(dir.path(), &hash).unwrap();
        assert_eq!(summary.message, "initial commit");
        assert_eq!(summary.author, "Test User");
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 2);
        assert_eq!(summary.deletions, 0);
    }

    #[test]
    fn create_and_delete_tag_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        let hash = get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();

        execute_action(dir.path(), GitAction::CreateTag("v1.0".to_string(), hash)).unwrap();
        let tags = Command::new("git")
            .args(["tag", "--list"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(tags.stdout).unwrap().trim(), "v1.0");

        execute_action(dir.path(), GitAction::DeleteTag("v1.0".to_string())).unwrap();
        let tags = Command::new("git")
            .args(["tag", "--list"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(String::from_utf8(tags.stdout).unwrap().trim().is_empty());
    }

    #[test]
    fn delete_branch_removes_other_branch() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");
        let hash = get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();
        execute_action(
            dir.path(),
            GitAction::CreateBranch("feature".to_string(), hash),
        )
        .unwrap();
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        execute_action(dir.path(), GitAction::DeleteBranch("feature".to_string())).unwrap();
        let branches = get_repository_status(dir.path()).unwrap().branches;
        assert!(!branches.contains(&"feature".to_string()), "got: {branches:?}");
    }

    #[test]
    fn nuke_discards_all_local_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        fs::write(dir.path().join("other.txt"), "other\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("file.txt"), "local edit\n").unwrap();
        fs::write(dir.path().join("untracked.txt"), "junk\n").unwrap();
        execute_action(dir.path(), GitAction::Stage("file.txt".to_string())).unwrap();

        execute_action(dir.path(), GitAction::Nuke).unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert!(state.changes.is_empty(), "got changes: {:?}", state.changes);
        assert_eq!(fs::read_to_string(dir.path().join("file.txt")).unwrap(), "hello\n");
        assert!(!dir.path().join("untracked.txt").exists());
    }

    #[test]
    fn nuke_resets_to_remote_origin() {
        let origin = tempfile::tempdir().unwrap();
        init_repo(origin.path());
        fs::write(origin.path().join("a.txt"), "v1\n").unwrap();
        commit_all(origin.path(), "initial");
        fs::write(origin.path().join("a.txt"), "v2\n").unwrap();
        commit_all(origin.path(), "second");

        let dir = tempfile::tempdir().unwrap();
        OsCommand::new("git")
            .args(["clone", "-q", origin.path().to_str().unwrap(), "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("a.txt"), "local hack\n").unwrap();
        fs::write(dir.path().join("local.txt"), "junk\n").unwrap();
        execute_action(dir.path(), GitAction::Stage("a.txt".to_string())).unwrap();

        execute_action(dir.path(), GitAction::Nuke).unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert!(state.changes.is_empty(), "got changes: {:?}", state.changes);
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "v2\n",
            "working tree should match remote HEAD"
        );
        assert!(!dir.path().join("local.txt").exists());
    }
}