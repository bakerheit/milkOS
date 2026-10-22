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
    /// An installed package: a box with its lid.
    Package,
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
                p.mono_a(tx + dx * sc, ty + dy * sc, ">", fg, 255, sc);
            }
            p.fill(tx + 8 * sc + u(2), ty + 13 * sc, u(11), th(3), fg);
        }
        Icon::Gear => {
            // A cog: eight teeth round a ring, so it does not read as a clock.
            for (a, b, w, h) in [
                (21, 7, 6, 6),
                (21, 35, 6, 6),
                (7, 21, 6, 6),
                (35, 21, 6, 6),
                (11, 11, 6, 6),
                (31, 11, 6, 6),
                (11, 31, 6, 6),
                (31, 31, 6, 6),
            ] {
                rect(p, a, b, w, h, u(2), fg);
            }
            ring(p, 12, 12, 24, 24, u(12), th(5), fg);
            rect(p, 21, 21, 6, 6, u(3), fg);
        }
        Icon::Note => {
            ring(p, 12, 11, 24, 26, u(3), th(2), fg);
            rect(p, 17, 18, 14, 2, 0, fg);
            rect(p, 17, 24, 14, 2, 0, fg);
            rect(p, 17, 30, 9, 2, 0, fg);
        }
        Icon::Globe => {
            // A globe: a wide meridian and three parallels (one thin line read as crosshairs).
            ring(p, 10, 10, 28, 28, u(14), th(3), fg);
            ring(p, 17, 10, 14, 28, u(7), th(2), fg);
            rect(p, 10, 23, 28, 2, 0, fg);
            rect(p, 13, 16, 22, 2, 0, fg);
            rect(p, 13, 30, 22, 2, 0, fg);
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
        Icon::Package => {
            ring(p, 11, 17, 26, 21, u(3), th(3), fg);
            rect(p, 9, 11, 30, 8, u(3), fg);
            rect(p, 21, 23, 6, 3, 0, fg);
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
            // A desk calendar: binder rings on top and a grid of days, today in straw.
            ring(p, 10, 12, 28, 26, u(4), th(3), fg);
            rect(p, 10, 18, 28, 3, 0, fg);
            rect(p, 16, 8, 3, 7, u(1), fg);
            rect(p, 29, 8, 3, 7, u(1), fg);
            for (a, b) in [(15, 25), (22, 25), (29, 25), (15, 31), (29, 31)] {
                rect(p, a, b, 4, 3, 0, fg);
            }
            rect(p, 21, 30, 6, 5, u(1), STRAW);
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

// ---- the artist's icons (crate::art) ------------------------------------------------------

/// The art asset for an app, by the name the dock, Skim and the drawer use.
pub fn art_name(app: &str) -> Option<&'static str> {
    Some(match app {
        "Files" => "files",
        "Terminal" => "terminal",
        "Browser" => "browser",
        "Notes" => "notes",
        "Paint" => "paint",
        "Music" => "music",
        "Settings" => "settings",
        "Store" | "Milk Store" => "store",
        "Today" => "today",
        "Calendar" => "calendar",
        "Spaces" => "spaces",
        "Clock" => "clock",
        "Preview" => "preview",
        "Activity" | "Activity Monitor" => "activity",
        "Text Editor" => "text-editor",
        "Disk" | "Disk Utility" => "disk-utility",
        "Reminders" => "reminders",
        _ => return None,
    })
}

/// The art for an installed app (a manifest): by its package name, its dock label
/// ("Disk Utility" -> disk-utility), or a known alias; None: the drawn letter tile.
pub fn package_art(pkg_name: &str, label: &str) -> Option<&'static str> {
    let kebab: alloc::string::String = label
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let alias = match pkg_name {
        "diskutil" => "disk-utility",
        "texteditor" | "edit" => "text-editor",
        _ => pkg_name,
    };
    let by_label = art_name(label).unwrap_or("");
    crate::art::icon_named(&[pkg_name, alias, &kebab, by_label])
}

/// Tiles that are milk (no edge in the art): the shell draws their 1px sand edge.
fn milk_tile(name: &str) -> bool {
    matches!(
        name,
        "notes"
            | "paint"
            | "text-editor"
            | "calendar"
            | "spaces"
            | "today"
            | "reminders"
            | "file-document"
            | "file-image"
            | "file-audio"
            | "file-archive"
            | "file-package"
    )
}

