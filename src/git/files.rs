// File browsing, preview, and external-app helpers.

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

/// Image extensions render inline in the preview pane. Driven by the shared
/// `IMAGE_EXTS` list so preview behavior stays aligned with `mime_for_path`.
pub fn is_image_path(path: &str) -> bool {
    mime_for_path(path).starts_with("image/")
}

/// Directories skipped when walking the repository tree for the file browser
/// and the file search. Kept in one place so `list_dir` and `search_files`
/// always agree on what to hide.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    ".idea",
    ".vscode",
    ".DS_Store",
];

/// Joins a caller-supplied relative path onto `repo_path`, refusing absolute
/// paths and `..` traversal components. Returns None when unsafe so handlers
/// can reject the request before touching the filesystem.
pub fn safe_join(repo_path: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
    {
        return None;
    }
    Some(repo_path.join(rel_path))
}

/// Lists the immediate children of a directory inside the repository for the
/// file browser. Empty `dir` means the repo root. `.git/` and common build/vendor
/// directories are skipped so navigation stays fast and focused on source.
/// Directories are returned first, then files, each alphabetically.
pub fn list_dir(repo_path: &Path, dir: &str) -> Result<Vec<FileTreeEntry>, GitError> {
    let base = if dir.is_empty() {
        repo_path.to_path_buf()
    } else {
        // Prevent path traversal outside the repository.
        match safe_join(repo_path, dir) {
            Some(base) => base,
            None => return Ok(Vec::new()),
        }
    };

    let mut entries = Vec::new();
    let read = match std::fs::read_dir(&base) {
        Ok(r) => r,
        Err(_) => return Ok(Vec::new()),
    };

    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let rel = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        match item.file_type() {
            Ok(ft) if ft.is_dir() => entries.push(FileTreeEntry {
                name,
                path: rel,
                is_dir: true,
                depth: 0,
            }),
            Ok(ft) if ft.is_file() => entries.push(FileTreeEntry {
                name,
                path: rel,
                is_dir: false,
                depth: 0,
            }),
            _ => {}
        }
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Recursively searches the repository for files whose path contains `query`
/// (case-insensitive substring match). Returns at most `limit` results.
/// Skips the same directories as `list_dir`.
pub fn search_files(repo_path: &Path, query: &str, limit: usize) -> Result<Vec<FileTreeEntry>, GitError> {
    let q = query.to_lowercase();
    let mut results = Vec::new();
    let mut stack = vec![String::new()]; // relative dir paths; "" = root
    while let Some(dir) = stack.pop() {
        let base = if dir.is_empty() {
            repo_path.to_path_buf()
        } else {
            repo_path.join(&dir)
        };
        let read = match std::fs::read_dir(&base) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let mut children = Vec::new();
        for item in read.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let rel = if dir.is_empty() { name.clone() } else { format!("{dir}/{name}") };
            match item.file_type() {
                Ok(ft) if ft.is_dir() => children.push((name, rel, true)),
                Ok(ft) if ft.is_file() => children.push((name, rel, false)),
                _ => {}
            }
        }
        // Directories first (so recursion explores tree depth-first), then files.
        children.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        for (name, rel, is_dir) in children {
            if is_dir {
                stack.push(rel);
            } else if rel.to_lowercase().contains(&q) {
                results.push(FileTreeEntry { name, path: rel, is_dir: false, depth: 0 });
                if results.len() >= limit {
                    return Ok(results);
                }
            }
        }
    }
    // Sort by path for stable output.
    results.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
    Ok(results)
}

/// Returns the MIME type for a file path based on its extension. Classification
/// reuses the shared `IMAGE_EXTS` / `TEXT_EXTS` lists so this always agrees with
/// `shared_config` (editor config, is-image preview) and `is_image_path`.
pub fn mime_for_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !crate::shared_config::IMAGE_EXTS.contains(&ext.as_str()) {
        if crate::shared_config::TEXT_EXTS.contains(&ext.as_str()) {
            return "text/plain";
        }
        return "application/octet-stream";
    }
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        "tiff" | "tif" => "image/tiff",
        "psd" => "image/vnd.adobe.photoshop",
        "ai" | "eps" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

