//! Self-update through GitHub Releases.
//!
//! * check:    GET https://api.github.com/repos/{REPO}/releases/latest (WinHTTP, background thread)
//! * download: the `Realism-Browser-Setup.exe` asset into %TEMP%, verified with SHA-256
//!   (GitHub's asset digest and/or the SHA256SUMS.txt asset of the release)
//! * install:  run the setup with `--silent --update --relaunch --dir <current folder>`, then exit;
//!   the setup waits until the browser has closed, replaces realism.exe and starts it again.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Security::Cryptography::*;

pub const REPO: &str = "realityunrealengine-cell/realism-browser";
pub const ASSET: &str = "Realism-Browser-Setup.exe";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct Release {
    pub tag: String,
    pub version: (u32, u32, u32),
    pub notes: String,
    pub url: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub page: String,
    pub published: String,
}

#[derive(Clone, Debug)]
pub enum State {
    Idle,
    Checking,
    UpToDate { latest: String },
    Available(Release),
    Downloading(Release),
    Ready(Release, PathBuf),
    Error(String),
}

enum Msg {
    Checked(Result<Option<Release>, String>),
    Downloaded(Result<(Release, PathBuf), String>),
}

static INBOX: Mutex<Vec<Msg>> = Mutex::new(Vec::new());
pub static DL_RECEIVED: AtomicU64 = AtomicU64::new(0);
pub static DL_TOTAL: AtomicU64 = AtomicU64::new(0);

pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u32>());
    let a = it.next()?.ok()?;
    let b = it.next().unwrap_or(Ok(0)).ok()?;
    let c = it.next().unwrap_or(Ok(0)).ok()?;
    Some((a, b, c))
}

pub fn current() -> (u32, u32, u32) {
    parse_version(VERSION).unwrap_or((0, 0, 0))
}

// ---------------------------------------------------------------- HTTP (WinHTTP)

