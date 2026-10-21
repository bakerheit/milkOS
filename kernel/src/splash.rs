//! Boot splash: cream background, a milk drop, the name, and a thin progress bar.

use crate::font::{self, GLYPH_H};
use crate::fontx;
use crate::gfx::{self, Color, DrawTarget, rgb};

const CREAM: Color = rgb(0xFA, 0xF4, 0xE6);
const WHITE: Color = rgb(0xFF, 0xFD, 0xF8);
const BLUE: Color = rgb(0x3E, 0x6F, 0xB0);
const SLATE: Color = rgb(0x2E, 0x3A, 0x4F);
const MUTED: Color = rgb(0x9A, 0x93, 0x84);
const TRACK: Color = rgb(0xE6, 0xDD, 0xC6);
const SHINE: Color = rgb(0xCF, 0xE0, 0xF4);
const SHADOW: Color = rgb(0xB9, 0xAD, 0x92);

/// Busy-wait using the architectural generic timer (no interrupts or setup needed).
fn delay_ms(ms: u64) {
    let (freq, start): (u64, u64);
    // SAFETY: reading EL0-accessible system counter registers.
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) start, options(nomem, nostack));
    }
    let ticks = freq / 1000 * ms;
    loop {
        let now: u64;
        // SAFETY: as above.
        unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) now, options(nomem, nostack)) };
        if now.wrapping_sub(start) >= ticks {
            break;
        }
    }
}

/// Is (x, y) (quarter pixels, origin at the circle centre, y down) inside a drop whose round part
/// has radius `r` and whose tip is `d` above the centre?
fn in_drop(x: i32, y: i32, r: i32, d: i32) -> bool {
    let (x, y, r, d) = (x as i64, y as i64, r as i64, d as i64);
    if y < -d {
        return false;
    }
    if y >= 0 || y * d >= -r * r {
        // The circular part: everything on or below the tangent points.
        return x * x + y * y <= r * r;
    }
    // The cone between the tip and the tangent points: half width grows linearly from the tip.
    x * x * (d * d - r * r) <= r * r * (y + d) * (y + d)
}

fn in_ellipse(x: i32, y: i32, cx: i32, cy: i32, rx: i32, ry: i32) -> bool {
    let (dx, dy, rx, ry) = ((x - cx) as i64, (y - cy) as i64, rx as i64, ry as i64);
    dx * dx * ry * ry + dy * dy * rx * rx <= rx * rx * ry * ry
}

// The artist's splash (crate::art: background, drop and the bar's track; the wordmark,
// caption and bar fill are drawn here). Used when the screen is its size.
const ART_INK: Color = rgb(0x3A, 0x2E, 0x28);
const ART_BLUE: Color = rgb(0x7F, 0x93, 0xC9);
const ART_COCOA: Color = rgb(0x6B, 0x58, 0x46);
const ART_STRAW: Color = rgb(0xE5, 0x8C, 0x97);
/// The art's track: (402, 557) 220 x 4; the wordmark sits between the drop and the bar.
const ART_BAR: (i32, i32, i32, i32) = (402, 557, 220, 4);
const ART_WORD_Y: i32 = 458;

fn art_splash(t: &dyn DrawTarget) -> Option<&'static crate::art::ImageArt> {
    crate::art::image("splash").filter(|a| a.w as usize == t.width() && a.h as usize == t.height())
}

