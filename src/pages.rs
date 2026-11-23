//! realism:// internal pages (served via a WebView2 custom protocol) and their JSON API.
//!
//! Security: every API call needs the per-launch random token that is only embedded in pages we serve
//! ourselves. Other sites cannot read our pages (no CORS headers) so they cannot learn the token;
//! pages also refuse to be framed.

use std::borrow::Cow;

use serde_json::{json, Value};

use crate::app::{App, DlState, EnvState, Mode};
use crate::db::V;
use crate::events::ProtoReq;
use crate::util::*;

const COMMON_CSS: &str = include_str!("../assets/common.css");
const COMMON_JS: &str = include_str!("../assets/common.js");
const FAVICON: &str = include_str!("../assets/favicon.svg");

fn page(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "settings" => (include_str!("../assets/settings.html"), "Настройки"),
        "history" => (include_str!("../assets/history.html"), "История"),
        "bookmarks" => (include_str!("../assets/bookmarks.html"), "Закладки"),
        "downloads" => (include_str!("../assets/downloads.html"), "Загрузки"),
        "performance" => (include_str!("../assets/performance.html"), "Производительность"),
        "about" => (include_str!("../assets/about.html"), "О программе"),
        _ => return None,
    })
}

fn script(name: &str) -> Option<&'static str> {
    Some(match name {
        "settings.js" => include_str!("../assets/settings.js"),
        "history.js" => include_str!("../assets/history.js"),
        "bookmarks.js" => include_str!("../assets/bookmarks.js"),
        "downloads.js" => include_str!("../assets/downloads.js"),
        "performance.js" => include_str!("../assets/performance.js"),
        "about.js" => include_str!("../assets/about.js"),
        "common.js" => COMMON_JS,
        _ => return None,
    })
}

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

pub fn respond(resp: wry::RequestAsyncResponder, status: u16, ctype: &str, body: Vec<u8>) {
    let r = http::Response::builder()
        .status(status)
        .header("Content-Type", ctype)
        .header("Cache-Control", "no-store")
        .header("X-Frame-Options", "DENY")
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Security-Policy", CSP)
        .body(Cow::Owned(body))
        .unwrap_or_else(|_| http::Response::new(Cow::Borrowed(&b""[..])));
    resp.respond(r);
}

pub fn respond_json(resp: wry::RequestAsyncResponder, v: &Value) {
    respond(resp, 200, "application/json; charset=utf-8", serde_json::to_vec(v).unwrap_or_default());
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0)
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

impl App {
    pub fn update_json(&self) -> Value {
        use crate::update::State;
        let rel = |r: &crate::update::Release| json!({ "tag": r.tag, "notes": r.notes, "page": r.page, "size": r.size, "published": r.published });
        let (state, release, error) = match &self.upd {
            State::Idle => ("idle", Value::Null, Value::Null),
            State::Checking => ("checking", Value::Null, Value::Null),
            State::UpToDate { latest } => ("uptodate", json!({ "tag": latest }), Value::Null),
            State::Available(r) => ("available", rel(r), Value::Null),
            State::Downloading(r) => ("downloading", rel(r), Value::Null),
            State::Ready(r, _) => ("ready", rel(r), Value::Null),
            State::Error(e) => ("error", Value::Null, json!(e)),
        };
        let last: i64 = self.db.get_setting("update_last_check").and_then(|v| v.parse().ok()).unwrap_or(0);
        json!({
            "current": crate::update::VERSION,
            "state": state, "release": release, "error": error,
            "received": crate::update::DL_RECEIVED.load(std::sync::atomic::Ordering::Relaxed),
            "total": crate::update::DL_TOTAL.load(std::sync::atomic::Ordering::Relaxed),
            "auto": self.db.get_setting("update_auto").as_deref() != Some("0"),
            "last_check": last,
            "repo": crate::update::REPO,
            "downloads_running": self.downloads.iter().any(|d| d.state == DlState::Running),
        })
    }

