//! Native browser window: custom title bar with tabs, toolbar, address bar, bars, native new-tab page.

use std::time::Instant;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETCUEBANNER;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::app::{with_app, App, Info, Tab};
use crate::cmd::Cmd;
use crate::db::V;
use crate::gfx::{glyph, Canvas, R};
use crate::popup::{Action, Item};
use crate::theme::{avatar_color, Theme, C};
use crate::util::*;

pub const STRIP_H: f32 = 40.0;
pub const TOOL_H: f32 = 46.0;
pub const BM_H: f32 = 32.0;
pub const INFO_H: f32 = 40.0;
const TAB_MAX: f32 = 232.0;
const TAB_MIN: f32 = 44.0;
const TAB_GAP: f32 = 4.0;
const CLASS: PCWSTR = w!("RealismBrowserWindow");
const EM_SETSEL: u32 = 0x00B1;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    None,
    Caption,
    Tab(usize),
    TabClose(usize),
    TabAudio(usize),
    NewTab,
    Min,
    Max,
    Close,
    Badge,
    Back,
    Fwd,
    Reload,
    AddrIcon,
    Star,
    Zoom,
    Ram,
    Dl,
    Menu,
    Bm(usize),
    Info(usize),
    Tile(usize),
    NtpSearch,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InfoBtn {
    PermAllow,
    PermBlock,
    PermDismiss,
    PopupOpen,
    PopupAllow,
    Reload,
    Close,
}

#[derive(Clone, Default)]
pub struct Layout {
    pub w: f32,
    pub h: f32,
    pub s: f32,
    pub content_y: f32,
    pub tool: R,
    pub bm: R,
    pub info: R,
    pub tabs: Vec<R>,
    pub tab_w: f32,
    pub newtab: R,
    pub min: R,
    pub max: R,
    pub close: R,
    pub badge: R,
    pub back: R,
    pub fwd: R,
    pub reload: R,
    pub addr: R,
    pub addr_icon: R,
    pub edit: R,
    pub star: R,
    pub zoom: R,
    pub ram: R,
    pub dl: R,
    pub menu: R,
    pub info_btns: Vec<(R, InfoBtn, String)>,
    pub bm_items: Vec<R>,
    pub tiles: Vec<R>,
    pub ntp_search: R,
}

