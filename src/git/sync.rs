// Remote sync probe: is this repo out of sync with its upstream?
//
// Deliberately separate from the `run()`/`execute_action_logged` machinery in
// `mod.rs`: this check is not user-initiated, must never appear in a tab's
// transcript, and needs a watchdog because `ls-remote` can block on a hung
// SSH connect. See NOTES.md ("Known Gaps").

use super::*;
use std::fmt;
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Deadline for a single probe. Generous next to the ~3.4s an SSH
/// `ls-remote` to github.com costs on this machine, tight enough that a wedged
/// transport cannot hold a slot for the whole 30s tick.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// How often the watchdog reaps the child while waiting.
const PROBE_POLL: Duration = Duration::from_millis(25);

/// How long the watchdog waits for the pipe readers to finish after the child
/// is gone. EOF is immediate once the process group is dead; this only bounds
/// the pathological case.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Conclusive or not. Only [`SyncOutcome::Unreachable`] is inconclusive and
/// therefore retryable — every other variant settles the tab, so a local-only
/// repo never burns its retry budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    InSync,
    OutOfDate,
    NoRemote,
    Unreachable,
}

impl SyncOutcome {
    /// True when this outcome is final and the tab needs no further probing.
    pub fn is_settled(self) -> bool {
        !matches!(self, SyncOutcome::Unreachable)
    }

    /// The badge value. Failures are never alarming: `Unreachable` is not
    /// evidence of being out of date, so it renders as `false`.
    pub fn out_of_date(self) -> bool {
        matches!(self, SyncOutcome::OutOfDate)
    }
}

/// Failure from a sync probe. Not [`GitError`] on purpose: the probe
/// bypasses `execute_action_logged`, runs under its own watchdog, and must not
/// be able to reach a tab's transcript. [`GitError`] deliberately carries no
/// exit code, so the kind lives here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncError {
    pub message: String,
    pub stderr: String,
    pub kind: SyncErrorKind,
}

impl SyncError {
    pub fn new(message: impl Into<String>, stderr: impl Into<String>) -> Self {
        SyncError {
            message: message.into(),
            stderr: stderr.into(),
            kind: SyncErrorKind::Local,
        }
    }

    pub fn with_kind(
        message: impl Into<String>,
        stderr: impl Into<String>,
        kind: SyncErrorKind,
    ) -> Self {
        SyncError {
            message: message.into(),
            stderr: stderr.into(),
            kind,
        }
    }

    /// The watchdog fired on `argv[0]`.
    pub fn timed_out(program: &str, arg0: &str) -> Self {
        SyncError::with_kind(
            format!("{program} {arg0} timed out"),
            format!("killed after {}s", PROBE_TIMEOUT.as_secs()),
            SyncErrorKind::TimedOut,
        )
    }

    /// True for the two failures that mean "the network or the remote did not
    /// answer", which are the only ones worth retrying.
    pub fn is_retryable_transport_failure(&self) -> bool {
        matches!(
            self.kind,
            SyncErrorKind::Unreachable | SyncErrorKind::TimedOut
        )
    }
}

/// Why a probe failed. Only [`SyncErrorKind::Unreachable`] and
/// [`SyncErrorKind::TimedOut`] justify another attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncErrorKind {
    /// git exited 128: DNS, refused connection, auth failure, no route.
    Unreachable,
    /// The watchdog killed a hung process.
    TimedOut,
    /// Anything local — no upstream configured, bad ref, spawn failure.
    Local,
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = self.stderr.trim();
        if detail.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.message, detail)
        }
    }
}

impl std::error::Error for SyncError {}

impl From<&SyncError> for SyncOutcome {
    /// Folds a probe failure back into an outcome. A transport failure is the
    /// only inconclusive case; every local failure settles as
    /// [`SyncOutcome::NoRemote`] — nothing to compare, nothing to alarm about.
    fn from(err: &SyncError) -> Self {
        if err.is_retryable_transport_failure() {
            SyncOutcome::Unreachable
        } else {
            SyncOutcome::NoRemote
        }
    }
}

impl From<GitError> for SyncError {
    fn from(e: GitError) -> Self {
        let stderr = if e.stderr.trim().is_empty() {
            e.stdout
        } else {
            e.stderr
        };
        SyncError::new(e.message, stderr)
    }
}

