//! The app-facing interface of the web engine. The Browser app builds against this file only;
//! everything else in the crate is internal. This shape is kept stable: anything added later is
//! added beside what is here, and a change to an existing item is noted in the CHANGES list
//! below with what to do instead.
//!
//! # How a page goes through the engine
//!
//! ```text
//!   bytes ──Document::parse──▶ Document ──pending_resources()──▶ app fetches
//!                                 ▲                                   │
//!                                 └──supply_stylesheet / supply_image─┘
//!                                 │   (or load_resources(&mut Fetcher) to do it all at once)
//!                                 ▼
//!              layout(&Viewport, &dyn TextMeasurer) ──▶ &DisplayList ──▶ app paints
//!                                 │
//!        hit_test / find / controls / submit / fragment_y / main_content
//! ```
//!
//! - Coordinates: everything the engine hands out (display list, hit-test input, find results,
//!   control rects) is in device pixels, document space: (0, 0) is the top-left corner of the
//!   page, before scrolling. The app subtracts its scroll offset when it paints. The one
//!   exception is [`DisplayList::fixed`] (position: fixed), whose items are in viewport space.
//! - Zoom: [`Viewport::zoom`] is a percentage. A CSS pixel is `zoom / 100` device pixels, media
//!   queries see `width * 100 / zoom` CSS pixels, and fonts are asked for at their zoomed size,
//!   so text stays crisp (browser-style full-page zoom, not a bitmap scale).
//! - Colours are `0xAARRGGBB`, straight alpha, like `imgdec`.
//! - Nothing here panics on hostile input, and the work is bounded: a page that is too big or
//!   too deep is cut short (see [`Limits`]), never crashes.
//! - Text: the engine breaks lines and positions words using the app's [`TextMeasurer`], and
//!   emits [`TextRun`]s the app draws with the same font, so measuring and drawing agree.
//!   Underlines, overlines and line-throughs are already in the list as [`Item::Rect`]s.
//! - Threads: parsing and layout recurse with the document's nesting (bounded by
//!   [`Limits::max_depth`]); run them on the main thread (8 MB stack) or a thread with at
//!   least 1 MB of stack, not a default 128 KB rt::thread.
//! - Scripts: a page parsed with [`Document::parse_scripted`] runs its JavaScript (see the
//!   "Scripts" section at the end): the app drives it with [`Document::run`] and passes the
//!   user's clicks, keys and focus through [`Document::click`] and friends. After any call
//!   that runs script the DOM may have changed: [`Document::layout`] restyles only the
//!   changed subtrees and lays out again, and [`ControlId`]s may have moved.
//! - Not supported (yet): `:hover`/`:focus` styles, CSS transforms, animations, web fonts,
//!   named grid lines, inner scrolling boxes (overflow: auto spills sideways and clips
//!   vertically), bidirectional text, vertical writing modes; for scripts: modules, fetch /
//!   XMLHttpRequest, canvas, workers, iframes' documents.
//!
//! # CHANGES
//! - Network requests from scripts (`fetch`, `XMLHttpRequest`, `navigator.sendBeacon`): the
//!   engine never does I/O. A script's request comes out of [`Document::take_net`] as a
//!   [`NetOut::Start`] ([`NetRequest`]); the app runs it (the same-origin policy, CORS,
//!   cookies and limits are the app's rules, not the engine's) and gives the outcome back
//!   with [`Document::net_event`] ([`NetEvent`]: the head, progress, the body, or a
//!   failure), each call being one task of the page's event loop. [`NetOut::Cancel`] says a
//!   request was aborted or its page no longer wants it. [`Document::net_pending`] counts the
//!   requests still outstanding (the tab's "loading" light). Added beside what was there.
//! - Scripts: [`Document::parse_scripted`], [`ScriptHost`], [`Storage`], [`ScriptLimits`],
//!   [`PageRequest`], [`ConsoleLine`], [`KeyInput`], [`Mods`], [`ScriptStats`] and the
//!   Document calls in the "Scripts" section. [`Document::parse`] and friends still parse
//!   with scripts off.
//! - [`ControlId`]s can change when a script adds or removes controls: keep the focus in the
//!   engine ([`Document::focused`], [`Document::set_focus`]) and look ids up again after
//!   any call that runs script. A form's entries are in tree order (not node order).
//! - [`Document::submit_form`] checks the form's constraints (required, type, length,
//!   range; pattern with scripts) and fires `submit` before giving the submission; a form
//!   that fails says why with [`PageRequest::Invalid`]. [`Document::submit`] stays as it was.
//! - [`Document::click`] toggles checkboxes and radio buttons itself (with scripts or
//!   without): don't also call [`Document::set_checked`] for a click.
//! - Added [`FormControl::content_drawn`] (true for `<button>`: the engine paints its
//!   content, the app only the face). Documented that [`Item::Control`] and
//!   [`FormControl::rect`] are the control's content box: its CSS background and borders are
//!   already in the display list.
//! - [`Font`] is `Ord` (usable as a cache key).
//! - Added [`Stats::layout_bytes`]; [`Stats::approx_bytes`] now counts only what is kept.
//! - Documented that text decorations are drawn by the engine (see [`TextRun::decoration`]).
//! - The UA defaults are milkOS's: a #FFFDF8 canvas, #3A2E28 text, #4F64A0 links (no pure white
//!   or black); [`Viewport::dark`] can give a page the dark defaults (see there).
//!   `color-scheme` is a property now.
//! - The default family (initial value, and where an unknown `font-family` list ends up) is
//!   [`Family::SansSerif`], not serif.

use alloc::string::String;
use alloc::vec::Vec;

/// `0xAARRGGBB`, straight alpha.
pub type Color = u32;

