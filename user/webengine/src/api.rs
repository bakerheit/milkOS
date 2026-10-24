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
//! - Frames: `<iframe>` elements are slots the host fills with documents it loaded (see the
//!   "Frames" section at the end); the engine lays them out inside the parent, paints,
//!   hit-tests, scrolls, focuses and searches across them.
//! - Not supported (yet): `:hover`/`:focus` styles, CSS transforms, animations, web fonts,
//!   named grid lines, inner scrolling boxes (overflow: auto spills sideways and clips
//!   vertically), bidirectional text, vertical writing modes, `<frameset>`/`<frame>` (they are
//!   not boxes), `<object>`/`<embed>` documents; for scripts: canvas, workers.
//!
//! # CHANGES
//! - Frames. `<iframe>` support: see "Frames" at the end of this file for the model (slots, ids,
//!   attach/detach, one display list with the frames' content inside it, hit testing, scrolling,
//!   focus order, find, `window.name`, `postMessage`, same-origin reach between frames). What an
//!   existing caller sees: [`Entry`] has a `frame` field (always [`FrameId::TOP`] for a page
//!   without frames), [`Hit`] has `frame`, `point` and `scroll`, [`FindMatch`] has `frame` and
//!   `frame_rects`, and [`Limits`] has `max_frames` and `max_frame_depth` (build a `Limits`
//!   with `..Default::default()`); all are plain fields set by the engine, so code that only
//!   reads is unchanged. [`ScriptHost`] has a new provided method, `frame_host`.
//!   [`Document::layout`], [`Document::display_list`], [`Document::hit_test`],
//!   [`Document::find`], [`Document::text`], [`Document::needs_layout`], [`Document::stats`]
//!   and [`Document::take_console`] now include the frames. [`Document::frame_doc`] /
//!   [`Document::frame_doc_mut`] are the names for a frame's document ([`Document::frame`]
//!   was taken by requestAnimationFrame). `window.postMessage` queues a message
//!   ([`Document::take_messages`]) instead of doing nothing, `window.name` is the browsing
//!   context's name, and there is a `MessageEvent`. An `<iframe>`'s `frameborder=0` (or `no`)
//!   removes its UA border. Moving an `<iframe>` element in the tree keeps its document (a
//!   browser would reload it); removing it closes the frame.
//! - ES modules in pages. `<script type=module>` (inline and `src`), `<script type=importmap>`,
//!   `<link rel=modulepreload>`, `nomodule` and `import()` work with no change to the calls
//!   an app makes: a module and everything it imports is fetched through the script network
//!   path, so the app sees them as [`NetOut::Start`] requests of the new kind
//!   [`NetKind::Module`] (a GET in mode [`RequestMode::Cors`], with [`CredentialsMode::SameOrigin`]
//!   or, for `crossorigin=use-credentials` and what such a module imports,
//!   [`CredentialsMode::Include`]) and answers them with [`Document::net_event`] like any other.
//!   An app that matches on [`NetKind`] must handle the new variant. The app should refuse a
//!   module response that is not a success status with a JavaScript (or JSON) MIME type (the
//!   engine checks too, and cancels the request with [`NetOut::Cancel`] if the head says no).
//!   Module addresses never come out of [`Document::pending_scripts`]. A page may have 2,000
//!   modules, an import chain 256 deep, 24 module requests in flight and 48 MB of module source;
//!   [`Document::net_pending`] counts module fetches, and stopping the scripts cancels them.
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
    /// Whose entry this is: [`FrameId::TOP`] for the page's own, else the frame whose
    /// document drew it (see "Frames"). It matters for [`Item::Image`] and [`Item::Control`]
    /// only: an [`ImageId`] or [`ControlId`] means something in that frame's document
    /// ([`Document::frame_doc`], then [`Document::image_url`] / [`Document::controls`]).
    pub frame: FrameId,
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
    /// The frame the pointer is in: [`FrameId::TOP`], or the innermost `<iframe>` document
    /// under it. `link`, `control` and `title` are that document's (a [`ControlId`] means
    /// something in that frame only).
    pub frame: FrameId,
    /// The pointer in that frame's own viewport coordinates, and the frame's scroll offset:
    /// pass both to the calls on that frame's document ([`Document::click`] and the like,
    /// through [`Document::with_frame`]).
    pub point: (i32, i32),
    pub scroll: (i32, i32),
}

// ---- Find in page -------------------------------------------------------------------------

