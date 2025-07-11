use reqwest;
use reqwest::blocking::Response as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::error::Error;
use std::fs;
use std::process::{Child, Command};
use tempfile::TempDir;
use tungstenite::{Message, connect};
use url::Url;

pub fn launch_chrome_with_cdp(use_real_profile: Option<String>) -> (Child, TempDir) {
    let temp_profile = tempfile::TempDir::new().unwrap();
    let chrome_path = chrome_path();
    let profile_path = if use_real_profile
        .map(|a| a.to_lowercase().eq("default"))
        .unwrap_or(false)
    {
        #[cfg(target_os = "macos")]
        let path = dirs::home_dir()
            .unwrap()
            .join("Library/Application Support/Google/Chrome/Default");
        #[cfg(target_os = "windows")]
        let path = dirs::home_dir()
            .unwrap()
            .join("AppData/Local/Google/Chrome/User Data/Default");
        #[cfg(target_os = "linux")]
        let path = dirs::home_dir()
            .unwrap()
            .join(".config/google-chrome/Default");
        path
    } else {
        temp_profile.path().to_path_buf()
    };
    let child = Command::new(chrome_path)
        .arg(format!("--remote-debugging-port=9222"))
        .arg(format!("--user-data-dir={}", profile_path.display()))
        .spawn()
        .expect("Failed to launch Chrome");
    (child, temp_profile)
}

fn chrome_path() -> String {
    #[cfg(target_os = "macos")]
    {
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        r"C:\Program Files\Google\Chrome\Application\chrome.exe".to_string()
    }
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let candidates = [
            "google-chrome-stable",
            "google-chrome",
            "chromium-browser",
            "chromium",
        ];
        for candidate in &candidates {
            if Command::new("which")
                .arg(candidate)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return candidate.to_string();
            }
        }
        "google-chrome".to_string()
    }
}

pub fn listen_tabs_ws() -> Result<(), Box<dyn std::error::Error>> {
    let version_info: Value =
        reqwest::blocking::get("http://localhost:9222/json/version")?.json()?;
    let ws_url = version_info["webSocketDebuggerUrl"].as_str().unwrap();
    let (mut socket, _response) = connect(ws_url)?;
    let enable_msg = json!({
        "id": 1,
        "method": "Target.setDiscoverTargets",
        "params": { "discover": true }
    });
    socket.write_message(Message::Text(enable_msg.to_string().into()))?;
    print_tabs_once();
    println!("Listening for tab events (press Ctrl+C to quit)...");
    loop {
        let msg = socket.read_message()?;
        if msg.is_text() {
            let text = msg.to_text()?;
            if let Ok(event) = serde_json::from_str::<Value>(text) {
                if let Some(method) = event.get("method") {
                    if method == "Target.targetCreated"
                        || method == "Target.targetDestroyed"
                        || method == "Target.targetInfoChanged"
                    {
                        print_tabs_once();
                    }
                }
            }
        }
    }
}

pub fn print_tabs_once() {
    let tabs: Vec<Value> = reqwest::blocking::get("http://localhost:9222/json")
        .and_then(|resp| resp.json())
        .unwrap_or_default();
    println!("\x1b[2J\x1b[1;1H");
    println!("Current Chrome tabs:");
    for (i, tab) in tabs.iter().enumerate() {
        let title = tab["title"].as_str().unwrap_or("");
        let url = tab["url"].as_str().unwrap_or("");
        println!("[{}] \"{}\"\n    {}", i, title, url);
    }
    println!("--- (event-driven; updates instantly) ---");
}

#[derive(Clone, Deserialize, Debug)]
pub struct ChromeTab {
    pub id: String,
    pub title: String,
    pub url: String,
    pub webSocketDebuggerUrl: Option<String>,
}

pub fn fetch_tabs() -> Result<Vec<ChromeTab>, Box<dyn std::error::Error>> {
    let tabs: Vec<ChromeTab> = reqwest::blocking::get("http://localhost:9222/json")?.json()?;
    Ok(tabs)
}

pub fn export_cookies_for_tab(tab: &ChromeTab) -> Result<String, Box<dyn std::error::Error>> {
    use std::fs::File;
    use std::io::Write;

    let ws_url = if let Some(ws) = &tab.webSocketDebuggerUrl {
        ws.clone()
    } else {
        get_ws_url_for_tab(&tab.id)?
    };

    let (mut socket, _) = connect(ws_url)?;

    let msg = json!({
        "id": 1,
        "method": "Network.getCookies",
        "params": { "urls": [ &tab.url ] }
    });

    socket.write_message(Message::Text(msg.to_string().into()))?;

    let reply = socket.read_message()?.into_text()?;
    let value: serde_json::Value = serde_json::from_str(&reply)?;
    let cookies = value["result"]["cookies"].clone();

    let filename = format!(
        "cookies_{}.json",
        tab.title.replace(' ', "_").replace('/', "_")
    );
    let mut file = File::create(&filename)?;
    file.write_all(serde_json::to_string_pretty(&cookies)?.as_bytes())?;
    Ok(filename)
}

