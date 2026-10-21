//! Buttons, segmented controls, switches, chips, text fields and text helpers, in the SPEC's
//! sizes and colours. Each widget draws itself and registers its region in [`Hits`].

// Immediate-mode widgets take the window, theme and hit list plus their own parameters, so
// the argument lists are long on purpose (a struct per call would only move the noise).
#![allow(clippy::too_many_arguments)]

use crate::{Field, Hits, Rect, Theme};
use alloc::string::String;
use gui::{Color, Window, str_width};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
}

/// A filled rounded rect with a 1px border.
pub fn bordered(win: &mut Window, r: Rect, radius: i32, fill: Color, border: Color) {
    win.rrect(r.x, r.y, r.w, r.h, radius, border);
    win.rrect(
        r.x + 1,
        r.y + 1,
        r.w - 2,
        r.h - 2,
        (radius - 1).max(0),
        fill,
    );
}

/// Text cut to `max_w` pixels, ending in "~" if it had to be shortened.
pub fn fit(s: &str, max_w: i32) -> String {
    if str_width(s, 1) <= max_w {
        return String::from(s);
    }
    let keep = ((max_w / 8) - 1).max(0) as usize;
    let mut o: String = s.chars().take(keep).collect();
    o.push('~');
    o
}

/// Text centred on `cx`.
pub fn text_c(win: &mut Window, cx: i32, y: i32, s: &str, c: Color, scale: i32) {
    win.text_x(cx - str_width(s, scale) / 2, y, s, c, scale);
}

/// Text ending at `rx`.
pub fn text_r(win: &mut Window, rx: i32, y: i32, s: &str, c: Color, scale: i32) {
    win.text_x(rx - str_width(s, scale), y, s, c, scale);
}

/// Bold text at a size (the second copy 1px right; at 2x and up, `scale` px).
pub fn bold_x(win: &mut Window, x: i32, y: i32, s: &str, c: Color, scale: i32) {
    win.text_x(x, y, s, c, scale);
    win.text_x(x + scale.max(1), y, s, c, scale);
}

pub fn bold_c(win: &mut Window, cx: i32, y: i32, s: &str, c: Color, scale: i32) {
    bold_x(win, cx - str_width(s, scale) / 2, y, s, c, scale);
}

/// A button in the SPEC's kinds. `pill` is the small 24px fully rounded one; otherwise the
/// 28px dialog button. `enabled == false` draws it disabled and registers no click.
pub fn button(
    win: &mut Window,
    th: &Theme,
    hits: &mut Hits,
    r: Rect,
    label: &str,
    kind: Kind,
    id: u32,
    enabled: bool,
    pill: bool,
) {
    let radius = if pill { r.h / 2 } else { 8 };
    let (fill, border, text) = if !enabled {
        (th.oat, Some(th.sand), th.latte)
    } else {
        match kind {
            Kind::Primary => (th.straw, None, th.ink),
            Kind::Secondary => (th.oat, Some(th.sand), th.espresso),
            Kind::Danger => (th.danger, None, th.on_danger),
        }
    };
    match border {
        Some(b) => bordered(win, r, radius, fill, b),
        None => win.rrect(r.x, r.y, r.w, r.h, radius, fill),
    }
    bold_c(win, r.x + r.w / 2 - 1, r.y + (r.h - 16) / 2, label, text, 1);
    if enabled {
        hits.add(r, id);
    }
}

/// The width a button needs for its label (min 88 for dialog buttons).
pub fn button_w(label: &str, pill: bool) -> i32 {
    let w = str_width(label, 1) + if pill { 28 } else { 24 };
    if pill { w } else { w.max(88) }
}

/// A segmented control: height 28, radius 14, oat fill, the selected segment milk with a sand
/// border. Segment `i` registers id `base + i`. Returns its width.
pub fn segmented(
    win: &mut Window,
    th: &Theme,
    hits: &mut Hits,
    x: i32,
    y: i32,
    labels: &[&str],
    selected: usize,
    base: u32,
) -> i32 {
    let pad = 14;
    let widths: alloc::vec::Vec<i32> = labels.iter().map(|l| str_width(l, 1) + 2 * pad).collect();
    let total: i32 = widths.iter().sum::<i32>() + 4;
    win.rrect(x, y, total, 28, 14, th.oat);
    let mut cx = x + 2;
    for (i, l) in labels.iter().enumerate() {
        let r = Rect::new(cx, y + 2, widths[i], 24);
        if i == selected {
            bordered(win, r, 12, th.milk, th.sand);
        }
        let col = if i == selected { th.espresso } else { th.cocoa };
        bold_c(win, r.x + r.w / 2 - 1, r.y + 4, l, col, 1);
        hits.add(r, base + i as u32);
        cx += widths[i];
    }
    total
}

