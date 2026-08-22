//! Shared configuration for tabs between desktop and web UIs.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::types::RepoState;

/// A saved tab configuration (shared by desktop and web).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTab {
    pub id: usize,
    pub name: String,
    pub path: String,
}

/// Config folder: `$XDG_CONFIG_HOME/bitshift/grit`
fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("bitshift").join("grit"))
}

/// Path of the shared tabs configuration file.
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.json"))
}

/// Loads saved tabs from the default configuration file.
pub fn load_tabs() -> Vec<SavedTab> {
    config_path()
        .map(|path| load_tabs_from(&path))
        .unwrap_or_default()
}

/// Loads saved tabs from a specific file (empty when absent or invalid).
pub fn load_tabs_from(path: &Path) -> Vec<SavedTab> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!(
                "failed to parse saved tabs at {}: {e}",
                path.display()
            );
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

/// Persists tabs to a file, creating parent directories as needed.
pub fn save_tabs(tabs: &[SavedTab]) {
    if let Some(path) = config_path() {
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                tracing::warn!("failed to create config folder {}: {e}", parent.display());
                return;
            }
        }
        match serde_json::to_vec_pretty(tabs) {
            Ok(bytes) => {
                if let Err(e) = fs::write(&path, bytes) {
                    tracing::warn!("failed to save tabs to {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("failed to serialize tabs: {e}"),
        }
    }
}

/// Converts a WebState to saved tabs and persists them.
pub fn persist_web_state(state: &crate::server::registry::WebState) {
    let tabs: Vec<SavedTab> = state
        .tabs
        .iter()
        // Never persist paths that are not live git repositories, whatever
        // their origin; restore-side pruning alone would keep re-saving junk.
        .filter(|t| is_live_repo(&t.repo_path))
        .map(|t| SavedTab {
            id: t.id,
            name: t.name.clone(),
            path: t.repo_path.clone(),
        })
        .collect();
    save_tabs(&tabs);
}

/// Returns true when the path points at an existing git repository.
pub fn is_live_repo(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let p = PathBuf::from(path);
    p.is_dir() && p.join(".git").exists()
}

/// Drops saved tabs whose repository path no longer exists.
fn prune_dead_tabs(tabs: Vec<SavedTab>) -> Vec<SavedTab> {
    tabs.into_iter()
        .filter(|t| is_live_repo(&t.path))
        .collect()
}

/// Builds a WebState from saved tabs.
fn web_state_from_saved(saved: Vec<SavedTab>) -> crate::server::registry::WebState {
    let mut tabs = Vec::new();
    for tab in saved {
        tabs.push(crate::server::registry::WebTab {
            id: tab.id,
            name: tab.name,
            repo_path: tab.path,
            state: RepoState::default(),
        });
    }
    let active = if tabs.is_empty() { 0 } else { 0 };
    crate::server::registry::WebState { active, tabs }
}

/// Restores a WebState from saved tabs, skipping dead repository paths.
pub fn restore_web_state() -> crate::server::registry::WebState {
    let saved = prune_dead_tabs(load_tabs());
    web_state_from_saved(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let tabs = vec![
            SavedTab {
                id: 1,
                name: "alpha".to_string(),
                path: "/home/me/alpha".to_string(),
            },
            SavedTab {
                id: 2,
                name: "beta".to_string(),
                path: "/home/me/beta".to_string(),
            },
        ];
        save_tabs_to_path(&path, &tabs);
        assert_eq!(load_tabs_from(&path), tabs);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_tabs_from(&dir.path().join("missing.json")).is_empty());
    }

    #[test]
    fn load_invalid_json_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, "not json").unwrap();
        assert!(load_tabs_from(&path).is_empty());
    }

    #[test]
    fn prune_dead_tabs_drops_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let live = SavedTab {
            id: 1,
            name: "live".to_string(),
            path: dir.path().display().to_string(),
        };
        let dead = SavedTab {
            id: 2,
            name: "dead".to_string(),
            path: "/definitely/missing/repo".to_string(),
        };
        let pruned = prune_dead_tabs(vec![dead, live.clone()]);
        assert_eq!(pruned, vec![live]);
    }

    // Helper for tests to use arbitrary paths
    fn save_tabs_to_path(path: &Path, tabs: &[SavedTab]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, serde_json::to_vec_pretty(tabs).unwrap()).unwrap();
    }
}