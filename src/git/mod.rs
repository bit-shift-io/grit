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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Maximum commits fetched for the History panel.
const HISTORY_LIMIT: &str = "50";

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

pub(crate) fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Renders a `Command` back into its shell form for the log, e.g.
/// `git add -- src/main.rs`.
/// Renders a program plus its argv as a single space-joined line. Shared
/// by transcript logging and action previews so both always look alike.
fn format_argv<I, A>(program: &str, args: I) -> String
where
    I: IntoIterator<Item = A>,
    A: AsRef<str>,
{
    let mut line = program.to_string();
    for arg in args {
        line.push(' ');
        line.push_str(arg.as_ref());
    }
    line
}

fn describe_command(cmd: &Command) -> String {
    format_argv(
        &cmd.get_program().to_string_lossy(),
        cmd.get_args().map(|a| a.to_string_lossy()),
    )
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

/// Minimum interval between live progress snapshots pushed to the sink, so
/// chatty commands (clone, push) cannot flood clients with broadcasts.
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(150);

/// Callback receiving a snapshot of a command's combined output while it
/// runs. Installed by [`execute_action_logged`] and consumed by the daemon
/// to revise the in-flight log entry in place.
pub type ProgressSink = std::sync::Arc<dyn Fn(String) + Send + Sync>;

thread_local! {
    /// Set together with `RECORDING` by [`execute_action_logged`]; when
    /// present, commands run through piped streaming and push throttled
    /// output snapshots to the sink as they execute.
    static PROGRESS: RefCell<Option<ProgressSink>> = const { RefCell::new(None) };
}

/// Joins both streams terminal-style for transcripts and live snapshots:
/// stdout first, errors last, empty streams omitted.
fn combine_streams(stdout: &str, stderr: &str) -> String {
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
    combined
}

fn notify_progress(sink: &ProgressSink, snapshot: String) {
    sink(truncate_output(snapshot));
}

/// Runs `cmd` with piped stdout/stderr, feeding throttled snapshots of the
/// combined output to `sink` while it executes. Returns the full contents
/// of each stream plus the exit success flag; the final transcript keeps
/// the exact shape the blocking path produces.
fn run_streamed(cmd: &mut Command, sink: &ProgressSink) -> std::io::Result<(String, String, bool)> {
    use std::process::Stdio;
    use std::sync::Mutex;

    /// Appends one raw output chunk to its stream buffer and pushes a live
    /// snapshot on first content, then at most once per flush interval.
    /// Carriage returns are normalized to newlines: git draws progress
    /// meters with `\r` redraws, which would otherwise coalesce into a
    /// single giant line delivered only at exit.
    fn flush_chunk(
        buffers: &Mutex<(String, String, Option<Instant>)>,
        is_stdout: bool,
        chunk: String,
        sink: &ProgressSink,
    ) {
        let mut guard = match buffers.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let (stdout, stderr, last_flush) = &mut *guard;
        let chunk = chunk.replace('\r', "\n");
        if is_stdout {
            stdout.push_str(&chunk);
        } else {
            stderr.push_str(&chunk);
        }
        let due = last_flush.map_or(true, |t| t.elapsed() >= STREAM_FLUSH_INTERVAL);
        if due {
            *last_flush = Some(Instant::now());
            let snapshot = combine_streams(stdout, stderr);
            drop(guard);
            notify_progress(sink, snapshot);
        }
    }

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    type Buffers = Mutex<(String, String, Option<Instant>)>;
    let buffers: std::sync::Arc<Buffers> = std::sync::Arc::new(Mutex::default());

    let stdout_pipe = child.stdout.take().expect("child stdout was piped");
    let stderr_pipe = child.stderr.take().expect("child stderr was piped");
    let out_reader = std::thread::spawn({
        let buffers = std::sync::Arc::clone(&buffers);
        let sink = std::sync::Arc::clone(sink);
        move || {
            // Fixed-size chunk reads (not line reads): git's progress
            // meters redraw with `\r` and would buffer until exit.
            let mut pipe = stdout_pipe;
            let mut buf = [0u8; 512];
            loop {
                match std::io::Read::read(&mut pipe, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => flush_chunk(
                        &buffers,
                        true,
                        String::from_utf8_lossy(&buf[..n]).into_owned(),
                        &sink,
                    ),
                }
            }
        }
    });
    let err_reader = std::thread::spawn({
        let buffers = std::sync::Arc::clone(&buffers);
        let sink = std::sync::Arc::clone(sink);
        move || {
            let mut pipe = stderr_pipe;
            let mut buf = [0u8; 512];
            loop {
                match std::io::Read::read(&mut pipe, &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => flush_chunk(
                        &buffers,
                        false,
                        String::from_utf8_lossy(&buf[..n]).into_owned(),
                        &sink,
                    ),
                }
            }
        }
    });

    // Readers own their pipe handles; once both finish the child is done.
    let _ = out_reader.join();
    let _ = err_reader.join();
    let status = child.wait()?;
    let contents = std::sync::Arc::try_unwrap(buffers)
        .ok()
        .and_then(|mutex| mutex.into_inner().ok())
        .unwrap_or_default();
    let (stdout, stderr, _) = contents;
    Ok((stdout, stderr, status.success()))
}

fn run(cmd: &mut Command) -> Result<String, GitError> {
    let command_line = describe_command(cmd);
    let started_ms = epoch_millis();
    let started = Instant::now();

    let progress = PROGRESS.with(|p| p.borrow().clone());
    // Captured (stdout, stderr, exit success); piped + streaming whenever
    // a progress sink is installed, plain blocking capture otherwise.
    let captured: std::io::Result<(String, String, bool)> = match progress.as_ref() {
        Some(sink) => run_streamed(cmd, sink),
        None => cmd.output().map(|o| {
            (
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
                o.status.success(),
            )
        }),
    };
    let (stdout, stderr, success) = match captured {
        Ok(parts) => parts,
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
    let duration_ms = started.elapsed().as_millis() as u64;

    // Terminal-like transcript: both streams verbatim, errors last.
    let combined = combine_streams(&stdout, &stderr);

    let status = if success {
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

    if success {
        Ok(stdout)
    } else {
        Err(GitError {
            message: "git command failed".to_string(),
            stderr,
            stdout,
        })
    }
}

/// The exact argv sequences a table-backed action executes. Single source
/// of truth for both execution (`execute_action`) and its shell-style
/// preview (`placeholder_command`), so they cannot drift apart.
///
/// Returns `None` for actions with bespoke execution (Reclone's
/// delete-and-clone flow, RunScript's terminal launch) or no server-side effect
/// (NewTab/CloseTab).
fn action_argv(action: &GitAction) -> Option<Vec<Vec<String>>> {
    fn seq(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
    let seqs: Vec<Vec<String>> = match action {
        GitAction::Stage(p) => vec![seq(&["add", "--", p])],
        GitAction::Unstage(p) => vec![seq(&["reset", "HEAD", "--", p])],
        GitAction::Discard(p) => vec![seq(&["restore", "--staged", "--worktree", "--", p])],
        GitAction::Commit(m) => vec![seq(&["commit", "-m", m])],
        GitAction::CommitAll(m) => vec![seq(&["add", "-A"]), seq(&["commit", "-m", m])],
        GitAction::CommitAllPush(m) => {
            vec![
                seq(&["add", "-A"]),
                seq(&["commit", "-m", m]),
                seq(&["push"]),
            ]
        }
        GitAction::DiscardAll => vec![seq(&["reset", "--hard", "HEAD"]), seq(&["clean", "-fd"])],
        GitAction::Push => vec![seq(&["push"])],
        GitAction::Pull => vec![seq(&["pull"])],
        GitAction::Fetch => vec![seq(&["fetch", "--prune"])],
        GitAction::CheckoutBranch(b) => vec![seq(&["checkout", b])],
        GitAction::Revert(h) => vec![seq(&["revert", "--no-edit", h])],
        GitAction::CreateBranch(n, f) => vec![seq(&["checkout", "-b", n, f])],
        GitAction::CreateTag(n, t) => vec![seq(&["tag", n, t])],
        GitAction::DeleteTag(n) => vec![seq(&["tag", "-d", n])],
        GitAction::DeleteBranch(n) => vec![seq(&["branch", "-d", n])],
        GitAction::Reclone
            | GitAction::RunScript(_)
            | GitAction::NewTab(_)
            | GitAction::CloseTab
            | GitAction::DiscardUntracked(_)
            | GitAction::SearchHistory(_) => {
            return None;
        }
    };
    Some(seqs)
}

/// Shell-style preview of what an action will do. Broadcast immediately
/// when a client action arrives so the log shows the command was entered
/// even while it is still running; replaced by the real per-command
/// entries once execution finishes.
///
/// Derived from [`action_argv`] and rendered by the same [`format_argv`]
/// used for real transcripts, so previews cannot drift from execution.
pub fn placeholder_command(action: &GitAction) -> String {
    if let Some(seqs) = action_argv(action) {
        return seqs
            .iter()
            .map(|seq| format_argv("git", seq))
            .collect::<Vec<_>>()
            .join(" && ");
    }
    match action {
        GitAction::Reclone => {
            "git remote get-url origin && rm -rf <repo> && git clone <origin-url> <repo>"
                .to_string()
        }
        GitAction::RunScript(rel_path) => format!("./{rel_path}"),
        GitAction::DiscardUntracked(p) => format!("rm -f -- {p}"),
        // Unreachable in practice: every other variant is table-backed
        // and handled above.
        _ => String::new(),
    }
}

pub fn get_repository_status(repo_path: &Path) -> Result<RepoState, GitError> {
    let current_branch = get_current_branch(repo_path)?;
    let branches = list_branches(repo_path)?;
    let remote_branches = list_remote_branches(repo_path)?;
    let changes = list_changes(repo_path)?;
    let history = get_history(repo_path)?;

    Ok(RepoState {
        current_branch,
        branches,
        remote_branches,
        changes,
        history,
        scripts: crate::actions::discover(repo_path),
    })
}

fn get_current_branch(repo_path: &Path) -> Result<String, GitError> {
    match run(git_command(repo_path).args(["symbolic-ref", "--short", "HEAD"])) {
        Ok(branch) => Ok(branch.trim().to_string()),
        Err(_) => {
            // Detached HEAD — fall back to short commit hash.
            let hash = run(git_command(repo_path).args(["rev-parse", "--short", "HEAD"]))?;
            Ok(format!("detached@{}", hash.trim()))
        }
    }
}

fn list_branches(repo_path: &Path) -> Result<Vec<String>, GitError> {
    let output = run(git_command(repo_path).args(["branch", "--format=%(refname:short)"]))?;
    Ok(output
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

fn list_remote_branches(repo_path: &Path) -> Result<Vec<String>, GitError> {
    let output = match run(git_command(repo_path).args(["branch", "-r", "--format=%(refname:short)"])) {
        Ok(output) => output,
        Err(e) if e.stderr.contains("no remote configured") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(output
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.ends_with("/HEAD"))
        .collect())
}

fn list_changes(repo_path: &Path) -> Result<Vec<FileChange>, GitError> {
    let mut changes = Vec::new();

    let staged = run(
        git_command(repo_path)
            .args(["diff", "--name-status", "--cached", "--diff-filter=ACMRD"]),
    )?;
    changes.extend(parse_name_status(&staged, true, GitStatus::Staged));

    let unstaged = run(
        git_command(repo_path)
            .args(["diff", "--name-status", "--diff-filter=ACMRD"]),
    )?;
    changes.extend(parse_name_status(&unstaged, false, GitStatus::Modified));

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
    let mut fields = line.split('\t');
    let status = fields.next()?.trim();
    if status.is_empty() {
        return None;
    }
    // Rename/copy entries carry a similarity score plus both paths; the
    // destination (final field) is the live path.
    if status.starts_with('R') || status.starts_with('C') {
        fields.next()?;
    }
    let path = fields.next()?.trim();
    if path.is_empty() {
        return None;
    }
    Some((status, path))
}

/// Maps `diff --name-status` output to `FileChange`s. Renames and deletions
/// carry their own status; everything else falls back to the caller's
/// staged/unstaged default.
fn parse_name_status(output: &str, is_staged: bool, fallback: GitStatus) -> Vec<FileChange> {
    output
        .lines()
        .filter_map(parse_status_line)
        .map(|(status, path)| FileChange {
            path: path.to_string(),
            status: if status.starts_with('R') {
                GitStatus::Renamed
            } else if status == "D" {
                GitStatus::Deleted
            } else {
                fallback.clone()
            },
            is_staged,
        })
        .collect()
}

fn parse_epoch(field: &str) -> Result<i64, GitError> {
    field.trim().parse::<i64>().map_err(|_| GitError {
        message: format!("malformed commit timestamp {field:?}"),
        stderr: String::new(),
        stdout: String::new(),
    })
}

fn get_history(repo_path: &Path) -> Result<Vec<CommitInfo>, GitError> {
    let output = match run(
        git_command(repo_path).args([
            "log",
            "--format=%H%x09%an%x09%ct%x09%s",
            "-n",
            HISTORY_LIMIT,
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
            timestamp: parse_epoch(timestamp)?,
        });
    }
    Ok(history)
}

const SEARCH_HISTORY_LIMIT: &str = "200";

pub fn search_history(repo_path: &Path, query: &str) -> Result<Vec<CommitInfo>, GitError> {
    let output = match run(
        git_command(repo_path).args([
            "log",
            "--format=%H%x09%an%x09%ct%x09%s",
            "--grep",
            query,
            "-i",
            "-n",
            SEARCH_HISTORY_LIMIT,
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
            timestamp: parse_epoch(timestamp)?,
        });
    }
    Ok(history)
}

/// Full-worktree diff for one file; only the desktop GUI renders diffs,
/// so web-only builds omit this unless compiling tests.
#[cfg(any(test, feature = "desktop"))]
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
    let current = match std::fs::read_to_string(repo_path.join(path)) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(GitError {
                message: format!("failed to read worktree file {path}"),
                stderr: e.to_string(),
                stdout: String::new(),
            })
        }
    };
    Ok(FilePair { original, current })
}

/// Parses a `--shortstat` line like "3 files changed, 10 insertions(+),
/// 2 deletions(-)" into (files_changed, insertions, deletions). Zeros when
/// no stat line is present.
fn parse_shortstat(output: &str) -> (i64, i64, i64) {
    let Some(line) = output.lines().find(|l| l.contains("changed")) else {
        return (0, 0, 0);
    };
    let mut files_changed = 0;
    let mut insertions = 0;
    let mut deletions = 0;
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
    (files_changed, insertions, deletions)
}

/// Human label for a `--name-status` letter code.
fn file_status_label(letter: char) -> &'static str {
    match letter {
        'A' => "Added",
        'D' => "Deleted",
        'M' => "Modified",
        'R' => "Renamed",
        'C' => "Copied",
        'T' => "Type Changed",
        _ => "Changed",
    }
}

/// Pairs `--name-status` output with `--numstat` counts into per-file
/// stats, positionally. Rows without a numstat counterpart report zeros.
fn parse_commit_files(name_status: &str, numstat: &str) -> Vec<FileStat> {
    let name_lines: Vec<&str> = name_status.lines().filter(|l| !l.trim().is_empty()).collect();
    let num_lines: Vec<&str> = numstat.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut files = Vec::new();
    for (i, line) in name_lines.iter().enumerate() {
        let mut fields = line.splitn(3, '\t');
        let letter = fields
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
        let (insertions, deletions) = num_lines
            .get(i)
            .map(|num| {
                let mut nf = num.splitn(3, '\t');
                (
                    nf.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0),
                    nf.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0));
        files.push(FileStat {
            status: file_status_label(letter).to_string(),
            path,
            insertions,
            deletions,
        });
    }
    files
}

pub fn get_commit_summary(repo_path: &Path, hash: &str) -> Result<CommitSummary, GitError> {
    let meta = run(
        git_command(repo_path).args(["show", "-s", "--format=%an%x09%ct%x09%B", hash]),
    )?;
    let mut lines = meta.lines();
    let header = lines.next().unwrap_or_default();
    let mut parts = header.splitn(3, '\t');
    let author = parts.next().unwrap_or_default().to_string();
    let timestamp = parse_epoch(parts.next().unwrap_or_default())?;
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

    let (files_changed, insertions, deletions) =
        run(git_command(repo_path).args(["show", "--format=", "--shortstat", hash]))
            .map(|stat| parse_shortstat(&stat))
            .unwrap_or((0, 0, 0));

    let name_status = run(git_command(repo_path).args(["show", "--format=", "--name-status", hash]))
        .unwrap_or_default();
    let numstat = run(git_command(repo_path).args(["show", "--format=", "--numstat", hash]))
        .unwrap_or_default();
    let files = parse_commit_files(&name_status, &numstat);

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
    if let Some(seqs) = action_argv(&action) {
        for seq in seqs {
            run(git_command(repo_path).args(seq))?;
        }
        return Ok(());
    }
    match action {
        GitAction::DiscardUntracked(p) => {
            let target = repo_path.join(&p);
            std::fs::remove_file(&target).map_err(|e| GitError {
                message: format!("failed to remove {}: {e}", p),
                stderr: String::new(),
                stdout: String::new(),
            })?;
        }
        GitAction::Reclone => reclone_repo(repo_path)?,
        GitAction::RunScript(rel_path) => {
            match crate::actions::launch(repo_path, &rel_path) {
                Ok(()) => {}
                Err(message) => {
                    return Err(GitError {
                        message,
                        stderr: String::new(),
                        stdout: String::new(),
                    });
                }
            }
        }
        GitAction::NewTab(_) | GitAction::CloseTab => {}
        // Unreachable in practice: every other variant is table-backed.
        _ => {}
    }
    Ok(())
}

/// Runs an action while capturing every executed git command and its
/// output. Returns the action result plus the transcript in execution
/// order; entries are produced even when the action fails mid-way.
/// Must be called from a single thread (it uses a thread-local buffer).
///
/// When `progress` is given, every command runs with piped streaming and
/// pushes throttled snapshots of its combined output to the sink while it
/// executes, giving clients live feedback for slow network operations.
pub fn execute_action_logged(
    repo_path: &Path,
    action: GitAction,
    progress: Option<ProgressSink>,
) -> (Result<(), GitError>, Vec<LogEntry>) {
    RECORDING.with(|r| r.set(true));
    PROGRESS.with(|p| *p.borrow_mut() = progress);
    PENDING_LOG.with(|p| p.borrow_mut().clear());
    let result = execute_action(repo_path, action);
    let log = PENDING_LOG.with(|p| std::mem::take(&mut *p.borrow_mut()));
    PROGRESS.with(|p| *p.borrow_mut() = None);
    RECORDING.with(|r| r.set(false));
    (result, log)
}

/// Deletes the repository directory and clones it back from `origin`.
///
/// The heavy-handed escape hatch for upstream-side surgery such as a
/// renamed default branch: a fresh clone adopts the remote's new branch
/// layout and tracking config wholesale. Safety rails:
///
/// * the origin URL is captured *before* anything is deleted, so a
///   repository without an `origin` remote is never touched;
/// * the path must contain `.git`, guarding against stale tab state.
///
/// Note this discards far more than a working-tree reset: local-only
/// branches, stashes, unpushed commits, tags, and `.git/config` edits are
/// all lost. Callers must restart any filesystem watcher afterwards — the
/// delete/re-clone cycle invalidates every registered watch.
fn reclone_repo(repo_path: &Path) -> Result<(), GitError> {
    if !repo_path.join(".git").exists() {
        return Err(GitError {
            message: format!("not a git repository: {}", repo_path.display()),
            stderr: String::new(),
            stdout: String::new(),
        });
    }

    let url = run(git_command(repo_path).args(["remote", "get-url", "origin"]))?
        .trim()
        .to_string();

    std::fs::remove_dir_all(repo_path).map_err(|e| GitError {
        message: format!("failed to delete {}: {e}", repo_path.display()),
        stderr: String::new(),
        stdout: String::new(),
    })?;
    record_synthetic(
        &format!("rm -rf {}", repo_path.display()),
        "repository deleted for fresh clone",
        LogStatus::Success,
    );

    run(Command::new("git").arg("clone").arg(&url).arg(repo_path))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_repo};
    use std::fs;
    use std::process::Command as OsCommand;

    #[test]
    fn malformed_timestamps_are_errors_not_epoch_zero() {
        let err = parse_epoch("not-a-number").unwrap_err();
        assert!(err.message.contains("timestamp"), "got: {}", err.message);
        assert_eq!(parse_epoch(" 1700000000 ").unwrap(), 1_700_000_000);
    }

    #[test]
    fn parse_name_status_maps_rename_delete_and_fallback() {
        let changes = parse_name_status(
            "R100\told.txt\tnew.txt\nD\tgone.txt\nM\ttouched.txt\n",
            true,
            GitStatus::Staged,
        );
        assert_eq!(changes.len(), 3);
        assert_eq!(
            changes[0],
            FileChange {
                path: "new.txt".to_string(),
                status: GitStatus::Renamed,
                is_staged: true
            }
        );
        assert_eq!(
            changes[1],
            FileChange {
                path: "gone.txt".to_string(),
                status: GitStatus::Deleted,
                is_staged: true
            }
        );
        assert_eq!(changes[2].status, GitStatus::Staged);

        let changes = parse_name_status("M\ttouched.txt\n", false, GitStatus::Modified);
        assert_eq!(
            changes[0],
            FileChange {
                path: "touched.txt".to_string(),
                status: GitStatus::Modified,
                is_staged: false
            }
        );

        assert!(parse_name_status("", false, GitStatus::Modified).is_empty());
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
    fn renamed_files_map_to_renamed_status_with_destination_path() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("old-name.txt"), "content\n").unwrap();
        commit_all(dir.path(), "initial");
        OsCommand::new("git")
            .args(["mv", "old-name.txt", "new-name.txt"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert!(
            state.changes.iter().any(|c| c.path == "new-name.txt"
                && c.status == GitStatus::Renamed
                && c.is_staged),
            "expected staged rename to new-name.txt, got: {:?}",
            state.changes
        );
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
    fn discard_untracked_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        commit_all(dir.path(), "initial");

        fs::write(dir.path().join("untracked.txt"), "new\n").unwrap();

        execute_action(
            dir.path(),
            GitAction::DiscardUntracked("untracked.txt".to_string()),
        )
        .unwrap();

        assert!(!dir.path().join("untracked.txt").exists(), "file deleted");
        let state = get_repository_status(dir.path()).unwrap();
        assert!(
            !state
                .changes
                .iter()
                .any(|c| c.path == "untracked.txt"),
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
        assert!(!dir.path().join("b.txt").exists(), "untracked removed");
        assert!(state.changes.is_empty(), "got: {:?}", state.changes);
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

        let (result, log) = execute_action_logged(
            dir.path(),
            GitAction::Stage("file.txt".to_string()),
            None,
        );
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
        let (result, log) = execute_action_logged(
            dir.path(),
            GitAction::CommitAllPush("ship".to_string()),
            None,
        );
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
    fn streaming_progress_receives_live_output() {
        use std::sync::{Arc, Mutex};
        // Bare origin so the push inside CommitAllPush succeeds and emits
        // real stderr output ("Enumerating objects", "main -> main", ...).
        let origin = tempfile::tempdir().unwrap();
        let bare = origin.path().join("origin.git");
        OsCommand::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&bare)
            .output()
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        OsCommand::new("git")
            .args(["clone", "-q", bare.to_str().unwrap(), "."])
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
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();

        let snapshots: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let collector = Arc::clone(&snapshots);
        let sink: ProgressSink = Arc::new(move |snapshot| {
            collector.lock().unwrap().push(snapshot);
        });

        let (result, log) =
            execute_action_logged(dir.path(), GitAction::CommitAllPush("ship".into()), Some(sink));
        assert!(result.is_ok());

        let got = snapshots.lock().unwrap();
        assert!(
            !got.is_empty(),
            "streamed commands must push at least one live snapshot"
        );
        assert!(
            got.iter().any(|s| !s.is_empty()),
            "snapshots must carry command output: {got:?}"
        );

        // The authoritative transcript is unaffected by streaming.
        assert_eq!(log.len(), 3, "add + commit + push: {log:?}");
        assert!(log.iter().all(|e| e.status == LogStatus::Success));
    }

    #[test]
    fn failed_commit_log_carries_git_stderr() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let (_, log) =
            execute_action_logged(dir.path(), GitAction::Commit("empty".to_string()), None);
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
        // Previews render exactly like real transcript lines: raw
        // space-joined argv, no added quoting.
        assert_eq!(
            placeholder_command(&GitAction::Stage("a b.txt".to_string())),
            "git add -- a b.txt"
        );
        assert!(placeholder_command(&GitAction::CloseTab).is_empty());
    }

    #[test]
    fn multi_command_actions_preview_the_full_chain() {
        assert_eq!(
            placeholder_command(&GitAction::CommitAllPush("done".to_string())),
            "git add -A && git commit -m done && git push"
        );
        assert_eq!(
            placeholder_command(&GitAction::CommitAll("wip message".to_string())),
            "git add -A && git commit -m wip message"
        );
    }

    #[test]
    fn table_backed_actions_always_preview_as_git_invocations() {
        use crate::git::types::GitAction::*;
        let actions: Vec<GitAction> = vec![
            Stage("f".into()),
            Unstage("f".into()),
            Discard("f".into()),
            Commit("m".into()),
            CommitAll("m".into()),
            CommitAllPush("m".into()),
            DiscardAll,
            Push,
            Pull,
            Fetch,
            CheckoutBranch("b".into()),
            Revert("abc".into()),
            CreateBranch("n".into(), "main".into()),
            CreateTag("t".into(), "head".into()),
            DeleteTag("t".into()),
            DeleteBranch("b".into()),
        ];
        for action in actions {
            let preview = placeholder_command(&action);
            assert!(
                preview.starts_with("git "),
                "{action:?} must preview as a git invocation, got: {preview}"
            );
        }
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
    fn get_file_pair_errors_when_worktree_read_fails() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::create_dir(dir.path().join("subdir")).unwrap();

        let result = get_file_pair(dir.path(), "subdir");
        assert!(
            result.is_err(),
            "unreadable worktree paths must surface as errors, not empty diffs"
        );
    }

    #[test]
    fn get_file_pair_treats_deleted_worktree_file_as_empty_current() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("gone.txt"), "was here\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::remove_file(dir.path().join("gone.txt")).unwrap();

        let pair = get_file_pair(dir.path(), "gone.txt").unwrap();
        assert_eq!(pair.original, "was here\n");
        assert_eq!(pair.current, "");
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
    fn parse_shortstat_extracts_totals() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)\n"),
            (3, 10, 2)
        );
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

    #[test]
    fn parse_commit_files_pairs_name_status_with_numstat() {
        let name_status = "M\tsrc/a.rs\nA\tnew.txt\n";
        let numstat = "5\t1\tsrc/a.rs\n0\t0\tnew.txt\n";
        let files = parse_commit_files(name_status, numstat);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].status, "Modified");
        assert_eq!(files[0].path, "src/a.rs");
        assert_eq!(files[0].insertions, 5);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[1].status, "Added");

        // Missing numstat row falls back to zeros; unknown letters map to
        // the generic label.
        let orphan = parse_commit_files("D\tgone.txt", "");
        assert_eq!(orphan.len(), 1);
        assert_eq!(orphan[0].status, "Deleted");
        assert_eq!(orphan[0].insertions, 0);
        let unknown = parse_commit_files("X\tweird.bin", "");
        assert_eq!(unknown[0].status, "Changed");
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
    fn reclone_adopts_remote_branch_layout() {
        let origin = tempfile::tempdir().unwrap();
        let bare = origin.path().join("origin.git");
        OsCommand::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&bare)
            .output()
            .unwrap();

        let seed = tempfile::tempdir().unwrap();
        init_repo(seed.path());
        fs::write(seed.path().join("a.txt"), "v1\n").unwrap();
        commit_all(seed.path(), "seed");
        OsCommand::new("git")
            .args(["push", "-q", bare.to_str().unwrap(), "main"])
            .current_dir(seed.path())
            .output()
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        OsCommand::new("git")
            .args(["clone", "-q", bare.to_str().unwrap(), "."])
            .current_dir(dir.path())
            .output()
            .unwrap();

        // Local drift that must vanish: a stray branch, a dirty edit,
        // and an untracked file.
        OsCommand::new("git")
            .args(["checkout", "-q", "-b", "stray-branch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        fs::write(dir.path().join("a.txt"), "local hack\n").unwrap();
        fs::write(dir.path().join("junk.txt"), "junk\n").unwrap();

        execute_action(dir.path(), GitAction::Reclone).unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert!(state.changes.is_empty(), "got changes: {:?}", state.changes);
        assert_eq!(state.current_branch, "main");
        assert_eq!(state.branches, vec!["main".to_string()]);
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "v1\n",
            "working tree should match the fresh clone"
        );
        assert!(!dir.path().join("junk.txt").exists());
    }

    #[test]
    fn reclone_refuses_repo_without_remote() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "precious\n").unwrap();
        commit_all(dir.path(), "initial");

        assert!(execute_action(dir.path(), GitAction::Reclone).is_err());

        // Nothing may be deleted when no origin URL could be captured.
        assert!(dir.path().join(".git").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("file.txt")).unwrap(),
            "precious\n"
        );
    }

    #[test]
    fn search_history_finds_commits_beyond_recent_window() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        // Seed more than HISTORY_LIMIT commits so the offending one falls
        // outside the default 50-commit history window.
        for i in 0..60 {
            fs::write(dir.path().join("f.txt"), format!("v{i}\n")).unwrap();
            commit_all(dir.path(), &format!("commit number {i}"));
        }

        // The default window is capped at 50; confirm the oldest commits
        // are simply absent from get_history().
        let recent = get_history(dir.path()).unwrap();
        assert_eq!(recent.len(), 50);
        assert!(
            recent.iter().all(|c| !c.message.contains("commit number 0")),
            "oldest commit must fall outside the default window"
        );

        let matches = search_history(dir.path(), "embed").unwrap();
        assert!(
            matches.is_empty(),
            "no commit mentions 'embed', got: {:?}",
            matches
        );

        let found = search_history(dir.path(), "commit number 0").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "commit number 0");
    }

    #[test]
    fn search_history_handles_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        assert!(search_history(dir.path(), "anything").unwrap().is_empty());
    }
}