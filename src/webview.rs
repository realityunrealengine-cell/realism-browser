//! WebView2 environment + per-tab webview creation (wry) and event wiring (webview2-com).

use std::path::Path;

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::{Rect, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};

use crate::events::{push, push_proto, Ev, ProcInfo, ProtoReq};
use crate::util::HwndRef;

type Token = i64;

pub struct Wv {
    pub wry: wry::WebView,
    pub core: ICoreWebView2,
    pub ctrl: ICoreWebView2Controller,
    pub frame_id: u32,
}

/// Injected into every document: lets the tab sleeper know that the user typed something,
/// so a tab with unsent form input is never discarded silently.
const TRACK_JS: &str = r#"(()=>{try{if(window.top!==window||!window.chrome||!chrome.webview)return;let d=0;
const p=m=>{try{chrome.webview.postMessage(m)}catch(e){}};
addEventListener('input',e=>{const t=e.target;if(d||!t)return;
if(t.isContentEditable||t.tagName==='TEXTAREA'||(t.tagName==='INPUT'&&!/^(search|checkbox|radio|range|button|submit|color)$/.test(t.type))){d=1;p('__rb:dirty')}},true);
addEventListener('submit',()=>{if(d){d=0;p('__rb:clean')}},true);}catch(e){}})();"#;

pub fn browser_args(process_per_site: bool, no_spare_renderer: bool, autoplay: bool) -> String {
    // msWebOOUI = Edge "mini menu" popup on text selection, msPdfOOUI = its PDF variant.
    // SpareRendererForSitePerProcess = Chromium keeps an extra idle renderer process pre-launched
    // for faster navigations (~20-40 MB permanently); we trade a few ms of navigation for that RAM.
    let mut a = String::from("--disable-features=msWebOOUI,msPdfOOUI");
    if no_spare_renderer {
        a.push_str(",SpareRendererForSitePerProcess");
    }
    if process_per_site {
        // Tabs of the same site share one renderer process instead of one process per tab.
        a.push_str(" --process-per-site");
    }
    if autoplay {
        // Benchmark mode only: identical conditions to the other browsers under test.
        a.push_str(" --autoplay-policy=no-user-gesture-required --mute-audio");
    }
    a
}

/// Starts asynchronous creation of the shared WebView2 environment (browser process).
pub fn create_env(data_dir: &Path, args: &str) -> windows::core::Result<()> {
    let options = CoreWebView2EnvironmentOptions::default();
    unsafe {
        options.set_additional_browser_arguments(args.to_string());
        let dir = HSTRING::from(data_dir.to_string_lossy().as_ref());
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &dir,
            &ICoreWebView2EnvironmentOptions::from(options),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|hr, env| {
                let r = hr.and_then(|_| env.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                if let Ok(e) = &r {
                    if let Ok(e5) = e.cast::<ICoreWebView2Environment5>() {
                        let mut t: Token = 0;
                        let _ = e5.add_BrowserProcessExited(
                            &BrowserProcessExitedEventHandler::create(Box::new(|_, args| {
                                if let Some(args) = args {
                                    let mut kind = COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND::default();
                                    let _ = args.BrowserProcessExitKind(&mut kind);
                                    if kind == COREWEBVIEW2_BROWSER_PROCESS_EXIT_KIND_FAILED {
                                        push(Ev::BrowserExited);
                                    }
                                }
                                Ok(())
                            })),
                            &mut t,
                        );
                    }
                }
                push(Ev::EnvCreated(r));
                Ok(())
            })),
        )
    }
}

pub fn runtime_version() -> String {
    unsafe {
        let mut v = PWSTR::null();
        if GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut v).is_ok() {
            return take_pwstr(v);
        }
    }
    String::new()
}

pub struct CreateOpts<'a> {
    pub env: &'a ICoreWebView2Environment,
    pub parent: HWND,
    pub tab: u64,
    pub private: bool,
    pub url: Option<&'a str>,
    pub bounds: RECT,
    pub visible: bool,
    pub bg: (u8, u8, u8),
    pub scheme: i32,
}