    pub fn handle_proto(&mut self, r: ProtoReq) {
        let uri = r.req.uri().clone();
        let path = uri.path().trim_start_matches('/').to_string();
        let query = uri.query().unwrap_or("").to_string();
        if let Some(api) = path.strip_prefix("api/") {
            let tok = r.req.headers().get("x-realism-token").and_then(|v| v.to_str().ok()).unwrap_or("");
            if tok != self.token {
                respond(r.responder, 403, "text/plain", b"forbidden".to_vec());
                return;
            }
            let body: Value = serde_json::from_slice(r.req.body()).unwrap_or(Value::Null);
            let tab: u64 = r.webview_id.parse().unwrap_or(0);
            self.api(api, &query, body, tab, r.responder);
            return;
        }
        match path.as_str() {
            "common.css" => respond(r.responder, 200, "text/css; charset=utf-8", COMMON_CSS.as_bytes().to_vec()),
            "favicon.svg" => respond(r.responder, 200, "image/svg+xml", FAVICON.as_bytes().to_vec()),
            p if p.ends_with(".js") => match script(p) {
                Some(js) => respond(r.responder, 200, "text/javascript; charset=utf-8", js.as_bytes().to_vec()),
                None => respond(r.responder, 404, "text/plain", b"not found".to_vec()),
            },
            p => {
                let name = p.split('/').next().unwrap_or("");
                match page(name) {
                    Some((html, title)) => {
                        let dark = if self.theme.dark { "dark" } else { "light" };
                        let out = html.replace("__TOKEN__", &self.token).replace("__TITLE__", title).replace("__THEME__", dark);
                        respond(r.responder, 200, "text/html; charset=utf-8", out.into_bytes());
                    }
                    None => respond(r.responder, 404, "text/html; charset=utf-8", b"<h1>404</h1>".to_vec()),
                }
            }
        }
    }

