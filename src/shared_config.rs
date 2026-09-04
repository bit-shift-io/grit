//! Shared configuration for tabs between desktop and web UIs.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::types::RepoState;

/// Configuration for external editors keyed by file extension.
///
/// `defaults()` resolves environment variables so callers get a ready-to-use
/// map even when no config file exists yet:
/// - Known text extensions → `$EDITOR` (falls back to `code`)
/// - Known image extensions → `$IMG_EDITOR` (falls back to `xdg-open`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorConfig {
    /// Extension (without dot) → shell command.
    #[serde(flatten)]
    pub editors: std::collections::HashMap<String, String>,
}

impl EditorConfig {
    pub fn defaults() -> Self {
        let text_cmd = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| "code".to_string());
        let img_cmd = std::env::var("IMG_EDITOR")
            .unwrap_or_else(|_| "xdg-open".to_string());
        let mut editors = std::collections::HashMap::new();
        for ext in TEXT_EXTS {
            editors.insert(ext.to_string(), text_cmd.clone());
        }
        for ext in IMAGE_EXTS {
            editors.insert(ext.to_string(), img_cmd.clone());
        }
        EditorConfig { editors }
    }

    /// Look up the editor for a given file path by its extension.
    pub fn for_path(&self, path: &str) -> String {
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        self.editors
            .get(&ext)
            .cloned()
            .unwrap_or_else(|| {
                // Fallback: try $EDITOR, then $VISUAL, then "code"
                std::env::var("EDITOR")
                    .or_else(|_| std::env::var("VISUAL"))
                    .unwrap_or_else(|_| "code".to_string())
            })
    }
}

const TEXT_EXTS: &[&str] = &[
    "rs", "py", "js", "ts", "tsx", "jsx", "c", "cpp", "h", "hpp", "go", "java",
    "rb", "php", "sh", "bash", "zsh", "fish", "vim", "lua", "r", "swift", "kt",
    "cs", "fs", "hs", "ex", "exs", "erl", "clj", "lisp", "el", "jl",
    "toml", "yaml", "yml", "json", "jsonc", "json5", "xml", "html", "htm",
    "css", "scss", "less", "sql", "graphql", "proto", "md", "txt", "csv",
    "ini", "cfg", "conf", "env", "gitignore", "gitattributes", "dockerignore",
    "dockerfile", "makefile", "cmake", "nix", "zig",
];

const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "ico", "avif", "tiff",
    "tif", "psd", "ai", "eps",
];

/// A saved tab configuration (shared by desktop and web).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTab {
    pub id: usize,
    pub name: String,
    pub path: String,
}

/// Full application configuration persisted in `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GritConfig {
    pub tabs: Vec<SavedTab>,
    #[serde(default)]
    pub editors: Option<EditorConfig>,
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
///
/// Handles both the legacy format (bare JSON array) and the new
/// `GritConfig` object format with `tabs` + `editors`.
pub fn load_tabs_from(path: &Path) -> Vec<SavedTab> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    // Try new object format first.
    if let Ok(cfg) = serde_json::from_slice::<GritConfig>(&bytes) {
        return cfg.tabs;
    }
    // Fall back to legacy bare-array format.
    serde_json::from_slice::<Vec<SavedTab>>(&bytes).unwrap_or_else(|e| {
        tracing::warn!("failed to parse config at {}: {e}", path.display());
        Vec::new()
    })
}

/// Loads the full config, or defaults when absent/invalid.
pub fn load_config() -> GritConfig {
    let path = match config_path() {
        Some(p) => p,
        None => return GritConfig { tabs: Vec::new(), editors: None },
    };
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("failed to parse config at {}: {e}", path.display());
            GritConfig { tabs: Vec::new(), editors: None }
        }),
        Err(_) => GritConfig { tabs: Vec::new(), editors: None },
    }
}

/// Returns the editor config, loading from file or falling back to defaults.
pub fn load_editor_config() -> EditorConfig {
    let cfg = load_config();
    cfg.editors.unwrap_or_else(EditorConfig::defaults)
}

/// Persists the full config, creating parent directories as needed.
pub fn save_config(cfg: &GritConfig) {
    if let Some(path) = config_path() {
        if let Some(parent) = path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                tracing::warn!("failed to create config folder {}: {e}", parent.display());
                return;
            }
        }
        match serde_json::to_vec_pretty(cfg) {
            Ok(bytes) => {
                if let Err(e) = fs::write(&path, bytes) {
                    tracing::warn!("failed to save config to {}: {e}", path.display());
                }
            }
            Err(e) => tracing::warn!("failed to serialize config: {e}"),
        }
    }
}