pub fn get_ws_url_for_tab(tab_id: &str) -> Result<String, Box<dyn std::error::Error>> {
    let tabs: Value = reqwest::blocking::get("http://localhost:9222/json")?.json()?;
    for tab in tabs.as_array().unwrap() {
        if tab["id"] == tab_id {
            return Ok(tab["webSocketDebuggerUrl"].as_str().unwrap().to_string());
        }
    }
    Err("WebSocketDebuggerUrl not found for tab".into())
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Cookie {
    pub domain: String,
    pub expires: Option<f64>,
    pub httpOnly: Option<bool>,
    pub name: String,
    pub path: String,
    pub priority: Option<String>,
    pub sameParty: Option<bool>,
    pub sameSite: Option<String>,
    pub secure: Option<bool>,
    pub session: Option<bool>,
    pub size: Option<u64>,
    pub sourcePort: Option<u16>,
    pub sourceScheme: Option<String>,
    pub value: String,
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

pub fn import_and_open_with_cookies(
    cookie_path: &std::path::Path,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let cookies = match universal_cookie_loader(cookie_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("JSON decode error: {}", e);
            return Err(e);
        }
    };

    let to_open = normalize_url(&url);

    let resp = reqwest::blocking::Client::new()
        .put(&format!("http://localhost:9222/json/new?{}", to_open))
        .send()?;
    let body = resp.text()?;
    let new_tab: Value = serde_json::from_str(&body)?;

    let ws_url = new_tab["webSocketDebuggerUrl"].as_str().unwrap();
    let (mut socket, _) = connect(ws_url)?;

    let enable_msg = json!({
        "id": 1, "method": "Network.enable"
    });

    socket.write_message(Message::Text(enable_msg.to_string().into()))?;

    for (i, cookie) in cookies.iter().enumerate() {
        let mut params = serde_json::Map::new();
        params.insert("name".to_string(), json!(cookie.name));
        params.insert("value".to_string(), json!(cookie.value));
        params.insert("domain".to_string(), json!(cookie.domain));
        params.insert("path".to_string(), json!(cookie.path));
        if let Some(exp) = cookie.expires {
            params.insert("expires".to_string(), json!(exp));
        }
        if let Some(secure) = cookie.secure {
            params.insert("secure".to_string(), json!(secure));
        }
        if let Some(http_only) = cookie.httpOnly {
            params.insert("httpOnly".to_string(), json!(http_only));
        }
        if let Some(ref samesite) = cookie.sameSite {
            params.insert("sameSite".to_string(), json!(samesite));
        }

        let msg = json!({
            "id": 2 + i as u64,
            "method": "Network.setCookie",
            "params": params
        });
        socket.write_message(Message::Text(msg.to_string().into()))?;
    }
    let nav_msg = json!({
        "id": 10000,
        "method": "Page.navigate",
        "params": {"url": to_open}
    });
    socket.write_message(Message::Text(nav_msg.to_string().into()))?;
    Ok(())
}
pub fn universal_cookie_loader(
    path: &std::path::Path,
) -> Result<Vec<Cookie>, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&content)?;

    if let Some(arr) = value.as_array() {
        let cookies: Vec<Cookie> = serde_json::from_value(value)?;
        return Ok(cookies);
    }

    if let Some(arr) = value.get("cookies").and_then(|v| v.as_array()) {
        let cookies: Vec<Cookie> = serde_json::from_value(arr.clone().into())?;
        return Ok(cookies);
    }

    Err("Unknown cookie JSON format".into())
}
fn normalize_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_owned()
    } else {
        format!("https://{}", raw)
    }
}
pub fn import_and_open_with_cookies_from_memory(
    cookies: &[Cookie],
    url: &str,
) -> Result<String, Box<dyn Error>> {
    let to_open = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{}", url)
    };
    println!("Navigating to URL: {}", to_open);
    let cdp_url = format!("http://localhost:9222/json/new?{}", to_open);
    println!("HTTP PUT {}", cdp_url);
    let resp = reqwest::blocking::Client::new().put(&cdp_url).send()?;
    println!("HTTP PUT status: {}", resp.status());
    let body = resp.text()?;
    println!("CDP response body: {}", body);
    let new_tab: serde_json::Value = serde_json::from_str(&body)?;
    let local_tab_id = new_tab["id"]
        .as_str()
        .ok_or("missing new tab ID")?
        .to_string();

    let ws_url = new_tab["webSocketDebuggerUrl"]
        .as_str()
        .ok_or("missing webSocketDebuggerUrl")?;
    println!("WebSocket URL: {}", ws_url);

    println!("Connecting WebSocket to {}", ws_url);
    let (mut socket, _) = connect(ws_url)?;
    let enable = json!({ "id": 1, "method": "Network.enable" });
    println!("Sending Network.enable");
    socket.write_message(Message::Text(enable.to_string().into()))?;

    for (i, cookie) in cookies.iter().enumerate() {
        let mut params = serde_json::Map::new();
        params.insert("name".into(), json!(cookie.name));
        params.insert("value".into(), json!(cookie.value));
        params.insert("domain".into(), json!(cookie.domain));
        params.insert("path".into(), json!(cookie.path));
        if let Some(ex) = cookie.expires {
            params.insert("expires".into(), json!(ex));
        }
        if let Some(true) = cookie.secure {
            params.insert("secure".into(), json!(true));
        }
        if let Some(true) = cookie.httpOnly {
            params.insert("httpOnly".into(), json!(true));
        }
        if let Some(ss) = &cookie.sameSite {
            params.insert("sameSite".into(), json!(ss));
        }

        let msg = json!({
            "id": 2 + i as u64,
            "method": "Network.setCookie",
            "params": params,
        });
        println!("Setting cookie {}: {}", cookie.name, msg);
        socket.write_message(Message::Text(msg.to_string().into()))?;
    }

    let nav = json!({
        "id": 10000,
        "method": "Page.navigate",
        "params": { "url": to_open }
    });
    println!("Sending navigate: {}", nav);
    socket.write_message(Message::Text(nav.to_string().into()))?;

    println!("import_and_open_with_cookies_from_memory complete");
    Ok(local_tab_id)
}