thread_local! {
    /// Address-bar colours per window, readable even while the app state is borrowed.
    static EDIT_COLORS: std::cell::RefCell<std::collections::HashMap<isize, (u32, u32, isize)>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn set_edit_colors(hwnd: HWND, th: &Theme, brush: HBRUSH) {
    EDIT_COLORS.with(|m| m.borrow_mut().insert(hwnd.0 as isize, (th.text.colorref(), th.addr.colorref(), brush.0 as isize)));
}

pub struct BmItem {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub folder: bool,
    pub width: f32,
}

pub struct TileItem {
    pub url: String,
    pub title: String,
    pub host: String,
}

pub struct Drag {
    pub tab: u64,
    pub grab_dx: f32,
    pub start_x: f32,
    pub x: f32,
    pub active: bool,
}

pub struct SavedPlacement {
    pub style: isize,
    pub ex_style: isize,
    pub placement: WINDOWPLACEMENT,
}

pub struct Win {
    pub id: u64,
    pub hwnd: HWND,
    pub edit: HWND,
    pub private: bool,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub dpi: u32,
    pub canvas: Option<Canvas>,
    pub hover: Hit,
    pub pressed: Hit,
    pub drag: Option<Drag>,
    pub fullscreen: Option<SavedPlacement>,
    pub html_fullscreen: bool,
    pub f11: bool,
    pub minimized: bool,
    pub editing: bool,
    pub focus_address: bool,
    pub frozen_tab_w: Option<f32>,
    pub lay: Layout,
    pub theme: Theme,
    pub edit_brush: HBRUSH,
    pub edit_font: HFONT,
    pub tracking: bool,
    pub bm_items: Vec<BmItem>,
    pub bm_dirty: bool,
    pub show_bm: bool,
    pub show_dl: bool,
    pub tiles: Vec<TileItem>,
    pub setting_text: bool,
    pub start_maximized: bool,
}

fn loword(v: isize) -> i32 {
    (v & 0xFFFF) as i16 as i32
}
fn hiword(v: isize) -> i32 {
    ((v >> 16) & 0xFFFF) as i16 as i32
}

pub fn register_class() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hIcon: LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        // Frame handling that must work even while the app state is busy.
        if msg == WM_NCCALCSIZE && wp.0 != 0 {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            if style & WS_CAPTION.0 == 0 {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            let params = &mut *(lp.0 as *mut NCCALCSIZE_PARAMS);
            let top = params.rgrc[0].top;
            let r = DefWindowProcW(hwnd, msg, wp, lp);
            if r.0 != 0 {
                return r;
            }
            params.rgrc[0].top = top;
            if IsZoomed(hwnd).as_bool() {
                let dpi = GetDpiForWindow(hwnd);
                params.rgrc[0].top += GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
            }
            return LRESULT(0);
        }
        if msg == WM_CTLCOLOREDIT {
            if let Some((text, bg, brush)) = EDIT_COLORS.with(|m| m.borrow().get(&(hwnd.0 as isize)).copied()) {
                let hdc = HDC(wp.0 as *mut _);
                SetTextColor(hdc, COLORREF(text));
                SetBkColor(hdc, COLORREF(bg));
                return LRESULT(brush);
            }
        }
        let r = with_app(|app| app.win_msg(hwnd, msg, wp, lp));
        let out = match r {
            Some(Some(v)) => v,
            Some(None) => DefWindowProcW(hwnd, msg, wp, lp),
            None => {
                // App busy (nested message pump during webview creation).
                match msg {
                    WM_NCHITTEST => {
                        let d = DefWindowProcW(hwnd, msg, wp, lp);
                        if d.0 as u32 == HTCLIENT {
                            let mut pt = POINT { x: loword(lp.0), y: hiword(lp.0) };
                            let _ = ScreenToClient(hwnd, &mut pt);
                            let s = GetDpiForWindow(hwnd) as f32 / 96.0;
                            if (pt.y as f32) < STRIP_H * s { LRESULT(HTCAPTION as isize) } else { d }
                        } else {
                            d
                        }
                    }
                    WM_PAINT => {
                        crate::busy_note_repaint(hwnd);
                        DefWindowProcW(hwnd, msg, wp, lp)
                    }
                    WM_SIZE | WM_DPICHANGED => {
                        crate::busy_note_resize(hwnd);
                        DefWindowProcW(hwnd, msg, wp, lp)
                    }
                    _ => DefWindowProcW(hwnd, msg, wp, lp),
                }
            }
        };
        crate::after_dispatch();
        out
    }
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    unsafe {
        match msg {
            WM_KEYDOWN => {
                let vk = wp.0 as u16;
                if vk == VK_RETURN.0 || vk == VK_ESCAPE.0 || vk == VK_UP.0 || vk == VK_DOWN.0 {
                    let alt = (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;
                    let handled = with_app(|app| app.edit_key(hwnd, vk, alt)).unwrap_or(false);
                    crate::after_dispatch();
                    if handled {
                        return LRESULT(0);
                    }
                }
            }
            WM_SYSKEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
                let handled = with_app(|app| app.edit_key(hwnd, VK_RETURN.0, true)).unwrap_or(false);
                crate::after_dispatch();
                if handled {
                    return LRESULT(0);
                }
            }
            WM_CHAR if wp.0 == 13 || wp.0 == 27 || wp.0 == 10 => return LRESULT(0),
            WM_LBUTTONDOWN if GetFocus() != hwnd => {
                let _ = SetFocus(Some(hwnd));
                SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                with_app(|app| {
                    if let Some(wi) = app.windows.iter().position(|w| w.edit == hwnd) {
                        app.windows[wi].focus_address = true;
                        app.close_popup();
                        app.redraw(wi);
                    }
                });
                crate::after_dispatch();
                return LRESULT(0);
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                let r = DefSubclassProc(hwnd, msg, wp, lp);
                let focus = msg == WM_SETFOCUS;
                with_app(|app| {
                    if let Some(wi) = app.windows.iter().position(|w| w.edit == hwnd) {
                        if focus {
                            app.windows[wi].focus_address = true;
                        } else if matches!(app.popup.as_ref().map(|p| &p.kind), Some(crate::popup::PopupKind::Suggest)) {
                            app.close_popup();
                        }
                        app.redraw(wi);
                    }
                });
                crate::after_dispatch();
                return r;
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

// ====================================================================== Win

impl Win {
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn client_px(&self) -> RECT {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut rc);
        }
        rc
    }

    pub fn content_px(&self) -> RECT {
        let rc = self.client_px();
        let top = (self.lay.content_y * self.scale()).round() as i32;
        RECT { left: 0, top: top.min(rc.bottom), right: rc.right, bottom: rc.bottom }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn place_webview(&self, ti: usize) {
        if let Some(t) = self.tabs.get(ti) {
            if let Some(wv) = &t.wv {
                let r = self.content_px();
                let _ = wv.wry.set_bounds(wry::Rect {
                    position: wry::dpi::PhysicalPosition::new(r.left, r.top).into(),
                    size: wry::dpi::PhysicalSize::new((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32).into(),
                });
            }
        }
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn set_edit_text(&mut self, s: &str) {
        self.setting_text = true;
        unsafe {
            let _ = SetWindowTextW(self.edit, &HSTRING::from(s));
        }
        self.setting_text = false;
    }

    pub fn edit_text(&self) -> String {
        unsafe {
            let n = GetWindowTextLengthW(self.edit);
            let mut buf = vec![0u16; n as usize + 1];
            let got = GetWindowTextW(self.edit, &mut buf);
            String::from_utf16_lossy(&buf[..got as usize])
        }
    }

    pub fn sync_address(&mut self) {
        if self.editing {
            return;
        }
        let s = self.active_tab().map(|t| if t.url.is_empty() { String::new() } else { display_url(&t.url) }).unwrap_or_default();
        if self.edit_text() != s {
            self.set_edit_text(&s);
        }
        self.update_title();
    }

    pub fn update_title(&self) {
        let t = self.active_tab().map(|t| t.display_title()).unwrap_or_default();
        let suffix = if self.private { "Realism (приватно)" } else { "Realism" };
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &HSTRING::from(format!("{t} — {suffix}")));
        }
    }

    pub fn focus_edit(&mut self) {
        unsafe {
            let _ = SetFocus(Some(self.edit));
            SendMessageW(self.edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
        self.focus_address = true;
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if self.start_maximized { SW_SHOWMAXIMIZED } else { SW_SHOW });
            let _ = UpdateWindow(self.hwnd);
        }
    }

    pub fn destroy(&mut self) {
        self.canvas = None;
        EDIT_COLORS.with(|m| m.borrow_mut().remove(&(self.hwnd.0 as isize)));
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(self.edit_brush.into());
            let _ = DeleteObject(self.edit_font.into());
        }
    }

    pub fn placement_string(&self) -> Option<String> {
        if self.fullscreen.is_some() {
            return None;
        }
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe { GetWindowPlacement(self.hwnd, &mut wp).ok()? };
        let r = wp.rcNormalPosition;
        let max = unsafe { IsZoomed(self.hwnd).as_bool() };
        Some(format!("{},{},{},{},{}", r.left, r.top, r.right - r.left, r.bottom - r.top, max as i32))
    }

    pub fn apply_theme(&mut self, th: &Theme) {
        self.theme = *th;
        unsafe {
            let _ = DeleteObject(self.edit_brush.into());
            self.edit_brush = CreateSolidBrush(COLORREF(th.addr.colorref()));
        }
        set_edit_colors(self.hwnd, th, self.edit_brush);
        set_dwm_colors(self.hwnd, th, self.private);
        self.invalidate();
        unsafe {
            let _ = InvalidateRect(Some(self.edit), None, true);
        }
    }

    pub fn apply_fullscreen(&mut self, on: bool) {
        unsafe {
            if on && self.fullscreen.is_none() {
                let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                let _ = GetWindowPlacement(self.hwnd, &mut wp);
                let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
                let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
                self.fullscreen = Some(SavedPlacement { style, ex_style: ex, placement: wp });
                let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let _ = GetMonitorInfoW(mon, &mut mi);
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE | WS_CLIPCHILDREN).0 as isize);
                let r = mi.rcMonitor;
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOP), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_FRAMECHANGED | SWP_NOOWNERZORDER);
                let _ = ShowWindow(self.edit, SW_HIDE);
            } else if !on {
                if let Some(sp) = self.fullscreen.take() {
                    SetWindowLongPtrW(self.hwnd, GWL_STYLE, sp.style);
                    SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, sp.ex_style);
                    let _ = SetWindowPlacement(self.hwnd, &sp.placement);
                    let _ = SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOOWNERZORDER);
                    let _ = ShowWindow(self.edit, SW_SHOW);
                }
            }
        }
        self.relayout();
    }

    fn info_buttons(&self) -> Option<(String, Vec<(InfoBtn, String)>)> {
        let t = self.active_tab()?;
        if let Some(p) = t.perms.first() {
            let who = if host_of(&p.origin).is_empty() { "Эта страница".to_string() } else { host_of(&p.origin).to_string() };
            let msg = format!("{who} запрашивает доступ: {}", perm_label(p.kind));
            return Some((msg, vec![(InfoBtn::PermAllow, "Разрешить".into()), (InfoBtn::PermBlock, "Блокировать".into()), (InfoBtn::PermDismiss, "Не сейчас".into())]));
        }
        match &t.info {
            Some(Info::PopupBlocked { origin, .. }) => Some((
                if host_of(origin).is_empty() { "Всплывающее окно заблокировано".to_string() } else { format!("Всплывающее окно с {} заблокировано", host_of(origin)) },
                vec![(InfoBtn::PopupOpen, "Открыть".into()), (InfoBtn::PopupAllow, "Всегда разрешать".into()), (InfoBtn::Close, "×".into())],
            )),
            Some(Info::Crashed) => Some(("Процесс страницы аварийно завершился".into(), vec![(InfoBtn::Reload, "Перезагрузить".into()), (InfoBtn::Close, "×".into())])),
            Some(Info::Message(m)) => Some((m.clone(), vec![(InfoBtn::Close, "×".into())])),
            None => None,
        }
    }

    pub fn relayout(&mut self) {
        let rc = self.client_px();
        let s = self.scale();
        let w = rc.right as f32 / s;
        let h = rc.bottom as f32 / s;
        let mut l = Layout { w, h, s, ..Default::default() };
        if self.fullscreen.is_some() {
            l.content_y = 0.0;
            self.lay = l;
            self.place_webview(self.active);
            return;
        }
        l.close = R::new(w - 46.0, 0.0, 46.0, 34.0);
        l.max = R::new(w - 92.0, 0.0, 46.0, 34.0);
        l.min = R::new(w - 138.0, 0.0, 46.0, 34.0);
        let mut x0 = 8.0;
        if self.private {
            l.badge = R::new(8.0, 8.0, 100.0, 26.0);
            x0 = l.badge.right() + 8.0;
        }
        let n = self.tabs.len().max(1) as f32;
        let avail = (l.min.x - 16.0 - x0 - 34.0).max(TAB_MIN);
        let fit = ((avail - TAB_GAP * (n - 1.0)) / n).clamp(TAB_MIN, TAB_MAX);
        let tw = self.frozen_tab_w.map(|f| f.min(TAB_MAX)).unwrap_or(fit);
        l.tab_w = tw;
        for i in 0..self.tabs.len() {
            l.tabs.push(R::new(x0 + i as f32 * (tw + TAB_GAP), 6.0, tw, 31.0));
        }
        let after = x0 + n * (tw + TAB_GAP);
        l.newtab = R::new(after.min(l.min.x - 36.0), 7.5, 28.0, 28.0);
        let ty = STRIP_H;
        l.tool = R::new(0.0, ty, w, TOOL_H);
        l.back = R::new(8.0, ty + 6.0, 34.0, 34.0);
        l.fwd = R::new(44.0, ty + 6.0, 34.0, 34.0);
        l.reload = R::new(80.0, ty + 6.0, 34.0, 34.0);
        l.menu = R::new(w - 42.0, ty + 6.0, 34.0, 34.0);
        l.dl = if self.show_dl { R::new(l.menu.x - 36.0, ty + 6.0, 34.0, 34.0) } else { R::new(l.menu.x, ty + 6.0, 0.0, 34.0) };
        let ram_right = if self.show_dl { l.dl.x } else { l.menu.x } - 6.0;
        l.ram = if w > 640.0 { R::new(ram_right - 86.0, ty + 8.0, 86.0, 30.0) } else { R::new(ram_right, ty + 8.0, 0.0, 30.0) };
        let ax = 122.0;
        l.addr = R::new(ax, ty + 6.0, (l.ram.x - 8.0 - ax).max(120.0), 34.0);
        l.addr_icon = R::new(l.addr.x + 4.0, l.addr.y + 3.0, 28.0, 28.0);
        l.star = R::new(l.addr.right() - 32.0, l.addr.y + 3.0, 28.0, 28.0);
        let zoom = self.active_tab().map(|t| t.zoom).unwrap_or(1.0);
        l.zoom = if (zoom - 1.0).abs() > 0.001 { R::new(l.star.x - 54.0, l.addr.y + 6.0, 50.0, 22.0) } else { R::new(l.star.x, 0.0, 0.0, 0.0) };
        let edit_h = 20.0;
        let ex = l.addr.x + 38.0;
        let er = if l.zoom.w > 0.0 { l.zoom.x } else { l.star.x } - 6.0;
        l.edit = R::new(ex, l.addr.y + (34.0 - edit_h) / 2.0, (er - ex).max(10.0), edit_h);
        let mut y = ty + TOOL_H;
        if self.show_bm {
            l.bm = R::new(0.0, y, w, BM_H);
            let mut x = 8.0;
            for it in &self.bm_items {
                let iw = it.width.min(200.0);
                if x + iw > w - 8.0 {
                    break;
                }
                l.bm_items.push(R::new(x, y + 4.0, iw, 24.0));
                x += iw + 2.0;
            }
            y += BM_H;
        }
        if let Some((_, btns)) = self.info_buttons() {
            l.info = R::new(0.0, y, w, INFO_H);
            let mut x = w - 12.0;
            for (b, label) in btns.into_iter().rev() {
                let bw = if label == "×" { 28.0 } else { label.chars().count() as f32 * 7.4 + 26.0 };
                x -= bw;
                l.info_btns.insert(0, (R::new(x, y + 6.0, bw, 28.0), b, label));
                x -= 8.0;
            }
            y += INFO_H;
        }
        l.content_y = y;
        // Native new tab page
        if self.active_tab().map(|t| t.is_ntp()).unwrap_or(false) {
            let ch = h - y;
            let top = y + (ch * 0.14).clamp(24.0, 120.0);
            let sw = (w - 64.0).min(580.0);
            l.ntp_search = R::new((w - sw) / 2.0, top + 104.0, sw, 48.0);
            let cols = if w > 640.0 { 4 } else { 2 };
            let (tw, th, gap) = (126.0, 104.0, 14.0);
            let gw = cols as f32 * tw + (cols - 1) as f32 * gap;
            let gx = (w - gw) / 2.0;
            let gy = l.ntp_search.bottom() + 36.0;
            for i in 0..self.tiles.len().min(8) {
                let (c, r) = (i % cols, i / cols);
                let tile = R::new(gx + c as f32 * (tw + gap), gy + r as f32 * (th + gap), tw, th);
                if tile.bottom() < h - 30.0 {
                    l.tiles.push(tile);
                }
            }
        }
        // Address edit placement
        let er = l.edit.px(s);
        unsafe {
            let _ = SetWindowPos(self.edit, None, er.left, er.top, er.right - er.left, er.bottom - er.top, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        self.lay = l;
        self.place_webview(self.active);
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        let l = &self.lay;
        if self.fullscreen.is_some() {
            return Hit::None;
        }
        if l.close.contains(x, y) {
            return Hit::Close;
        }
        if l.max.contains(x, y) {
            return Hit::Max;
        }
        if l.min.contains(x, y) {
            return Hit::Min;
        }
        if self.private && l.badge.contains(x, y) {
            return Hit::Badge;
        }
        for (i, r) in l.tabs.iter().enumerate() {
            let ax = self.tabs.get(i).map(|t| if t.x_anim.is_nan() { r.x } else { t.x_anim }).unwrap_or(r.x);
            let rr = R::new(ax, r.y, r.w, r.h);
            if rr.contains(x, y) {
                let (close, audio) = tab_parts(&rr, self.tabs.get(i), i == self.active, self.hover == Hit::Tab(i) || self.hover == Hit::TabClose(i));
                if close.w > 0.0 && close.contains(x, y) {
                    return Hit::TabClose(i);
                }
                if audio.w > 0.0 && audio.contains(x, y) {
                    return Hit::TabAudio(i);
                }
                return Hit::Tab(i);
            }
        }
        if l.newtab.contains(x, y) {
            return Hit::NewTab;
        }
        if y < STRIP_H {
            return Hit::Caption;
        }
        for (r, h) in [(l.back, Hit::Back), (l.fwd, Hit::Fwd), (l.reload, Hit::Reload), (l.menu, Hit::Menu), (l.dl, Hit::Dl), (l.ram, Hit::Ram)] {
            if r.w > 0.0 && r.contains(x, y) {
                return h;
            }
        }
        if l.star.contains(x, y) {
            return Hit::Star;
        }
        if l.zoom.w > 0.0 && l.zoom.contains(x, y) {
            return Hit::Zoom;
        }
        if l.addr_icon.contains(x, y) {
            return Hit::AddrIcon;
        }
        for (i, r) in l.bm_items.iter().enumerate() {
            if r.contains(x, y) {
                return Hit::Bm(i);
            }
        }
        for (i, (r, _, _)) in l.info_btns.iter().enumerate() {
            if r.contains(x, y) {
                return Hit::Info(i);
            }
        }
        if y >= l.content_y {
            for (i, r) in l.tiles.iter().enumerate() {
                if r.contains(x, y) {
                    return Hit::Tile(i);
                }
            }
            if l.ntp_search.w > 0.0 && l.ntp_search.contains(x, y) {
                return Hit::NtpSearch;
            }
        }
        Hit::None
    }

    /// One animation step. Returns true while something is still moving.
    pub fn animate(&mut self) -> bool {
        let mut busy = false;
        let drag_tab = self.drag.as_ref().filter(|d| d.active).map(|d| (d.tab, d.x - d.grab_dx));
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let Some(target) = self.lay.tabs.get(i).map(|r| r.x) else { continue };
            if let Some((id, x)) = drag_tab {
                if id == t.id {
                    t.x_anim = x;
                    continue;
                }
            }
            if t.x_anim.is_nan() {
                t.x_anim = target;
            }
            let d = target - t.x_anim;
            if d.abs() < 0.5 {
                t.x_anim = target;
            } else {
                t.x_anim += d * 0.32;
                busy = true;
            }
            if t.loading {
                busy = true;
            }
        }
        if self.tabs.iter().any(|t| t.loading) {
            busy = true;
        }
        if busy {
            self.invalidate();
        }
        busy
    }
}

