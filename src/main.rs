//! Realism Browser — a lightweight Windows browser: native Rust UI (Win32 + Direct2D),
//! web content via Microsoft Edge WebView2 (wry + webview2-com), aggressive RAM management.

#![windows_subsystem = "windows"]

mod app;
mod cmd;
mod db;
mod events;
mod gfx;
mod memory;
mod pages;
mod popup;
mod procmem;
mod theme;
mod ui;
mod update;
mod util;
mod webview;

use std::cell::RefCell;
use std::path::PathBuf;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use app::{with_app, App, Args, Mode, UiTask, APP, TIMER_ANIM, TIMER_DL, TIMER_SESSION, TIMER_TICK};

thread_local! {
    static REPAINT: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

pub fn busy_note_repaint(h: HWND) {
    REPAINT.with(|r| r.borrow_mut().push(h));
}

thread_local! {
    static RESIZE: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

/// A window was resized while the app state was busy: re-apply its size afterwards.
pub fn busy_note_resize(h: HWND) {
    RESIZE.with(|r| r.borrow_mut().push(h));
}

/// Called after every window message: runs deferred UI work (modal dialogs) outside the app borrow.
pub fn after_dispatch() {
    let Some(tasks) = with_app(|a| std::mem::take(&mut a.ui_tasks)) else { return };
    for t in tasks {
        run_ui_task(t);
    }
    let rs: Vec<HWND> = RESIZE.with(|r| std::mem::take(&mut *r.borrow_mut()));
    if !rs.is_empty() {
        with_app(|a| {
            for h in &rs {
                if let Some(wi) = a.win_index(*h) {
                    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(*h) };
                    if dpi != 0 {
                        a.windows[wi].dpi = dpi;
                    }
                    a.sync_size(wi);
                }
            }
        });
    }
    let rp: Vec<HWND> = REPAINT.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for h in rp {
        unsafe {
            let _ = InvalidateRect(Some(h), None, false);
        }
    }
    if events::pending() {
        events::kick();
    }
}

fn run_ui_task(t: UiTask) {
    unsafe {
        match t {
            UiTask::ConfirmClose(hwnd) => {
                let r = MessageBoxW(
                    Some(hwnd),
                    w!("Идёт загрузка файлов. Закрыть браузер и прервать загрузки?"),
                    w!("Realism Browser"),
                    MB_YESNO | MB_ICONQUESTION,
                );
                if r == IDYES {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(1));
                }
            }
            UiTask::PickDownloadDir => {
                let owner = with_app(|a| a.windows.first().map(|w| w.hwnd)).flatten();
                if let Some(dir) = pick_folder(owner) {
                    with_app(|a| {
                        a.s.download_dir = dir.clone();
                        a.db.set_setting("download_dir", &dir);
                    });
                }
            }
            UiTask::SaveAs { args, deferral, tab, suggested } => {
                let info = with_app(|a| (a.loc(tab).map(|(wi, _)| a.windows[wi].hwnd), a.download_dir()));
                let (owner, dir) = info.unwrap_or((None, PathBuf::from(".")));
                match save_dialog(owner, &dir, &suggested) {
                    Some(path) => {
                        with_app(|a| a.start_download(tab, &args, std::path::Path::new(&path)));
                    }
                    None => {
                        let _ = args.SetCancel(true);
                    }
                }
                let _ = deferral.Complete();
            }
        }
    }
}

fn pick_folder(owner: Option<HWND>) -> Option<String> {
    unsafe {
        let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dlg.GetOptions().ok()?;
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dlg.SetTitle(w!("Папка для загрузок"));
        dlg.Show(owner).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn save_dialog(owner: Option<HWND>, dir: &std::path::Path, name: &str) -> Option<String> {
    unsafe {
        let dlg: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let _ = dlg.SetFileName(&HSTRING::from(name));
        if let Ok(folder) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.to_string_lossy().as_ref()), None) {
            let _ = dlg.SetFolder(&folder);
        }
        dlg.Show(owner).ok()?;
        let item = dlg.GetResult().ok()?;
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

unsafe extern "system" fn msg_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let r = match msg {
            events::WM_APP_EVENTS => {
                events::clear_posted();
                with_app(|a| a.drain());
                Some(LRESULT(0))
            }
            WM_TIMER => {
                let id = wp.0;
                with_app(|a| match id {
                    TIMER_TICK => a.tick(),
                    TIMER_ANIM => a.anim_tick(),
                    TIMER_DL => a.poll_downloads(None),
                    TIMER_SESSION => {
                        let _ = KillTimer(Some(hwnd), TIMER_SESSION);
                        a.save_session();
                    }
                    _ => {}
                });
                Some(LRESULT(0))
            }
            WM_COPYDATA => {
                let cds = &*(lp.0 as *const COPYDATASTRUCT);
                if cds.dwData == 0x5245414C && !cds.lpData.is_null() {
                    let bytes = std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) {
                        let urls: Vec<String> =
                            v["urls"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                        let private = v["private"].as_bool().unwrap_or(false);
                        let new_window = v["new_window"].as_bool().unwrap_or(false);
                        with_app(|a| a.open_from_other_instance(urls, private, new_window));
                    }
                }
                Some(LRESULT(1))
            }
            _ => None,
        };
        after_dispatch();
        r.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
    }
}

