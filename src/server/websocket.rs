//! WebSocket real-time state streaming and `GitAction` dispatch.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::stream::StreamExt;
use futures_util::SinkExt;

use crate::git::types::GitAction;
use crate::server::AppState;

#[derive(Debug, serde::Deserialize)]
struct ClientMessage {
    /// Absent/null when the client has no tab selected (e.g. right after
    /// removing the last tab); tab-scoped actions then simply no-op.
    tab: Option<usize>,
    action: GitAction,
}

/// Serializes a client operation into the exact wire format the browser
/// uses, so the desktop GUI in remote mode speaks the same protocol.
#[cfg(any(test, feature = "desktop"))]
pub fn encode_client_message(tab: Option<usize>, action: &GitAction) -> String {
    serde_json::json!({ "tab": tab, "action": action }).to_string()
}

/// Opens a validated repository tab in the shared registry. This is the
/// single mutation point for adding tabs; both the WebSocket handler and
/// the desktop GUI route through it so ids come from one allocator.
///
/// The repository directory must exist and contain `.git`; the name is
/// derived from the folder when empty or "new".
pub async fn open_repo_tab(
    registry: &crate::server::registry::TabRegistry,
    name: String,
    path: String,
) -> Result<usize, String> {
    let repo_path = crate::git::types::validate_open_repo_input(&path)?;
    let tab_name = if name.is_empty() || name == "new" {
        repo_path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "repo".to_string())
    } else {
        name
    };
    let new_id = registry.alloc_id();
    let mut new_state = registry.snapshot();
    new_state.tabs.push(crate::server::registry::WebTab {
        id: new_id,
        name: tab_name,
        repo_path: repo_path.display().to_string(),
        state: crate::git::types::RepoState::default(),
        log: Vec::new(),
    });
    new_state.active = new_state.tabs.len() - 1;
    registry.set(new_state);
    Ok(new_id)
}

/// Removes one tab from the workspace by id. The repository itself on disk
/// is left untouched. Returns false when the id is unknown.
pub fn close_tab_by_id(registry: &crate::server::registry::TabRegistry, id: usize) -> bool {
    match registry.remove_tab(id) {
        Some(removed) => {
            tracing::info!(
                "CloseTab removed id={} name={} path={}",
                removed.id,
                removed.name,
                removed.repo_path
            );
            true
        }
        None => {
            tracing::warn!("CloseTab ignored: unknown tab id {}", id);
            false
        }
    }
}

/// Parses the NewTab JSON payload into (name, path).
fn parse_new_tab_payload(payload: &str) -> (String, String) {
    if payload.starts_with('{') {
        #[derive(serde::Deserialize)]
        struct NewTabPayload {
            name: String,
            path: String,
        }
        if let Ok(parsed) = serde_json::from_str::<NewTabPayload>(payload) {
            (parsed.name, parsed.path)
        } else {
            ("new".to_string(), String::new())
        }
    } else {
        (payload.to_string(), String::new())
    }
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(app): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_websocket(socket, app))
}