/// A rectangle in device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
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
    pub fn right(&self) -> i32 {
        self.x.saturating_add(self.w)
    }
    pub fn bottom(&self) -> i32 {
        self.y.saturating_add(self.h)
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    /// The overlap of two rectangles (empty, w = h = 0, when they don't overlap).
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        if x1 <= x0 || y1 <= y0 {
            Rect::default()
        } else {
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.intersect(o).is_empty()
    }
}

// ---- Fonts and text measuring -------------------------------------------------------------

/// The generic family a font resolves to. The engine maps named families it knows
/// (Helvetica, Arial, Georgia, Times, Courier, Menlo, ...) onto these; unknown names fall
/// through the `font-family` list to the next one, and finally to the document default,
/// [`Family::SansSerif`] (milkOS's: the desktop's UI font, where classic browsers use serif).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    Serif,
    SansSerif,
    Monospace,
    Cursive,
    Fantasy,
    /// `system-ui`, `-apple-system`, `BlinkMacSystemFont`, `Segoe UI` and the like.
    SystemUi,
}

/// One font as the page asks for it, at device size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Font {
    pub family: Family,
    /// The em size in device pixels (CSS size times zoom), at least 1.
    pub size: u16,
    /// CSS weight, 100 to 900 (400 normal, 700 bold).
    pub weight: u16,
    pub italic: bool,
}

/// Vertical metrics of a [`Font`], device pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct FontMetrics {
    /// From the baseline up to the top of the tallest glyphs.
    pub ascent: i32,
    /// From the baseline down (positive).
    pub descent: i32,
    /// The height of a line at `line-height: normal`.
    pub line_height: i32,
}

/// How the app measures text. Must be deterministic: the same font and text give the same
/// width every time (the layout caches widths).
pub trait TextMeasurer {
    /// The advance width of `text` set in `font`, in device pixels. `text` never contains a
    /// line break or a tab (the engine expands those).
    fn text_width(&self, font: &Font, text: &str) -> i32;
    fn metrics(&self, font: &Font) -> FontMetrics;
}

/// A measurer that needs no font data: proportional widths from a fixed per-character table
/// (Helvetica-like for sans and serif, every character 0.6 em for monospace), ascent 0.8 em,
/// descent 0.2 em, line height 1.2 em. The golden tests use it; the app can use it before its
/// real fonts are wired up.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApproxMeasurer;

// ---- Viewport -----------------------------------------------------------------------------

/// What the page is laid out into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    /// Device pixels.
    pub width: i32,
    pub height: i32,
    /// Percent: 100 is 1:1. Clamped to 25..=500.
    pub zoom: u16,
    /// The desktop is dark: `prefers-color-scheme: dark` matches, and a page that opts into a
    /// dark scheme (`color-scheme`, `<meta name=color-scheme>`) or sets no colours or styles at
    /// all gets the dark UA defaults (canvas #2E2520, text #F4EBE0, links #A9B8E8). A page that
    /// sets some colours but no scheme is never darkened.
    pub dark: bool,
}

impl Viewport {
    pub fn new(width: i32, height: i32) -> Viewport {
        Viewport {
            width,
            height,
            zoom: 100,
            dark: false,
        }
    }
}

// ---- Resources (stylesheets, images) ------------------------------------------------------

/// A handle for one image URL in one document (the same URL used twice is one image).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    /// `<link rel=stylesheet>` or `@import`; supply the bytes with
    /// [`Document::supply_stylesheet`].
    Stylesheet,
    /// `<img src>`, `<input type=image>`, or a CSS `background-image`; decode it and supply its
    /// intrinsic size with [`Document::supply_image`]. Keep the pixels on the app side, keyed
    /// by the [`ImageId`].
    Image(ImageId),
}

/// Something the page needs fetched. `url` is absolute (resolved against the base URL).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub kind: ResourceKind,
    pub url: String,
}

/// The synchronous way to load resources: [`Document::load_resources`] calls these for each
/// pending resource. Return `None` for a failure (network error, 404, undecodable image).
pub trait Fetcher {
    /// The body of a stylesheet (any charset label is applied by the engine; UTF-8 assumed).
    fn stylesheet(&mut self, url: &str) -> Option<Vec<u8>>;
    /// Fetch and decode an image, keep its pixels under `id`, return its intrinsic size in
    /// image pixels.
    fn image(&mut self, url: &str, id: ImageId) -> Option<(u32, u32)>;
}

// ---- The display list ---------------------------------------------------------------------

/// Text decoration lines, a bit set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Decoration(pub u8);

impl Decoration {
    pub const NONE: Decoration = Decoration(0);
    pub const UNDERLINE: Decoration = Decoration(1);
    pub const OVERLINE: Decoration = Decoration(2);
    pub const LINE_THROUGH: Decoration = Decoration(4);
    pub fn has(self, d: Decoration) -> bool {
        self.0 & d.0 != 0
    }
}

/// One run of text on one line, all in one font and colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRun {
    /// The pen position: `x` is where the first glyph starts, `baseline` the baseline's y.
    pub x: i32,
    pub baseline: i32,
    /// The run's measured width (what [`TextMeasurer::text_width`] said).
    pub width: i32,
    pub font: Font,
    pub color: Color,
    /// The decoration lines in effect, for information: the engine has already put them in
    /// the display list as [`Item::Rect`]s right after this run, so don't draw them again.
    pub decoration: Decoration,
    /// Where the text is in [`DisplayList::text`] (byte offsets; use [`DisplayList::run_text`]).
    pub start: u32,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BorderStyle {
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
    /// groove, ridge, inset and outset are drawn as solid in a darker/lighter shade (already
    /// applied to the colours here).
    Shaded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    Disc,
    Circle,
    Square,
}

