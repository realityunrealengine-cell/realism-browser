//! Realism Browser setup: per-user install (no admin rights), Start menu / desktop shortcuts,
//! "Apps & features" entry, registration as a browser choice in Windows "Default apps", uninstall.
//!
//!   realism-setup.exe                  interactive install
//!   realism-setup.exe --silent [--desktop] [--dir PATH]
//!   uninstall.exe --uninstall [--silent] [--purge]   (--purge also removes the browser profile)

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

use windows::core::{w, Interface, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

static PAYLOAD: &[u8] = include_bytes!(env!("REALISM_EXE"));
const VERSION: &str = env!("REALISM_VERSION");
const APP: &str = "Realism Browser";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\RealismBrowser";
const CLIENT_KEY: &str = r"Software\Clients\StartMenuInternet\RealismBrowser";
const PROGID: &str = "RealismHTML";
const WEBVIEW2_GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn known(id: &windows::core::GUID) -> PathBuf {
    unsafe {
        match SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                PathBuf::from(s)
            }
            Err(_) => PathBuf::from("."),
        }
    }
}

fn default_dir() -> PathBuf {
    known(&FOLDERID_LocalAppData).join("Programs").join(APP)
}

// ---------------------------------------------------------------- registry helpers

fn reg_set(root: HKEY, key: &str, name: Option<&str>, value: &str) -> bool {
    unsafe {
        let mut h = HKEY::default();
        if RegCreateKeyExW(root, &HSTRING::from(key), None, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut h, None).is_err() {
            return false;
        }
        let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
        let n = name.map(HSTRING::from);
        let r = RegSetValueExW(h, n.as_ref().map(|s| PCWSTR(s.as_ptr())).unwrap_or(PCWSTR::null()), None, REG_SZ, Some(bytes));
        let _ = RegCloseKey(h);
        r.is_ok()
    }
}

fn reg_set_dword(root: HKEY, key: &str, name: &str, value: u32) {
    unsafe {
        let mut h = HKEY::default();
        if RegCreateKeyExW(root, &HSTRING::from(key), None, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut h, None).is_ok() {
            let _ = RegSetValueExW(h, &HSTRING::from(name), None, REG_DWORD, Some(&value.to_le_bytes()));
            let _ = RegCloseKey(h);
        }
    }
}

fn reg_get(root: HKEY, key: &str, name: &str) -> Option<String> {
    unsafe {
        let mut buf = [0u16; 512];
        let mut size = (buf.len() * 2) as u32;
        RegGetValueW(root, &HSTRING::from(key), &HSTRING::from(name), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size))
            .ok()
            .ok()?;
        let end = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

fn webview2_installed() -> bool {
    let paths = [
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{WEBVIEW2_GUID}")),
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{WEBVIEW2_GUID}")),
        (HKEY_CURRENT_USER, format!(r"Software\Microsoft\EdgeUpdate\Clients\{WEBVIEW2_GUID}")),
    ];
    paths.iter().any(|(r, k)| reg_get(*r, k, "pv").map(|v| !v.is_empty() && v != "0.0.0.0").unwrap_or(false))
}

// ---------------------------------------------------------------- shortcuts

fn make_shortcut(lnk: &Path, target: &Path) -> bool {
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return false };
        let t = HSTRING::from(target.to_string_lossy().as_ref());
        let _ = link.SetPath(&t);
        let _ = link.SetWorkingDirectory(&HSTRING::from(target.parent().unwrap_or(Path::new(".")).to_string_lossy().as_ref()));
        let _ = link.SetDescription(w!("Realism Browser — лёгкий браузер"));
        let _ = link.SetIconLocation(&t, 0);
        let Ok(pf) = link.cast::<IPersistFile>() else { return false };
        if let Some(dir) = lnk.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        pf.Save(&HSTRING::from(lnk.to_string_lossy().as_ref()), true).is_ok()
    }
}

fn start_menu_lnk() -> PathBuf {
    known(&FOLDERID_Programs).join(format!("{APP}.lnk"))
}
fn desktop_lnk() -> PathBuf {
    known(&FOLDERID_Desktop).join(format!("{APP}.lnk"))
}

// ---------------------------------------------------------------- dialogs