/// One occurrence of the search text. A match that wraps across lines has several rects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindMatch {
    /// Where it is, in the top page's document space as it is drawn now: for a match inside a
    /// frame, in the frame's scroll position, so a match scrolled out of its frame's view
    /// lies outside the frame's box (use [`Document::reveal`] to scroll it in).
    pub rects: Vec<Rect>,
    /// The frame whose document holds the text ([`FrameId::TOP`] for the page itself).
    pub frame: FrameId,
    /// The same rectangles in that frame's own document space (what [`Document::reveal`]
    /// takes).
    pub frame_rects: Vec<Rect>,
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
    /// `<iframe>` slots in a whole page (the frames of frames included). Frames over it are
    /// not given a slot; the page stays as it is.
    pub max_frames: usize,
    /// How deep frames nest: the page is depth 0, its frames 1. A frame deeper than this
    /// gets a slot that is [`FrameBlock::TooDeep`]. Only the top document's limits count.
    pub max_frame_depth: usize,
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
            max_frames: 32,
            max_frame_depth: 5,
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
    /// The host for the scripts of frame `id` ([`FrameId::TOP`] included), for when one
    /// frame's script reaches into another's (same-origin access: `contentWindow`, `parent`).
    /// None: no access (the bindings throw a SecurityError). The host that runs a frame's
    /// scripts is a different one per frame (its viewport, cookies and storage are the frame's
    /// origin's), so this is how a call finds the other's.
    fn frame_host(&mut self, _id: FrameId) -> Option<&mut dyn ScriptHost> {
        None
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
    /// The most memory (an estimate, in bytes) one parse and compile of a script may use at
    /// once for its syntax tree, apart from the heap above (the tree only exists while a
    /// script or one function is being compiled; big functions are compiled on their first
    /// call). See `jsengine::Limits::max_compile_bytes`.
    pub compile: usize,
}

impl Default for ScriptLimits {
    fn default() -> ScriptLimits {
        ScriptLimits {
            memory: 64 << 20,
            slow_ms: 2_000,
            max_source: 8 << 20,
            compile: 96 << 20,
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
    /// A script asked for another frame's window to go somewhere (`top.location = ...`,
    /// `iframe.contentWindow.location.href = ...`, `frames[0].location.replace(...)`):
    /// the request comes from the frame whose queue it is in, so the host can apply the
    /// sandbox rules of the one that asked.
    NavigateOther {
        target: FrameId,
        url: String,
        replace: bool,
    },
    /// A script clicked a link that has a `target` other than `_self` (`a.click()`): the host
    /// chooses the frame or tab like it does for a click by the user.
    NavigateTarget { url: String, target: String },
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

// ---- Frames ---------------------------------------------------------------------------------

/// A frame (a browsing context) of a page: [`FrameId::TOP`] is the page itself, every other id
/// names the slot of one `<iframe>` element somewhere in the page's tree of frames. Ids are
/// handed out by the top document when it first sees the element (see
/// [`Document::frame_slots`]), are unique in that page and are never used twice, even for a
/// frame whose element is gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FrameId(pub u32);

impl FrameId {
    /// The page itself (the top document).
    pub const TOP: FrameId = FrameId(0);
    pub fn is_top(self) -> bool {
        self.0 == 0
    }
}

/// Why a slot can never hold a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameBlock {
    /// Nested deeper than [`Limits::max_frame_depth`]. The element is an empty box.
    TooDeep,
}

/// What the `scrolling` attribute says (HTML's obsolete but still used `scrolling=no`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameScrolling {
    Auto,
    /// `scrolling=no`: the frame's document does not scroll (the engine pins its offset at 0).
    No,
}

/// The `sandbox` attribute, read (HTML §4.8.5): each field is true when the token that allows
/// it is present. The engine only parses; the host enforces. An empty attribute is all false.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sandbox {
    pub scripts: bool,
    pub forms: bool,
    pub same_origin: bool,
    pub top_navigation: bool,
    pub top_navigation_by_user_activation: bool,
    pub popups: bool,
    pub popups_to_escape_sandbox: bool,
    pub modals: bool,
    pub downloads: bool,
    pub pointer_lock: bool,
    pub orientation_lock: bool,
    pub presentation: bool,
    pub storage_access_by_user_activation: bool,
}

impl Sandbox {
    /// Read the attribute's value: space-separated, ASCII-case-insensitive `allow-*` tokens;
    /// unknown tokens are ignored.
    pub fn parse(value: &str) -> Sandbox {
        let mut s = Sandbox::default();
        for t in value.split([' ', '\t', '\n', '\r', '\x0c']) {
            match t.to_ascii_lowercase().as_str() {
                "allow-scripts" => s.scripts = true,
                "allow-forms" => s.forms = true,
                "allow-same-origin" => s.same_origin = true,
                "allow-top-navigation" => s.top_navigation = true,
                "allow-top-navigation-by-user-activation" => {
                    s.top_navigation_by_user_activation = true
                }
                "allow-popups" => s.popups = true,
                "allow-popups-to-escape-sandbox" => s.popups_to_escape_sandbox = true,
                "allow-modals" => s.modals = true,
                "allow-downloads" => s.downloads = true,
                "allow-pointer-lock" => s.pointer_lock = true,
                "allow-orientation-lock" => s.orientation_lock = true,
                "allow-presentation" => s.presentation = true,
                "allow-storage-access-by-user-activation" => {
                    s.storage_access_by_user_activation = true
                }
                _ => {}
            }
        }
        s
    }
}

