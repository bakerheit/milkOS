//! Smooth large text from the Unifont bitmaps (no new font files): a glyph is enlarged with
//! EPX / Scale2x (which rounds off stair-steps on diagonals and curves) to twice the target
//! size, then box-filtered down 2x2 into an anti-aliased coverage map. Used for 2x headings and
//! the 4x clock; 1x UI text stays crisp Unifont. Faux bold is applied at the source resolution
//! (each row OR-ed with itself shifted one pixel right) before smoothing, so bold stays clean.
//!
//! Maps are built on first use and cached forever (a few KiB per glyph and size).
//!
//! Shared by the boot splash (which runs before the desktop, drawing straight onto the
//! framebuffer with [`draw_smooth`] / [`draw_prop`]) and the desktop's painter, so the
//! "milkOS" wordmark looks the same on the splash, the lock screen and the desktop. It
//! needs the heap (built in `mem::init`, before the splash).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use crate::font;
use crate::gfx::{Color, DrawTarget};
use crate::sync::SpinLock;

/// Source bitmap width: 8 plus one column for faux bold.
const SRC_W: usize = 9;
const SRC_H: usize = 16;

/// (code point, scale, bold) -> coverage map.
type Key = (u32, u8, bool);
static CACHE: SpinLock<BTreeMap<Key, &'static [u8]>> = SpinLock::new(BTreeMap::new());

/// One Scale2x step on a `w` x `h` bitmap.
fn scale2x(src: &[bool], w: usize, h: usize) -> Vec<bool> {
    let at = |x: isize, y: isize| -> bool {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            false
        } else {
            src[y as usize * w + x as usize]
        }
    };
    let mut out = vec![false; 4 * w * h];
    let ow = 2 * w;
    for y in 0..h as isize {
        for x in 0..w as isize {
            let p = at(x, y);
            let (a, b, c, d) = (at(x, y - 1), at(x + 1, y), at(x - 1, y), at(x, y + 1));
            let (mut e0, mut e1, mut e2, mut e3) = (p, p, p, p);
            if c == a && c != d && a != b {
                e0 = a;
            }
            if a == b && a != c && b != d {
                e1 = b;
            }
            if d == c && d != b && c != a {
                e2 = c;
            }
            if b == d && b != a && d != c {
                e3 = d;
            }
            let (ox, oy) = (2 * x as usize, 2 * y as usize);
            out[oy * ow + ox] = e0;
            out[oy * ow + ox + 1] = e1;
            out[(oy + 1) * ow + ox] = e2;
            out[(oy + 1) * ow + ox + 1] = e3;
        }
    }
    out
}

fn build(c: char, scale: usize, bold: bool) -> Vec<u8> {
    // Unifont's zero is slashed, which reads oddly at display sizes (the 4x clock): use the
    // round capital O there.
    let g = font::glyph(if c == '0' && scale >= 4 { 'O' } else { c });
    let mut bm = vec![false; SRC_W * SRC_H];
    for (row, &bits) in g.iter().enumerate() {
        let bits = (bits as u16) << 1; // 9 columns: bit 8 is the leftmost
        let bits = if bold { bits | bits >> 1 } else { bits };
        for col in 0..SRC_W {
            bm[row * SRC_W + col] = bits & (0x100 >> col) != 0;
        }
    }
    // Up to 2 * scale with Scale2x, then 2x2 box filter down to `scale`.
    let (mut w, mut h) = (SRC_W, SRC_H);
    let mut k = 1;
    while k < 2 * scale {
        bm = scale2x(&bm, w, h);
        w *= 2;
        h *= 2;
        k *= 2;
    }
    let (tw, th) = (w / 2, h / 2);
    let mut out = vec![0u8; tw * th];
    for y in 0..th {
        for x in 0..tw {
            let n = [(0, 0), (1, 0), (0, 1), (1, 1)]
                .iter()
                .filter(|(dx, dy)| bm[(2 * y + dy) * w + 2 * x + dx])
                .count();
            out[y * tw + x] = (n * 255 / 4) as u8;
        }
    }
    out
}

