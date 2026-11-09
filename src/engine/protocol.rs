use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};
use url::Url;

pub const OVERLAP: u64 = 32 * 1024;
pub const MIN_SEGMENT: u64 = 12 * 1024;

pub fn is_chatgpt_session_download(url: &Url) -> bool {
    matches!(url.host_str(), Some("chatgpt.com" | "chat.openai.com"))
        && url.path().starts_with("/backend-api/estuary/content")
}

pub fn validate_url(raw: &str) -> Result<String, String> {
    if raw.len() > 32768 {
        return Err("Download URL is too long".into());
    }
    let mut url = Url::parse(raw.trim()).map_err(|_| "Enter a valid HTTP, HTTPS or FTP URL")?;
    if url.host_str().is_none() {
        return Err("QDM supports HTTP, HTTPS and FTP URLs".into());
    }
    if url.scheme() == "ftp" {
        url.set_fragment(None);
        return Ok(url.to_string());
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err("QDM supports HTTP, HTTPS and FTP URLs".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "Use request authentication headers rather than credentials inside a URL".into(),
        );
    }
    if is_chatgpt_session_download(&url) {
        return Err("This ChatGPT image requires your browser session. Download it with Zen/your browser; QDM will not intercept it.".into());
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

/// FTP endpoints are single-stream (no byte ranges). Passive mode with
/// `REST` resume is supported; credentials may live in the URL
/// (`ftp://user:pass@host/path`) or default to anonymous.
pub struct FtpTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub path: String,
}

pub fn parse_ftp_url(raw: &str) -> Result<FtpTarget, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "Enter a valid FTP URL")?;
    if url.scheme() != "ftp" {
        return Err("Only ftp:// URLs use the FTP engine".into());
    }
    let host = url.host_str().ok_or("FTP URL needs a host")?.to_owned();
    let port = url.port_or_known_default().unwrap_or(21);
    let username = if url.username().is_empty() {
        "anonymous".to_owned()
    } else {
        url.username().to_owned()
    };
    let password = url.password().map(str::to_owned).unwrap_or_else(|| "anonymous@".into());
    let path = if url.path().is_empty() { "/".to_owned() } else { url.path().to_owned() };
    Ok(FtpTarget { host, port, username, password, path })
}

pub fn request_headers(values: &BTreeMap<String, String>) -> Result<HeaderMap, String> {
    if values.len() > 64 {
        return Err("Too many request headers".into());
    }
    let mut headers = HeaderMap::new();
    let mut bytes = 0usize;
    for (name, value) in values {
        bytes += name.len() + value.len();
        if bytes > 64 * 1024 {
            return Err("Request headers exceed 64 KiB".into());
        }
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "Invalid header name")?;
        if matches!(
            name.as_str(),
            "host"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "range"
                | "if-range"
                | "accept-encoding"
                | "proxy-authorization"
                | "proxy-connection"
                | "upgrade"
        ) {
            return Err(format!("QDM manages the {} header itself", name));
        }
        let value = HeaderValue::from_str(value).map_err(|_| "Invalid request header value")?;
        headers.insert(name, value);
    }
    headers.insert("accept-encoding", HeaderValue::from_static("identity"));
    Ok(headers)
}