/// A form control's box; the app draws it with its own widgets, using the state in
/// [`Document::controls`] (index [`ControlId`]). The rect is the control's content box: its
/// CSS background and borders (the UA gives fields an inset border and buttons an outset one)
/// are already painted by the entries before it, so the app draws only what goes inside: the
/// value or placeholder and caret of a text field, the label of an `<input type=submit>`, the
/// check of a checkbox, the selected option and arrow of a `<select>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ControlId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// A filled rectangle (backgrounds, horizontal rules).
    Rect {
        rect: Rect,
        color: Color,
    },
    /// The four sides of a border box. `widths` and `colors` are top, right, bottom, left.
    /// `radius` is a uniform corner radius (0 = square), already clamped to half the size.
    Border {
        rect: Rect,
        widths: [i32; 4],
        colors: [Color; 4],
        styles: [BorderStyle; 4],
        radius: i32,
    },
    /// A filled rounded rectangle (a background with border-radius).
    RoundRect {
        rect: Rect,
        color: Color,
        radius: i32,
    },
    Text(TextRun),
    /// An image scaled into `rect`.
    Image {
        rect: Rect,
        image: ImageId,
    },
    /// A background image tiled over `area`: one tile is `tile` (its origin and its size after
    /// background-size), repeated in x and/or y from that origin, clipped to `area`.
    Tiled {
        area: Rect,
        tile: Rect,
        image: ImageId,
        repeat_x: bool,
        repeat_y: bool,
    },
    /// A list bullet in `rect` (numbered lists use a [`Item::Text`] instead).
    Marker {
        rect: Rect,
        kind: MarkerKind,
        color: Color,
    },
    /// A form control (see [`ControlId`]).
    Control {
        rect: Rect,
        control: ControlId,
    },
}

/// A display item and the clip it is painted under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub item: Item,
    /// Index into [`DisplayList::clips`]; 0 is "no clip" (clips\[0\] is the whole page).
    pub clip: u16,
}

/// What to paint, back to front. Every entry carries its own clip, so any subset painted in
/// order (for example only the ones [`DisplayList::visible`] returns) paints correctly.
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    /// The canvas colour behind everything (the root or body background, white by default).
    pub background: Color,
    /// Document space, scrolls with the page.
    pub entries: Vec<Entry>,
    /// position: fixed boxes, viewport space, painted after (over) `entries`.
    pub fixed: Vec<Entry>,
    /// Clip rectangles, document space for `entries` and viewport space for `fixed`.
    pub clips: Vec<Rect>,
    /// All the text of all runs; [`TextRun::start`]/[`TextRun::len`] index it.
    pub text: String,
    /// The size of the whole document (scrollable area), device pixels.
    pub width: i32,
    pub height: i32,
    /// For [`DisplayList::visible`]: per band of [`BAND`] pixels, the entries touching it.
    pub(crate) bands: Vec<Vec<u32>>,
}

/// The height of one culling band of [`DisplayList::visible`], device pixels.
pub const BAND: i32 = 512;

impl DisplayList {
    pub fn run_text(&self, r: &TextRun) -> &str {
        let s = r.start as usize;
        self.text.get(s..s + r.len as usize).unwrap_or("")
    }
    /// The entries of `entries` that may touch `area` (document space), in paint order. Uses
    /// a band index built at layout, so it is cheap on long pages.
    pub fn visible(&self, area: Rect) -> Vec<&Entry> {
        let mut out = Vec::new();
        if area.is_empty() || self.bands.is_empty() {
            for e in &self.entries {
                if entry_rect(&e.item).intersects(&area) {
                    out.push(e);
                }
            }
            return out;
        }
        let b0 = (area.y.max(0) / BAND) as usize;
        let b1 = ((area.bottom().max(0) - 1).max(0) / BAND) as usize;
        let mut idx: Vec<u32> = Vec::new();
        for band in self
            .bands
            .iter()
            .take(b1.min(self.bands.len() - 1) + 1)
            .skip(b0)
        {
            idx.extend_from_slice(band);
        }
        idx.sort_unstable();
        idx.dedup();
        for i in idx {
            let e = &self.entries[i as usize];
            if entry_rect(&e.item).intersects(&area) {
                out.push(e);
            }
        }
        out
    }
}

/// The rectangle an item paints into (for culling).
pub fn entry_rect(i: &Item) -> Rect {
    match *i {
        Item::Rect { rect, .. }
        | Item::Border { rect, .. }
        | Item::RoundRect { rect, .. }
        | Item::Image { rect, .. }
        | Item::Marker { rect, .. }
        | Item::Control { rect, .. } => rect,
        Item::Tiled { area, .. } => area,
        Item::Text(t) => {
            let h = t.font.size as i32;
            // Generous: ascent and descent are within 1.25 em of the baseline for any font.
            Rect::new(t.x, t.baseline - h - h / 4, t.width.max(1), h * 2)
        }
    }
}

// ---- Hit testing --------------------------------------------------------------------------

/// A link under the pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Absolute URL (resolved against the base URL). `javascript:` links are reported with
    /// their href as written; the app should ignore them.
    pub url: String,
    /// The `target` attribute (`_blank` means "a new tab"), if any.
    pub target: Option<String>,
    /// `download` attribute present.
    pub download: bool,
}

/// What is at a point.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hit {
    pub link: Option<Link>,
    pub control: Option<ControlId>,
    /// The `title` attribute of the nearest element that has one (a tooltip).
    pub title: Option<String>,
    /// The pointer is over text (an I-beam cursor would fit).
    pub text: bool,
}

// ---- Find in page -------------------------------------------------------------------------

/// One occurrence of the search text. A match that wraps across lines has several rects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindMatch {
    pub rects: Vec<Rect>,
}

