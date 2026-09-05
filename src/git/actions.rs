// Action dispatch, argv tables, previews, and repository reclone.

use super::*;

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
        GitAction::CommitPush(m) => vec![seq(&["commit", "-m", m]), seq(&["push"])],
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
        GitAction::StashPush(m) => vec![seq(&["stash", "push", "-m", m])],
        GitAction::StashApply(id) => vec![seq(&["stash", "apply", id])],
        GitAction::StashPop(id) => vec![seq(&["stash", "pop", id])],
        GitAction::StashDrop(id) => vec![seq(&["stash", "drop", id])],
        GitAction::Reclone
            | GitAction::RunScript(_)
            | GitAction::NewTab(_)
            | GitAction::CloseTab
            | GitAction::DiscardUntracked(_)
            | GitAction::SearchHistory(_)
            | GitAction::OpenExternal(_)
            | GitAction::OpenWith(_, _)
            | GitAction::DeleteFile(_)
            | GitAction::RenameFile(_, _) => {
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
    fn every_action_variant_is_previewable() {
        use crate::git::types::GitAction::*;
        // Exhaustive, no wildcard arm: introducing a new GitAction variant is a
        // compile error here until its preview behavior is decided. Mark a
        // variant `false` only when it intentionally has no server-side effect
        // to preview (NewTab/CloseTab are handled locally by the clients).
        let cases: Vec<(GitAction, bool)> = vec![
            (Stage("f".into()), true),
            (Unstage("f".into()), true),
            (Discard("f".into()), true),
            (DiscardUntracked("f".into()), true),
            (Commit("m".into()), true),
            (CommitAll("m".into()), true),
            (CommitAllPush("m".into()), true),
            (CommitPush("m".into()), true),
            (DiscardAll, true),
            (Push, true),
            (Pull, true),
            (Fetch, true),
            (CheckoutBranch("b".into()), true),
            (Revert("h".into()), true),
            (CreateBranch("n".into(), "f".into()), true),
            (CreateTag("n".into(), "t".into()), true),
            (DeleteTag("n".into()), true),
            (DeleteBranch("n".into()), true),
            (StashPush("m".into()), true),
            (StashApply("stash@{0}".into()), true),
            (StashPop("stash@{0}".into()), true),
            (StashDrop("stash@{0}".into()), true),
            (Reclone, true),
            (NewTab("x".into()), false),
            (CloseTab, false),
            (RunScript("tool.sh".into()), true),
            // File ops + history search are handled bespoke in the WebSocket
            // layer with no placeholder transcript entry today.
            (SearchHistory("q".into()), false),
            (OpenExternal("f".into()), false),
            (OpenWith("code".into(), "%f".into()), false),
            (DeleteFile("f".into()), false),
            (RenameFile("a".into(), "b".into()), false),
        ];
        assert_eq!(
            cases.len(),
            31,
            "GitAction gained a variant; update this list and types.rs round-trip list"
        );
        for (action, previewable) in cases {
            let preview = placeholder_command(&action);
            if previewable {
                assert!(
                    !preview.is_empty(),
                    "previewable variant {action:?} rendered an empty placeholder"
                );
            } else {
                assert!(
                    preview.is_empty(),
                    "no-preview variant {action:?} unexpectedly rendered {preview}"
                );
            }
        }
        assert_eq!(
            placeholder_command(&Reclone),
            "git remote get-url origin && rm -rf <repo> && git clone <origin-url> <repo>"
        );
        assert_eq!(placeholder_command(&RunScript("tool.sh".into())), "./tool.sh");
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
        assert_eq!(
            placeholder_command(&GitAction::CommitPush("staged".to_string())),
            "git commit -m staged && git push"
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
            CommitPush("m".into()),
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

}
