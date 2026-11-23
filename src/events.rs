//! Event queue between WebView2 callbacks and the application state.
//!
//! WebView2 callbacks can fire while we are inside a re-entrant message pump (wry pumps messages
//! while it waits for a controller). To keep the application state single-borrow and simple,
//! every callback only *records* what happened and the app drains the queue from its own handler.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::cmd::Cmd;

pub const WM_APP_EVENTS: u32 = WM_APP + 1;

pub struct ProtoReq {
    pub webview_id: String,
    pub req: http::Request<Vec<u8>>,
    pub responder: wry::RequestAsyncResponder,
}

pub struct ProcInfo {
    pub pid: u32,
    pub kind: i32,
    pub frames: Vec<u32>,
}

pub enum Ev {
    EnvCreated(windows::core::Result<ICoreWebView2Environment>),
    BrowserExited,
    NavStarting { tab: u64, url: String, kind: i32, user: bool },
    NavCompleted { tab: u64, ok: bool, status: i32 },
    SourceChanged { tab: u64 },
    TitleChanged { tab: u64 },
    HistoryChanged { tab: u64 },
    Favicon { tab: u64, pixels: Option<Vec<u8>> },
    Audio { tab: u64 },
    Fullscreen { tab: u64 },
    NewWindow { tab: u64, args: ICoreWebView2NewWindowRequestedEventArgs, deferral: ICoreWebView2Deferral },
    Permission { tab: u64, args: ICoreWebView2PermissionRequestedEventArgs, deferral: ICoreWebView2Deferral },
    Download { tab: u64, args: ICoreWebView2DownloadStartingEventArgs, deferral: ICoreWebView2Deferral },
    DownloadState { id: u64 },
    Ipc { tab: u64, msg: String },
    CloseRequested { tab: u64 },
    ProcessFailed { tab: u64, kind: i32 },
    Zoom { tab: u64 },
    Cmd { tab: u64, cmd: Cmd },
    Suspended { tab: u64, ok: bool },
    Focus { tab: u64 },
    CertError { tab: u64 },
    PerfInfos { req: u64, infos: Vec<ProcInfo> },
    ClearDone { req: u64, ok: bool },
}

static MSG_HWND: AtomicIsize = AtomicIsize::new(0);
static POSTED: AtomicBool = AtomicBool::new(false);
static PROTO: Mutex<Vec<ProtoReq>> = Mutex::new(Vec::new());

thread_local! {
    static QUEUE: RefCell<VecDeque<Ev>> = const { RefCell::new(VecDeque::new()) };
}

pub fn set_msg_hwnd(h: HWND) {
    MSG_HWND.store(h.0 as isize, Ordering::SeqCst);
}

pub fn msg_hwnd() -> HWND {
    HWND(MSG_HWND.load(Ordering::SeqCst) as *mut _)
}

pub fn kick() {
    if !POSTED.swap(true, Ordering::SeqCst) {
        unsafe {
            let _ = PostMessageW(Some(msg_hwnd()), WM_APP_EVENTS, WPARAM(0), LPARAM(0));
        }
    }
}

/// Called when the WM_APP_EVENTS message is received: allows the next push to post again.
pub fn clear_posted() {
    POSTED.store(false, Ordering::SeqCst);
}

pub fn push(ev: Ev) {
    QUEUE.with(|q| q.borrow_mut().push_back(ev));
    kick();
}

pub fn push_proto(r: ProtoReq) {
    if let Ok(mut q) = PROTO.lock() {
        q.push(r);
    }
    kick();
}

pub fn take_events() -> Vec<Ev> {
    POSTED.store(false, Ordering::SeqCst);
    QUEUE.with(|q| q.borrow_mut().drain(..).collect())
}

pub fn take_proto() -> Vec<ProtoReq> {
    PROTO.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

pub fn pending() -> bool {
    QUEUE.with(|q| !q.borrow().is_empty()) || PROTO.lock().map(|q| !q.is_empty()).unwrap_or(false)
}