async fn handle_websocket(socket: WebSocket, app: AppState) {
    let (mut sender, mut receiver) = socket.split();

    let current = app.registry.snapshot();
    if let Ok(text) = serde_json::to_string(&current) {
        if sender.send(Message::Text(text.into())).await.is_err() {
            return;
        }
    }

    let mut broadcast_rx = app.broadcast.subscribe();

    // Actions execute on a dedicated worker so this select loop never blocks
    // on a slow git command. While a pull/push runs, streaming log revisions
    // broadcast through `broadcast_rx` and must reach this very connection
    // immediately — awaiting dispatch inline here is exactly what froze them
    // until the command exited. The worker consumes one action at a time to
    // preserve per-client ordering.
    let (action_tx, mut action_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let worker_app = app.clone();
    let worker = tokio::spawn(async move {
        while let Some(text) = action_rx.recv().await {
            handle_client_text(&worker_app, &text).await;
        }
    });

    loop {
        tokio::select! {
            incoming = receiver.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if action_tx.send(text.to_string()).is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            update = broadcast_rx.recv() => match update {
                Ok(state) => {
                    if !push_state(&mut sender, state).await {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        }
    }

    // Closing the channel ends the worker after any in-flight action, so its
    // transcript still lands in the registry even if the peer vanished.
    drop(action_tx);
    let _ = worker.await;
}

/// Parses and runs one inbound client action; malformed payloads are logged
/// and dropped rather than tearing down the connection.
async fn handle_client_text(app: &AppState, text: &str) {
    match serde_json::from_str::<ClientMessage>(text) {
        Ok(msg) => dispatch_and_refresh(app, msg).await,
        Err(e) => tracing::debug!("ignoring malformed action: {e}"),
    }
}

/// Serializes and sends one state frame. Returns false only when the peer
/// is gone, signalling the caller to end the session.
async fn push_state(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    state: crate::server::registry::WebState,
) -> bool {
    let Ok(text) = serde_json::to_string(&state) else {
        return true;
    };
    sender.send(Message::Text(text.into())).await.is_ok()
}

async fn dispatch_and_refresh(app: &AppState, msg: ClientMessage) {
    if matches!(msg.action, crate::git::types::GitAction::CloseTab) {
        let Some(target_id) = msg.tab else {
            tracing::debug!("CloseTab ignored: no tab id provided");
            return;
        };
        close_tab_by_id(&app.registry, target_id);
        return;
    }

    if let crate::git::types::GitAction::NewTab(payload) = &msg.action {
        let (name, path) = parse_new_tab_payload(payload);
        // The "+" form lives entirely in each client's local UI state;
        // NewTab only ever appends a fully validated repository tab.
        match open_repo_tab(&app.registry, name, path).await {
            Ok(new_id) => crate::server::refresh_tab(app, new_id).await,
            Err(reason) => tracing::debug!("NewTab rejected: {reason}"),
        }
        return;
    }

    if let crate::git::types::GitAction::SearchHistory(query) = &msg.action {
        let Some(tab_id) = msg.tab else {
            tracing::debug!("SearchHistory ignored: no tab id");
            return;
        };
        let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
            tracing::debug!("SearchHistory ignored: unknown tab {}", tab_id);
            return;
        };
        let query = query.clone();
        let results = tokio::task::spawn_blocking(move || {
            crate::git::search_history(std::path::Path::new(&repo_path), &query)
        })
        .await;
        match results {
            Ok(Ok(commits)) => {
                app.registry.update_tab_history(tab_id, commits);
            }
            Ok(Err(e)) => tracing::error!("search_history failed: {e}"),
            Err(e) => tracing::error!("search_history task panicked: {e}"),
        }
        // update_tab_history already mutated the registry, which broadcasts a
        // fresh frame. Do NOT refresh_tab here: get_repository_status resets
        // history to the default window, clobbering the search results.
        return;
    }

    if let crate::git::types::GitAction::OpenExternal(path) = &msg.action {
        let Some(tab_id) = msg.tab else {
            tracing::debug!("OpenExternal ignored: no tab id");
            return;
        };
        let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
            tracing::debug!("OpenExternal ignored: unknown tab {}", tab_id);
            return;
        };
        let path = path.clone();
        let repo_path = repo_path.clone();
        tokio::task::spawn_blocking(move || {
            let file_path = std::path::Path::new(&repo_path).join(&path);
            let editor_cfg = crate::shared_config::load_editor_config();
            let editor = editor_cfg.for_path(&path);
            let _ = std::process::Command::new(&editor)
                .arg(&file_path)
                .spawn();
        }).await.ok();
        return;
    }

    if let crate::git::types::GitAction::OpenWith(path, exec) = &msg.action {
        let Some(tab_id) = msg.tab else {
            tracing::debug!("OpenWith ignored: no tab id");
            return;
        };
        let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
            tracing::debug!("OpenWith ignored: unknown tab {}", tab_id);
            return;
        };
        let path = path.clone();
        let exec = exec.clone();
        let repo_path = repo_path.clone();
        tokio::task::spawn_blocking(move || {
            let file_path = std::path::Path::new(&repo_path).join(&path);
            // exec may contain field codes like %f — strip them for basic spawning
            let cmd = exec.split_whitespace().next().unwrap_or(&exec);
            let _ = std::process::Command::new(cmd)
                .arg(&file_path)
                .spawn();
        }).await.ok();
        return;
    }

    if let crate::git::types::GitAction::DeleteFile(path) = &msg.action {
        let Some(tab_id) = msg.tab else {
            tracing::debug!("DeleteFile ignored: no tab id");
            return;
        };
        let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
            tracing::debug!("DeleteFile ignored: unknown tab {}", tab_id);
            return;
        };
        let path = path.clone();
        let repo_path = repo_path.clone();
        tokio::task::spawn_blocking(move || {
            let file_path = std::path::Path::new(&repo_path).join(&path);
            if file_path.exists() {
                let _ = std::fs::remove_file(&file_path);
            }
        }).await.ok();
        crate::server::refresh_tab(&app, tab_id).await;
        return;
    }

    if let crate::git::types::GitAction::RenameFile(old_path, new_path) = &msg.action {
        let Some(tab_id) = msg.tab else {
            tracing::debug!("RenameFile ignored: no tab id");
            return;
        };
        let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
            tracing::debug!("RenameFile ignored: unknown tab {}", tab_id);
            return;
        };
        let old_path = old_path.clone();
        let new_path = new_path.clone();
        let repo_path = repo_path.clone();
        tokio::task::spawn_blocking(move || {
            let old = std::path::Path::new(&repo_path).join(&old_path);
            let new = std::path::Path::new(&repo_path).join(&new_path);
            if old.exists() {
                let _ = std::fs::rename(&old, &new);
            }
        }).await.ok();
        crate::server::refresh_tab(&app, tab_id).await;
        return;
    }

    let Some(tab_id) = msg.tab else {
        tracing::debug!("ignoring action without a tab id");
        return;
    };
    let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
        tracing::debug!("ignoring action for unknown tab {}", tab_id);
        return;
    };

    // Broadcast a `running` entry up front so every client sees the command
    // was entered even while a slow pull/push is still in flight.
    let action = msg.action;
    let placeholder = crate::git::placeholder_command(&action);
    let log_seq = app.registry.start_log_entry(tab_id, placeholder);

    // Reclone deletes and re-creates the repository directory, invalidating
    // every registered filesystem watch; the daemon must respawn it.
    let needs_watcher_reset = matches!(action, crate::git::types::GitAction::Reclone);
    let reset_path = repo_path.clone();
    // Live feedback: while the action runs, streaming snapshots revise the
    // placeholder entry's output in place so clients watch progress.
    let progress: Option<crate::git::ProgressSink> = log_seq.map(|seq| {
        let app = app.clone();
        let sink: crate::git::ProgressSink = std::sync::Arc::new(move |snapshot: String| {
            app.registry.update_log_output(tab_id, seq, snapshot);
        });
        sink
    });
    let result = tokio::task::spawn_blocking(move || {
        crate::git::execute_action_logged(&repo_path, action, progress)
    })
    .await;
    match &result {
        Ok((Ok(()), _)) => {
            if needs_watcher_reset {
                let _ = app.watcher_resets.send(reset_path);
            }
        }
        Ok((Err(e), _)) => tracing::error!("action failed: {e}"),
        Err(e) => tracing::error!("action task panicked: {e}"),
    }
    if let Some(seq) = log_seq {
        let transcript = result
            .ok()
            .map(|(_, log)| log)
            .unwrap_or_else(|| {
                vec![crate::git::types::LogEntry {
                    seq: 0,
                    command: "internal error".to_string(),
                    output: "the action task panicked before completing".to_string(),
                    status: crate::git::types::LogStatus::Failed,
                    started_ms: 0,
                    duration_ms: 0,
                }]
            });
        app.registry.finish_log_entry(tab_id, seq, transcript);
    }

    crate::server::refresh_tab(app, tab_id).await;
}