unsafe extern "system" fn td_callback(_h: HWND, msg: TASKDIALOG_NOTIFICATIONS, _w: WPARAM, l: LPARAM, _d: isize) -> windows::core::HRESULT {
    if msg == TDN_HYPERLINK_CLICKED {
        unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(l.0 as *const u16), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
    S_OK
}

/// Returns (button id, checkbox state).
fn dialog(title: &str, content: &str, buttons: &[(i32, &str)], checkbox: Option<(&str, bool)>, warn: bool) -> (i32, bool) {
    let title_h = HSTRING::from(APP);
    let main_h = HSTRING::from(title);
    let content_h = HSTRING::from(content);
    let btn_texts: Vec<HSTRING> = buttons.iter().map(|b| HSTRING::from(b.1)).collect();
    let btns: Vec<TASKDIALOG_BUTTON> = buttons.iter().zip(&btn_texts).map(|(b, t)| TASKDIALOG_BUTTON { nButtonID: b.0, pszButtonText: PCWSTR(t.as_ptr()) }).collect();
    let cb_h = checkbox.map(|c| HSTRING::from(c.0));
    let mut flags = TDF_ENABLE_HYPERLINKS | TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW;
    if checkbox.map(|c| c.1).unwrap_or(false) {
        flags |= TDF_VERIFICATION_FLAG_CHECKED;
    }
    let hinst = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default() };
    let icon = unsafe { LoadIconW(Some(hinst.into()), PCWSTR(1 as _)).unwrap_or_default() };
    if !warn {
        flags |= TDF_USE_HICON_MAIN;
    }
    let cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hInstance: hinst.into(),
        dwFlags: flags,
        pszWindowTitle: PCWSTR(title_h.as_ptr()),
        Anonymous1: if warn { TASKDIALOGCONFIG_0 { pszMainIcon: TD_WARNING_ICON } } else { TASKDIALOGCONFIG_0 { hMainIcon: icon } },
        pszMainInstruction: PCWSTR(main_h.as_ptr()),
        pszContent: PCWSTR(content_h.as_ptr()),
        cButtons: btns.len() as u32,
        pButtons: btns.as_ptr(),
        nDefaultButton: buttons.first().map(|b| b.0).unwrap_or(0),
        pszVerificationText: cb_h.as_ref().map(|h| PCWSTR(h.as_ptr())).unwrap_or(PCWSTR::null()),
        pfCallback: Some(td_callback),
        cxWidth: 0,
        ..Default::default()
    };
    let mut pressed = 0i32;
    let mut checked = windows::core::BOOL(0);
    unsafe {
        let _ = TaskDialogIndirect(&cfg, Some(&mut pressed), None, Some(&mut checked));
    }
    (pressed, checked.as_bool())
}

fn info(title: &str, content: &str, warn: bool) {
    dialog(title, content, &[(1, "OK")], None, warn);
}

// ---------------------------------------------------------------- install

/// In update mode the browser is still shutting down: retry while realism.exe is locked.
fn write_payload_wait(dir: &Path, wait: bool) -> Result<PathBuf, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(if wait { 60 } else { 0 });
    loop {
        match write_payload(dir) {
            Ok(p) => return Ok(p),
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    }
}

fn write_payload(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Не удалось создать папку {}: {e}", dir.display()))?;
    let exe = dir.join("realism.exe");
    let tmp = dir.join("realism.exe.new");
    std::fs::write(&tmp, PAYLOAD).map_err(|e| format!("Не удалось записать файл: {e}"))?;
    if exe.exists() {
        // Fails while the browser is running from this folder.
        std::fs::remove_file(&exe).map_err(|_| "Realism Browser сейчас запущен. Закройте все его окна и повторите установку.".to_string())?;
    }
    std::fs::rename(&tmp, &exe).map_err(|e| format!("Не удалось установить файл: {e}"))?;
    // Keep a copy of the setup as the uninstaller.
    if let Ok(me) = std::env::current_exe() {
        let un = dir.join("uninstall.exe");
        if me != un {
            let _ = std::fs::copy(&me, &un);
        }
    }
    Ok(exe)
}

