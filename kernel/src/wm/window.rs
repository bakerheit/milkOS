//! Windows: an offscreen surface each, the chrome (SPEC: Window chrome), and the client
//! interface [`WindowContents`].
//!
//! The compositor owns the frame (title bar, traffic lights, border, rounded corners, shadow);
//! the client owns the body. Kernel-side apps implement `WindowContents` directly. A userland
//! client later gets the same trait implemented on its behalf by a proxy that maps the window's
//! body buffer (contiguous PMM frames, see `buffer.rs`) into the client as shared memory, forwards
//! events over a channel and turns "I drew" messages into `invalidate`.

use alloc::boxed::Box;
use alloc::vec::Vec;

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
        if self.dark() { TERM_BG } else { MILK }
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
    /// The file this window is about (Notes' note, Files' selected file), for File >
    /// Browse Versions...
    fn document_path(&self) -> Option<alloc::string::String> {
        None
    }
    /// The smallest body this can live in without being clipped (client apps draw a fixed
    /// size buffer and cannot be resized yet). None: anything goes.
    fn min_body(&self) -> Option<(i32, i32)> {
        None
    }
    /// The contents' own tabs (the Terminal's shells), when there are two or more: titles and
    /// the active one. Alone in its window, they are drawn as the window's tab strip.
    fn inner_tabs(&self) -> Option<(Vec<alloc::string::String>, usize)> {
        None
    }
    fn inner_select(&mut self, _i: usize) {}
    fn inner_close(&mut self, _i: usize) {}
    fn inner_new(&mut self) {}
    /// The window draws `inner_tabs` in its title strip (so don't draw them in the body).
    fn set_strip_external(&mut self, _on: bool) {}
    /// The pointer moved over the body with no button down (or left it: (-1, -1)). True if
    /// the body needs redrawing (a hover state changed).
    fn hover(&mut self, _x: i32, _y: i32) -> bool {
        false
    }
    /// A drag began at body (x, y): the path of the file being dragged there, if any
    /// (Files). The desktop takes the drag over from then on.
    fn drag_start(&mut self, _x: i32, _y: i32) -> Option<alloc::string::String> {
        None
    }
    /// A file was dropped on the window. True if it did something with it.
    fn drop_path(&mut self, _path: &str) -> bool {
        false
    }
    /// A part of the body that takes a dragged file even when the drag started in this
    /// window (Files' NEARBY devices): its rect in body coordinates.
    fn drop_zone(&self, _x: i32, _y: i32, _path: &str) -> Option<Rect> {
        None
    }
    /// A file dropped at body (x, y): the drop zone's turn first, then [`drop_path`].
    fn drop_at(&mut self, _x: i32, _y: i32, path: &str) -> bool {
        self.drop_path(path)
    }
    /// A named command from the menu bar (File > Share..., ...). True if it did something.
    fn command(&mut self, _name: &str) -> bool {
        false
    }
    /// Commands this window takes now, as (label, name) for its File menu.
    fn commands(&self) -> Vec<(&'static str, &'static str)> {
        Vec::new()
    }
    /// The window is finished (a dialog that has its answer): the desktop closes it.
    fn wants_close(&self) -> bool {
        false
    }
    /// Cmd+W: close a part of the window (a tab) rather than the window. True if it did.
    fn close_part(&mut self) -> bool {
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

/// The hit under `at` (hover states): its index in `hits`.
pub fn hover_index<T>(hits: &[(Rect, T)], at: Option<(i32, i32)>) -> Option<usize> {
    let (x, y) = at?;
    hits.iter().position(|(r, _)| super::paint::hit(r, x, y))
}

/// Move the hover point; true if it now lands on a different hit (redraw).
pub fn hover_move<T>(hits: &[(Rect, T)], at: &mut Option<(i32, i32)>, x: i32, y: i32) -> bool {
    let new = (x >= 0).then_some((x, y));
    let changed = hover_index(hits, *at) != hover_index(hits, new);
    *at = new;
    changed
}

/// SPEC hover: a button lightens (white, 6%); a list row gets a faint strawberry tint.
pub fn paint_hover(p: &mut Painter, r: Rect, row: bool) {
    if row {
        p.rrect_r(r, R_ROW, STRAW, 0x22);
    } else {
        p.rrect_r(r, R_BUTTON, WHITE, a(70));
    }
}

/// Which built-in app a window belongs to (dock dots, menu bar name, single instance).
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum App {
    Files,
    Terminal,
    Notes,
    About,
    Settings,
    Browser,
    Music,
    Versions,
    /// The trusted Open / Save panel (powerbox.rs).
    Powerbox,
    Welcome,
    /// Milk Drop: receiving files from another milkOS (apps/milkdrop.rs).
    Drop,
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
            App::Settings => "Settings",
            App::Browser => "Browser",
            App::Music => "Music",
            App::Versions => "Versions",
            App::Powerbox => "Open",
            App::Welcome => "Welcome",
            App::Drop => "Milk Drop",
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
    /// A tab in the strip, its close x, the + after the tabs.
    Tab(usize),
    TabClose(usize),
    TabNew,
}

/// SPEC Tabs: the strip replaces the 32px title bar with a 38px one.
pub const STRIP_H: i32 = 38;
const TAB_Y: i32 = 6;
const TAB_H: i32 = 32;
const TAB_X: i32 = 84;
const TAB_GAP: i32 = 6;
const TAB_MIN: i32 = 96;
const TAB_MAX: i32 = 160;
const TAB_R: i32 = 10;
const PLUS_W: i32 = 28;
const PLUS_H: i32 = 24;
/// Terminal strip colours (SPEC: the same in both themes).
pub const T_BAR: Color = TERM_BAR;
pub const T_SECOND: Color = crate::gfx::rgb(0xBE, 0xA8, 0x90);
pub const T_HOVER: Color = crate::gfx::rgb(0x55, 0x46, 0x3C);

/// What the strip shows.
pub struct Strip {
    pub titles: Vec<alloc::string::String>,
    pub active: usize,
    /// Terminal colours (a Terminal's own tabs).
    pub terminal: bool,
    /// The tabs are the contents' own (inner_*), not window tabs.
    pub inner: bool,
}

/// Tab `i` of `n` in a strip `w` wide: its rect (window coordinates).
pub fn tab_rect(i: usize, n: usize, w: i32, x0: i32) -> Rect {
    let n = n.max(1) as i32;
    let room = w - x0 - 8 - PLUS_W - 4;
    let tw = ((room - (n - 1) * TAB_GAP) / n).clamp(TAB_MIN, TAB_MAX);
    Rect::new(x0 + i as i32 * (tw + TAB_GAP), TAB_Y, tw, TAB_H)
}

/// A tab's close x hit box (20x20, right-aligned with 8px padding).
pub fn tab_close_rect(t: Rect) -> Rect {
    Rect::new(t.x + t.w - 8 - 20 + 4, t.y + (t.h - 20) / 2, 20, 20)
}

/// The + after the last of `n` tabs.
pub fn plus_rect(n: usize, w: i32, x0: i32) -> Rect {
    let last = tab_rect(n.saturating_sub(1), n, w, x0);
    Rect::new(
        last.x + last.w + 4,
        TAB_Y + (TAB_H - PLUS_H) / 2,
        PLUS_W,
        PLUS_H,
    )
}

/// `s` cut with "~" to fit `px` pixels of proportional text.
pub fn fit(s: &str, px: i32, bold: bool) -> alloc::string::String {
    if super::fontx::prop_width(s, bold) <= px {
        return alloc::string::String::from(s);
    }
    let mut out = alloc::string::String::new();
    for c in s.chars() {
        out.push(c);
        if super::fontx::prop_width(&out, bold) + 8 > px {
            out.pop();
            break;
        }
    }
    out.push('~');
    out
}

/// Draw a SPEC tab strip at the top of `p` (`w` wide, starting at x0). `hover` / `close_hover`
/// are tab indices; `active_fill` is the active tab's fill (the window body colour);
/// `active_text` its title colour.
#[allow(clippy::too_many_arguments)]
pub fn paint_strip(
    p: &mut Painter,
    s: &Strip,
    w: i32,
    x0: i32,
    hover: Option<usize>,
    close_hover: Option<usize>,
    active_fill: Color,
    active_text: Color,
    plus: bool,
) {
    let n = s.titles.len();
    let (second, hov, edge) = if s.terminal {
        (T_SECOND, T_HOVER, T_BAR)
    } else {
        (COCOA, OAT, SAND)
    };
    for (i, t) in s.titles.iter().enumerate() {
        let r = tab_rect(i, n, w, x0);
        let active = i == s.active;
        if active {
            // Body fill, sand border on top / left / right, open at the bottom.
            p.rrect(r.x, r.y, r.w, r.h + TAB_R, TAB_R, edge, 255);
            p.rrect(
                r.x + 1,
                r.y + 1,
                r.w - 2,
                r.h + TAB_R,
                TAB_R - 1,
                active_fill,
                255,
            );
        } else if hover == Some(i) {
            p.rrect(r.x, r.y, r.w, r.h + TAB_R, TAB_R, hov, a(600));
        }
        // The x takes its colours from the tab it sits on: a Terminal-coloured active tab
        // in a light window still gets the Terminal's x (light cocoa there is 1.94:1).
        let dark_tab = s.terminal || (active && active_fill == TERM_BG);
        let (x_rest, x_circle, x_hover) = if dark_tab {
            (T_SECOND, T_HOVER, TERM_FG)
        } else {
            (second, SAND, ESPRESSO)
        };
        let fg = if active { active_text } else { second };
        let room = r.w - 12 - 8 - 20 - 4;
        let label = fit(t, room, active);
        p.text_p(r.x + 12, r.y + 8, &label, fg, active);
        // The close x, at rest too; hover: a circle behind it, the x in primary.
        let c = tab_close_rect(r);
        let xc = if close_hover == Some(i) {
            p.circle(c.x, c.y, 20, x_circle, 255);
            x_hover
        } else {
            x_rest
        };
        p.text_p_center(c.x, c.w, c.y + 2, "x", xc, false);
    }
    if plus {
        let r = plus_rect(n, w, x0);
        let (bg, fg) = if s.terminal {
            (T_HOVER, T_SECOND)
        } else {
            (OAT, ESPRESSO)
        };
        p.rrect_r(r, 8, bg, 255);
        p.fill(r.x + 9, r.y + 11, 10, 2, fg);
        p.fill(r.x + 13, r.y + 7, 2, 10, fg);
    }
}

/// SPEC drop target: where a dragged window would land as a tab.
/// The drop target is at least this wide, so "Drop to add as a tab" fits.
pub const DROP_MIN_W: i32 = 176;

pub fn paint_drop_target(p: &mut Painter, r: Rect) {
    p.rrect_r(r, TAB_R, STRAW, a(200));
    // A 2px dashed border: dashes of 6, gaps of 4, along each side.
    let mut x = r.x + TAB_R;
    while x < r.x + r.w - TAB_R {
        p.fill(x, r.y, 6.min(r.x + r.w - TAB_R - x), 2, STRAW);
        p.fill(x, r.y + r.h - 2, 6.min(r.x + r.w - TAB_R - x), 2, STRAW);
        x += 10;
    }
    let mut y = r.y + TAB_R;
    while y < r.y + r.h - TAB_R {
        p.fill(r.x, y, 2, 6.min(r.y + r.h - TAB_R - y), STRAW);
        p.fill(r.x + r.w - 2, y, 2, 6.min(r.y + r.h - TAB_R - y), STRAW);
        y += 10;
    }
    p.rrect_stroke(r.x, r.y, r.w, r.h, TAB_R, 1, STRAW, a(500));
    // Primary text: straw on the straw tint is 1.96:1; the dashed border carries the colour.
    let label = fit("Drop to add as a tab", r.w - 16, true);
    p.text_p(r.x + 8, r.y + (r.h - 16) / 2, &label, ESPRESSO, true);
}

/// A tab parked behind the active one.
pub struct Parked {
    pub app: App,
    pub contents: Box<dyn WindowContents>,
}

/// A stand-in while a tab's contents are being moved.
struct Blank;

impl WindowContents for Blank {
    fn title(&self) -> &str {
        ""
    }
    fn draw(&mut self, _p: &mut Painter, _w: i32, _h: i32, _focused: bool) {}
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
    /// Traffic light being pressed (drawn 4% darker).
    pub pressed_light: Option<usize>,
    /// Tabs everywhere: the other tabs (the active one is `app` / `contents`), and where the
    /// active one sits among them.
    pub parked: Vec<Parked>,
    pub active: usize,
    /// Title bar height now: 32, or 38 with a tab strip.
    pub bar_h: i32,
    /// Tab under the pointer, close x under the pointer (strip hover states).
    pub tab_hover: Option<usize>,
    pub close_hover: Option<usize>,
    /// Another window is dragged over this one's title bar: show where it would land.
    pub drop_hint: bool,
    /// Being dragged onto another window's strip: drawn at 92% with a straw edge.
    pub ghost: bool,
}

/// Light `i`'s box in window coordinates, in a title bar `bar_h` high (SPEC: y10 in the
/// 32px bar, y12 in the 38px strip).
pub fn light_rect(i: usize, bar_h: i32) -> Rect {
    Rect::new(
        LIGHT_X + i as i32 * (LIGHT_D + LIGHT_GAP),
        LIGHT_Y + (bar_h - TITLE_H) / 3,
        LIGHT_D,
        LIGHT_D,
    )
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
            pressed_light: None,
            parked: Vec::new(),
            active: 0,
            bar_h: TITLE_H,
            tab_hover: None,
            close_hover: None,
            drop_hint: false,
            ghost: false,
        })
    }

    /// Body rect in window coordinates: inside the border, under the title bar and its line.
    /// The smallest body any of this window's tabs needs: (w, h).
    pub fn min_body(&self) -> (i32, i32) {
        core::iter::once(&self.contents)
            .chain(self.parked.iter().map(|t| &t.contents))
            .filter_map(|c| c.min_body())
            .fold((0, 0), |(w, h), (cw, ch)| (w.max(cw), h.max(ch)))
    }

    pub fn body(&self) -> Rect {
        Rect::new(
            1,
            self.bar_h + 1,
            self.rect.w - 2,
            self.rect.h - self.bar_h - 2,
        )
    }

    pub fn tab_count(&self) -> usize {
        self.parked.len() + 1
    }

    /// The window's tabs' titles, in order.
    pub fn tab_titles(&self) -> Vec<alloc::string::String> {
        let mut v: Vec<alloc::string::String> = self
            .parked
            .iter()
            .map(|t| alloc::string::String::from(t.contents.title()))
            .collect();
        v.insert(
            self.active,
            alloc::string::String::from(self.contents.title()),
        );
        v
    }

    /// The app of tab `i`.
    pub fn tab_app(&self, i: usize) -> Option<App> {
        if i == self.active {
            return Some(self.app);
        }
        let k = if i < self.active { i } else { i - 1 };
        self.parked.get(k).map(|t| t.app)
    }

    /// Does any tab of this window belong to `app`? Which one?
    pub fn tab_of(&self, app: App) -> Option<usize> {
        (0..self.tab_count()).find(|&i| self.tab_app(i) == Some(app))
    }

    /// Make tab `i` the active one.
    pub fn switch_to(&mut self, i: usize) {
        if i == self.active || i >= self.tab_count() {
            return;
        }
        let k = if i < self.active { i } else { i - 1 };
        let t = self.parked.remove(k);
        let old = Parked {
            app: core::mem::replace(&mut self.app, t.app),
            contents: core::mem::replace(&mut self.contents, t.contents),
        };
        // The old active goes back to its place in the order.
        let back = if self.active > i {
            self.active - 1
        } else {
            self.active
        };
        self.parked.insert(back.min(self.parked.len()), old);
        self.active = i;
        self.dirty = true;
    }

    /// Add a tab at the end; it becomes the active one.
    pub fn add_tab(&mut self, app: App, contents: Box<dyn WindowContents>) {
        let old = Parked {
            app: core::mem::replace(&mut self.app, app),
            contents: core::mem::replace(&mut self.contents, contents),
        };
        self.parked.insert(self.active.min(self.parked.len()), old);
        self.active = self.parked.len();
        self.dirty = true;
    }

    /// Take tab `i` out (the window keeps at least one tab: None if it has only one).
    pub fn take_tab(&mut self, i: usize) -> Option<Parked> {
        if self.tab_count() < 2 || i >= self.tab_count() {
            return None;
        }
        if i == self.active {
            // A neighbour becomes active first.
            let next = if i + 1 < self.tab_count() {
                i + 1
            } else {
                i - 1
            };
            self.switch_to(next);
        }
        let k = if i < self.active { i } else { i - 1 };
        let t = self.parked.remove(k);
        if i < self.active {
            self.active -= 1;
        }
        self.dirty = true;
        Some(t)
    }

    /// Every tab, active one included, in order (the window is left with a blank).
    pub fn take_all(&mut self) -> Vec<Parked> {
        let mut v: Vec<Parked> = core::mem::take(&mut self.parked);
        let me = Parked {
            app: self.app,
            contents: core::mem::replace(&mut self.contents, Box::new(Blank)),
        };
        v.insert(self.active.min(v.len()), me);
        v
    }

    /// The strip to draw, if any: window tabs (2+), else the contents' own tabs (2+).
    pub fn strip(&self) -> Option<Strip> {
        if self.tab_count() >= 2 {
            return Some(Strip {
                titles: self.tab_titles(),
                active: self.active,
                terminal: false,
                inner: false,
            });
        }
        self.contents.inner_tabs().map(|(titles, active)| Strip {
            titles,
            active,
            terminal: self.contents.dark(),
            inner: true,
        })
    }

    /// The bar height this window should have now.
    pub fn want_bar(&self) -> i32 {
        if self.strip().is_some() {
            STRIP_H
        } else {
            TITLE_H
        }
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
            if super::paint::hit(&grow(&light_rect(i, self.bar_h), 3), x, y) {
                return Some(Part::Light(i));
            }
        }
        if x >= self.rect.w - 16 && y >= self.rect.h - 16 {
            return Some(Part::Resize);
        }
        if y < self.bar_h + 1 {
            if let Some(s) = self.strip() {
                let n = s.titles.len();
                for i in 0..n {
                    let r = tab_rect(i, n, self.rect.w, TAB_X);
                    if super::paint::hit(&tab_close_rect(r), x, y) {
                        return Some(Part::TabClose(i));
                    }
                    if super::paint::hit(&r, x, y) {
                        return Some(Part::Tab(i));
                    }
                }
                if super::paint::hit(&plus_rect(n, self.rect.w, TAB_X), x, y) {
                    return Some(Part::TabNew);
                }
            }
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
        let strip = self.strip();
        let title = alloc::string::String::from(self.contents.title());
        let br = self.body();
        let mut p = Painter::new(&mut self.surf);
        let body = self.contents.background();
        p.fill(0, 0, w, h, body);
        // Title bar and the line under it.
        let (bar, line, edge) = if dark {
            (TERM_BAR, TERM_BAR, TERM_BAR)
        } else {
            (CREAM, SAND, SAND)
        };
        // A window strip over a Terminal tab still uses the window's colours; only a
        // Terminal's own strip is in Terminal colours.
        let (bar, line, edge) = match &strip {
            Some(s) if !s.inner => (CREAM, SAND, if dark { TERM_BAR } else { SAND }),
            _ => (bar, line, edge),
        };
        let bh = self.bar_h;
        p.fill(0, 0, w, bh, bar);
        p.fill(0, bh, w, 1, line);
        // Traffic lights.
        let lights = if focused {
            [STRAW, HONEY, MATCHA]
        } else {
            [LATTE, LATTE, LATTE]
        };
        for (i, c) in lights.into_iter().enumerate() {
            let r = light_rect(i, bh);
            p.circle(r.x, r.y, r.w, c, 255);
            if self.pressed_light == Some(i) {
                p.circle(r.x, r.y, r.w, 0, a(40) * 2); // pressed: darker (8%, 4% reads as nothing on a 12px light)
            }
            if self.hover {
                // x, -, + (hover state; the mockup does not show one, this is macOS's).
                let (x, y, m) = (r.x, r.y, INK);
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
            (true, true) => TERM_FG,
            (true, false) => LATTE,
            (false, true) => ESPRESSO,
            (false, false) => COCOA,
        };
        let title = title.as_str();
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
        match &strip {
            Some(s) => {
                let (fill, text) = if dark {
                    (TERM_BG, TERM_FG)
                } else {
                    (body, ESPRESSO)
                };
                paint_strip(
                    &mut p,
                    s,
                    w,
                    TAB_X,
                    self.tab_hover,
                    self.close_hover,
                    fill,
                    text,
                    true,
                );
            }
            // The drop target takes the title's place while a window hovers here.
            None if self.drop_hint => {}
            None => p.text_p_center(0, w, 9, t, tc, true),
        }
        if self.drop_hint {
            let n = strip.as_ref().map_or(1, |s| s.titles.len());
            let r = if strip.is_some() {
                tab_rect(n, n + 1, w, TAB_X)
            } else {
                Rect::new(TAB_X, 3, TAB_MAX + 40, bh - 6)
            };
            // At least 176 wide so the label fits (SPEC Tabs), inside the window.
            let r = Rect::new(r.x, r.y, r.w.max(DROP_MIN_W).min(w - r.x - 8), r.h);
            paint_drop_target(&mut p, r);
        }
        self.contents
            .set_strip_external(strip.as_ref().is_some_and(|s| s.inner));
        // Body.
        {
            let mut bp = p.sub(br);
            self.contents.draw(&mut bp, br.w, br.h, focused);
        }
        // Border last, over everything.
        p.rrect_stroke(0, 0, w, h, R_WINDOW, 1, edge, 255);
        true
    }
}
