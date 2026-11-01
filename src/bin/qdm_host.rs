//! QDM Firefox/Zen and Chromium native-messaging host. Only the listed WebExtension can launch
//! it through Gecko's native-host registry. Secrets are sent only on stdio.
#![cfg_attr(windows, windows_subsystem = "windows")]
#[path = "../platform.rs"]
#[allow(dead_code)]
mod platform;

use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn stored_token(data: &Path) -> Result<String, String> {
    let encrypted = std::fs::read(data.join("browser-pairing-v2.bin"))
        .map_err(|_| "QDM pairing data is not ready".to_string())?;
    let value = String::from_utf8(platform::unprotect(&encrypted)?)
        .map_err(|_| "QDM pairing data is invalid".to_string())?;
    if value.len() != 64 || !value.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err("QDM pairing data is invalid".into());
    }
    Ok(value)
}

fn probe(port: u16, token: &str) -> bool {
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    let Ok(mut socket) = TcpStream::connect_timeout(&address, Duration::from_millis(160)) else {
        return false;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(550)));
    let _ = socket.set_write_timeout(Some(Duration::from_millis(550)));
    let request = format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n");
    if socket.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buffer = Vec::new();
    if socket.take(4096).read_to_end(&mut buffer).is_err() {
        return false;
    }
    let Ok(text) = std::str::from_utf8(&buffer) else {
        return false;
    };
    let Some((headers, payload)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    if !headers.starts_with("HTTP/1.1 200 ") {
        return false;
    }
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|v| v.get("name").and_then(Value::as_str).map(str::to_owned))
        .as_deref() == Some("QDM")
}

fn find_bridge(token: &str) -> Option<u16> {
    (17876..=17885).find(|port| probe(*port, token))
}

fn start_manager() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("QDM.exe");
    if !exe.is_file() {
        return Err("QDM.exe is missing from the installation".into());
    }
    Command::new(exe)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot start QDM: {e}"))?;
    Ok(())
}

fn discover(start: bool) -> Value {
    let data = platform::data_dir();
    if let Ok(token) = stored_token(&data) {
        if let Some(port) = find_bridge(&token) {
            return json!({"ok":true, "port":port, "token":token});
        }
    }
    if !start {
        return json!({"ok":false,"error":"QDM is offline"});
    }
    if let Err(e) = start_manager() {
        return json!({"ok":false,"error":e});
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if let Ok(token) = stored_token(&data) {
            if let Some(port) = find_bridge(&token) {
                return json!({"ok":true,"port":port,"token":token});
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    json!({"ok":false,"error":"QDM did not become ready"})
}

fn run() -> Result<(), String> {
    // Gecko supplies the caller extension ID in argv. This check is defensive:
    // the native-host manifest is the actual browser-side allowlist.
    if !std::env::args().skip(1).any(|arg| {
        arg == "qdm@queen.local" ||
        (arg.len() == 32 && arg.bytes().all(|b| (b'a'..=b'p').contains(&b)))
    }) {
        return Err("Only the QDM extension may request pairing".into());
    }
    let mut stdin = std::io::stdin().lock();
    let mut length = [0u8; 4];
    stdin.read_exact(&mut length).map_err(|e| e.to_string())?;
    let count = u32::from_le_bytes(length) as usize;
    if count == 0 || count > 16384 {
        return Err("Invalid native message length".into());
    }
    let mut data = vec![0; count];
    stdin.read_exact(&mut data).map_err(|e| e.to_string())?;
    let request = serde_json::from_slice::<Value>(&data).map_err(|e| e.to_string())?;
    let response = if request.get("action").and_then(Value::as_str) == Some("discover") {
        discover(request.get("start").and_then(Value::as_bool).unwrap_or(false))
    } else {
        json!({"ok":false,"error":"Unknown native operation"})
    };
    let bytes = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&(bytes.len() as u32).to_le_bytes()).map_err(|e| e.to_string())?;
    stdout.write_all(&bytes).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        // Never print tokens or arbitrary message payloads into browser logs.
        eprintln!("QDM native host: {error}");
        std::process::exit(1);
    }
}
