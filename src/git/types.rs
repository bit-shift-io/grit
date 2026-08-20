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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoState {
    pub current_branch: String,
    pub branches: Vec<String>,
    pub changes: Vec<FileChange>,
    pub history: Vec<CommitInfo>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitAction {
    Stage(String),
    Unstage(String),
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
        };

        let json = serde_json::to_string(&state).unwrap();
        let parsed: RepoState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, state);
    }

    #[test]
    fn git_action_round_trips_through_json() {
        let actions = vec![
            GitAction::Stage("a.txt".to_string()),
            GitAction::Unstage("b.txt".to_string()),
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
        ];
        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            let parsed: GitAction = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn status_variants_are_distinct() {
        assert_ne!(GitStatus::Modified, GitStatus::Untracked);
        assert_ne!(GitStatus::Renamed, GitStatus::Deleted);
        assert_ne!(GitStatus::Staged, GitStatus::Modified);
    }
}