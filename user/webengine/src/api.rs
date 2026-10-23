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
//! - Not supported (yet): scripts, `:hover`/`:focus` styles, CSS transforms, animations,
//!   web fonts, named grid lines, inner scrolling boxes (overflow: auto spills sideways and
//!   clips vertically), bidirectional text, vertical writing modes.
//!
//! # CHANGES
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
/// [`Family::Serif`], as in every browser.
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

// ---- The document -------------------------------------------------------------------------

/// A parsed page. Not tied to any I/O; all inputs come in through the methods.
pub struct Document {
    pub(crate) page: crate::page::Page,
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
        }
    }

    /// Parse a string of HTML (no charset step).
    pub fn parse_str(html: &str, url: &str) -> Document {
        Document {
            page: crate::page::Page::parse_str(html, url, Limits::default()),
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
