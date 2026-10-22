//! The desktop's system font: proportional, anti-aliased UI text from real typefaces.
//!
//! The glyphs are baked at build time by `scripts/gen_sysfont.py` (FreeType's hinted grey
//! coverage, 4 bits a pixel, cropped to the ink) into `sysfont/<family>.bin`, indexed by the
//! generated `sysfont_data.rs`. Five families (Figtree, IBM Plex Sans, Fira Sans and Atkinson
//! Hyperlegible Next under the SIL OFL 1.1, Ubuntu Sans under the Ubuntu Font Licence 1.0; see
//! `assets/fonts/`), each at the sizes the UI uses:
//!
//! | scale | em     | line box | baseline | weights                 | characters          |
//! |-------|--------|----------|----------|-------------------------|---------------------|
//! | 1     | 13 px  | 16 px    | 13       | Regular, Medium, Bold   | Latin-1 + U+FFFD    |
//! | 2     | 26 px  | 32 px    | 26       | Regular, Bold           | Latin-1 + U+FFFD    |
//! | 4     | 52 px  | 64 px    | 52       | Bold                    | letters, digits, :  |
//! | 8     | 104 px | 128 px   | 104      | Regular                 | digits, :           |
//!
//! Text is laid out in line boxes `16 * scale` tall, the same cells the 8x16 Unifont text used,
//! so a caller's `y` is still the top of the line. Anything not baked (another scale, a weight
//! or character a big face doesn't have) is drawn from the nearest smaller face, magnified.
//!
//! Which family the UI uses is a desktop preference (`font = figtree` in the settings file,
//! Settings > Appearance); [`set_current`] switches it and the desktop redraws everything.
//! Grids (the console, fbcon, Terminal, Notes' editor) stay on monospace Unifont (`font`).
//! The boot splash runs before the settings are read, so it always uses the default family;
//! the "milkOS" wordmark is always drawn in [`BRAND`].

#![allow(dead_code)] // A text API: not every measure is used by every caller yet.

use alloc::string::String;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::gfx::{Color, DrawTarget};
pub use crate::sysfont_data::FAMILIES;

/// One baked size and weight of a family (see the module table).
pub struct Face {
    pub size: u8,
    pub line: u8,
    pub ascent: u8,
    pub weight: u16,
    /// Its glyph records: `first .. first + count` in the family blob.
    pub first: u16,
    pub count: u16,
}

pub struct Family {
    /// The settings-file value (`font = figtree`).
    pub key: &'static str,
    pub name: &'static str,
    /// Its copyright line (credits).
    pub copyright: &'static str,
    /// Its licence ("SIL Open Font License 1.1", "Ubuntu Font Licence 1.0").
    pub licence: &'static str,
    /// Glyph records (12 bytes each) then coverage data, see `scripts/gen_sysfont.py`.
    pub blob: &'static [u8],
    pub faces: &'static [Face],
}

/// Face indices in every family (the generator's `FACES` order).
const BODY_R: usize = 0;
const BODY_M: usize = 1;
const BODY_B: usize = 2;
const HEAD_R: usize = 3;
const HEAD_B: usize = 4;
const BIG_B: usize = 5;
const HUGE_R: usize = 6;

/// Figtree: the default, and the family of the "milkOS" wordmark.
pub const DEFAULT: usize = 0;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weight {
    Regular,
    Medium,
    Bold,
}

impl Weight {
    pub fn bold(b: bool) -> Weight {
        if b { Weight::Bold } else { Weight::Regular }
    }
}

/// One glyph's bitmap and metrics, in the face's pixels (before magnification).
#[derive(Clone, Copy)]
pub struct Glyph {
    pub w: i32,
    pub h: i32,
    /// Left edge relative to the pen.
    pub left: i32,
    /// Top edge in pixels above the baseline.
    pub top: i32,
    pub adv: i32,
    data: &'static [u8],
    rle: bool,
}

