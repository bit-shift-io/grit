// File diff helpers shared by the desktop GUI and the web diff view.
// Previous file-browser functions (tree listing, search, preview, external
// apps) were removed in favor of the external folio file explorer.

use super::*;

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

/// Reads both sides of a changed file: the worktree version and the committed
/// `HEAD` version. Returns an empty `original` for untracked files (git failure
/// because the path does not exist in `HEAD`); any other failure is surfaced so
/// the UI can distinguish \"untracked\" from a genuine read problem.
pub fn get_file_pair(repo_path: &Path, path: &str) -> Result<FilePair, GitError> {
    let original = match run(git_command(repo_path).args(["show", &format!("HEAD:{path}")])) {
        Ok(output) => output,
        Err(e) if e.stderr.contains("not in 'HEAD'") || e.stderr.contains("does not exist") => {
            String::new()
        }
        Err(e) => {
            return Err(GitError {
                message: format!("failed to read HEAD version of {path}"),
                stderr: e.stderr,
                stdout: e.stdout,
            })
        }
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_repo};
    use std::fs;

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
        fs::write(dir.path().join("base.txt"), "committed\n").unwrap();
        commit_all(dir.path(), "initial");
        fs::write(dir.path().join("new.txt"), "brand new\n").unwrap();

        let pair = get_file_pair(dir.path(), "new.txt").unwrap();
        assert_eq!(pair.original, "");
        assert_eq!(pair.current, "brand new\n");
    }

    #[test]
    fn get_file_pair_errors_when_head_read_fails() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file.txt"), "hello\n").unwrap();

        let result = get_file_pair(dir.path(), "file.txt");
        assert!(
            result.is_err(),
            "a repo-level git failure must surface as an error, not a silent empty original"
        );
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
}