/// git exits 128 for "the remote could not be contacted" (DNS failure,
/// refused connection, auth failure, no route to host) and 1-127 for local
/// usage errors, so the code is what separates "unreachable" from "our fault".
fn is_unreachable(status: &ExitStatus) -> bool {
    status.code() == Some(128)
}

/// Never runs `git` in the daemon's own process group: a transport child
/// (`ssh`, a credential helper) inherits it, and killing only git would leave
/// that child holding the pipes open. With its own group, one `kill` takes the
/// whole tree down and the reader threads see EOF.
#[cfg(unix)]
fn isolate_process_group(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn isolate_process_group(_cmd: &mut std::process::Command) {}

/// SIGKILLs the child's entire process group, then reaps it. Uses the POSIX
/// shell builtin rather than a crate so the watchdog needs no new dependency;
/// the child has no TTY and holds no state, so there is nothing to clean up
/// gracefully.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        let _ = std::process::Command::new("sh")
            .args(["-c", &format!("kill -9 -{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A draining reader thread, handed back as a receiver so the caller can wait
/// with a deadline. A plain `JoinHandle` would block forever on a pipe whose
/// writer is an orphaned grandchild.
fn spawn_pipe_reader<R: std::io::Read + Send + 'static>(pipe: R) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_pipe(pipe));
    });
    rx
}

/// Collects both streams, giving up after `grace` so a wedged writer cannot
/// hold the caller past the watchdog. Missing output is empty, not an error:
/// this only runs on the timeout path, where the error already says so.
fn collect_streams(
    out: std::sync::mpsc::Receiver<String>,
    err: std::sync::mpsc::Receiver<String>,
    grace: Duration,
) -> (String, String) {
    let drained = |rx: std::sync::mpsc::Receiver<String>| {
        rx.recv_timeout(grace).unwrap_or_default()
    };
    (drained(out), drained(err))
}

/// Runs `git <argv>` in `repo` under a hard deadline and returns its stdout.
///
/// A watchdog rather than `tokio::time::timeout` around the call: cancelling a
/// join stops *waiting* but leaves the git child alive, and a stuck SSH connect
/// would then leak a process per retry for the life of the daemon. The kill has
/// to come from [`std::process::Child`], so this blocks and must be called from
/// a `spawn_blocking` context.
///
/// Never records to the transcript — the whole point of bypassing `run()`.
fn run_with_timeout(repo: &Path, argv: &[&str], timeout: Duration) -> Result<String, SyncError> {
    let arg0 = argv.first().copied().unwrap_or("");
    let mut cmd = git_command(repo);
    cmd.args(argv);
    // The daemon has no TTY, so a prompt is not a question anyone can answer —
    // it is a hang. `BatchMode` likewise turns an interactive host-key prompt
    // into an immediate failure instead of a wait.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate_process_group(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| SyncError::new(format!("failed to execute git: {e}"), ""))?;
    let out_pipe = child.stdout.take().expect("child stdout was piped");
    let err_pipe = child.stderr.take().expect("child stderr was piped");
    // Both pipes must be drained while the child runs: a full pipe buffer would
    // block git from ever exiting.
    let stdout_rx = spawn_pipe_reader(out_pipe);
    let stderr_rx = spawn_pipe_reader(err_pipe);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                kill_tree(&mut child);
                return Err(SyncError::new(format!("failed to wait for git: {e}"), ""));
            }
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(PROBE_POLL);
    };

    let Some(status) = status else {
        kill_tree(&mut child);
        let _ = collect_streams(stdout_rx, stderr_rx, DRAIN_GRACE);
        return Err(SyncError::timed_out("git", arg0));
    };

    let (stdout, stderr) = collect_streams(stdout_rx, stderr_rx, DRAIN_GRACE);
    if status.success() {
        Ok(stdout)
    } else {
        let kind = if is_unreachable(&status) {
            SyncErrorKind::Unreachable
        } else {
            SyncErrorKind::Local
        };
        Err(SyncError::with_kind(
            format!("git {arg0} failed"),
            stderr,
            kind,
        ))
    }
}

