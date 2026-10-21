//! Windows: an offscreen surface each, the chrome (SPEC: Window chrome), and the client
//! interface [`WindowContents`].
//!
//! The compositor owns the frame (title bar, traffic lights, border, rounded corners, shadow);
//! the client owns the body. Kernel-side apps implement `WindowContents` directly. A userland
//! client later gets the same trait implemented on its behalf by a proxy that maps the window's
//! body buffer (contiguous PMM frames, see `buffer.rs`) into the client as shared memory, forwards
//! events over a channel and turns "I drew" messages into `invalidate`.

use alloc::boxed::Box;

use crate::gfx::{Color, Rect};

use super::buffer::Buffer;
use super::event::{Button, Key, Mods};
use super::paint::{Painter, grow};
use super::theme::*;

/// A key press delivered to the focused window.
#[derive(Clone, Copy, Debug)]
pub struct KeyInput {
    pub key: Key,
    /// The text the key types (None for shortcuts and non-text keys).
    pub ch: Option<char>,
    pub mods: Mods,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointerKind {
    Down(Button),
    Up(Button),
    /// Moved with the button that went down in this window still held.
    Drag,
}

/// A pointer event in body coordinates (0, 0 = top-left of the body).
#[derive(Clone, Copy, Debug)]
pub struct PointerInput {
    pub x: i32,
    pub y: i32,
    pub kind: PointerKind,
}

/// What a window's client implements. All methods but `title` and `draw` have defaults.
/// Methods returning `bool` return true when the body needs redrawing.
pub trait WindowContents {
    /// Shown centred in the title bar.
    fn title(&self) -> &str;
    /// Dark chrome (the Terminal's espresso look).
    fn dark(&self) -> bool {
        false
    }
    /// Body background (what the body is cleared to before `draw`).
    fn background(&self) -> Color {
        if self.dark() { ESPRESSO } else { MILK }
    }
    /// Draw the body. `p`'s logical (0, 0) is the body's top-left; it is `w` x `h`.
    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, focused: bool);
    fn key(&mut self, _k: &KeyInput) -> bool {
        false
    }
    fn pointer(&mut self, _e: &PointerInput) -> bool {
        false
    }
    /// Wheel; positive moves towards the end of the content.
    fn scroll(&mut self, _dy: i32) -> bool {
        false
    }
    /// Called every frame with the uptime; for clocks, live stats, log followers.
    fn tick(&mut self, _now_ns: u64) -> bool {
        false
    }
    /// Menu titles for the menu bar while this window is focused.
    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "View", "Window", "Help"]
    }
}

/// Which built-in app a window belongs to (dock dots, menu bar name, single instance).
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum App {
    Files,
    Terminal,
    Notes,
    About,
    /// A userland window client (`wm::client`), by its client id.
    Client(u32),
}

impl App {
    pub fn name(self) -> &'static str {
        match self {
            App::Files => "Files",
            App::Terminal => "Terminal",
            App::Notes => "Notes",
            App::About => "About milkOS",
            App::Client(_) => "App",
        }
    }
}

/// Parts of the frame the pointer can hit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    /// Traffic light 0 = close, 1 = minimise, 2 = zoom.
    Light(usize),
    Title,
    Resize,
    Body,
}

pub struct Window {
    pub id: u32,
    pub app: App,
    /// Screen position and size, frame included.
    pub rect: Rect,
    pub space: usize,
    pub minimized: bool,
    /// The rect to go back to when un-zooming.
    pub restore: Option<Rect>,
    pub surf: Buffer,
    pub contents: Box<dyn WindowContents>,
    /// The surface is out of date.
    pub dirty: bool,
    /// Focus state the surface was last drawn with.
    drawn_focused: bool,
    /// The pointer is over the traffic lights: show their symbols.
    pub hover: bool,
}

/// Light `i`'s box in window coordinates.
pub fn light_rect(i: usize) -> Rect {
    Rect::new(
        LIGHT_X + i as i32 * (LIGHT_D + LIGHT_GAP),
        LIGHT_Y,
        LIGHT_D,
        LIGHT_D,
    )
}

/// Body rect in window coordinates: inside the border, under the title bar and its line.
pub fn body_rect(w: i32, h: i32) -> Rect {
    Rect::new(1, TITLE_H + 1, w - 2, h - TITLE_H - 2)
}

impl Window {
    pub fn new(
        id: u32,
        app: App,
        rect: Rect,
        space: usize,
        contents: Box<dyn WindowContents>,
    ) -> Option<Window> {
        let surf = Buffer::new(rect.w as usize, rect.h as usize, MILK)?;
        Some(Window {
            id,
            app,
            rect,
            space,
            minimized: false,
            restore: None,
            surf,
            contents,
            dirty: true,
            drawn_focused: false,
            hover: false,
        })
    }