pub fn safe_filename(raw: &str) -> String {
    let mut name: String = raw.chars().map(|c| {
        if c.is_control() || matches!(c, '<'|'>'|':'|'"'|'/'|'\\'|'|'|'?'|'*'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') { '_' } else { c }
    }).collect();
    name = name
        .trim()
        .trim_start_matches('.')
        .trim_end_matches([' ', '.'])
        .to_string();
    if name.is_empty() {
        name = "download".into();
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .map(|s| {
                    matches!(
                        s,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
                .unwrap_or(false)
        });
    if reserved {
        name.insert(0, '_');
    }
    if name.encode_utf16().count() > 180 {
        let extension = name
            .rsplit_once('.')
            .map(|(_, e)| format!(".{}", e))
            .filter(|e| e.encode_utf16().count() <= 24)
            .unwrap_or_default();
        let max = 180 - extension.encode_utf16().count();
        let mut units = 0;
        let prefix: String = name
            .chars()
            .take_while(|c| {
                units += c.len_utf16();
                units <= max
            })
            .collect();
        name = prefix + &extension;
    }
    name
}

pub fn collision_filename(original: &str, index: u32) -> String {
    if index == 0 {
        return safe_filename(original);
    }
    let original = safe_filename(original);
    let (stem, extension) = original
        .rsplit_once('.')
        .map(|(stem, extension)| (stem, format!(".{extension}")))
        .filter(|(_, extension)| extension.encode_utf16().count() <= 24)
        .unwrap_or((&original, String::new()));
    let suffix = format!(" ({index})");
    let budget = 180 - suffix.encode_utf16().count() - extension.encode_utf16().count();
    let mut units = 0;
    let stem: String = stem
        .chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= budget
        })
        .collect();
    format!("{stem}{suffix}{extension}")
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let value =
                u8::from_str_radix(std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?, 16)
                    .ok()?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn filename_from_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut parts| parts.next_back())
                .map(str::to_owned)
        })
        .map(|name| safe_filename(&percent_decode(&name).unwrap_or(name)))
        .unwrap_or_else(|| "download".into())
}

pub fn disposition_filename(raw: &str) -> Option<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in raw.chars() {
        if escaped {
            field.push(character);
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ';' && !quoted {
            fields.push(std::mem::take(&mut field));
        } else {
            field.push(character);
        }
    }
    fields.push(field);
    let mut legacy = None;
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if key.trim().eq_ignore_ascii_case("filename*") {
            let mut parts = value.splitn(3, '\'');
            let charset = parts.next().unwrap_or("");
            let _language = parts.next();
            if charset.eq_ignore_ascii_case("utf-8") {
                if let Some(decoded) = parts.next().and_then(percent_decode) {
                    return Some(safe_filename(&decoded));
                }
            }
        } else if key.trim().eq_ignore_ascii_case("filename") {
            legacy = Some(safe_filename(value));
        }
    }
    legacy
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: u64,
}

