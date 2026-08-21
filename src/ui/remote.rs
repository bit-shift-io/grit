//! WebSocket client that attaches the desktop GUI to a running daemon.
//!
//! In remote mode the GUI owns no registry: this module streams the
//! daemon's broadcasts into the app as `WebTabsSync` deliveries and sends
//! user operations back over the same protocol the browser uses.

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::UnboundedSender;
use tokio_tungstenite::tungstenite::Message;

use crate::server::registry::WebTab;

/// Connects to the daemon's `/ws` endpoint and forwards every broadcast's
/// tab list through `tx`. Reconnects with a short backoff if the daemon
/// restarts or the connection drops; runs until the process exits.
pub async fn run_client(port: u16, tx: UnboundedSender<Vec<WebTab>>) {
    let url = format!("ws://127.0.0.1:{port}/ws");
    loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => {
                tracing::debug!("connected to daemon on port {port}");
                let (_, mut read) = ws.split();
                while let Some(item) = read.next().await {
                    let Ok(Message::Text(text)) = item else {
                        continue;
                    };
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                        continue;
                    };
                    if let Some(tabs) = value
                        .get("tabs")
                        .and_then(|t| serde_json::from_value::<Vec<WebTab>>(t.clone()).ok())
                    {
                        if tx.send(tabs).is_err() {
                            return; // GUI gone; nothing left to feed.
                        }
                    }
                }
                tracing::debug!("daemon connection closed; reconnecting");
            }
            Err(e) => tracing::debug!("daemon connect failed on port {port}: {e}"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
}

/// One-shot operation send: connect, transmit `msg_json`, close.
pub async fn send_op(port: u16, msg_json: String) {
    let url = format!("ws://127.0.0.1:{port}/ws");
    match tokio_tungstenite::connect_async(&url).await {
        Ok((mut ws, _)) => {
            let _ = ws.send(Message::Text(msg_json.into())).await;
            let _ = ws.close(None).await;
        }
        Err(e) => tracing::warn!("could not reach daemon on port {port}: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::types::GitAction;
    use crate::server::registry::{TabRegistry, WebTab};
    use std::time::Duration;

    #[tokio::test]
    async fn client_streams_state_and_send_op_closes_tab() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let registry =
            TabRegistry::with_single_tab(0, "t".to_string(), dir.path().to_path_buf());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (_refresh_tx, refresh_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _server = crate::server::run_server(
            listener,
            crate::server::AppState::new(registry),
            refresh_rx,
        );

        let (ctx, mut crx) = tokio::sync::mpsc::unbounded_channel::<Vec<WebTab>>();
        tokio::spawn(run_client(port, ctx));

        let first = tokio::time::timeout(Duration::from_secs(5), crx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.len(), 1, "daemon's single tab must arrive");

        send_op(
            port,
            crate::server::websocket::encode_client_message(Some(0), &GitAction::CloseTab),
        )
        .await;

        let mut saw_empty = false;
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(300), crx.recv()).await {
                Ok(Some(tabs)) if tabs.is_empty() => {
                    saw_empty = true;
                    break;
                }
                Ok(Some(_)) => continue,
                _ => break,
            }
        }
        assert!(saw_empty, "CloseTab via send_op must broadcast an empty tab list");
    }
}