fn register(dir: &Path, exe: &Path) {
    let hk = HKEY_CURRENT_USER;
    let exe_s = exe.to_string_lossy().to_string();
    let un = dir.join("uninstall.exe").to_string_lossy().to_string();
    // Apps & features
    reg_set(hk, UNINSTALL_KEY, Some("DisplayName"), APP);
    reg_set(hk, UNINSTALL_KEY, Some("DisplayVersion"), VERSION);
    reg_set(hk, UNINSTALL_KEY, Some("Publisher"), "Realism");
    reg_set(hk, UNINSTALL_KEY, Some("DisplayIcon"), &format!("{exe_s},0"));
    reg_set(hk, UNINSTALL_KEY, Some("InstallLocation"), &dir.to_string_lossy());
    reg_set(hk, UNINSTALL_KEY, Some("UninstallString"), &format!("\"{un}\" --uninstall"));
    reg_set(hk, UNINSTALL_KEY, Some("QuietUninstallString"), &format!("\"{un}\" --uninstall --silent"));
    reg_set_dword(hk, UNINSTALL_KEY, "NoModify", 1);
    reg_set_dword(hk, UNINSTALL_KEY, "NoRepair", 1);
    reg_set_dword(hk, UNINSTALL_KEY, "EstimatedSize", ((PAYLOAD.len() * 2) / 1024) as u32);
    // Browser registration (appears in Settings > Apps > Default apps; does not change the default).
    let open = format!("\"{exe_s}\" \"%1\"");
    reg_set(hk, &format!(r"Software\Classes\{PROGID}"), None, "Realism HTML Document");
    reg_set(hk, &format!(r"Software\Classes\{PROGID}\DefaultIcon"), None, &format!("{exe_s},0"));
    reg_set(hk, &format!(r"Software\Classes\{PROGID}\shell\open\command"), None, &open);
    reg_set(hk, CLIENT_KEY, None, APP);
    reg_set(hk, &format!(r"{CLIENT_KEY}\DefaultIcon"), None, &format!("{exe_s},0"));
    reg_set(hk, &format!(r"{CLIENT_KEY}\shell\open\command"), None, &format!("\"{exe_s}\""));
    let caps = format!(r"{CLIENT_KEY}\Capabilities");
    reg_set(hk, &caps, Some("ApplicationName"), APP);
    reg_set(hk, &caps, Some("ApplicationDescription"), "Лёгкий браузер, который бережёт оперативную память");
    reg_set(hk, &caps, Some("ApplicationIcon"), &format!("{exe_s},0"));
    for proto in ["http", "https"] {
        reg_set(hk, &format!(r"{caps}\URLAssociations"), Some(proto), PROGID);
    }
    for ext in [".htm", ".html", ".shtml", ".xhtml", ".pdf", ".svg", ".webp"] {
        reg_set(hk, &format!(r"{caps}\FileAssociations"), Some(ext), PROGID);
    }
    reg_set(hk, &format!(r"{caps}\StartMenu"), Some("StartMenuInternet"), "RealismBrowser");
    reg_set(hk, r"Software\RegisteredApplications", Some(APP), &caps);
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    n(a) == n(b)
}