pub fn perm_label(kind: i32) -> &'static str {
    match kind {
        1 => "микрофон",
        2 => "камера",
        3 => "местоположение",
        4 => "уведомления",
        5 => "датчики",
        6 => "чтение буфера обмена",
        7 => "автоматические загрузки",
        8 => "доступ к файлам",
        9 => "автовоспроизведение",
        10 => "локальные шрифты",
        11 => "MIDI",
        12 => "управление окнами",
        _ => "разрешение",
    }
}

/// (close button, audio button) rects inside a tab.
fn tab_parts(r: &R, t: Option<&Tab>, active: bool, hovered: bool) -> (R, R) {
    let show_close = active || hovered || r.w >= 110.0;
    let close = if show_close && r.w >= 60.0 { R::new(r.right() - 26.0, r.y + 5.5, 20.0, 20.0) } else { R::default() };
    let audio = match t {
        Some(t) if (t.audio || t.muted) && r.w >= 84.0 => {
            let ax = if close.w > 0.0 { close.x - 22.0 } else { r.right() - 26.0 };
            R::new(ax, r.y + 5.5, 20.0, 20.0)
        }
        _ => R::default(),
    };
    (close, audio)
}

fn set_dwm_colors(hwnd: HWND, th: &Theme, private: bool) {
    unsafe {
        let dark: u32 = th.dark as u32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
        let c = if private { th.strip_private } else { th.strip };
        let border = c.mix(th.text, 0.10).colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as _, 4);
        let cap = c.colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &cap as *const _ as _, 4);
    }
}

// ====================================================================== App: window creation & messages

