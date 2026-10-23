//! Page text in the system fonts (gui::text, the desktop's families): browserkit's [`Text`]
//! backend. CSS generic families map onto the five: sans-serif and system-ui are the family
//! Settings chose (so pages follow the Settings font, like the chrome), serif is IBM Plex Sans
//! (no serif face is baked; Plex is the most bookish), cursive Atkinson Hyperlegible Next,
//! fantasy Ubuntu Sans. Monospace never comes here (browserkit draws it in Unifont). Sizes
//! snap to 13, 26 and 52 px; weights of 600 and up are bold, 500 medium; italics are drawn
//! oblique.

use browserkit::engine::{Family, Font, Rect};
use browserkit::font::{Text, snap};
use gui::text::{self, Weight};

/// Family indices in gui::text::NAMES order.
const PLEX: usize = 1;
const ATKINSON: usize = 3;
const UBUNTU: usize = 4;

/// `ui`: the desktop's family, read once per layout or frame (gui::text::current_family).
#[derive(Clone, Copy)]
pub struct SysText {
    pub ui: usize,
}

impl SysText {
    pub fn now() -> SysText {
        SysText {
            ui: text::current_family(),
        }
    }
}

/// The system font for an engine font, with `ui` the desktop's family.
pub fn font_for(f: &Font, ui: usize) -> text::Font {
    let family = match f.family {
        Family::Serif => PLEX,
        Family::Cursive => ATKINSON,
        Family::Fantasy => UBUNTU,
        _ => ui,
    };
    let weight = match f.weight {
        600.. => Weight::Bold,
        500..=599 => Weight::Medium,
        _ => Weight::Regular,
    };
    let g = text::Font::of(family, snap(f.size as i32), weight);
    if f.italic { g.italic() } else { g }
}

impl Text for SysText {
    fn width(&self, f: &Font, s: &str) -> i32 {
        font_for(f, self.ui).width(s)
    }

    fn draw(
        &self,
        px: &mut [u32],
        stride: usize,
        clip: Rect,
        x: i32,
        y: i32,
        f: &Font,
        s: &str,
        c: u32,
    ) -> i32 {
        font_for(f, self.ui).draw(px, stride, (clip.x, clip.y, clip.w, clip.h), x, y, s, c)
    }
}