/// One `<iframe>` of the page, as the host sees it (see [`Document::frame_slots`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameSlot {
    pub id: FrameId,
    /// The frame whose document holds the element ([`FrameId::TOP`] for the page).
    pub parent: FrameId,
    /// 1 for the page's own frames, 2 for theirs, ...
    pub depth: u32,
    /// Bumped whenever `src` or `srcdoc` changes (a script set them, or the element was
    /// replaced): a document loaded for an older generation is stale.
    pub generation: u32,
    /// The `src` attribute resolved against the parent's base URL; None if absent or blank.
    pub src: Option<String>,
    /// The `srcdoc` attribute (it wins over `src`).
    pub srcdoc: Option<String>,
    /// The `name` attribute: the name of the frame's browsing context (`window.name`,
    /// `target=`).
    pub name: String,
    /// The `sandbox` attribute; None: not sandboxed.
    pub sandbox: Option<Sandbox>,
    /// `loading=lazy`: the host may wait until the frame is near the viewport (`rect`).
    pub lazy: bool,
    /// The `allow` attribute as written, and `allowfullscreen`.
    pub allow: String,
    pub allow_fullscreen: bool,
    /// The `referrerpolicy` attribute, lower-cased ("" when absent).
    pub referrer_policy: String,
    pub scrolling: FrameScrolling,
    /// The frame's content box (inside the border and padding) in its parent's document
    /// space, device pixels, as of the last layout; empty if the element has no box
    /// (`display: none`, not laid out yet). It is the size of the frame's viewport.
    pub rect: Rect,
    /// The element has a box: it can be seen (and its document is laid out).
    pub rendered: bool,
    /// None: the slot can hold a document.
    pub blocked: Option<FrameBlock>,
    /// The [`FrameSlot::generation`] the attached document was loaded for; None: no
    /// document attached yet.
    pub attached: Option<u32>,
    /// The frame's document scroll offset, and the size of its scrollable area (the last
    /// layout), device pixels.
    pub scroll: (i32, i32),
    pub content: (i32, i32),
}

impl FrameSlot {
    /// The host should load something: the slot can hold a document and has none for its
    /// current `src` / `srcdoc` (a `srcdoc`, `about:blank` for no `src`, or the `src`).
    pub fn wants_load(&self) -> bool {
        self.blocked.is_none() && self.attached != Some(self.generation)
    }
}

/// Where a document sits in its page's frame tree (see [`Document::frame_info`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    pub id: FrameId,
    /// None for the top document.
    pub parent: Option<FrameId>,
    /// 0 for the top document.
    pub depth: u32,
}

/// A message `window.postMessage` queued, for the host to deliver (see
/// [`Document::take_messages`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutMessage {
    /// The frame whose window the script called `postMessage` on.
    pub target: FrameId,
    /// The `targetOrigin` argument as given (`"*"`, `"/"`, an origin).
    pub target_origin: String,
    /// The value, as JSON text (`JSON.stringify`); empty for `undefined`.
    pub data: String,
    /// The message ports it carries (in transit until the receiver adopts them).
    pub ports: Vec<u64>,
}

/// A message to fire at a window (see [`Document::deliver_message`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// The value as JSON text; empty for `undefined`.
    pub data: String,
    /// `event.origin`: the sender's serialized origin ("null" for an opaque one).
    pub origin: String,
    /// `event.source`: the sending frame.
    pub source: FrameId,
    /// The message ports it carries: `event.ports`.
    pub ports: Vec<u64>,
}

// ---- The document -------------------------------------------------------------------------

/// A parsed page. Not tied to any I/O; all inputs come in through the methods.
pub struct Document {
    pub(crate) page: crate::page::Page,
    /// Its scripts, if it was parsed with them on ([`Document::parse_scripted`]).
    pub(crate) js: Option<alloc::boxed::Box<crate::script::Runtime>>,
    /// Requests made without scripts (a form that didn't validate).
    pub(crate) notes: Vec<PageRequest>,
    /// The frames of the page (see "Frames"); only the top document has them.
    pub(crate) frames: Option<alloc::boxed::Box<crate::frames::Frames>>,
    /// This document is a frame's document, owned by the top document's table.
    pub(crate) attached: bool,
    /// The display list with the frames' content in it (None: the page's own is it).
    pub(crate) merged: Option<alloc::boxed::Box<crate::frames::Merged>>,
    /// `Frames::version` at the last layout.
    pub(crate) frames_seen: u64,
    /// For a frame's document inside [`Document::with_frame`]: the rest of the tree.
    pub(crate) peers: Option<crate::script::frames::Peers>,
}