// ---- Forms --------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    /// text, search, email, url, tel, number, date and other one-line types
    Text,
    Password,
    Checkbox,
    Radio,
    /// `<input type=submit>`, `<button>` (type submit, the default)
    Submit,
    Reset,
    /// `<input type=button>`, `<button type=button>`: does nothing without scripts
    Button,
    /// `<input type=image>`: a submit button drawn as its image (see [`FormControl::image`])
    ImageSubmit,
    /// `<input type=file>`: shown, not submittable yet
    File,
    /// `<select>` (`multiple` in [`FormControl::multiple`])
    Select,
    TextArea,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOption {
    pub label: String,
    pub value: String,
    pub selected: bool,
    pub disabled: bool,
}

/// A form control's state. `value` is what the user has typed (or the `value` attribute at
/// first); the app changes it with [`Document::set_value`] and friends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormControl {
    pub id: ControlId,
    pub kind: ControlKind,
    /// The `type` attribute as written, lower-cased ("email", "search", ...), for keyboards.
    pub input_type: String,
    pub name: String,
    pub value: String,
    pub checked: bool,
    pub disabled: bool,
    pub readonly: bool,
    pub required: bool,
    pub placeholder: String,
    /// What a button shows (its value or its text content).
    pub label: String,
    pub options: Vec<SelectOption>,
    pub multiple: bool,
    /// `maxlength`, if given.
    pub max_length: Option<u32>,
    /// `<textarea rows cols>`, `<select size>` (0 when absent).
    pub rows: u32,
    pub cols: u32,
    /// The image of an [`ControlKind::ImageSubmit`].
    pub image: Option<ImageId>,
    /// Its content box after the last layout (document space, device px; see
    /// [`Item::Control`]). Empty if not laid out (hidden, display: none).
    pub rect: Rect,
    /// The engine paints this control's content (a `<button>` holds arbitrary HTML): the app
    /// should draw only a pressed/focus state, not a label.
    pub content_drawn: bool,
    /// The font its text should be drawn in.
    pub font: Font,
    pub color: Color,
    /// Which form it belongs to (index of the `<form>` in document order), if any.
    pub form: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// A form submission for the app to perform (HTML, "form submission algorithm", with the
/// entry list encoded as application/x-www-form-urlencoded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub method: Method,
    /// Absolute. For GET the query is already in it (replacing any query the action had).
    pub url: String,
    /// For POST: the urlencoded body. Empty for GET.
    pub body: Vec<u8>,
    /// For POST: "application/x-www-form-urlencoded".
    pub content_type: &'static str,
    /// The form's (or submitter's) `target`, if any.
    pub target: Option<String>,
}

// ---- Limits -------------------------------------------------------------------------------

/// The bounds that keep hostile pages in check. The defaults are generous for real pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of input parsed (more is ignored, and [`Document::truncated`] says so).
    pub max_input: usize,
    /// DOM nodes created.
    pub max_nodes: usize,
    /// Element nesting depth: deeper elements are attached at this depth.
    pub max_depth: usize,
    /// CSS rules kept over all stylesheets.
    pub max_rules: usize,
    /// Stylesheets loaded (including @import).
    pub max_stylesheets: usize,
    /// Images referenced.
    pub max_images: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_input: 16 << 20,
            max_nodes: 1_000_000,
            max_depth: 256,
            max_rules: 100_000,
            max_stylesheets: 64,
            max_images: 2_000,
        }
    }
}

/// How long each stage took and how big things got, for the app's debug line and the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub nodes: usize,
    pub elements: usize,
    pub rules: usize,
    pub boxes: usize,
    pub display_items: usize,
    /// Bytes the document keeps between layouts, roughly (DOM, styles, display list and its
    /// hit-test and find data).
    pub approx_bytes: usize,
    /// Bytes the last layout's box tree took while it existed (freed after painting).
    pub layout_bytes: usize,
}

// ---- Scripts --------------------------------------------------------------------------------

/// What a page's scripts reach outside the page, from the app. Every call that may run
/// script takes one. Nothing here may block for long except the dialogs (which are modal:
/// the app shows them and waits for the user) and [`ScriptHost::slow`].
pub trait ScriptHost {
    /// For layout a script forces (getBoundingClientRect, offsetHeight, ...).
    fn measurer(&self) -> &dyn TextMeasurer;
    /// The viewport the page is laid out in (innerWidth, media queries).
    fn viewport(&self) -> Viewport;
    /// The app's scroll offset, device pixels (scrollX / scrollY).
    fn scroll(&self) -> (i32, i32);
    /// A monotonic clock in milliseconds (timers, performance.now()).
    fn now(&self) -> f64;
    /// Milliseconds since 1970-01-01 UTC (Date.now()).
    fn time(&self) -> f64;
    /// The local time zone's offset from UTC in minutes (east positive).
    fn tz_offset(&self) -> i32 {
        0
    }
    /// navigator.userAgent.
    fn user_agent(&self) -> String {
        String::from("Mozilla/5.0 (milkOS) MilkBrowser/1.0")
    }
    /// `document.cookie`: the page's cookies as a Cookie header would carry them, without
    /// HttpOnly ones.
    fn cookies(&mut self) -> String {
        String::new()
    }
    /// `document.cookie = ...`: one Set-Cookie line (the app refuses HttpOnly ones).
    fn set_cookie(&mut self, _line: &str) {}
    /// The page's origin's localStorage (`session` false) or this tab's sessionStorage. None:
    /// storage is not available (the API then throws SecurityError, as for opaque origins).
    fn storage(&mut self, _session: bool) -> Option<&mut dyn Storage> {
        None
    }
    /// window.alert: show `msg` and wait for OK.
    fn alert(&mut self, _msg: &str) {}
    /// window.confirm: OK (true) or Cancel.
    fn confirm(&mut self, _msg: &str) -> bool {
        false
    }
    /// window.prompt: the text typed, or None for Cancel.
    fn prompt(&mut self, _msg: &str, _default: &str) -> Option<String> {
        None
    }
    /// A script has been running for `running_ms` without a break (asked about every 100 ms
    /// or so once past [`ScriptLimits::slow_ms`]). Return false to stop it: the page's
    /// scripts then end for good ("This page is slow — Stop it?").
    fn slow(&mut self, _running_ms: f64) -> bool {
        true
    }
}

