//! Small helpers: wide strings, time, URL heuristics, window-handle wrapper.

use std::num::NonZeroIsize;
use std::time::{SystemTime, UNIX_EPOCH};

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use windows::Win32::Foundation::HWND;

pub const INTERNAL_PREFIX: &str = "https://realism.localhost/";
pub const INTERNAL_ORIGIN: &str = "https://realism.localhost";

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Debug log (only when REALISM_DEBUG is set): TEMP/realism-debug.log
pub fn dlog(msg: &str) {
    if std::env::var_os("REALISM_DEBUG").is_none() {
        return;
    }
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(std::env::temp_dir().join("realism-debug.log")) {
        let _ = writeln!(f, "{} {msg}", now_ms());
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Borrowed HWND usable by wry's `build_as_child`.
pub struct HwndRef(pub HWND);

impl HasWindowHandle for HwndRef {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let nz = NonZeroIsize::new(self.0 .0 as isize).ok_or(HandleError::Unavailable)?;
        let h = Win32WindowHandle::new(nz);
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(h)) })
    }
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let h = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = h {
                    out.push(v);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `a=1&b=2` query into pairs.
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (url_decode(k), url_decode(v)),
            None => (url_decode(p), String::new()),
        })
        .collect()
}

/// "https://www.example.com:8080/path" -> "www.example.com"
pub fn host_of(url: &str) -> &str {
    let rest = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => return "",
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let hostport = &rest[..end];
    let hostport = hostport.rsplit('@').next().unwrap_or(hostport);
    if hostport.starts_with('[') {
        return hostport.split(']').next().map(|h| &h[..]).unwrap_or(hostport);
    }
    hostport.split(':').next().unwrap_or(hostport)
}

/// "https://www.example.com:8080/path" -> "https://www.example.com:8080"
pub fn origin_of(url: &str) -> String {
    match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            format!("{}://{}", &url[..i].to_ascii_lowercase(), &rest[..end].to_ascii_lowercase())
        }
        None => url.to_string(),
    }
}

pub fn is_internal(url: &str) -> bool {
    url.starts_with(INTERNAL_PREFIX) || url == INTERNAL_ORIGIN
}

/// Real URL -> what we show in the address bar.
pub fn display_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix(INTERNAL_PREFIX) {
        let rest = rest.trim_end_matches('/');
        return format!("realism://{rest}");
    }
    if url == "about:blank" {
        return String::new();
    }
    url.to_string()
}

/// Map `realism://x` to the served URL.
pub fn internal_url(page: &str) -> String {
    format!("{INTERNAL_PREFIX}{page}")
}

pub fn search_url(engine: &str, q: &str) -> String {
    let q = url_encode(q);
    match engine {
        "bing" => format!("https://www.bing.com/search?q={q}"),
        "duckduckgo" => format!("https://duckduckgo.com/?q={q}"),
        _ => format!("https://www.google.com/search?q={q}"),
    }
}

fn looks_like_ipv4(h: &str) -> bool {
    let parts: Vec<&str> = h.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.chars().all(|c| c.is_ascii_digit()))
}

/// What the user typed into the address bar -> URL to navigate to.
/// Returns `None` for the native new-tab page.
pub fn normalize_input(input: &str, engine: &str) -> Option<String> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if let Some(page) = lower.strip_prefix("realism://") {
        let page = page.trim_end_matches('/');
        if page.is_empty() || page == "newtab" {
            return None;
        }
        return Some(internal_url(&t["realism://".len()..]));
    }
    // Explicit schemes
    for s in ["http://", "https://", "file://", "ftp://", "edge://", "view-source:", "about:", "data:", "mailto:"] {
        if lower.starts_with(s) {
            return Some(t.to_string());
        }
    }
    if t.contains(char::is_whitespace) {
        return Some(search_url(engine, t));
    }
    // Windows path, e.g. C:\file.pdf
    if t.len() > 2 && t.as_bytes()[1] == b':' && (t.as_bytes()[2] == b'\\' || t.as_bytes()[2] == b'/') {
        return Some(format!("file:///{}", t.replace('\\', "/")));
    }
    let host_end = t.find(['/', '?', '#']).unwrap_or(t.len());
    let hostport = &t[..host_end];
    let host = hostport.split(':').next().unwrap_or("");
    let port_ok = hostport.split(':').nth(1).map(|p| p.chars().all(|c| c.is_ascii_digit())).unwrap_or(true);
    if !port_ok {
        return Some(search_url(engine, t));
    }
    let host_l = host.to_ascii_lowercase();
    if host_l == "localhost" || host_l.ends_with(".localhost") || looks_like_ipv4(&host_l) {
        return Some(format!("http://{t}"));
    }
    if let Some(dot) = host_l.rfind('.') {
        let tld = &host_l[dot + 1..];
        let valid_chars = host_l.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.');
        if tld.len() >= 2 && tld.chars().all(|c| c.is_alphabetic()) && valid_chars && dot > 0 {
            return Some(format!("https://{t}"));
        }
    }
    Some(search_url(engine, t))
}

pub fn fmt_mb(bytes: u64) -> String {
    format!("{:.0} МБ", bytes as f64 / 1048576.0)
}

pub fn hash_str(s: &str) -> u64 {
    // FNV-1a
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn random_token() -> String {
    let mut seed = now_ms() as u64 ^ (std::process::id() as u64) << 32;
    let addr = &seed as *const u64 as u64;
    seed ^= addr.rotate_left(17);
    let mut out = String::new();
    for _ in 0..4 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.push_str(&format!("{seed:016x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn input_normalization() {
        assert_eq!(normalize_input("example.com", "google").unwrap(), "https://example.com");
        assert_eq!(normalize_input("localhost:3000/x", "google").unwrap(), "http://localhost:3000/x");
        assert!(normalize_input("hello world", "google").unwrap().starts_with("https://www.google.com/search?q=hello+world"));
        assert!(normalize_input("rust", "duckduckgo").unwrap().starts_with("https://duckduckgo.com/?q=rust"));
        assert_eq!(normalize_input("realism://settings", "google").unwrap(), "https://realism.localhost/settings");
        assert_eq!(normalize_input("realism://newtab", "google"), None);
        assert_eq!(normalize_input("192.168.0.1", "google").unwrap(), "http://192.168.0.1");
        assert_eq!(normalize_input("https://a.b/c d", "google").unwrap(), "https://a.b/c d");
    }
    #[test]
    fn hosts() {
        assert_eq!(host_of("https://www.example.com:8080/p?q"), "www.example.com");
        assert_eq!(origin_of("https://Example.com/path"), "https://example.com");
        assert_eq!(display_url("https://realism.localhost/settings"), "realism://settings");
        assert_eq!(url_decode("a%20b+c"), "a b c");
    }
}
