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
    if path.is_empty() {
        return Err("Folder path is required".to_string());
    }
    let repo_path = crate::server::expand_tilde(&path);
    if !repo_path.is_dir() {
        return Err(format!("Not a directory: {path}"));
    }
    if !repo_path.join(".git").exists() {
        return Err(format!("Not a git repository: {path}"));
    }
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
    });
    new_state.active = new_state.tabs.len() - 1;
    registry.set(new_state);
    Ok(new_id)
}

/// Removes one tab from the workspace by id. The repository itself on disk
/// is left untouched. Returns false when the id is unknown.
pub fn close_tab_by_id(registry: &crate::server::registry::TabRegistry, id: usize) -> bool {
    let mut state = registry.snapshot();
    if let Some(idx) = state.tabs.iter().position(|t| t.id == id) {
        let removed = state.tabs.remove(idx);
        tracing::info!(
            "CloseTab removed id={} name={} path={}; {} tab(s) remain",
            removed.id,
            removed.name,
            removed.repo_path,
            state.tabs.len()
        );
        if state.active >= state.tabs.len() {
            state.active = state.tabs.len().saturating_sub(1);
        }
        registry.set(state);
        true
    } else {
        tracing::warn!("CloseTab ignored: unknown tab id {}", id);
        false
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

    let Some(tab_id) = msg.tab else {
        tracing::debug!("ignoring action without a tab id");
        return;
    };
    let Some(repo_path) = app.registry.repo_path_for(tab_id) else {
        tracing::debug!("ignoring action for unknown tab {}", tab_id);
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
    crate::server::refresh_tab(app, tab_id).await;
}

#[cfg(test)]
mod tests {
    use futures_util::stream::StreamExt;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    use super::encode_client_message;
    use crate::git::types::{GitAction, GitStatus};
    use crate::server::registry::TabRegistry;
    use crate::server::{run_server, AppState};

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
}