pub fn create(o: CreateOpts) -> std::result::Result<Wv, String> {
    let id = o.tab.to_string();
    let tab = o.tab;
    let r = o.bounds;
    let mut b = WebViewBuilder::new()
        .with_id(&id)
        .with_bounds(Rect {
            position: PhysicalPosition::new(r.left, r.top).into(),
            size: PhysicalSize::new((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32).into(),
        })
        .with_visible(o.visible)
        .with_focused(false)
        .with_incognito(o.private)
        .with_devtools(true)
        .with_hotkeys_zoom(true)
        .with_back_forward_navigation_gestures(true)
        .with_background_color((o.bg.0, o.bg.1, o.bg.2, 255))
        .with_initialization_script(TRACK_JS)
        .with_ipc_handler(move |req| push(Ev::Ipc { tab, msg: req.into_body() }))
        .with_asynchronous_custom_protocol("realism".into(), |id, req, responder| {
            push_proto(ProtoReq { webview_id: id.to_string(), req, responder })
        })
        .with_environment(o.env.clone())
        .with_https_scheme(true)
        .with_default_context_menus(true)
        .with_browser_accelerator_keys(true);
    if let Some(u) = o.url {
        b = b.with_url(u);
    }
    let w = b.build_as_child(&HwndRef(o.parent)).map_err(|e| e.to_string())?;
    let core = w.webview();
    let ctrl = w.controller();
    unsafe {
        if let Ok(s) = core.Settings() {
            if let Ok(s4) = s.cast::<ICoreWebView2Settings4>() {
                let _ = s4.SetIsPasswordAutosaveEnabled(!o.private);
                let _ = s4.SetIsGeneralAutofillEnabled(true);
            }
            if let Ok(s8) = s.cast::<ICoreWebView2Settings8>() {
                let _ = s8.SetIsReputationCheckingRequired(true);
            }
        }
    }
    set_color_scheme(&core, o.scheme);
    attach(tab, &core, &ctrl).map_err(|e| e.to_string())?;
    let mut frame_id = 0u32;
    unsafe {
        if let Ok(c20) = core.cast::<ICoreWebView2_20>() {
            let _ = c20.FrameId(&mut frame_id);
        }
    }
    Ok(Wv { wry: w, core, ctrl, frame_id })
}

/// 0 = auto, 1 = light, 2 = dark (COREWEBVIEW2_PREFERRED_COLOR_SCHEME)
pub fn set_color_scheme(core: &ICoreWebView2, scheme: i32) {
    unsafe {
        if let Ok(c13) = core.cast::<ICoreWebView2_13>() {
            if let Ok(p) = c13.Profile() {
                let _ = p.SetPreferredColorScheme(COREWEBVIEW2_PREFERRED_COLOR_SCHEME(scheme));
            }
        }
    }
}

fn pwstr_arg(f: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if f(&mut p).is_ok() { take_pwstr(p) } else { String::new() }
}

fn attach(tab: u64, core: &ICoreWebView2, ctrl: &ICoreWebView2Controller) -> windows::core::Result<()> {
    let mut t: Token = 0;
    unsafe {
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(a) = args {
                    let url = pwstr_arg(|p| a.Uri(p));
                    let mut user = windows::core::BOOL(0);
                    let _ = a.IsUserInitiated(&mut user);
                    let mut kind = COREWEBVIEW2_NAVIGATION_KIND::default();
                    if let Ok(a3) = a.cast::<ICoreWebView2NavigationStartingEventArgs3>() {
                        let _ = a3.NavigationKind(&mut kind);
                    }
                    push(Ev::NavStarting { tab, url, kind: kind.0, user: user.as_bool() });
                }
                Ok(())
            })),
            &mut t,
        )?;
        core.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                let mut ok = windows::core::BOOL(1);
                let mut st = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                if let Some(a) = args {
                    let _ = a.IsSuccess(&mut ok);
                    let _ = a.WebErrorStatus(&mut st);
                }
                push(Ev::NavCompleted { tab, ok: ok.as_bool(), status: st.0 });
                Ok(())
            })),
            &mut t,
        )?;
        core.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |_, _| {
                push(Ev::SourceChanged { tab });
                Ok(())
            })),
            &mut t,
        )?;
        core.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |_, _| {
                push(Ev::TitleChanged { tab });
                Ok(())
            })),
            &mut t,
        )?;
        core.add_HistoryChanged(
            &HistoryChangedEventHandler::create(Box::new(move |_, _| {
                push(Ev::HistoryChanged { tab });
                Ok(())
            })),
            &mut t,
        )?;
        core.add_ContainsFullScreenElementChanged(
            &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |_, _| {
                push(Ev::Fullscreen { tab });
                Ok(())
            })),
            &mut t,
        )?;
        core.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(a) = args {
                    let deferral = a.GetDeferral()?;
                    let _ = a.SetHandled(true);
                    push(Ev::NewWindow { tab, args: a, deferral });
                }
                Ok(())
            })),
            &mut t,
        )?;
        core.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(a) = args {
                    if let Ok(a3) = a.cast::<ICoreWebView2PermissionRequestedEventArgs3>() {
                        // We persist decisions ourselves (per origin, visible in settings).
                        let _ = a3.SetSavesInProfile(false);
                    }
                    let deferral = a.GetDeferral()?;
                    push(Ev::Permission { tab, args: a, deferral });
                }
                Ok(())
            })),
            &mut t,
        )?;
        core.add_WindowCloseRequested(
            &WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                push(Ev::CloseRequested { tab });
                Ok(())
            })),
            &mut t,
        )?;
        core.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                let mut k = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                if let Some(a) = args {
                    let _ = a.ProcessFailedKind(&mut k);
                }
                push(Ev::ProcessFailed { tab, kind: k.0 });
                Ok(())
            })),
            &mut t,
        )?;
        if let Ok(c4) = core.cast::<ICoreWebView2_4>() {
            c4.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    if let Some(a) = args {
                        let deferral = a.GetDeferral()?;
                        // Hide the default WebView2 download flyout; we have our own download manager.
                        let _ = a.SetHandled(true);
                        push(Ev::Download { tab, args: a, deferral });
                    }
                    Ok(())
                })),
                &mut t,
            )?;
        }
        if let Ok(c8) = core.cast::<ICoreWebView2_8>() {
            c8.add_IsDocumentPlayingAudioChanged(
                &IsDocumentPlayingAudioChangedEventHandler::create(Box::new(move |_, _| {
                    push(Ev::Audio { tab });
                    Ok(())
                })),
                &mut t,
            )?;
            c8.add_IsMutedChanged(
                &IsMutedChangedEventHandler::create(Box::new(move |_, _| {
                    push(Ev::Audio { tab });
                    Ok(())
                })),
                &mut t,
            )?;
        }
        if let Ok(c14) = core.cast::<ICoreWebView2_14>() {
            c14.add_ServerCertificateErrorDetected(
                &ServerCertificateErrorDetectedEventHandler::create(Box::new(move |_, _| {
                    // Keep the default (safe) behaviour: WebView2 shows its blocking interstitial.
                    push(Ev::CertError { tab });
                    Ok(())
                })),
                &mut t,
            )?;
        }
        if let Ok(c15) = core.cast::<ICoreWebView2_15>() {
            c15.add_FaviconChanged(
                &FaviconChangedEventHandler::create(Box::new(move |wv, _| {
                    let Some(wv) = wv else { return Ok(()) };
                    let Ok(c15) = wv.cast::<ICoreWebView2_15>() else { return Ok(()) };
                    let uri = pwstr_arg(|p| c15.FaviconUri(p));
                    if uri.is_empty() {
                        push(Ev::Favicon { tab, pixels: None });
                        return Ok(());
                    }
                    let _ = c15.GetFavicon(
                        COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG,
                        &GetFaviconCompletedHandler::create(Box::new(move |hr, stream| {
                            let px = if hr.is_ok() { stream.as_ref().and_then(crate::gfx::decode_icon) } else { None };
                            push(Ev::Favicon { tab, pixels: px });
                            Ok(())
                        })),
                    );
                    Ok(())
                })),
                &mut t,
            )?;
        }
        ctrl.add_ZoomFactorChanged(
            &ZoomFactorChangedEventHandler::create(Box::new(move |_, _| {
                push(Ev::Zoom { tab });
                Ok(())
            })),
            &mut t,
        )?;
        ctrl.add_GotFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                push(Ev::Focus { tab });
                Ok(())
            })),
            &mut t,
        )?;
        ctrl.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(a) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                a.KeyEventKind(&mut kind)?;
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                    return Ok(());
                }
                let mut vk = 0u32;
                a.VirtualKey(&mut vk)?;
                crate::util::dlog(&format!("accel tab={tab} vk={vk} kind={} ctrl={}", kind.0, unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x11)
                }));
                let fs = crate::app::tab_in_browser_fullscreen(tab);
                if let Some(cmd) = crate::cmd::map_key(vk, true, fs) {
                    a.SetHandled(true)?;
                    push(Ev::Cmd { tab, cmd });
                }
                Ok(())
            })),
            &mut t,
        )?;
    }
    Ok(())
}

