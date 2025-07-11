use futures::{AsyncRead, AsyncWrite};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

/// What you broadcast to clients when sharing cookies
#[derive(Serialize, Deserialize, Clone)]
pub struct GrantMessage {
    pub tab_id: String,
    pub url: String,
    pub cookies: Vec<crate::chrome::Cookie>,
}

/// For revocation you only need name/domain/path
#[derive(Serialize, Deserialize, Clone)]
pub struct RevokeMessage {
    pub tab_id: String,
    pub cookies: Vec<RevokeCookie>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RevokeCookie {
    pub name: String,
    pub domain: String,
    pub path: String,
}

/// Spawn the broadcast server
pub async fn spawn_server(
    addr: SocketAddr,
) -> (
    broadcast::Sender<GrantMessage>,
    broadcast::Sender<RevokeMessage>,
) {
    let (grant_tx, _) = broadcast::channel(16);
    let (revoke_tx, _) = broadcast::channel(16);
    let listener = TcpListener::bind(addr).await.unwrap();

    tokio::spawn({
        let grant_tx = grant_tx.clone();
        let revoke_tx = revoke_tx.clone();
        async move {
            while let Ok((stream, _)) = listener.accept().await {
                let mut grant_rx = grant_tx.subscribe();
                let mut revoke_rx = revoke_tx.subscribe();
                let mut ws = accept_async(stream).await.unwrap();

                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            Ok(grant) = grant_rx.recv() => {
                                let txt =
                                    serde_json::to_string(&grant).unwrap();
                                let _ = ws.send(Message::Text(txt.into())).await;
                            }
                            Ok(revoke) = revoke_rx.recv() => {
                                let txt =
                                    serde_json::to_string(&revoke).unwrap();
                                let _ = ws.send(Message::Text(txt.into())).await;
                            }
                            msg = ws.next() => {
                                // handle client→server if needed
                                if msg.is_none() { break; }
                            }
                        }
                    }
                });
            }
        }
    });

    (grant_tx, revoke_tx)
}

pub async fn connect_client(addr: SocketAddr) {
    let url = format!("ws://{}", addr);
    let (mut ws, _) = connect_async(url).await.unwrap();
    while let Some(Ok(Message::Text(txt))) = ws.next().await {
        let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
        match v.get("type").and_then(|t| t.as_str()) {
            Some("Grant") => {
                let grant: GrantMessage = serde_json::from_value(v).unwrap();
                crate::chrome::import_and_open_with_cookies_from_memory(&grant.cookies, &grant.url)
                    .unwrap();
            }
            Some("Revoke") => {
                let revoke: RevokeMessage = serde_json::from_value(v.clone()).unwrap();

                // Build the slice of (name, domain, path) tuples
                let tuples: Vec<(&str, &str, &str)> = revoke
                    .cookies
                    .iter()
                    .map(|c| (c.name.as_str(), c.domain.as_str(), c.path.as_str()))
                    .collect();

                // Now call revoke_cookies with tab_id & tuple slice
                crate::chrome::revoke_cookies(&revoke.tab_id, &tuples).unwrap();
            }
            _ => {}
        }
    }
}
