// Status, branch, stash, and working-tree change collection.

use super::*;

pub fn get_repository_status(repo_path: &Path) -> Result<RepoState, GitError> {
    let current_branch = get_current_branch(repo_path)?;
    let branches = list_branches(repo_path)?;
    let remote_branches = list_remote_branches(repo_path)?;
    let stashes = list_stashes(repo_path)?;
    let changes = list_changes(repo_path)?;
    let history = get_history(repo_path)?;

    Ok(RepoState {
        current_branch,
        branches,
        remote_branches,
        stashes,
        changes,
        history,
        scripts: crate::actions::discover(repo_path),
    })
}

pub(crate) fn get_current_branch(repo_path: &Path) -> Result<String, GitError> {
    match run(git_command(repo_path).args(["symbolic-ref", "--short", "HEAD"])) {
        Ok(branch) => Ok(branch.trim().to_string()),
        Err(_) => {
            // Detached HEAD — fall back to short commit hash.
            let hash = run(git_command(repo_path).args(["rev-parse", "--short", "HEAD"]))?;
            Ok(format!("detached@{}", hash.trim()))
        }
    }
}

pub(crate) fn list_branches(repo_path: &Path) -> Result<Vec<String>, GitError> {
    let output = run(git_command(repo_path).args(["branch", "--format=%(refname:short)"]))?;
    Ok(output
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

pub(crate) fn list_remote_branches(repo_path: &Path) -> Result<Vec<String>, GitError> {
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

pub(crate) fn list_stashes(repo_path: &Path) -> Result<Vec<StashEntry>, GitError> {
    let output = match run(
        git_command(repo_path).args(["stash", "list", "--format=%gd%x09%gs%x09%ct"]),
    ) {
        Ok(output) => output,
        Err(e) if e.stderr.contains("ref 'refs/stash' does not exist")
            || e.stderr.contains("No stash entries found") =>
        {
            return Ok(Vec::new());
        }
        Err(e) => return Err(e),
    };

    let mut stashes = Vec::new();
    for line in output.lines() {
        let mut parts = line.splitn(3, '\t');
        let id = parts.next().unwrap_or_default().trim().to_string();
        let msg_with_branch = parts.next().unwrap_or_default().trim().to_string();
        let timestamp = parts.next().unwrap_or_default().trim();

        if id.is_empty() {
            continue;
        }

        let (branch, message) = split_stash_subject(&msg_with_branch);
        let files = stash_files(repo_path, &id);

        stashes.push(StashEntry {
            id,
            branch,
            message,
            timestamp: parse_epoch(timestamp).unwrap_or(0),
            files,
        });
    }
    Ok(stashes)
}

/// `git stash list` subject is `WIP on <branch>: <short message>`. Split the
/// branch and message apart; both fall back to empty strings when absent.
pub(crate) fn split_stash_subject(subject: &str) -> (String, String) {
    if let Some(rest) = subject.strip_prefix("WIP on ") {
        if let Some((b, m)) = rest.split_once(": ") {
            return (b.to_string(), m.to_string());
        }
    }
    // Non-WIP form (e.g. `On <branch>: <message>` for stash with message).
    if let Some(rest) = subject.strip_prefix("On ") {
        if let Some((b, m)) = rest.split_once(": ") {
            return (b.to_string(), m.to_string());
        }
    }
    (subject.to_string(), String::new())
}

/// Best-effort file list for a stash: `git stash show` with name+num stats.
/// Falls back to an empty list on any git error so a single bad stash never
/// fails an entire status refresh.
pub(crate) fn stash_files(repo_path: &Path, id: &str) -> Vec<FileStat> {
    let name_status =
        run(git_command(repo_path).args(["stash", "show", "--name-status", id])).unwrap_or_default();
    let numstat = run(git_command(repo_path).args(["stash", "show", "--numstat", id]))
        .unwrap_or_default();
    parse_commit_files(&name_status, &numstat)
}

pub(crate) fn list_changes(repo_path: &Path) -> Result<Vec<FileChange>, GitError> {
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
pub(crate) fn parse_name_status(output: &str, is_staged: bool, fallback: GitStatus) -> Vec<FileChange> {
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

pub(crate) fn parse_epoch(field: &str) -> Result<i64, GitError> {
    field.trim().parse::<i64>().map_err(|_| GitError {
        message: format!("malformed commit timestamp {field:?}"),
        stderr: String::new(),
        stdout: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_repo};
    use std::fs;
    use std::process::Command as OsCommand;

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
    fn stash_push_populate_status_and_drop() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("a.txt"), "v2\n").unwrap();

        assert!(get_repository_status(dir.path()).unwrap().stashes.is_empty());

        execute_action(dir.path(), GitAction::StashPush("wip".to_string())).unwrap();

        let state = get_repository_status(dir.path()).unwrap();
        assert_eq!(state.stashes.len(), 1);
        assert_eq!(state.stashes[0].id, "stash@{0}");
        assert_eq!(state.stashes[0].message, "wip");
        assert_eq!(state.stashes[0].branch, "main");
        assert!(
            state.stashes[0].files.iter().any(|f| f.path == "a.txt"),
            "stash should capture a.txt, got: {:?}",
            state.stashes[0].files
        );

        execute_action(dir.path(), GitAction::StashDrop("stash@{0}".to_string())).unwrap();
        assert!(get_repository_status(dir.path()).unwrap().stashes.is_empty());
    }

    #[test]
    fn stash_apply_restores_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("a.txt"), "v1\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("a.txt"), "v2\n").unwrap();

        execute_action(dir.path(), GitAction::StashPush("keep".to_string())).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "v1\n"
        );

        execute_action(dir.path(), GitAction::StashApply("stash@{0}".to_string())).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "v2\n"
        );
        assert_eq!(get_repository_status(dir.path()).unwrap().stashes.len(), 1);
    }

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
}