/// Drains a piped stream to a string; unreadable bytes are lossily decoded
/// because stderr here is diagnostic text, never data.
fn read_pipe<R: std::io::Read>(mut pipe: R) -> String {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Extracts the SHA from `git ls-remote <remote> <ref>` output, whose lines are
/// `<sha>\t<ref>`. Returns `None` when the ref is absent (git exits 0 with
/// empty output) or a line is malformed — either way there is nothing to
/// compare against.
fn parse_ls_remote_sha(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let sha = line.split('\t').next()?.trim();
        let is_hex = !sha.is_empty()
            && sha.len() >= 40
            && sha.chars().all(|c| c.is_ascii_hexdigit());
        is_hex.then(|| sha.to_string())
    })
}

/// A purely local git query. Failures are always [`SyncErrorKind::Local`]
/// whatever the exit code: `rev-parse @{upstream}` on a branch with no
/// upstream exits 128, which would otherwise masquerade as an unreachable
/// remote and send the ticker round the retry loop for nothing.
fn local_query(repo: &Path, argv: &[&str]) -> Result<String, SyncError> {
    run_with_timeout(repo, argv, PROBE_TIMEOUT).map_err(|e| {
        SyncError::with_kind(e.message, e.stderr, SyncErrorKind::Local)
    })
}

/// Full ref name of the branch's upstream, e.g. `origin/main`. A failure
/// means the branch has no upstream — a settled [`SyncOutcome::NoRemote`], not
/// an error worth retrying.
pub fn upstream_ref(repo: &Path) -> Result<String, SyncError> {
    let out = local_query(
        repo,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )?;
    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        return Err(SyncError::new(
            "upstream resolved to an empty ref",
            "",
        ));
    }
    Ok(trimmed)
}

/// SHA we last saw for `upstream`, from `refs/remotes/*` as written by the
/// last fetch. Comparing it against the live `ls-remote` SHA is the whole
/// check.
pub fn cached_sha(repo: &Path, upstream: &str) -> Result<String, SyncError> {
    let out = local_query(repo, &["rev-parse", upstream])?;
    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        return Err(SyncError::new(
            format!("{upstream} resolved to an empty sha"),
            "",
        ));
    }
    Ok(trimmed)
}

/// Is this repo out of sync with its upstream?
///
/// `Ok(true)`/`Ok(false)` is conclusive: the caller should settle and, if it
/// keeps a [`SyncOutcome`], `Ok(false)` is never evidence of being out of date.
/// `Err` means "no answer this time" and carries the reason in
/// [`SyncError::kind`] — retry on a transport failure, give up on
/// [`SyncErrorKind::Local`].
///
/// **Deviation from the plan, deliberate:** the plan called for `Unreachable` to
/// come back as `Ok(false)`. That was rejected because it destroys the only
/// signal the retry loop has: `Ok` means settled, so an offline-at-startup
/// machine would settle on the first attempt and never badge at all. `Err` is
/// strictly safer for that goal — a caller cannot read it as `true` — and
/// `SyncOutcome::from(&err)` renders it as `Unreachable`, which is exactly the
/// `false` the badge wants.
pub fn check_remote_sync(repo: &Path) -> Result<bool, SyncError> {
    let upstream = upstream_ref(repo)
        .map_err(|_| SyncError::new("branch has no upstream branch", ""))?;
    let Some((remote, branch)) = upstream.rsplit_once('/') else {
        // Tracks a local branch, so there is no remote to ask.
        return Err(SyncError::new(
            format!("{upstream} is not a remote-tracking branch"),
            "",
        ));
    };

    let refspec = format!("refs/heads/{branch}");
    let live = run_with_timeout(repo, &["ls-remote", remote, &refspec], PROBE_TIMEOUT)?;
    let Some(live_sha) = parse_ls_remote_sha(&live) else {
        // The branch is gone upstream, or came back in a shape we do not
        // understand. Nothing to compare against, so no answer — but no
        // network fault either, so do not retry.
        return Err(SyncError::new(
            format!("{remote} has no live sha for {refspec}"),
            live,
        ));
    };

    let cached = cached_sha(repo, &upstream)?;
    Ok(cached != live_sha)
}

#[cfg(test)]
mod tests {
use super::*;
    use crate::test_support::{commit_all, init_repo};
    use std::path::PathBuf;

    const SHA: &str = "9f2c1a4b8e7d6c5b4a39281706f5e4d3c2b1a09f8";