    fn api(&mut self, api: &str, query: &str, body: Value, tab: u64, resp: wry::RequestAsyncResponder) {
        let q = parse_query(query);
        let qget = |k: &str| q.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone()).unwrap_or_default();
        let wi = self.loc(tab).map(|x| x.0).unwrap_or(0);
        let private = self.windows.get(wi).map(|w| w.private).unwrap_or(false);
        let out: Value = match api {
            // ---------------------------------------------------------------- performance
            "perf" => {
                self.begin_perf(resp);
                return;
            }
            "perf/free" => {
                let n = self.free_memory();
                json!({ "ok": true, "actions": n })
            }
            "tab" => {
                let id = i(&body, "id") as u64;
                match s(&body, "action").as_str() {
                    "sleep" => {
                        if let Some((w, t)) = self.loc(id) {
                            if self.windows[w].active != t || self.windows[w].minimized {
                                if self.can_discard(w, t).is_ok() {
                                    self.discard(w, t);
                                } else if let Some(wv) = &self.windows[w].tabs[t].wv {
                                    crate::webview::set_low_memory(&wv.core, true);
                                    crate::webview::try_suspend(id, wv, true);
                                }
                            }
                        }
                    }
                    "activate" => {
                        if let Some((w, t)) = self.loc(id) {
                            self.activate(w, t);
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(self.windows[w].hwnd);
                            }
                        }
                    }
                    "close" => self.close_tab_id(id),
                    _ => {}
                }
                json!({ "ok": true })
            }
            // ---------------------------------------------------------------- history
            "history" => {
                let term = qget("q");
                let before: i64 = qget("before").parse().unwrap_or(i64::MAX);
                let like = format!("%{}%", term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
                let rows = self.db.query(
                    "SELECT id,url,title,visits,last_visit FROM history WHERE last_visit < ? AND (url LIKE ?2 ESCAPE '\\' OR title LIKE ?2 ESCAPE '\\') ORDER BY last_visit DESC LIMIT 150",
                    &[V::I(before), V::T(&like)],
                    |r| json!({ "id": r.int(0), "url": r.text(1), "title": r.text(2), "visits": r.int(3), "last": r.int(4) }),
                );
                json!(rows)
            }
            "history/delete" => {
                if let Some(ids) = body.get("ids").and_then(|v| v.as_array()) {
                    let _ = self.db.exec("BEGIN");
                    for id in ids.iter().filter_map(|v| v.as_i64()) {
                        self.db.run("DELETE FROM history WHERE id=?", &[V::I(id)]);
                    }
                    let _ = self.db.exec("COMMIT");
                }
                json!({ "ok": true })
            }
            "history/clear" => {
                self.db.run("DELETE FROM history", &[]);
                json!({ "ok": true })
            }
            // ---------------------------------------------------------------- bookmarks
            "bookmarks" => {
                let rows = self.db.query("SELECT id,parent,is_folder,title,url,pos FROM bookmarks ORDER BY parent,pos,id", &[], |r| {
                    json!({ "id": r.int(0), "parent": r.int(1), "folder": r.int(2) != 0, "title": r.text(3), "url": r.text(4), "pos": r.int(5) })
                });
                json!(rows)
            }
            "bookmarks/add" => {
                let parent = i(&body, "parent").max(1);
                let pos = self.db.query("SELECT COALESCE(MAX(pos),-1)+1 FROM bookmarks WHERE parent=?", &[V::I(parent)], |r| r.int(0))[0];
                let mut url = s(&body, "url");
                let folder = b(&body, "folder");
                if !folder {
                    url = normalize_input(&url, &self.s.search).unwrap_or_default();
                }
                self.db.run(
                    "INSERT INTO bookmarks(parent,is_folder,title,url,pos,added) VALUES(?,?,?,?,?,?)",
                    &[V::I(parent), V::I(folder as i64), V::T(&s(&body, "title")), V::T(&url), V::I(pos), V::I(now_ms())],
                );
                self.refresh_bookmarks_bar();
                json!({ "ok": true, "id": self.db.last_id() })
            }
            "bookmarks/update" => {
                let id = i(&body, "id");
                if id > 2 {
                    let url = s(&body, "url");
                    let url = if url.is_empty() { url } else { normalize_input(&url, &self.s.search).unwrap_or_default() };
                    self.db.run("UPDATE bookmarks SET title=?, url=CASE WHEN is_folder=1 THEN url ELSE ? END WHERE id=?", &[V::T(&s(&body, "title")), V::T(&url), V::I(id)]);
                }
                self.refresh_bookmarks_bar();
                json!({ "ok": true })
            }
            "bookmarks/delete" => {
                let id = i(&body, "id");
                if id > 2 {
                    // Recursive delete of a folder subtree.
                    let mut stack = vec![id];
                    let _ = self.db.exec("BEGIN");
                    while let Some(x) = stack.pop() {
                        let kids = self.db.query("SELECT id FROM bookmarks WHERE parent=?", &[V::I(x)], |r| r.int(0));
                        stack.extend(kids);
                        self.db.run("DELETE FROM bookmarks WHERE id=?", &[V::I(x)]);
                    }
                    let _ = self.db.exec("COMMIT");
                }
                self.refresh_bookmarks_bar();
                json!({ "ok": true })
            }
            "bookmarks/move" => {
                let id = i(&body, "id");
                let parent = i(&body, "parent");
                let pos = i(&body, "pos");
                // Refuse moving a folder into itself / its own subtree.
                let mut p = parent;
                let mut ok = id > 2 && parent >= 1;
                while ok && p > 0 {
                    if p == id {
                        ok = false;
                    }
                    p = self.db.query("SELECT parent FROM bookmarks WHERE id=?", &[V::I(p)], |r| r.int(0)).first().copied().unwrap_or(0);
                }
                if ok {
                    let _ = self.db.exec("BEGIN");
                    let mut sib = self.db.query("SELECT id FROM bookmarks WHERE parent=? AND id<>? ORDER BY pos,id", &[V::I(parent), V::I(id)], |r| r.int(0));
                    sib.insert((pos.max(0) as usize).min(sib.len()), id);
                    for (n, sid) in sib.iter().enumerate() {
                        self.db.run("UPDATE bookmarks SET parent=?, pos=? WHERE id=?", &[V::I(parent), V::I(n as i64), V::I(*sid)]);
                    }
                    let _ = self.db.exec("COMMIT");
                }
                self.refresh_bookmarks_bar();
                json!({ "ok": ok })
            }
            // ---------------------------------------------------------------- downloads
            "downloads" => {
                self.poll_downloads(None);
                let list: Vec<Value> = self
                    .downloads
                    .iter()
                    .filter(|d| !d.private || private)
                    .map(|d| {
                        let name = std::path::Path::new(&d.path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        json!({
                            "id": d.id, "name": name, "path": d.path, "url": d.url, "total": d.total, "received": d.received,
                            "state": d.state.key(), "speed": d.speed as i64, "started": d.started,
                            "can_resume": d.op.is_some() && d.state == DlState::Paused,
                            "exists": std::path::Path::new(&d.path).exists(),
                        })
                    })
                    .collect();
                json!({ "items": list, "dir": self.download_dir().to_string_lossy() })
            }
            "downloads/action" => {
                let ok = self.download_action(i(&body, "id") as u64, &s(&body, "action"));
                json!({ "ok": ok })
            }
            "downloads/clear" => {
                self.db.run("DELETE FROM downloads", &[]);
                self.downloads.retain(|d| matches!(d.state, DlState::Running | DlState::Paused));
                json!({ "ok": true })
            }
            // ---------------------------------------------------------------- settings
            "settings" => {
                let modes: Vec<Value> = [Mode::Saver, Mode::Balanced, Mode::Performance]
                    .iter()
                    .map(|m| {
                        let p = m.policy();
                        json!({ "key": m.key(), "label": m.label(),
                            "suspend_after": p.suspend_after.as_secs(), "discard_after": p.discard_after.map(|d| d.as_secs()),
                            "max_live_bg": if p.max_live_bg == usize::MAX { None } else { Some(p.max_live_bg) },
                            "low_mem_on_hide": p.low_mem_on_hide, "low_mem_after": p.low_mem_after.map(|d| d.as_secs()),
                            "process_per_site": p.process_per_site, "no_spare_renderer": !p.prewarm,
                            "engine_idle": p.env_idle.map(|d| d.as_secs()) })
                    })
                    .collect();
                let perms = self.db.query("SELECT origin,kind,state FROM permissions ORDER BY origin", &[], |r| {
                    let k = r.int(1) as i32;
                    json!({ "origin": r.text(0), "kind": k, "label": crate::ui::perm_label(k), "state": r.int(2) })
                });
                let popups = self.db.query("SELECT origin FROM popups_allowed ORDER BY origin", &[], |r| r.text(0));
                json!({
                    "theme": self.s.theme, "search": self.s.search, "mode": self.s.mode.key(), "startup": self.s.startup,
                    "download_dir": self.download_dir().to_string_lossy(), "custom_download_dir": !self.s.download_dir.is_empty(),
                    "ask_download": self.s.ask_download, "bookmarks_bar": self.s.bookmarks_bar,
                    "exceptions": self.s.exceptions, "permissions": perms, "popups": popups,
                    "data_dir": self.data_dir.to_string_lossy(),
                    "engine_restart_needed": self.engine_restart_needed(),
                    "modes": modes,
                })
            }
            "settings/set" => {
                let key = s(&body, "key");
                let val = body.get("value").cloned().unwrap_or(Value::Null);
                match key.as_str() {
                    "theme" => {
                        let t = val.as_str().unwrap_or("dark");
                        if ["dark", "light", "system"].contains(&t) {
                            self.db.set_setting("theme", t);
                            self.set_theme(t);
                        }
                    }
                    "search" => {
                        let v = val.as_str().unwrap_or("google");
                        if ["google", "bing", "duckduckgo"].contains(&v) {
                            self.s.search = v.into();
                            self.db.set_setting("search", v);
                        }
                    }
                    "mode" => self.set_mode(Mode::parse(val.as_str().unwrap_or(""))),
                    "startup" => {
                        let v = if val.as_str() == Some("newtab") { "newtab" } else { "restore" };
                        self.s.startup = v.into();
                        self.db.set_setting("startup", v);
                    }
                    "ask_download" => {
                        self.s.ask_download = val.as_bool().unwrap_or(false);
                        self.db.set_setting("ask_download", if self.s.ask_download { "1" } else { "0" });
                    }
                    "bookmarks_bar" => {
                        self.s.bookmarks_bar = val.as_bool().unwrap_or(false);
                        self.db.set_setting("bookmarks_bar", if self.s.bookmarks_bar { "1" } else { "0" });
                        self.refresh_bookmarks_bar();
                    }
                    "exceptions" => {
                        let list: Vec<String> = val
                            .as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect())
                            .unwrap_or_default();
                        self.db.set_setting("exceptions", &serde_json::to_string(&list).unwrap_or_default());
                        self.s.exceptions = list;
                    }
                    "download_dir_reset" => {
                        self.s.download_dir.clear();
                        self.db.set_setting("download_dir", "");
                    }
                    _ => {}
                }
                json!({ "ok": true })
            }
            "settings/pick-dir" => {
                self.ui_tasks.push(crate::app::UiTask::PickDownloadDir);
                json!({ "ok": true })
            }
            "permissions/delete" => {
                self.db.run("DELETE FROM permissions WHERE origin=? AND kind=?", &[V::T(&s(&body, "origin")), V::I(i(&body, "kind"))]);
                json!({ "ok": true })
            }
            "popups/delete" => {
                self.db.run("DELETE FROM popups_allowed WHERE origin=?", &[V::T(&s(&body, "origin"))]);
                json!({ "ok": true })
            }
            "clear-data" => {
                if b(&body, "history") {
                    self.db.run("DELETE FROM history", &[]);
                }
                if b(&body, "downloads") {
                    self.db.run("DELETE FROM downloads", &[]);
                    self.downloads.retain(|d| matches!(d.state, DlState::Running | DlState::Paused));
                }
                use webview2_com::Microsoft::Web::WebView2::Win32::*;
                let mut kinds = 0i32;
                if b(&body, "cookies") {
                    kinds |= COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES.0 | COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE.0;
                }
                if b(&body, "cache") {
                    kinds |= COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0;
                }
                if b(&body, "passwords") {
                    kinds |= COREWEBVIEW2_BROWSING_DATA_KINDS_GENERAL_AUTOFILL.0 | COREWEBVIEW2_BROWSING_DATA_KINDS_PASSWORD_AUTOSAVE.0;
                }
                if kinds != 0 {
                    // Use any live webview of the normal profile (the settings page itself, usually).
                    let core = self
                        .windows
                        .iter()
                        .filter(|w| !w.private)
                        .flat_map(|w| w.tabs.iter())
                        .find_map(|t| t.wv.as_ref().map(|wv| wv.core.clone()));
                    if let Some(core) = core {
                        let req = self.new_id();
                        self.pending_api.insert(req, (resp, "clear".into()));
                        if !crate::webview::clear_data(&core, kinds, req) {
                            if let Some((resp, _)) = self.pending_api.remove(&req) {
                                respond_json(resp, &json!({ "ok": false }));
                            }
                        }
                        return;
                    }
                }
                json!({ "ok": true })
            }
            "open" => {
                let url = s(&body, "url");
                if let Some(u) = normalize_input(&url, &self.s.search) {
                    if wi < self.windows.len() {
                        if b(&body, "newtab") {
                            self.add_tab(wi, Some(u), None, true);
                        } else if let Some((w, t)) = self.loc(tab) {
                            self.navigate_tab(w, t, Some(u));
                        }
                    }
                }
                json!({ "ok": true })
            }
            "update" => self.update_json(),
            "update/check" => {
                self.check_updates();
                self.update_json()
            }
            "update/download" => {
                if let crate::update::State::Available(r) = &self.upd {
                    let r = r.clone();
                    self.upd = crate::update::State::Downloading(r.clone());
                    crate::update::start_download(r);
                }
                self.update_json()
            }
            "update/install" => {
                if let crate::update::State::Ready(_, path) = &self.upd {
                    let path = path.clone();
                    match crate::update::launch_installer(&path) {
                        Ok(()) => {
                            // The setup waits for us to exit, installs and starts the new version.
                            self.save_session();
                            for w in &self.windows {
                                unsafe {
                                    let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                                        Some(w.hwnd),
                                        windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                                        windows::Win32::Foundation::WPARAM(0),
                                        windows::Win32::Foundation::LPARAM(1),
                                    );
                                }
                            }
                        }
                        Err(e) => self.upd = crate::update::State::Error(e),
                    }
                }
                self.update_json()
            }
            "update/auto" => {
                self.db.set_setting("update_auto", if b(&body, "on") { "1" } else { "0" });
                self.update_json()
            }
            "about" => {
                json!({
                    "version": env!("CARGO_PKG_VERSION"),
                    "webview2": if self.webview2_version.is_empty() { crate::webview::runtime_version() } else { self.webview2_version.clone() },
                    "data_dir": self.data_dir.to_string_lossy(),
                    "exe": std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
                    "engine": match &self.env { EnvState::Ready(_) => "running", EnvState::Creating => "starting", EnvState::None => "stopped" },
                })
            }
            _ => {
                respond(resp, 404, "application/json", b"{\"error\":\"unknown api\"}".to_vec());
                return;
            }
        };
        respond_json(resp, &out);
    }
}
