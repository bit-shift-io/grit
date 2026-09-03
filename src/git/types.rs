use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Expands a leading `~` in a user-supplied path to the home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    expand_tilde_with(std::env::var("HOME").ok().as_deref(), path)
}

fn expand_tilde_with(home: Option<&str>, path: &str) -> PathBuf {
    if path == "~" {
        return home.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = home {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Shared validation for "open repository" requests so the desktop GUI and
/// the WebSocket daemon enforce identical rules with identical messages.
/// Returns the tilde-expanded repository path on success.
pub(crate) fn validate_open_repo_input(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Folder path is required".to_string());
    }
    let repo_path = expand_tilde(trimmed);
    if !repo_path.is_dir() {
        return Err(format!("Not a directory: {trimmed}"));
    }
    if !repo_path.join(".git").exists() {
        return Err(format!("Not a git repository: {trimmed}"));
    }
    Ok(repo_path)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitStatus {
    Modified,
    Untracked,
    Renamed,
    Deleted,
    Staged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: GitStatus,
    pub is_staged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub hash: String,
    pub author: String,
    pub message: String,
    pub timestamp: i64,
}

/// One entry in the stash reflog, e.g. `stash@{0}`. `files` lists the paths
/// captured by the stash, reusing the same per-file stat shape as commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashEntry {
    /// Reflog name, e.g. `stash@{0}`.
    pub id: String,
    /// Branch the stash was created on, e.g. `main`.
    pub branch: String,
    /// Short user-provided message (may be empty).
    pub message: String,
    pub timestamp: i64,
    pub files: Vec<FileStat>,
}

/// An executable script discovered in a repository (root, `scripts/`,
/// or `tools/`). `rel_path` is repo-relative and is what gets launched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptEntry {
    pub name: String,
    pub rel_path: String,
}

impl std::fmt::Display for ScriptEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoState {
    pub current_branch: String,
    pub branches: Vec<String>,
    #[serde(default)]
    pub remote_branches: Vec<String>,
    pub changes: Vec<FileChange>,
    pub history: Vec<CommitInfo>,
    #[serde(default)]
    pub stashes: Vec<StashEntry>,
    #[serde(default)]
    pub scripts: Vec<ScriptEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePair {
    pub original: String,
    pub current: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub status: String,
    pub path: String,
    pub insertions: i64,
    pub deletions: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitSummary {
    pub message: String,
    pub author: String,
    pub timestamp: i64,
    pub files_changed: i64,
    pub insertions: i64,
    pub deletions: i64,
    pub files: Vec<FileStat>,
}

impl CommitSummary {
    /// Neutral fallback payload for error responses: every content field is
    /// zeroed and the message slot carries the failure reason.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            author: String::new(),
            timestamp: 0,
            files_changed: 0,
            insertions: 0,
            deletions: 0,
            files: Vec::new(),
        }
    }
}

/// Lifecycle of a logged command: broadcast as `running` the moment a
/// client action arrives, then finalized with captured output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogStatus {
    Running,
    Success,
    Failed,
}