/// Revoke (delete) cookies in a live tab, based on name/domain/path.
///
/// You must have a running tab (identified by its `tab_id`) on localhost:9222.
pub fn revoke_cookies(
    tab_id: &str,
    cookies: &[(&str, &str, &str)], // (name, domain, path)
) -> Result<(), Box<dyn Error>> {
    // 1) Find the WS URL for this tab
    let ws_url = {
        let tabs: serde_json::Value =
            reqwest::blocking::get("http://localhost:9222/json")?.json()?;
        let list = tabs.as_array().ok_or("tabs not array")?;
        let entry = list
            .iter()
            .find(|t| t["id"] == tab_id)
            .ok_or("tab not found")?;
        entry["webSocketDebuggerUrl"]
            .as_str()
            .ok_or("missing webSocketDebuggerUrl")?
            .to_string()
    };

    let (mut socket, _) = connect(ws_url)?;

    for (i, &(name, domain, path)) in cookies.iter().enumerate() {
        let params = json!({
            "name": name,
            "domain": domain,
            "path": path,
        });
        let msg = json!({
            "id": 10_000 + i as u64,
            "method": "Network.deleteCookies",
            "params": params,
        });
        socket.write_message(Message::Text(msg.to_string().into()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_cookie_json_deserialization() {
        let path = "/Users/manavkdubey/Downloads/cookies_ChatGPT.json";
        let data = fs::read_to_string(path).expect("File not found");
        let try_array = serde_json::from_str::<Vec<Cookie>>(&data);

        match try_array {
            Ok(cookies) => {
                println!(
                    "✅ Parsed as Vec<Cookie>. First entry:\n{:#?}",
                    cookies.get(0)
                );
            }
            Err(e) => {
                println!("❌ Failed to parse as Vec<Cookie>: {e}");

                let as_value: serde_json::Value = serde_json::from_str(&data).unwrap();
                if let Some(arr) = as_value.get("cookies") {
                    let cookies_json = arr.to_string();
                    let try_key = serde_json::from_str::<Vec<Cookie>>(&cookies_json);
                    match try_key {
                        Ok(cookies) => println!(
                            "Parsed as {{ cookies: [...] }}. First entry:\n{:#?}",
                            cookies.get(0)
                        ),
                        Err(e) => println!(
                            "❌ Failed to parse cookies array in {{ cookies: [...] }}: {e}"
                        ),
                    }
                }
            }
        }
    }
}
pub fn get_cookies_for_tab(tab: &ChromeTab) -> Result<Vec<Cookie>, Box<dyn Error>> {
    let ws_url = if let Some(ws) = &tab.webSocketDebuggerUrl {
        ws.clone()
    } else {
        get_ws_url_for_tab(&tab.id)?
    };

    let (mut socket, _) = connect(ws_url)?;

    let cmd = serde_json::json!({
        "id": 1,
        "method": "Network.getCookies",
        "params": { "urls": [ tab.url ] }
    });
    socket.write_message(Message::Text(cmd.to_string().into()))?;

    let txt = socket.read_message()?.into_text()?;
    let v: Value = serde_json::from_str(&txt)?;

    let arr = v["result"]["cookies"].clone();
    let cookies: Vec<Cookie> = serde_json::from_value(arr)?;
    Ok(cookies)
}
