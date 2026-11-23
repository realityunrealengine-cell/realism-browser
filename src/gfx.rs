//! Direct2D + DirectWrite drawing for the native browser chrome.
//!
//! Uses a *software* DC render target: no D3D device, no GPU swap chain, a few MB of RAM.
//! The browser chrome is small, so CPU rasterisation is effectively free.

use std::cell::RefCell;

use windows::core::{w, Interface, PCWSTR};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER};
use windows_numerics::Vector2;

use crate::theme::C;

pub mod glyph {
    pub const BACK: char = '\u{E72B}';
    pub const FORWARD: char = '\u{E72A}';
    pub const REFRESH: char = '\u{E72C}';
    pub const STOP: char = '\u{E711}';
    pub const ADD: char = '\u{E710}';
    pub const MORE: char = '\u{E712}';
    pub const DOWNLOAD: char = '\u{E896}';
    pub const STAR: char = '\u{E734}';
    pub const STAR_FILL: char = '\u{E735}';
    pub const LOCK: char = '\u{E72E}';
    pub const INFO: char = '\u{E946}';
    pub const WARN: char = '\u{E7BA}';
    pub const MIN: char = '\u{E921}';
    pub const MAX: char = '\u{E922}';
    pub const RESTORE: char = '\u{E923}';
    pub const CLOSE: char = '\u{E8BB}';
    pub const VOLUME: char = '\u{E767}';
    pub const MUTE: char = '\u{E74F}';
    pub const MOON: char = '\u{E708}';
    pub const GLOBE: char = '\u{E774}';
    pub const SHIELD: char = '\u{EA18}';
    pub const FOLDER: char = '\u{E8B7}';
    pub const SEARCH: char = '\u{E721}';
    pub const SETTINGS: char = '\u{E713}';
    pub const HISTORY: char = '\u{E81C}';
    pub const CAMERA: char = '\u{E722}';
    pub const POPUP: char = '\u{E8A7}';
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> R {
        R { x, y, w, h }
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn inset(&self, dx: f32, dy: f32) -> R {
        R::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    pub fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
    pub fn cx(&self) -> f32 {
        self.x + self.w / 2.0
    }
    pub fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
    pub fn px(&self, scale: f32) -> RECT {
        RECT {
            left: (self.x * scale).round() as i32,
            top: (self.y * scale).round() as i32,
            right: ((self.x + self.w) * scale).round() as i32,
            bottom: ((self.y + self.h) * scale).round() as i32,
        }
    }
}

fn col(c: C) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 }
}

fn v2(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

pub struct Fonts {
    pub tab: IDWriteTextFormat,
    pub ui: IDWriteTextFormat,
    pub ui_center: IDWriteTextFormat,
    pub ui_bold: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub small_center: IDWriteTextFormat,
    pub icon: IDWriteTextFormat,
    pub icon_sm: IDWriteTextFormat,
    pub icon_xs: IDWriteTextFormat,
    pub icon_lg: IDWriteTextFormat,
    pub hero: IDWriteTextFormat,
    pub hero_sub: IDWriteTextFormat,
    pub tile: IDWriteTextFormat,
    pub letter: IDWriteTextFormat,
    pub menu: IDWriteTextFormat,
    pub menu_right: IDWriteTextFormat,
}

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub fonts: Fonts,
}

fn has_family(dw: &IDWriteFactory, name: PCWSTR) -> bool {
    unsafe {
        let mut coll: Option<IDWriteFontCollection> = None;
        if dw.GetSystemFontCollection(&mut coll, false).is_err() {
            return false;
        }
        let Some(coll) = coll else { return false };
        let mut idx = 0u32;
        let mut exists = windows::core::BOOL(0);
        coll.FindFamilyName(name, &mut idx, &mut exists).is_ok() && exists.as_bool()
    }
}

fn fmt(
    dw: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
    ellipsis: bool,
) -> windows::core::Result<IDWriteTextFormat> {
    unsafe {
        let f = dw.CreateTextFormat(family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, w!(""))?;
        f.SetTextAlignment(align)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        if ellipsis {
            let sign = dw.CreateEllipsisTrimmingSign(&f)?;
            let t = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            f.SetTrimming(&t, &sign)?;
        }
        Ok(f)
    }
}

