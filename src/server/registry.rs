//! Shared tab registry bridging the desktop GUI and the embedded web daemon.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::git::types::{LogEntry, RepoState};

/// Upper bound on retained log entries per tab; oldest entries fall off.
const MAX_LOG_ENTRIES: usize = 200;

/// A repository tab as exposed to the web UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebTab {
    pub id: usize,
    pub name: String,
    pub repo_path: String,
    pub state: RepoState,
    /// Transcript of executed git commands (terminal-style log).
    #[serde(default)]
    pub log: Vec<LogEntry>,
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
    /// clients can treat every unseen id as a genuinely new tab. Shared
    /// through an `Arc` because every handle (server, desktop GUI) is a
    /// clone: independent counters would hand out colliding ids.
    next_id: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Monotonic allocator for log entry ids; shared across clones for
    /// the same reason as `next_id`.
    next_log_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Clone for TabRegistry {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            rx: self.rx.clone(),
            next_id: std::sync::Arc::clone(&self.next_id),
            next_log_seq: std::sync::Arc::clone(&self.next_log_seq),
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
            next_id: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            next_log_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
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
                log: Vec::new(),
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

    /// Appends a `running` placeholder entry for `tab_id` so every client
    /// sees a command was entered while it is still executing. Returns the
    /// entry's seq for the later [`Self::finish_log_entry`] call, or None
    /// when the tab no longer exists.
    pub fn start_log_entry(&self, tab_id: usize, command: String) -> Option<u64> {
        let seq = self.alloc_log_seq();
        let mut current = self.snapshot();
        let Some(tab) = current.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return None;
        };
        tab.log.push(LogEntry {
            seq,
            command,
            output: String::new(),
            status: crate::git::types::LogStatus::Running,
            started_ms: epoch_millis(),
            duration_ms: 0,
        });
        truncate_log(&mut tab.log);
        self.set(current);
        Some(seq)
    }

    /// Replaces the `running` placeholder `seq` with the final transcript
    /// entries (one per executed git command) and re-broadcasts. Unknown
    /// seq or missing tabs are ignored; each new entry gets a fresh seq.
    pub fn finish_log_entry(
        &self,
        tab_id: usize,
        seq: u64,
        entries: Vec<crate::git::types::LogEntry>,
    ) {
        if entries.is_empty() {
            // Nothing to record (e.g. pure UI actions): drop the placeholder.
            let mut current = self.snapshot();
            if let Some(tab) = current.tabs.iter_mut().find(|t| t.id == tab_id) {
                if tab.log.iter().any(|e| e.seq == seq) {
                    tab.log.retain(|e| e.seq != seq);
                    self.set(current);
                }
            }
            return;
        }
        let mut finished = entries;
        for entry in &mut finished {
            entry.seq = self.alloc_log_seq();
        }
        let mut current = self.snapshot();
        let Some(tab) = current.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        tab.log.retain(|e| e.seq != seq);
        tab.log.extend(finished);
        truncate_log(&mut tab.log);
        self.set(current);
    }

    fn alloc_log_seq(&self) -> u64 {
        self.next_log_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }
}

fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn truncate_log(log: &mut Vec<LogEntry>) {
    if log.len() > MAX_LOG_ENTRIES {
        let excess = log.len() - MAX_LOG_ENTRIES;
        log.drain(..excess);
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
            log: Vec::new(),
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
    fn clones_share_one_id_allocator() {
        let registry = TabRegistry::new();
        let clone_a = registry.clone();
        let clone_b = clone_a.clone();
        let first = clone_a.alloc_id();
        let second = clone_b.alloc_id();
        let third = registry.alloc_id();
        assert_ne!(first, second, "clones must not hand out duplicate ids");
        assert_ne!(first, third);
        assert_ne!(second, third);
    }

    #[test]
    fn clone_shares_watch_channel() {
        let registry = TabRegistry::new();
        let _ = registry.alloc_id();
        let clone = registry.clone();
        clone.set(WebState {
            active: 0,
            tabs: vec![sample_tab(9, "x")],
        });
        assert_eq!(registry.snapshot().tabs.len(), 1);
    }

    #[test]
    fn start_and_finish_log_entry_replace_placeholder() {
        use crate::git::types::LogStatus;

        let registry =
            TabRegistry::with_single_tab(1, "r".to_string(), PathBuf::from("/repo/r"));

        let seq = registry
            .start_log_entry(1, "git push".to_string())
            .unwrap();
        let log = &registry.snapshot().tabs[0].log;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].status, LogStatus::Running);
        assert_eq!(log[0].command, "git push");

        let done = vec![crate::git::types::LogEntry {
            seq: 0,
            command: "git add -A".to_string(),
            output: String::new(),
            status: LogStatus::Success,
            started_ms: 0,
            duration_ms: 5,
        }];
        registry.finish_log_entry(1, seq, done);
        let log = &registry.snapshot().tabs[0].log;
        assert_eq!(log.len(), 1);
        assert_ne!(log[0].seq, seq, "finished entry gets a fresh seq");
        assert_eq!(log[0].command, "git add -A");
        assert_eq!(log[0].status, LogStatus::Success);
    }

    #[test]
    fn finish_log_entry_without_entries_drops_placeholder() {
        use crate::git::types::LogStatus;

        let registry =
            TabRegistry::with_single_tab(2, "r".to_string(), PathBuf::from("/repo/r"));
        let seq = registry.start_log_entry(2, "noop".to_string()).unwrap();
        registry.finish_log_entry(2, seq, Vec::new());
        assert!(registry.snapshot().tabs[0].log.is_empty());

        // Unknown seq/tab combinations are ignored.
        let foreign = crate::git::types::LogEntry {
            seq: 0,
            command: "git status".to_string(),
            output: String::new(),
            status: LogStatus::Success,
            started_ms: 0,
            duration_ms: 0,
        };
        registry.finish_log_entry(99, seq, vec![foreign]);
        assert!(
            registry.snapshot().tabs[0].log.is_empty(),
            "finish targeting an unknown tab must not touch any tab"
        );
    }

    #[test]
    fn start_log_entry_for_unknown_tab_returns_none() {
        let registry = TabRegistry::new();
        assert_eq!(registry.start_log_entry(42, "git pull".to_string()), None);
    }

    #[test]
    fn log_is_capped_dropping_oldest() {
        use crate::git::types::{LogEntry, LogStatus};

        let registry =
            TabRegistry::with_single_tab(3, "r".to_string(), PathBuf::from("/repo/r"));
        for i in 0..(MAX_LOG_ENTRIES + 25) {
            let seq = registry.start_log_entry(3, format!("cmd {i}")).unwrap();
            registry.finish_log_entry(
                3,
                seq,
                vec![LogEntry {
                    seq: 0,
                    command: format!("done {i}"),
                    output: String::new(),
                    status: LogStatus::Success,
                    started_ms: 0,
                    duration_ms: 0,
                }],
            );
        }
        let log = &registry.snapshot().tabs[0].log;
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        assert!(
            log.last().unwrap().command.ends_with(&format!("done {}", MAX_LOG_ENTRIES + 24)),
            "newest entry must be retained: {:?}",
            log.last().unwrap()
        );
    }

    #[test]
    fn webtab_log_defaults_for_older_payloads() {
        let json = r#"{"id":0,"name":"n","repo_path":"/repo/n","state":{"current_branch":"","branches":[],"changes":[],"history":[]}}"#;
        let tab: WebTab = serde_json::from_str(json).unwrap();
        assert!(tab.log.is_empty(), "missing log field must default");
    }
}