impl App {
    fn open_from_other_instance(&mut self, urls: Vec<String>, private: bool, new_window: bool) {
        let target = self.windows.iter().rposition(|w| w.private == private);
        match target {
            Some(wi) if !new_window => {
                if urls.is_empty() {
                    self.add_tab(wi, None, None, true);
                }
                for (i, u) in urls.iter().enumerate() {
                    let url = util::normalize_input(u, &self.s.search);
                    self.add_tab(wi, url, None, i == 0);
                }
                if let Some(w) = self.windows.get(wi) {
                    unsafe {
                        if IsIconic(w.hwnd).as_bool() {
                            let _ = ShowWindow(w.hwnd, SW_RESTORE);
                        }
                        let _ = SetForegroundWindow(w.hwnd);
                    }
                }
            }
            _ => self.open_window(private, urls, None),
        }
    }

    /// Keyboard shortcuts for the native parts of the window (address bar, chrome).
    fn global_key(&mut self, hwnd: HWND, vk: u32) -> bool {
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        let Some(wi) = self.win_index(root) else { return false };
        let in_edit = hwnd == self.windows[wi].edit;
        let fs = self.windows[wi].fullscreen.is_some();
        match cmd::map_key(vk, false, fs) {
            Some(cmd::Cmd::Escape) if in_edit && !fs => false,
            Some(c) => {
                self.run_cmd(wi, c);
                true
            }
            None => false,
        }
    }
}

fn parse_args() -> Args {
    let mut a = Args { profile_dir: None, urls: Vec::new(), private: false, no_restore: false, bench: false, mode: None, window_size: None };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        let (k, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut val = || inline.clone().or_else(|| it.next());
        match k.as_str() {
            "--profile-dir" => a.profile_dir = val().map(PathBuf::from),
            "--private" | "--incognito" => a.private = true,
            "--no-restore" => a.no_restore = true,
            "--bench" => {
                a.bench = true;
                a.no_restore = true;
            }
            "--mode" => a.mode = val().map(|m| Mode::parse(&m)),
            "--window-size" => {
                if let Some(v) = val() {
                    let p: Vec<i32> = v.split([',', 'x']).filter_map(|s| s.trim().parse().ok()).collect();
                    if p.len() == 2 {
                        a.window_size = Some((p[0], p[1]));
                    }
                }
            }
            s if s.starts_with("--") => {}
            _ => a.urls.push(arg),
        }
    }
    a
}

fn data_dir(args: &Args) -> PathBuf {
    if let Some(p) = &args.profile_dir {
        return p.clone();
    }
    let base = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
    base.join("RealismBrowser")
}

fn main() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let args = parse_args();
    let dir = data_dir(&args);
    let key = format!("{:016x}", util::hash_str(&dir.to_string_lossy().to_lowercase()));
    let msg_title = HSTRING::from(format!("RealismMsg-{key}"));

    // Single instance per profile: forward the request to the running browser.
    unsafe {
        let _mutex = CreateMutexW(None, true, &HSTRING::from(format!("Local\\RealismBrowser-{key}")));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // The running instance may still be starting up: give it a moment to create its window.
            let mut found = None;
            for _ in 0..25 {
                if let Ok(h) = FindWindowExW(Some(HWND_MESSAGE), None, w!("RealismMsg"), &msg_title) {
                    found = Some(h);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            if let Some(other) = found {
                let payload = serde_json::json!({ "urls": args.urls, "private": args.private, "new_window": args.urls.is_empty() }).to_string();
                let cds = COPYDATASTRUCT { dwData: 0x5245414C, cbData: payload.len() as u32, lpData: payload.as_ptr() as *mut _ };
                let mut res = 0usize;
                let _ = SendMessageTimeoutW(other, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, 5000, Some(&mut res));
                return;
            }
        }
        std::mem::forget(_mutex);
    }

    ui::register_class();
    popup::register_class();
    let msg_hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(msg_proc),
            hInstance: hinst.into(),
            lpszClassName: w!("RealismMsg"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(WINDOW_EX_STYLE::default(), w!("RealismMsg"), &msg_title, WINDOW_STYLE::default(), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None)
            .expect("message window")
    };
    events::set_msg_hwnd(msg_hwnd);

    let app = match App::new(args, dir, msg_hwnd) {
        Ok(a) => a,
        Err(e) => {
            unsafe {
                MessageBoxW(None, &HSTRING::from(format!("Не удалось запустить Realism Browser:\n{e}")), w!("Realism Browser"), MB_ICONERROR);
            }
            return;
        }
    };
    APP.with(|a| *a.borrow_mut() = Some(app));

    with_app(|a| {
        a.load_download_history();
        let urls = std::mem::take(&mut a.args.urls);
        let private = a.args.private;
        let restore = a.s.startup == "restore" && !a.args.no_restore;
        if !urls.is_empty() || private {
            a.open_window(private, urls, None);
        } else if !(restore && a.restore_session()) {
            let place = a.db.get_setting("window");
            a.open_window(false, vec![], place.as_deref());
        }
        if a.policy().prewarm {
            a.start_env();
        }
        unsafe {
            SetTimer(Some(msg_hwnd), TIMER_TICK, 10_000, None);
        }
        // First RAM figure for the toolbar without waiting 10 s.
        a.ram_total = procmem::sample_tree(std::process::id()).total.private_ws;
    });
    after_dispatch();

    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN {
                let (h, vk) = (msg.hwnd, msg.wParam.0 as u32);
                let handled = with_app(|a| a.global_key(h, vk)).unwrap_or(false);
                after_dispatch();
                if handled {
                    continue;
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Release all webviews and the engine before COM goes away.
    let app = APP.with(|a| a.borrow_mut().take());
    drop(app);
    unsafe {
        CoUninitialize();
    }
    let _ = PCWSTR::null();
}