/// A Web Storage area (localStorage or sessionStorage), kept by the app.
pub trait Storage {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The `i`th key (a stable order: insertion order).
    fn key(&self, i: usize) -> Option<String>;
    fn get(&self, key: &str) -> Option<String>;
    /// False if it would go over the quota (the script gets QuotaExceededError).
    fn set(&mut self, key: &str, value: &str) -> bool;
    fn remove(&mut self, key: &str);
    fn clear(&mut self);
}

/// The bounds a page's scripts run in. A page over them gets an uncatchable error, its
/// scripts stop, and the page stays as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptLimits {
    /// Bytes of script heap.
    pub memory: usize,
    /// A script running longer than this without returning makes the app hear about it
    /// ([`ScriptHost::slow`]).
    pub slow_ms: u32,
    /// Bytes of one script's source.
    pub max_source: usize,
}

impl Default for ScriptLimits {
    fn default() -> ScriptLimits {
        ScriptLimits {
            memory: 64 << 20,
            slow_ms: 2_000,
            max_source: 8 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

/// One line of a page's console (console.log and friends, uncaught errors, notes from the
/// engine such as "modules aren't supported yet").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleLine {
    pub level: ConsoleLevel,
    pub text: String,
}

/// Something a script asked of the app (see [`Document::take_requests`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageRequest {
    /// location.href = / assign / replace, a link a script clicked: go there (absolute URL).
    /// `replace`: no new history entry.
    Navigate { url: String, replace: bool },
    /// location.reload().
    Reload,
    /// history.back() / forward() / go(n).
    History(i32),
    /// history.pushState / replaceState: the address bar shows `url` (absolute, same
    /// origin), with a new history entry unless `replace`; no load.
    SetAddress { url: String, replace: bool },
    /// window.scrollTo, scrollIntoView, element.focus() of something out of view: scroll
    /// the view to this document position (device pixels).
    ScrollTo { x: i32, y: i32 },
    /// form.submit() / requestSubmit(): send this (as a submit button would).
    Submit(Submission),
    /// window.open(url): a new tab.
    Open(String),
    /// `document.title` changed.
    Title,
    /// A form wasn't sent: this control is invalid (the engine has focused it); show the
    /// message (constraint validation: required, type, pattern, ...).
    Invalid(String),
}

/// Keys as scripts see them (KeyboardEvent.key / code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    /// "a", "A", "Enter", "ArrowLeft", "Backspace", "Escape", "Tab", " " ...
    pub key: String,
    /// "KeyA", "Enter", "ArrowLeft", "Space" ... ("" if unknown).
    pub code: String,
    pub mods: Mods,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

/// How a page's scripts are doing, for the app's debug line and the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScriptStats {
    /// Scripts run (inline and external).
    pub scripts: u32,
    /// Uncaught errors.
    pub errors: u32,
    /// Event listener calls, timer and frame callbacks run.
    pub callbacks: u32,
    /// Script heap in use, bytes.
    pub heap_bytes: usize,
    /// The scripts were stopped (by the user, or for going over a limit).
    pub stopped: bool,
}

/// Fetch's request `mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestMode {
    SameOrigin,
    Cors,
    NoCors,
    Navigate,
}

/// Fetch's request `credentials`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialsMode {
    Omit,
    SameOrigin,
    Include,
}

/// Fetch's request `redirect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirectMode {
    Follow,
    Error,
    Manual,
}

/// Which API made a request (the app logs it, and an XHR's failures are events, not a
/// rejected promise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetKind {
    Fetch,
    Xhr,
    /// `navigator.sendBeacon` (a POST nobody waits for) or a `keepalive` fetch.
    Beacon,
    /// A module script or one of its imports (`<script type=module>`, `import`, `import()`,
    /// `<link rel=modulepreload>`): a GET in mode cors. The response has to be a success
    /// and a JavaScript (or, for a JSON module, JSON) MIME type; the page's console says
    /// why when it is not.
    Module,
}

/// A request a script made, for the app to run. Everything is as the script set it, except
/// that the engine has already dropped headers a script may not set; the app applies the
/// rules again (it does not trust this) and adds what only it may (`Cookie`, `Origin`,
/// `Referer`, `Host`, the framing headers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetRequest {
    /// The handle for [`NetOut::Cancel`] and [`Document::net_event`].
    pub id: u32,
    pub kind: NetKind,
    /// Absolute (resolved against the page), without a fragment.
    pub url: String,
    /// Upper case for the standard methods, as written otherwise.
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub mode: RequestMode,
    pub credentials: CredentialsMode,
    pub redirect: RedirectMode,
    /// The `referrer` member: "" for none, "about:client" for the page, else an address.
    pub referrer: String,
    pub referrer_policy: String,
    /// The `cache` member ("default", "no-store", ...).
    pub cache: String,
    /// The request's own time limit, ms (an XHR's `timeout`); 0: none. The engine also
    /// ends the request itself when it passes, so the app may ignore it.
    pub timeout_ms: u32,
    /// Always preflight a cross-origin request (an XHR with listeners on its `upload`).
    pub force_preflight: bool,
}

/// What a script's request leaves for the app, in order (see [`Document::take_net`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetOut {
    Start(NetRequest),
    /// The script aborted it (or its timeout passed): stop the work, send nothing more.
    Cancel(u32),
}

/// The kind of response the page gets (Fetch's response `type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseKind {
    Basic,
    Cors,
    /// A `no-cors` cross-origin response: status 0, no headers, no body.
    Opaque,
    /// A `manual` redirect that was not followed: status 0, no headers, no body.
    OpaqueRedirect,
}