/// Freeze a hidden tab. WebView2 refuses to suspend a page that has never been visible
/// (a tab opened in the background), so such pages get one IsVisible true->false cycle first;
/// the wry container window stays hidden, so nothing appears on screen.
pub fn try_suspend(tab: u64, wv: &Wv, never_shown: bool) {
    let core = &wv.core;
    unsafe {
        if never_shown {
            let _ = wv.ctrl.SetIsVisible(true);
            let _ = wv.ctrl.SetIsVisible(false);
        }
        if let Ok(c3) = core.cast::<ICoreWebView2_3>() {
            crate::util::dlog(&format!("try_suspend tab={tab}"));
            let _ = c3.TrySuspend(&TrySuspendCompletedHandler::create(Box::new(move |hr, ok| {
                crate::util::dlog(&format!("try_suspend result tab={tab} hr={hr:?} ok={ok}"));
                push(Ev::Suspended { tab, ok: hr.is_ok() && ok });
                Ok(())
            })));
        }
    }
}

pub fn set_low_memory(core: &ICoreWebView2, low: bool) {
    unsafe {
        if let Ok(c19) = core.cast::<ICoreWebView2_19>() {
            let lvl = if low { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW } else { COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL };
            let _ = c19.SetMemoryUsageTargetLevel(lvl);
        }
    }
}