/// Draw the splash (with an empty progress bar) on a target of any size. See [`bar`] to advance it.
pub fn draw(t: &mut dyn DrawTarget) {
    if let Some(a) = art_splash(t) {
        let stride = t.stride();
        a.decode_rows(t.pixels_mut(), stride);
        // The wordmark in the desktop's smooth brand lettering (crate::fontx, 4x bold):
        // "milk" espresso, "OS" blue; the caption in the UI's proportional face.
        let w = t.width() as i32;
        let scale = 4;
        let tx = (w - font::str_width("milkOS", scale)) / 2;
        let x = fontx::draw_smooth(t, tx, ART_WORD_Y, "milk", ART_INK, scale, true);
        fontx::draw_smooth(t, x, ART_WORD_Y, "OS", ART_BLUE, scale, true);
        let cap = "booting";
        let cx = (w - fontx::prop_width(cap, false)) / 2;
        fontx::draw_prop(t, cx, ART_BAR.1 + 24, cap, ART_COCOA, false);
        bar(t, 0);
        return;
    }
    let (w, h) = (t.width() as i32, t.height() as i32);
    t.clear(CREAM);

    const R: i32 = 60; // round part radius, pixels
    const D: i32 = 138; // centre to tip, pixels
    let cx = w / 2;
    let cy = h / 2 - 66 + R / 2;

    // Soft shadow under the drop.
    let sy = cy + R + 16;
    for (grow, alpha) in [(0, 40u8), (1, 40), (2, 40)] {
        let rx = (R + 14 - grow * 12) * 4;
        let ry = (9 - grow * 3) * 4;
        gfx::fill_aa(
            t,
            (cx - R - 20, sy - 14, 2 * R + 40, 28),
            (cx, sy),
            SHADOW,
            alpha,
            &|x, y| in_ellipse(x, y, 0, 0, rx, ry),
        );
    }

    // Outer drop in blue, inner in milk white, so it reads as an outline.
    let bbox = (cx - R - 2, cy - D - 2, 2 * R + 4, D + R + 4);
    gfx::fill_aa(t, bbox, (cx, cy), BLUE, 255, &|x, y| {
        in_drop(x, y, R * 4, D * 4)
    });
    gfx::fill_aa(t, bbox, (cx, cy), WHITE, 255, &|x, y| {
        in_drop(x, y, (R - 10) * 4, (D - 22) * 4)
    });
    // A little shine on the lower left.
    gfx::fill_aa(t, bbox, (cx, cy), SHINE, 255, &|x, y| {
        in_ellipse(x, y, -26 * 4, 16 * 4, 7 * 4, 18 * 4)
    });

    // Name. "milk" dark, "OS" in the drop blue.
    let scale = 4;
    let name_w = font::str_width("milkOS", scale);
    let tx = (w - name_w) / 2;
    let ty = cy + R + 56;
    let x = font::draw_str(t, tx, ty, "milk", SLATE, None, scale);
    font::draw_str(t, x, ty, "OS", BLUE, None, scale);

    // Quiet caption.
    let cap = "booting";
    font::draw_str(
        t,
        (w - font::str_width(cap, 1)) / 2,
        ty + GLYPH_H as i32 * scale + 56,
        cap,
        MUTED,
        None,
        1,
    );
    bar(t, 0);
}

/// Geometry of the progress bar: (x, y, w, h).
fn bar_rect(t: &dyn DrawTarget) -> (i32, i32, i32, i32) {
    let w = 220;
    let y = t.height() as i32 / 2 + 172;
    ((t.width() as i32 - w) / 2, y, w, 4)
}

/// Redraw the progress bar at `percent` (0..=100).
pub fn bar(t: &mut dyn DrawTarget, percent: i32) {
    if art_splash(t).is_some() {
        // Over the art's track: straw fill only (the track is in the picture).
        let (x, y, w, h) = ART_BAR;
        t.fill_rect(x, y, w * percent.clamp(0, 100) / 100, h, ART_STRAW);
        return;
    }
    let (x, y, w, h) = bar_rect(t);
    t.fill_rect(x, y, w, h, TRACK);
    t.fill_rect(x, y, w * percent.clamp(0, 100) / 100, h, BLUE);
}

/// Show the splash for about `ms` milliseconds, then fade to `to` (or leave it up if `None`).
pub fn show(t: &mut dyn DrawTarget, ms: u64, to: Option<Color>) {
    draw(t);
    const STEPS: i32 = 40;
    for i in 1..=STEPS {
        delay_ms(ms / STEPS as u64);
        bar(t, i * 100 / STEPS);
    }
    let Some(to) = to else {
        return;
    };
    for _ in 0..6 {
        let (w, h) = (t.width() as i32, t.height() as i32);
        t.fill_rect_alpha(0, 0, w, h, to, 70);
    }
    t.clear(to);
}
