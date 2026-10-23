//! Shared look and widgets for the built-in apps (Clock, Calendar, Activity Monitor).
//!
//! The apps draw their whole window each time (immediate mode) and record clickable regions in a
//! [`Hits`] list as they go; a click is looked up there and handled by id. All colours come from
//! [`Theme`], so the dark theme is automatic. Sizes follow docs/ui-concepts/SPEC.md. Text is the
//! system font ([`text`]) in the family Settings chooses; Unifont only where an app asks for
//! monospace (docs/ui-concepts/SPEC-FONT-ROLLOUT.md).

#![no_std]

extern crate alloc;

pub mod field;
pub mod shapes;
pub mod theme;
pub mod widgets;

pub use field::Field;
pub use theme::Theme;

/// The system font for appkit apps (re-exported from `gui::text`; see there for the API):
/// `Window::text`/`bold`/`text_x`/`bold_x` and the widgets draw with it, `Window::mono*` and
/// `widgets::mono_r`/`fit_mono` are the monospace opt-in.
pub use gui::text;

/// A rectangle in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    pub fn inset(&self, d: i32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// The clickable regions drawn this frame. Later ones sit on top of earlier ones.
#[derive(Default)]
pub struct Hits(alloc::vec::Vec<(Rect, u32)>);

impl Hits {
    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn add(&mut self, r: Rect, id: u32) {
        self.0.push((r, id));
    }

    /// The id of the top region under the point.
    pub fn at(&self, x: i32, y: i32) -> Option<u32> {
        self.0
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .map(|&(_, id)| id)
    }

    /// Drop everything drawn so far (a sheet opens over the page: only it takes clicks).
    pub fn modal(&mut self) {
        self.0.clear();
    }
}
