//! Custom-drawn popup menus and address-bar suggestions (non-activating popup window).

use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::app::{with_app, App, Mode};
use crate::cmd::Cmd;
use crate::db::V;
use crate::gfx::{glyph, Canvas, R};
use crate::ui::Hit;

const ITEM_H: f32 = 32.0;
const SUGG_H: f32 = 40.0;
const SEP_H: f32 = 9.0;
const PAD: f32 = 6.0;
const WM_MOUSELEAVE: u32 = 0x02A3;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    None,
    Cmd(Cmd),
    NewTabAt(usize),
    TabReload(u64),
    TabDuplicate(u64),
    TabMute(u64),
    TabSleep(u64),
    TabClose(u64),
    TabCloseOthers(u64),
    TabCloseRight(u64),
    Open(String),
    OpenNewTab(String),
    ResetSitePerms(String),
    Theme(&'static str),
    Mode(Mode),
    BmDelete(i64),
    Suggest(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Normal,
    Sep,
    Header,
    Suggestion,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    pub right: String,
    pub icon: Option<char>,
    pub action: Action,
    pub enabled: bool,
    pub checked: bool,
    pub kind: Kind,
}

impl Item {
    pub fn new(label: &str, action: Action) -> Item {
        Item { label: label.to_string(), right: String::new(), icon: None, action, enabled: true, checked: false, kind: Kind::Normal }
    }
    pub fn sep() -> Item {
        Item { kind: Kind::Sep, ..Item::new("", Action::None) }
    }
    pub fn suggestion(label: &str, sub: &str, icon: char, action: Action) -> Item {
        Item { right: sub.to_string(), icon: Some(icon), kind: Kind::Suggestion, ..Item::new(label, action) }
    }
    pub fn right(mut self, s: &str) -> Item {
        self.right = s.to_string();
        self
    }
    pub fn header(mut self) -> Item {
        self.kind = Kind::Header;
        self
    }
    pub fn checked(mut self, c: bool) -> Item {
        self.checked = c;
        self
    }
    pub fn enabled(mut self, e: bool) -> Item {
        self.enabled = e;
        self
    }
    pub fn icon(mut self, g: char) -> Item {
        self.icon = Some(g);
        self
    }
    fn height(&self) -> f32 {
        match self.kind {
            Kind::Sep => SEP_H,
            Kind::Suggestion => SUGG_H,
            _ => ITEM_H,
        }
    }
    fn clickable(&self) -> bool {
        self.enabled && matches!(self.kind, Kind::Normal | Kind::Suggestion) && self.action != Action::None
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PopupKind {
    Menu,
    Suggest,
}

pub struct Popup {
    pub hwnd: HWND,
    pub owner: u64,
    pub kind: PopupKind,
    pub items: Vec<Item>,
    pub hover: Option<usize>,
    pub anchor: Hit,
    pub w: f32,
    pub h: f32,
    pub dpi: u32,
    pub canvas: Option<Canvas>,
    pub tracking: bool,
}

impl Popup {
    fn rows(&self) -> Vec<R> {
        let mut y = PAD;
        self.items
            .iter()
            .map(|it| {
                let r = R::new(PAD, y, self.w - PAD * 2.0, it.height());
                y += it.height();
                r
            })
            .collect()
    }
    pub fn selected_action(&self) -> Option<Action> {
        self.hover.and_then(|i| self.items.get(i)).map(|it| it.action.clone())
    }
    /// Keyboard selection for suggestions. Returns the URL of the newly selected row.
    pub fn move_sel(&mut self, d: i32) -> Option<String> {
        let n = self.items.len() as i32;
        if n == 0 {
            return None;
        }
        let cur = self.hover.map(|h| h as i32).unwrap_or(-1);
        let next = (cur + d).rem_euclid(n) as usize;
        self.hover = Some(next);
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        match &self.items[next].action {
            Action::Suggest(u) => Some(u.clone()),
            _ => None,
        }
    }
}

pub fn register_class() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(popup_proc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("RealismPopup"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_MOUSEACTIVATE {
            return LRESULT(MA_NOACTIVATE as isize);
        }
        let r = with_app(|app| app.popup_msg(hwnd, msg, wp, lp)).flatten();
        crate::after_dispatch();
        r.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
    }
}

impl App {
    /// `width < 0` means: right-align the popup to `pt.x` with |width|.
    pub fn show_popup(&mut self, wi: usize, kind: PopupKind, items: Vec<Item>, pt: POINT, anchor: Hit, width: f32) {
        self.close_popup();
        if items.is_empty() {
            return;
        }
        let owner = self.windows[wi].hwnd;
        let dpi = unsafe { GetDpiForWindow(owner) };
        let s = dpi as f32 / 96.0;
        let mut w = width.abs();
        if kind == PopupKind::Menu {
            for it in &items {
                let lw = self.gfx.measure(&it.label, &self.gfx.fonts.menu) + 40.0;
                let rw = if it.right.is_empty() { 0.0 } else { self.gfx.measure(&it.right, &self.gfx.fonts.menu_right) + 24.0 };
                w = w.max(lw + rw + PAD * 2.0 + 8.0);
            }
            w = w.min(420.0);
        }
        let h: f32 = items.iter().map(|i| i.height()).sum::<f32>() + PAD * 2.0;
        let (pw, ph) = ((w * s) as i32, (h * s) as i32);
        let mut x = if width < 0.0 { pt.x - pw } else { pt.x };
        let mut y = pt.y;
        unsafe {
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(mon, &mut mi);
            let wa = mi.rcWork;
            x = x.clamp(wa.left, (wa.right - pw).max(wa.left));
            if y + ph > wa.bottom {
                y = (wa.bottom - ph).max(wa.top);
            }
        }
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            match CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                w!("RealismPopup"),
                w!(""),
                WS_POPUP,
                x,
                y,
                pw,
                ph,
                Some(owner),
                None,
                Some(hinst.into()),
                None,
            ) {
                Ok(h) => h,
                Err(_) => return,
            }
        };
        unsafe {
            let corner = DWMWCP_ROUNDSMALL;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, 4);
            let dark: u32 = self.theme.dark as u32;
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        }
        let hover = if kind == PopupKind::Suggest { None } else { None };
        self.popup = Some(Popup {
            hwnd,
            owner: self.windows[wi].id,
            kind,
            items,
            hover,
            anchor,
            w,
            h,
            dpi,
            canvas: None,
            tracking: false,
        });
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn close_popup(&mut self) {
        if let Some(p) = self.popup.take() {
            unsafe {
                let _ = DestroyWindow(p.hwnd);
            }
        }
    }

    fn popup_msg(&mut self, hwnd: HWND, msg: u32, _wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let p = self.popup.as_mut()?;
        if p.hwnd != hwnd {
            return None;
        }
        let s = p.dpi as f32 / 96.0;
        let (mx, my) = (((lp.0 & 0xFFFF) as i16) as f32 / s, (((lp.0 >> 16) & 0xFFFF) as i16) as f32 / s);
        match msg {
            WM_PAINT => {
                self.paint_popup();
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_MOUSEMOVE => {
                if !p.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    p.tracking = true;
                }
                let rows = p.rows();
                let h = rows.iter().position(|r| r.contains(mx, my)).filter(|&i| p.items[i].clickable());
                if h != p.hover {
                    p.hover = h;
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                p.tracking = false;
                if p.kind == PopupKind::Menu {
                    p.hover = None;
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let rows = p.rows();
                let Some(i) = rows.iter().position(|r| r.contains(mx, my)) else { return Some(LRESULT(0)) };
                if !p.items[i].clickable() {
                    return Some(LRESULT(0));
                }
                let action = p.items[i].action.clone();
                let owner = p.owner;
                self.close_popup();
                if let Some(wi) = self.windows.iter().position(|w| w.id == owner) {
                    self.popup_action(wi, action);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_RBUTTONUP => Some(LRESULT(0)),
            _ => None,
        }
    }

    fn paint_popup(&mut self) {
        let Some(mut p) = self.popup.take() else { return };
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(p.hwnd, &mut ps) };
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(p.hwnd, &mut rc);
        }
        if p.canvas.is_none() {
            p.canvas = Canvas::new(&self.gfx).ok();
        }
        let th = self.theme;
        let f = &self.gfx.fonts;
        let rows = p.rows();
        if let Some(c) = p.canvas.as_mut() {
            if c.begin(hdc, &rc, p.dpi) {
                c.clear(th.popup);
                c.round_stroke(R::new(0.0, 0.0, p.w, p.h), 0.0, th.popup_border, 1.0);
                for (i, (it, r)) in p.items.iter().zip(rows.iter()).enumerate() {
                    match it.kind {
                        Kind::Sep => c.fill(R::new(r.x + 8.0, r.cy(), r.w - 16.0, 1.0), th.sep),
                        Kind::Header => {
                            if let Some(g) = it.icon {
                                c.icon(g, &f.icon_sm, R::new(r.x + 6.0, r.y, 20.0, r.h), th.accent);
                            }
                            c.text(&it.label, &f.ui_bold, R::new(r.x + 12.0, r.y, r.w - 24.0, r.h), th.text_dim);
                        }
                        Kind::Normal => {
                            if p.hover == Some(i) {
                                c.round(*r, 6.0, th.popup_hover);
                            }
                            let col = if it.enabled { th.text } else { th.text_faint };
                            if it.checked {
                                c.circle(r.x + 16.0, r.cy(), 3.5, th.accent);
                            } else if let Some(g) = it.icon {
                                c.icon(g, &f.icon_sm, R::new(r.x + 6.0, r.y, 20.0, r.h), th.text_dim);
                            }
                            let rw = if it.right.is_empty() { 0.0 } else { self.gfx.measure(&it.right, &f.menu_right) + 12.0 };
                            c.text(&it.label, &f.menu, R::new(r.x + 30.0, r.y, r.w - 40.0 - rw, r.h), col);
                            if !it.right.is_empty() {
                                c.text(&it.right, &f.menu_right, R::new(r.right() - rw - 10.0, r.y, rw, r.h), th.text_faint);
                            }
                        }
                        Kind::Suggestion => {
                            if p.hover == Some(i) {
                                c.round(*r, 8.0, th.popup_hover);
                            }
                            if let Some(g) = it.icon {
                                let col = if g == glyph::STAR_FILL { th.accent } else { th.text_dim };
                                c.icon(g, &f.icon_sm, R::new(r.x + 8.0, r.y, 22.0, r.h), col);
                            }
                            let label_w = (self.gfx.measure(&it.label, &f.menu) + 4.0).min(r.w * 0.6);
                            c.text(&it.label, &f.menu, R::new(r.x + 38.0, r.y, label_w, r.h), th.text);
                            if !it.right.is_empty() {
                                let x = r.x + 38.0 + label_w + 10.0;
                                c.text(&format!("— {}", it.right), &f.small, R::new(x, r.y, (r.right() - x - 10.0).max(0.0), r.h), th.accent.mix(th.text_dim, 0.5));
                            }
                        }
                    }
                }
                if !c.end() {
                    p.canvas = None;
                }
            }
        }
        unsafe {
            let _ = EndPaint(p.hwnd, &ps);
        }
        self.popup = Some(p);
    }

    pub fn popup_action(&mut self, wi: usize, a: Action) {
        match a {
            Action::None => {}
            Action::Cmd(c) => self.run_cmd(wi, c),
            Action::NewTabAt(i) => {
                self.add_tab(wi, None, Some(i), true);
            }
            Action::TabReload(id) => {
                if let Some(t) = self.tab_mut(id) {
                    if let Some(wv) = &t.wv {
                        unsafe {
                            let _ = wv.core.Reload();
                        }
                    }
                }
            }
            Action::TabDuplicate(id) => {
                if let Some((wi, ti)) = self.loc(id) {
                    let url = self.windows[wi].tabs[ti].url.clone();
                    if !url.is_empty() {
                        self.add_tab(wi, Some(url), Some(ti + 1), true);
                    }
                }
            }
            Action::TabMute(id) => {
                if let Some(t) = self.tab_mut(id) {
                    if let Some(wv) = &t.wv {
                        if let Ok(c8) = windows::core::Interface::cast::<webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_8>(&wv.core) {
                            let m = t.muted;
                            unsafe {
                                let _ = c8.SetIsMuted(!m);
                            }
                        }
                    }
                }
            }
            Action::TabSleep(id) => {
                if let Some((wi, ti)) = self.loc(id) {
                    if self.windows[wi].active != ti {
                        if self.can_discard(wi, ti).is_ok() {
                            self.discard(wi, ti);
                        } else if let Some(wv) = &self.windows[wi].tabs[ti].wv {
                            crate::webview::set_low_memory(&wv.core, true);
                            crate::webview::try_suspend(id, wv, true);
                        }
                    }
                }
            }
            Action::TabClose(id) => self.close_tab_id(id),
            Action::TabCloseOthers(id) => {
                let ids: Vec<u64> = self.windows[wi].tabs.iter().map(|t| t.id).filter(|&t| t != id).collect();
                for t in ids {
                    self.close_tab_id(t);
                }
            }
            Action::TabCloseRight(id) => {
                if let Some((wi, ti)) = self.loc(id) {
                    let ids: Vec<u64> = self.windows[wi].tabs[ti + 1..].iter().map(|t| t.id).collect();
                    for t in ids {
                        self.close_tab_id(t);
                    }
                }
            }
            Action::Open(u) | Action::Suggest(u) => {
                let ti = self.windows[wi].active;
                self.navigate_tab(wi, ti, Some(u));
            }
            Action::OpenNewTab(u) => {
                self.add_tab(wi, Some(u), None, false);
            }
            Action::ResetSitePerms(origin) => {
                self.db.run("DELETE FROM permissions WHERE origin=?", &[V::T(&origin)]);
            }
            Action::Theme(t) => {
                self.db.set_setting("theme", t);
                self.set_theme(t);
            }
            Action::Mode(m) => self.set_mode(m),
            Action::BmDelete(id) => {
                self.db.run("DELETE FROM bookmarks WHERE id=? OR parent=?", &[V::I(id), V::I(id)]);
                self.refresh_bookmarks_bar();
            }
        }
    }
}