fn install(silent: bool, desktop_flag: bool, dir: PathBuf, update: bool, relaunch: bool) -> i32 {
    let wv2 = webview2_installed();
    let mut desktop = desktop_flag;
    if !silent {
        let note = if wv2 {
            "Microsoft Edge WebView2 Runtime: найден ✓".to_string()
        } else {
            "⚠ Microsoft Edge WebView2 Runtime не найден. Браузеру он нужен для отображения сайтов — \
             скачайте его бесплатно: <a href=\"https://go.microsoft.com/fwlink/p/?LinkId=2124703\">WebView2 Runtime</a>"
                .to_string()
        };
        let content = format!(
            "Версия {VERSION}. Установка для текущего пользователя, права администратора не нужны.\n\nПапка: {}\n\n{note}",
            dir.display()
        );
        let (b, cb) = dialog("Установить Realism Browser?", &content, &[(100, "Установить"), (2, "Отмена")], Some(("Создать ярлык на рабочем столе", true)), false);
        if b != 100 {
            return 1;
        }
        desktop = cb;
    }
    let exe = match write_payload_wait(&dir, update) {
        Ok(e) => e,
        Err(e) => {
            if !silent {
                info("Установка не выполнена", &e, true);
            }
            return 2;
        }
    };
    // An update of a portable / other copy must not take over the registration of the installed one.
    let installed = reg_get(HKEY_CURRENT_USER, UNINSTALL_KEY, "InstallLocation").map(PathBuf::from);
    let owns_registration = !update || installed.as_ref().map(|p| same_dir(p, &dir)).unwrap_or(true);
    if owns_registration {
        register(&dir, &exe);
        make_shortcut(&start_menu_lnk(), &exe);
    }
    if desktop && owns_registration {
        make_shortcut(&desktop_lnk(), &exe);
    }
    if relaunch {
        let _ = std::process::Command::new(&exe).spawn();
    }
    if !silent {
        let (b, _) = dialog(
            "Realism Browser установлен",
            "Ярлык добавлен в меню «Пуск». Сделать Realism браузером по умолчанию можно в Параметры → Приложения → Приложения по умолчанию.",
            &[(100, "Запустить Realism"), (1, "Готово")],
            None,
            false,
        );
        if b == 100 {
            let _ = std::process::Command::new(&exe).spawn();
        }
    }
    0
}

// ---------------------------------------------------------------- uninstall

fn uninstall(silent: bool, purge_flag: bool) -> i32 {
    let dir = reg_get(HKEY_CURRENT_USER, UNINSTALL_KEY, "InstallLocation").map(PathBuf::from).unwrap_or_else(default_dir);
    let mut purge = purge_flag;
    if !silent {
        let (b, cb) = dialog(
            "Удалить Realism Browser?",
            &format!("Будут удалены программа ({}), ярлыки и записи реестра.", dir.display()),
            &[(100, "Удалить"), (2, "Отмена")],
            Some(("Удалить также данные профиля (история, закладки, cookies, пароли)", false)),
            false,
        );
        if b != 100 {
            return 1;
        }
        purge = cb;
    }
    let exe = dir.join("realism.exe");
    if exe.exists() && std::fs::OpenOptions::new().write(true).open(&exe).is_err() {
        if !silent {
            info("Realism Browser запущен", "Закройте все окна браузера и повторите удаление.", true);
        }
        return 3;
    }
    let _ = std::fs::remove_file(start_menu_lnk());
    let _ = std::fs::remove_file(desktop_lnk());
    unsafe {
        let hk = HKEY_CURRENT_USER;
        let _ = RegDeleteTreeW(hk, &HSTRING::from(UNINSTALL_KEY));
        let _ = RegDeleteTreeW(hk, &HSTRING::from(CLIENT_KEY));
        let _ = RegDeleteTreeW(hk, &HSTRING::from(format!(r"Software\Classes\{PROGID}")));
        let _ = RegDeleteKeyValueW(hk, w!(r"Software\RegisteredApplications"), &HSTRING::from(APP));
        let _ = RegDeleteKeyW(hk, &HSTRING::from(UNINSTALL_KEY));
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    if purge {
        let profile = known(&FOLDERID_LocalAppData).join("RealismBrowser");
        let _ = std::fs::remove_dir_all(profile);
    }
    let _ = std::fs::remove_file(&exe);
    // The running uninstaller cannot delete itself: let cmd remove the folder after we exit.
    let me = std::env::current_exe().unwrap_or_default();
    if me.starts_with(&dir) {
        let _ = std::process::Command::new("cmd")
            .raw_arg(format!("/c ping 127.0.0.1 -n 3 >nul & rmdir /s /q \"{}\"", dir.display()))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    } else {
        let _ = std::fs::remove_dir_all(&dir);
    }
    if !silent {
        info("Realism Browser удалён", if purge { "Программа и данные профиля удалены." } else { "Программа удалена. Данные профиля сохранены." }, false);
    }
    0
}

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let dir = args.iter().position(|a| a == "--dir").and_then(|i| args.get(i + 1)).map(PathBuf::from).unwrap_or_else(default_dir);
    let code = if has("--uninstall") {
        uninstall(has("--silent"), has("--purge"))
    } else {
        install(has("--silent"), has("--desktop"), dir, has("--update"), has("--relaunch"))
    };
    std::process::exit(code);
}
