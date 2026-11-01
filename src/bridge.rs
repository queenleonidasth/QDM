//! Small authenticated loopback endpoint for the QDM browser extensions.
use crate::{api::AddRequest, engine::Engine};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

pub struct Bridge {
    pub port: u16,
    pub token: String,
    task: tokio::task::JoinHandle<()>,
}
impl Bridge {
    pub fn close(self) {
        self.task.abort();
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start(engine: Engine, data: &Path) -> Result<Bridge, String> {
    let token = token(data).await?;
    let mut listener = None;
    for port in 17876..=17885 {
        match TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
            Ok(v) => {
                listener = Some(v);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let listener = listener.ok_or("QDM browser connection ports are occupied")?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let secret = token.clone();
    let task = tokio::spawn(async move {
        let permits = Arc::new(Semaphore::new(16));
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                drop(socket);
                continue;
            };
            let engine = engine.clone();
            let secret = secret.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _ = tokio::time::timeout(
                    Duration::from_secs(10),
                    serve(socket, engine, port, secret),
                )
                .await;
            });
        }
    });
    Ok(Bridge { port, token, task })
}
async fn token(data: &Path) -> Result<String, String> {
    let path = data.join("browser-pairing-v2.bin");
    if let Ok(bytes) = tokio::fs::read(&path).await {
        if let Ok(value) = crate::platform::unprotect(&bytes)
            .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
        {
            if value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Ok(value);
            }
        }
    }
    let value = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let protected = crate::platform::protect(value.as_bytes())?;
    tokio::fs::write(&path, protected)
        .await
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(value)
}
fn trusted_origin(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if let Some(host) = value.strip_prefix("chrome-extension://") {
        return host.len() == 32 && host.bytes().all(|b| (b'a'..=b'p').contains(&b));
    }
    if let Some(host) = value.strip_prefix("moz-extension://") {
        return !host.is_empty()
            && host.len() <= 128
            && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    }
    false
}
fn equal_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |diff, (x, y)| diff | (x ^ y))
            == 0
}
async fn reply(
    socket: &mut TcpStream,
    status: u16,
    value: Value,
    origin: &str,
) -> std::io::Result<()> {
    let body = if status == 204 {
        Vec::new()
    } else {
        serde_json::to_vec(&value).unwrap_or_default()
    };
    let mut headers=format!("HTTP/1.1 {status} QDM\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",body.len());
    if !origin.is_empty() && trusted_origin(origin) {
        headers.push_str(&format!("Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type, Authorization\r\nAccess-Control-Allow-Private-Network: true\r\n"));
    }
    headers.push_str("\r\n");
    socket.write_all(headers.as_bytes()).await?;
    socket.write_all(&body).await?;
    socket.shutdown().await
}
async fn serve(
    mut socket: TcpStream,
    engine: Engine,
    port: u16,
    token: String,
) -> std::io::Result<()> {
    let mut bytes = Vec::with_capacity(4096);
    let header_end;
    loop {
        if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = i + 4;
            break;
        }
        if bytes.len() > 32768 {
            return reply(
                &mut socket,
                431,
                json!({"error":"Request headers too large"}),
                "",
            )
            .await;
        }
        let mut buf = [0u8; 4096];
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    if header_end > 32768 {
        return reply(
            &mut socket,
            431,
            json!({"error":"Request headers too large"}),
            "",
        )
        .await;
    }
    let Ok(text) = std::str::from_utf8(&bytes[..header_end]) else {
        return reply(
            &mut socket,
            400,
            json!({"error":"Invalid request headers"}),
            "",
        )
        .await;
    };
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_owned();
    let route = first.next().unwrap_or("").to_owned();
    if first.next() != Some("HTTP/1.1") || first.next().is_some() {
        return reply(
            &mut socket,
            400,
            json!({"error":"Invalid request line"}),
            "",
        )
        .await;
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|s| !s.is_empty()) {
        let Some((key, value)) = line.split_once(':') else {
            return reply(&mut socket, 400, json!({"error":"Invalid header"}), "").await;
        };
        if headers
            .insert(key.trim().to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return reply(&mut socket, 400, json!({"error":"Duplicate header"}), "").await;
        }
    }
    let origin = headers.get("origin").cloned().unwrap_or_default();
    if headers.get("host").map(String::as_str) != Some(format!("127.0.0.1:{port}").as_str())
        || !trusted_origin(&origin)
    {
        return reply(&mut socket, 403, json!({"error":"Origin denied"}), "").await;
    }
    if method == "OPTIONS" {
        return reply(&mut socket, 204, Value::Null, &origin).await;
    }
    if !equal_secret(
        headers
            .get("authorization")
            .map(String::as_str)
            .unwrap_or(""),
        &format!("Bearer {token}"),
    ) {
        return reply(
            &mut socket,
            401,
            json!({"error":"Pair QDM with the browser extension first"}),
            &origin,
        )
        .await;
    }
    if method == "GET" && route == "/health" {
        return reply(
            &mut socket,
            200,
            json!({"name":"QDM","version":env!("CARGO_PKG_VERSION")}),
            &origin,
        )
        .await;
    }
    if method != "POST" || route != "/downloads" {
        return reply(&mut socket, 404, json!({"error":"Not found"}), &origin).await;
    }
    if !headers
        .get("content-type")
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false)
    {
        return reply(
            &mut socket,
            415,
            json!({"error":"JSON is required"}),
            &origin,
        )
        .await;
    }
    if headers.contains_key("transfer-encoding") {
        return reply(
            &mut socket,
            400,
            json!({"error":"Content-Length is required"}),
            &origin,
        )
        .await;
    }
    let Some(length) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n <= 256 * 1024)
    else {
        return reply(
            &mut socket,
            413,
            json!({"error":"Request body too large or missing length"}),
            &origin,
        )
        .await;
    };
    while bytes.len() < header_end + length {
        let mut buf = [0u8; 8192];
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    let request = serde_json::from_slice::<Value>(&bytes[header_end..header_end + length]);
    let value = match request {
        Ok(v) => v,
        Err(_) => return reply(&mut socket, 400, json!({"error":"Invalid JSON"}), &origin).await,
    };
    if !value.is_object() {
        return reply(
            &mut socket,
            400,
            json!({"error":"Download information must be an object"}),
            &origin,
        )
        .await;
    }
    for key in ["filename", "expectedSha256", "expected_sha256"] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_string())
        {
            return reply(
                &mut socket,
                400,
                json!({"error":format!("{key} must be a string")}),
                &origin,
            )
            .await;
        }
    }
    if value.get("start").is_some_and(|v| !v.is_boolean())
        || value
            .get("scheduled_at")
            .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
        || value.get("urls").is_some_and(|v| !v.is_array())
    {
        return reply(
            &mut socket,
            400,
            json!({"error":"Invalid download options"}),
            &origin,
        )
        .await;
    }
    let urls = if let Some(list) = value.get("urls").and_then(Value::as_array) {
        list.clone()
    } else {
        vec![value.get("url").cloned().unwrap_or(Value::Null)]
    };
    if urls.is_empty() || urls.len() > 200 {
        return reply(
            &mut socket,
            400,
            json!({"error":"Add between 1 and 200 URLs"}),
            &origin,
        )
        .await;
    }
    // Validate the whole batch before accepting any of it.
    for u in &urls {
        if u.as_str()
            .filter(|u| u.len() <= 16384)
            .and_then(|u| url::Url::parse(u).ok())
            .filter(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.username().is_empty()
                    && u.password().is_none()
            })
            .is_none()
        {
            return reply(
                &mut socket,
                400,
                json!({"error":"Use HTTP(S) URLs without embedded passwords"}),
                &origin,
            )
            .await;
        }
    }
    // Reject protected browser-session transfers before accepting any batch item.
    // The browser extension leaves the original download untouched on this error.
    if urls.iter().any(|item| item.as_str()
        .and_then(|s| url::Url::parse(s).ok())
        .is_some_and(|url| crate::engine::protocol::is_chatgpt_session_download(&url))) {
        return reply(&mut socket, 400,
            json!({"error":"ChatGPT session-protected image: download with your browser; QDM will not intercept it."}),
            &origin).await;
    }
    let session_headers = value
        .get("headers")
        .cloned()
        .map(serde_json::from_value::<BTreeMap<String, String>>)
        .transpose();
    let session_headers = match session_headers {
        Ok(v) => v.unwrap_or_default(),
        Err(_) => {
            return reply(
                &mut socket,
                400,
                json!({"error":"Invalid session headers"}),
                &origin,
            )
            .await
        }
    };
    let mut ids = Vec::new();
    for u in urls {
        let request = AddRequest {
            url: u.as_str().unwrap_or("").to_owned(),
            filename: value
                .get("filename")
                .and_then(Value::as_str)
                .map(str::to_owned),
            expected_sha256: value
                .get("expectedSha256")
                .or_else(|| value.get("expected_sha256"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            start: value.get("start").and_then(Value::as_bool).unwrap_or(true),
            headers: session_headers.clone(),
            scheduled_at: value.get("scheduled_at").and_then(Value::as_u64),
            ..Default::default()
        };
        match engine.add(request).await {
            Ok(id) => ids.push(id),
            Err(e) => return reply(&mut socket, 400, json!({"error":e,"ids":ids}), &origin).await,
        }
    }
    reply(
        &mut socket,
        201,
        json!({"accepted":ids.len(),"ids":ids}),
        &origin,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn origin_allowlist() {
        assert!(trusted_origin(
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(trusted_origin("moz-extension://test-123"));
        assert!(!trusted_origin("https://example.com"));
        assert!(!trusted_origin(
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/path"
        ));
        assert!(!trusted_origin("moz-extension://test\r\nX: a"));
    }
    #[test]
    fn secrets_match_exactly() {
        assert!(equal_secret("abc", "abc"));
        assert!(!equal_secret("abc", "ab"));
        assert!(!equal_secret("abc", "abd"));
    }
}
