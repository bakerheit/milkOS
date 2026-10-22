//! Buttons (SPEC "Buttons"): one painter for every labelled button, so the contrast fix is in
//! one place. Filled buttons are straw with ink text (5.33:1; cream on straw was 2.42:1), the
//! text is the fixed ink so it stays dark in Chocolate milk too.
//!
//! * Primary: straw, ink bold. Hover #EA9AA4, pressed #DE7D89.
//! * Secondary: oat with a 1px sand border, primary text.
//! * Danger: #B5473B with a fixed near-white (never the milk token, which turns dark).
//! * Disabled: oat with a 1px sand border, latte label (exempt, never the only signal: the
//!   caller also leaves it out of its hit list).
//!
//! Sizes: a pill (cards, toasts, banners) is 24 high, fully rounded, 14px padding each side; a
//! dialog button (sheets, panels, Open / Save) is 28 high, radius 8, at least 88 wide.

use crate::gfx::{Color, Rect, rgb};
use crate::sync::SpinLock;

use super::paint::Painter;
use super::theme::{COCOA, ESPRESSO, HONEY, INK, LATTE, OAT, R_BUTTON, SAND, STRAW};

pub const STRAW_HOVER: Color = rgb(0xEA, 0x9A, 0xA4);
pub const STRAW_PRESSED: Color = rgb(0xDE, 0x7D, 0x89);
pub const DANGER: Color = rgb(0xB5, 0x47, 0x3B);
/// Text on danger: #FFFDF8 in both themes. One LSB off milk so the theme map leaves it.
pub const DANGER_FG: Color = rgb(0xFF, 0xFD, 0xF9);

pub const PILL_H: i32 = 24;
pub const PILL_PAD: i32 = 14;
pub const DIALOG_H: i32 = 28;
pub const DIALOG_MIN_W: i32 = 88;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Primary,
    Secondary,
    Danger,
    Disabled,
}

/// Interaction state (pressed wins over hover).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct State {
    pub hover: bool,
    pub pressed: bool,
    pub focus: bool,
}

impl State {
    pub fn hover(on: bool) -> State {
        State {
            hover: on,
            ..State::default()
        }
    }
}

/// Where the left button is held down in the surface being drawn (window coordinates),
/// set by the desktop around each window's render: a primary button under it is pressed.
static HELD: SpinLock<Option<(i32, i32)>> = SpinLock::new(None);

pub fn set_held(at: Option<(i32, i32)>) {
    *HELD.lock() = at;
}

/// The width of a pill for `label`.
pub fn pill_w(label: &str) -> i32 {
    crate::sysfont::width(label, true) + 2 * PILL_PAD
}

/// The width of a dialog button for `label`.
pub fn dialog_w(label: &str) -> i32 {
    (crate::sysfont::width(label, true) + 2 * 16).max(DIALOG_MIN_W)
}

/// Paint a button filling `r`. Its radius follows its height: a 24px pill is fully rounded,
/// anything taller is a dialog button (radius 8).
pub fn paint(p: &mut Painter, r: Rect, label: &str, kind: Kind, st: State) {
    let radius = if r.h <= PILL_H { r.h / 2 } else { R_BUTTON };
    // Pressed: the button is held down (in buffer coordinates, through the painter).
    let (ox, oy) = p.origin();
    let held = HELD
        .lock()
        .is_some_and(|(x, y)| super::paint::hit(&Rect::new(r.x + ox, r.y + oy, r.w, r.h), x, y));
    let st = State {
        pressed: st.pressed || held,
        hover: st.hover || held,
        ..st
    };
    let (fill, text, border) = match kind {
        Kind::Primary => {
            let f = if st.pressed {
                STRAW_PRESSED
            } else if st.hover {
                STRAW_HOVER
            } else {
                STRAW
            };
            (f, INK, None)
        }
        Kind::Secondary => (OAT, ESPRESSO, Some(SAND)),
        Kind::Danger => (DANGER, DANGER_FG, None),
        Kind::Disabled => (OAT, LATTE, Some(SAND)),
    };
    if st.focus {
        p.rrect_stroke(
            r.x - 4,
            r.y - 4,
            r.w + 8,
            r.h + 8,
            radius + 4,
            2,
            HONEY,
            255,
        );
    }
    p.rrect_r(r, radius, fill, 255);
    if let Some(b) = border {
        p.rrect_stroke(r.x, r.y, r.w, r.h, radius, 1, b, 255);
    }
    // Secondary hover: lighten 4% / darken 4% (SPEC "What the mockup does not show").
    if kind == Kind::Secondary && (st.hover || st.pressed) {
        let c = if st.pressed {
            COCOA
        } else {
            super::theme::WHITE
        };
        p.rrect_r(r, radius, c, 0x14);
        p.rrect_stroke(r.x, r.y, r.w, r.h, radius, 1, SAND, 255);
    }
    p.text_p_center(r.x, r.w, r.y + (r.h - 16) / 2, label, text, true);
}
