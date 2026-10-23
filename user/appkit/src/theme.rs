//! The SPEC colour tokens, light and "Chocolate milk" (dark).

use alloc::string::String;
use gui::{Color, rgb};
use rt::sys;

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    pub milk: Color,
    pub cream: Color,
    pub oat: Color,
    pub sand: Color,
    pub latte: Color,
    /// Secondary text.
    pub cocoa: Color,
    /// Primary text.
    pub espresso: Color,
    pub straw: Color,
    pub honey: Color,
    pub matcha: Color,
    pub blue: Color,
    pub lav: Color,
    /// Fixed in both themes: glyphs on accent fills (#3A2E27).
    pub ink: Color,
    /// Danger button fill, and the "slowest" red.
    pub danger: Color,
    /// Text on the danger fill (fixed #FFFDF8).
    pub on_danger: Color,
    /// "Fastest" green on milk.
    pub good: Color,
    /// Link text (always underlined; SPEC "Type"), and while hovered or pressed. Not the
    /// accent blue, which is for fills and meters only.
    pub link: Color,
    pub link_pressed: Color,
}

impl Theme {
    pub fn light() -> Theme {
        Theme {
            dark: false,
            milk: gui::MILK,
            cream: gui::CREAM,
            oat: gui::OAT,
            sand: gui::SAND,
            latte: gui::LATTE,
            cocoa: gui::COCOA,
            espresso: gui::ESPRESSO,
            link: rgb(0x4F, 0x64, 0xA0),
            link_pressed: rgb(0x3F, 0x52, 0x8A),
            ..Theme::fixed()
        }
    }

    pub fn dark() -> Theme {
        Theme {
            dark: true,
            milk: rgb(0x2E, 0x25, 0x20),
            cream: rgb(0x38, 0x2D, 0x27),
            oat: rgb(0x42, 0x35, 0x2D),
            sand: rgb(0x55, 0x46, 0x3C),
            latte: rgb(0x8C, 0x77, 0x64),
            cocoa: rgb(0xBE, 0xA8, 0x90),
            espresso: rgb(0xF4, 0xEB, 0xE0),
            // Green on dark milk needs the plain accent to keep its contrast.
            good: gui::MATCHA,
            link: rgb(0xA9, 0xB8, 0xE8),
            link_pressed: rgb(0xC3, 0xCD, 0xF0),
            ..Theme::fixed()
        }
    }

    fn fixed() -> Theme {
        Theme {
            dark: false,
            milk: 0,
            cream: 0,
            oat: 0,
            sand: 0,
            latte: 0,
            cocoa: 0,
            espresso: 0,
            straw: gui::STRAW,
            honey: gui::HONEY,
            matcha: gui::MATCHA,
            blue: gui::BLUE,
            lav: gui::LAV,
            ink: rgb(0x3A, 0x2E, 0x27),
            danger: rgb(0xB5, 0x47, 0x3B),
            on_danger: rgb(0xFF, 0xFD, 0xF8),
            good: rgb(0x42, 0x70, 0x4B),
            link: 0,
            link_pressed: 0,
        }
    }

    /// The light or the dark theme (an appearance event says which).
    pub fn for_dark(dark: bool) -> Theme {
        if dark { Theme::dark() } else { Theme::light() }
    }

    /// The theme the desktop is using (`/dev/wm` status, else the settings file).
    pub fn current() -> Theme {
        let status = read("/dev/wm");
        let dark = match status.lines().find_map(|l| l.strip_prefix("theme ")) {
            Some(t) => matches!(t.trim(), "dark" | "chocolate"),
            None => read("/Users/milk/.config/milk/settings").lines().any(|l| {
                let (k, v) = l.split_once('=').unwrap_or((l, ""));
                k.trim() == "theme" && matches!(v.trim(), "chocolate" | "dark")
            }),
        };
        if dark { Theme::dark() } else { Theme::light() }
    }

    /// A calendar colour name to its accent.
    pub fn accent(&self, name: &str) -> Color {
        match name {
            "matcha" => self.matcha,
            "blue" => self.blue,
            "honey" => self.honey,
            "lav" => self.lav,
            _ => self.straw,
        }
    }

    /// A calendar or list colour for thin shapes (a checkbox ring, a chart line): the darker "ink"
    /// versions on the light theme (3:1 on milk), the plain accents on the dark one.
    pub fn accent_ink(&self, name: &str) -> Color {
        if self.dark {
            return self.accent(name);
        }
        match name {
            "matcha" => rgb(0x5B, 0x8A, 0x5E),
            "blue" => rgb(0x4B, 0x63, 0xA8),
            "honey" => rgb(0xA8, 0x78, 0x12),
            "lav" => rgb(0x7E, 0x6A, 0xAE),
            _ => rgb(0xC9, 0x56, 0x6B),
        }
    }

    /// `c` at `alpha`/255 over `under` (a tint).
    pub fn tint(under: Color, c: Color, alpha: u8) -> Color {
        gui::blend(under, c, alpha)
    }
}

fn read(p: &str) -> String {
    sys::read_file(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}