/// A response's head, with the headers the page may see (the app has already removed
/// Set-Cookie and, for a cross-origin response, everything CORS doesn't expose).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetHead {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    /// The address it came from after redirects ("" for an opaque response).
    pub url: String,
    pub redirected: bool,
    pub kind: ResponseKind,
}

/// Why a request failed (for an XHR: `error` or `timeout`; for fetch: a TypeError).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetFailureKind {
    /// No connection, a broken one, a certificate, a bad response.
    Network,
    /// The same-origin policy or CORS said no.
    Blocked,
    /// Over a size limit.
    TooBig,
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetFailure {
    pub kind: NetFailureKind,
    /// One plain-English line for the page's console ("Blocked by CORS: ...", "Mixed content:
    /// ...", "GET http://... failed: ...").
    pub message: String,
}

/// An outcome the app gives back for request `id` (see [`Document::net_event`]). A request
/// gets at most one `Head`, any number of `Progress`, then one `Done` or one `Failed` (which
/// may come without a `Head`, and ends it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetEvent {
    Head(NetHead),
    /// Body bytes received so far, and the total if the server said.
    Progress { loaded: u64, total: Option<u64> },
    /// The whole body (empty for an opaque response and for HEAD).
    Done(Vec<u8>),
    Failed(NetFailure),
}

// ---- The document -------------------------------------------------------------------------

/// A parsed page. Not tied to any I/O; all inputs come in through the methods.
pub struct Document {
    pub(crate) page: crate::page::Page,
    /// Its scripts, if it was parsed with them on ([`Document::parse_scripted`]).
    pub(crate) js: Option<alloc::boxed::Box<crate::script::Runtime>>,
    /// Requests made without scripts (a form that didn't validate).
    pub(crate) notes: Vec<PageRequest>,
}

impl Document {
    /// Parse a page. `bytes` is the response body (already un-gzipped), `url` the final URL
    /// after redirects (the base for relative links), `content_type` the Content-Type header if
    /// there was one (its charset wins; otherwise a BOM, then `<meta charset>`, then UTF-8 if
    /// it is valid, else windows-1252). `text/plain` bodies become a `<pre>` page.
    pub fn parse(bytes: &[u8], url: &str, content_type: Option<&str>) -> Document {
        Document::parse_with(bytes, url, content_type, Limits::default())
    }

    pub fn parse_with(
        bytes: &[u8],
        url: &str,
        content_type: Option<&str>,
        limits: Limits,
    ) -> Document {
        Document {
            page: crate::page::Page::parse(bytes, url, content_type, limits),
            js: None,
            notes: Vec::new(),
        }
    }

    /// Parse a string of HTML (no charset step).
    pub fn parse_str(html: &str, url: &str) -> Document {
        Document {
            page: crate::page::Page::parse_str(html, url, Limits::default()),
            js: None,
            notes: Vec::new(),
        }
    }

    /// The `<title>`, whitespace collapsed (empty if none).
    pub fn title(&self) -> String {
        self.page.title()
    }

    /// The URL relative links resolve against (`<base href>` if present, else the page URL).
    pub fn base_url(&self) -> &str {
        self.page.base_url()
    }

    /// Resolve an href against [`Document::base_url`].
    pub fn resolve(&self, href: &str) -> String {
        self.page.resolve(href)
    }

    /// The icon from `<link rel=icon>`, absolute, if any.
    pub fn favicon(&self) -> Option<String> {
        self.page.favicon()
    }

    /// `<meta http-equiv=refresh content="5; url=...">`: (seconds, absolute URL or None for
    /// "reload this page").
    pub fn meta_refresh(&self) -> Option<(u32, Option<String>)> {
        self.page.meta_refresh()
    }

    /// The input was cut at [`Limits`] (bytes, nodes or depth).
    pub fn truncated(&self) -> bool {
        self.page.truncated()
    }

    /// Stylesheets and images not supplied yet. Stylesheets come first (they can change which
    /// images are needed); call again after supplying them. `<img>` images are listed from the
    /// start; CSS background images once styles are known (after a [`Document::layout`]; the
    /// next layout uses them). [`Document::load_resources`] does all of this in one go.
    pub fn pending_resources(&self) -> Vec<Resource> {
        self.page.pending_resources()
    }

    /// Give a stylesheet's body (None if it failed to load). May add new pending resources
    /// (@import, background images). The next [`Document::layout`] restyles.
    pub fn supply_stylesheet(&mut self, url: &str, body: Option<&[u8]>) {
        self.page.supply_stylesheet(url, body)
    }

    /// Give an image's intrinsic size in image pixels (None if it failed: the alt text is
    /// shown). The next [`Document::layout`] uses it.
    pub fn supply_image(&mut self, id: ImageId, size: Option<(u32, u32)>) {
        self.page.supply_image(id, size)
    }

    /// The absolute URL of an image.
    pub fn image_url(&self, id: ImageId) -> &str {
        self.page.image_url(id)
    }

    /// Fetch every pending resource through `f` (repeatedly, for @import chains), up to the
    /// [`Limits`].
    pub fn load_resources(&mut self, f: &mut dyn Fetcher) {
        self.page.load_resources(f)
    }

    /// Lay the page out and build its display list. Cheap to call again with the same
    /// viewport (it returns the cached list); a new width, zoom, a supplied resource or a
    /// changed control restyle and relayout as needed.
    pub fn layout(&mut self, vp: &Viewport, m: &dyn TextMeasurer) -> &DisplayList {
        self.page.layout(vp, m)
    }

    /// The last display list (empty before the first layout).
    pub fn display_list(&self) -> &DisplayList {
        self.page.display_list()
    }

