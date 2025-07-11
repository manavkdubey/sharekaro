// src/network.rs

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
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

pub async fn connect_client(
    addr: SocketAddr,
    remote_to_local: Arc<Mutex<HashMap<String, String>>>,
) {
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

    while let Some(Ok(Message::Text(txt))) = ws.next().await {
        println!("📥 Client received: {}", txt);
        let v: serde_json::Value = match serde_json::from_str(&txt) {
            Ok(v) => v,
            Err(e) => {
                println!("⚠️ JSON parse error: {}", e);
                continue;
            }
        };

        if let Some("Grant") = v.get("type").and_then(|t| t.as_str()) {
            let grant: GrantMessage = match serde_json::from_value(v.clone()) {
                Ok(g) => g,
                Err(e) => {
                    println!("⚠️ Grant parse error: {}", e);
                    continue;
                }
            };

            let remote_id = grant.tab_id.clone();
            let cookies = grant.cookies.clone();
            let url = grant.url.clone();
            let map = remote_to_local.clone();
            tokio::task::spawn_blocking(move || {
                println!("🔄 spawn_blocking: import_and_open_with_cookies_from_memory");
                if let Ok(local_id) =
                    crate::chrome::import_and_open_with_cookies_from_memory(&cookies, &url)
                {
                    let mut guard = map.lock().unwrap();
                    guard.insert(remote_id.clone(), local_id.clone());
                }
            });
        }

        if let Some("Revoke") = v.get("type").and_then(|t| t.as_str()) {
            // parse the incoming message
            let revoke: RevokeMessage = match serde_json::from_value(v.clone()) {
                Ok(r) => r,
                Err(e) => {
                    println!("⚠️ Revoke parse error: {}", e);
                    continue;
                }
            };
            let remote_id = revoke.tab_id.clone();
            let local_id = remote_to_local
                .lock()
                .unwrap()
                .get(&remote_id)
                .cloned()
                .unwrap_or(remote_id.clone());

            let cookies = revoke.cookies.clone();

            tokio::task::spawn_blocking(move || {
                println!("🔄 spawn_blocking: revoke_cookies");

                let tuples: Vec<(&str, &str, &str)> = cookies
                    .iter()
                    .map(|c| (c.name.as_str(), c.domain.as_str(), c.path.as_str()))
                    .collect();

                match crate::chrome::revoke_cookies(&local_id, &tuples) {
                    Ok(_) => println!("✅ revoke_cookies succeeded"),
                    Err(e) => println!("❌ revoke error: {}", e),
                }
            });
        }
    }

    println!("⚠️ Client websocket loop ended");
}
