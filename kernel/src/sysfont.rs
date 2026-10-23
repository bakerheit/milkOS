//! The desktop's system font: proportional, anti-aliased UI text from real typefaces.
//!
//! The glyphs are baked at build time by `scripts/gen_sysfont.py` (FreeType's hinted grey
//! coverage, 4 bits a pixel, cropped to the ink) into `sysfont/<family>.font`, indexed by the
//! generated `sysfont_data.rs`; the blob format, the face choice, the fallback between families
//! and the measuring live in the shared `fontkit` crate (`user/fontkit`), which user space uses
//! too. Five families (Figtree, IBM Plex Sans, Fira Sans and Atkinson Hyperlegible Next under
//! the SIL OFL 1.1, Ubuntu Sans under the Ubuntu Font Licence 1.0; see `assets/fonts/`) and a
//! drawn symbol set, each at the sizes the UI uses:
//!
//! | scale | em     | line box | baseline | weights                 | characters                |
//! |-------|--------|----------|----------|-------------------------|---------------------------|
//! | 1     | 13 px  | 16 px    | 13       | Regular, Medium, Bold   | Latin-1 and the font's    |
//! | 2     | 26 px  | 32 px    | 26       | Regular, Bold           | Latin Ext., Greek, ...    |
//! | 4     | 52 px  | 64 px    | 52       | Bold                    | letters, digits, :        |
//! | 8     | 104 px | 128 px   | 104      | Regular                 | digits, :                 |
//!
//! A character the chosen family lacks comes from the others (`fontkit::FALLBACK`), then the
//! drawn symbols, then a fold to similar characters, then a visible box.
//!
//! Text is laid out in line boxes `16 * scale` tall, the same cells the 8x16 Unifont text used,
//! so a caller's `y` is still the top of the line. Anything not baked (another scale, a weight
//! or character a big face doesn't have) is drawn from the nearest smaller face, magnified.
//!
//! Which family the UI uses is a desktop preference (`font = figtree` in the settings file,
//! Settings > Appearance); [`set_current`] switches it and the desktop redraws everything.
//! `/dev/wm` reports it (`font figtree`) so apps can follow. Grids (the console, fbcon,
//! Terminal, Notes' editor) stay on monospace Unifont (`font`). The boot splash runs before the
//! settings are read, so it always uses the default family; the "milkOS" wordmark is always
//! drawn in [`BRAND`]. [`install`] puts the blobs in `/System/Fonts/<key>.font` (no copy: the
//! files point at the kernel's own bytes) for user space.

#![allow(dead_code)] // A text API: not every measure is used by every caller yet.

use alloc::string::String;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::gfx::{Color, DrawTarget};
pub use crate::sysfont_data::{FAMILIES, SYMBOLS};
pub use fontkit::Weight;

/// A glyph: its bitmap (as coverage spans) and metrics.
pub type Glyph = fontkit::Glyph<'static>;

pub struct Family {
    /// The settings-file value (`font = figtree`).
    pub key: &'static str,
    pub name: &'static str,
    /// Its copyright line (credits).
    pub copyright: &'static str,
    /// Its licence ("SIL Open Font License 1.1", "Ubuntu Font Licence 1.0").
    pub licence: &'static str,
    /// The baked glyphs.
    pub font: fontkit::Family<'static>,
}

// The kernel's families are fontkit's, in the same order.
const _: () = {
    assert!(FAMILIES.len() == fontkit::FAMILY_COUNT);
};

/// Figtree: the default, and the family of the "milkOS" wordmark.
pub const DEFAULT: usize = fontkit::DEFAULT;
pub const BRAND: usize = 0;

static CURRENT: AtomicUsize = AtomicUsize::new(DEFAULT);

/// The family the UI draws in now.
pub fn current() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

pub fn set_current(fam: usize) {
    CURRENT.store(fam.min(FAMILIES.len() - 1), Ordering::Relaxed);
}

/// The family whose key is `key`.
pub fn find(key: &str) -> Option<usize> {
    FAMILIES.iter().position(|f| f.key == key)
}

/// fontkit's view of the baked families.
struct Baked;

impl fontkit::Source<'static> for Baked {
    fn family(&self, i: usize) -> Option<&'static fontkit::Family<'static>> {
        match i {
            fontkit::SYMBOLS => Some(&SYMBOLS),
            _ => FAMILIES.get(i).map(|f| &f.font),
        }
    }
}

/// A text style: family, scale (line boxes of `16 * scale` pixels) and weight. All UI text
/// goes through one of these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Font {
    pub family: usize,
    pub scale: i32,
    pub weight: Weight,
}

impl Font {
    /// The current UI family.
    pub fn ui(scale: i32, weight: Weight) -> Font {
        Font::of(current(), scale, weight)
    }

    pub fn of(family: usize, scale: i32, weight: Weight) -> Font {
        Font {
            family: family.min(FAMILIES.len() - 1),
            scale: scale.max(1),
            weight,
        }
    }

    /// Body text (scale 1), regular or bold.
    pub fn body(bold: bool) -> Font {
        Font::ui(1, Weight::bold(bold))
    }

    fn kit(&self) -> fontkit::Font {
        fontkit::Font::new(self.family, self.scale, self.weight)
    }

    /// Height of a line box.
    pub fn line_height(&self) -> i32 {
        16 * self.scale
    }

