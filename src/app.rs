//! Application state: windows, tabs, webview lifecycle, navigation, downloads, permissions, session.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{take_pwstr, StateChangedEventHandler};
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cmd::Cmd;
use crate::db::{Db, V};
use crate::events::{self, Ev};
use crate::gfx::Gfx;
use crate::theme::{self, Theme};
use crate::util::*;
use crate::webview::{self, Wv};

pub const TIMER_TICK: usize = 1;
pub const TIMER_ANIM: usize = 2;
pub const TIMER_DL: usize = 3;
pub const TIMER_SESSION: usize = 4;

thread_local! {
    pub static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static ANY_FULLSCREEN: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with the app if it is not already borrowed (re-entrancy guard).
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| match a.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

pub fn tab_in_browser_fullscreen(_tab: u64) -> bool {
    ANY_FULLSCREEN.with(|c| c.get())
}

// ------------------------------------------------------------------ settings / policy

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Saver,
    Balanced,
    Performance,
}

impl Mode {
    pub fn key(self) -> &'static str {
        match self {
            Mode::Saver => "saver",
            Mode::Balanced => "balanced",
            Mode::Performance => "performance",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Saver => "Экономия памяти",
            Mode::Balanced => "Сбалансированный",
            Mode::Performance => "Максимальная производительность",
        }
    }
    pub fn parse(s: &str) -> Mode {
        match s {
            "saver" => Mode::Saver,
            "performance" => Mode::Performance,
            _ => Mode::Balanced,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    /// Lower the renderer memory target as soon as a tab goes to the background.
    pub low_mem_on_hide: bool,
    /// Lower the renderer memory target after the tab has been hidden this long (None = only when frozen).
    pub low_mem_after: Option<Duration>,
    /// Freeze (TrySuspend) a hidden tab after this long.
    pub suspend_after: Duration,
    /// Unload the tab completely after this long (None = never).
    pub discard_after: Option<Duration>,
    /// Keep at most this many live background webviews per browser (LRU discard).
    pub max_live_bg: usize,
    /// Shut the whole WebView2 engine down when no webview has been alive for this long.
    pub env_idle: Option<Duration>,
    /// Start the engine at launch even if the first tab is the native new-tab page.
    pub prewarm: bool,
    /// Same-site tabs share one renderer process.
    pub process_per_site: bool,
}

impl Mode {
    pub fn policy(self) -> Policy {
        match self {
            Mode::Saver => Policy {
                low_mem_on_hide: true,
                low_mem_after: Some(Duration::ZERO),
                suspend_after: Duration::from_secs(20),
                discard_after: Some(Duration::from_secs(5 * 60)),
                max_live_bg: 3,
                env_idle: Some(Duration::from_secs(20)),
                prewarm: false,
                process_per_site: true,
            },
            Mode::Balanced => Policy {
                low_mem_on_hide: false,
                low_mem_after: Some(Duration::from_secs(10)),
                suspend_after: Duration::from_secs(2 * 60),
                discard_after: Some(Duration::from_secs(30 * 60)),
                max_live_bg: 12,
                env_idle: Some(Duration::from_secs(120)),
                prewarm: false,
                process_per_site: false,
            },
            Mode::Performance => Policy {
                low_mem_on_hide: false,
                low_mem_after: None,
                suspend_after: Duration::from_secs(30 * 60),
                discard_after: None,
                max_live_bg: usize::MAX,
                env_idle: None,
                prewarm: true,
                process_per_site: false,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub theme: String,
    pub search: String,
    pub mode: Mode,
    pub startup: String,
    pub download_dir: String,
    pub ask_download: bool,
    pub bookmarks_bar: bool,
    pub exceptions: Vec<String>,
}

impl Settings {
    pub fn load(db: &Db) -> Settings {
        let g = |k: &str, d: &str| db.get_setting(k).unwrap_or_else(|| d.to_string());
        Settings {
            theme: g("theme", "dark"),
            search: g("search", "google"),
            mode: Mode::parse(&g("mode", "balanced")),
            startup: g("startup", "restore"),
            download_dir: g("download_dir", ""),
            ask_download: g("ask_download", "0") == "1",
            bookmarks_bar: g("bookmarks_bar", "0") == "1",
            exceptions: serde_json::from_str(&g("exceptions", "[]")).unwrap_or_default(),
        }
    }
}

// ------------------------------------------------------------------ data types

pub enum EnvState {
    None,
    Creating,
    Ready(ICoreWebView2Environment),
}

pub struct PermPrompt {
    pub args: ICoreWebView2PermissionRequestedEventArgs,
    pub deferral: ICoreWebView2Deferral,
    pub origin: String,
    pub kind: i32,
}

#[derive(Clone)]
pub enum Info {
    PopupBlocked { origin: String, url: String },
    Crashed,
    Message(String),
}

pub struct Tab {
    pub id: u64,
    pub url: String,
    pub title: String,
    pub icon: Option<Rc<Vec<u8>>>,
    pub wv: Option<Wv>,
    pub loading: bool,
    pub load_start: Instant,
    pub committed: bool,
    pub can_back: bool,
    pub can_fwd: bool,
    pub audio: bool,
    pub muted: bool,
    pub suspended: bool,
    pub low_mem: bool,
    pub dirty_input: bool,
    pub media_active: bool,
    pub cert_error: bool,
    pub hidden_since: Option<Instant>,
    pub last_active: Instant,
    pub zoom: f64,
    pub zoom_host: String,
    pub back_stack: Vec<String>,
    pub came_from_ntp: bool,
    pub pending_nav: Option<String>,
    pub perms: Vec<PermPrompt>,
    pub info: Option<Info>,
    pub opener: Option<u64>,
    /// The webview has been visible at least once (WebView2 only suspends such pages).
    pub shown: bool,
    /// Last failed suspend attempt (WebView2 refuses e.g. while media/WebRTC is active): retry later.
    pub suspend_failed: Option<Instant>,
    pub x_anim: f32,
    pub created: Instant,
    /// Committed navigation history we track ourselves (survives discarding).
    pub hist: Vec<String>,
    pub hist_idx: usize,
}

impl Tab {
    pub fn new(id: u64, url: String) -> Tab {
        Tab {
            id,
            url,
            title: String::new(),
            icon: None,
            wv: None,
            loading: false,
            load_start: Instant::now(),
            committed: false,
            can_back: false,
            can_fwd: false,
            audio: false,
            muted: false,
            suspended: false,
            low_mem: false,
            dirty_input: false,
            media_active: false,
            cert_error: false,
            hidden_since: Some(Instant::now()),
            last_active: Instant::now(),
            zoom: 1.0,
            zoom_host: String::new(),
            back_stack: Vec::new(),
            came_from_ntp: false,
            pending_nav: None,
            perms: Vec::new(),
            info: None,
            opener: None,
            shown: false,
            suspend_failed: None,
            x_anim: f32::NAN,
            created: Instant::now(),
            hist: Vec::new(),
            hist_idx: 0,
        }
    }
    pub fn is_ntp(&self) -> bool {
        self.wv.is_none() && self.url.is_empty() && self.pending_nav.is_none()
    }
    pub fn is_discarded(&self) -> bool {
        self.wv.is_none() && (!self.url.is_empty() || self.pending_nav.is_some())
    }
    pub fn sleeping(&self) -> bool {
        self.suspended || (self.wv.is_none() && !self.url.is_empty())
    }
    pub fn display_title(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        if self.url.is_empty() {
            return "Новая вкладка".into();
        }
        let d = display_url(&self.url);
        if d.is_empty() { "Новая вкладка".into() } else { d }
    }
}

pub struct ClosedTab {
    pub url: String,
    pub title: String,
    pub icon: Option<Rc<Vec<u8>>>,
    pub win: u64,
    pub index: usize,
    pub back: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DlState {
    Running,
    Paused,
    Done,
    Failed,
    Canceled,
}

impl DlState {
    pub fn key(self) -> &'static str {
        match self {
            DlState::Running => "running",
            DlState::Paused => "paused",
            DlState::Done => "done",
            DlState::Failed => "failed",
            DlState::Canceled => "canceled",
        }
    }
}

pub struct Download {
    pub id: u64,
    pub db_id: i64,
    pub op: Option<ICoreWebView2DownloadOperation>,
    pub url: String,
    pub path: String,
    pub total: i64,
    pub received: i64,
    pub state: DlState,
    pub tab: u64,
    pub private: bool,
    pub started: i64,
    pub speed: f64,
    pub last_sample: (Instant, i64),
}

pub enum UiTask {
    ConfirmClose(HWND),
    PickDownloadDir,
    SaveAs { args: ICoreWebView2DownloadStartingEventArgs, deferral: ICoreWebView2Deferral, tab: u64, suggested: String },
}

pub struct Args {
    pub profile_dir: Option<PathBuf>,
    pub urls: Vec<String>,
    pub private: bool,
    pub no_restore: bool,
    pub bench: bool,
    pub mode: Option<Mode>,
    pub window_size: Option<(i32, i32)>,
}

pub struct App {
    pub db: Db,
    pub s: Settings,
    pub gfx: Gfx,
    pub theme: Theme,
    pub data_dir: PathBuf,
    pub env: EnvState,
    pub env_mode: Mode,
    pub env_idle_since: Option<Instant>,
    pub windows: Vec<crate::ui::Win>,
    pub popup: Option<crate::popup::Popup>,
    pub next_id: u64,
    pub closed: Vec<ClosedTab>,
    pub downloads: Vec<Download>,
    pub msg_hwnd: HWND,
    pub token: String,
    pub cpu_prev: HashMap<u32, u64>,
    pub cpu_prev_at: Option<Instant>,
    pub zombies: Vec<(u64, Wv)>,
    pub pending_api: HashMap<u64, (wry::RequestAsyncResponder, String)>,
    pub ram_total: u64,
    pub ui_tasks: Vec<UiTask>,
    pub session_dirty: bool,
    pub args: Args,
    pub private_perms: HashMap<(String, i32), i32>,
    pub anim_on: bool,
    pub dl_timer_on: bool,
    pub quitting: bool,
    pub webview2_version: String,
    pub upd: crate::update::State,
}

impl App {
    pub fn new(args: Args, data_dir: PathBuf, msg_hwnd: HWND) -> Result<App, String> {
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let db = Db::open(&data_dir.join("realism.db"))?;
        let mut s = Settings::load(&db);
        if let Some(m) = args.mode {
            s.mode = m;
        }
        let gfx = Gfx::new().map_err(|e| e.to_string())?;
        let theme = Self::resolve_theme(&s.theme);
        let mode = s.mode;
        Ok(App {
            db,
            s,
            gfx,
            theme,
            data_dir,
            env: EnvState::None,
            env_mode: mode,
            env_idle_since: None,
            windows: Vec::new(),
            popup: None,
            next_id: 1,
            closed: Vec::new(),
            downloads: Vec::new(),
            msg_hwnd,
            token: random_token(),
            cpu_prev: HashMap::new(),
            cpu_prev_at: None,
            zombies: Vec::new(),
            pending_api: HashMap::new(),
            ram_total: 0,
            ui_tasks: Vec::new(),
            session_dirty: false,
            args,
            private_perms: HashMap::new(),
            anim_on: false,
            dl_timer_on: false,
            quitting: false,
            webview2_version: String::new(),
            upd: crate::update::State::Idle,
        })
    }

    pub fn resolve_theme(t: &str) -> Theme {
        match t {
            "light" => theme::LIGHT,
            "system" => {
                if theme::system_prefers_dark() { theme::DARK } else { theme::LIGHT }
            }
            _ => theme::DARK,
        }
    }

    pub fn scheme(&self) -> i32 {
        match self.s.theme.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        }
    }

    pub fn policy(&self) -> Policy {
        self.s.mode.policy()
    }

    pub fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn loc(&self, tab: u64) -> Option<(usize, usize)> {
        for (wi, w) in self.windows.iter().enumerate() {
            if let Some(ti) = w.tabs.iter().position(|t| t.id == tab) {
                return Some((wi, ti));
            }
        }
        None
    }

    pub fn tab_mut(&mut self, tab: u64) -> Option<&mut Tab> {
        let (wi, ti) = self.loc(tab)?;
        Some(&mut self.windows[wi].tabs[ti])
    }

    pub fn win_index(&self, hwnd: HWND) -> Option<usize> {
        self.windows.iter().position(|w| w.hwnd == hwnd)
    }

    pub fn live_webviews(&self) -> usize {
        self.windows.iter().flat_map(|w| w.tabs.iter()).filter(|t| t.wv.is_some()).count() + self.zombies.len()
    }

    // ------------------------------------------------------------------ engine

    pub fn start_env(&mut self) {
        if !matches!(self.env, EnvState::None) {
            return;
        }
        let pol = self.policy();
        self.env_mode = self.s.mode;
        let args = webview::browser_args(pol.process_per_site, !pol.prewarm, self.args.bench);
        let dir = self.data_dir.join("WebView2");
        match webview::create_env(&dir, &args) {
            Ok(()) => self.env = EnvState::Creating,
            Err(e) => self.show_error(&format!("Не удалось запустить WebView2: {e}\nУстановите Microsoft Edge WebView2 Runtime.")),
        }
    }

    pub fn show_error(&self, msg: &str) {
        let hwnd = self.windows.first().map(|w| w.hwnd);
        unsafe {
            MessageBoxW(hwnd, &HSTRING::from(msg), &HSTRING::from("Realism Browser"), MB_ICONERROR | MB_OK);
        }
    }

    fn on_env_created(&mut self, r: windows::core::Result<ICoreWebView2Environment>) {
        match r {
            Ok(env) => {
                self.env = EnvState::Ready(env);
                self.env_idle_since = None;
                if self.webview2_version.is_empty() {
                    self.webview2_version = webview::runtime_version();
                }
                // Create webviews for every tab that was waiting for the engine.
                let mut todo = Vec::new();
                for (wi, w) in self.windows.iter().enumerate() {
                    for (ti, t) in w.tabs.iter().enumerate() {
                        if t.wv.is_none() && t.pending_nav.is_some() {
                            todo.push((wi, ti, t.id));
                        }
                    }
                }
                for (_, _, id) in todo {
                    if let Some((wi, ti)) = self.loc(id) {
                        let url = self.windows[wi].tabs[ti].pending_nav.take();
                        self.create_webview(wi, ti, url.as_deref());
                    }
                }
            }
            Err(e) => {
                self.env = EnvState::None;
                self.show_error(&format!(
                    "Движок WebView2 недоступен ({e}).\nУстановите Microsoft Edge WebView2 Runtime: https://developer.microsoft.com/microsoft-edge/webview2/"
                ));
            }
        }
    }

    /// The browser process died: every webview is gone. Mark tabs as discarded so they reload on demand.
    fn on_browser_exited(&mut self) {
        for w in self.windows.iter_mut() {
            for t in w.tabs.iter_mut() {
                if t.wv.take().is_some() {
                    t.suspended = false;
                    t.loading = false;
                }
            }
        }
        self.zombies.clear();
        self.env = EnvState::None;
        let active: Vec<(usize, usize)> = self.windows.iter().enumerate().map(|(wi, w)| (wi, w.active)).collect();
        for (wi, ti) in active {
            if ti < self.windows[wi].tabs.len() && !self.windows[wi].tabs[ti].url.is_empty() {
                let url = self.windows[wi].tabs[ti].url.clone();
                self.ensure_webview(wi, ti, Some(url));
            }
        }
        self.redraw_all();
    }

    /// Create the webview for tab now, or as soon as the engine is ready.
    pub fn ensure_webview(&mut self, wi: usize, ti: usize, url: Option<String>) {
        if self.windows[wi].tabs[ti].wv.is_some() {
            if let Some(u) = url {
                webview::navigate(&self.windows[wi].tabs[ti].wv.as_ref().unwrap().core, &u);
            }
            return;
        }
        match &self.env {
            EnvState::Ready(_) => self.create_webview(wi, ti, url.as_deref()),
            _ => {
                let t = &mut self.windows[wi].tabs[ti];
                t.pending_nav = Some(url.unwrap_or_else(|| "about:blank".into()));
                t.loading = true;
                t.load_start = Instant::now();
                self.start_env();
                self.ensure_anim();
            }
        }
    }

    pub fn create_webview(&mut self, wi: usize, ti: usize, url: Option<&str>) {
        let EnvState::Ready(env) = &self.env else { return };
        let env = env.clone();
        let w = &self.windows[wi];
        let visible = w.active == ti && !w.minimized;
        let bounds = w.content_px();
        // Pages without their own background get the standard white canvas, as in every browser.
        let bg = (255u8, 255u8, 255u8);
        let tab_id = w.tabs[ti].id;
        let r = webview::create(webview::CreateOpts {
            env: &env,
            parent: w.hwnd,
            tab: tab_id,
            private: w.private,
            url,
            bounds,
            visible,
            bg,
            scheme: self.scheme(),
        });
        // Creating a webview pumps messages; the tab may have moved. Re-locate it.
        let Some((wi, ti)) = self.loc(tab_id) else { return };
        match r {
            Ok(wv) => {
                let t = &mut self.windows[wi].tabs[ti];
                if let Some(u) = url {
                    if u != "about:blank" {
                        t.loading = true;
                        t.load_start = Instant::now();
                    }
                }
                t.suspended = false;
                t.low_mem = false;
                t.pending_nav = None;
                t.shown = visible;
                t.wv = Some(wv);
                self.env_idle_since = None;
                let w = &self.windows[wi];
                if w.active == ti && !w.minimized {
                    // Bounds may have changed while the controller was being created.
                    self.windows[wi].place_webview(ti);
                    if !self.windows[wi].focus_address {
                        self.focus_web(wi);
                    }
                }
                self.ensure_anim();
            }
            Err(e) => {
                eprintln!("webview creation failed: {e}");
                let t = &mut self.windows[wi].tabs[ti];
                t.loading = false;
                t.info = Some(Info::Message(format!("Не удалось создать вкладку: {e}")));
            }
        }
        self.windows[wi].relayout();
        self.redraw(wi);
    }

    // ------------------------------------------------------------------ tabs

    /// Adds a tab. `url = None` -> native new tab page (no webview, no RAM).
    pub fn add_tab(&mut self, wi: usize, url: Option<String>, at: Option<usize>, activate: bool) -> u64 {
        let id = self.new_id();
        let mut t = Tab::new(id, String::new());
        let w = &mut self.windows[wi];
        let idx = at.unwrap_or(w.tabs.len()).min(w.tabs.len());
        if let Some(u) = &url {
            t.url = u.clone();
        }
        w.tabs.insert(idx, t);
        if w.active >= idx && w.tabs.len() > 1 && !activate {
            w.active += 1;
        }
        if activate {
            self.activate(wi, idx);
            if url.is_some() {
                self.ensure_webview(wi, idx, url);
            }
        } else if url.is_some() {
            self.ensure_webview(wi, idx, url);
        }
        if let Some((wi, _)) = self.loc(id) {
            self.windows[wi].relayout();
            self.redraw(wi);
        }
        self.mark_session();
        id
    }

    pub fn activate(&mut self, wi: usize, idx: usize) {
        let pol = self.policy();
        let w = &mut self.windows[wi];
        if idx >= w.tabs.len() {
            return;
        }
        let prev = w.active;
        if prev != idx && prev < w.tabs.len() {
            let pt = &mut w.tabs[prev];
            pt.hidden_since = Some(Instant::now());
            if let Some(wv) = &pt.wv {
                let _ = wv.wry.set_visible(false);
                if pol.low_mem_on_hide && !pt.audio && !pt.media_active {
                    webview::set_low_memory(&wv.core, true);
                    pt.low_mem = true;
                }
            }
        }
        w.active = idx;
        let has_wv = w.tabs[idx].wv.is_some();
        {
            let t = &mut w.tabs[idx];
            t.hidden_since = None;
            t.last_active = Instant::now();
        }
        if has_wv {
            w.place_webview(idx);
            let t = &mut w.tabs[idx];
            let wv = t.wv.as_ref().unwrap();
            if t.low_mem {
                webview::set_low_memory(&wv.core, false);
                t.low_mem = false;
            }
            if !w.minimized {
                let _ = wv.wry.set_visible(true);
                t.suspended = false;
                t.shown = true;
            }
        }
        w.focus_address = !has_wv && w.tabs[idx].is_ntp();
        w.editing = false;
        w.sync_address();
        w.relayout();
        if !has_wv {
            let t = &self.windows[wi].tabs[idx];
            if t.is_discarded() && t.pending_nav.is_none() {
                // Restore an unloaded tab.
                let url = t.url.clone();
                self.ensure_webview(wi, idx, Some(url));
            }
        }
        if self.windows[wi].focus_address {
            self.windows[wi].focus_edit();
        } else if has_wv {
            self.focus_web(wi);
        }
        self.redraw(wi);
        self.mark_session();
        if wi < self.windows.len() {
            self.refresh_ntp(wi);
        }
    }

    pub fn focus_web(&mut self, wi: usize) {
        let w = &mut self.windows[wi];
        if let Some(t) = w.tabs.get(w.active) {
            if let Some(wv) = &t.wv {
                unsafe {
                    let _ = wv.ctrl.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
                }
                w.focus_address = false;
            }
        }
    }

    pub fn close_tab(&mut self, wi: usize, ti: usize) {
        if wi >= self.windows.len() || ti >= self.windows[wi].tabs.len() {
            return;
        }
        let private = self.windows[wi].private;
        let mut t = self.windows[wi].tabs.remove(ti);
        // Resolve pending prompts so the page does not hang.
        for p in t.perms.drain(..) {
            unsafe {
                let _ = p.args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY);
                let _ = p.deferral.Complete();
            }
        }
        if !private && (!t.url.is_empty()) {
            self.closed.push(ClosedTab {
                url: t.url.clone(),
                title: t.title.clone(),
                icon: t.icon.clone(),
                win: self.windows[wi].id,
                index: ti,
                back: t.hist[..t.hist_idx.min(t.hist.len())].to_vec(),
            });
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
        if let Some(wv) = t.wv.take() {
            let busy = self.downloads.iter().any(|d| d.tab == t.id && matches!(d.state, DlState::Running | DlState::Paused));
            if busy {
                // Keep the webview alive (hidden) until its downloads finish.
                let _ = wv.wry.set_visible(false);
                self.zombies.push((t.id, wv));
            }
        }
        let w = &mut self.windows[wi];
        if w.tabs.is_empty() {
            self.close_window(wi);
            return;
        }
        // Prefer returning to the opener (popup-style tabs).
        let mut next = if w.active > ti || w.active >= w.tabs.len() { w.active.saturating_sub(1) } else { w.active };
        if w.active == ti {
            if let Some(op) = t.opener {
                if let Some(oi) = w.tabs.iter().position(|x| x.id == op) {
                    next = oi;
                }
            } else {
                next = ti.min(w.tabs.len() - 1);
            }
            w.active = usize::MAX;
        }
        let next = next.min(w.tabs.len() - 1);
        if w.active == usize::MAX {
            self.activate(wi, next);
        } else {
            w.active = next;
            w.relayout();
            self.redraw(wi);
        }
        self.mark_session();
    }

    pub fn close_tab_id(&mut self, tab: u64) {
        if let Some((wi, ti)) = self.loc(tab) {
            self.close_tab(wi, ti);
        }
    }

    pub fn reopen_closed(&mut self, wi: usize) {
        let Some(c) = self.closed.pop() else { return };
        let target = self.windows.iter().position(|w| w.id == c.win && !w.private).unwrap_or(wi);
        let idx = c.index.min(self.windows[target].tabs.len());
        let id = self.new_id();
        let mut t = Tab::new(id, c.url.clone());
        t.title = c.title;
        t.icon = c.icon;
        t.back_stack = c.back;
        self.windows[target].tabs.insert(idx, t);
        if self.windows[target].active >= idx && self.windows[target].active != usize::MAX {
            self.windows[target].active += 1;
        }
        self.activate(target, idx);
    }

    /// Unload a tab's webview, keeping url/title/icon so it can be restored.
    pub fn discard(&mut self, wi: usize, ti: usize) {
        let t = &mut self.windows[wi].tabs[ti];
        if let Some(wv) = t.wv.take() {
            if let Ok(src) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| webview::source(&wv.core))) {
                if !src.is_empty() && src != "about:blank" {
                    t.url = src;
                }
            }
            t.back_stack = t.hist[..t.hist_idx.min(t.hist.len())].to_vec();
            drop(wv);
        }
        t.suspended = false;
        t.shown = false;
        t.low_mem = false;
        t.loading = false;
        t.audio = false;
        if self.live_webviews() == 0 {
            self.env_idle_since = Some(Instant::now());
        }
        self.redraw(wi);
    }

    // ------------------------------------------------------------------ navigation

    pub fn navigate_input(&mut self, wi: usize, input: &str, new_tab: bool) {
        let target = normalize_input(input, &self.s.search);
        if new_tab {
            self.add_tab(wi, target, None, true);
            return;
        }
        let ti = self.windows[wi].active;
        self.navigate_tab(wi, ti, target);
    }

    pub fn navigate_tab(&mut self, wi: usize, ti: usize, url: Option<String>) {
        let w = &mut self.windows[wi];
        w.editing = false;
        let Some(url) = url else {
            // Back to the native new tab page: drop the webview entirely.
            let t = &mut w.tabs[ti];
            t.wv = None;
            t.url.clear();
            t.title.clear();
            t.icon = None;
            t.loading = false;
            t.hist.clear();
            t.hist_idx = 0;
            w.focus_address = true;
            w.sync_address();
            w.focus_edit();
            self.refresh_ntp(wi);
            self.redraw(wi);
            return;
        };
        let t = &mut w.tabs[ti];
        if t.wv.is_none() && t.url.is_empty() {
            t.came_from_ntp = true;
        }
        t.url = url.clone();
        t.loading = true;
        t.load_start = Instant::now();
        w.focus_address = false;
        w.sync_address();
        if let Some(wv) = &w.tabs[ti].wv {
            webview::navigate(&wv.core, &url);
            self.focus_web(wi);
        } else {
            self.ensure_webview(wi, ti, Some(url));
        }
        self.ensure_anim();
        self.redraw(wi);
    }

    pub fn go_back(&mut self, wi: usize) {
        let ti = self.windows[wi].active;
        let t = &mut self.windows[wi].tabs[ti];
        if let Some(wv) = &t.wv {
            if t.can_back {
                unsafe {
                    let _ = wv.core.GoBack();
                }
                return;
            }
        }
        if let Some(prev) = t.back_stack.pop() {
            // History restored from an unloaded tab: recreate the view at the previous page.
            t.wv = None;
            t.url = prev.clone();
            let rest = std::mem::take(&mut t.back_stack);
            t.hist = rest.clone();
            t.hist_idx = t.hist.len();
            t.back_stack = rest;
            self.ensure_webview(wi, ti, Some(prev));
            return;
        }
        if t.came_from_ntp {
            t.came_from_ntp = false;
            self.navigate_tab(wi, ti, None);
        }
    }

    pub fn can_go_back(&self, t: &Tab) -> bool {
        t.can_back || !t.back_stack.is_empty() || (t.came_from_ntp && !t.url.is_empty())
    }

    pub fn open_internal(&mut self, wi: usize, page: &str) {
        let url = internal_url(page);
        if let Some(ti) = self.windows[wi].tabs.iter().position(|t| t.url.starts_with(&url)) {
            self.activate(wi, ti);
            return;
        }
        let ti = self.windows[wi].active;
        if self.windows[wi].tabs.get(ti).map(|t| t.is_ntp()).unwrap_or(false) {
            self.navigate_tab(wi, ti, Some(url));
        } else {
            self.add_tab(wi, Some(url), None, true);
        }
    }

    // ------------------------------------------------------------------ events

    pub fn drain(&mut self) {
        if crate::update::poll(&mut self.upd) {
            // Badge on the menu button / about page refresh.
            self.redraw_all();
        }
        for _ in 0..64 {
            let evs = events::take_events();
            let protos = events::take_proto();
            if evs.is_empty() && protos.is_empty() {
                break;
            }
            for e in evs {
                self.handle(e);
            }
            for p in protos {
                self.handle_proto(p);
            }
        }
    }

    fn handle(&mut self, e: Ev) {
        match e {
            Ev::EnvCreated(r) => self.on_env_created(r),
            Ev::BrowserExited => self.on_browser_exited(),
            Ev::NavStarting { tab, url, kind, user: _ } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let w = &mut self.windows[wi];
                let t = &mut w.tabs[ti];
                t.loading = true;
                t.load_start = Instant::now();
                t.dirty_input = false;
                t.media_active = false;
                t.cert_error = false;
                if matches!(t.info, Some(Info::Crashed)) {
                    t.info = None;
                }
                if url != "about:blank" {
                    t.url = url;
                }
                if kind == COREWEBVIEW2_NAVIGATION_KIND_BACK_OR_FORWARD.0 {
                    if t.hist_idx >= 2 && t.hist.get(t.hist_idx - 2) == Some(&t.url) {
                        t.hist_idx -= 1;
                    } else if t.hist.get(t.hist_idx) == Some(&t.url) {
                        t.hist_idx += 1;
                    }
                }
                if w.active == ti {
                    w.sync_address();
                }
                self.ensure_anim();
                self.redraw(wi);
            }
            Ev::NavCompleted { tab, ok, status: _ } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let private = self.windows[wi].private;
                let t = &mut self.windows[wi].tabs[ti];
                t.loading = false;
                if let Some(wv) = &t.wv {
                    let src = webview::source(&wv.core);
                    if !src.is_empty() && src != "about:blank" {
                        t.url = src;
                    }
                    t.title = webview::title(&wv.core);
                    t.can_back = webview::get_bool(|b| unsafe { wv.core.CanGoBack(b) });
                    t.can_fwd = webview::get_bool(|b| unsafe { wv.core.CanGoForward(b) });
                }
                if ok {
                    t.committed = true;
                    if t.hist.get(t.hist_idx.wrapping_sub(1)) != Some(&t.url) {
                        t.hist.truncate(t.hist_idx);
                        t.hist.push(t.url.clone());
                        t.hist_idx = t.hist.len();
                        if t.hist.len() > 50 {
                            t.hist.remove(0);
                            t.hist_idx -= 1;
                        }
                    }
                    if !private && (t.url.starts_with("http://") || t.url.starts_with("https://")) && !is_internal(&t.url) {
                        let (u, ti2) = (t.url.clone(), t.title.clone());
                        self.db.add_visit(&u, &ti2);
                    }
                }
                self.apply_site_zoom(tab);
                if let Some((wi, ti)) = self.loc(tab) {
                    if self.windows[wi].active == ti {
                        self.windows[wi].sync_address();
                    }
                    self.windows[wi].relayout();
                    self.redraw(wi);
                }
                self.mark_session();
            }
            Ev::SourceChanged { tab } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let private = self.windows[wi].private;
                let t = &mut self.windows[wi].tabs[ti];
                if let Some(wv) = &t.wv {
                    let src = webview::source(&wv.core);
                    if !src.is_empty() && src != "about:blank" && src != t.url {
                        t.url = src;
                        // Same-document navigation (SPA): record it in history too.
                        if !t.loading && !private && t.url.starts_with("http") && !is_internal(&t.url) {
                            let (u, ti2) = (t.url.clone(), t.title.clone());
                            self.db.add_visit(&u, &ti2);
                        }
                    }
                }
                self.apply_site_zoom(tab);
                if let Some((wi, ti)) = self.loc(tab) {
                    if self.windows[wi].active == ti {
                        self.windows[wi].sync_address();
                    }
                    self.redraw(wi);
                }
            }
            Ev::TitleChanged { tab } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let private = self.windows[wi].private;
                let t = &mut self.windows[wi].tabs[ti];
                if let Some(wv) = &t.wv {
                    t.title = webview::title(&wv.core);
                    if !private && t.url.starts_with("http") {
                        let (u, ti2) = (t.url.clone(), t.title.clone());
                        self.db.update_title(&u, &ti2);
                    }
                }
                self.windows[wi].update_title();
                self.redraw(wi);
                self.mark_session();
            }
            Ev::HistoryChanged { tab } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let t = &mut self.windows[wi].tabs[ti];
                if let Some(wv) = &t.wv {
                    t.can_back = webview::get_bool(|b| unsafe { wv.core.CanGoBack(b) });
                    t.can_fwd = webview::get_bool(|b| unsafe { wv.core.CanGoForward(b) });
                }
                self.redraw(wi);
            }
            Ev::Favicon { tab, pixels } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                self.windows[wi].tabs[ti].icon = pixels.map(Rc::new);
                self.redraw(wi);
                self.mark_session();
            }
            Ev::Audio { tab } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let t = &mut self.windows[wi].tabs[ti];
                if let Some(wv) = &t.wv {
                    if let Ok(c8) = wv.core.cast::<ICoreWebView2_8>() {
                        t.audio = webview::get_bool(|b| unsafe { c8.IsDocumentPlayingAudio(b) });
                        t.muted = webview::get_bool(|b| unsafe { c8.IsMuted(b) });
                    }
                }
                self.windows[wi].relayout();
                self.redraw(wi);
            }
            Ev::Fullscreen { tab } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let fs = self.windows[wi].tabs[ti]
                    .wv
                    .as_ref()
                    .map(|wv| webview::get_bool(|b| unsafe { wv.core.ContainsFullScreenElement(b) }))
                    .unwrap_or(false);
                self.set_fullscreen(wi, fs, true);
            }
            Ev::NewWindow { tab, args, deferral } => self.on_new_window(tab, args, deferral),
            Ev::Permission { tab, args, deferral } => self.on_permission(tab, args, deferral),
            Ev::Download { tab, args, deferral } => self.on_download(tab, args, deferral),
            Ev::DownloadState { id } => self.poll_downloads(Some(id)),
            Ev::Ipc { tab, msg } => {
                if let Some(t) = self.tab_mut(tab) {
                    match msg.as_str() {
                        "__rb:dirty" => t.dirty_input = true,
                        "__rb:clean" => t.dirty_input = false,
                        _ => {}
                    }
                }
            }
            Ev::CloseRequested { tab } => self.close_tab_id(tab),
            Ev::ProcessFailed { tab, kind } => {
                if let Some((wi, ti)) = self.loc(tab) {
                    if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED.0
                        || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE.0
                    {
                        let t = &mut self.windows[wi].tabs[ti];
                        t.loading = false;
                        t.audio = false;
                        t.info = Some(Info::Crashed);
                        self.windows[wi].relayout();
                        self.redraw(wi);
                    }
                }
            }
            Ev::Zoom { tab } => {
                let Some((wi, ti)) = self.loc(tab) else { return };
                let private = self.windows[wi].private;
                let t = &mut self.windows[wi].tabs[ti];
                if let Some(wv) = &t.wv {
                    let mut z = 1.0f64;
                    unsafe {
                        let _ = wv.ctrl.ZoomFactor(&mut z);
                    }
                    t.zoom = z;
                    let host = host_of(&t.url).to_string();
                    if !private && !host.is_empty() {
                        self.db.set_zoom(&host, z);
                    }
                }
                self.windows[wi].relayout();
                self.redraw(wi);
            }
            Ev::Cmd { tab, cmd } => {
                dlog(&format!("cmd {cmd:?} from tab {tab}"));
                if let Some((wi, _)) = self.loc(tab) {
                    self.run_cmd(wi, cmd);
                }
            }
            Ev::Suspended { tab, ok } => {
                dlog(&format!("suspend tab={tab} ok={ok}"));
                if let Some((wi, ti)) = self.loc(tab) {
                    let w = &mut self.windows[wi];
                    // A tab may have been activated while the request was in flight.
                    let visible = w.active == ti && !w.minimized;
                    w.tabs[ti].suspended = ok && !visible;
                    w.tabs[ti].suspend_failed = if ok { None } else { Some(Instant::now()) };
                    if ok && !visible {
                        // Re-apply the LOW target on the now frozen page: memory that crept back into the
                        // working set since the first trim is paged out again, and a frozen page does not touch it.
                        if let Some(wv) = &w.tabs[ti].wv {
                            webview::set_low_memory(&wv.core, false);
                            webview::set_low_memory(&wv.core, true);
                            w.tabs[ti].low_mem = true;
                        }
                    }
                    self.redraw(wi);
                }
            }
            Ev::Focus { tab } => {
                self.close_popup();
                if let Some((wi, _)) = self.loc(tab) {
                    let w = &mut self.windows[wi];
                    w.focus_address = false;
                    if w.editing {
                        w.editing = false;
                        w.sync_address();
                    }
                    self.redraw(wi);
                }
            }
            Ev::CertError { tab } => {
                if let Some(t) = self.tab_mut(tab) {
                    t.cert_error = true;
                }
                if let Some((wi, _)) = self.loc(tab) {
                    self.redraw(wi);
                }
            }
            Ev::PerfInfos { req, infos } => self.finish_perf(req, Some(infos)),
            Ev::ClearDone { req, ok } => {
                if let Some((resp, _)) = self.pending_api.remove(&req) {
                    crate::pages::respond_json(resp, &serde_json::json!({ "ok": ok }));
                }
            }
        }
    }

    fn apply_site_zoom(&mut self, tab: u64) {
        let Some((wi, ti)) = self.loc(tab) else { return };
        let private = self.windows[wi].private;
        let t = &mut self.windows[wi].tabs[ti];
        let host = host_of(&t.url).to_string();
        if host == t.zoom_host {
            return;
        }
        t.zoom_host = host.clone();
        let z = if private { 1.0 } else { self.db.zoom_for(&host).unwrap_or(1.0) };
        if let Some(wv) = &t.wv {
            if (z - t.zoom).abs() > 0.001 {
                unsafe {
                    let _ = wv.ctrl.SetZoomFactor(z);
                }
                t.zoom = z;
            }
        }
    }

    fn on_new_window(&mut self, tab: u64, args: ICoreWebView2NewWindowRequestedEventArgs, deferral: ICoreWebView2Deferral) {
        let done = |d: &ICoreWebView2Deferral| unsafe {
            let _ = d.Complete();
        };
        let Some((wi, ti)) = self.loc(tab) else {
            done(&deferral);
            return;
        };
        let uri = unsafe {
            let mut p = PWSTR::null();
            let _ = args.Uri(&mut p);
            take_pwstr(p)
        };
        let user = webview::get_bool(|b| unsafe { args.IsUserInitiated(b) });
        let opener_url = self.windows[wi].tabs[ti].url.clone();
        let origin = origin_of(&opener_url);
        let allowed = user
            || !self.db.query("SELECT 1 FROM popups_allowed WHERE origin=?", &[V::T(&origin)], |r| r.int(0)).is_empty()
            || is_internal(&opener_url);
        if !allowed {
            unsafe {
                let _ = args.SetHandled(true);
            }
            done(&deferral);
            self.windows[wi].tabs[ti].info = Some(Info::PopupBlocked { origin, url: uri });
            self.windows[wi].relayout();
            self.redraw(wi);
            return;
        }
        let background = unsafe { (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 || (GetKeyState(VK_MBUTTON.0 as i32) as u16 & 0x8000) != 0 };
        // Insert after the opener and after its other children (like Chrome).
        let w = &self.windows[wi];
        let mut at = ti + 1;
        while at < w.tabs.len() && w.tabs[at].opener == Some(tab) {
            at += 1;
        }
        let id = self.new_id();
        let mut t = Tab::new(id, uri.clone());
        t.opener = Some(tab);
        t.loading = true;
        self.windows[wi].tabs.insert(at, t);
        if self.windows[wi].active >= at {
            self.windows[wi].active += 1;
        }
        let EnvState::Ready(_) = &self.env else {
            done(&deferral);
            return;
        };
        self.create_webview(wi, at, None);
        let Some((wi, ti)) = self.loc(id) else {
            done(&deferral);
            return;
        };
        if let Some(wv) = &self.windows[wi].tabs[ti].wv {
            unsafe {
                let _ = args.SetNewWindow(&wv.core);
                let _ = args.SetHandled(true);
            }
        }
        done(&deferral);
        if !background {
            self.activate(wi, ti);
        }
        self.windows[wi].relayout();
        self.redraw(wi);
        self.mark_session();
    }

    fn on_permission(&mut self, tab: u64, args: ICoreWebView2PermissionRequestedEventArgs, deferral: ICoreWebView2Deferral) {
        let Some((wi, ti)) = self.loc(tab) else {
            unsafe {
                let _ = deferral.Complete();
            }
            return;
        };
        let uri = unsafe {
            let mut p = PWSTR::null();
            let _ = args.Uri(&mut p);
            take_pwstr(p)
        };
        let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
        unsafe {
            let _ = args.PermissionKind(&mut kind);
        }
        let kind = kind.0;
        let origin = origin_of(&uri);
        let private = self.windows[wi].private;
        let stored = if private { self.private_perms.get(&(origin.clone(), kind)).copied() } else { self.db.permission(&origin, kind) };
        if is_internal(&uri) {
            // Internal pages never need permissions.
            unsafe {
                let _ = args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY);
                let _ = deferral.Complete();
            }
            return;
        }
        if let Some(st) = stored {
            unsafe {
                let _ = args.SetState(COREWEBVIEW2_PERMISSION_STATE(st));
                let _ = deferral.Complete();
            }
            if st == COREWEBVIEW2_PERMISSION_STATE_ALLOW.0 && (kind == 1 || kind == 2) {
                self.windows[wi].tabs[ti].media_active = true;
            }
            return;
        }
        self.windows[wi].tabs[ti].perms.push(PermPrompt { args, deferral, origin, kind });
        self.windows[wi].relayout();
        self.redraw(wi);
    }

    /// Answer the first pending permission prompt of the active tab.
    pub fn answer_permission(&mut self, wi: usize, allow: bool, remember: bool) {
        let ti = self.windows[wi].active;
        let private = self.windows[wi].private;
        let t = &mut self.windows[wi].tabs[ti];
        if t.perms.is_empty() {
            return;
        }
        let p = t.perms.remove(0);
        let st = if allow { COREWEBVIEW2_PERMISSION_STATE_ALLOW } else { COREWEBVIEW2_PERMISSION_STATE_DENY };
        unsafe {
            let _ = p.args.SetState(st);
            let _ = p.deferral.Complete();
        }
        if allow && (p.kind == 1 || p.kind == 2) {
            t.media_active = true;
        }
        if remember {
            if private {
                self.private_perms.insert((p.origin.clone(), p.kind), st.0);
            } else {
                self.db.set_permission(&p.origin, p.kind, st.0);
            }
        }
        self.windows[wi].relayout();
        self.redraw(wi);
    }

    // ------------------------------------------------------------------ downloads

    pub fn download_dir(&self) -> PathBuf {
        if !self.s.download_dir.is_empty() {
            return PathBuf::from(&self.s.download_dir);
        }
        known_downloads_dir()
    }

    fn on_download(&mut self, tab: u64, args: ICoreWebView2DownloadStartingEventArgs, deferral: ICoreWebView2Deferral) {
        let suggested = unsafe {
            let mut p = PWSTR::null();
            let _ = args.ResultFilePath(&mut p);
            take_pwstr(p)
        };
        let name = std::path::Path::new(&suggested).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "download".into());
        if self.s.ask_download {
            self.ui_tasks.push(UiTask::SaveAs { args, deferral, tab, suggested: name });
            return;
        }
        let path = unique_path(&self.download_dir(), &name);
        self.start_download(tab, &args, &path);
        unsafe {
            let _ = deferral.Complete();
        }
    }

    pub fn start_download(&mut self, tab: u64, args: &ICoreWebView2DownloadStartingEventArgs, path: &std::path::Path) {
        let Ok(op) = (unsafe { args.DownloadOperation() }) else { return };
        unsafe {
            let _ = args.SetResultFilePath(&HSTRING::from(path.to_string_lossy().as_ref()));
            let _ = args.SetHandled(true);
        }
        let id = self.new_id();
        let mut tok: i64 = 0;
        unsafe {
            let _ = op.add_StateChanged(
                &StateChangedEventHandler::create(Box::new(move |_, _| {
                    events::push(Ev::DownloadState { id });
                    Ok(())
                })),
                &mut tok,
            );
        }
        let url = unsafe {
            let mut p = PWSTR::null();
            let _ = op.Uri(&mut p);
            take_pwstr(p)
        };
        let mut total = 0i64;
        unsafe {
            let _ = op.TotalBytesToReceive(&mut total);
        }
        let private = self.loc(tab).map(|(wi, _)| self.windows[wi].private).unwrap_or(false);
        self.downloads.insert(
            0,
            Download {
                id,
                db_id: 0,
                op: Some(op),
                url,
                path: path.to_string_lossy().into_owned(),
                total,
                received: 0,
                state: DlState::Running,
                tab,
                private,
                started: now_ms(),
                speed: 0.0,
                last_sample: (Instant::now(), 0),
            },
        );
        for w in self.windows.iter_mut() {
            if !w.show_dl {
                w.show_dl = true;
                w.relayout();
            }
        }
        // A tab that was opened only to start this download (never showed a page): close it,
        // the webview stays alive in the background until the download finishes.
        if let Some((wi, ti)) = self.loc(tab) {
            let t = &self.windows[wi].tabs[ti];
            if !t.committed && self.windows[wi].tabs.len() > 1 {
                self.close_tab(wi, ti);
            }
        }
        self.ensure_dl_timer();
        self.redraw_all();
    }

    pub fn poll_downloads(&mut self, _changed: Option<u64>) {
        let mut finished = Vec::new();
        for d in self.downloads.iter_mut() {
            let Some(op) = &d.op else { continue };
            if !matches!(d.state, DlState::Running | DlState::Paused) {
                continue;
            }
            let mut rec = 0i64;
            let mut tot = 0i64;
            let mut st = COREWEBVIEW2_DOWNLOAD_STATE::default();
            unsafe {
                let _ = op.BytesReceived(&mut rec);
                let _ = op.TotalBytesToReceive(&mut tot);
                let _ = op.State(&mut st);
            }
            let now = Instant::now();
            let dt = now.duration_since(d.last_sample.0).as_secs_f64();
            if dt >= 0.9 {
                let inst = (rec - d.last_sample.1).max(0) as f64 / dt;
                d.speed = if d.speed == 0.0 { inst } else { d.speed * 0.5 + inst * 0.5 };
                d.last_sample = (now, rec);
            }
            d.received = rec;
            d.total = tot;
            let new_state = if st == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED {
                DlState::Done
            } else if st == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED {
                let mut reason = COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON::default();
                unsafe {
                    let _ = op.InterruptReason(&mut reason);
                }
                if reason == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_PAUSED {
                    DlState::Paused
                } else if reason == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED {
                    DlState::Canceled
                } else {
                    DlState::Failed
                }
            } else {
                DlState::Running
            };
            if new_state != d.state {
                d.state = new_state;
                if matches!(new_state, DlState::Done | DlState::Failed | DlState::Canceled) {
                    d.speed = 0.0;
                    finished.push(d.id);
                }
            }
        }
        for id in finished {
            let Some(d) = self.downloads.iter_mut().find(|d| d.id == id) else { continue };
            if d.state == DlState::Done {
                d.received = d.received.max(d.total);
            }
            if !d.private {
                self.db.run(
                    "INSERT INTO downloads(url,path,total,received,state,ts) VALUES(?,?,?,?,?,?)",
                    &[V::T(&d.url), V::T(&d.path), V::I(d.total), V::I(d.received), V::T(d.state.key()), V::I(d.started)],
                );
                d.db_id = self.db.last_id();
            }
            if d.state != DlState::Failed {
                d.op = None;
            }
        }
        // Drop webviews kept alive only for finished downloads.
        let active_tabs: Vec<u64> =
            self.downloads.iter().filter(|d| matches!(d.state, DlState::Running | DlState::Paused)).map(|d| d.tab).collect();
        self.zombies.retain(|(t, _)| active_tabs.contains(t));
        if !self.downloads.iter().any(|d| d.state == DlState::Running) {
            self.stop_dl_timer();
        }
        self.redraw_all();
    }

    pub fn load_download_history(&mut self) {
        let rows = self.db.query(
            "SELECT id,url,path,total,received,state,ts FROM downloads ORDER BY ts DESC LIMIT 200",
            &[],
            |r| (r.int(0), r.text(1), r.text(2), r.int(3), r.int(4), r.text(5), r.int(6)),
        );
        for (db_id, url, path, total, received, state, ts) in rows {
            let id = self.new_id();
            let state = match state.as_str() {
                "done" => DlState::Done,
                "canceled" => DlState::Canceled,
                _ => DlState::Failed,
            };
            self.downloads.push(Download {
                id,
                db_id,
                op: None,
                url,
                path,
                total,
                received,
                state,
                tab: 0,
                private: false,
                started: ts,
                speed: 0.0,
                last_sample: (Instant::now(), 0),
            });
        }
    }

    pub fn download_action(&mut self, id: u64, action: &str) -> bool {
        let Some(idx) = self.downloads.iter().position(|d| d.id == id) else { return false };
        let d = &mut self.downloads[idx];
        match action {
            "open" => shell_open(&d.path, None),
            "show" => shell_open("explorer.exe", Some(&format!("/select,\"{}\"", d.path))),
            "cancel" => {
                if let Some(op) = &d.op {
                    unsafe {
                        let _ = op.Cancel();
                    }
                }
            }
            "pause" => {
                if let Some(op) = &d.op {
                    unsafe {
                        let _ = op.Pause();
                    }
                }
            }
            "resume" => {
                if let Some(op) = &d.op {
                    unsafe {
                        let _ = op.Resume();
                    }
                    d.state = DlState::Running;
                    self.ensure_dl_timer();
                }
            }
            "remove" => {
                if matches!(d.state, DlState::Running | DlState::Paused) {
                    return false;
                }
                if d.db_id > 0 {
                    self.db.run("DELETE FROM downloads WHERE id=?", &[V::I(d.db_id)]);
                }
                self.downloads.remove(idx);
            }
            _ => return false,
        }
        self.poll_downloads(None);
        true
    }

    pub fn ensure_dl_timer(&mut self) {
        if !self.dl_timer_on {
            unsafe {
                SetTimer(Some(self.msg_hwnd), TIMER_DL, 500, None);
            }
            self.dl_timer_on = true;
        }
    }

    fn stop_dl_timer(&mut self) {
        if self.dl_timer_on {
            unsafe {
                let _ = KillTimer(Some(self.msg_hwnd), TIMER_DL);
            }
            self.dl_timer_on = false;
        }
    }

    pub fn downloads_progress(&self) -> Option<f32> {
        let running: Vec<&Download> = self.downloads.iter().filter(|d| d.state == DlState::Running).collect();
        if running.is_empty() {
            return None;
        }
        let tot: i64 = running.iter().map(|d| d.total.max(0)).sum();
        let rec: i64 = running.iter().map(|d| d.received).sum();
        Some(if tot > 0 { (rec as f32 / tot as f32).clamp(0.02, 1.0) } else { 0.25 })
    }

    // ------------------------------------------------------------------ fullscreen

    pub fn set_fullscreen(&mut self, wi: usize, on: bool, html: bool) {
        let w = &mut self.windows[wi];
        if html {
            w.html_fullscreen = on;
        }
        let want = w.html_fullscreen || w.f11;
        w.apply_fullscreen(want);
        ANY_FULLSCREEN.with(|c| c.set(self.windows.iter().any(|w| w.fullscreen.is_some())));
        self.redraw(wi);
    }

    // ------------------------------------------------------------------ commands

    pub fn run_cmd(&mut self, wi: usize, cmd: Cmd) {
        if wi >= self.windows.len() {
            return;
        }
        let ti = self.windows[wi].active;
        match cmd {
            Cmd::NewTab => {
                self.add_tab(wi, None, None, true);
            }
            Cmd::CloseTab => self.close_tab(wi, ti),
            Cmd::ReopenClosed => self.reopen_closed(wi),
            Cmd::FocusAddress => {
                self.close_popup();
                self.windows[wi].focus_edit();
            }
            Cmd::Reload => {
                let t = &mut self.windows[wi].tabs[ti];
                if let Some(wv) = &t.wv {
                    unsafe {
                        let _ = wv.core.Reload();
                    }
                } else if !t.url.is_empty() {
                    let u = t.url.clone();
                    self.ensure_webview(wi, ti, Some(u));
                }
            }
            Cmd::HardReload => {
                if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                    webview::hard_reload(&wv.core);
                }
            }
            Cmd::Stop => {
                if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                    unsafe {
                        let _ = wv.core.Stop();
                    }
                }
            }
            Cmd::Back => self.go_back(wi),
            Cmd::Forward => {
                if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                    unsafe {
                        let _ = wv.core.GoForward();
                    }
                }
            }
            Cmd::NextTab => {
                let n = self.windows[wi].tabs.len();
                self.activate(wi, (ti + 1) % n);
            }
            Cmd::PrevTab => {
                let n = self.windows[wi].tabs.len();
                self.activate(wi, (ti + n - 1) % n);
            }
            Cmd::SelectTab(i) => {
                if i < self.windows[wi].tabs.len() {
                    self.activate(wi, i);
                }
            }
            Cmd::LastTab => {
                let n = self.windows[wi].tabs.len();
                self.activate(wi, n - 1);
            }
            Cmd::NewWindow => self.open_window(false, vec![], None),
            Cmd::NewPrivateWindow => self.open_window(true, vec![], None),
            Cmd::History => self.open_internal(wi, "history"),
            Cmd::Downloads => self.open_internal(wi, "downloads"),
            Cmd::Bookmarks => self.open_internal(wi, "bookmarks"),
            Cmd::Settings => self.open_internal(wi, "settings"),
            Cmd::Performance => self.open_internal(wi, "performance"),
            Cmd::About => self.open_internal(wi, "about"),
            Cmd::CheckUpdates => {
                self.check_updates();
                self.open_internal(wi, "about");
            }
            Cmd::BookmarkPage => self.toggle_bookmark(wi),
            Cmd::ToggleBookmarksBar => {
                self.s.bookmarks_bar = !self.s.bookmarks_bar;
                self.db.set_setting("bookmarks_bar", if self.s.bookmarks_bar { "1" } else { "0" });
                self.refresh_bookmarks_bar();
            }
            Cmd::ZoomIn | Cmd::ZoomOut | Cmd::ZoomReset => {
                const STEPS: [f64; 15] = [0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 3.0, 5.0];
                if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                    let mut z = 1.0f64;
                    unsafe {
                        let _ = wv.ctrl.ZoomFactor(&mut z);
                    }
                    let nz = match cmd {
                        Cmd::ZoomIn => *STEPS.iter().find(|s| **s > z + 0.001).unwrap_or(&5.0),
                        Cmd::ZoomOut => *STEPS.iter().rev().find(|s| **s < z - 0.001).unwrap_or(&0.25),
                        _ => 1.0,
                    };
                    unsafe {
                        let _ = wv.ctrl.SetZoomFactor(nz);
                    }
                }
            }
            Cmd::Fullscreen => {
                let w = &mut self.windows[wi];
                w.f11 = !w.f11;
                if !w.f11 {
                    w.html_fullscreen = false;
                }
                self.set_fullscreen(wi, false, false);
            }
            Cmd::Escape => {
                self.close_popup();
                let w = &mut self.windows[wi];
                if w.fullscreen.is_some() {
                    w.f11 = false;
                    w.html_fullscreen = false;
                    self.set_fullscreen(wi, false, false);
                } else if w.editing {
                    w.editing = false;
                    w.sync_address();
                } else if w.tabs[ti].loading {
                    self.run_cmd(wi, Cmd::Stop);
                }
            }
            Cmd::DevTools => {
                if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                    unsafe {
                        let _ = wv.core.OpenDevToolsWindow();
                    }
                }
            }
            Cmd::Print => {
                if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                    if let Ok(c16) = wv.core.cast::<ICoreWebView2_16>() {
                        unsafe {
                            let _ = c16.ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER);
                        }
                    }
                }
            }
            Cmd::CloseWindow => unsafe {
                let _ = PostMessageW(Some(self.windows[wi].hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Cmd::FreeMemory => {
                self.free_memory();
            }
            Cmd::Exit => {
                for w in &self.windows {
                    unsafe {
                        let _ = PostMessageW(Some(w.hwnd), WM_CLOSE, WPARAM(0), LPARAM(1));
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ bookmarks

    pub fn is_bookmarked(&self, url: &str) -> bool {
        !url.is_empty() && !self.db.query("SELECT 1 FROM bookmarks WHERE url=? AND is_folder=0 LIMIT 1", &[V::T(url)], |r| r.int(0)).is_empty()
    }

    pub fn toggle_bookmark(&mut self, wi: usize) {
        let ti = self.windows[wi].active;
        let t = &self.windows[wi].tabs[ti];
        if t.url.is_empty() {
            return;
        }
        let (url, title) = (t.url.clone(), t.display_title());
        if self.is_bookmarked(&url) {
            self.db.run("DELETE FROM bookmarks WHERE url=? AND is_folder=0", &[V::T(&url)]);
        } else {
            let pos = self.db.query("SELECT COALESCE(MAX(pos),-1)+1 FROM bookmarks WHERE parent=1", &[], |r| r.int(0))[0];
            self.db.run(
                "INSERT INTO bookmarks(parent,is_folder,title,url,pos,added) VALUES(1,0,?,?,?,?)",
                &[V::T(&title), V::T(&url), V::I(pos), V::I(now_ms())],
            );
        }
        self.refresh_bookmarks_bar();
    }

    pub fn refresh_bookmarks_bar(&mut self) {
        for wi in 0..self.windows.len() {
            self.windows[wi].bm_dirty = true;
            self.windows[wi].relayout();
            self.redraw(wi);
        }
    }

    // ------------------------------------------------------------------ session

    pub fn mark_session(&mut self) {
        if self.args.bench || self.session_dirty {
            return;
        }
        self.session_dirty = true;
        unsafe {
            SetTimer(Some(self.msg_hwnd), TIMER_SESSION, 2500, None);
        }
    }

    pub fn save_session(&mut self) {
        self.session_dirty = false;
        if self.args.bench {
            return;
        }
        let _ = self.db.exec("BEGIN");
        self.db.run("DELETE FROM session", &[]);
        let mut wpos = 0i64;
        for w in self.windows.iter().filter(|w| !w.private) {
            for (i, t) in w.tabs.iter().enumerate() {
                let url = if t.url.is_empty() { t.pending_nav.clone().unwrap_or_default() } else { t.url.clone() };
                let back: Vec<String> = if t.hist_idx > 1 { t.hist[..t.hist_idx - 1].to_vec() } else { t.back_stack.clone() };
                let back = serde_json::to_string(&back).unwrap_or_else(|_| "[]".into());
                let icon: &[u8] = t.icon.as_deref().map(|v| v.as_slice()).unwrap_or(&[]);
                self.db.run(
                    "INSERT INTO session(win,pos,url,title,active,icon,back) VALUES(?,?,?,?,?,?,?)",
                    &[V::I(wpos), V::I(i as i64), V::T(&url), V::T(&t.title), V::I((w.active == i) as i64), if icon.is_empty() { V::N } else { V::B(icon) }, V::T(&back)],
                );
            }
            wpos += 1;
        }
        let _ = self.db.exec("COMMIT");
        if let Some(w) = self.windows.iter().find(|w| !w.private) {
            if let Some(p) = w.placement_string() {
                self.db.set_setting("window", &p);
            }
        }
    }

    pub fn restore_session(&mut self) -> bool {
        let rows = self.db.query("SELECT win,url,title,active,icon,back FROM session ORDER BY win,pos", &[], |r| {
            (r.int(0), r.text(1), r.text(2), r.int(3) != 0, r.blob(4), r.text(5))
        });
        if rows.is_empty() {
            return false;
        }
        let mut cur_win = -1i64;
        let mut wi = 0usize;
        for (win, url, title, active, icon, back) in rows {
            if win != cur_win {
                cur_win = win;
                let place = if self.windows.is_empty() { self.db.get_setting("window") } else { None };
                wi = self.create_window(false, place.as_deref());
            }
            let id = self.new_id();
            let mut t = Tab::new(id, url);
            t.title = title;
            if icon.len() == (crate::gfx::ICON_PX * crate::gfx::ICON_PX * 4) as usize {
                t.icon = Some(Rc::new(icon));
            }
            t.back_stack = serde_json::from_str(&back).unwrap_or_default();
            self.windows[wi].tabs.push(t);
            if active {
                self.windows[wi].active = self.windows[wi].tabs.len() - 1;
            }
        }
        for wi in 0..self.windows.len() {
            let a = self.windows[wi].active.min(self.windows[wi].tabs.len().saturating_sub(1));
            self.windows[wi].active = usize::MAX;
            self.activate(wi, a);
            self.windows[wi].show();
        }
        true
    }

    /// Open a browser window. `urls` empty -> one new-tab page.
    pub fn open_window(&mut self, private: bool, urls: Vec<String>, place: Option<&str>) {
        let wi = self.create_window(private, place);
        if urls.is_empty() {
            self.add_tab(wi, None, None, true);
        } else {
            let n = urls.len();
            for (i, u) in urls.into_iter().enumerate() {
                let url = normalize_input(&u, &self.s.search);
                self.add_tab(wi, url, None, i == 0 || i == n);
            }
            if let Some((wi2, _)) = self.windows.get(wi).and_then(|w| w.tabs.first()).and_then(|t| self.loc(t.id)) {
                self.activate(wi2, 0);
            }
        }
        if wi < self.windows.len() {
            self.windows[wi].show();
        }
        if self.policy().prewarm {
            self.start_env();
        }
    }

    pub fn close_window(&mut self, wi: usize) {
        if wi >= self.windows.len() {
            return;
        }
        self.close_popup();
        let is_last_normal = !self.windows[wi].private && self.windows.iter().filter(|w| !w.private).count() == 1;
        if is_last_normal {
            self.save_session();
        }
        let mut w = self.windows.remove(wi);
        for t in w.tabs.iter_mut() {
            for p in t.perms.drain(..) {
                unsafe {
                    let _ = p.deferral.Complete();
                }
            }
            if let Some(wv) = t.wv.take() {
                let busy = self.downloads.iter().any(|d| d.tab == t.id && matches!(d.state, DlState::Running | DlState::Paused));
                if busy {
                    let _ = wv.wry.set_visible(false);
                    // Re-parent is not needed: the zombie only has to stay alive, never shown.
                    self.zombies.push((t.id, wv));
                }
            }
        }
        if !is_last_normal && !w.private {
            self.save_session();
        }
        w.destroy();
        if self.windows.is_empty() {
            self.quit();
        }
    }

    pub fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        for w in self.windows.iter_mut() {
            for t in w.tabs.iter_mut() {
                t.wv = None;
            }
        }
        self.zombies.clear();
        self.env = EnvState::None;
        unsafe { PostQuitMessage(0) };
    }

    // ------------------------------------------------------------------ redraw helpers

    pub fn redraw(&self, wi: usize) {
        if let Some(w) = self.windows.get(wi) {
            w.invalidate();
        }
    }

    pub fn redraw_all(&self) {
        for w in &self.windows {
            w.invalidate();
        }
    }

    pub fn ensure_anim(&mut self) {
        if !self.anim_on {
            self.anim_on = true;
            unsafe {
                SetTimer(Some(self.msg_hwnd), TIMER_ANIM, 16, None);
            }
        }
    }

    pub fn anim_tick(&mut self) {
        let mut busy = false;
        for wi in 0..self.windows.len() {
            if self.windows[wi].minimized {
                continue;
            }
            if self.windows[wi].animate() {
                busy = true;
            }
        }
        if !busy {
            self.anim_on = false;
            unsafe {
                let _ = KillTimer(Some(self.msg_hwnd), TIMER_ANIM);
            }
        }
    }

    /// Starts an update check unless one is already running / an update is being installed.
    pub fn check_updates(&mut self) {
        use crate::update::State;
        if matches!(self.upd, State::Checking | State::Downloading(_) | State::Ready(..)) {
            return;
        }
        self.upd = State::Checking;
        self.db.set_setting("update_last_check", &now_ms().to_string());
        crate::update::start_check();
    }

    /// Automatic check: once a day (setting `update_auto`, on by default).
    pub fn auto_update_check(&mut self) {
        if self.args.bench || self.db.get_setting("update_auto").as_deref() == Some("0") {
            return;
        }
        let last: i64 = self.db.get_setting("update_last_check").and_then(|v| v.parse().ok()).unwrap_or(0);
        if now_ms() - last > 24 * 3600 * 1000 && matches!(self.upd, crate::update::State::Idle) {
            self.check_updates();
        }
    }

    pub fn update_available(&self) -> bool {
        matches!(self.upd, crate::update::State::Available(_) | crate::update::State::Ready(..))
    }

    pub fn set_theme(&mut self, t: &str) {
        self.s.theme = t.to_string();
        self.theme = Self::resolve_theme(t);
        let scheme = self.scheme();
        for w in self.windows.iter_mut() {
            w.apply_theme(&self.theme);
            for t in &w.tabs {
                if let Some(wv) = &t.wv {
                    webview::set_color_scheme(&wv.core, scheme);
                }
            }
        }
        self.redraw_all();
    }
}

pub fn known_downloads_dir() -> PathBuf {
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    std::env::var("USERPROFILE").map(|p| PathBuf::from(p).join("Downloads")).unwrap_or_else(|_| PathBuf::from("."))
}

pub fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let clean: String = name.chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let clean = if clean.trim().is_empty() { "download".to_string() } else { clean };
    let p = dir.join(&clean);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match clean.rfind('.') {
        Some(i) if i > 0 => (clean[..i].to_string(), clean[i..].to_string()),
        _ => (clean.clone(), String::new()),
    };
    for n in 1..1000 {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}{ext}", now_ms()))
}

pub fn shell_open(file: &str, params: Option<&str>) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    unsafe {
        let p = params.map(HSTRING::from);
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &HSTRING::from(file),
            p.as_ref().map(|h| windows::core::PCWSTR(h.as_ptr())).unwrap_or(windows::core::PCWSTR::null()),
            windows::core::PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}
