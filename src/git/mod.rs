pub mod types;
pub mod watcher;

mod status;
mod history;
mod files;
mod actions;
pub(crate) use status::*;
pub(crate) use history::*;
pub(crate) use files::*;
pub(crate) use actions::*;

pub use types::{
    CommitInfo, CommitSummary, FileChange, FileContent, FilePair, FileStat, FileTreeEntry,
    GitAction, GitStatus, LogEntry, LogStatus, RepoState, StashEntry,
};

use std::cell::{Cell, RefCell};
use std::fmt;
use std::path::{Path, PathBuf};
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