impl Gfx {
    pub fn new() -> windows::core::Result<Gfx> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text = if has_family(&dw, w!("Segoe UI Variable Text")) { w!("Segoe UI Variable Text") } else { w!("Segoe UI") };
            let display = if has_family(&dw, w!("Segoe UI Variable Display")) { w!("Segoe UI Variable Display") } else { w!("Segoe UI") };
            let icons = if has_family(&dw, w!("Segoe Fluent Icons")) { w!("Segoe Fluent Icons") } else { w!("Segoe MDL2 Assets") };
            let l = DWRITE_TEXT_ALIGNMENT_LEADING;
            let c = DWRITE_TEXT_ALIGNMENT_CENTER;
            let t = DWRITE_TEXT_ALIGNMENT_TRAILING;
            let n = DWRITE_FONT_WEIGHT_NORMAL;
            let sb = DWRITE_FONT_WEIGHT_SEMI_BOLD;
            let fonts = Fonts {
                tab: fmt(&dw, text, 12.5, n, l, true)?,
                ui: fmt(&dw, text, 13.0, n, l, true)?,
                ui_center: fmt(&dw, text, 13.0, n, c, true)?,
                ui_bold: fmt(&dw, text, 13.0, sb, l, true)?,
                small: fmt(&dw, text, 11.5, n, l, true)?,
                small_center: fmt(&dw, text, 11.5, sb, c, false)?,
                icon: fmt(&dw, icons, 15.0, n, c, false)?,
                icon_sm: fmt(&dw, icons, 12.0, n, c, false)?,
                icon_xs: fmt(&dw, icons, 10.0, n, c, false)?,
                icon_lg: fmt(&dw, icons, 18.0, n, c, false)?,
                hero: fmt(&dw, display, 54.0, sb, c, false)?,
                hero_sub: fmt(&dw, text, 14.0, n, c, false)?,
                tile: fmt(&dw, text, 12.0, n, c, true)?,
                letter: fmt(&dw, display, 18.0, sb, c, false)?,
                menu: fmt(&dw, text, 13.0, n, l, true)?,
                menu_right: fmt(&dw, text, 12.0, n, t, false)?,
            };
            Ok(Gfx { d2d, dw, fonts })
        }
    }

    /// Width of a single line of text in DIPs.
    pub fn measure(&self, s: &str, f: &IDWriteTextFormat) -> f32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dw.CreateTextLayout(&w, f, 4096.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 }
        }
    }
}

/// A software Direct2D render target bound to a window DC during WM_PAINT.
pub struct Canvas {
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    pub alloc_area: i64,
}