pub fn get_bool(f: impl FnOnce(*mut windows::core::BOOL) -> windows::core::Result<()>) -> bool {
    let mut b = windows::core::BOOL(0);
    f(&mut b).is_ok() && b.as_bool()
}

pub fn title(core: &ICoreWebView2) -> String {
    unsafe { pwstr_arg(|p| core.DocumentTitle(p)) }
}

pub fn source(core: &ICoreWebView2) -> String {
    unsafe { pwstr_arg(|p| core.Source(p)) }
}

pub fn navigate(core: &ICoreWebView2, url: &str) {
    unsafe {
        let _ = core.Navigate(&HSTRING::from(url));
    }
}

pub fn exec_js(core: &ICoreWebView2, js: &str) {
    unsafe {
        let _ = core.ExecuteScript(&HSTRING::from(js), None::<&ICoreWebView2ExecuteScriptCompletedHandler>);
    }
}

pub fn hard_reload(core: &ICoreWebView2) {
    unsafe {
        let _ = core.CallDevToolsProtocolMethod(
            &HSTRING::from("Page.reload"),
            &HSTRING::from("{\"ignoreCache\":true}"),
            None::<&ICoreWebView2CallDevToolsProtocolMethodCompletedHandler>,
        );
    }
}

/// Asks WebView2 which processes it runs and which frames each renderer hosts.
pub fn query_processes(env: &ICoreWebView2Environment, req: u64) -> bool {
    unsafe {
        let Ok(e13) = env.cast::<ICoreWebView2Environment13>() else { return false };
        e13.GetProcessExtendedInfos(&GetProcessExtendedInfosCompletedHandler::create(Box::new(move |hr, coll| {
            let mut infos = Vec::new();
            if let (Ok(()), Some(coll)) = (hr, coll) {
                let mut n = 0u32;
                let _ = coll.Count(&mut n);
                for i in 0..n {
                    let Ok(ext) = coll.GetValueAtIndex(i) else { continue };
                    let Ok(pi) = ext.ProcessInfo() else { continue };
                    let mut pid = 0i32;
                    let _ = pi.ProcessId(&mut pid);
                    let mut kind = COREWEBVIEW2_PROCESS_KIND::default();
                    let _ = pi.Kind(&mut kind);
                    let mut frames = Vec::new();
                    if let Ok(fc) = ext.AssociatedFrameInfos() {
                        if let Ok(it) = fc.GetIterator() {
                            let mut has = windows::core::BOOL(0);
                            let _ = it.HasCurrent(&mut has);
                            while has.as_bool() {
                                if let Ok(fi) = it.GetCurrent() {
                                    // Walk up to the main frame of the webview this frame belongs to.
                                    // Walk up to the top-level frame of this frame's webview.
                                    let mut main = fi.clone();
                                    for _ in 0..16 {
                                        match main.cast::<ICoreWebView2FrameInfo2>().and_then(|f2| f2.ParentFrameInfo()) {
                                            Ok(p) => main = p,
                                            Err(_) => break,
                                        }
                                    }
                                    if let Ok(m) = main.cast::<ICoreWebView2FrameInfo2>() {
                                        let mut id = 0u32;
                                        if m.FrameId(&mut id).is_ok() {
                                            frames.push(id);
                                        }
                                    }
                                }
                                if it.MoveNext(&mut has).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    infos.push(ProcInfo { pid: pid as u32, kind: kind.0, frames });
                }
            }
            push(Ev::PerfInfos { req, infos });
            Ok(())
        })))
        .is_ok()
    }
}

/// Clears browsing data of the profile used by `core`. `kinds` is a COREWEBVIEW2_BROWSING_DATA_KINDS bitmask.
pub fn clear_data(core: &ICoreWebView2, kinds: i32, req: u64) -> bool {
    unsafe {
        let Ok(c13) = core.cast::<ICoreWebView2_13>() else { return false };
        let Ok(p) = c13.Profile() else { return false };
        let Ok(p2) = p.cast::<ICoreWebView2Profile2>() else { return false };
        p2.ClearBrowsingData(
            COREWEBVIEW2_BROWSING_DATA_KINDS(kinds),
            &ClearBrowsingDataCompletedHandler::create(Box::new(move |hr| {
                push(Ev::ClearDone { req, ok: hr.is_ok() });
                Ok(())
            })),
        )
        .is_ok()
    }
}
