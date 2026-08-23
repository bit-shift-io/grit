use serde::{Deserialize, Serialize};

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
    pub changes: Vec<FileChange>,
    pub history: Vec<CommitInfo>,
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
    Nuke,
    NewTab(String),
    CloseTab,
    /// Launch a discovered executable by repo-relative path (fire-and-forget).
    RunScript(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repostate_serializes_and_deserializes() {
        let state = RepoState {
            current_branch: "main".to_string(),
            branches: vec!["main".to_string(), "dev".to_string()],
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
            GitAction::Nuke,
            GitAction::NewTab(r#"{"name":"new","path":""}"#.to_string()),
            GitAction::CloseTab,
            GitAction::RunScript("scripts/deploy.sh".to_string()),
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
}