/// An on/off switch 44x24 (straw when on).
pub fn switch(win: &mut Window, th: &Theme, hits: &mut Hits, x: i32, y: i32, on: bool, id: u32) {
    let r = Rect::new(x, y, 44, 24);
    win.rrect(r.x, r.y, r.w, r.h, 12, if on { th.straw } else { th.sand });
    let knob_x = if on { x + 44 - 12 - 3 } else { x + 12 + 3 };
    win.disc(knob_x, y + 12, 9, th.milk);
    hits.add(r, id);
}

/// A chip (preset button, tag): height 24, radius 8, oat fill with a sand border, or straw tint
/// when `on`.
pub fn chip(
    win: &mut Window,
    th: &Theme,
    hits: &mut Hits,
    x: i32,
    y: i32,
    label: &str,
    on: bool,
    id: u32,
) -> i32 {
    let w = str_width(label, 1) + 24;
    let r = Rect::new(x, y, w, 24);
    if on {
        bordered(win, r, 8, Theme::tint(th.milk, th.straw, 0x66), th.straw);
    } else {
        bordered(win, r, 8, th.oat, th.sand);
    }
    win.text(x + 12, y + 4, label, th.espresso);
    hits.add(r, id);
    w
}

/// A text field: milk fill, 1px sand border (honey ring when focused), the caret drawn when
/// focused and `caret_on`. `placeholder` shows in latte when empty.
pub fn field(
    win: &mut Window,
    th: &Theme,
    hits: &mut Hits,
    r: Rect,
    f: &Field,
    placeholder: &str,
    focused: bool,
    caret_on: bool,
    id: u32,
) {
    if focused {
        win.rrect(r.x - 2, r.y - 2, r.w + 4, r.h + 4, 10, th.honey);
    }
    bordered(win, r, 8, th.milk, th.sand);
    let max_w = r.w - 16;
    let ty = r.y + (r.h - 16) / 2;
    if f.text.is_empty() && !placeholder.is_empty() {
        win.text(r.x + 8, ty, &fit(placeholder, max_w), th.latte);
    } else {
        // Scroll so the caret stays visible.
        let chars: alloc::vec::Vec<char> = f.text.chars().collect();
        let fit_n = (max_w / 8).max(1) as usize;
        let start = f.cur.saturating_sub(fit_n.saturating_sub(1));
        let shown: String = chars.iter().skip(start).take(fit_n).collect();
        win.text(r.x + 8, ty, &shown, th.espresso);
        if focused && caret_on {
            let cx = r.x + 8 + (f.cur - start) as i32 * 8;
            win.fill(cx, ty, 2, 16, th.espresso);
        }
    }
    hits.add(r, id);
}

/// A 1px horizontal line.
pub fn hline(win: &mut Window, x: i32, y: i32, w: i32, c: Color) {
    win.fill(x, y, w, 1, c);
}

/// A 1px vertical line.
pub fn vline(win: &mut Window, x: i32, y: i32, h: i32, c: Color) {
    win.fill(x, y, 1, h, c);
}

/// The window's standard toolbar band (40 high, cream, 1px sand line below) at the top.
pub fn toolbar(win: &mut Window, th: &Theme) {
    let w = win.w;
    win.fill(0, 0, w, 40, th.cream);
    hline(win, 0, 40, w, th.sand);
}

/// The status bar (24 high, cream, 1px sand line above) at the bottom.
pub fn status_bar(win: &mut Window, th: &Theme, text: &str) {
    let (w, h) = (win.w, win.h);
    win.fill(0, h - 24, w, 24, th.cream);
    hline(win, 0, h - 24, w, th.sand);
    win.text(12, h - 20, text, th.cocoa);
}