    #[test]
    fn parses_sha_from_ls_remote_output() {
        let out = format!("{SHA}\trefs/heads/main\n");
        assert_eq!(parse_ls_remote_sha(&out).as_deref(), Some(SHA));
    }

    #[test]
    fn parses_sha_from_ls_remote_output_without_trailing_newline() {
        let out = format!("{SHA}\trefs/heads/main");
        assert_eq!(parse_ls_remote_sha(&out).as_deref(), Some(SHA));
    }

    #[test]
    fn ls_remote_sha_is_none_when_the_ref_is_absent() {
        // `ls-remote` exits 0 with empty output for an unknown ref.
        assert_eq!(parse_ls_remote_sha(""), None);
        assert_eq!(parse_ls_remote_sha("\n"), None);
    }

    #[test]
    fn ls_remote_sha_ignores_leading_noise_and_extra_refs() {
        let out = format!("warning: noise\n{SHA}\trefs/heads/main\n{SHA}\trefs/tags/v1\n");
        assert_eq!(parse_ls_remote_sha(&out).as_deref(), Some(SHA));
    }

    #[test]
    fn ls_remote_sha_is_none_for_malformed_lines() {
        assert_eq!(parse_ls_remote_sha("not-a-sha\trefs/heads/main\n"), None);
        assert_eq!(parse_ls_remote_sha("refs/heads/main\n"), None);
    }

    #[test]
    fn only_unreachable_leaves_a_tab_unsettled() {
        assert!(SyncOutcome::InSync.is_settled());
        assert!(SyncOutcome::OutOfDate.is_settled());
        assert!(SyncOutcome::NoRemote.is_settled());
        assert!(!SyncOutcome::Unreachable.is_settled());
    }

    #[test]
    fn outcomes_render_as_false_except_out_of_date() {
        assert!(!SyncOutcome::InSync.out_of_date());
        assert!(SyncOutcome::OutOfDate.out_of_date());
        assert!(!SyncOutcome::NoRemote.out_of_date());
        assert!(!SyncOutcome::Unreachable.out_of_date());
    }

    /// Runs `git` in `dir`, panicking on failure so fixture setup bugs are
    /// never mistaken for the behaviour under test.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A work repo with one commit and an `origin/main` upstream, built from a
    /// bare local-path remote seeded with a single local `fetch` — no `push`
    /// and no commit against the remote. Returns the work repo tempdir (the
    /// bare remote lives inside it) so tests can move the remote's ref to go
    /// out of date.
    fn repo_with_upstream() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");

        let remote = dir.path().join("remote.git");
        let work = dir.path();
        git(work, &["init", "-q", "--bare", "remote.git"]);
        git(
            &remote,
            &[
                "fetch",
                "-q",
                work.to_str().unwrap(),
                "refs/heads/main:refs/heads/main",
            ],
        );
        git(
            dir.path(),
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(dir.path(), &["fetch", "-q", "origin"]);
        git(dir.path(), &["config", "branch.main.remote", "origin"]);
        git(dir.path(), &["config", "branch.main.merge", "refs/heads/main"]);

        (dir, remote)
    }

    #[test]
    fn upstream_ref_reads_the_configured_upstream() {
        let (dir, _remote) = repo_with_upstream();

        assert_eq!(upstream_ref(dir.path()).unwrap(), "origin/main");
        assert_eq!(cached_sha(dir.path(), "origin/main").unwrap().len(), 40);
    }

    #[test]
    fn upstream_ref_fails_when_the_branch_has_no_upstream() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");