    /// Everything this window paints on screen: frame plus shadow.
    pub fn bounds(&self) -> Rect {
        let r = self.rect;
        let mut b = r;
        for (g, dy, _) in SHADOW_WINDOW {
            b = b.union(&Rect::new(r.x - g, r.y - g + dy, r.w + 2 * g, r.h + 2 * g));
        }
        b
    }

    /// A rect fully covered by opaque window pixels (rows clear of the rounded corners).
    pub fn core(&self) -> Rect {
        let r = self.rect;
        Rect::new(r.x, r.y + R_WINDOW, r.w, r.h - 2 * R_WINDOW)
    }

    pub fn hit(&self, sx: i32, sy: i32) -> Option<Part> {
        let (x, y) = (sx - self.rect.x, sy - self.rect.y);
        if x < 0 || y < 0 || x >= self.rect.w || y >= self.rect.h {
            return None;
        }
        for i in 0..3 {
            if super::paint::hit(&grow(&light_rect(i), 3), x, y) {
                return Some(Part::Light(i));
            }
        }
        if x >= self.rect.w - 16 && y >= self.rect.h - 16 {
            return Some(Part::Resize);
        }
        if y < TITLE_H + 1 {
            return Some(Part::Title);
        }
        Some(Part::Body)
    }

    /// Change size (keeps the surface allocation when it can). Marks dirty.
    pub fn set_rect(&mut self, r: Rect) {
        if r.w != self.rect.w || r.h != self.rect.h {
            if !self.surf.resize(r.w as usize, r.h as usize) {
                return; // out of memory: keep the old size
            }
            self.dirty = true;
        }
        self.rect = r;
    }

    /// Redraw the surface if needed (or if the focus state changed). True if it was redrawn.
    pub fn render(&mut self, focused: bool) -> bool {
        if !self.dirty && self.drawn_focused == focused {
            return false;
        }
        self.dirty = false;
        self.drawn_focused = focused;
        let (w, h) = (self.rect.w, self.rect.h);
        let dark = self.contents.dark();
        let mut p = Painter::new(&mut self.surf);
        let body = self.contents.background();
        p.fill(0, 0, w, h, body);
        // Title bar and the line under it.
        let (bar, line, edge) = if dark {
            (TERM_BAR, TERM_BAR, TERM_BAR)
        } else {
            (CREAM, SAND, SAND)
        };
        p.fill(0, 0, w, TITLE_H, bar);
        p.fill(0, TITLE_H, w, 1, line);
        // Traffic lights.
        let lights = if focused {
            [STRAW, HONEY, MATCHA]
        } else {
            [LATTE, LATTE, LATTE]
        };
        for (i, c) in lights.into_iter().enumerate() {
            let r = light_rect(i);
            p.circle(r.x, r.y, r.w, c, 255);
            if self.hover {
                // x, -, + (hover state; the mockup does not show one, this is macOS's).
                let (x, y, m) = (r.x, r.y, ESPRESSO);
                match i {
                    0 => {
                        for k in 0..6 {
                            p.fill_a(x + 3 + k, y + 3 + k, 1, 1, m, 0xB0);
                            p.fill_a(x + 8 - k, y + 3 + k, 1, 1, m, 0xB0);
                        }
                    }
                    1 => p.fill_a(x + 3, y + 5, 6, 2, m, 0xB0),
                    _ => {
                        p.fill_a(x + 3, y + 5, 6, 2, m, 0xB0);
                        p.fill_a(x + 5, y + 3, 2, 2, m, 0xB0);
                        p.fill_a(x + 5, y + 7, 2, 2, m, 0xB0);
                    }
                }
            }
        }
        // Title: bold, centred, top at y8 inside the border.
        let tc = match (dark, focused) {
            (true, true) => CREAM,
            (true, false) => LATTE,
            (false, true) => ESPRESSO,
            (false, false) => COCOA,
        };
        let title = self.contents.title();
        // Keep the title clear of the lights.
        let max_chars = ((w - 2 * 80) / 8).max(0) as usize;
        let t: &str = if title.chars().count() > max_chars {
            let end = title
                .char_indices()
                .nth(max_chars)
                .map_or(title.len(), |(i, _)| i);
            &title[..end]
        } else {
            title
        };
        p.text_center(0, w, 9, t, tc, true);
        // Body.
        let br = body_rect(w, h);
        {
            let mut bp = p.sub(br);
            self.contents.draw(&mut bp, br.w, br.h, focused);
        }
        // Border last, over everything.
        p.rrect_stroke(0, 0, w, h, R_WINDOW, 1, edge, 255);
        true
    }
}