struct H(*mut core::ffi::c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// HTTPS GET. Follows redirects (GitHub release assets redirect to their CDN).
/// `sink` receives the body in chunks; returns the HTTP status.
fn https_get(url: &str, accept: &str, mut sink: impl FnMut(&[u8], u64) -> Result<(), String>) -> Result<u32, String> {
    let rest = url.strip_prefix("https://").ok_or("only https is allowed")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    unsafe {
        let ua = HSTRING::from(format!("RealismBrowser/{VERSION} (+https://github.com/{REPO})"));
        let s = H(WinHttpOpen(&ua, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if s.0.is_null() {
            return Err("WinHttpOpen failed".into());
        }
        let _ = WinHttpSetTimeouts(s.0, 10_000, 10_000, 15_000, 30_000);
        let c = H(WinHttpConnect(s.0, &HSTRING::from(host), 443, 0));
        if c.0.is_null() {
            return Err(format!("нет соединения с {host}"));
        }
        let r = H(WinHttpOpenRequest(c.0, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if r.0.is_null() {
            return Err("WinHttpOpenRequest failed".into());
        }
        let headers: Vec<u16> = format!("Accept: {accept}\r\nX-GitHub-Api-Version: 2022-11-28\r\n").encode_utf16().collect();
        WinHttpSendRequest(r.0, Some(&headers), None, 0, 0, 0).map_err(|e| format!("запрос не отправлен: {e}"))?;
        WinHttpReceiveResponse(r.0, std::ptr::null_mut()).map_err(|e| format!("нет ответа сервера: {e}"))?;
        let mut status = 0u32;
        let mut len = 4u32;
        let mut idx = 0u32;
        let _ = WinHttpQueryHeaders(
            r.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            &mut idx,
        );
        let mut clen_buf = [0u16; 32];
        let mut clen_len = (clen_buf.len() * 2) as u32;
        let mut idx = 0u32;
        let total = if WinHttpQueryHeaders(r.0, WINHTTP_QUERY_CONTENT_LENGTH, PCWSTR::null(), Some(clen_buf.as_mut_ptr() as *mut _), &mut clen_len, &mut idx).is_ok() {
            String::from_utf16_lossy(&clen_buf[..(clen_len / 2) as usize]).trim().parse().unwrap_or(0)
        } else {
            0
        };
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(r.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).map_err(|e| format!("обрыв загрузки: {e}"))?;
            if read == 0 {
                break;
            }
            sink(&buf[..read as usize], total)?;
        }
        Ok(status)
    }
}

pub fn sha256_hex(data: &[u8]) -> Option<String> {
    unsafe {
        let mut alg = BCRYPT_ALG_HANDLE::default();
        if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_SHA256_ALGORITHM, PCWSTR::null(), BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0)).is_err() {
            return None;
        }
        let mut out = [0u8; 32];
        let st = BCryptHash(alg, None, data, &mut out);
        let _ = BCryptCloseAlgorithmProvider(alg, 0);
        if st.is_err() {
            return None;
        }
        Some(out.iter().map(|b| format!("{b:02x}")).collect())
    }
}

// ---------------------------------------------------------------- check

fn fetch_latest() -> Result<Option<Release>, String> {
    let mut body = Vec::new();
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let status = https_get(&url, "application/vnd.github+json", |chunk, _| {
        body.extend_from_slice(chunk);
        if body.len() > 4 * 1024 * 1024 { Err("слишком большой ответ".into()) } else { Ok(()) }
    })?;
    if status == 404 {
        return Ok(None); // no releases published yet
    }
    if status != 200 {
        return Err(format!("GitHub ответил {status}"));
    }
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().unwrap_or("").to_string();
    let version = parse_version(&tag).ok_or_else(|| format!("непонятная версия «{tag}»"))?;
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let setup = assets.iter().find(|a| a["name"].as_str() == Some(ASSET)).ok_or("в релизе нет установщика")?;
    let mut sha256 = setup["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(|s| s.to_ascii_lowercase());
    if sha256.is_none() {
        // Fallback: SHA256SUMS.txt published next to the setup.
        if let Some(sums) = assets.iter().find(|a| a["name"].as_str() == Some("SHA256SUMS.txt")) {
            let mut txt = Vec::new();
            let u = sums["browser_download_url"].as_str().unwrap_or("");
            if https_get(u, "application/octet-stream", |c, _| {
                txt.extend_from_slice(c);
                Ok(())
            })
            .ok()
                == Some(200)
            {
                sha256 = String::from_utf8_lossy(&txt)
                    .lines()
                    .find(|l| l.contains(ASSET))
                    .and_then(|l| l.split_whitespace().next())
                    .map(|s| s.to_ascii_lowercase());
            }
        }
    }
    let r = Release {
        tag,
        version,
        notes: v["body"].as_str().unwrap_or("").to_string(),
        url: setup["browser_download_url"].as_str().unwrap_or("").to_string(),
        size: setup["size"].as_u64().unwrap_or(0),
        sha256,
        page: v["html_url"].as_str().unwrap_or("").to_string(),
        published: v["published_at"].as_str().unwrap_or("").to_string(),
    };
    Ok(Some(r))
}

pub fn start_check() {
    std::thread::spawn(|| {
        let r = fetch_latest();
        if let Ok(mut q) = INBOX.lock() {
            q.push(Msg::Checked(r));
        }
        crate::events::kick();
    });
}

pub fn start_download(rel: Release) {
    DL_RECEIVED.store(0, Ordering::SeqCst);
    DL_TOTAL.store(rel.size, Ordering::SeqCst);
    std::thread::spawn(move || {
        let r = download(&rel).map(|p| (rel.clone(), p));
        if let Ok(mut q) = INBOX.lock() {
            q.push(Msg::Downloaded(r));
        }
        crate::events::kick();
    });
}

fn download(rel: &Release) -> Result<PathBuf, String> {
    if !rel.url.starts_with("https://github.com/") && !rel.url.starts_with("https://objects.githubusercontent.com/") {
        return Err("неожиданный адрес загрузки".into());
    }
    let mut data = Vec::with_capacity(rel.size as usize);
    let status = https_get(&rel.url, "application/octet-stream", |chunk, total| {
        data.extend_from_slice(chunk);
        DL_RECEIVED.store(data.len() as u64, Ordering::Relaxed);
        if total > 0 {
            DL_TOTAL.store(total, Ordering::Relaxed);
        }
        if data.len() > 200 * 1024 * 1024 { Err("файл слишком большой".into()) } else { Ok(()) }
    })?;
    if status != 200 {
        return Err(format!("сервер ответил {status}"));
    }
    if rel.size > 0 && data.len() as u64 != rel.size {
        return Err(format!("размер не совпал: {} из {} байт", data.len(), rel.size));
    }
    let got = sha256_hex(&data).ok_or("не удалось посчитать SHA-256")?;
    match &rel.sha256 {
        Some(want) if *want == got => {}
        Some(want) => return Err(format!("контрольная сумма не совпала (ожидалась {want}, получена {got})")),
        None => return Err("у релиза нет контрольной суммы SHA-256 — установка отменена".into()),
    }
    if data.len() < 2 || &data[..2] != b"MZ" {
        return Err("загруженный файл не является программой Windows".into());
    }
    let dir = std::env::temp_dir().join("RealismBrowserUpdate");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("Realism-Browser-Setup-{}.exe", rel.tag.trim_start_matches('v')));
    std::fs::write(&path, &data).map_err(|e| format!("не удалось сохранить файл: {e}"))?;
    Ok(path)
}

/// Results from worker threads (called on the UI thread).
pub fn poll(state: &mut State) -> bool {
    let msgs: Vec<Msg> = INBOX.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
    let changed = !msgs.is_empty();
    for m in msgs {
        *state = match m {
            Msg::Checked(Ok(Some(r))) if r.version > current() => State::Available(r),
            Msg::Checked(Ok(Some(r))) => State::UpToDate { latest: r.tag },
            Msg::Checked(Ok(None)) => State::UpToDate { latest: format!("v{VERSION}") },
            Msg::Checked(Err(e)) => State::Error(format!("Не удалось проверить обновления: {e}")),
            Msg::Downloaded(Ok((r, p))) => State::Ready(r, p),
            Msg::Downloaded(Err(e)) => State::Error(format!("Не удалось скачать обновление: {e}")),
        };
    }
    changed
}

/// Starts the downloaded setup in update mode. The caller must then exit the browser.
pub fn launch_installer(setup: &std::path::Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("нет папки программы")?;
    std::process::Command::new(setup)
        .args(["--silent", "--update", "--relaunch", "--dir"])
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("не удалось запустить установщик: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.10"), Some((1, 10, 0)));
        assert_eq!(parse_version("v2.0.0-beta.1"), Some((2, 0, 0)));
        assert!(parse_version("v1.10.0") > parse_version("v1.9.9"));
        assert_eq!(parse_version("abc"), None);
    }
    /// Network test against the real GitHub release: `cargo test --bin realism -- --ignored live_release`
    #[test]
    #[ignore]
    fn live_release() {
        let rel = fetch_latest().expect("check").expect("a published release");
        assert!(rel.sha256.is_some(), "release must carry a SHA-256");
        let path = download(&rel).expect("download + verification");
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len() as u64, rel.size);
        assert_eq!(sha256_hex(&data), rel.sha256.clone());
        println!("downloaded {} -> {}", rel.tag, path.display());
    }

    #[test]
    fn hash() {
        assert_eq!(sha256_hex(b"abc").unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
