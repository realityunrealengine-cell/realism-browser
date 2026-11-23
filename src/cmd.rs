//! Browser commands and keyboard shortcuts.

use windows::Win32::UI::Input::KeyboardAndMouse::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    NewTab,
    CloseTab,
    ReopenClosed,
    FocusAddress,
    Reload,
    HardReload,
    Stop,
    Back,
    Forward,
    NextTab,
    PrevTab,
    SelectTab(usize),
    LastTab,
    NewWindow,
    NewPrivateWindow,
    History,
    Downloads,
    Bookmarks,
    BookmarkPage,
    ToggleBookmarksBar,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Fullscreen,
    Escape,
    Performance,
    Settings,
    About,
    CheckUpdates,
    DevTools,
    Print,
    CloseWindow,
    FreeMemory,
    Exit,
}

fn down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// Map a key press to a browser command using the live modifier state.
/// `in_web`: key comes from the web content (then a few keys are left to the page).
pub fn map_key(vk: u32, in_web: bool, fullscreen: bool) -> Option<Cmd> {
    let ctrl = down(VK_CONTROL);
    let shift = down(VK_SHIFT);
    let alt = down(VK_MENU);
    let vk = VIRTUAL_KEY(vk as u16);
    let key = |c: u8| vk.0 == c as u16;
    if ctrl && !alt {
        if shift {
            if key(b'T') { return Some(Cmd::ReopenClosed); }
            if key(b'N') { return Some(Cmd::NewPrivateWindow); }
            if key(b'B') { return Some(Cmd::ToggleBookmarksBar); }
            if key(b'O') { return Some(Cmd::Bookmarks); }
            if key(b'R') { return Some(Cmd::HardReload); }
            if key(b'W') { return Some(Cmd::CloseWindow); }
            if vk == VK_TAB { return Some(Cmd::PrevTab); }
            if vk == VK_DELETE { return Some(Cmd::Settings); }
            if vk == VK_ESCAPE { return Some(Cmd::Performance); }
            return None;
        }
        if key(b'T') { return Some(Cmd::NewTab); }
        if key(b'W') || vk == VK_F4 { return Some(Cmd::CloseTab); }
        if key(b'L') { return Some(Cmd::FocusAddress); }
        if key(b'R') { return Some(Cmd::Reload); }
        if key(b'N') { return Some(Cmd::NewWindow); }
        if key(b'H') { return Some(Cmd::History); }
        if key(b'J') { return Some(Cmd::Downloads); }
        if key(b'D') { return Some(Cmd::BookmarkPage); }
        if key(b'K') || key(b'E') { return Some(Cmd::FocusAddress); }
        if vk == VK_TAB || vk == VK_NEXT { return Some(Cmd::NextTab); }
        if vk == VK_PRIOR { return Some(Cmd::PrevTab); }
        if vk == VK_F5 { return Some(Cmd::HardReload); }
        if vk == VK_OEM_PLUS || vk == VK_ADD { return Some(Cmd::ZoomIn); }
        if vk == VK_OEM_MINUS || vk == VK_SUBTRACT { return Some(Cmd::ZoomOut); }
        if key(b'0') || vk == VK_NUMPAD0 { return Some(Cmd::ZoomReset); }
        if (b'1'..=b'8').any(|c| key(c)) { return Some(Cmd::SelectTab((vk.0 - b'1' as u16) as usize)); }
        if key(b'9') { return Some(Cmd::LastTab); }
        return None;
    }
    if alt && !ctrl {
        if vk == VK_LEFT { return Some(Cmd::Back); }
        if vk == VK_RIGHT { return Some(Cmd::Forward); }
        if key(b'D') { return Some(Cmd::FocusAddress); }
        if vk == VK_HOME { return Some(Cmd::NewTab); }
        return None;
    }
    if !ctrl && !alt {
        if vk == VK_F5 { return Some(if shift { Cmd::HardReload } else { Cmd::Reload }); }
        if vk == VK_F11 { return Some(Cmd::Fullscreen); }
        if vk == VK_F6 { return Some(Cmd::FocusAddress); }
        if vk == VK_BROWSER_BACK { return Some(Cmd::Back); }
        if vk == VK_BROWSER_FORWARD { return Some(Cmd::Forward); }
        if vk == VK_BROWSER_REFRESH { return Some(Cmd::Reload); }
        if shift && vk == VK_ESCAPE { return Some(Cmd::Performance); }
        // Escape is left to the page (it closes dialogs / element fullscreen) unless we are in browser fullscreen.
        if vk == VK_ESCAPE && (!in_web || fullscreen) { return Some(Cmd::Escape); }
    }
    None
}