/// Converts a WebState to saved tabs and persists them.
pub fn persist_web_state(state: &crate::server::registry::WebState) {
    let mut cfg = load_config();
    cfg.tabs = state
        .tabs
        .iter()
        .filter(|t| is_live_repo(&t.repo_path))
        .map(|t| SavedTab {
            id: t.id,
            name: t.name.clone(),
            path: t.repo_path.clone(),
        })
        .collect();
    save_config(&cfg);
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
///
/// Ids must be unique — clients key selection and "active" styling on them
/// — but configs written by earlier versions could contain collisions.
/// Kept ids pass through untouched; a collision is reassigned above every
/// id seen so far, keeping future allocations collision-free too.
fn web_state_from_saved(saved: Vec<SavedTab>) -> crate::server::registry::WebState {
    let mut tabs = Vec::with_capacity(saved.len());
    let mut seen = std::collections::HashSet::new();
    let mut next_fallback = saved.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    for tab in saved {
        let id = if seen.insert(tab.id) {
            tab.id
        } else {
            let id = next_fallback;
            next_fallback += 1;
            tracing::warn!(
                "saved tabs contained duplicate id {}; reassigned {} to {id}",
                tab.id,
                tab.name
            );
            id
        };
        tabs.push(crate::server::registry::WebTab {
            id,
            name: tab.name,
            repo_path: tab.path,
            state: RepoState::default(),
            log: Vec::new(),
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

    #[test]
    fn restored_states_never_contain_duplicate_ids() {
        // A config written while the id-allocator bug was live can hold two
        // tabs with the same id; restore must reassign one so clients never
        // see two "active" tabs for a single selection id.
        let healed = web_state_from_saved(vec![
            SavedTab { id: 0, name: "a".into(), path: "/r/a".into() },
            SavedTab { id: 1, name: "b".into(), path: "/r/b".into() },
            SavedTab { id: 0, name: "grit".into(), path: "/r/grit".into() },
            SavedTab { id: 3, name: "d".into(), path: "/r/d".into() },
        ]);
        let mut ids: Vec<usize> = healed.tabs.iter().map(|t| t.id).collect();
        let len = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), len, "duplicate ids survived restore: {ids:?}");
        // Existing ids keep their value; only collisions are reassigned.
        assert_eq!(healed.tabs[0].id, 0);
        assert_eq!(healed.tabs[1].id, 1);
        assert_eq!(healed.tabs[2].name, "grit");
        assert_ne!(healed.tabs[2].id, 0);
        assert_eq!(healed.tabs[3].id, 3);
        // Reassigned ids must stay above every other id so future allocs
        // cannot collide either.
        assert!(healed.tabs[2].id > 3);
    }

    // Helper for tests to use arbitrary paths
    fn save_tabs_to_path(path: &Path, tabs: &[SavedTab]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, serde_json::to_vec_pretty(tabs).unwrap()).unwrap();
    }

    #[test]
    fn editor_config_defaults_populate_text_and_image_exts() {
        let cfg = EditorConfig::defaults();
        assert!(cfg.editors.contains_key("rs"));
        assert!(cfg.editors.contains_key("py"));
        assert!(cfg.editors.contains_key("png"));
        assert!(cfg.editors.contains_key("jpg"));
    }

    #[test]
    fn editor_config_for_path_returns_correct_editor() {
        let mut editors = std::collections::HashMap::new();
        editors.insert("rs".to_string(), "zed".to_string());
        editors.insert("png".to_string(), "krita".to_string());
        let cfg = EditorConfig { editors };
        assert_eq!(cfg.for_path("src/main.rs"), "zed");
        assert_eq!(cfg.for_path("image.png"), "krita");
        // Unknown ext falls back to $EDITOR or "code"
        let fallback = cfg.for_path("foo.xyz");
        assert!(!fallback.is_empty());
    }

    #[test]
    fn editor_config_round_trips_through_grit_config() {
        let mut editors = std::collections::HashMap::new();
        editors.insert("rs".to_string(), "zed".to_string());
        let grit = GritConfig {
            tabs: vec![],
            editors: Some(EditorConfig { editors }),
        };
        let json = serde_json::to_vec(&grit).unwrap();
        let loaded: GritConfig = serde_json::from_slice(&json).unwrap();
        let ec = loaded.editors.unwrap();
        assert_eq!(ec.for_path("main.rs"), "zed");
    }

    #[test]
    fn grit_config_backward_compat_bare_tab_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        // Write legacy bare-array format
        fs::write(&path, br#"[{"id":1,"name":"t","path":"/p"}]"#).unwrap();
        let tabs = load_tabs_from(&path);
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].name, "t");
    }
}