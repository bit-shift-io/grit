//! Persistence of configured repository tabs to the platform data folder.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A saved repository tab configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRepo {
    pub name: String,
    pub path: PathBuf,
}

/// Grit data folder, e.g. `$XDG_DATA_HOME/grit` on Linux.
fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("grit"))
}

/// Path of the repositories configuration file.
pub fn config_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("repos.json"))
}

/// Loads saved repositories from the default configuration file.
pub fn load_repos() -> Vec<SavedRepo> {
    config_path()
        .map(|path| load_repos_from(&path))
        .unwrap_or_default()
}

/// Loads saved repositories from a specific file (empty when absent or invalid).
pub fn load_repos_from(path: &Path) -> Vec<SavedRepo> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(
                "failed to parse saved repositories at {}: {e}",
                path.display()
            );
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

/// Persists repositories to a file, creating parent directories as needed.
pub fn save_repos(path: &Path, repos: &[SavedRepo]) {
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            tracing::warn!("failed to create data folder {}: {e}", parent.display());
            return;
        }
    }
    match serde_json::to_vec_pretty(repos) {
        Ok(bytes) => {
            if let Err(e) = fs::write(path, bytes) {
                tracing::warn!("failed to save repositories to {}: {e}", path.display());
            }
        }
        Err(e) => tracing::warn!("failed to serialize repositories: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.json");
        let repos = vec![
            SavedRepo {
                name: "alpha".to_string(),
                path: PathBuf::from("/home/me/alpha"),
            },
            SavedRepo {
                name: "beta".to_string(),
                path: PathBuf::from("/home/me/beta"),
            },
        ];
        save_repos(&path, &repos);
        assert_eq!(load_repos_from(&path), repos);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_repos_from(&dir.path().join("missing.json")).is_empty());
    }

    #[test]
    fn load_invalid_json_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("repos.json");
        fs::write(&path, "not json").unwrap();
        assert!(load_repos_from(&path).is_empty());
    }
}