//! Charts, bars and gauges for Activity Monitor.

use appkit::Rect;
use appkit::shapes::isqrt;
use gui::{Color, Window};
use sysview::system::Ring;

/// A column chart of the last samples: one column per sample, newest at the right, scaled to
/// `max` (a floor of 1 so an idle chart is flat, not a divide by zero). Draws a light fill under
/// the top of each column so the line stays the carrier of meaning.
pub fn columns(win: &mut Window, r: Rect, data: &Ring<60>, max: u32, line: Color, track: Color) {
    win.fill(r.x, r.y, r.w, r.h, track);
    let max = max.max(1) as i64;
    let n = data.len() as i32;
    let colw = (r.w / 60).max(1);
    let start = r.x + r.w - n * colw;
    for (i, v) in data.iter().enumerate() {
        let h = ((v as i64 * r.h as i64) / max).clamp(0, r.h as i64) as i32;
        let x = start + i as i32 * colw;
        if h > 0 {
            win.fill(x, r.y + r.h - h, colw - 1, h, gui::blend(track, line, 0x4D));
            win.fill(x, r.y + r.h - h, colw - 1, 2.min(h), line);
        }
    }
}

/// A horizontal bar `frac` (0..=1000, tenths of a percent) full on a track.
pub fn bar(win: &mut Window, r: Rect, frac: u32, fill: Color, track: Color) {
    win.rrect(r.x, r.y, r.w, r.h, r.h / 2, track);
    let w = (r.w as i64 * frac.min(1000) as i64 / 1000) as i32;
    if w > 0 {
        win.rrect(r.x, r.y, w.max(r.h.min(w)), r.h, r.h / 2, fill);
    }
}

/// A stacked bar with rounded ends: segments of `(value, colour)` laid out left to right over
/// `total`, the rest left as `track`.
pub fn stacked(win: &mut Window, r: Rect, segs: &[(u64, Color)], total: u64, track: Color) {
    let rad = (r.h / 2) as i64;
    let total = total.max(1);
    // Segment boundaries in pixels.
    let mut edges = alloc::vec::Vec::new();
    let mut acc = 0u64;
    for (v, _) in segs {
        acc += v;
        edges.push(((acc.min(total) as i64 * r.w as i64) / total as i64) as i32);
    }
    for x in 0..r.w {
        // How much the round end takes off the top and bottom of this column.
        let d = if (x as i64) < rad {
            rad - x as i64
        } else if x as i64 >= r.w as i64 - rad {
            x as i64 - (r.w as i64 - rad) + 1
        } else {
            0
        };
        let inset = if d > 0 {
            (rad - isqrt((rad * rad - d * d).max(0) as u64) as i64).max(0) as i32
        } else {
            0
        };
        let colour = edges
            .iter()
            .position(|&e| x < e)
            .map(|i| segs[i].1)
            .unwrap_or(track);
        win.fill(r.x + x, r.y + inset, 1, r.h - 2 * inset, colour);
    }
}