impl App {
    pub fn create_window(&mut self, private: bool, place: Option<&str>) -> usize {
        let id = self.new_id();
        let mut x = CW_USEDEFAULT;
        let mut y = CW_USEDEFAULT;
        let mut cw = 1280;
        let mut ch = 820;
        let mut maximized = false;
        if let Some((w, h)) = self.args.window_size {
            cw = w;
            ch = h;
            x = 0;
            y = 0;
        } else if let Some(p) = place {
            let v: Vec<i32> = p.split(',').filter_map(|s| s.parse().ok()).collect();
            if v.len() == 5 && v[2] > 300 && v[3] > 200 {
                x = v[0];
                y = v[1];
                cw = v[2];
                ch = v[3];
                maximized = v[4] != 0;
            }
        } else if let Some(prev) = self.windows.last() {
            let mut r = RECT::default();
            unsafe {
                let _ = GetWindowRect(prev.hwnd, &mut r);
            }
            x = r.left + 32;
            y = r.top + 32;
            cw = r.right - r.left;
            ch = r.bottom - r.top;
        } else {
            unsafe {
                let dpi = GetDpiForSystem();
                let s = dpi as f32 / 96.0;
                cw = (1280.0 * s) as i32;
                ch = (820.0 * s) as i32;
                let sw = GetSystemMetrics(SM_CXSCREEN);
                let sh = GetSystemMetrics(SM_CYSCREEN);
                cw = cw.min(sw - 40);
                ch = ch.min(sh - 60);
                x = (sw - cw) / 2;
                y = (sh - ch) / 2;
            }
        }
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                CLASS,
                w!("Realism"),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                x,
                y,
                cw,
                ch,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .expect("CreateWindowExW")
        };
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let th = self.theme;
        let edit = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE((ES_AUTOHSCROLL | ES_LEFT) as u32),
                0,
                0,
                10,
                10,
                Some(hwnd),
                None,
                Some(hinst.into()),
                None,
            )
            .expect("edit")
        };
        let font = make_font(dpi);
        unsafe {
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            let cue = HSTRING::from(format!("Поиск в {} или адрес сайта", engine_name(&self.s.search)));
            SendMessageW(edit, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(cue.as_ptr() as isize)));
            let _ = SetWindowSubclass(edit, Some(edit_proc), 1, 0);
            // Apply the custom frame (removes the caption).
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER);
        }
        set_dwm_colors(hwnd, &th, private);
        let mut win = Win {
            id,
            hwnd,
            edit,
            private,
            tabs: Vec::new(),
            active: 0,
            dpi,
            canvas: None,
            hover: Hit::None,
            pressed: Hit::None,
            drag: None,
            fullscreen: None,
            html_fullscreen: false,
            f11: false,
            minimized: false,
            editing: false,
            focus_address: false,
            frozen_tab_w: None,
            lay: Layout::default(),
            theme: th,
            edit_brush: unsafe { CreateSolidBrush(COLORREF(th.addr.colorref())) },
            edit_font: font,
            tracking: false,
            bm_items: Vec::new(),
            bm_dirty: true,
            show_bm: self.s.bookmarks_bar,
            show_dl: self.downloads.iter().any(|d| d.tab != 0),
            tiles: Vec::new(),
            setting_text: false,
            start_maximized: maximized,
        };
        set_edit_colors(win.hwnd, &th, win.edit_brush);
        win.relayout();
        self.windows.push(win);
        let wi = self.windows.len() - 1;
        self.rebuild_bm(wi);
        self.refresh_ntp(wi);
        wi
    }

    pub fn rebuild_bm(&mut self, wi: usize) {
        let show = self.s.bookmarks_bar;
        let rows = if show {
            self.db.query("SELECT id,title,url,is_folder FROM bookmarks WHERE parent=1 ORDER BY pos,id LIMIT 60", &[], |r| (r.int(0), r.text(1), r.text(2), r.int(3) != 0))
        } else {
            Vec::new()
        };
        let items = rows
            .into_iter()
            .map(|(id, title, url, folder)| {
                let label = if title.is_empty() { host_of(&url).to_string() } else { title };
                let width = self.gfx.measure(&label, &self.gfx.fonts.small).min(150.0) + 36.0;
                BmItem { id, title: label, url, folder, width }
            })
            .collect();
        let w = &mut self.windows[wi];
        w.bm_items = items;
        w.show_bm = show;
        w.bm_dirty = false;
        w.relayout();
    }

    pub fn refresh_ntp(&mut self, wi: usize) {
        if !self.windows[wi].active_tab().map(|t| t.is_ntp()).unwrap_or(false) {
            return;
        }
        let rows = if self.windows[wi].private {
            Vec::new()
        } else {
            self.db.query(
                "SELECT url,title FROM history WHERE url NOT LIKE 'https://realism.localhost%' ORDER BY visits DESC, last_visit DESC LIMIT 40",
                &[],
                |r| (r.text(0), r.text(1)),
            )
        };
        let mut seen = std::collections::HashSet::new();
        let mut tiles = Vec::new();
        for (url, title) in rows {
            let host = host_of(&url).trim_start_matches("www.").to_string();
            if host.is_empty() || !seen.insert(host.clone()) {
                continue;
            }
            tiles.push(TileItem { url, title, host });
            if tiles.len() == 8 {
                break;
            }
        }
        self.windows[wi].tiles = tiles;
        self.windows[wi].relayout();
    }

    /// Main window message handler. `None` -> DefWindowProc.
    pub fn win_msg(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let wi = self.win_index(hwnd)?;
        unsafe {
            match msg {
                WM_PAINT => {
                    self.paint(wi);
                    Some(LRESULT(0))
                }
                WM_ERASEBKGND => Some(LRESULT(1)),
                WM_SIZE => {
                    self.sync_size(wi);
                    Some(LRESULT(0))
                }
                WM_MOVE | WM_MOVING => {
                    let w = &self.windows[wi];
                    if let Some(wv) = w.active_tab().and_then(|t| t.wv.as_ref()) {
                        let _ = wv.ctrl.NotifyParentWindowPositionChanged();
                    }
                    self.close_popup();
                    None
                }
                WM_GETMINMAXINFO => {
                    let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                    let s = self.windows[wi].scale();
                    mmi.ptMinTrackSize = POINT { x: (520.0 * s) as i32, y: (360.0 * s) as i32 };
                    Some(LRESULT(0))
                }
                WM_DPICHANGED => {
                    let w = &mut self.windows[wi];
                    w.dpi = (wp.0 & 0xFFFF) as u32;
                    let r = &*(lp.0 as *const RECT);
                    let _ = DeleteObject(w.edit_font.into());
                    w.edit_font = make_font(w.dpi);
                    SendMessageW(w.edit, WM_SETFONT, Some(WPARAM(w.edit_font.0 as usize)), Some(LPARAM(1)));
                    let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                    w.canvas = None;
                    self.rebuild_bm(wi);
                    self.windows[wi].invalidate();
                    Some(LRESULT(0))
                }
                WM_NCHITTEST => {
                    let d = DefWindowProcW(hwnd, msg, wp, lp);
                    if d.0 as u32 != HTCLIENT {
                        return Some(d);
                    }
                    let w = &self.windows[wi];
                    let mut pt = POINT { x: loword(lp.0), y: hiword(lp.0) };
                    let _ = ScreenToClient(hwnd, &mut pt);
                    let s = w.scale();
                    let (x, y) = (pt.x as f32 / s, pt.y as f32 / s);
                    if w.fullscreen.is_none() && !IsZoomed(hwnd).as_bool() {
                        let border = GetSystemMetricsForDpi(SM_CYSIZEFRAME, w.dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, w.dpi);
                        if pt.y < border {
                            let rc = w.client_px();
                            if pt.x < border * 2 {
                                return Some(LRESULT(HTTOPLEFT as isize));
                            }
                            if pt.x > rc.right - border * 2 {
                                return Some(LRESULT(HTTOPRIGHT as isize));
                            }
                            return Some(LRESULT(HTTOP as isize));
                        }
                    }
                    let r = match w.hit(x, y) {
                        Hit::Caption => HTCAPTION,
                        Hit::Min => HTMINBUTTON,
                        Hit::Max => HTMAXBUTTON,
                        Hit::Close => HTCLOSE,
                        _ => HTCLIENT,
                    };
                    Some(LRESULT(r as isize))
                }
                WM_NCMOUSEMOVE => {
                    let h = match wp.0 as u32 {
                        HTMINBUTTON => Hit::Min,
                        HTMAXBUTTON => Hit::Max,
                        HTCLOSE => Hit::Close,
                        _ => Hit::Caption,
                    };
                    let w = &mut self.windows[wi];
                    if w.hover != h {
                        w.hover = h;
                        w.invalidate();
                    }
                    if !w.tracking {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE | TME_NONCLIENT,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        w.tracking = true;
                    }
                    None
                }
                WM_NCLBUTTONDOWN => {
                    dlog(&format!("NCLBUTTONDOWN ht={}", wp.0));
                    self.close_popup();
                    let h = match wp.0 as u32 {
                        HTMINBUTTON => Hit::Min,
                        HTMAXBUTTON => Hit::Max,
                        HTCLOSE => Hit::Close,
                        _ => return None,
                    };
                    self.windows[wi].pressed = h;
                    self.windows[wi].invalidate();
                    Some(LRESULT(0))
                }
                WM_NCLBUTTONUP => {
                    dlog(&format!("NCLBUTTONUP ht={} pressed={:?}", wp.0, self.windows[wi].pressed));
                    let h = match wp.0 as u32 {
                        HTMINBUTTON => Hit::Min,
                        HTMAXBUTTON => Hit::Max,
                        HTCLOSE => Hit::Close,
                        _ => return None,
                    };
                    let pressed = std::mem::replace(&mut self.windows[wi].pressed, Hit::None);
                    if pressed == h {
                        match h {
                            // Posted (not ShowWindow here): the resulting WM_SIZE must arrive when the
                            // app state is free, otherwise the new size would be ignored.
                            Hit::Min => {
                                let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(SC_MINIMIZE as usize), LPARAM(0));
                            }
                            Hit::Max => {
                                let cmd = if IsZoomed(hwnd).as_bool() { SC_RESTORE } else { SC_MAXIMIZE };
                                let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(cmd as usize), LPARAM(0));
                            }
                            Hit::Close => {
                                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                            }
                            _ => {}
                        }
                    }
                    self.windows[wi].invalidate();
                    Some(LRESULT(0))
                }
                WM_NCMOUSELEAVE | WM_MOUSELEAVE => {
                    let w = &mut self.windows[wi];
                    w.tracking = false;
                    w.hover = Hit::None;
                    if msg == WM_MOUSELEAVE && w.frozen_tab_w.take().is_some() {
                        w.relayout();
                        self.ensure_anim();
                    }
                    self.windows[wi].invalidate();
                    None
                }
                WM_MOUSEMOVE => {
                    self.on_mouse_move(wi, loword(lp.0), hiword(lp.0));
                    Some(LRESULT(0))
                }
                WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                    self.on_lbutton_down(wi, loword(lp.0), hiword(lp.0), msg == WM_LBUTTONDBLCLK);
                    Some(LRESULT(0))
                }
                WM_LBUTTONUP => {
                    dlog("LBUTTONUP (client)");
                    self.on_lbutton_up(wi, loword(lp.0), hiword(lp.0));
                    Some(LRESULT(0))
                }
                WM_MBUTTONUP => {
                    self.on_mbutton_up(wi, loword(lp.0), hiword(lp.0));
                    Some(LRESULT(0))
                }
                WM_RBUTTONUP => {
                    self.on_rbutton_up(wi, loword(lp.0), hiword(lp.0));
                    Some(LRESULT(0))
                }
                WM_MOUSEWHEEL => {
                    // Scroll over the tab strip switches tabs.
                    let mut pt = POINT { x: loword(lp.0), y: hiword(lp.0) };
                    let _ = ScreenToClient(hwnd, &mut pt);
                    let s = self.windows[wi].scale();
                    if (pt.y as f32 / s) < STRIP_H {
                        let delta = hiword(wp.0 as isize);
                        let n = self.windows[wi].tabs.len();
                        let a = self.windows[wi].active;
                        let next = if delta < 0 { (a + 1).min(n - 1) } else { a.saturating_sub(1) };
                        if next != a {
                            self.activate(wi, next);
                        }
                    }
                    Some(LRESULT(0))
                }
                WM_COMMAND => {
                    let code = ((wp.0 >> 16) & 0xFFFF) as u32;
                    if lp.0 as *mut core::ffi::c_void == self.windows[wi].edit.0 && code == EN_CHANGE {
                        if !self.windows[wi].setting_text && GetFocus() == self.windows[wi].edit {
                            self.windows[wi].editing = true;
                            // Warm up the engine while the user is still typing.
                            self.start_env();
                            self.update_suggestions(wi);
                        }
                    }
                    Some(LRESULT(0))
                }
                WM_ACTIVATE => {
                    let active = (wp.0 & 0xFFFF) != 0;
                    if !active {
                        self.close_popup();
                    } else {
                        let w = &mut self.windows[wi];
                        if w.focus_address || w.active_tab().map(|t| t.wv.is_none()).unwrap_or(true) {
                            let _ = SetFocus(Some(w.edit));
                        } else {
                            self.focus_web(wi);
                        }
                    }
                    Some(LRESULT(0))
                }
                WM_SETTINGCHANGE => {
                    if self.s.theme == "system" {
                        let t = self.s.theme.clone();
                        self.set_theme(&t);
                    }
                    None
                }
                WM_CLOSE => {
                    let force = lp.0 == 1;
                    let running = self.downloads.iter().any(|d| d.state == crate::app::DlState::Running);
                    let last_normal = !self.windows[wi].private && self.windows.iter().filter(|w| !w.private).count() == 1;
                    if running && last_normal && !force {
                        self.ui_tasks.push(crate::app::UiTask::ConfirmClose(hwnd));
                    } else {
                        self.close_window(wi);
                    }
                    Some(LRESULT(0))
                }
                _ => None,
            }
        }
    }

    // ------------------------------------------------------------------ mouse

    fn dip(&self, wi: usize, x: i32, y: i32) -> (f32, f32) {
        let s = self.windows[wi].scale();
        (x as f32 / s, y as f32 / s)
    }

    fn on_mouse_move(&mut self, wi: usize, px: i32, py: i32) {
        let (x, y) = self.dip(wi, px, py);
        let w = &mut self.windows[wi];
        if !w.tracking {
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: w.hwnd, dwHoverTime: 0 };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            w.tracking = true;
        }
        if let Some(d) = &mut w.drag {
            d.x = x;
            if !d.active && (x - d.start_x).abs() > 6.0 {
                d.active = true;
            }
            if d.active {
                let id = d.tab;
                let grab = d.grab_dx;
                let n = w.tabs.len();
                let x0 = w.lay.tabs.first().map(|r| r.x).unwrap_or(8.0);
                let step = w.lay.tab_w + TAB_GAP;
                let target = (((x - grab) - x0) / step).round().clamp(0.0, (n - 1) as f32) as usize;
                if let Some(cur) = w.tabs.iter().position(|t| t.id == id) {
                    if cur != target {
                        let t = w.tabs.remove(cur);
                        w.tabs.insert(target, t);
                        w.active = target;
                        w.relayout();
                        self.mark_session();
                    }
                }
                self.ensure_anim();
                self.windows[wi].invalidate();
                return;
            }
        }
        let w = &mut self.windows[wi];
        let h = w.hit(x, y);
        if h != w.hover {
            w.hover = h;
            w.invalidate();
        }
    }

    fn on_lbutton_down(&mut self, wi: usize, px: i32, py: i32, dbl: bool) {
        let (x, y) = self.dip(wi, px, py);
        let h = self.windows[wi].hit(x, y);
        let popup_was = self.popup.as_ref().map(|p| p.anchor);
        self.close_popup();
        let w = &mut self.windows[wi];
        // Clicking the button that owns the open popup only closes it (toggle).
        w.pressed = if popup_was == Some(h) && matches!(h, Hit::Menu | Hit::AddrIcon) { Hit::None } else { h };
        match h {
            Hit::Tab(i) => {
                if dbl {
                    return;
                }
                let tx = w.tabs[i].x_anim;
                let tx = if tx.is_nan() { w.lay.tabs[i].x } else { tx };
                w.drag = Some(Drag { tab: w.tabs[i].id, grab_dx: x - tx, start_x: x, x, active: false });
                unsafe {
                    SetCapture(w.hwnd);
                }
                if i != w.active {
                    self.activate(wi, i);
                }
            }
            Hit::TabAudio(i) => {
                if let Some(wv) = &w.tabs[i].wv {
                    if let Ok(c8) = windows::core::Interface::cast::<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_8>(&wv.core) {
                        let muted = w.tabs[i].muted;
                        unsafe {
                            let _ = c8.SetIsMuted(!muted);
                        }
                    }
                }
            }
            Hit::NewTab => {
                if !dbl {
                    self.run_cmd(wi, Cmd::NewTab);
                }
            }
            Hit::NtpSearch => self.windows[wi].focus_edit(),
            _ => {}
        }
        self.windows[wi].invalidate();
    }

    fn on_lbutton_up(&mut self, wi: usize, px: i32, py: i32) {
        let (x, y) = self.dip(wi, px, py);
        let w = &mut self.windows[wi];
        if let Some(d) = w.drag.take() {
            unsafe {
                let _ = ReleaseCapture();
            }
            if d.active {
                w.pressed = Hit::None;
                self.ensure_anim();
                return;
            }
        }
        let h = w.hit(x, y);
        let pressed = std::mem::replace(&mut w.pressed, Hit::None);
        if h != pressed {
            w.invalidate();
            return;
        }
        let ti = w.active;
        match h {
            Hit::TabClose(i) => {
                // Keep tab widths stable while the mouse is on the strip (close several in a row).
                let tw = w.lay.tab_w;
                w.frozen_tab_w = Some(tw);
                self.close_tab(wi, i);
            }
            Hit::Back => self.run_cmd(wi, Cmd::Back),
            Hit::Fwd => self.run_cmd(wi, Cmd::Forward),
            Hit::Reload => {
                let loading = self.windows[wi].tabs.get(ti).map(|t| t.loading).unwrap_or(false);
                self.run_cmd(wi, if loading { Cmd::Stop } else { Cmd::Reload });
            }
            Hit::Star => self.run_cmd(wi, Cmd::BookmarkPage),
            Hit::Zoom => self.run_cmd(wi, Cmd::ZoomReset),
            Hit::Ram => self.run_cmd(wi, Cmd::Performance),
            Hit::Dl => self.run_cmd(wi, Cmd::Downloads),
            Hit::Menu => self.show_main_menu(wi),
            Hit::AddrIcon => self.show_site_info(wi),
            Hit::Badge => {}
            Hit::Bm(i) => {
                let Some(it) = self.windows[wi].bm_items.get(i) else { return };
                if it.folder {
                    let (id, r) = (it.id, self.windows[wi].lay.bm_items[i]);
                    self.show_bm_folder(wi, id, r);
                } else {
                    let url = it.url.clone();
                    self.navigate_tab(wi, ti, Some(url));
                }
            }
            Hit::Info(i) => self.info_action(wi, i),
            Hit::Tile(i) => {
                if let Some(t) = self.windows[wi].tiles.get(i) {
                    let url = t.url.clone();
                    self.navigate_tab(wi, ti, Some(url));
                }
            }
            _ => {}
        }
        if wi < self.windows.len() {
            self.windows[wi].invalidate();
        }
    }

    fn on_mbutton_up(&mut self, wi: usize, px: i32, py: i32) {
        let (x, y) = self.dip(wi, px, py);
        match self.windows[wi].hit(x, y) {
            Hit::Tab(i) | Hit::TabClose(i) | Hit::TabAudio(i) => {
                let tw = self.windows[wi].lay.tab_w;
                self.windows[wi].frozen_tab_w = Some(tw);
                self.close_tab(wi, i);
            }
            Hit::Bm(i) => {
                if let Some(it) = self.windows[wi].bm_items.get(i) {
                    if !it.folder {
                        let u = it.url.clone();
                        self.add_tab(wi, Some(u), None, false);
                    }
                }
            }
            Hit::Tile(i) => {
                if let Some(t) = self.windows[wi].tiles.get(i) {
                    let u = t.url.clone();
                    self.add_tab(wi, Some(u), None, false);
                }
            }
            _ => {}
        }
    }

    fn on_rbutton_up(&mut self, wi: usize, px: i32, py: i32) {
        let (x, y) = self.dip(wi, px, py);
        match self.windows[wi].hit(x, y) {
            Hit::Tab(i) | Hit::TabClose(i) | Hit::TabAudio(i) => self.show_tab_menu(wi, i),
            Hit::Bm(i) => {
                if let Some(it) = self.windows[wi].bm_items.get(i) {
                    let mut items = Vec::new();
                    if !it.folder {
                        items.push(Item::new("Открыть в новой вкладке", Action::OpenNewTab(it.url.clone())));
                    }
                    items.push(Item::new("Удалить", Action::BmDelete(it.id)));
                    items.push(Item::sep());
                    items.push(Item::new("Менеджер закладок", Action::Cmd(Cmd::Bookmarks)));
                    let pt = self.screen_pt(wi, x, y);
                    self.show_popup(wi, crate::popup::PopupKind::Menu, items, pt, Hit::Bm(i), 220.0);
                }
            }
            _ => {}
        }
    }

    pub fn screen_pt(&self, wi: usize, x: f32, y: f32) -> POINT {
        let w = &self.windows[wi];
        let s = w.scale();
        let mut pt = POINT { x: (x * s) as i32, y: (y * s) as i32 };
        unsafe {
            let _ = ClientToScreen(w.hwnd, &mut pt);
        }
        pt
    }

    fn info_action(&mut self, wi: usize, i: usize) {
        let Some((_, b, _)) = self.windows[wi].lay.info_btns.get(i).cloned() else { return };
        let ti = self.windows[wi].active;
        match b {
            InfoBtn::PermAllow => self.answer_permission(wi, true, true),
            InfoBtn::PermBlock => self.answer_permission(wi, false, true),
            InfoBtn::PermDismiss => self.answer_permission(wi, false, false),
            InfoBtn::PopupOpen | InfoBtn::PopupAllow => {
                if let Some(Info::PopupBlocked { origin, url }) = self.windows[wi].tabs[ti].info.take() {
                    if b == InfoBtn::PopupAllow && !self.windows[wi].private {
                        self.db.run("INSERT OR IGNORE INTO popups_allowed(origin) VALUES(?)", &[V::T(&origin)]);
                    }
                    if !url.is_empty() && url != "about:blank" {
                        self.add_tab(wi, Some(url), Some(ti + 1), true);
                    }
                }
            }
            InfoBtn::Reload => {
                self.windows[wi].tabs[ti].info = None;
                self.run_cmd(wi, Cmd::Reload);
            }
            InfoBtn::Close => {
                self.windows[wi].tabs[ti].info = None;
            }
        }
        if wi < self.windows.len() {
            self.windows[wi].relayout();
            self.redraw(wi);
        }
    }

    // ------------------------------------------------------------------ address bar keys

    pub fn edit_key(&mut self, edit: HWND, vk: u16, alt: bool) -> bool {
        let Some(wi) = self.windows.iter().position(|w| w.edit == edit) else { return false };
        let sugg = matches!(self.popup.as_ref().map(|p| &p.kind), Some(crate::popup::PopupKind::Suggest));
        if vk == VK_RETURN.0 {
            let mut text = self.windows[wi].edit_text();
            if sugg {
                if let Some(Action::Suggest(u)) = self.popup.as_ref().and_then(|p| p.selected_action()) {
                    text = u;
                }
            }
            self.close_popup();
            self.windows[wi].editing = false;
            self.navigate_input(wi, &text, alt);
            return true;
        }
        if vk == VK_ESCAPE.0 {
            if sugg {
                self.close_popup();
            } else {
                let w = &mut self.windows[wi];
                w.editing = false;
                w.sync_address();
                unsafe {
                    SendMessageW(w.edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
                }
            }
            return true;
        }
        if (vk == VK_DOWN.0 || vk == VK_UP.0) && sugg {
            let url = self.popup.as_mut().and_then(|p| p.move_sel(if vk == VK_DOWN.0 { 1 } else { -1 }));
            if let Some(u) = url {
                let w = &mut self.windows[wi];
                w.set_edit_text(&display_url(&u));
                unsafe {
                    let n = GetWindowTextLengthW(w.edit) as isize;
                    SendMessageW(w.edit, EM_SETSEL, Some(WPARAM(n as usize)), Some(LPARAM(n)));
                }
            }
            return true;
        }
        false
    }

    pub fn update_suggestions(&mut self, wi: usize) {
        let text = self.windows[wi].edit_text();
        let t = text.trim().to_string();
        if t.is_empty() {
            self.close_popup();
            return;
        }
        let mut items = Vec::new();
        let target = normalize_input(&t, &self.s.search).unwrap_or_default();
        let is_search = target.contains("/search?q=") || target.contains("duckduckgo.com/?q=");
        if is_search {
            items.push(Item::suggestion(&format!("{t} — поиск в {}", engine_name(&self.s.search)), "", glyph::SEARCH, Action::Suggest(target.clone())));
        } else {
            items.push(Item::suggestion(&t, "Перейти", glyph::GLOBE, Action::Suggest(target.clone())));
        }
        if !self.windows[wi].private {
            let like = format!("%{}%", t.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            let bms = self.db.query(
                "SELECT title,url FROM bookmarks WHERE is_folder=0 AND (url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\') LIMIT 3",
                &[V::T(&like)],
                |r| (r.text(0), r.text(1)),
            );
            for (title, url) in bms {
                items.push(Item::suggestion(&title, &display_url(&url), glyph::STAR_FILL, Action::Suggest(url)));
            }
            let hist = self.db.query(
                "SELECT title,url FROM history WHERE url LIKE ?1 ESCAPE '\\' OR title LIKE ?1 ESCAPE '\\' ORDER BY visits DESC, last_visit DESC LIMIT 7",
                &[V::T(&like)],
                |r| (r.text(0), r.text(1)),
            );
            for (title, url) in hist {
                if items.iter().any(|i| matches!(&i.action, Action::Suggest(u) if *u == url)) {
                    continue;
                }
                let label = if title.is_empty() { display_url(&url) } else { title };
                items.push(Item::suggestion(&label, &display_url(&url), glyph::HISTORY, Action::Suggest(url)));
                if items.len() >= 8 {
                    break;
                }
            }
        }
        let l = self.windows[wi].lay.addr;
        let pt = self.screen_pt(wi, l.x, l.bottom() + 4.0);
        self.show_popup(wi, crate::popup::PopupKind::Suggest, items, pt, Hit::None, l.w);
    }

    // ------------------------------------------------------------------ menus

    pub fn show_main_menu(&mut self, wi: usize) {
        let ti = self.windows[wi].active;
        let zoom = self.windows[wi].tabs.get(ti).map(|t| t.zoom).unwrap_or(1.0);
        let mode = self.s.mode;
        let dark = self.theme.dark;
        let mut items = Vec::new();
        if let crate::update::State::Available(r) | crate::update::State::Ready(r, _) = &self.upd {
            items.push(Item::new(&format!("Доступно обновление {}", r.tag), Action::Cmd(Cmd::About)).icon(glyph::DOWNLOAD));
            items.push(Item::sep());
        }
        items.extend(vec![
            Item::new("Новая вкладка", Action::Cmd(Cmd::NewTab)).right("Ctrl+T"),
            Item::new("Новое окно", Action::Cmd(Cmd::NewWindow)).right("Ctrl+N"),
            Item::new("Приватное окно", Action::Cmd(Cmd::NewPrivateWindow)).right("Ctrl+Shift+N"),
            Item::sep(),
            Item::new("История", Action::Cmd(Cmd::History)).right("Ctrl+H"),
            Item::new("Загрузки", Action::Cmd(Cmd::Downloads)).right("Ctrl+J"),
            Item::new("Закладки", Action::Cmd(Cmd::Bookmarks)).right("Ctrl+Shift+O"),
            Item::new(if self.s.bookmarks_bar { "Скрыть панель закладок" } else { "Показать панель закладок" }, Action::Cmd(Cmd::ToggleBookmarksBar)).right("Ctrl+Shift+B"),
            Item::sep(),
            Item::new(&format!("Масштаб: {:.0}%", zoom * 100.0), Action::None).header(),
            Item::new("Увеличить", Action::Cmd(Cmd::ZoomIn)).right("Ctrl +"),
            Item::new("Уменьшить", Action::Cmd(Cmd::ZoomOut)).right("Ctrl −"),
            Item::new("Сбросить масштаб", Action::Cmd(Cmd::ZoomReset)).right("Ctrl+0"),
            Item::new("Полноэкранный режим", Action::Cmd(Cmd::Fullscreen)).right("F11"),
            Item::sep(),
            Item::new(&format!("Режим: {}", mode.label()), Action::None).header(),
            Item::new("Экономия памяти", Action::Mode(crate::app::Mode::Saver)).checked(mode == crate::app::Mode::Saver),
            Item::new("Сбалансированный", Action::Mode(crate::app::Mode::Balanced)).checked(mode == crate::app::Mode::Balanced),
            Item::new("Максимальная производительность", Action::Mode(crate::app::Mode::Performance)).checked(mode == crate::app::Mode::Performance),
            Item::new("Освободить память", Action::Cmd(Cmd::FreeMemory)),
            Item::new("Монитор ресурсов", Action::Cmd(Cmd::Performance)).right("Shift+Esc"),
            Item::sep(),
            Item::new(if dark { "Светлая тема" } else { "Тёмная тема" }, Action::Theme(if dark { "light" } else { "dark" })),
            Item::new("Печать…", Action::Cmd(Cmd::Print)).right("Ctrl+P"),
            Item::new("Инструменты разработчика", Action::Cmd(Cmd::DevTools)).right("F12"),
            Item::new("Настройки", Action::Cmd(Cmd::Settings)),
            Item::new("Проверить обновления", Action::Cmd(Cmd::CheckUpdates)),
            Item::new("О Realism Browser", Action::Cmd(Cmd::About)),
            Item::sep(),
            Item::new("Выход", Action::Cmd(Cmd::Exit)),
        ]);
        let m = self.windows[wi].lay.menu;
        let pt = self.screen_pt(wi, m.right(), m.bottom() + 4.0);
        self.show_popup(wi, crate::popup::PopupKind::Menu, items, POINT { x: pt.x, y: pt.y }, Hit::Menu, -280.0);
    }

    pub fn show_tab_menu(&mut self, wi: usize, i: usize) {
        let t = &self.windows[wi].tabs[i];
        let id = t.id;
        let live = t.wv.is_some();
        let muted = t.muted;
        let items = vec![
            Item::new("Новая вкладка справа", Action::NewTabAt(i + 1)),
            Item::sep(),
            Item::new("Обновить", Action::TabReload(id)),
            Item::new("Дублировать", Action::TabDuplicate(id)),
            Item::new(if muted { "Включить звук сайта" } else { "Выключить звук сайта" }, Action::TabMute(id)).enabled(live),
            Item::new("Усыпить вкладку", Action::TabSleep(id)).enabled(live && i != self.windows[wi].active),
            Item::sep(),
            Item::new("Закрыть", Action::TabClose(id)).right("Ctrl+W"),
            Item::new("Закрыть другие вкладки", Action::TabCloseOthers(id)),
            Item::new("Закрыть вкладки справа", Action::TabCloseRight(id)),
            Item::new("Открыть закрытую вкладку", Action::Cmd(Cmd::ReopenClosed)).right("Ctrl+Shift+T").enabled(!self.closed.is_empty()),
        ];
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        self.show_popup(wi, crate::popup::PopupKind::Menu, items, pt, Hit::Tab(i), 250.0);
    }

    pub fn show_bm_folder(&mut self, wi: usize, folder: i64, r: R) {
        let rows = self.db.query("SELECT id,title,url,is_folder FROM bookmarks WHERE parent=? ORDER BY pos,id LIMIT 50", &[V::I(folder)], |r| {
            (r.int(0), r.text(1), r.text(2), r.int(3) != 0)
        });
        let mut items: Vec<Item> = rows
            .into_iter()
            .filter(|x| !x.3)
            .map(|(_, title, url, _)| {
                let label = if title.is_empty() { display_url(&url) } else { title };
                Item::new(&label, Action::Open(url))
            })
            .collect();
        if items.is_empty() {
            items.push(Item::new("Папка пуста", Action::None).header());
        }
        let pt = self.screen_pt(wi, r.x, r.bottom() + 2.0);
        self.show_popup(wi, crate::popup::PopupKind::Menu, items, pt, Hit::Bm(0), 260.0);
    }

    pub fn show_site_info(&mut self, wi: usize) {
        let ti = self.windows[wi].active;
        let t = &self.windows[wi].tabs[ti];
        if t.url.is_empty() {
            return;
        }
        let origin = origin_of(&t.url);
        let mut items = Vec::new();
        let (secure, label) = if is_internal(&t.url) {
            (true, "Встроенная страница Realism".to_string())
        } else if t.cert_error {
            (false, "Проблема с сертификатом сайта!".to_string())
        } else if t.url.starts_with("https://") {
            (true, "Соединение защищено (HTTPS)".to_string())
        } else if t.url.starts_with("http://") {
            (false, "Соединение не защищено (HTTP)".to_string())
        } else {
            (true, "Локальная страница".to_string())
        };
        items.push(Item::new(host_of(&t.url), Action::None).header());
        items.push(Item::new(&label, Action::None).icon(if secure { glyph::LOCK } else { glyph::WARN }));
        if !self.windows[wi].private {
            let perms = self.db.query("SELECT kind,state FROM permissions WHERE origin=?", &[V::T(&origin)], |r| (r.int(0) as i32, r.int(1) as i32));
            if !perms.is_empty() {
                items.push(Item::sep());
                items.push(Item::new("Разрешения сайта", Action::None).header());
                for (k, st) in perms {
                    let s = if st == 1 { "разрешено" } else { "заблокировано" };
                    items.push(Item::new(&format!("{} — {s}", perm_label(k)), Action::None));
                }
                items.push(Item::new("Сбросить разрешения", Action::ResetSitePerms(origin.clone())));
            }
        }
        if t.wv.is_some() {
            items.push(Item::sep());
            items.push(Item::new(if t.muted { "Включить звук" } else { "Выключить звук" }, Action::TabMute(t.id)));
        }
        let r = self.windows[wi].lay.addr_icon;
        let pt = self.screen_pt(wi, r.x, r.bottom() + 6.0);
        self.show_popup(wi, crate::popup::PopupKind::Menu, items, pt, Hit::AddrIcon, 280.0);
    }

    /// Applies the window's current size/minimized state (WM_SIZE, or deferred when the app was busy).
    pub fn sync_size(&mut self, wi: usize) {
        let w = &mut self.windows[wi];
        let was_min = w.minimized;
        w.minimized = unsafe { IsIconic(w.hwnd).as_bool() };
        let active = w.active;
        if w.minimized != was_min {
            // Hidden windows -> hidden webviews: WebView2 throttles timers and rendering.
            if let Some(t) = w.tabs.get_mut(active) {
                if let Some(wv) = &t.wv {
                    let _ = wv.wry.set_visible(!w.minimized);
                    if !w.minimized {
                        t.shown = true;
                    }
                }
                t.hidden_since = if w.minimized { Some(Instant::now()) } else { None };
                if !w.minimized {
                    t.suspended = false;
                }
            }
        }
        if !w.minimized {
            w.canvas = None;
            w.relayout();
            w.invalidate();
        }
    }

    // ====================================================================== painting

    pub fn paint(&mut self, wi: usize) {
        if self.windows[wi].bm_dirty {
            self.rebuild_bm(wi);
        }
        let hwnd = self.windows[wi].hwnd;
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
        let w = &self.windows[wi];
        let rc = w.client_px();
        let active_native = w.active_tab().map(|t| t.wv.is_none()).unwrap_or(true);
        let s = w.scale();
        let bind = if active_native { rc } else { RECT { left: 0, top: 0, right: rc.right, bottom: ((w.lay.content_y * s).ceil() as i32).min(rc.bottom) } };
        let needed = ((bind.right - bind.left) as i64) * ((bind.bottom - bind.top) as i64);
        let mut canvas = self.windows[wi].canvas.take();
        // Release a big off-screen buffer once we only need the toolbar again.
        if let Some(c) = &canvas {
            if c.alloc_area > needed * 2 && c.alloc_area > 2_000_000 {
                canvas = None;
            }
        }
        if canvas.is_none() {
            canvas = Canvas::new(&self.gfx).ok();
        }
        if let Some(mut c) = canvas {
            if bind.bottom > bind.top && bind.right > bind.left && c.begin(hdc, &bind, self.windows[wi].dpi) {
                self.draw_window(wi, &c);
                if !c.end() {
                    self.windows[wi].canvas = None;
                } else {
                    self.windows[wi].canvas = Some(c);
                }
            } else {
                self.windows[wi].canvas = Some(c);
            }
        }
        unsafe {
            let _ = EndPaint(hwnd, &ps);
        }
    }

    fn draw_window(&self, wi: usize, c: &Canvas) {
        let w = &self.windows[wi];
        let th = &w.theme;
        let l = &w.lay;
        let f = &self.gfx.fonts;
        if w.fullscreen.is_some() {
            c.clear(th.content_bg);
            return;
        }
        // ---- strip
        let strip = if w.private { th.strip_private } else { th.strip };
        c.fill(R::new(0.0, 0.0, l.w, STRIP_H), strip);
        if w.private {
            let b = l.badge;
            c.round(b, 13.0, th.accent.a(0.22));
            c.icon(glyph::SHIELD, &f.icon_sm, R::new(b.x + 8.0, b.y, 18.0, b.h), th.accent);
            c.text("Приватно", &f.small, R::new(b.x + 28.0, b.y, b.w - 30.0, b.h), th.text);
        }
        // caption buttons
        let max_icon = if unsafe { IsZoomed(w.hwnd).as_bool() } { glyph::RESTORE } else { glyph::MAX };
        for (r, g, h) in [(l.min, glyph::MIN, Hit::Min), (l.max, max_icon, Hit::Max), (l.close, glyph::CLOSE, Hit::Close)] {
            let hov = w.hover == h;
            let mut col = th.text_dim;
            if hov {
                if h == Hit::Close {
                    c.fill(r, th.close_red);
                    col = C(1.0, 1.0, 1.0, 1.0);
                } else {
                    c.fill(r, th.btn_hover);
                    col = th.text;
                }
            }
            c.icon(g, &f.icon_xs, r, col);
        }
        // tabs: inactive first, then active on top (dragging looks right)
        let order: Vec<usize> = (0..w.tabs.len()).filter(|&i| i != w.active).chain(std::iter::once(w.active)).collect();
        c.clip(R::new(0.0, 0.0, l.min.x - 4.0, STRIP_H));
        for i in order {
            if let (Some(t), Some(r)) = (w.tabs.get(i), l.tabs.get(i)) {
                let x = if t.x_anim.is_nan() { r.x } else { t.x_anim };
                self.draw_tab(c, w, t, R::new(x, r.y, r.w, r.h), i);
            }
        }
        c.unclip();
        // new tab
        if w.hover == Hit::NewTab {
            c.round(l.newtab, 8.0, th.btn_hover);
        }
        c.icon(glyph::ADD, &f.icon_sm, l.newtab, th.text_dim);

        // ---- toolbar
        c.fill(R::new(0.0, STRIP_H, l.w, l.content_y - STRIP_H), th.toolbar);
        let t = w.active_tab();
        let can_back = t.map(|t| self.can_go_back(t)).unwrap_or(false);
        let can_fwd = t.map(|t| t.can_fwd).unwrap_or(false);
        let loading = t.map(|t| t.loading).unwrap_or(false);
        self.tool_button(c, w, l.back, glyph::BACK, Hit::Back, can_back);
        self.tool_button(c, w, l.fwd, glyph::FORWARD, Hit::Fwd, can_fwd);
        self.tool_button(c, w, l.reload, if loading { glyph::STOP } else { glyph::REFRESH }, Hit::Reload, t.map(|t| !t.url.is_empty()).unwrap_or(false));
        self.tool_button(c, w, l.menu, glyph::MORE, Hit::Menu, true);
        if self.update_available() {
            // update badge
            c.circle(l.menu.right() - 8.0, l.menu.y + 8.0, 4.0, th.accent2);
        }
        if l.dl.w > 0.0 {
            self.tool_button(c, w, l.dl, glyph::DOWNLOAD, Hit::Dl, true);
            if let Some(p) = self.downloads_progress() {
                c.arc(&self.gfx, l.dl.cx(), l.dl.cy(), 13.0, -std::f32::consts::FRAC_PI_2, p * std::f32::consts::TAU, th.accent, 2.0);
            }
        }
        // RAM chip
        if l.ram.w > 0.0 {
            let r = l.ram;
            let hov = w.hover == Hit::Ram;
            c.round(r, 15.0, if hov { th.btn_hover } else { th.addr });
            // tiny memory-chip glyph
            let (cx, cy) = (r.x + 16.0, r.cy());
            c.round(R::new(cx - 5.5, cy - 5.5, 11.0, 11.0), 2.5, th.accent2);
            c.fill(R::new(cx - 2.5, cy - 2.5, 5.0, 5.0), th.addr);
            for k in 0..3 {
                let o = -3.5 + k as f32 * 3.5;
                c.fill(R::new(cx + o - 0.6, cy - 8.0, 1.2, 2.0), th.accent2);
                c.fill(R::new(cx + o - 0.6, cy + 6.0, 1.2, 2.0), th.accent2);
            }
            let txt = if self.ram_total > 0 { fmt_mb(self.ram_total) } else { "RAM".into() };
            c.text(&txt, &f.small, R::new(r.x + 28.0, r.y, r.w - 32.0, r.h), th.text_dim);
        }
        // address pill
        let a = l.addr;
        c.round(a, 17.0, th.addr);
        if w.focus_address {
            c.round_stroke(a, 17.0, th.accent.a(0.9), 1.5);
        }
        if let Some(t) = t {
            let (g, col) = if t.url.is_empty() {
                (glyph::SEARCH, th.text_faint)
            } else if is_internal(&t.url) {
                (glyph::SHIELD, th.accent)
            } else if t.cert_error {
                (glyph::WARN, th.close_red)
            } else if t.url.starts_with("https://") {
                (glyph::LOCK, th.text_dim)
            } else if t.url.starts_with("http://") {
                (glyph::WARN, th.warn)
            } else {
                (glyph::INFO, th.text_dim)
            };
            if w.hover == Hit::AddrIcon && !t.url.is_empty() {
                c.round(l.addr_icon, 14.0, th.btn_hover);
            }
            c.icon(g, &f.icon_sm, l.addr_icon, col);
            if !t.url.is_empty() && !w.private || (!t.url.is_empty()) {
                let starred = self.is_bookmarked(&t.url);
                if w.hover == Hit::Star {
                    c.round(l.star, 14.0, th.btn_hover);
                }
                c.icon(if starred { glyph::STAR_FILL } else { glyph::STAR }, &f.icon_sm, l.star, if starred { th.accent } else { th.text_dim });
            }
            if l.zoom.w > 0.0 {
                c.round(l.zoom, 11.0, if w.hover == Hit::Zoom { th.btn_press } else { th.btn_hover });
                c.text(&format!("{:.0}%", t.zoom * 100.0), &f.small_center, l.zoom, th.text_dim);
            }
        }
        // progress line
        if loading {
            let el = t.map(|t| t.load_start.elapsed().as_secs_f32()).unwrap_or(0.0);
            let y = l.content_y - 2.0;
            let seg = l.w * 0.28;
            let pos = ((el * 0.9) % 1.0) * (l.w + seg) - seg;
            c.clip(R::new(0.0, y, l.w, 2.0));
            c.linear_rect(R::new(pos, y, seg, 2.0), 1.0, th.accent, th.accent2);
            c.unclip();
        } else {
            c.fill(R::new(0.0, l.content_y - 1.0, l.w, 1.0), th.sep);
        }
        // bookmarks bar
        if w.show_bm {
            for (i, r) in l.bm_items.iter().enumerate() {
                let Some(it) = w.bm_items.get(i) else { continue };
                if w.hover == Hit::Bm(i) {
                    c.round(*r, 7.0, th.btn_hover);
                }
                if it.folder {
                    c.icon(glyph::FOLDER, &f.icon_sm, R::new(r.x + 6.0, r.y, 18.0, r.h), th.warn);
                } else {
                    let col = avatar_color(hash_str(host_of(&it.url)));
                    c.circle(r.x + 15.0, r.cy(), 7.0, col.a(0.9));
                    let letter: String = host_of(&it.url).trim_start_matches("www.").chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
                    c.text(&letter, &f.small_center, R::new(r.x + 8.0, r.y, 14.0, r.h), th.strip);
                }
                c.text(&it.title, &f.small, R::new(r.x + 28.0, r.y, r.w - 32.0, r.h), th.text);
            }
            if w.bm_items.is_empty() {
                c.text("Добавляйте сайты в закладки звёздочкой в адресной строке (Ctrl+D)", &f.small, R::new(12.0, l.bm.y, l.w - 24.0, BM_H), th.text_faint);
            }
        }
        // info bar
        if l.info.h > 0.0 {
            let r = l.info;
            c.fill(r, th.info_bg);
            if let Some((msg, _)) = w.info_buttons() {
                let is_perm = w.active_tab().map(|t| !t.perms.is_empty()).unwrap_or(false);
                c.icon(if is_perm { glyph::CAMERA } else { glyph::INFO }, &f.icon_sm, R::new(12.0, r.y, 22.0, r.h), th.accent);
                let right = l.info_btns.first().map(|b| b.0.x).unwrap_or(r.w) - 12.0;
                c.text(&msg, &f.ui, R::new(40.0, r.y, right - 40.0, r.h), th.text);
            }
            for (i, (br, b, label)) in l.info_btns.iter().enumerate() {
                let primary = matches!(b, InfoBtn::PermAllow | InfoBtn::PopupOpen | InfoBtn::Reload);
                let hov = w.hover == Hit::Info(i);
                if primary {
                    c.round(*br, 8.0, if hov { th.accent.mix(th.text, 0.15) } else { th.accent });
                    c.text(label, &f.ui_center, *br, C(1.0, 1.0, 1.0, 1.0));
                } else {
                    c.round(*br, 8.0, if hov { th.btn_press } else { th.btn_hover });
                    c.text(label, &f.ui_center, *br, th.text);
                }
            }
        }
        // ---- content (native)
        if let Some(t) = t {
            if t.wv.is_none() {
                let cr = R::new(0.0, l.content_y, l.w, l.h - l.content_y);
                c.fill(cr, th.content_bg);
                if t.is_ntp() {
                    self.draw_ntp(c, w, cr);
                } else {
                    let a = t.load_start.elapsed().as_secs_f32() * 5.0;
                    c.arc(&self.gfx, cr.cx(), cr.cy(), 16.0, a, 4.2, th.accent, 3.0);
                }
            }
        }
    }

    fn tool_button(&self, c: &Canvas, w: &Win, r: R, g: char, h: Hit, enabled: bool) {
        let th = &w.theme;
        if enabled && w.hover == h {
            c.round(r, 9.0, if w.pressed == h { th.btn_press } else { th.btn_hover });
        }
        c.icon(g, &self.gfx.fonts.icon, r, if enabled { th.text } else { th.text_faint });
    }

    fn draw_tab(&self, c: &Canvas, w: &Win, t: &Tab, r: R, i: usize) {
        let th = &w.theme;
        let f = &self.gfx.fonts;
        let active = i == w.active;
        let hovered = matches!(w.hover, Hit::Tab(j) | Hit::TabClose(j) | Hit::TabAudio(j) if j == i);
        if active {
            c.round(r, 9.0, th.tab_active);
            let gl = R::new(r.x + 14.0, r.bottom() - 2.0, r.w - 28.0, 2.0);
            c.linear_rect(gl, 1.0, th.accent, th.accent2);
        } else if hovered {
            c.round(r, 9.0, th.tab_hover);
        }
        let sleeping = t.sleeping();
        let compact = r.w < 60.0;
        let ix = if compact { r.cx() - 8.0 } else { r.x + 11.0 };
        let icon_r = R::new(ix, r.cy() - 8.0, 16.0, 16.0);
        if t.loading {
            let a = t.load_start.elapsed().as_secs_f32() * 6.0;
            c.arc(&self.gfx, icon_r.cx(), icon_r.cy(), 6.5, a, 4.0, th.accent, 2.0);
        } else if let Some(px) = &t.icon {
            c.bitmap(px, crate::gfx::ICON_PX, crate::gfx::ICON_PX, icon_r, if sleeping { 0.45 } else { 1.0 });
        } else if is_internal(&t.url) {
            c.circle(icon_r.cx(), icon_r.cy(), 7.5, th.accent);
            c.text("R", &f.small_center, icon_r, C(1.0, 1.0, 1.0, 1.0));
        } else if t.url.is_empty() {
            c.icon(glyph::ADD, &f.icon_xs, icon_r, th.text_dim);
        } else {
            c.icon(glyph::GLOBE, &f.icon_sm, icon_r, if sleeping { th.text_faint } else { th.text_dim });
        }
        if sleeping && !t.loading {
            // moon badge = sleeping tab
            let bx = icon_r.right() - 1.0;
            let by = icon_r.bottom() - 1.0;
            c.circle(bx, by, 6.0, if active { th.tab_active } else { if w.private { th.strip_private } else { th.strip } });
            c.icon(glyph::MOON, &f.icon_xs, R::new(bx - 6.0, by - 6.0, 12.0, 12.0), th.accent2);
        }
        if compact {
            return;
        }
        let (close, audio) = tab_parts(&r, Some(t), active, hovered);
        let text_right = if audio.w > 0.0 { audio.x } else if close.w > 0.0 { close.x } else { r.right() - 8.0 } - 4.0;
        let col = if sleeping { th.text_faint } else if active { th.text } else { th.text_dim };
        c.text(&t.display_title(), &f.tab, R::new(r.x + 34.0, r.y, (text_right - r.x - 34.0).max(0.0), r.h), col);
        if audio.w > 0.0 {
            if w.hover == Hit::TabAudio(i) {
                c.round(audio, 6.0, th.btn_hover);
            }
            c.icon(if t.muted { glyph::MUTE } else { glyph::VOLUME }, &f.icon_xs, audio, if t.muted { th.text_faint } else { th.accent2 });
        }
        if close.w > 0.0 {
            if w.hover == Hit::TabClose(i) {
                c.round(close, 6.0, th.btn_press);
            }
            c.icon(glyph::CLOSE, &f.icon_xs, close, th.text_dim);
        }
    }

    fn draw_ntp(&self, c: &Canvas, w: &Win, cr: R) {
        let th = &w.theme;
        let f = &self.gfx.fonts;
        let l = &w.lay;
        let top = l.ntp_search.y - 104.0;
        c.glow(cr.cx(), top + 40.0, (cr.w * 0.42).max(320.0), 220.0, th.accent.a(if th.dark { 0.16 } else { 0.10 }));
        c.glow(cr.cx() + cr.w * 0.18, top + 120.0, 260.0, 160.0, th.accent2.a(if th.dark { 0.07 } else { 0.06 }));
        let title = if w.private { "Realism · приватно" } else { "Realism" };
        c.linear_text(title, &f.hero, R::new(0.0, top, cr.w, 72.0), th.accent, th.accent2);
        let sub = if w.private {
            "История, cookies и данные форм удаляются после закрытия окна"
        } else {
            "Лёгкий браузер, который бережёт память"
        };
        c.text(sub, &f.hero_sub, R::new(0.0, top + 64.0, cr.w, 26.0), th.text_dim);
        let s = l.ntp_search;
        c.round(s, 24.0, if w.hover == Hit::NtpSearch { th.tile_hover } else { th.tile });
        c.round_stroke(s, 24.0, th.sep, 1.0);
        c.icon(glyph::SEARCH, &f.icon, R::new(s.x + 14.0, s.y, 24.0, s.h), th.text_dim);
        c.text(&format!("Поиск в {} или адрес сайта", engine_name(&self.s.search)), &f.ui, R::new(s.x + 46.0, s.y, s.w - 60.0, s.h), th.text_faint);
        for (i, r) in l.tiles.iter().enumerate() {
            let Some(t) = w.tiles.get(i) else { continue };
            let hov = w.hover == Hit::Tile(i);
            c.round(*r, 14.0, if hov { th.tile_hover } else { th.tile });
            let col = avatar_color(hash_str(&t.host));
            c.circle(r.cx(), r.y + 36.0, 20.0, col.a(0.18));
            let letter: String = t.host.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
            c.text(&letter, &f.letter, R::new(r.x, r.y + 16.0, r.w, 40.0), col);
            let label = if t.title.is_empty() { t.host.clone() } else { t.title.clone() };
            c.text(&label, &f.tile, R::new(r.x + 8.0, r.y + 64.0, r.w - 16.0, 30.0), th.text);
        }
        let foot = format!(
            "Режим: {}   ·   Память браузера: {}",
            self.s.mode.label(),
            if self.ram_total > 0 { fmt_mb(self.ram_total) } else { "—".into() }
        );
        c.text(&foot, &f.small_center, R::new(0.0, cr.bottom() - 34.0, cr.w, 24.0), th.text_faint);
    }
}

pub fn engine_name(e: &str) -> &'static str {
    match e {
        "bing" => "Bing",
        "duckduckgo" => "DuckDuckGo",
        _ => "Google",
    }
}

fn make_font(dpi: u32) -> HFONT {
    let mut lf = LOGFONTW { lfHeight: -((14.0 * dpi as f32 / 96.0).round() as i32), lfWeight: 400, lfQuality: CLEARTYPE_QUALITY, lfCharSet: DEFAULT_CHARSET, ..Default::default() };
    let name: Vec<u16> = "Segoe UI Variable Text".encode_utf16().collect();
    for (i, ch) in name.iter().enumerate().take(31) {
        lf.lfFaceName[i] = *ch;
    }
    unsafe { CreateFontIndirectW(&lf) }
}