impl Glyph {
    /// Call `f(x, y, len, coverage 0..=255)` for every run of non-zero coverage, row by row
    /// (glyph-local coordinates, `mag` times magnified).
    pub fn spans(&self, mag: i32, mut f: impl FnMut(i32, i32, i32, u8)) {
        let (w, n) = (self.w, self.w * self.h);
        let mut i = 0; // pixel index
        let emit = |i: i32, len: i32, v: u8, f: &mut dyn FnMut(i32, i32, i32, u8)| {
            if v == 0 {
                return;
            }
            let a = v * 17;
            let (mut i, mut len) = (i, len);
            while len > 0 {
                let (x, y) = (i % w, i / w);
                let l = len.min(w - x);
                for r in 0..mag {
                    f(x * mag, y * mag + r, l * mag, a);
                }
                i += l;
                len -= l;
            }
        };
        if self.rle {
            for &b in self.data {
                if i >= n {
                    break;
                }
                let len = (b & 15) as i32 + 1;
                emit(i, len.min(n - i), b >> 4, &mut f);
                i += len;
            }
        } else {
            // Packed nibbles: merge equal neighbours into runs.
            let mut run_start = 0;
            let mut run_v = 0u8;
            for &b in self.data.iter().take((n as usize).div_ceil(2)) {
                for v in [b >> 4, b & 15] {
                    if i >= n {
                        break;
                    }
                    if v != run_v {
                        emit(run_start, i - run_start, run_v, &mut f);
                        run_start = i;
                        run_v = v;
                    }
                    i += 1;
                }
            }
            emit(run_start, i - run_start, run_v, &mut f);
        }
    }
}

