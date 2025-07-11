// src/network.rs

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

    let listener = match TcpListener::bind(addr).await {
        Ok(l) => {
            println!("✅ Server bound to {}", addr);
            l
        }
        Err(e) => panic!("⚠️ Failed to bind server to {}: {}", addr, e),
    };

    tokio::spawn({
        let grant_tx = grant_tx.clone();
        let revoke_tx = revoke_tx.clone();
        async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        println!("🔗 Accepted connection from {}", peer);
                        let mut grant_rx = grant_tx.subscribe();
                        let mut revoke_rx = revoke_tx.subscribe();
                        let mut ws = match accept_async(stream).await {
                            Ok(ws) => ws,
                            Err(e) => {
                                println!("⚠️ WebSocket accept error: {}", e);
                                continue;
                            }
                        };

                        tokio::spawn(async move {
                            loop {
                                tokio::select! {
                                    Ok(grant) = grant_rx.recv() => {
                                        let mut msg = serde_json::to_value(&grant).unwrap();
                                        if let Value::Object(ref mut map) = msg {
                                            map.insert("type".into(), Value::String("Grant".into()));
                                        }
                                        let txt = msg.to_string();
                                        println!("📤 Sending Grant: {}", txt);
                                        let _ = ws.send(Message::Text(txt.into())).await;
                                    }
                                    Ok(revoke) = revoke_rx.recv() => {
                                        let mut msg = serde_json::to_value(&revoke).unwrap();
                                        if let Value::Object(ref mut map) = msg {
                                            map.insert("type".into(), Value::String("Revoke".into()));
                                        }
                                        let txt = msg.to_string();
                                        println!("📤 Sending Revoke: {}", txt);
                                        let _ = ws.send(Message::Text(txt.into())).await;
                                    }
                                    msg = ws.next() => {
                                        if msg.is_none() {
                                            println!("⚠️ WebSocket closed by client");
                                            break;
                                        }
                                    }
                                }
                            }
                        });
                    }
                    Err(e) => {
                        println!("⚠️ Accept error: {}", e);
                    }
                }
            }
        }
    });

    (grant_tx, revoke_tx)
}

pub async fn connect_client(addr: SocketAddr) {
    let url = format!("ws://{}", addr);
    println!("▶️ Client dialing {}", url);
    let (mut ws, _) = match connect_async(&url).await {
        Ok(pair) => {
            println!("✅ Client connected to {}", url);
            pair
        }
        Err(e) => {
            println!("❌ Client connect error: {}", e);
            return;
        }
    };

    while let Some(msg) = ws.next().await {
        match msg {
            Ok(Message::Text(txt)) => {
                println!("📥 Client received: {}", txt);
                let v: Value = match serde_json::from_str(&txt) {
                    Ok(v) => v,
                    Err(e) => {
                        println!("⚠️ JSON parse error: {}", e);
                        continue;
                    }
                };
                match v.get("type").and_then(|t| t.as_str()) {
                    Some("Grant") => {
                        if let Ok(grant) = serde_json::from_value::<GrantMessage>(v.clone()) {
                            println!("🔄 Handling Grant for URL {}", grant.url);
                            if let Err(e) = crate::chrome::import_and_open_with_cookies_from_memory(
                                &grant.cookies,
                                &grant.url,
                            ) {
                                println!("⚠️ import error: {}", e);
                            }
                        }
                    }
                    Some("Revoke") => {
                        if let Ok(revoke) = serde_json::from_value::<RevokeMessage>(v.clone()) {
                            println!("🔄 Handling Revoke for tab {}", revoke.tab_id);
                            let tuples: Vec<(&str, &str, &str)> = revoke
                                .cookies
                                .iter()
                                .map(|c| (c.name.as_str(), c.domain.as_str(), c.path.as_str()))
                                .collect();
                            if let Err(e) = crate::chrome::revoke_cookies(&revoke.tab_id, &tuples) {
                                println!("⚠️ revoke error: {}", e);
                            }
                        }
                    }
                    other => {
                        println!("⚠️ Unknown message type: {:?}", other);
                    }
                }
            }
            Ok(other) => {
                println!("⚠️ Non-text WebSocket message: {:?}", other);
            }
            Err(e) => {
                println!("❌ WebSocket error: {}", e);
                break;
            }
        }
    }

    println!("⚠️ Client websocket loop ended");
}