impl Canvas {
    pub fn new(gfx: &Gfx) -> windows::core::Result<Canvas> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_IGNORE },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_GDI_COMPATIBLE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = gfx.d2d.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            let brush = rt.CreateSolidColorBrush(&col(C(1.0, 1.0, 1.0, 1.0)), None)?;
            Ok(Canvas { rt, brush, alloc_area: 0 })
        }
    }

    /// Bind to `hdc` for the pixel rect `rc`. Drawing coordinates are DIPs relative to the window client origin.
    pub fn begin(&mut self, hdc: HDC, rc: &RECT, dpi: u32) -> bool {
        unsafe {
            if self.rt.BindDC(hdc, rc).is_err() {
                return false;
            }
            self.alloc_area = self.alloc_area.max(((rc.right - rc.left) as i64) * ((rc.bottom - rc.top) as i64));
            self.rt.SetDpi(dpi as f32, dpi as f32);
            self.rt.BeginDraw();
            let s = dpi as f32 / 96.0;
            // BindDC maps the sub-rect origin to (0,0); shift so we can draw in window coordinates.
            self.rt.SetTransform(&windows_numerics::Matrix3x2::translation(-(rc.left as f32) / s, -(rc.top as f32) / s));
            true
        }
    }

    pub fn end(&mut self) -> bool {
        unsafe { self.rt.EndDraw(None, None).is_ok() }
    }

    fn b(&self, c: C) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&col(c)) };
        &self.brush
    }

    pub fn clear(&self, c: C) {
        unsafe { self.rt.Clear(Some(&col(c))) }
    }

    pub fn fill(&self, r: R, c: C) {
        unsafe { self.rt.FillRectangle(&r.d2d(), self.b(c)) }
    }

    pub fn round(&self, r: R, rad: f32, c: C) {
        let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: rad, radiusY: rad };
        unsafe { self.rt.FillRoundedRectangle(&rr, self.b(c)) }
    }

    pub fn round_stroke(&self, r: R, rad: f32, c: C, width: f32) {
        let r2 = r.inset(width / 2.0, width / 2.0);
        let rr = D2D1_ROUNDED_RECT { rect: r2.d2d(), radiusX: rad, radiusY: rad };
        unsafe { self.rt.DrawRoundedRectangle(&rr, self.b(c), width, None) }
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: C, width: f32) {
        unsafe { self.rt.DrawLine(v2(x0, y0), v2(x1, y1), self.b(c), width, None) }
    }

    pub fn circle(&self, cx: f32, cy: f32, r: f32, c: C) {
        let e = D2D1_ELLIPSE { point: v2(cx, cy), radiusX: r, radiusY: r };
        unsafe { self.rt.FillEllipse(&e, self.b(c)) }
    }

    pub fn text(&self, s: &str, f: &IDWriteTextFormat, r: R, c: C) {
        if s.is_empty() {
            return;
        }
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.rt.DrawText(&w, f, &r.d2d(), self.b(c), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    pub fn icon(&self, g: char, f: &IDWriteTextFormat, r: R, c: C) {
        let mut buf = [0u16; 2];
        let s = g.encode_utf16(&mut buf);
        unsafe {
            self.rt.DrawText(s, f, &r.d2d(), self.b(c), D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    /// Arc around (cx,cy) from angle `a0` sweeping `sweep` radians (clockwise).
    pub fn arc(&self, gfx: &Gfx, cx: f32, cy: f32, r: f32, a0: f32, sweep: f32, c: C, width: f32) {
        let sweep = sweep.clamp(0.01, std::f32::consts::TAU - 0.01);
        let p0 = v2(cx + r * a0.cos(), cy + r * a0.sin());
        let a1 = a0 + sweep;
        let p1 = v2(cx + r * a1.cos(), cy + r * a1.sin());
        unsafe {
            let Ok(geom) = gfx.d2d.CreatePathGeometry() else { return };
            let Ok(sink) = geom.Open() else { return };
            sink.BeginFigure(p0, D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: p1,
                size: D2D_SIZE_F { width: r, height: r },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if sweep > std::f32::consts::PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            if sink.Close().is_err() {
                return;
            }
            let Ok(style) = gfx.d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    miterLimit: 1.0,
                    dashStyle: D2D1_DASH_STYLE_SOLID,
                    dashOffset: 0.0,
                },
                None,
            ) else {
                return;
            };
            self.rt.DrawGeometry(&geom, self.b(c), width, &style);
        }
    }

    pub fn bitmap(&self, px: &[u8], w: u32, h: u32, r: R, opacity: f32) {
        if px.len() < (w * h * 4) as usize || w == 0 {
            return;
        }
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            if let Ok(bmp) = self.rt.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(px.as_ptr() as *const _), w * 4, &props) {
                self.rt.DrawBitmap(&bmp, Some(&r.d2d()), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
            }
        }
    }

    pub fn linear_text(&self, s: &str, f: &IDWriteTextFormat, r: R, c0: C, c1: C) {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: col(c0) }, D2D1_GRADIENT_STOP { position: 1.0, color: col(c1) }];
            let Ok(coll) = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v2(r.x + r.w * 0.3, r.y), endPoint: v2(r.x + r.w * 0.7, r.bottom()) };
            let Ok(br) = self.rt.CreateLinearGradientBrush(&props, None, &coll) else { return };
            self.rt.DrawText(&w, f, &r.d2d(), &br, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    pub fn linear_rect(&self, r: R, rad: f32, c0: C, c1: C) {
        unsafe {
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: col(c0) }, D2D1_GRADIENT_STOP { position: 1.0, color: col(c1) }];
            let Ok(coll) = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: v2(r.x, r.y), endPoint: v2(r.right(), r.y) };
            let Ok(br) = self.rt.CreateLinearGradientBrush(&props, None, &coll) else { return };
            let rr = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: rad, radiusY: rad };
            self.rt.FillRoundedRectangle(&rr, &br);
        }
    }

    pub fn glow(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: C) {
        unsafe {
            let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: col(c) }, D2D1_GRADIENT_STOP { position: 1.0, color: col(c.a(0.0)) }];
            let Ok(coll) = self.rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) else { return };
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: v2(cx, cy), gradientOriginOffset: v2(0.0, 0.0), radiusX: rx, radiusY: ry };
            let Ok(br) = self.rt.CreateRadialGradientBrush(&props, None, &coll) else { return };
            let e = D2D1_ELLIPSE { point: v2(cx, cy), radiusX: rx, radiusY: ry };
            self.rt.FillEllipse(&e, &br);
        }
    }

    pub fn clip(&self, r: R) {
        unsafe { self.rt.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) }
    }
    pub fn unclip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() }
    }
}

// ---------------------------------------------------------------- favicons via WIC

thread_local! {
    static WIC: RefCell<Option<IWICImagingFactory>> = const { RefCell::new(None) };
}

fn wic() -> Option<IWICImagingFactory> {
    WIC.with(|w| {
        let mut w = w.borrow_mut();
        if w.is_none() {
            *w = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok() };
        }
        w.clone()
    })
}

pub const ICON_PX: u32 = 32;

/// Decode an image stream (PNG favicon) into 32x32 premultiplied BGRA.
pub fn decode_icon(stream: &IStream) -> Option<Vec<u8>> {
    let f = wic()?;
    unsafe {
        let dec = f.CreateDecoderFromStream(stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let scaler = f.CreateBitmapScaler().ok()?;
        scaler.Initialize(&frame, ICON_PX, ICON_PX, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut buf = vec![0u8; (ICON_PX * ICON_PX * 4) as usize];
        conv.CopyPixels(std::ptr::null(), ICON_PX * 4, &mut buf).ok()?;
        let _ = stream.cast::<windows::core::IUnknown>();
        Some(buf)
    }
}