/// One executed git command as shown in the web UI log: the exact command
/// line plus whatever git printed on stdout/stderr.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Monotonic id for stable UI keys and in-place completion updates.
    pub seq: u64,
    /// Display form, e.g. `git push origin main`.
    pub command: String,
    /// Combined stdout+stderr exactly as git produced it (may be empty).
    pub output: String,
    pub status: LogStatus,
    /// Epoch milliseconds when execution started.
    pub started_ms: i64,
    /// Wall-clock duration of the finished command; 0 while running.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitAction {
    Stage(String),
    Unstage(String),
    Discard(String),
    DiscardUntracked(String),
    Commit(String),
    CommitAll(String),
    CommitAllPush(String),
    DiscardAll,
    Push,
    Pull,
    Fetch,
    CheckoutBranch(String),
    Revert(String),
    CreateBranch(String, String),
    CreateTag(String, String),
    DeleteTag(String),
    DeleteBranch(String),
    Reclone,
    NewTab(String),
    CloseTab,
    /// Launch a discovered executable by repo-relative path (fire-and-forget).
    RunScript(String),
    /// Server-side history search via `git log --grep`.
    SearchHistory(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repostate_serializes_and_deserializes() {
        let state = RepoState {
            current_branch: "main".to_string(),
            branches: vec!["main".to_string(), "dev".to_string()],
            remote_branches: vec!["origin/main".to_string(), "origin/feature".to_string()],
            changes: vec![FileChange {
                path: "src/main.rs".to_string(),
                status: GitStatus::Modified,
                is_staged: false,
            }],
            history: vec![CommitInfo {
                hash: "abc123".to_string(),
                author: "Alice".to_string(),
                message: "initial".to_string(),
                timestamp: 1_600_000_000,
            }],
            scripts: vec![ScriptEntry {
                name: "build.sh".to_string(),
                rel_path: "scripts/build.sh".to_string(),
            }],
        };

        let json = serde_json::to_string(&state).unwrap();
        let parsed: RepoState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, state);
    }

    #[test]
    fn commit_summary_error_payload_is_neutral() {
        let summary = CommitSummary::error("no repository tabs open");
        assert_eq!(summary.message, "no repository tabs open");
        assert_eq!(summary.author, "");
        assert_eq!(summary.timestamp, 0);
        assert_eq!(summary.files_changed, 0);
        assert_eq!(summary.insertions, 0);
        assert_eq!(summary.deletions, 0);
        assert!(summary.files.is_empty());

        // Accepts anything string-like; the wire shape must stay stable.
        let owned = CommitSummary::error(String::from("boom"));
        assert_eq!(owned.message, "boom");
    }

    #[test]
    fn repostate_defaults_scripts_for_older_payloads() {
        let parsed: RepoState =
            serde_json::from_str(r#"{"current_branch":"","branches":[],"changes":[],"history":[]}"#)
                .unwrap();
        assert!(parsed.scripts.is_empty(), "missing field must default");
    }

    #[test]
    fn git_action_round_trips_through_json() {
        let actions = vec![
            GitAction::Stage("a.txt".to_string()),
            GitAction::Unstage("b.txt".to_string()),
            GitAction::Discard("c.txt".to_string()),
            GitAction::Commit("fix bug".to_string()),
            GitAction::CommitAll("all the things".to_string()),
            GitAction::CommitAllPush("ship it".to_string()),
            GitAction::DiscardAll,
            GitAction::Push,
            GitAction::Pull,
            GitAction::Fetch,
            GitAction::CheckoutBranch("feature".to_string()),
            GitAction::Revert("deadbeef".to_string()),
            GitAction::CreateBranch("feature".to_string(), "deadbeef".to_string()),
            GitAction::CreateTag("v1.0".to_string(), "deadbeef".to_string()),
            GitAction::DeleteTag("v1.0".to_string()),
            GitAction::DeleteBranch("feature".to_string()),
            GitAction::Reclone,
            GitAction::NewTab(r#"{"name":"new","path":""}"#.to_string()),
            GitAction::CloseTab,
            GitAction::RunScript("scripts/deploy.sh".to_string()),
            GitAction::SearchHistory("embed".to_string()),
        ];
        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            let parsed: GitAction = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn log_entry_round_trips_and_defaults_status_names() {
        let entry = LogEntry {
            seq: 7,
            command: "git pull".to_string(),
            output: "Already up to date.".to_string(),
            status: LogStatus::Success,
            started_ms: 1_700_000_000_000,
            duration_ms: 250,
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains("\"status\":\"success\""), "got: {json}");
        let parsed: LogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry);

        let running: LogEntry = serde_json::from_str(
            r#"{"seq":1,"command":"git push","output":"","status":"running","started_ms":0,"duration_ms":0}"#,
        )
        .unwrap();
        assert_eq!(running.status, LogStatus::Running);
    }

    #[test]
    fn status_variants_are_distinct() {
        assert_ne!(GitStatus::Modified, GitStatus::Untracked);
        assert_ne!(GitStatus::Renamed, GitStatus::Deleted);
        assert_ne!(GitStatus::Staged, GitStatus::Modified);
    }

    #[test]
    fn validate_open_repo_input_enforces_shared_rules() {
        assert_eq!(
            super::validate_open_repo_input("   "),
            Err("Folder path is required".to_string())
        );

        let missing = "/definitely/not/a/dir";
        assert_eq!(
            super::validate_open_repo_input(missing),
            Err(format!("Not a directory: {missing}"))
        );

        let dir = tempfile::tempdir().unwrap();
        let typed = dir.path().to_str().unwrap();
        assert_eq!(
            super::validate_open_repo_input(typed),
            Err(format!("Not a git repository: {typed}")),
            "a plain directory is not a repository"
        );

        crate::test_support::init_repo(dir.path());
        assert_eq!(
            super::validate_open_repo_input(typed),
            Ok(dir.path().to_path_buf())
        );
    }

    #[test]
    fn expand_tilde_resolves_home_prefixes() {
        let home = "/home/bronson";
        assert_eq!(
            super::expand_tilde_with(Some(home), "~/projects/grit"),
            PathBuf::from("/home/bronson/projects/grit")
        );
        assert_eq!(
            super::expand_tilde_with(Some(home), "~"),
            PathBuf::from(home)
        );
        assert_eq!(
            super::expand_tilde_with(Some(home), "/usr/local"),
            PathBuf::from("/usr/local")
        );
        assert_eq!(
            super::expand_tilde_with(None, "~/projects"),
            PathBuf::from("~/projects")
        );
        assert_eq!(
            super::expand_tilde_with(Some(home), "~other/x"),
            PathBuf::from("~other/x")
        );
    }
}