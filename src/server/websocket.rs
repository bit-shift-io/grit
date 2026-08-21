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
    tab: usize,
    action: GitAction,
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

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(msg) => dispatch_and_refresh(&app, msg).await,
                            Err(e) => {
                                tracing::debug!("ignoring malformed action: {e}");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                }
            }
            update = broadcast_rx.recv() => {
                match update {
                    Ok(state) => {
                        if let Ok(text) = serde_json::to_string(&state) {
if sender.send(Message::Text(text.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn dispatch_and_refresh(app: &AppState, msg: ClientMessage) {
    if matches!(msg.action, crate::git::types::GitAction::CloseTab) {
        let mut state = app.registry.snapshot();
        if let Some(idx) = state.tabs.iter().position(|t| t.id == msg.tab) {
            if state.tabs[idx].repo_path.is_empty() {
                state.tabs.remove(idx);
                if state.active >= state.tabs.len() {
                    state.active = state.tabs.len().saturating_sub(1);
                }
                app.registry.set(state);
            }
        }
        return;
    }

    if let crate::git::types::GitAction::NewTab(payload) = &msg.action {
        let (name, path) = if payload.starts_with('{') {
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
            (payload.clone(), String::new())
        };

        let mut new_state = app.registry.snapshot();

        // Opening a real repository replaces the placeholder form tab it was
        // submitted from; otherwise we are adding a fresh placeholder tab.
        let placeholder_idx = new_state
            .tabs
            .iter()
            .position(|t| t.id == msg.tab && t.repo_path.is_empty());

        if let Some(idx) = placeholder_idx {
            if !path.is_empty() {
                let expanded = crate::server::expand_tilde(&path);
                let repo_path = expanded;
                if !repo_path.is_dir() {
                    tracing::debug!("ignoring NewTab with missing directory: {path}");
                    return;
                }
                let tab_name = if name.is_empty() || name == "new" {
                    repo_path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "repo".to_string())
                } else {
                    name
                };
                let new_id = new_state.tabs[idx].id;
                new_state.tabs[idx] = crate::server::registry::WebTab {
                    id: new_id,
                    name: tab_name,
                    repo_path: repo_path.display().to_string(),
                    state: crate::git::types::RepoState::default(),
                };
                new_state.active = idx;
                app.registry.set(new_state);
                crate::server::refresh_tab(app, new_id).await;
                return;
            }
        }

        let new_id = new_state.tabs.iter().map(|t| t.id).max().map_or(0, |m| m + 1);
        new_state.tabs.push(crate::server::registry::WebTab {
            id: new_id,
            name,
            repo_path: String::new(),
            state: crate::git::types::RepoState::default(),
        });
        new_state.active = new_state.tabs.len() - 1;
        app.registry.set(new_state);
        return;
    }

    let Some(repo_path) = app.registry.repo_path_for(msg.tab) else {
        tracing::debug!("ignoring action for unknown tab {}", msg.tab);
        return;
    };
    let result = tokio::task::spawn_blocking(move || {
        crate::git::execute_action(&repo_path, msg.action)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!("action failed: {e}"),
        Err(e) => tracing::error!("action task panicked: {e}"),
    }
    crate::server::refresh_tab(app, msg.tab).await;
}

#[cfg(test)]
mod tests {
    use futures_util::stream::StreamExt;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    use crate::git::types::{GitStatus};
    use crate::server::registry::TabRegistry;
    use crate::server::{run_server, AppState};

    fn init_repo(dir: &std::path::Path) {
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir)
            .output()
            .unwrap();
    }

    fn app_for(dir: &std::path::Path) -> AppState {
        AppState::new(TabRegistry::with_single_tab(
            0,
            "repo".to_string(),
            dir.to_path_buf(),
        ))
    }

    async fn connect_with_retry(url: &str) -> tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    > {
        for _ in 0..40 {
            if let Ok((ws, _)) = tokio_tungstenite::connect_async(url).await {
                return ws;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("failed to connect to {url}");
    }

    async fn recv_state(ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >) -> crate::server::registry::WebState {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for ws message"))
            .expect("stream closed")
            .expect("ws error");
        match msg {
            Message::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("expected text state message, got {other:?}"),
        }
    }

    async fn recv_state_until(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        mut pred: impl FnMut(&crate::server::registry::WebState) -> bool,
    ) -> crate::server::registry::WebState {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let state = recv_state(ws).await;
                if pred(&state) {
                    return state;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for matching state"))
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

        let updated = recv_state(&mut ws).await;
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
                },
                crate::server::registry::WebTab {
                    id: 1,
                    name: "two".to_string(),
                    repo_path: dir2.path().display().to_string(),
                    state: crate::git::types::RepoState::default(),
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

        let updated = recv_state(&mut ws).await;
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
    async fn new_tab_creates_placeholder_then_opens_repo() {
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

        let _ = recv_state(&mut ws).await;

        ws.send(Message::Text(
            r#"{"tab":0,"action":{"NewTab":"{\"name\":\"new\",\"path\":\"\"}"}}"#.into(),
        ))
        .await
        .unwrap();

        let with_placeholder =
            recv_state_until(&mut ws, |s| s.tabs.iter().any(|t| t.repo_path.is_empty())).await;
        assert_eq!(with_placeholder.tabs.len(), 2);
        assert_eq!(with_placeholder.active, 1);
        let placeholder = &with_placeholder.tabs[1];
        assert_eq!(placeholder.name, "new");
        assert_eq!(placeholder.repo_path, "");

        let payload = format!(
            r#"{{"tab":{},"action":{{"NewTab":"{{\"name\":\"\",\"path\":\"{}\"}}"}}}}"#,
            placeholder.id,
            dir.path().display()
        );
        ws.send(Message::Text(payload.into())).await.unwrap();

        let opened = recv_state_until(&mut ws, |s| {
            s.tabs
                .iter()
                .any(|t| !t.repo_path.is_empty() && t.id == placeholder.id)
        })
        .await;
        assert_eq!(opened.tabs.len(), 2);
        let repo_tab = opened
            .tabs
            .iter()
            .find(|t| t.id == placeholder.id)
            .unwrap();
        assert_eq!(repo_tab.repo_path, dir.path().display().to_string());
        assert!(!repo_tab.name.is_empty());
    }

    #[tokio::test]
    async fn close_tab_removes_placeholder_only() {
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

        ws.send(Message::Text(
            r#"{"tab":0,"action":{"NewTab":"{\"name\":\"new\",\"path\":\"\"}"}}"#.into(),
        ))
        .await
        .unwrap();

        let with_placeholder =
            recv_state_until(&mut ws, |s| s.tabs.iter().any(|t| t.repo_path.is_empty())).await;
        let placeholder_id = with_placeholder.tabs[1].id;

        ws.send(Message::Text(format!(r#"{{"tab":{placeholder_id},"action":"CloseTab"}}"#).into()))
            .await
            .unwrap();

        let closed = recv_state_until(&mut ws, |s| {
            s.tabs.len() == 1 && s.tabs.iter().all(|t| !t.repo_path.is_empty())
        })
        .await;
        assert_eq!(closed.tabs.len(), 1);

        ws.send(Message::Text(r#"{"tab":0,"action":"CloseTab"}"#.into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let state = app.registry.snapshot();
        assert_eq!(state.tabs.len(), 1, "real tabs must not be closable via web");
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
}