    /// Baseline, from the top of the line box.
    pub fn baseline(&self) -> i32 {
        13 * self.scale
    }

    /// Pixels the text sits above the baseline (cap height, roughly 0.7 em).
    pub fn cap_height(&self) -> i32 {
        (13 * self.scale * 7 + 5) / 10
    }

    /// The line-box top that centres capitals (and digits) in `[y, y + h)`.
    pub fn center_caps(&self, y: i32, h: i32) -> i32 {
        let cap = self.cap_height();
        y + (h - cap) / 2 - (self.baseline() - cap)
    }

    pub fn advance(&self, c: char) -> i32 {
        self.kit().advance(&Baked, c)
    }

    /// Width of `s` in pixels.
    pub fn width(&self, s: &str) -> i32 {
        self.kit().width(&Baked, s)
    }

    /// How many bytes of `s` fit in `max` pixels (a char boundary).
    pub fn fit(&self, s: &str, max: i32) -> usize {
        self.kit().fit(&Baked, s, max)
    }

    /// `s` cut to `max` pixels with "..." when it doesn't fit.
    pub fn truncate(&self, s: &str, max: i32) -> String {
        self.kit().truncate(&Baked, s, max)
    }

    /// The longest end of `s` that fits in `max` pixels (a text field scrolled to its caret).
    pub fn tail<'s>(&self, s: &'s str, max: i32) -> &'s str {
        self.kit().tail(&Baked, s, max)
    }

    /// The char index in `s` nearest pixel offset `x` (caret placement).
    pub fn index_at(&self, s: &str, x: i32) -> usize {
        self.kit().index_at(&Baked, s, x)
    }

    /// Width of the first `n` chars of `s` (where a caret after them goes).
    pub fn prefix_width(&self, s: &str, n: usize) -> i32 {
        self.kit().prefix_width(&Baked, s, n)
    }

    /// Call `f(pen x, glyph, magnification)` for each glyph of `s` starting at pen `x`;
    /// returns the pen after the last one.
    pub fn layout(&self, x: i32, s: &str, mut f: impl FnMut(i32, &Glyph, i32)) -> i32 {
        self.kit().layout(&Baked, x, s, |pen, g, m| f(pen, g, m))
    }

    /// Draw `s` with the top of its line box at `y` straight onto `t` (the boot splash; the
    /// desktop draws through its painter). Returns the end x.
    pub fn draw(&self, t: &mut dyn DrawTarget, x: i32, y: i32, s: &str, c: Color) -> i32 {
        let base = y + self.baseline();
        self.layout(x, s, |pen, g, mag| {
            let (gx, gy) = (pen + g.left * mag, base - g.top * mag);
            g.spans(mag, |sx, sy, len, a| {
                if a == 255 {
                    t.fill_rect(gx + sx, gy + sy, len, 1, c);
                } else {
                    t.fill_rect_alpha(gx + sx, gy + sy, len, 1, c, a);
                }
            });
        })
    }
}

/// Width of `s` in the UI's body face (scale 1).
pub fn width(s: &str, bold: bool) -> i32 {
    Font::body(bold).width(s)
}

/// Put every blob in `/System/Fonts/<key>.font`, read-only, for user space (appkit's text).
/// The files are the kernel's own bytes, not copies. After the initramfs is unpacked.
pub fn install() {
    let _ = crate::fs::mkdir("/System/Fonts", 0o755);
    let mut n = 0;
    let blobs = FAMILIES
        .iter()
        .map(|f| f.font.blob())
        .chain([SYMBOLS.blob()]);
    for (key, blob) in fontkit::KEYS.iter().zip(blobs) {
        let path = alloc::format!("/System/Fonts/{key}.font");
        match crate::fs::initramfs::put_file(&path, blob) {
            Ok(()) => n += 1,
            Err(e) => crate::kprintln!("fonts: {path}: {e}"),
        }
    }
    crate::kprintln!("fonts: {n} font files in /System/Fonts");
}

/// Boot self-test: every blob validates, each Latin-1 character resolves at every scale and
/// weight from its own family, other scripts resolve to something, and text measures sensibly.
pub fn selftest() -> bool {
    for fam in FAMILIES.iter().map(|f| &f.font).chain([&SYMBOLS]) {
        if fam.validate().is_err() {
            return false;
        }
    }
    for (i, fam) in FAMILIES.iter().enumerate() {
        if fam.font.key() != fam.key || fam.font.faces().len() != 7 {
            return false;
        }
        for scale in [1, 2, 4, 8] {
            for weight in [Weight::Regular, Weight::Medium, Weight::Bold] {
                let f = Font::of(i, scale, weight);
                for cp in (0x20u32..0x7F).chain(0xA0..0x100).chain([0xFFFD]) {
                    let c = char::from_u32(cp).unwrap_or('?');
                    if fam.font.find(cp, scale, weight).is_none()
                        || (f.advance(c) <= 0 && c != '\u{AD}')
                    {
                        return false;
                    }
                }
                for c in ['Ω', 'Ж', 'ő', '€', '→', '★', '中'] {
                    if f.advance(c) <= 0 {
                        return false;
                    }
                }
                let w = f.width("milkOS 12:45");
                if w < 40 * scale || w > 160 * scale {
                    return false;
                }
            }
        }
    }
    true
}