/// Scaled copies of icons at sizes the art does not ship, made once per (name, size, theme).
type ScaledIcons = alloc::collections::BTreeMap<(&'static str, i32, bool), &'static [u32]>;
static SCALED: crate::sync::SpinLock<ScaledIcons> =
    crate::sync::SpinLock::new(alloc::collections::BTreeMap::new());

/// The pixels of art icon `name` at `s` px in the current theme.
fn art_pixels(name: &str, s: i32) -> Option<(&'static str, &'static [u32])> {
    let dark = super::theme::dark();
    if let Some(i) = crate::art::icon(name, s, dark) {
        return Some((i.name, i.pixels()));
    }
    let src = crate::art::icon_near(name, s, dark)?;
    let key = (src.name, s, dark);
    if let Some(px) = SCALED.lock().get(&key) {
        return Some((src.name, px));
    }
    let px: &'static [u32] = alloc::boxed::Box::leak(
        crate::art::scale_icon(src.pixels(), src.size as usize, s as usize).into_boxed_slice(),
    );
    SCALED.lock().insert(key, px);
    Some((src.name, px))
}

/// Draw the artist's icon `name` as an `s` x `s` tile at (x, y). False if there is no such
/// asset (draw the built-in one instead).
pub fn art(p: &mut Painter, x: i32, y: i32, s: i32, name: &str) -> bool {
    let Some((name, px)) = art_pixels(name, s) else {
        return false;
    };
    p.sprite(x, y, px, s as usize, s as usize);
    art_finish(p, x, y, s, name);
    true
}

/// Art `name`'s pixels at exactly `s` px, from the smallest larger size the art ships (the 96
/// px icons for a magnified dock): not cached, the caller keeps them.
pub fn art_scaled(name: &str, s: i32) -> Option<(&'static str, alloc::vec::Vec<u32>)> {
    let dark = super::theme::dark();
    if let Some(i) = crate::art::icon(name, s, dark) {
        return Some((i.name, i.pixels().to_vec()));
    }
    let src = crate::art::icon_near(name, s, dark)?;
    Some((
        src.name,
        crate::art::scale_icon(src.pixels(), src.size as usize, s as usize),
    ))
}

/// What the shell adds on top of an art tile: today's date on the calendar, the sand edge
/// of milk tiles.
pub fn art_finish(p: &mut Painter, x: i32, y: i32, s: i32, name: &str) {
    if name == "calendar" && s >= 28 {
        // The art carries a sample date: today's instead, on the tile's milk.
        let day = alloc::format!("{}", super::rtc::now().day);
        let (sc, top) = if s >= 40 {
            (2, s * 40 / 100)
        } else {
            (1, s * 42 / 100)
        };
        p.rrect(
            x + s / 8,
            y + top,
            s - s / 4,
            s - top - s / 10,
            2,
            MILK,
            255,
        );
        let f = crate::sysfont::Font::ui(sc, crate::sysfont::Weight::Bold);
        let (by, bh) = (y + top, s - top - s / 10);
        p.text_f(
            x + (s - f.width(&day)) / 2,
            f.center_caps(by, bh),
            &day,
            ESPRESSO,
            f,
        );
    }
    if milk_tile(name) {
        let r = (s * 24 + 50) / 100;
        p.rrect_stroke(x, y, s, s, r, 1, SAND, 255);
    }
}

/// An app's tile: the art if there is one, else the drawn glyph.
#[allow(clippy::too_many_arguments)]
pub fn app_tile(
    p: &mut Painter,
    x: i32,
    y: i32,
    s: i32,
    app: &str,
    icon: Icon,
    bg: Color,
    fg: Color,
) {
    if let Some(n) = art_name(app)
        && art(p, x, y, s, n)
    {
        return;
    }
    draw(p, x, y, s, icon, bg, 255, fg, bg == MILK || bg == CREAM);
}

/// A file type tile for Files (folder, document, image, audio, archive, package).
pub fn file_tile(p: &mut Painter, x: i32, y: i32, s: i32, kind: &str) -> bool {
    art(p, x, y, s, &alloc::format!("file-{kind}"))
}
