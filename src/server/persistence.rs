//! Persistence of web server repository tabs to the shared config folder.

use crate::shared_config::{
    persist_web_state as persist_web_state_impl,
    restore_web_state as restore_web_state_impl,
};

/// Converts a WebState to saved tabs and persists them.
pub fn persist_web_state(state: &crate::server::registry::WebState) {
    persist_web_state_impl(state)
}

/// Restores a WebState from saved tabs.
pub fn restore_web_state() -> crate::server::registry::WebState {
    restore_web_state_impl()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared_config::{load_tabs_from, SavedTab};
    use std::fs;
    use std::path::Path;

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-tabs.json");
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
        let path = dir.path().join("web-tabs.json");
        fs::write(&path, "not json").unwrap();
        assert!(load_tabs_from(&path).is_empty());
    }

    // Helper for tests to use arbitrary paths
    fn save_tabs_to_path(path: &Path, tabs: &[SavedTab]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, serde_json::to_vec_pretty(tabs).unwrap()).unwrap();
    }
}