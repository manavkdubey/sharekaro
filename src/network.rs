use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use tokio::{net::TcpListener, sync::broadcast};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message};

#[derive(Serialize, Deserialize, Clone)]
pub struct GrantMessage {
    pub tab_id: String,
    pub url: String,
    pub cookies: Vec<crate::chrome::Cookie>,
}

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
                                let mut msg = serde_json::to_value(&grant).unwrap();
                                if let Value::Object(ref mut map) = msg {
                                    map.insert("type".into(), Value::String("Grant".into()));
                                }
                                let txt = msg.to_string();
                                let _ = ws.send(Message::Text(txt.into())).await;
                            }
                            Ok(revoke) = revoke_rx.recv() => {
                                let mut msg = serde_json::to_value(&revoke).unwrap();
                                if let Value::Object(ref mut map) = msg {
                                    map.insert("type".into(), Value::String("Revoke".into()));
                                }
                                let txt = msg.to_string();
                                let _ = ws.send(Message::Text(txt.into())).await;
                            }
                            msg = ws.next() => {
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
                let tuples: Vec<(&str, &str, &str)> = revoke
                    .cookies
                    .iter()
                    .map(|c| (c.name.as_str(), c.domain.as_str(), c.path.as_str()))
                    .collect();
                crate::chrome::revoke_cookies(&revoke.tab_id, &tuples).unwrap();
            }
            _ => {}
        }
    }
}
