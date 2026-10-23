//! The page renderer, as the Browser sees it: `parse(bytes, url)` gives a [`Document`],
//! [`Document::layout`] at a width gives a [`DisplayList`] (rectangles, text runs, images,
//! borders and form controls, in page pixels), and [`Document::hit_test`] says what is under
//! a point (a link or a form control). The app paints the list (crate::paint), measures
//! text and knows image sizes for the renderer through [`Host`].
//!
//! This is the app side of the boundary with `user/webengine` (its `src/api.rs`). Until that
//! engine is merged, [`stub`] is a small renderer of its own behind the same shape: an HTML
//! subset (headings, paragraphs, lists, quotes, pre, tables as rows, links, bold / italic /
//! code, images, forms with text, password, checkbox, radio, select, textarea and buttons),
//! no CSS. Swapping engines is this module: [`parse`] and the types below.

pub mod stub;

use alloc::string::String;
use alloc::vec::Vec;

pub use stub::Document;

/// A pixel colour, 0x00RRGGBB.
pub type Color = u32;

/// How text looks. `size` is the font size in pixels (16 = body text).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Font {
    pub size: i32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

impl Font {
    pub const BODY: Font = Font {
        size: 16,
        bold: false,
        italic: false,
        mono: false,
    };
}

impl Default for Font {
    fn default() -> Font {
        Font::BODY
    }
}

/// What the renderer asks the app for while laying out.
pub trait Host {
    /// The width of `text` in `font`, in pixels.
    fn text_width(&self, text: &str, font: Font) -> i32;
    /// The height of a line of `font`, in pixels.
    fn line_height(&self, font: Font) -> i32;
    /// The natural size of the image at `url` (absolute), once it has loaded.
    fn image_size(&self, url: &str) -> Option<(i32, i32)>;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
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

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// One thing to paint, in page coordinates (0, 0 = the page's top left).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Fill {
        rect: Rect,
        color: Color,
    },
    /// A run of text whose top left is (x, y), `width` wide.
    Text {
        x: i32,
        y: i32,
        width: i32,
        text: String,
        font: Font,
        color: Color,
        underline: bool,
    },
    /// An image (absolute address) drawn into `rect`; `alt` when it isn't loaded.
    Image {
        rect: Rect,
        url: String,
        alt: String,
    },
    Border {
        rect: Rect,
        width: i32,
        color: Color,
    },
    /// Form control `id` ([`Document::controls`]): the app draws it in its current state.
    Control {
        rect: Rect,
        id: usize,
    },
}

#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub width: i32,
    pub height: i32,
    pub background: Color,
    pub items: Vec<Item>,
}

/// What a click at a point lands on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    /// A link (its `href` as written in the page).
    Link(String),
    /// Form control `id`.
    Control(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlKind {
    Text,
    Password,
    Checkbox,
    Radio,
    Select,
    TextArea,
    Submit,
    Reset,
    Button,
    Hidden,
}

impl ControlKind {
    /// Does it take typing?
    pub fn is_text(self) -> bool {
        matches!(
            self,
            ControlKind::Text | ControlKind::Password | ControlKind::TextArea
        )
    }
}

/// A form control and its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Control {
    pub kind: ControlKind,
    pub name: String,
    /// What is typed (text kinds), or what is sent when checked / pressed.
    pub value: String,
    /// A checkbox or radio that is on.
    pub checked: bool,
    /// A select's choices: (label, value).
    pub options: Vec<(String, String)>,
    pub selected: usize,
    /// Its form (index into the document's forms), if it is in one.
    pub form: Option<usize>,
    /// A button's label, or a text field's placeholder.
    pub label: String,
    pub disabled: bool,
    /// Visible size: columns, and rows for a textarea.
    pub cols: i32,
    pub rows: i32,
    /// The most characters it takes (maxlength), 0 = no limit.
    pub max_len: usize,
}

/// A `<form>`: where it goes and how.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub action: String,
    pub method: milkweb::http::Method,
}

/// Parse a page. `content_type` is the response's (text/plain shows as is), `url` the page's
/// address (links and images are relative to it, or to its `<base>`).
pub fn parse(bytes: &[u8], content_type: &str, url: &str) -> Document {
    stub::parse(bytes, content_type, url)
}
