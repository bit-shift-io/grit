//! Shared tab registry bridging the desktop GUI and the embedded web daemon.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::git::types::RepoState;

/// A repository tab as exposed to the web UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebTab {
    pub id: usize,
    pub name: String,
    pub repo_path: String,
    pub state: RepoState,
}

/// The full set of open tabs, broadcast to connected web clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebState {
    pub active: usize,
    pub tabs: Vec<WebTab>,
}

impl Default for WebState {
    fn default() -> Self {
        Self {
            active: 0,
            tabs: Vec::new(),
        }
    }
}

/// Shared registry that both the desktop GUI and web server read/write.
#[derive(Debug)]
pub struct TabRegistry {
    tx: watch::Sender<WebState>,
    rx: watch::Receiver<WebState>,
    /// Monotonic id allocator; ids are never reused within a session so
    /// clients can treat every unseen id as a genuinely new tab.
    next_id: std::sync::atomic::AtomicUsize,
}

impl Clone for TabRegistry {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            rx: self.rx.clone(),
            next_id: std::sync::atomic::AtomicUsize::new(
                self.next_id.load(std::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

impl TabRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(WebState::default());
        Self {
            tx,
            rx,
            next_id: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Allocates a fresh tab id that will never be handed out again.
    pub fn alloc_id(&self) -> usize {
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Ensures future allocations stay above every id in `used`.
    pub fn raise_next_id_floor(&self, used: impl IntoIterator<Item = usize>) {
        let max = used.into_iter().max().map_or(0, |m| m + 1);
        self.next_id.fetch_max(max, std::sync::atomic::Ordering::Relaxed);
    }

    /// Creates a registry pre-seeded with a single repository tab.
    pub fn with_single_tab(id: usize, name: String, repo_path: PathBuf) -> Self {
        let registry = Self::new();
        registry.raise_next_id_floor(std::iter::once(id));
        registry.set(WebState {
            active: 0,
            tabs: vec![WebTab {
                id,
                name,
                repo_path: repo_path.display().to_string(),
                state: RepoState::default(),
            }],
        });
        registry
    }

    /// Replaces the entire tab list.
    pub fn set(&self, state: WebState) {
        let _ = self.tx.send(state);
    }

    /// Returns the current snapshot of the tab list.
    pub fn snapshot(&self) -> WebState {
        self.rx.borrow().clone()
    }

    /// Returns a receiver that observes subsequent registry changes.
    pub fn subscribe(&self) -> watch::Receiver<WebState> {
        self.rx.clone()
    }

    /// Updates the state of a single tab in place.
    pub fn update_state(&self, id: usize, state: RepoState) {
        let mut current = self.snapshot();
        if let Some(tab) = current.tabs.iter_mut().find(|t| t.id == id) {
            tab.state = state;
            self.set(current);
        }
    }

    /// Resolves the repository path backing a tab id.
    pub fn repo_path_for(&self, id: usize) -> Option<PathBuf> {
        self.snapshot()
            .tabs
            .into_iter()
            .find(|t| t.id == id)
            .map(|t| PathBuf::from(t.repo_path))
    }
}

impl Default for TabRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tab(id: usize, name: &str) -> WebTab {
        WebTab {
            id,
            name: name.to_string(),
            repo_path: format!("/repo/{name}"),
            state: RepoState {
                current_branch: "main".to_string(),
                branches: vec!["main".to_string()],
                changes: vec![],
                history: vec![],
                scripts: vec![],
            },
        }
    }

    #[test]
    fn default_registry_snapshot_is_empty() {
        let registry = TabRegistry::new();
        assert_eq!(registry.snapshot(), WebState::default());
    }

    #[test]
    fn with_single_tab_seeds_one_tab() {
        let registry =
            TabRegistry::with_single_tab(3, "proj".to_string(), PathBuf::from("/repo/proj"));
        let state = registry.snapshot();
        assert_eq!(state.active, 0);
        assert_eq!(state.tabs.len(), 1);
        assert_eq!(state.tabs[0].id, 3);
        assert_eq!(state.tabs[0].name, "proj");
        assert_eq!(state.tabs[0].repo_path, "/repo/proj");
    }

    #[test]
    fn set_replaces_entire_tab_list() {
        let registry = TabRegistry::new();
        registry.set(WebState {
            active: 1,
            tabs: vec![sample_tab(0, "a"), sample_tab(1, "b")],
        });
        let state = registry.snapshot();
        assert_eq!(state.active, 1);
        assert_eq!(state.tabs.len(), 2);
    }

    #[test]
    fn update_state_edits_matching_tab_only() {
        let registry = TabRegistry::new();
        registry.set(WebState {
            active: 0,
            tabs: vec![sample_tab(0, "a"), sample_tab(1, "b")],
        });
        let fresh = RepoState {
            current_branch: "dev".to_string(),
            branches: vec!["dev".to_string()],
            changes: vec![],
            history: vec![],
            scripts: vec![],
        };
        registry.update_state(1, fresh.clone());
        let state = registry.snapshot();
        assert_eq!(state.tabs[0].state.current_branch, "main");
        assert_eq!(state.tabs[1].state, fresh);
    }

    #[test]
    fn repo_path_for_resolves_id() {
        let registry = TabRegistry::new();
        registry.set(WebState {
            active: 0,
            tabs: vec![sample_tab(7, "alpha")],
        });
        assert_eq!(
            registry.repo_path_for(7),
            Some(PathBuf::from("/repo/alpha"))
        );
        assert_eq!(registry.repo_path_for(99), None);
    }

    #[tokio::test]
    async fn subscribe_receives_change_notifications() {
        let registry = TabRegistry::new();
        let mut rx = registry.subscribe();
        registry.set(WebState {
            active: 0,
            tabs: vec![sample_tab(0, "new")],
        });
        assert!(rx.changed().await.is_ok());
        assert_eq!(registry.snapshot().tabs.len(), 1);
    }

    #[test]
    fn alloc_ids_are_monotonic_and_never_reused() {
        let registry = TabRegistry::with_single_tab(3, "a".to_string(), PathBuf::from("/repo/a"));
        assert_eq!(registry.alloc_id(), 4);
        assert_eq!(registry.alloc_id(), 5);

        // Removing the highest tab must not recycle its id.
        registry.raise_next_id_floor(std::iter::once(5));
        assert_eq!(registry.alloc_id(), 6);
    }

    #[test]
    fn clone_shares_watch_channel_but_copies_counter() {
        let registry = TabRegistry::new();
        let _ = registry.alloc_id();
        let clone = registry.clone();
        assert_eq!(clone.alloc_id(), 1);
        clone.set(WebState {
            active: 0,
            tabs: vec![sample_tab(9, "x")],
        });
        assert_eq!(registry.snapshot().tabs.len(), 1);
    }
}