impl Document {
    pub(crate) fn from_page(page: crate::page::Page) -> Document {
        Document {
            page,
            js: None,
            notes: Vec::new(),
            frames: None,
            attached: false,
            merged: None,
            frames_seen: 0,
            peers: None,
        }
    }

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
        Document::from_page(crate::page::Page::parse(bytes, url, content_type, limits))
    }

    /// Parse a string of HTML (no charset step).
    pub fn parse_str(html: &str, url: &str) -> Document {
        Document::from_page(crate::page::Page::parse_str(html, url, Limits::default()))
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
        if self.attached {
            return self.page.layout(vp, m);
        }
        self.sync_frames();
        self.page.layout(vp, m);
        crate::frames::layout_frames(self, vp, m);
        self.display_list()
    }

    /// The last display list (empty before the first layout).
    pub fn display_list(&self) -> &DisplayList {
        match self.merged.as_ref() {
            Some(m) => &m.dl,
            None => self.page.display_list(),
        }
    }

    /// What is at a point. `x`, `y` are viewport coordinates, `scroll` the app's scroll
    /// offset (so fixed boxes are hit where they are drawn).
    pub fn hit_test(&self, x: i32, y: i32, scroll: (i32, i32)) -> Hit {
        crate::frames::hit_test(self, x, y, scroll)
    }

    /// Every occurrence of `query` in the laid-out text, in document order. Whitespace in the
    /// page is collapsed as displayed; `case_sensitive` false folds ASCII and Latin-1 case.
    pub fn find(&self, query: &str, case_sensitive: bool) -> Vec<FindMatch> {
        crate::frames::find(self, query, case_sensitive)
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
        crate::frames::text(self)
    }

    /// The page's numbers, with its frames' added in (`approx_bytes` also counts the merged
    /// display list).
    pub fn stats(&self) -> Stats {
        let mut st = self.page.stats();
        if let Some(f) = self.frames.as_ref() {
            for s in &f.slots {
                if let Some(d) = s.doc.as_ref() {
                    let c = d.page.stats();
                    st.nodes += c.nodes;
                    st.elements += c.elements;
                    st.rules += c.rules;
                    st.boxes += c.boxes;
                    st.display_items += c.display_items;
                    st.approx_bytes += c.approx_bytes + d.merged_bytes();
                    st.layout_bytes += c.layout_bytes;
                }
            }
        }
        st.approx_bytes += self.merged_bytes();
        st
    }

    /// Bytes the merged display list (the page's list with its frames in it) takes.
    pub(crate) fn merged_bytes(&self) -> usize {
        self.merged.as_ref().map_or(0, |m| {
            m.dl.entries.capacity() * core::mem::size_of::<Entry>()
                + m.dl.fixed.capacity() * core::mem::size_of::<Entry>()
                + m.dl.text.capacity()
                + m.dl.clips.capacity() * core::mem::size_of::<Rect>()
        })
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
        let mut d = Document::from_page(page);
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
        self.armed(|d| match d.js.as_mut() {
            Some(j) => j.run(&mut d.page, host),
            None => false,
        })
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
        self.armed(|d| match d.js.as_mut() {
            Some(j) => j.frame(&mut d.page, host),
            None => false,
        })
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
        self.armed(|d| crate::script::click(d, host, x, y, scroll, mods))
    }

    /// A key went down (`up` false) or up, for the focused element (or the body). False if
    /// a listener cancelled it: the app skips what the key would do (typing, scrolling).
    pub fn key(&mut self, host: &mut dyn ScriptHost, key: &KeyInput, up: bool) -> bool {
        self.armed(|d| match d.js.as_mut() {
            Some(j) => j.key(&mut d.page, host, key, up),
            None => true,
        })
    }

    /// The form control with the keyboard, as the page sees it.
    pub fn focused(&self) -> Option<ControlId> {
        self.page.focused_control()
    }

    /// Give a control the keyboard (None: take it away): blur, focusout, focus, focusin go
    /// to the elements, and `change` to a text field left with a new value.
    pub fn set_focus(&mut self, host: &mut dyn ScriptHost, id: Option<ControlId>) {
        let node = id.and_then(|i| self.page.control_node(i));
        self.armed(|d| match d.js.as_mut() {
            Some(j) => j.focus(&mut d.page, host, node),
            None => d.page.set_focus_node(node),
        })
    }

    /// The user changed control `id`'s value (typed, pasted, picked an option): the
    /// `input` event (and for a select, `change`).
    pub fn input(&mut self, host: &mut dyn ScriptHost, id: ControlId) {
        if let Some(n) = self.page.control_node(id) {
            self.armed(|d| {
                if let Some(j) = d.js.as_mut() {
                    j.user_input(&mut d.page, host, n);
                }
            });
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
        let scripted = self.js.as_ref().is_some_and(|j| !j.stopped());
        let go = scripted && {
            self.arm();
            let go = match self.js.as_mut() {
                Some(j) => j.submit_event(&mut self.page, host, form, id),
                None => true,
            };
            self.disarm();
            go
        };
        match scripted {
            true => {
                if !go {
                    return None;
                }
            }
            false => {
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
        let mut v = self
            .frames
            .as_mut()
            .map(|f| f.take_notes())
            .unwrap_or_default();
        if let Some(j) = self.js.as_mut() {
            v.extend(j.take_console());
        }
        v
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
        self.armed(|d| match d.js.as_mut() {
            Some(j) => j.net_event(&mut d.page, host, id, ev),
            None => false,
        })
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
        crate::frames::needs_layout(self)
    }
}

/// # Frames
///
/// `<iframe>` support. A page is a tree of frames: the top document and, for each `<iframe>`
/// element in it, a *slot* that holds a document of its own (which can have iframes in turn).
/// The engine does no I/O, so the host fetches the documents and the engine does everything
/// that crosses a frame boundary: lays the frames out inside their parents, paints them,
/// finds what the pointer is over, scrolls, searches and orders the focus across them.
///
/// ```text
///   top.frame_slots() ──▶ [FrameSlot { id, src, srcdoc, name, sandbox, rect, .. }]
///        │  a slot that wants_load(): the host fetches (or builds from srcdoc) a Document
///        ▼
///   top.attach_frame(id, doc, Some(generation))      top.detach_frame(id)
///        │                                           top.take_closed_frames()
///        ▼
///   top.with_frame(id, |doc| ..) ──▶ the frame's Document, driven exactly like the top one:
///                          supply_stylesheet, supply_image, run, net_event, click, key,
///                          take_requests, take_console, take_net, ... Use `with_frame`
///                          for the calls that run script, so same-origin scripts can reach
///                          each other; `frame_doc` / `frame_doc_mut` do for the rest. One
///                          ScriptHost per frame (its viewport is
///                          [`Document::frame_viewport`], its cookies and storage are its
///                          origin's), and `frame_host` says how one finds another's.
///   top.layout(&vp, m) ──▶ lays out the children into their slots (a viewport the size of
///                          the slot's content box, the parent's zoom and scheme) and paints
///                          them into the one display list the host draws
/// ```
///
/// - **One tree, one set of ids.** [`FrameId::TOP`] is the page. Every other id is a slot of
///   the top document's table, whatever the depth, and is never reused. Calls that cross
///   frames are made on the top document and take or give [`FrameId`]s; the calls on a
///   frame's own document look at that document alone (its `<iframe>`s are slots of the top
///   document: `frame_slots` on a frame's document is empty). A document attached to a slot
///   is owned by it; a document with frames of its own attached elsewhere loses them.
/// - **Loading.** A slot [`wants_load`](FrameSlot::wants_load) until a document is attached
///   for its current [`FrameSlot::generation`]. What to load is `srcdoc`, else `src`, else
///   `about:blank` (the host builds an empty document); whether it may be loaded (framing
///   headers, sandbox, mixed content, cookies) is the host's, and a refusal is an error page
///   attached to the slot. A document for an older generation is refused by `attach_frame`.
///   A frame that navigates (a link with a target, `location`) is `attach_frame(id, doc,
///   None)`. `loading=lazy` is `FrameSlot::lazy` with the slot's `rect` for the host to
///   judge. `<frame>` and `<frameset>` are not supported (they are not boxes); neither are
///   `<object>` / `<embed>` documents.
/// - **The display list.** [`Document::layout`] on the top document returns one list with the
///   frames' content inside it, in the order they are painted (at the `<iframe>`'s place in
///   the stacking order, so positioned boxes can cover a frame), each entry clipped to the
///   slot's content box and moved by the slot's position and the frame's scroll offset; only
///   what is in the frame's view is listed. [`Entry::frame`] says whose entry it is, which
///   matters for [`Item::Image`] and [`Item::Control`] (an [`ImageId`] or [`ControlId`]
///   belongs to a document: `frame_doc(e.frame)`). A frame paints its own canvas colour
///   over its slot, so a frame is never see-through; `visibility: hidden` and `display:
///   none` frames paint nothing. A frame's `position: fixed` boxes are fixed to the frame's
///   view. [`FrameSlot::scroll`] and [`FrameSlot::content`] with the slot's `rect` are what a
///   host needs to draw scrollbars.
/// - **Scrolling.** The engine keeps each frame's offset ([`Document::set_frame_scroll`],
///   clamped; pinned by `scrolling=no`); the wheel goes through
///   [`Document::scroll_frames_at`] (the frame under the pointer first, then each parent, then
///   what is left is the page's); [`Document::reveal`] and [`Document::frame_fragment`] scroll
///   a frame, and the frames above it, to something inside it. After any of them, lay out
///   again ([`Document::needs_layout`] says so).
/// - **Pointer.** [`Document::hit_test`] looks through frames: [`Hit::frame`] is the frame
///   under the pointer and [`Hit::point`] / [`Hit::scroll`] the pointer and the scroll in
///   that frame's own viewport, which are what `top.with_frame(hit.frame, |d| d.click(host,
///   hit.point.0, hit.point.1, hit.scroll, mods))` wants. A box above a frame (positioned,
///   later in the order) gets the pointer, not the frame. [`Document::frame_at`] is the same
///   question for the cursor and the wheel.
/// - **Keyboard.** Each document keeps its own focus ([`Document::focused`]); the tree
///   keeps which frame has the keyboard ([`Document::focused_frame`]) and
///   [`Document::focus_order`] lists the controls of all frames in Tab order, the frame's at
///   the place of its `<iframe>`. The host moves the document's focus to match
///   ([`Document::set_focus`] fires `blur` and `focus`).
/// - **Find, text.** [`Document::find`] and [`Document::text`] include the frames, in
///   document order. A [`FindMatch`] says which frame it is in, where it is drawn (cut to the
///   slot) and where it is in the frame ([`FindMatch::frame_rects`], for
///   [`Document::reveal`]).
/// - **Names and messages.** `window.name` is kept per frame ([`Document::window_name`]; it
///   starts as the element's `name` and survives the frame's navigations);
///   [`Document::frames_named`] lists the frames a `target=` can mean. `window.postMessage`
///   queues an [`OutMessage`] ([`Document::take_messages`]) and the host delivers it with
///   [`Document::deliver_message`] after the origin checks. Neither the engine nor its
///   `window` knows origins: the host does.
/// - **Scripts across frames.** [`Document::with_frame`] arms the page's other frames for the
///   scripts of the document it runs, and the bindings reach one through
///   `script::frames::in_peer` / `with_peer` (a native's way into another realm, with that frame's
///   host from [`ScriptHost::frame_host`]). A frame that is running (it called into another
///   that now calls back) is busy and refused; so is a frame the host has no host for.
/// - **Limits.** [`Limits::max_frames`] slots per page and [`Limits::max_frame_depth`]
///   levels. A slot over the first is not made; one over the second is
///   [`FrameBlock::TooDeep`] (no document may be attached to it). The engine says so in the
///   top document's console ([`Document::take_console`]). The merged list holds only what is in
///   each frame's view, so a frame with a huge viewport or document costs what it shows.
impl Document {
    /// Make the other frames reachable from this document's scripts for the call that follows
    /// (see [`Document::with_frame`]).
    pub(crate) fn arm(&mut self) {
        self.touch_hub();
        let p = if self.attached {
            self.peers
        } else {
            let root = self as *mut Document;
            self.frames
                .as_deref_mut()
                .map(|f| crate::script::frames::Peers {
                    frames: f as *mut crate::frames::Frames,
                    root,
                })
        };
        if let Some(j) = self.js.as_ref() {
            j.set_peers(p);
        }
    }

    pub(crate) fn disarm(&mut self) {
        if let Some(j) = self.js.as_ref() {
            j.set_peers(None);
        }
    }

    /// Tell the frames' hub where this (top) document is now: documents move, and a script of a
    /// frame finds the top page through that address (script/hub.rs).
    pub(crate) fn touch_hub(&self) {
        if !self.attached
            && let Some(f) = self.frames.as_ref()
        {
            f.hub.set_top(self as *const Document as *mut Document);
        }
    }

    /// `f` with the other frames armed.
    pub(crate) fn armed<R>(&mut self, f: impl FnOnce(&mut Document) -> R) -> R {
        self.arm();
        let r = f(self);
        self.disarm();
        r
    }

    /// Run `f` on frame `id`'s document with the rest of the page reachable from its scripts:
    /// what a script needs to touch another frame's window (`parent.document`,
    /// `iframe.contentWindow.x`, [`ScriptHost::frame_host`] says whose host). While `f` runs
    /// the frame's document is out of the table (a script of another frame that wants it, because
    /// this one called into it, is refused: it is busy). Use it for any call that runs script
    /// ([`Document::run`], [`Document::net_event`], [`Document::click`], ...); it is
    /// [`Document::frame_doc_mut`] for everything else. [`FrameId::TOP`] runs `f` on this
    /// document. None for an unknown or empty frame.
    pub fn with_frame<R>(&mut self, id: FrameId, f: impl FnOnce(&mut Document) -> R) -> Option<R> {
        self.touch_hub();
        if id.is_top() {
            return Some(f(self));
        }
        let root = self as *mut Document;
        let frames = self.frames.as_deref_mut()?;
        let mut doc = frames.slot_mut(id)?.doc.take()?;
        doc.peers = Some(crate::script::frames::Peers {
            frames: frames as *mut crate::frames::Frames,
            root,
        });
        let out = f(&mut doc);
        doc.peers = None;
        // The slot may be gone (a script removed its element) or taken over by another
        // document: then this one is dropped.
        if let Some(frames) = self.frames.as_deref_mut()
            && let Some(s) = frames.slot_mut(id)
            && s.doc.is_none()
        {
            s.doc = Some(doc);
        }
        Some(out)
    }

    /// Where this document sits in its page's frame tree.
    pub fn frame_info(&self) -> FrameInfo {
        let c = self.page.frame;
        FrameInfo {
            id: c.id,
            parent: if c.id.is_top() { None } else { Some(c.parent) },
            depth: c.depth,
        }
    }

    /// Look at the `<iframe>` elements again (scripts may have added, changed or removed some)
    /// and keep the table in step. Called by the calls below; cheap when nothing changed.
    fn sync_frames(&mut self) {
        if self.attached {
            return;
        }
        let pending = self.page.frames_pending()
            || self.frames.as_ref().is_some_and(|f| {
                f.slots
                    .iter()
                    .any(|s| s.doc.as_ref().is_some_and(|d| d.page.frames_pending()))
            });
        if !pending {
            return;
        }
        self.page.flush();
        let lim = *self.page.limits();
        // Whatever the host asked for, the table and the recursion stay small.
        let (maxf, maxd) = (lim.max_frames.min(256), lim.max_frame_depth.min(16));
        if self.frames.is_none() && self.page.iframe_nodes().is_empty() {
            self.page.frames_dirty = false;
            self.page.take_freed_iframes();
            return;
        }
        let me = self as *mut Document;
        let frames = self
            .frames
            .get_or_insert_with(|| alloc::boxed::Box::new(crate::frames::Frames::new()));
        frames.hub.set_top(me);
        if let Some(js) = self.js.as_ref() {
            js.link_hub(frames.hub.clone(), 0, me);
        }
        frames.max_frames = maxf;
        frames.max_depth = maxd;
        frames.sync_doc(self.page.frame, &mut self.page);
        for id in frames.walk() {
            let Some(mut d) = frames.slot_mut(id).and_then(|s| s.doc.take()) else {
                continue;
            };
            if d.page.frames_pending() {
                d.page.flush();
                frames.sync_doc(d.page.frame, &mut d.page);
            }
            if let Some(s) = frames.slot_mut(id) {
                s.doc = Some(d);
            }
        }
    }

    /// Every `<iframe>` slot of the page, parents before their children, in tree order. Look
    /// at it after anything that may have run script or changed the page: new slots show up
    /// here (a slot that [`FrameSlot::wants_load`] needs its document).
    pub fn frame_slots(&mut self) -> Vec<FrameSlot> {
        self.sync_frames();
        let Some(f) = self.frames.as_ref() else {
            return Vec::new();
        };
        f.walk()
            .into_iter()
            .filter_map(|id| f.slot(id).map(|s| s.info()))
            .collect()
    }

    /// Put a loaded document into slot `id`, replacing the one there (which is dropped, and
    /// its frames' slots with it). `for_generation`: the [`FrameSlot::generation`] the load
    /// was started for; if the slot has moved on since (a script set `src` again), the
    /// document is refused and false returned. None: a navigation of the frame itself (a
    /// link, `location`): always taken. False too for an unknown or blocked slot. The
    /// frame's `window.name` is kept across a replacement (it starts as the element's `name`).
    /// The document should be a fresh one (never attached; any frames it had are dropped).
    pub fn attach_frame(&mut self, id: FrameId, doc: Document, for_generation: Option<u32>) -> bool {
        if self.attached || id.is_top() {
            return false;
        }
        self.sync_frames();
        let Some(frames) = self.frames.as_mut() else {
            return false;
        };
        crate::frames::attach(frames, id, doc, for_generation)
    }

    /// Take the document out of slot `id` (the slot stays, now empty, with the frames below it
    /// gone). The host drops it and cancels what is loading for it.
    pub fn detach_frame(&mut self, id: FrameId) -> Option<Document> {
        let frames = self.frames.as_mut()?;
        crate::frames::detach(frames, id)
    }

    /// Frames whose `<iframe>` element went away (or whose slot did, with its parent) since the
    /// last call: the host cancels their loads and forgets them.
    pub fn take_closed_frames(&mut self) -> Vec<FrameId> {
        self.sync_frames();
        self.frames
            .as_mut()
            .map(|f| f.take_closed())
            .unwrap_or_default()
    }

    /// The document of frame `id` ([`FrameId::TOP`]: this one).
    pub fn frame_doc(&self, id: FrameId) -> Option<&Document> {
        self.touch_hub();
        if id.is_top() {
            return Some(self);
        }
        self.frames.as_ref()?.slot(id)?.doc.as_deref()
    }

    /// The document of frame `id` to work on: supply its resources, run its scripts, give it
    /// the pointer and keys ([`FrameId::TOP`]: this one).
    pub fn frame_doc_mut(&mut self, id: FrameId) -> Option<&mut Document> {
        self.touch_hub();
        if id.is_top() {
            return Some(self);
        }
        self.frames.as_mut()?.slot_mut(id)?.doc.as_deref_mut()
    }

    /// The frame under a point of the top document's viewport (`scroll`: the page's scroll),
    /// [`FrameId::TOP`] if none: for the cursor, the wheel and where a click goes.
    pub fn frame_at(&self, x: i32, y: i32, scroll: (i32, i32)) -> FrameId {
        crate::frames::frame_at(self, x, y, scroll)
    }

    /// The frames that have a document, in tree order (not the top document).
    pub fn frame_ids(&self) -> Vec<FrameId> {
        let Some(f) = self.frames.as_ref() else {
            return Vec::new();
        };
        f.walk()
            .into_iter()
            .filter(|&id| f.slot(id).is_some_and(|s| s.doc.is_some()))
            .collect()
    }

    /// The direct children of frame `of` that have a slot, in tree order (`window.frames` of
    /// that window); those without a document are in it too.
    pub fn child_frames(&self, of: FrameId) -> Vec<FrameId> {
        self.frames
            .as_ref()
            .map(|f| f.children(of))
            .unwrap_or_default()
    }

    /// The frames whose browsing context is called `name` (`window.name`, in tree order,
    /// [`FrameId::TOP`] first if it is the page): the candidates of `target=name`. Whether a
    /// link may navigate one is the host's call (sandboxing, same site).
    pub fn frames_named(&self, name: &str) -> Vec<FrameId> {
        let mut v = Vec::new();
        if name.is_empty() {
            return v;
        }
        if self.page.window_name == name {
            v.push(FrameId::TOP);
        }
        if let Some(f) = self.frames.as_ref() {
            for id in f.walk() {
                if f.slot(id)
                    .and_then(|s| s.doc.as_ref())
                    .is_some_and(|d| d.page.window_name == name)
                {
                    v.push(id);
                }
            }
        }
        v
    }

    /// `window.name` of this document's browsing context.
    pub fn window_name(&self) -> &str {
        &self.page.window_name
    }

    /// Set `window.name` (the host clears it when a frame navigates to another site).
    pub fn set_window_name(&mut self, name: &str) {
        self.page.window_name = String::from(name);
    }

    /// The document has finished loading: with scripts, its `load` event has fired and none of
    /// its requests are outstanding; without, every stylesheet and image it asked for has been
    /// supplied. The host uses it to tell when a frame is done ([`Document::frame_load_event`]).
    pub fn loaded(&self) -> bool {
        match self.js.as_ref() {
            Some(j) if !j.stopped() => j.loaded() && self.page.pending_resources().is_empty(),
            _ => self.page.pending_resources().is_empty(),
        }
    }

    /// Fire `load` (or `error`: the document could not be loaded) at the `<iframe>` element
    /// of frame `id`, in the realm of the document that holds it: `iframe.onload`, an
    /// `addEventListener('load')`. `host` is the host of that document (the frame named by
    /// [`FrameSlot::parent`]). The page's own `load` does not wait for its frames. True if
    /// the event was dispatched.
    pub fn frame_load_event(&mut self, id: FrameId, host: &mut dyn ScriptHost, error: bool) -> bool {
        let Some((parent, node)) = self
            .frames
            .as_ref()
            .and_then(|f| f.slot(id))
            .map(|s| (s.parent, s.node))
        else {
            return false;
        };
        let ty = if error { "error" } else { "load" };
        self.with_frame(parent, |pd| {
            pd.armed(|pd| match pd.js.as_mut() {
                Some(j) if !j.stopped() => j.fire_node(&mut pd.page, host, node, ty),
                _ => false,
            })
        })
        .unwrap_or(false)
    }

    /// The viewport frame `id`'s document is laid out in, as of the last layout (the slot's
    /// content box at the parent's zoom). None for an unknown frame; the page's own
    /// viewport for [`FrameId::TOP`] is whatever the host passed to [`Document::layout`].
    /// A frame that has not been laid out yet reads 300 x 150, like a fresh iframe.
    pub fn frame_viewport(&self, id: FrameId) -> Option<Viewport> {
        crate::frames::viewport_of(self, id)
    }

    /// Tell the document a message arrived (`postMessage`): the `message` event is fired at
    /// its window with `data` parsed from JSON, `origin` and `source`. The host has checked
    /// `targetOrigin`. True if a script ran.
    pub fn deliver_message(&mut self, host: &mut dyn ScriptHost, msg: &Message) -> bool {
        crate::script::deliver_message(self, host, msg)
    }

    /// The messages scripts of this document posted since the last call, in order: the host
    /// checks each one's `target_origin`, finds the target frame's document and calls
    /// [`Document::deliver_message`] on it.
    pub fn take_messages(&mut self) -> Vec<OutMessage> {
        match self.js.as_mut() {
            Some(j) => j.take_messages(),
            None => Vec::new(),
        }
    }

    /// The frame that has the keyboard (the page, or a frame the user clicked in).
    pub fn focused_frame(&self) -> FrameId {
        self.frames
            .as_ref()
            .map(|f| f.focused)
            .unwrap_or(FrameId::TOP)
    }

    /// Give a frame the keyboard (unknown ids are ignored). The engine keeps the state; the
    /// host moves each document's own focus ([`Document::set_focus`]) to match, which fires
    /// the blur and focus events.
    pub fn set_focused_frame(&mut self, id: FrameId) {
        if let Some(f) = self.frames.as_mut()
            && (id.is_top() || f.slot(id).is_some_and(|s| s.doc.is_some()))
        {
            f.focused = id;
        }
    }

    /// The scroll offset of frame `id`'s document ([`FrameId::TOP`] is the host's own).
    pub fn frame_scroll(&self, id: FrameId) -> Option<(i32, i32)> {
        self.frames.as_ref()?.slot(id).map(|s| s.scroll)
    }

    /// Scroll frame `id`'s document to (`x`, `y`), clamped to its scrollable area (and pinned
    /// at 0 by `scrolling=no`); the offset it ended at. The next [`Document::layout`] moves
    /// the frame's content.
    pub fn set_frame_scroll(&mut self, id: FrameId, x: i32, y: i32) -> (i32, i32) {
        crate::frames::set_scroll(self, id, x, y)
    }

    /// The wheel (or a drag) at viewport point (`x`, `y`) of the top document, with the page
    /// scrolled by `scroll`: scroll the innermost frame under the pointer by (`dx`, `dy`)
    /// as far as it can, hand what is left to its parent frame, and so on up; returns what
    /// is left for the page itself (the host scrolls the page by it).
    pub fn scroll_frames_at(
        &mut self,
        x: i32,
        y: i32,
        scroll: (i32, i32),
        dx: i32,
        dy: i32,
    ) -> (i32, i32) {
        crate::frames::scroll_at(self, x, y, scroll, dx, dy)
    }

    /// Scroll so that `rect`, a rectangle in frame `id`'s document space, is in view: the frame
    /// and the frames above it move as far as needed. Returns the rectangle in the top
    /// document's space, for the host to scroll the page to. For find, `#fragment`, focus and
    /// a script's `scrollIntoView`/`scrollTo`.
    pub fn reveal(&mut self, id: FrameId, rect: Rect) -> Rect {
        crate::frames::reveal(self, id, rect)
    }

    /// Scroll frame `id` to the element named by `fragment` (as [`Document::fragment_y`]
    /// does for the page); true if it was found. Before the frame's first layout nothing can
    /// be found: the fragment is kept and applied then (and true says only that there is a
    /// document). The same goes for [`Document::set_frame_scroll`]'s offset.
    pub fn frame_fragment(&mut self, id: FrameId, fragment: &str) -> bool {
        crate::frames::fragment(self, id, fragment)
    }

    /// The controls of the page and of all its frames in Tab order: the page's controls in
    /// document order, a frame's controls at the place of its `<iframe>`. Only controls that
    /// have a box and are not disabled.
    pub fn focus_order(&self) -> Vec<(FrameId, ControlId)> {
        crate::frames::focus_order(self)
    }
}
