//! Realism colour palettes (dark is the signature look; light is the alternative).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct C(pub f32, pub f32, pub f32, pub f32);

pub const fn hex(v: u32) -> C {
    C(((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0, 1.0)
}

impl C {
    pub const fn a(self, a: f32) -> C {
        C(self.0, self.1, self.2, a)
    }
    pub fn mix(self, o: C, t: f32) -> C {
        C(self.0 + (o.0 - self.0) * t, self.1 + (o.1 - self.1) * t, self.2 + (o.2 - self.2) * t, self.3 + (o.3 - self.3) * t)
    }
    pub fn colorref(self) -> u32 {
        let r = (self.0 * 255.0) as u32;
        let g = (self.1 * 255.0) as u32;
        let b = (self.2 * 255.0) as u32;
        r | (g << 8) | (b << 16)
    }
    pub fn rgb8(self) -> (u8, u8, u8) {
        ((self.0 * 255.0) as u8, (self.1 * 255.0) as u8, (self.2 * 255.0) as u8)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    pub strip: C,
    pub strip_private: C,
    pub tab_active: C,
    pub tab_hover: C,
    pub toolbar: C,
    pub addr: C,
    pub addr_hover: C,
    pub addr_focus: C,
    pub text: C,
    pub text_dim: C,
    pub text_faint: C,
    pub accent: C,
    pub accent2: C,
    pub btn_hover: C,
    pub btn_press: C,
    pub sep: C,
    pub close_red: C,
    pub info_bg: C,
    pub warn: C,
    pub ok: C,
    pub content_bg: C,
    pub tile: C,
    pub tile_hover: C,
    pub popup: C,
    pub popup_border: C,
    pub popup_hover: C,
}

pub const DARK: Theme = Theme {
    dark: true,
    strip: hex(0x0E1014),
    strip_private: hex(0x1A1030),
    tab_active: hex(0x1E2129),
    tab_hover: C(1.0, 1.0, 1.0, 0.055),
    toolbar: hex(0x1E2129),
    addr: hex(0x14161B),
    addr_hover: hex(0x191C22),
    addr_focus: hex(0x8B7CFF),
    text: hex(0xE9EAEE),
    text_dim: hex(0x9EA3AE),
    text_faint: hex(0x6B7180),
    accent: hex(0x8B7CFF),
    accent2: hex(0x3DD9C5),
    btn_hover: C(1.0, 1.0, 1.0, 0.07),
    btn_press: C(1.0, 1.0, 1.0, 0.12),
    sep: C(1.0, 1.0, 1.0, 0.07),
    close_red: hex(0xC42B1C),
    info_bg: hex(0x262A36),
    warn: hex(0xF2B84B),
    ok: hex(0x3DD9C5),
    content_bg: hex(0x15171C),
    tile: hex(0x1E2129),
    tile_hover: hex(0x272B35),
    popup: hex(0x23262F),
    popup_border: C(1.0, 1.0, 1.0, 0.09),
    popup_hover: C(1.0, 1.0, 1.0, 0.07),
};

pub const LIGHT: Theme = Theme {
    dark: false,
    strip: hex(0xE6E8EF),
    strip_private: hex(0xD9D0F5),
    tab_active: hex(0xFFFFFF),
    tab_hover: C(0.0, 0.0, 0.0, 0.05),
    toolbar: hex(0xFFFFFF),
    addr: hex(0xF0F2F6),
    addr_hover: hex(0xE8EBF1),
    addr_focus: hex(0x6A5AE0),
    text: hex(0x1B1D23),
    text_dim: hex(0x5D6370),
    text_faint: hex(0x9298A4),
    accent: hex(0x6A5AE0),
    accent2: hex(0x0FA593),
    btn_hover: C(0.0, 0.0, 0.0, 0.06),
    btn_press: C(0.0, 0.0, 0.0, 0.11),
    sep: C(0.0, 0.0, 0.0, 0.08),
    close_red: hex(0xC42B1C),
    info_bg: hex(0xEEF0FA),
    warn: hex(0xB7791F),
    ok: hex(0x0F8A7B),
    content_bg: hex(0xF6F7FA),
    tile: hex(0xFFFFFF),
    tile_hover: hex(0xF0F1F6),
    popup: hex(0xFFFFFF),
    popup_border: C(0.0, 0.0, 0.0, 0.10),
    popup_hover: C(0.0, 0.0, 0.0, 0.05),
};

/// Reads the Windows "apps use light theme" preference.
pub fn system_prefers_dark() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    use windows::core::w;
    let mut val: u32 = 1;
    let mut size = 4u32;
    unsafe {
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut val as *mut u32 as *mut _),
            Some(&mut size),
        );
        if r.is_err() {
            return true;
        }
    }
    val == 0
}

/// Stable colour for a letter avatar.
pub fn avatar_color(seed: u64) -> C {
    const P: [u32; 8] = [0x8B7CFF, 0x3DD9C5, 0xF2706B, 0xF2B84B, 0x5AA9F2, 0xD37CF2, 0x6BD46B, 0xF28F4B];
    hex(P[(seed % P.len() as u64) as usize])
}