pub fn content_range(value: &str) -> Option<ContentRange> {
    let (unit, range) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = range.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    if [start, end, total]
        .iter()
        .any(|v| v.is_empty() || !v.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let parsed = ContentRange {
        start: start.parse().ok()?,
        end: end.parse().ok()?,
        total: total.parse().ok()?,
    };
    (parsed.start <= parsed.end && parsed.end < parsed.total).then_some(parsed)
}

pub fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|h| String::from_utf8_lossy(h.as_bytes()).trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn content_length(headers: &HeaderMap) -> Result<Option<u64>, String> {
    let Some(raw) = header_string(headers, "content-length") else {
        return Ok(None);
    };
    if !raw.bytes().all(|v| v.is_ascii_digit()) {
        return Err("Invalid Content-Length".into());
    }
    raw.parse()
        .map(Some)
        .map_err(|_| "File length exceeds QDM's supported 64-bit length".into())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Validator {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    #[serde(default)]
    pub strong_date: bool,
}

impl Validator {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let etag = header_string(headers, "etag");
        let last_modified = header_string(headers, "last-modified")
            .filter(|v| httpdate::parse_http_date(v).is_ok());
        let date = header_string(headers, "date").and_then(|v| httpdate::parse_http_date(&v).ok());
        let strong_date = last_modified
            .as_deref()
            .and_then(|v| httpdate::parse_http_date(v).ok())
            .zip(date)
            .and_then(|(modified, date)| date.duration_since(modified).ok())
            .map(|d| d >= Duration::from_secs(60))
            .unwrap_or(false);
        Self {
            etag,
            last_modified,
            strong_date,
        }
    }
    pub fn strong_etag(&self) -> Option<&str> {
        self.etag.as_deref().filter(|e| {
            e.len() >= 2
                && e.starts_with('"')
                && e.ends_with('"')
                && !e[1..e.len() - 1].contains('"')
        })
    }
    pub fn if_range(&self) -> Option<&str> {
        self.strong_etag().or_else(|| {
            (self.etag.is_none() && self.strong_date)
                .then_some(self.last_modified.as_deref())
                .flatten()
        })
    }
    pub fn changed_from(&self, previous: &Self) -> bool {
        if let (Some(a), Some(b)) = (&self.etag, &previous.etag) {
            if a != b {
                return true;
            }
        }
        if let (Some(a), Some(b)) = (&self.last_modified, &previous.last_modified) {
            if httpdate::parse_http_date(a).ok() != httpdate::parse_http_date(b).ok() {
                return true;
            }
        }
        false
    }
}

pub fn retry_after(headers: &HeaderMap) -> Duration {
    let seconds = header_string(headers, "retry-after")
        .and_then(|s| {
            s.parse::<u64>().ok().or_else(|| {
                httpdate::parse_http_date(&s)
                    .ok()
                    .and_then(|time| time.duration_since(SystemTime::now()).ok())
                    .map(|d| d.as_secs())
            })
        })
        .unwrap_or(0);
    Duration::from_secs(seconds.min(120))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_boundaries() {
        assert_eq!(safe_filename("../CON.txt"), "_CON.txt");
        assert_eq!(safe_filename("NUL"), "_NUL");
        assert!(!safe_filename("../../escape.exe").contains('/'));
        assert_eq!(
            disposition_filename(
                "attachment; filename*=UTF-8''%E0%B9%84%E0%B8%9F%E0%B8%A5%E0%B9%8C.bin"
            ),
            Some("ไฟล์.bin".into())
        );
        assert!(safe_filename(&"😀".repeat(200)).encode_utf16().count() <= 180);
        let long = safe_filename(&format!("{}.bin", "😀".repeat(200)));
        let second = collision_filename(&long, 1);
        assert_ne!(long, second);
        assert!(second.ends_with(" (1).bin"));
        assert!(second.encode_utf16().count() <= 180);
    }
    #[test]
    fn byte_range_boundaries() {
        assert_eq!(
            content_range("bytes 0-9/10"),
            Some(ContentRange {
                start: 0,
                end: 9,
                total: 10
            })
        );
        for value in [
            "bytes 9-8/10",
            "bytes 0-10/10",
            "bytes */10",
            "bytes +1-2/3",
            "items 0-1/2",
        ] {
            assert_eq!(content_range(value), None);
        }
    }
    #[test]
    fn chatgpt_session_images_stay_with_browser() {
        for url in [
            "https://chatgpt.com/backend-api/estuary/content?id=abc",
            "https://chat.openai.com/backend-api/estuary/content?x=1",
        ] {
            assert!(validate_url(url).unwrap_err().contains("browser session"));
        }
        assert!(validate_url("https://example.com/backend-api/estuary/content").is_ok());
        assert!(validate_url("https://chatgpt.com/ordinary-image.png").is_ok());
    }

    #[test]
    fn validator_strength() {
        let mut h = HeaderMap::new();
        h.insert("etag", HeaderValue::from_static("W/\"v1\""));
        h.insert(
            "last-modified",
            HeaderValue::from_static("Mon, 26 Oct 2026 08:00:00 GMT"),
        );
        h.insert(
            "date",
            HeaderValue::from_static("Mon, 26 Oct 2026 09:00:00 GMT"),
        );
        assert_eq!(Validator::from_headers(&h).if_range(), None);
        h.remove("etag");
        assert_eq!(
            Validator::from_headers(&h).if_range(),
            Some("Mon, 26 Oct 2026 08:00:00 GMT")
        );
        h.insert("etag", HeaderValue::from_static("\"v1\""));
        assert_eq!(Validator::from_headers(&h).if_range(), Some("\"v1\""));
    }
}
