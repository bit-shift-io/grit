pub mod types;
pub mod watcher;

pub use types::{CommitInfo, FileChange, GitAction, GitStatus, RepoState};

use std::fmt;
use std::path::Path;
use std::process::Command;

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

fn git_command(repo_path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    cmd.env("LC_ALL", "C");
    cmd
}

fn run(cmd: &mut Command) -> Result<String, GitError> {
    let output = cmd
        .output()
        .map_err(|e| GitError {
            message: format!("failed to execute git: {e}"),
            stderr: String::new(),
            stdout: String::new(),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

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

pub fn execute_action(repo_path: &Path, action: GitAction) -> Result<(), GitError> {
    match action {
        GitAction::Stage(path) => {
            run(git_command(repo_path).args(["add", "--", path.as_str()]))?;
        }
        GitAction::Unstage(path) => {
            run(git_command(repo_path).args(["reset", "HEAD", "--", path.as_str()]))?;
        }
        GitAction::Commit(message) => {
            run(git_command(repo_path).args(["commit", "-m", message.as_str()]))?;
        }
        GitAction::Push => {
            run(git_command(repo_path).args(["push"]))?;
        }
        GitAction::Pull => {
            run(git_command(repo_path).args(["pull"]))?;
        }
        GitAction::CheckoutBranch(branch) => {
            run(git_command(repo_path).args(["checkout", branch.as_str()]))?;
        }
        GitAction::Revert(hash) => {
            run(git_command(repo_path).args(["revert", "--no-edit", hash.as_str()]))?;
        }
    }
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
}