#[cfg(test)]
mod tests {
    use futures_util::stream::StreamExt;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    use super::encode_client_message;
    use crate::git::types::{GitAction, GitStatus, LogStatus};
    use crate::server::registry::TabRegistry;
    use crate::server::{run_server, AppState};
    use crate::test_support::{
        app_for, connect_with_retry, init_repo, recv_state, recv_state_until,
    };

    #[test]
    fn encoded_client_messages_round_trip() {
        let json = encode_client_message(Some(3), &GitAction::Stage("a.txt".to_string()));
        let msg: super::ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.tab, Some(3));
        assert_eq!(msg.action, GitAction::Stage("a.txt".to_string()));

        let json = encode_client_message(None, &GitAction::CloseTab);
        let msg: super::ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.tab, None);
        assert_eq!(msg.action, GitAction::CloseTab);

        let payload = r#"{"name":"My Repo","path":"/tmp/repo"}"#;
        let json = encode_client_message(None, &GitAction::NewTab(payload.to_string()));
        let msg: super::ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.tab, None);
        assert!(matches!(msg.action, GitAction::NewTab(p) if p == payload));
    }

    #[tokio::test]
    async fn websocket_streams_state_and_dispatches_actions() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;

        let initial = recv_state(&mut ws).await;
        assert_eq!(initial.tabs[0].state.current_branch, "main");
        assert_eq!(initial.tabs[0].state.changes.len(), 1);
        assert_eq!(initial.tabs[0].state.changes[0].path, "a.txt");
        assert_eq!(
            initial.tabs[0].state.changes[0].status,
            GitStatus::Untracked
        );

        ws.send(Message::Text(r#"{"tab":0,"action":{"Stage":"a.txt"}}"#.into()))
            .await
            .unwrap();

        // Log broadcasts may interleave with state updates, so wait for the
        // staged change rather than reading a single message.
        let updated = recv_state_until(&mut ws, |s| {
            s.tabs[0]
                .state
                .changes
                .iter()
                .any(|c| c.path == "a.txt" && c.is_staged)
        })
        .await;
        assert_eq!(updated.tabs[0].state.changes.len(), 1);
        assert_eq!(updated.tabs[0].state.changes[0].path, "a.txt");
        assert!(updated.tabs[0].state.changes[0].is_staged);
    }

    #[tokio::test]
    async fn websocket_dispatches_action_to_named_tab() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        init_repo(dir1.path());
        init_repo(dir2.path());
        std::fs::write(dir1.path().join("one.txt"), "one").unwrap();
        std::fs::write(dir2.path().join("two.txt"), "two").unwrap();

        let registry = TabRegistry::new();
        registry.set(crate::server::registry::WebState {
            active: 0,
            tabs: vec![
                crate::server::registry::WebTab {
                    id: 0,
                    name: "one".to_string(),
                    repo_path: dir1.path().display().to_string(),
                    state: crate::git::types::RepoState::default(),
                    log: Vec::new(),
                },
                crate::server::registry::WebTab {
                    id: 1,
                    name: "two".to_string(),
                    repo_path: dir2.path().display().to_string(),
                    state: crate::git::types::RepoState::default(),
                    log: Vec::new(),
                },
            ],
        });
        let app = AppState::new(registry);
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;

        let _ = recv_state(&mut ws).await;

        ws.send(Message::Text(r#"{"tab":1,"action":{"Stage":"two.txt"}}"#.into()))
            .await
            .unwrap();

        let updated = recv_state_until(&mut ws, |s| {
            s.tabs
                .iter()
                .any(|t| t.id == 1 && t.state.changes.iter().any(|c| c.path == "two.txt" && c.is_staged))
        })
        .await;
        let tab1 = updated.tabs.iter().find(|t| t.id == 1).unwrap();
        let tab0 = updated.tabs.iter().find(|t| t.id == 0).unwrap();
        assert!(tab1
            .state
            .changes
            .iter()
            .any(|c| c.path == "two.txt" && c.is_staged));
        assert!(!tab0
            .state
            .changes
            .iter()
            .any(|c| c.path == "two.txt"));
    }

    #[tokio::test]
    async fn websocket_broadcasts_state_to_all_clients() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws1 = connect_with_retry(&url).await;
        let mut ws2 = connect_with_retry(&url).await;

        let _ = recv_state(&mut ws1).await;
        let _ = recv_state(&mut ws2).await;

        crate::server::refresh_all(&app).await;

        let state1 = recv_state(&mut ws1).await;
        let state2 = recv_state(&mut ws2).await;
        assert_eq!(state1, state2);
        assert_eq!(state1.tabs[0].state.changes.len(), 1);
    }

    #[tokio::test]
    async fn actions_stream_command_log_entries_to_clients() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let mut ws = connect_with_retry(&format!("ws://{addr}/ws")).await;
        let _ = recv_state(&mut ws).await;

        // Commit with nothing staged must fail; the log entry carries git's
        // own stderr so the user sees exactly what a terminal would show.
        ws.send(Message::Text(r#"{"tab":0,"action":{"Commit":"empty"}}"#.into()))
            .await
            .unwrap();
        let logged = recv_state_until(&mut ws, |s| {
            s.tabs[0].log.iter().any(|e| {
                e.command.contains("git commit")
                    && e.status != crate::git::types::LogStatus::Running
            })
        })
        .await;
        let entries: Vec<&crate::git::types::LogEntry> = logged.tabs[0]
            .log
            .iter()
            .filter(|e| {
                e.command.contains("git commit")
                    && e.status != crate::git::types::LogStatus::Running
            })
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, crate::git::types::LogStatus::Failed);
        assert!(
            entries[0].output.contains("nothing added to commit"),
            "got: {:?}",
            entries[0].output
        );

        // The running placeholder must have been replaced (no dangling
        // `running` entries after the action completes).
        let state = app.registry.snapshot();
        assert!(state.tabs[0]
            .log
            .iter()
            .all(|e| e.status != crate::git::types::LogStatus::Running));
    }

    #[tokio::test]
    async fn new_tab_appends_validated_repo_tab() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        let other = tempfile::tempdir().unwrap();
        init_repo(other.path());

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;

        let _ = recv_state(&mut ws).await;

        ws.send(Message::Text(
            format!(
                r#"{{"tab":null,"action":{{"NewTab":"{{\"name\":\"\",\"path\":\"{}\"}}"}}}}"#,
                other.path().display()
            )
            .into(),
        ))
        .await
        .unwrap();

        let opened =
            recv_state_until(&mut ws, |s| s.tabs.len() == 2).await;
        assert_eq!(opened.active, 1, "newly opened tab becomes active");
        let repo_tab = &opened.tabs[1];
        assert_eq!(repo_tab.repo_path, other.path().display().to_string());
        assert_eq!(
            repo_tab.name,
            other.path().file_name().unwrap().to_string_lossy()
        );

        // The "+" form is client-local now: no empty placeholder may appear.
        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(60), ws.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let s: crate::server::registry::WebState =
                        serde_json::from_str(&text).unwrap();
                    assert!(
                        s.tabs.iter().all(|t| !t.repo_path.is_empty()),
                        "no placeholder tab may exist"
                    );
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn new_tab_without_path_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;
        let initial = recv_state(&mut ws).await;
        assert_eq!(initial.tabs.len(), 1);

        for _ in 0..3 {
            ws.send(Message::Text(
                r#"{"tab":null,"action":{"NewTab":"{\"name\":\"new\",\"path\":\"\"}"}}"#.into(),
            ))
            .await
            .unwrap();
        }

        for _ in 0..10 {
            match tokio::time::timeout(std::time::Duration::from_millis(60), ws.next())
                .await
                .ok()
                .flatten()
            {
                Some(Ok(Message::Text(text))) => {
                    let s: crate::server::registry::WebState =
                        serde_json::from_str(&text).unwrap();
                    assert_eq!(s.tabs.len(), 1, "pathless NewTab must not create tabs");
                    assert!(s.tabs.iter().all(|t| !t.repo_path.is_empty()));
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn close_tab_removes_any_tab_but_keeps_repo_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;

        let _ = recv_state(&mut ws).await;

        // Closing a real repository tab removes it from the workspace...
        ws.send(Message::Text(r#"{"tab":0,"action":"CloseTab"}"#.into()))
            .await
            .unwrap();
        let emptied = recv_state_until(&mut ws, |s| s.tabs.is_empty()).await;
        assert!(emptied.tabs.is_empty());

        // The removal must stick: no later broadcast may resurrect the tab.
        for _ in 0..10 {
            match tokio::time::timeout(
                std::time::Duration::from_millis(60),
                ws.next(),
            )
            .await
            {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let state: crate::server::registry::WebState =
                        serde_json::from_str(&text).unwrap();
                    assert!(
                        state.tabs.is_empty(),
                        "removed tab resurrected in broadcast: {text}"
                    );
                }
                Ok(_) => continue,
                Err(_) => continue, // quiet period, fine
            }
        }

        // ...while the repository on disk is left untouched.
        assert!(dir.path().join(".git").exists());
        assert!(dir.path().is_dir());
    }

    #[tokio::test]
    async fn close_tab_targets_the_addressed_id_not_the_first_tab() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        init_repo(dir_a.path());
        init_repo(dir_b.path());

        let app = app_for(dir_a.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let mut ws = connect_with_retry(&format!("ws://{addr}/ws")).await;
        let initial = recv_state(&mut ws).await;
        assert_eq!(initial.tabs.len(), 1);
        let id_a = initial.tabs[0].id;

        let inner = serde_json::json!({
            "name": "",
            "path": dir_b.path().display().to_string()
        })
        .to_string();
        let wire = serde_json::json!({ "tab": null, "action": { "NewTab": inner } })
            .to_string();
        ws.send(Message::Text(wire.into())).await.unwrap();

        let two = recv_state_until(&mut ws, |s| s.tabs.len() == 2).await;
        let id_b = two
            .tabs
            .iter()
            .find(|t| t.repo_path == dir_b.path().display().to_string())
            .map(|t| t.id)
            .expect("second tab must exist");

        ws.send(Message::Text(
            encode_client_message(Some(id_b), &GitAction::CloseTab).into(),
        ))
        .await
        .unwrap();
        let after = recv_state_until(&mut ws, |s| s.tabs.len() == 1).await;
        assert_eq!(
            after.tabs[0].id, id_a,
            "the closed tab must be the addressed one, not the first"
        );
    }

    #[tokio::test]
    async fn run_script_action_launches_script_and_surfaces_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::create_dir(dir.path().join("scripts")).unwrap();
        let script = dir.path().join("scripts/marker.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch {}/marker\nsleep 30\n", dir.path().display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let mut ws = connect_with_retry(&format!("ws://{addr}/ws")).await;
        let initial = recv_state(&mut ws).await;
        assert!(
            initial.tabs[0]
                .state
                .scripts
                .iter()
                .any(|s| s.rel_path == "scripts/marker.sh"),
            "discovered scripts must ride RepoState: {:?}",
            initial.tabs[0].state.scripts
        );

        ws.send(Message::Text(
            r#"{"tab":0,"action":{"RunScript":"scripts/marker.sh"}}"#.into(),
        ))
        .await
        .unwrap();

        for _ in 0..100 {
            if dir.path().join("marker").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            dir.path().join("marker").exists(),
            "RunScript over the daemon wire must launch the script"
        );
    }

    #[tokio::test]
    async fn run_script_rejects_escape_paths_without_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let outside = tempfile::tempdir().unwrap();
        let outside_script = outside.path().join("evil.sh");
        std::fs::write(
            &outside_script,
            format!("#!/bin/sh\ntouch {}/pwned\n", dir.path().display()),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&outside_script, std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let mut ws = connect_with_retry(&format!("ws://{addr}/ws")).await;
        let _ = recv_state(&mut ws).await;

        ws.send(Message::Text(r#"{"tab":0,"action":{"RunScript":"../evil.sh"}}"#.into()))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            !dir.path().join("pwned").exists(),
            "escape paths must never execute"
        );
    }

    #[tokio::test]
    async fn webstate_contains_tab_id_and_repo_path() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let app = app_for(dir.path());
        let snapshot = app.registry.snapshot();
        assert_eq!(snapshot.tabs[0].id, 0);
        assert_eq!(
            snapshot.tabs[0].repo_path,
            dir.path().display().to_string()
        );
    }

    #[tokio::test]
    async fn open_repo_tab_validates_and_appends() {
        let registry = TabRegistry::new();

        assert_eq!(
            super::open_repo_tab(&registry, String::new(), String::new())
                .await
                .unwrap_err(),
            "Folder path is required"
        );
        assert!(registry.snapshot().tabs.is_empty());

        let plain = tempfile::tempdir().unwrap();
        assert!(
            super::open_repo_tab(&registry, String::new(), plain.path().display().to_string())
                .await
                .unwrap_err()
                .contains("Not a git repository")
        );

        let repo = tempfile::tempdir().unwrap();
        init_repo(repo.path());
        let id = super::open_repo_tab(
            &registry,
            "my project".to_string(),
            repo.path().display().to_string(),
        )
        .await
        .unwrap();
        let state = registry.snapshot();
        assert_eq!(state.tabs.len(), 1);
        assert_eq!(state.active, 0);
        assert_eq!(state.tabs[0].name, "my project");
        assert_eq!(state.tabs[0].repo_path, repo.path().display().to_string());

        // Name is derived from the folder when empty or "new".
        let second = super::open_repo_tab(&registry, String::new(), repo.path().display().to_string())
            .await
            .unwrap();
        let state = registry.snapshot();
        assert_ne!(id, second, "ids come from one monotonic allocator");
        assert_eq!(state.tabs.len(), 2);
        assert_eq!(
            state.tabs[1].name,
            repo.path().file_name().unwrap().to_string_lossy()
        );
    }

    #[tokio::test]
    async fn close_tab_by_id_reports_unknown_ids() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let registry = TabRegistry::with_single_tab(7, "repo".to_string(), dir.path().to_path_buf());

        assert!(!super::close_tab_by_id(&registry, 99));
        assert_eq!(registry.snapshot().tabs.len(), 1);

        assert!(super::close_tab_by_id(&registry, 7));
        assert!(registry.snapshot().tabs.is_empty());
        assert!(dir.path().join(".git").exists(), "disk untouched");
    }

    /// A killed daemon must be immediately restartable on the same port and
    /// serve fresh WebSocket clients — the browser-side reconnect path.
    #[tokio::test]
    async fn restarted_daemon_rebinds_port_and_accepts_new_clients() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = crate::server::create_listener(0).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut first = connect_with_retry(&url).await;
        // Wait until the daemon has served real state before killing it.
        let _ = recv_state_until(&mut first, |s| {
            !s.tabs.is_empty() && s.tabs[0].state.current_branch == "main"
        })
        .await;
        drop(first);

        // Kill the daemon: connected clients see their socket drop.
        server.abort();
        let _ = server.await;

        // Instant restart on the SAME port: SO_REUSEADDR must win any
        // TIME_WAIT race left behind by the old connections. Under heavy
        // parallel-test load the old listener can take a moment to be
        // released, so retry briefly instead of failing the restart.
        let listener = {
            let mut rebound = None;
            for _ in 0..100 {
                match crate::server::create_listener(addr.port()).await {
                    Ok(l) => {
                        rebound = Some(l);
                        break;
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                }
            }
            rebound.expect("old daemon never released the port")
        };
        assert_eq!(listener.local_addr().unwrap(), addr);
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app, refresh_rx);

        // The browser's retry loop lands here and resumes streaming.
        let mut second = connect_with_retry(&url).await;
        let after = recv_state(&mut second).await;
        assert_eq!(after.tabs[0].state.current_branch, "main");
    }

    /// Regression: the issuing connection must see its own action's
    /// `running` log entry while the command still executes. The handler
    /// used to await dispatch inline in the per-connection select loop, so
    /// every streaming frame queued up behind the git command and only
    /// reached this very client when it exited.
    #[tokio::test]
    async fn running_entry_reaches_issuing_client_mid_action() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

        // A pre-commit hook makes CommitAll genuinely slow through the
        // normal dispatch path, without needing a network remote.
        let hooks = dir.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nsleep 3\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let app = app_for(dir.path());
        crate::server::refresh_all(&app).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = run_server(listener, app.clone(), refresh_rx);

        let url = format!("ws://{addr}/ws");
        let mut ws = connect_with_retry(&url).await;
        let _ = recv_state(&mut ws).await;

        ws.send(Message::Text(r#"{"tab":0,"action":{"CommitAll":"slow"}}"#.into()))
            .await
            .unwrap();

        // Well inside the hook sleep the placeholder must already be here.
        // The ceiling tolerates scheduler jitter under suite-wide load; a
        // longer wait would also pass on the broken code once the action
        // finished, which is why it must stay below the sleep.
        tokio::time::timeout(std::time::Duration::from_millis(1500), async {
            recv_state_until(&mut ws, |s| {
                s.tabs[0]
                    .log
                    .iter()
                    .any(|e| e.status == LogStatus::Running && e.output.is_empty())
            })
            .await
        })
        .await
        .expect("running entry never reached the issuing client mid-action");

        // The command then completes normally and seals the transcript.
        let finished = recv_state_until(&mut ws, |s| {
            s.tabs[0]
                .log
                .iter()
                .any(|e| e.status == LogStatus::Success && e.command.contains("commit"))
        })
        .await;
        assert!(finished.tabs[0]
            .log
            .iter()
            .all(|e| e.status != LogStatus::Running));
    }
}