        assert!(upstream_ref(dir.path()).is_err());
    }

    #[test]
    fn cached_sha_errors_on_an_unknown_ref() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        assert!(cached_sha(dir.path(), "origin/nope").is_err());
    }

    // --- watchdog helper ---

    /// A scratch repo, enough for `run_with_timeout`, which only needs a cwd.
    fn scratch_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        dir
    }

    /// Exit status from `sh -c <script>`, for exercising the classifier with
    /// real `ExitStatus` values instead of hand-rolled ones.
    fn shell_status(script: &str) -> std::process::ExitStatus {
        std::process::Command::new("sh")
            .args(["-c", script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
    }

    #[test]
    fn exit_128_means_the_remote_could_not_be_reached() {
        assert!(is_unreachable(&shell_status("exit 128")));
        assert!(!is_unreachable(&shell_status("exit 1")));
        assert!(!is_unreachable(&shell_status("exit 0")));
        assert!(!is_unreachable(&shell_status("exit 127")));
    }

    #[test]
    fn run_with_timeout_returns_stdout() {
        let dir = scratch_repo();

        let out = run_with_timeout(
            dir.path(),
            &["-c", "alias.shout=!echo hello", "shout"],
            Duration::from_secs(10),
        )
        .unwrap();

        assert_eq!(out.trim(), "hello");
    }

    #[test]
    fn run_with_timeout_disables_credential_prompts_in_the_child_env() {
        let dir = scratch_repo();

        let out = run_with_timeout(
            dir.path(),
            &[
                "-c",
                "alias.envcheck=!echo $GIT_TERMINAL_PROMPT/$GIT_SSH_COMMAND",
                "envcheck",
            ],
            Duration::from_secs(10),
        )
        .unwrap();

        assert_eq!(out.trim(), "0/ssh -oBatchMode=yes");
    }

    #[test]
    fn run_with_timeout_kills_a_hung_command_instead_of_hanging() {
        let dir = scratch_repo();
        let started = Instant::now();

        let err = run_with_timeout(
            dir.path(),
            &["-c", "alias.hang=!sleep 43.21 & wait", "hang"],
            Duration::from_millis(300),
        )
        .unwrap_err();

        assert_eq!(err.kind, SyncErrorKind::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "watchdog must fire, not wait out the sleep: {:?}",
            started.elapsed()
        );

        // The sleep is a grandchild (git runs the alias through a shell), so
        // killing only the direct child would leave it holding the pipes open
        // and the drain would hang. Nothing may survive the watchdog.
        #[cfg(target_os = "linux")]
        {
            let survivors: Vec<String> = std::fs::read_dir("/proc")
                .unwrap()
                .filter_map(|entry| {
                    let pid = entry.ok()?.file_name().to_str()?.to_string();
                    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
                    // Exact argv match on the marker duration above: a general
                    // "is there a sleep?" sweep would trip over unrelated ones.
                    let argv: Vec<&[u8]> = cmdline
                        .split(|b| *b == 0)
                        .filter(|arg| !arg.is_empty())
                        .collect();
                    (argv.as_slice() == [b"sleep".as_slice(), b"43.21".as_slice()]).then_some(pid)
                })
                .collect();
            assert!(
                survivors.is_empty(),
                "the watchdog leaked processes: {survivors:?}"
            );
        }
    }

    #[test]
    fn run_with_timeout_maps_a_transport_failure_to_unreachable() {
        let dir = scratch_repo();

        let err = run_with_timeout(
            dir.path(),
            &["-c", "alias.boom=!echo 'ssh: connect to host: No route' >&2; exit 128", "boom"],
            Duration::from_secs(10),
        )
        .unwrap_err();

        assert_eq!(err.kind, SyncErrorKind::Unreachable);
        assert!(err.stderr.contains("No route"), "got: {}", err.stderr);
    }

    #[test]
    fn run_with_timeout_keeps_local_failures_distinguishable_from_unreachable() {
        let dir = scratch_repo();

        let err = run_with_timeout(
            dir.path(),
            &["-c", "alias.nope=!echo 'usage: nonsense' >&2; exit 129", "nope"],
            Duration::from_secs(10),
        )
        .unwrap_err();

        assert_eq!(err.kind, SyncErrorKind::Local);
    }

    #[test]
    fn sync_errors_carry_their_kind_into_the_display_form() {
        let err = SyncError {
            message: "git ls-remote failed".to_string(),
            stderr: "ssh: connect to host git: Connection refused".to_string(),
            kind: SyncErrorKind::Unreachable,
        };

        assert_eq!(
            err.to_string(),
            "git ls-remote failed: ssh: connect to host git: Connection refused"
        );
        assert!(SyncError::timed_out("git ls-remote", "ls-remote")
            .is_retryable_transport_failure());
        assert!(SyncError::new("git ls-remote failed", "").kind == SyncErrorKind::Local);
        assert!(!SyncError::new("git ls-remote failed", "").is_retryable_transport_failure());
    }

    #[test]
    fn git_errors_convert_to_local_sync_errors() {
        let converted: SyncError = GitError {
            message: "git command failed".to_string(),
            stderr: "fatal: no upstream".to_string(),
            stdout: String::new(),
        }
        .into();

        assert_eq!(converted.kind, SyncErrorKind::Local);
        assert_eq!(converted.stderr, "fatal: no upstream");
    }

    // --- the actual check ---

    /// Advances the bare remote's `main` by one commit *without* the work repo
    /// fetching, which is exactly the state a colleague's push leaves behind:
    /// `refs/remotes/origin/main` is stale, the live ref has moved on.
    fn advance_remote(work: &Path, remote: &Path) {
        std::fs::write(work.join("file.txt"), "second\n").unwrap();
        commit_all(work, "second");
        git(
            remote,
            &[
                "fetch",
                "-q",
                work.to_str().unwrap(),
                "refs/heads/main:refs/heads/main",
            ],
        );
    }

    #[test]
    fn check_remote_sync_reports_in_sync_when_the_remote_has_not_moved() {
        let (dir, _remote) = repo_with_upstream();

        assert!(!check_remote_sync(dir.path()).unwrap());
    }

    #[test]
    fn check_remote_sync_reports_out_of_date_when_the_remote_moved_ahead() {
        let (dir, remote) = repo_with_upstream();
        advance_remote(dir.path(), &remote);

        assert!(check_remote_sync(dir.path()).unwrap());
    }

    #[test]
    fn check_remote_sync_follows_a_fetch_back_to_in_sync() {
        let (dir, remote) = repo_with_upstream();
        advance_remote(dir.path(), &remote);
        assert!(check_remote_sync(dir.path()).unwrap());

        git(dir.path(), &["fetch", "-q", "origin"]);

        assert!(!check_remote_sync(dir.path()).unwrap());
    }

    #[test]
    fn check_remote_sync_settles_a_local_only_repo_without_probing() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("file.txt"), "hello\n").unwrap();
        commit_all(dir.path(), "initial");

        // Settles immediately: `Err`, not a retryable transport failure.
        let err = check_remote_sync(dir.path()).unwrap_err();
        assert!(!err.is_retryable_transport_failure(), "got: {err}");
        assert!(!SyncOutcome::from(&err).out_of_date());
    }

    #[test]
    fn check_remote_sync_never_alarms_when_the_remote_is_unreachable() {
        let (dir, _remote) = repo_with_upstream();
        git(
            dir.path(),
            &["remote", "set-url", "origin", "/nonexistent/grit-test-remote"],
        );

        let err = check_remote_sync(dir.path()).unwrap_err();

        assert!(
            err.is_retryable_transport_failure(),
            "an unreachable remote must ask for another attempt, got: {err}"
        );
        assert!(
            !SyncOutcome::from(&err).out_of_date(),
            "failures never set the badge"
        );
    }

    #[test]
    fn check_remote_sync_settles_when_the_upstream_branch_is_gone_from_the_remote() {
        let (dir, remote) = repo_with_upstream();
        git(&remote, &["update-ref", "-d", "refs/heads/main"]);

        let err = check_remote_sync(dir.path()).unwrap_err();

        // Nothing to compare against, and no network problem either.
        assert!(!err.is_retryable_transport_failure(), "got: {err}");
        assert!(!SyncOutcome::from(&err).out_of_date());
    }

    #[test]
    fn check_remote_sync_leaves_no_trace_in_the_action_transcript() {
        let (dir, remote) = repo_with_upstream();
        advance_remote(dir.path(), &remote);

        // Recording active is the worst case: if the probe went through `run()`
        // its rev-parse and ls-remote calls would land in the tab's log.
        RECORDING.with(|r| r.set(true));
        PENDING_LOG.with(|p| p.borrow_mut().clear());
        let _ = check_remote_sync(dir.path());
        let log: Vec<LogEntry> = PENDING_LOG.with(|p| std::mem::take(&mut *p.borrow_mut()));
        RECORDING.with(|r| r.set(false));

        assert!(
            log.is_empty(),
            "the probe must never reach the transcript, got: {:?}",
            log.iter().map(|e| &e.command).collect::<Vec<_>>()
        );
    }
}