    /// What is at a point. `x`, `y` are viewport coordinates, `scroll` the app's scroll
    /// offset (so fixed boxes are hit where they are drawn).
    pub fn hit_test(&self, x: i32, y: i32, scroll: (i32, i32)) -> Hit {
        self.page.hit_test(x, y, scroll)
    }

    /// Every occurrence of `query` in the laid-out text, in document order. Whitespace in the
    /// page is collapsed as displayed; `case_sensitive` false folds ASCII and Latin-1 case.
    pub fn find(&self, query: &str, case_sensitive: bool) -> Vec<FindMatch> {
        self.page.find(query, case_sensitive)
    }

    /// The y offset of the element named by a URL fragment (`id`, or `<a name>`), for
    /// scrolling to `#frag`. None if no such element (the app stays at the top).
    pub fn fragment_y(&self, fragment: &str) -> Option<i32> {
        self.page.fragment_y(fragment)
    }

    /// Where the main content is: the `<main>` element or `role=main`, else the single
    /// `<article>`, else `#content` / `#main-content` / `#main`. For "skip to content".
    pub fn main_content(&self) -> Option<Rect> {
        self.page.main_content()
    }

    /// The form controls, indexed by [`ControlId`]. Rects are from the last layout.
    pub fn controls(&self) -> &[FormControl] {
        self.page.controls()
    }

    /// Set a text field's, textarea's (or other value control's) value. Ignored for disabled
    /// or read-only controls and for kinds that have no typed value. `maxlength` is applied.
    pub fn set_value(&mut self, id: ControlId, value: &str) {
        self.page.set_value(id, value)
    }

    /// Check or uncheck a checkbox or radio button (checking a radio unchecks the others of
    /// its group).
    pub fn set_checked(&mut self, id: ControlId, checked: bool) {
        self.page.set_checked(id, checked)
    }

    /// Select option `index` of a `<select>` (for a single select this deselects the rest;
    /// for `multiple` it toggles the option to `selected`).
    pub fn select_option(&mut self, id: ControlId, index: usize, selected: bool) {
        self.page.select_option(id, index, selected)
    }

    /// Submit the form `id` belongs to, as if `id` were activated: a submit button is the
    /// submitter; Enter in a text field submits with no submitter (the "implicit submission"
    /// rule). None if `id` is in no form or the method is not GET/POST (dialog).
    /// For [`ControlKind::ImageSubmit`] pass the click point inside the image as `click`.
    pub fn submit(&self, id: ControlId, click: Option<(i32, i32)>) -> Option<Submission> {
        self.page.submit(id, click)
    }

    /// Reset the form `id` belongs to (the `value`/`checked`/`selected` attributes again).
    pub fn reset(&mut self, id: ControlId) {
        self.page.reset(id)
    }

    /// The text of the page as displayed (block boundaries become newlines), for copy-all and
    /// reader-like uses.
    pub fn text(&self) -> String {
        self.page.text()
    }

    pub fn stats(&self) -> Stats {
        self.page.stats()
    }
}

/// # Scripts
///
/// A page parsed with [`Document::parse_scripted`] runs its JavaScript, in its own realm
/// with the [`ScriptLimits`]. The app drives it:
///
/// ```text
///   parse_scripted ─▶ pending_scripts() ──app fetches──▶ supply_script()
///          │                                                   │
///          ▼                                                   ▼
///   run(host) whenever something came in, or at next_wake(): scripts in order, timers,
///             DOMContentLoaded / load, microtasks
///   frame(host) before drawing while wants_frame(): requestAnimationFrame callbacks
///   click / key / set_focus / input / change / submit_form: the user's doings as events
///   take_requests(), take_console(): what the scripts asked for and said
/// ```
///
/// After any of these, lay out again before hit testing or drawing (the DOM may have
/// changed; [`Document::layout`] restyles only what changed). Form controls may come and go:
/// [`ControlId`]s are only good until the next call that runs script, so keep the focus in
/// the engine ([`Document::focused`]) rather than an id of your own.
impl Document {
    /// Parse a page with its scripts on (`<noscript>` is then not shown). None for
    /// `scripts` is [`Document::parse_with`]: scripts off.
    pub fn parse_scripted(
        bytes: &[u8],
        url: &str,
        content_type: Option<&str>,
        limits: Limits,
        scripts: Option<ScriptLimits>,
    ) -> Document {
        let page = crate::page::Page::parse_scripting(
            bytes,
            url,
            content_type,
            limits,
            scripts.is_some(),
        );
        let mut d = Document {
            page,
            js: None,
            notes: Vec::new(),
        };
        if let Some(l) = scripts {
            d.js = Some(alloc::boxed::Box::new(crate::script::Runtime::new(&d.page, l)));
        }
        d
    }

    /// The page runs scripts (and they haven't been stopped).
    pub fn scripting(&self) -> bool {
        self.js.as_ref().is_some_and(|j| !j.stopped())
    }

    /// External scripts to fetch (absolute URLs), in the order they are wanted.
    pub fn pending_scripts(&self) -> Vec<String> {
        self.js.as_ref().map(|j| j.pending()).unwrap_or_default()
    }

    /// An external script's body (None: the fetch failed; its `error` event fires). Call
    /// [`Document::run`] after.
    pub fn supply_script(&mut self, url: &str, body: Option<&[u8]>) {
        if let Some(j) = self.js.as_mut() {
            j.supply(url, body);
        }
    }

    /// Do the script work that is due: scripts whose turn it is, timers due by
    /// [`ScriptHost::now`], the DOMContentLoaded and load events when their time comes, and
    /// the microtasks after each. True if anything ran (the page may have changed).
    pub fn run(&mut self, host: &mut dyn ScriptHost) -> bool {
        match self.js.as_mut() {
            Some(j) => j.run(&mut self.page, host),
            None => false,
        }
    }