/// Coverage map of `c` at `scale` (2 or 4): `9 * scale` x `16 * scale` bytes, row-major.
pub fn glyph(c: char, scale: i32, bold: bool) -> &'static [u8] {
    let key = (c as u32, scale as u8, bold);
    if let Some(m) = CACHE.lock().get(&key) {
        return m;
    }
    // Build outside the lock (it allocates a few hundred KiB of temporaries at 4x).
    let m: &'static [u8] = Box::leak(build(c, scale as usize, bold).into_boxed_slice());
    CACHE.lock().insert(key, m);
    m
}

pub const fn width(scale: i32) -> i32 {
    SRC_W as i32 * scale
}

/// Sizes that get the smooth treatment (powers of two: Scale2x steps).
pub fn smooth_scale(scale: i32) -> bool {
    scale == 2 || scale == 4 || scale == 8
}

// ---- proportional 1x ------------------------------------------------------------------------

/// The shell's chrome text (menu bar, menus, titles, toasts, tips, Skim, control centre) uses
/// the proportional face below. `false` puts it back to plain monospace Unifont everywhere.
pub const PROPORTIONAL_UI: bool = true;

/// Proportional metrics for a Unifont glyph: (first used column, advance in pixels).
/// Unifont is monospace; trimming each glyph to its ink plus one pixel of spacing gives a
/// friendlier UI face from the same bitmaps.
pub fn prop_metrics(c: char) -> (i32, i32) {
    if !PROPORTIONAL_UI {
        return (0, 8);
    }
    if c == ' ' {
        return (0, 5);
    }
    let g = font::glyph(c);
    let ink = g.iter().fold(0u8, |a, &b| a | b);
    if ink == 0 {
        return (0, 4);
    }
    let first = ink.leading_zeros() as i32;
    let last = 7 - ink.trailing_zeros() as i32;
    (first, last - first + 2)
}

/// Width of `s` in the proportional face (+1 per glyph when bold).
pub fn prop_width(s: &str, bold: bool) -> i32 {
    let w: i32 = s.chars().map(|c| prop_metrics(c).1).sum();
    let n = s.chars().count() as i32;
    if !PROPORTIONAL_UI {
        return w + bold as i32;
    }
    w + if bold { n } else { 0 } - if n > 0 { 1 } else { 0 }
}

// ---- drawing on any target (the boot splash) -----------------------------------------------

/// Smooth scaled text (2x, 4x, 8x; other sizes pixel-doubled) straight onto `t`, optionally
/// faux bold. Returns the end x. The same glyphs the desktop's painter draws.
#[allow(clippy::too_many_arguments)]
pub fn draw_smooth(
    t: &mut dyn DrawTarget,
    x: i32,
    y: i32,
    s: &str,
    c: Color,
    scale: i32,
    bold: bool,
) -> i32 {
    if !smooth_scale(scale) {
        let end = font::draw_str(t, x, y, s, c, None, scale);
        if bold {
            font::draw_str(t, x + scale, y, s, c, None, scale);
        }
        return end;
    }
    let (gw, gh) = (width(scale), font::GLYPH_H as i32 * scale);
    let mut cx = x;
    for ch in s.chars() {
        if ch != ' ' {
            let m = glyph(ch, scale, bold);
            for gy in 0..gh {
                for gx in 0..gw {
                    let a = m[(gy * gw + gx) as usize];
                    if a != 0 {
                        t.blend_pixel(cx + gx, y + gy, c, a);
                    }
                }
            }
        }
        cx += font::GLYPH_W as i32 * scale;
    }
    cx
}

/// Proportional 1x text ([`prop_metrics`]) onto `t`. Returns the end x.
pub fn draw_prop(t: &mut dyn DrawTarget, x: i32, y: i32, s: &str, c: Color, bold: bool) -> i32 {
    let mut cx = x;
    for ch in s.chars() {
        let (first, adv) = prop_metrics(ch);
        if ch != ' ' {
            let mut b = [0u8; 4];
            let g = ch.encode_utf8(&mut b);
            font::draw_str(t, cx - first, y, g, c, None, 1);
            if bold {
                font::draw_str(t, cx - first + 1, y, g, c, None, 1);
            }
        }
        cx += adv + (bold && PROPORTIONAL_UI) as i32;
    }
    cx - PROPORTIONAL_UI as i32
}