fn face_glyph(fam: &'static Family, face: usize, cp: u32) -> Option<Glyph> {
    let fc = &fam.faces[face];
    let rec = |i: usize| -> &[u8] { &fam.blob[12 * i..12 * i + 12] };
    let (mut lo, mut hi) = (fc.first as usize, fc.first as usize + fc.count as usize);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let r = rec(mid);
        let c = u16::from_le_bytes([r[0], r[1]]) as u32;
        if c == cp {
            let off = u32::from_le_bytes([r[2], r[3], r[4], r[5]]);
            let (w, h) = (r[6] as i32, r[7] as i32);
            let rle = off & 1 << 31 != 0;
            let start = (off & !(1 << 31)) as usize;
            return Some(Glyph {
                w,
                h,
                left: r[8] as i8 as i32,
                top: r[9] as i8 as i32,
                adv: r[10] as i32,
                // The decoder stops after w * h pixels.
                data: &fam.blob[start..],
                rle,
            });
        }
        if c < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
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

    fn fam(&self) -> &'static Family {
        &FAMILIES[self.family]
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

    /// The face that draws scale `s` best, and how much it must be magnified.
    fn face_for(&self, s: i32) -> (usize, i32) {
        let bold = self.weight != Weight::Regular;
        match s {
            1 => (
                match self.weight {
                    Weight::Regular => BODY_R,
                    Weight::Medium => BODY_M,
                    Weight::Bold => BODY_B,
                },
                1,
            ),
            2 | 3 => (if bold { HEAD_B } else { HEAD_R }, 1),
            4..=7 if bold => (BIG_B, 1),
            4..=7 => (HEAD_R, 2),
            8.. if !bold => (HUGE_R, s / 8),
            8.. => (BIG_B, s / 4),
            _ => (BODY_R, 1),
        }
    }

    /// The glyph for `c` and its magnification: from the best face, else the next smaller one
    /// (a big face only has some characters), else U+FFFD.
    pub fn glyph(&self, c: char) -> (Glyph, i32) {
        let fam = self.fam();
        let mut s = self.scale;
        loop {
            let (face, mag) = self.face_for(s);
            let base = s / mag.max(1);
            if let Some(g) = face_glyph(fam, face, c as u32) {
                return (g, self.scale / base.max(1));
            }
            if s <= 3 {
                if let Some(g) = face_glyph(fam, face, 0xFFFD) {
                    return (g, self.scale / base.max(1));
                }
                // Unreachable with a complete bake; keep going with the body face.
                let g = face_glyph(fam, BODY_R, 0xFFFD).or_else(|| face_glyph(fam, BODY_R, 32));
                return (g.expect("sysfont: no replacement glyph"), self.scale);
            }
            s /= 2;
        }
    }

    pub fn advance(&self, c: char) -> i32 {
        let (g, mag) = self.glyph(c);
        g.adv * mag
    }

    /// Width of `s` in pixels.
    pub fn width(&self, s: &str) -> i32 {
        s.chars().map(|c| self.advance(c)).sum()
    }

    /// How many bytes of `s` fit in `max` pixels (a char boundary).
    pub fn fit(&self, s: &str, max: i32) -> usize {
        let mut w = 0;
        for (i, c) in s.char_indices() {
            w += self.advance(c);
            if w > max {
                return i;
            }
        }
        s.len()
    }

    /// `s` cut to `max` pixels with "..." when it doesn't fit.
    pub fn truncate(&self, s: &str, max: i32) -> String {
        if self.width(s) <= max {
            return String::from(s);
        }
        let dots = self.width("...");
        let n = self.fit(s, (max - dots).max(0));
        let mut out = String::from(s[..n].trim_end());
        out.push_str("...");
        out
    }

    /// The longest end of `s` that fits in `max` pixels (a text field scrolled to its caret).
    pub fn tail<'s>(&self, s: &'s str, max: i32) -> &'s str {
        let mut w = 0;
        for (i, c) in s.char_indices().rev() {
            w += self.advance(c);
            if w > max {
                return &s[i + c.len_utf8()..];
            }
        }
        s
    }

    /// The char index in `s` nearest pixel offset `x` (caret placement).
    pub fn index_at(&self, s: &str, x: i32) -> usize {
        let mut w = 0;
        for (i, c) in s.chars().enumerate() {
            let a = self.advance(c);
            if x < w + a / 2 {
                return i;
            }
            w += a;
        }
        s.chars().count()
    }

    /// Width of the first `n` chars of `s` (where a caret after them goes).
    pub fn prefix_width(&self, s: &str, n: usize) -> i32 {
        s.chars().take(n).map(|c| self.advance(c)).sum()
    }

    /// Call `f(pen x, glyph, magnification)` for each char of `s` starting at pen `x`;
    /// returns the pen after the last one.
    pub fn layout(&self, x: i32, s: &str, mut f: impl FnMut(i32, &Glyph, i32)) -> i32 {
        let mut pen = x;
        for c in s.chars() {
            let (g, mag) = self.glyph(c);
            if g.w > 0 {
                f(pen, &g, mag);
            }
            pen += g.adv * mag;
        }
        pen
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

/// Boot self-test: every family has every face, every baked glyph decodes inside its box,
/// each Latin-1 character resolves at every scale and weight, and text measures sensibly.
pub fn selftest() -> bool {
    for (i, fam) in FAMILIES.iter().enumerate() {
        if fam.faces.len() != 7 {
            return false;
        }
        for fc in fam.faces {
            for r in fc.first as usize..fc.first as usize + fc.count as usize {
                let rec = &fam.blob[12 * r..12 * r + 12];
                let cp = u16::from_le_bytes([rec[0], rec[1]]) as u32;
                let face = fam
                    .faces
                    .iter()
                    .position(|f| core::ptr::eq(f, fc))
                    .unwrap_or(0);
                let Some(g) = face_glyph(fam, face, cp) else {
                    return false;
                };
                let mut ok = true;
                let mut px = 0;
                g.spans(1, |x, y, len, _| {
                    ok &= x >= 0 && y >= 0 && len > 0 && x + len <= g.w && y < g.h;
                    px += len;
                });
                if !ok || px > g.w * g.h || g.adv <= 0 && cp != 0xAD {
                    return false;
                }
            }
        }
        for scale in [1, 2, 4, 8] {
            for weight in [Weight::Regular, Weight::Medium, Weight::Bold] {
                let f = Font::of(i, scale, weight);
                for cp in (0x20u32..0x7F).chain(0xA0..0x100).chain([0xFFFD]) {
                    let c = char::from_u32(cp).unwrap_or('?');
                    if f.advance(c) <= 0 && c != '\u{AD}' {
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
