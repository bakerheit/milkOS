//! App and file icons drawn from primitives (ported from the mockup's `icon()` in scenes.mjs):
//! a rounded tile (radius 24% of its size) plus a few shapes, laid out on a 48-unit grid.

#![allow(clippy::too_many_arguments)]

use crate::gfx::Color;

use super::paint::Painter;
use super::theme::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Folder,
    Term,
    Globe,
    Note,
    Music,
    Photo,
    Cal,
    Gear,
    Doc,
    Clock,
    Spaces,
    Paint,
    Calc,
    /// An app action in Skim: a little play triangle.
    Action,
}

/// Draw `icon` as an `s` x `s` tile at (x, y), tile colour `bg` (blended at `bg_a`), glyph `fg`.
/// `border` adds the 1px sand edge the mockup uses on light tiles.
pub fn draw(
    p: &mut Painter,
    x: i32,
    y: i32,
    s: i32,
    icon: Icon,
    bg: Color,
    bg_a: u8,
    fg: Color,
    border: bool,
) {
    let r = (s * 24 + 50) / 100;
    p.rrect(x, y, s, s, r, bg, bg_a);
    if border {
        p.rrect_stroke(x, y, s, s, r, 1, SAND, 255);
    }
    // Grid units (48 per tile) -> pixels.
    let u = |v: i32| (v * s + 24) / 48;
    let th = |v: i32| ((v * s + 24) / 48).max(1);
    let rect = |p: &mut Painter, a: i32, b: i32, w: i32, h: i32, rad: i32, c: Color| {
        p.rrect(x + u(a), y + u(b), u(w).max(1), u(h).max(1), rad, c, 255);
    };
    let ring = |p: &mut Painter, a: i32, b: i32, w: i32, h: i32, rad: i32, t: i32, c: Color| {
        p.rrect_stroke(x + u(a), y + u(b), u(w), u(h), rad, t, c, 255);
    };
    match icon {
        Icon::Folder => {
            rect(p, 9, 14, 14, 7, u(3), fg);
            rect(p, 9, 19, 30, 18, u(3), fg);
        }
        Icon::Term => {
            // ">_" like the mockup's 18px bold glyphs: a heavy chevron and a thick underscore.
            let sc = if s >= 72 { 2 } else { 1 };
            let tx = x + u(8);
            let ty = y + (s - 16 * sc) / 2 - u(1);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                p.text_a(tx + dx * sc, ty + dy * sc, ">", fg, 255, sc);
            }
            p.fill(tx + 8 * sc + u(2), ty + 13 * sc, u(11), th(3), fg);
        }
        Icon::Gear => {
            ring(p, 11, 11, 26, 26, u(13), th(5), fg);
            rect(p, 20, 20, 8, 8, u(4), fg);
        }
        Icon::Note => {
            ring(p, 12, 11, 24, 26, u(3), th(2), fg);
            rect(p, 17, 18, 14, 2, 0, fg);
            rect(p, 17, 24, 14, 2, 0, fg);
            rect(p, 17, 30, 9, 2, 0, fg);
        }
        Icon::Globe => {
            ring(p, 10, 10, 28, 28, u(14), th(3), fg);
            ring(p, 21, 10, 6, 28, u(3), th(2), fg);
            rect(p, 10, 23, 28, 2, 0, fg);
        }
        Icon::Music => {
            rect(p, 14, 28, 11, 9, u(5), fg);
            rect(p, 23, 11, 3, 23, 0, fg);
            rect(p, 23, 11, 13, 5, u(2), fg);
        }
        Icon::Paint => {
            // A milk palette with three paint dots and a thumb hole.
            p.circle(x + u(9), y + u(10), u(30), MILK, 255);
            p.circle(x + u(27), y + u(26), u(9), ESPRESSO, 0x30);
            rect(p, 15, 17, 7, 7, u(4), STRAW);
            rect(p, 24, 14, 7, 7, u(4), BLUE);
            rect(p, 15, 27, 7, 7, u(4), MATCHA);
            p.circle(x + u(27), y + u(26), u(7), HONEY, 255);
        }
        Icon::Action => {
            // A right-pointing triangle, one span per row.
            let (top, h) = (u(13), u(22));
            for k in 0..h {
                let half = h / 2;
                let wpx = (if k <= half { k } else { h - k }) * u(18) / half.max(1);
                p.fill(x + u(17), y + top + k, wpx.max(1), 1, fg);
            }
        }
        Icon::Calc => {
            rect(p, 11, 11, 11, 11, u(3), fg);
            rect(p, 26, 11, 11, 11, u(3), fg);
            rect(p, 11, 26, 11, 11, u(3), fg);
            rect(p, 26, 26, 11, 11, u(3), STRAW);
        }
        Icon::Clock => {
            ring(p, 10, 10, 28, 28, u(14), th(3), fg);
            rect(p, 23, 16, 3, 9, 0, fg);
            rect(p, 23, 23, 8, 3, 0, fg);
        }
        Icon::Photo => {
            ring(p, 9, 12, 30, 24, u(4), th(3), fg);
            rect(p, 16, 17, 8, 8, u(4), HONEY);
            rect(p, 14, 29, 22, 5, 0, MATCHA);
        }
        Icon::Cal => {
            ring(p, 10, 12, 28, 26, u(4), th(3), fg);
            rect(p, 10, 18, 28, 3, 0, fg);
            rect(p, 17, 26, 5, 5, 0, STRAW);
        }
        Icon::Doc => {
            ring(p, 14, 9, 20, 30, u(3), th(3), fg);
            rect(p, 19, 17, 10, 2, 0, fg);
            rect(p, 19, 23, 10, 2, 0, fg);
        }
        Icon::Spaces => {
            rect(p, 9, 13, 14, 10, u(2), fg);
            rect(p, 25, 13, 14, 10, u(2), fg);
            rect(p, 9, 26, 30, 10, u(2), fg);
        }
    }
}

/// The milk-drop logo (SPEC: menu bar): a milk circle with an espresso ring, `s` across.
pub fn logo(p: &mut Painter, x: i32, y: i32, s: i32, fg: Color) {
    p.circle(x, y, s, MILK, 255);
    p.ring(x, y, s, (s / 6).max(2), fg, 255);
}

/// A proper drop shape (round bottom, pointed top) for the About window, like the boot splash.
/// `r` is the radius of the round part, `d` the distance from its centre up to the tip; (cx, cy)
/// is the centre of the round part.
pub fn drop_shape(p: &mut Painter, cx: i32, cy: i32, r: i32, d: i32, c: Color) {
    let inside = |x: i64, y: i64| {
        // Quarter-pixel units relative to the centre.
        let (r, d) = (4 * r as i64, 4 * d as i64);
        if y < -d {
            return false;
        }
        if y >= 0 || y * d >= -r * r {
            return x * x + y * y <= r * r;
        }
        x * x * (d * d - r * r) <= r * r * (y + d) * (y + d)
    };
    let clip = p.clip();
    for py in (cy - d - 1).max(clip.y)..(cy + r + 1).min(clip.y + clip.h) {
        for px in (cx - r - 1).max(clip.x)..(cx + r + 1).min(clip.x + clip.w) {
            let mut n = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    if inside(((px - cx) * 4 + sx) as i64, ((py - cy) * 4 + sy) as i64) {
                        n += 1;
                    }
                }
            }
            if n > 0 {
                p.fill_a(px, py, 1, 1, c, (n * 255 / 16) as u8);
            }
        }
    }
}
