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

    let listener = TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind server on {}: {}", addr, e));
    println!("Server is listening on {}", addr);

    let grant_tx_clone = grant_tx.clone();
    let revoke_tx_clone = revoke_tx.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    println!("New connection from {}", peer);

                    let mut grant_rx = grant_tx_clone.subscribe();
                    let mut revoke_rx = revoke_tx_clone.subscribe();
                    let ws = match accept_async(stream).await {
                        Ok(ws) => ws,
                        Err(e) => {
                            eprintln!("Failed to accept WebSocket: {}", e);
                            continue;
                        }
                    };

                    tokio::spawn(async move {
                        let mut ws = ws;
                        loop {
                            tokio::select! {
                                Ok(grant) = grant_rx.recv() => {
                                    let mut msg = serde_json::to_value(&grant).unwrap();
                                    if let Value::Object(ref mut map) = msg {
                                        map.insert("type".into(), Value::String("Grant".into()));
                                    }
                                    let text = msg.to_string();
                                    println!("Broadcasting grant: {}", text);
                                    let _ = ws.send(Message::Text(text.into())).await;
                                }
                                Ok(revoke) = revoke_rx.recv() => {
                                    let mut msg = serde_json::to_value(&revoke).unwrap();
                                    if let Value::Object(ref mut map) = msg {
                                        map.insert("type".into(), Value::String("Revoke".into()));
                                    }
                                    let text = msg.to_string();
                                    println!("Broadcasting revoke: {}", text);
                                    let _ = ws.send(Message::Text(text.into())).await;
                                }
                                msg = ws.next() => {
                                    if msg.is_none() {
                                        println!("Client disconnected");
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    eprintln!("Error accepting connection: {}", e);
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
    println!("Connecting to {}", url);

    let (mut ws, _) = match connect_async(&url).await {
        Ok(pair) => {
            println!("Connected to server at {}", url);
            pair
        }
        Err(e) => {
            eprintln!("Failed to connect to {}: {}", url, e);
            return;
        }
    };

    while let Some(Ok(Message::Text(text))) = ws.next().await {
        println!("Received: {}", text);

        let v: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Invalid JSON received: {}", e);
                continue;
            }
        };

        match v.get("type").and_then(|t| t.as_str()) {
            Some("Grant") => {
                let grant: GrantMessage = match serde_json::from_value(v.clone()) {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("Failed to parse grant message: {}", e);
                        continue;
                    }
                };
                let cookies = grant.cookies.clone();
                let url = grant.url.clone();
                let tab_id = grant.tab_id.clone();
                let map = Arc::clone(&remote_to_local);

                tokio::task::spawn_blocking(move || {
                    println!("Importing URL with cookies: {}", url);
                    if let Ok(local_id) =
                        crate::chrome::import_and_open_with_cookies_from_memory(&cookies, &url)
                    {
                        let mut guard = map.lock().unwrap();
                        guard.insert(tab_id.clone(), local_id);
                    }
                });
            }
            Some("Revoke") => {
                let revoke: RevokeMessage = match serde_json::from_value(v.clone()) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("Failed to parse revoke message: {}", e);
                        continue;
                    }
                };
                let tab_id = revoke.tab_id.clone();
                let local_id = {
                    let guard = remote_to_local.lock().unwrap();
                    guard.get(&tab_id).cloned().unwrap_or(tab_id.clone())
                };
                let cookies = revoke.cookies.clone();

                tokio::task::spawn_blocking(move || {
                    println!("Revoking cookies for tab {}", local_id);
                    let cookie_tuples: Vec<(&str, &str, &str)> = cookies
                        .iter()
                        .map(|c| (c.name.as_str(), c.domain.as_str(), c.path.as_str()))
                        .collect();

                    if let Err(e) = crate::chrome::revoke_cookies(&local_id, &cookie_tuples) {
                        eprintln!("Error revoking cookies: {}", e);
                    } else {
                        println!("Cookies revoked successfully");
                    }
                });
            }
            _ => {
                eprintln!("Unknown message type: {:?}", v.get("type"));
            }
        }
    }

    println!("WebSocket listener loop has ended");
}