    /// When [`Document::run`] has timer work next ([`ScriptHost::now`]'s clock). None: only
    /// when something comes in.
    pub fn next_wake(&self) -> Option<f64> {
        self.js.as_ref().and_then(|j| j.next_wake())
    }

    /// requestAnimationFrame callbacks are waiting: call [`Document::frame`] before the next
    /// draw (at the display's rate while this stays true).
    pub fn wants_frame(&self) -> bool {
        self.js.as_ref().is_some_and(|j| j.wants_frame())
    }

    /// Run the animation frame callbacks (just before drawing).
    pub fn frame(&mut self, host: &mut dyn ScriptHost) -> bool {
        match self.js.as_mut() {
            Some(j) => j.frame(&mut self.page, host),
            None => false,
        }
    }

    /// A click at viewport point (`x`, `y`) with the view scrolled by `scroll`: mousedown,
    /// mouseup and click go to the element there. A checkbox or radio button under it is
    /// toggled here (before the click event, and back if a listener cancels it; `input`
    /// and `change` follow), so the app must not toggle it again. False if a listener
    /// cancelled the click (preventDefault): the app then skips its own action (following
    /// a link, pressing a button, opening a select).
    pub fn click(
        &mut self,
        host: &mut dyn ScriptHost,
        x: i32,
        y: i32,
        scroll: (i32, i32),
        mods: Mods,
    ) -> bool {
        crate::script::click(self, host, x, y, scroll, mods)
    }

    /// A key went down (`up` false) or up, for the focused element (or the body). False if
    /// a listener cancelled it: the app skips what the key would do (typing, scrolling).
    pub fn key(&mut self, host: &mut dyn ScriptHost, key: &KeyInput, up: bool) -> bool {
        match self.js.as_mut() {
            Some(j) => j.key(&mut self.page, host, key, up),
            None => true,
        }
    }

    /// The form control with the keyboard, as the page sees it.
    pub fn focused(&self) -> Option<ControlId> {
        self.page.focused_control()
    }

    /// Give a control the keyboard (None: take it away): blur, focusout, focus, focusin go
    /// to the elements, and `change` to a text field left with a new value.
    pub fn set_focus(&mut self, host: &mut dyn ScriptHost, id: Option<ControlId>) {
        let node = id.and_then(|i| self.page.control_node(i));
        match self.js.as_mut() {
            Some(j) => j.focus(&mut self.page, host, node),
            None => self.page.set_focus_node(node),
        }
    }

    /// The user changed control `id`'s value (typed, pasted, picked an option): the
    /// `input` event (and for a select, `change`).
    pub fn input(&mut self, host: &mut dyn ScriptHost, id: ControlId) {
        if let Some(n) = self.page.control_node(id)
            && let Some(j) = self.js.as_mut()
        {
            j.user_input(&mut self.page, host, n);
        }
    }

    /// Submit the form control `id` belongs to, as [`Document::submit`] would, after its
    /// `submit` event: None if a listener cancelled it (or there is nothing to submit).
    pub fn submit_form(
        &mut self,
        host: &mut dyn ScriptHost,
        id: ControlId,
        click: Option<(i32, i32)>,
    ) -> Option<Submission> {
        // Something to submit at all (a form, a method that sends)?
        self.page.submit(id, click)?;
        let form = self.page.form_node_of(id)?;
        match self.js.as_mut() {
            Some(j) if !j.stopped() => {
                if !j.submit_event(&mut self.page, host, form, id) {
                    return None;
                }
            }
            _ => {
                if let Some((n, msg)) = crate::script::first_invalid(&mut self.page, form, id) {
                    self.page.set_focus_node(Some(n));
                    self.notes.push(PageRequest::Invalid(msg));
                    return None;
                }
            }
        }
        // Listeners may have changed the values: the submission as it is now.
        self.page.sync_forms();
        let id = self.page.control_of_node(self.page.control_node(id)?)?;
        self.page.submit(id, click)
    }

    /// What the scripts asked of the app since the last call, in order.
    pub fn take_requests(&mut self) -> Vec<PageRequest> {
        let mut v = core::mem::take(&mut self.notes);
        if let Some(j) = self.js.as_mut() {
            v.extend(j.take_requests());
        }
        v
    }

    /// Console lines since the last call.
    pub fn take_console(&mut self) -> Vec<ConsoleLine> {
        self.js.as_mut().map(|j| j.take_console()).unwrap_or_default()
    }

    /// Network requests the scripts made (or aborted) since the last call, in order.
    pub fn take_net(&mut self) -> Vec<NetOut> {
        self.js.as_mut().map(|j| j.take_net()).unwrap_or_default()
    }

    /// Requests made and not finished yet (no `Done` or `Failed` has come, no cancel).
    pub fn net_pending(&self) -> usize {
        self.js.as_ref().map_or(0, |j| j.net_pending())
    }

    /// The outcome of request `id` (see [`NetEvent`]): one task of the page's event loop,
    /// the microtasks after it. An id the page no longer waits for (aborted, or the
    /// scripts were stopped) is ignored. True if a script ran.
    pub fn net_event(&mut self, host: &mut dyn ScriptHost, id: u32, ev: NetEvent) -> bool {
        match self.js.as_mut() {
            Some(j) => j.net_event(&mut self.page, host, id, ev),
            None => false,
        }
    }

    /// Stop the page's scripts for good (the user said so): nothing more runs, timers and
    /// listeners are dropped, the page stays as it is.
    pub fn stop_scripts(&mut self) {
        if let Some(j) = self.js.as_mut() {
            j.stop("stopped");
        }
    }

    pub fn script_stats(&self) -> ScriptStats {
        self.js.as_ref().map(|j| j.stats()).unwrap_or_default()
    }

    /// The DOM changed since the last layout: lay out again before hit testing.
    pub fn needs_layout(&self) -> bool {
        self.page.changed()
    }
}