/// An entry returned by `list_apps_for_mime`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppEntry {
    pub name: String,
    pub exec: String,
}

/// Scans installed `.desktop` files for applications that can handle the
/// given MIME type. Uses `freedesktop-desktop-entry` to iterate all known
/// desktop entries. Returns at most 30 apps sorted by name.
#[cfg(not(target_os = "macos"))]
pub fn list_apps_for_mime(mime_type: &str) -> Vec<AppEntry> {
    use freedesktop_desktop_entry::{default_paths, DesktopEntry, Iter};

    let locales = freedesktop_desktop_entry::get_languages_from_env();
    let mut seen_exec = std::collections::HashSet::new();
    let mut apps = Vec::new();

    for path in Iter::new(default_paths()) {
        let Ok(entry) = DesktopEntry::from_path(path, Some(&locales)) else {
            continue;
        };
        if entry.hidden() {
            continue;
        }
        if entry.exec().is_none() {
            continue;
        }
        if let Some(mimes) = entry.mime_type() {
            if mimes.iter().any(|m| *m == mime_type) {
                let name = entry
                    .name(&locales)
                    .unwrap_or_default()
                    .to_string();
                let exec = entry.exec().unwrap_or("").to_string();
                if !name.is_empty() && !exec.is_empty() && seen_exec.insert(exec.clone()) {
                    apps.push(AppEntry { name, exec });
                }
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps.truncate(30);
    apps
}

#[cfg(target_os = "macos")]
pub fn list_apps_for_mime(_mime_type: &str) -> Vec<AppEntry> {
    Vec::new()
}

/// Reads a worktree file for the preview pane, detecting binary contents and
/// image files. Errors are folded into the payload's `error` field so the
/// handler can return a 200 with a graceful client-side message.
pub fn get_file_content(repo_path: &Path, path: &str) -> FileContent {
    let Some(full) = safe_join(repo_path, path) else {
        return FileContent {
            path: path.to_string(),
            size: 0,
            is_binary: false,
            is_image: false,
            content: String::new(),
            error: "path escapes the repository".to_string(),
        };
    };
    let meta = match std::fs::metadata(&full) {
        Ok(m) => m,
        Err(e) => {
            return FileContent {
                path: path.to_string(),
                size: 0,
                is_binary: false,
                is_image: false,
                content: String::new(),
                error: format!("failed to read {path}: {e}"),
            }
        }
    };
    let bytes = match std::fs::read(&full) {
        Ok(b) => b,
        Err(e) => {
            return FileContent {
                path: path.to_string(),
                size: 0,
                is_binary: false,
                is_image: false,
                content: String::new(),
                error: format!("failed to read {path}: {e}"),
            }
        }
    };
    let is_image = is_image_path(path);
    let is_binary = !is_image && bytes.contains(&0u8);
    let content = if is_binary || is_image {
        String::new()
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    FileContent {
        path: path.to_string(),
        size: meta.len(),
        is_binary,
        is_image,
        content,
        error: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_repo};
    use std::fs;


    #[test]
    fn safe_join_rejects_traversal_and_absolute_paths() {
        let repo = std::path::Path::new("/repo");
        assert_eq!(
            safe_join(repo, "a/b.txt"),
            Some(std::path::PathBuf::from("/repo/a/b.txt"))
        );
        assert_eq!(safe_join(repo, ""), Some(std::path::PathBuf::from("/repo")));
        assert!(safe_join(repo, "../etc/passwd").is_none());
        assert!(safe_join(repo, "a/../../etc/passwd").is_none());
        assert!(safe_join(repo, "/etc/passwd").is_none());
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
