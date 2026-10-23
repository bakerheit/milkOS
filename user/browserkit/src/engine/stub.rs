//! The stand-in renderer (see the module above): tag soup in, blocks of styled inline pieces,
//! laid out top to bottom with greedy line breaking. Small on purpose; `user/webengine`
//! replaces it. Hostile pages are bounded: at most [`MAX_BLOCKS`] blocks, [`MAX_INLINES`]
//! pieces, [`MAX_CONTROLS`] controls, list nesting [`MAX_DEPTH`], and layout stops at
//! [`MAX_HEIGHT`] pixels.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use milkweb::html::{attr, decode_bytes, decode_entities, is_html};
use milkweb::http::Method;
use milkweb::url::Url;

use super::{Color, Control, ControlKind, DisplayList, Font, Form, Hit, Host, Item, Rect};
use crate::form::Submission;

pub const MAX_BLOCKS: usize = 20_000;
pub const MAX_INLINES: usize = 200_000;
pub const MAX_CONTROLS: usize = 2_000;
pub const MAX_DEPTH: usize = 16;
pub const MAX_HEIGHT: i32 = 400_000;

pub const BACKGROUND: Color = 0xFFFDF8;
pub const TEXT: Color = 0x3A2E28;
/// Link text on the page's milk background (the light theme's link token, SPEC "Type"):
/// the app sets its theme's with [`Document::set_link_color`].
pub const LINK: Color = 0x4F64A0;
pub const MUTED: Color = 0x6B5846;
pub const RULE: Color = 0xE2D5BD;
pub const CODE_BG: Color = 0xF3ECDF;
pub const PAD: i32 = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Style {
    bold: bool,
    italic: bool,
    mono: bool,
    underline: bool,
    small: bool,
    link: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Inline {
    Text(String, Style),
    Break,
    Image {
        url: String,
        alt: String,
        w: Option<i32>,
        h: Option<i32>,
        link: Option<usize>,
    },
    Control(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Para,
    Heading(u8),
    /// A list item: its marker (None = a bullet).
    Item(Option<u32>),
    Pre,
    Rule,
    /// A table row (its cells are in `cells`).
    Row {
        header: bool,
    },
}

#[derive(Clone, Debug)]
struct Block {
    kind: Kind,
    /// Left indent in levels (lists, quotes).
    depth: i32,
    quote: bool,
    inlines: Vec<Inline>,
    cells: Vec<Vec<Inline>>,
    /// Names that point here (`id=`, `<a name=>`), for `#fragment`s.
    anchors: Vec<String>,
    /// The `<main>` element starts here.
    main: bool,
    /// Column count of the table this row is in.
    table_cols: usize,
}

impl Block {
    fn new(kind: Kind, depth: i32, quote: bool) -> Block {
        Block {
            kind,
            depth,
            quote,
            inlines: Vec::new(),
            cells: Vec::new(),
            anchors: Vec::new(),
            main: false,
            table_cols: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.inlines
            .iter()
            .all(|i| matches!(i, Inline::Text(t, _) if t.trim().is_empty()))
            && self.cells.is_empty()
            && !matches!(self.kind, Kind::Rule)
    }
}

/// A parsed page.
#[derive(Clone, Debug)]
pub struct Document {
    url: Option<Url>,
    base: String,
    title: String,
    blocks: Vec<Block>,
    links: Vec<String>,
    controls: Vec<Control>,
    forms: Vec<Form>,
    images: Vec<String>,
    // The last layout.
    list: DisplayList,
    link_boxes: Vec<(Rect, usize)>,
    control_boxes: Vec<(Rect, usize)>,
    anchor_y: Vec<(String, i32)>,
    main_y: Option<i32>,
    link_color: Color,
}

// ---- parsing ------------------------------------------------------------------------------------

struct Builder {
    doc: Document,
    cur: Block,
    styles: Vec<(String, Style)>,
    lists: Vec<Option<u32>>,
    quote: i32,
    pre: usize,
    form: Option<usize>,
    /// Inside a `<select>`: its control.
    select: Option<usize>,
    /// Inside an `<option>`: its label so far and value.
    option: Option<(String, Option<String>)>,
    /// Inside a `<button>`: its control.
    button: Option<usize>,
    /// Inside a table cell.
    cell: bool,
    table_cols: usize,
    in_title: bool,
    pending_anchors: Vec<String>,
    pending_main: bool,
    inline_count: usize,
}

const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "aside",
    "dl",
    "dt",
    "dd",
    "figure",
    "figcaption",
    "address",
    "center",
    "details",
    "summary",
    "fieldset",
    "legend",
    "noscript",
    "menu",
    "hgroup",
    "search",
    "dialog",
    "caption",
    "body",
    "html",
    "form",
    "main",
];

const SKIP: &[&str] = &[
    "script", "style", "template", "svg", "math", "iframe", "object", "noembed", "canvas", "video",
    "audio",
];

fn style_for(tag: &str, mut s: Style) -> Option<Style> {
    match tag {
        "b" | "strong" | "th" => s.bold = true,
        "i" | "em" | "cite" | "var" | "dfn" => s.italic = true,
        "code" | "kbd" | "samp" | "tt" => s.mono = true,
        "u" | "ins" => s.underline = true,
        "small" | "sub" | "sup" => s.small = true,
        "span" | "font" | "abbr" | "mark" | "q" | "s" | "del" | "strike" | "big" | "label"
        | "time" | "data" | "bdi" | "bdo" | "nobr" => {}
        _ => return None,
    }
    Some(s)
}

impl Builder {
    fn style(&self) -> Style {
        self.styles.last().map_or(Style::default(), |s| s.1)
    }

    fn depth(&self) -> i32 {
        self.lists.len().min(MAX_DEPTH) as i32 + self.quote.min(MAX_DEPTH as i32)
    }

    /// End the current block and start one of `kind`.
    fn flush(&mut self, kind: Kind) {
        let depth = self.depth();
        let quote = self.quote > 0;
        let next = Block::new(kind, depth, quote);
        let done = core::mem::replace(&mut self.cur, next);
        if !done.is_empty() && self.doc.blocks.len() < MAX_BLOCKS {
            self.doc.blocks.push(done);
        } else if !done.anchors.is_empty() || done.main {
            // Keep where anchors pointed.
            self.cur.anchors.extend(done.anchors);
            self.cur.main |= done.main;
        }
        let a = core::mem::take(&mut self.pending_anchors);
        self.cur.anchors.extend(a);
        if core::mem::take(&mut self.pending_main) {
            self.cur.main = true;
        }
    }

    fn push(&mut self, i: Inline) {
        if self.inline_count >= MAX_INLINES {
            return;
        }
        self.inline_count += 1;
        if self.cell && matches!(self.cur.kind, Kind::Row { .. }) {
            if self.cur.cells.is_empty() {
                self.cur.cells.push(Vec::new());
            }
            self.cur.cells.last_mut().unwrap().push(i);
        } else {
            self.cur.inlines.push(i);
        }
    }

    fn text(&mut self, raw: &str) {
        if self.in_title {
            if self.doc.title.len() < 500 {
                self.doc.title.push_str(&decode_entities(raw));
            }
            return;
        }
        let t = decode_entities(raw);
        if let Some((label, _)) = &mut self.option {
            if label.len() < 200 {
                label.push_str(&t);
            }
            return;
        }
        if self.select.is_some() {
            return;
        }
        if let Some(b) = self.button {
            let c = &mut self.doc.controls[b];
            if c.label.len() < 200 {
                c.label.push_str(&t);
            }
            return;
        }
        let s = self.style();
        if self.pre > 0 {
            self.push(Inline::Text(t, s));
            return;
        }
        // Collapse white space (the line breaker splits at the spaces).
        let mut out = String::with_capacity(t.len());
        let mut sp = false;
        for c in t.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                sp = true;
            } else {
                if sp {
                    out.push(' ');
                }
                sp = false;
                out.push(c);
            }
        }
        if sp {
            out.push(' ');
        }
        if !out.is_empty() {
            self.push(Inline::Text(out, s));
        }
    }

    fn base_url(&self) -> Option<Url> {
        match &self.doc.url {
            Some(u) => u.join(&self.doc.base).ok().or_else(|| Some(u.clone())),
            None => Url::parse(&self.doc.base).ok(),
        }
    }

    fn abs(&self, href: &str) -> String {
        match self.base_url() {
            Some(b) => b
                .join(href)
                .map_or_else(|_| href.to_string(), |u| u.to_text()),
            None => href.to_string(),
        }
    }

    fn control(&mut self, c: Control) -> Option<usize> {
        if self.doc.controls.len() >= MAX_CONTROLS {
            return None;
        }
        self.doc.controls.push(c);
        Some(self.doc.controls.len() - 1)
    }

    fn start(&mut self, name: &str, tag: &str) {
        if let Some(id) = attr(tag, "id").filter(|s| !s.is_empty()) {
            self.pending_anchors.push(id);
        }
        if name == "main" || attr(tag, "role").is_some_and(|r| r.eq_ignore_ascii_case("main")) {
            self.pending_main = true;
        }
        match name {
            "title" => self.in_title = true,
            "base" => {
                if let Some(h) = attr(tag, "href") {
                    self.doc.base = h;
                }
            }
            "br" => self.push(Inline::Break),
            "hr" => {
                self.flush(Kind::Rule);
                self.flush(Kind::Para);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush(Kind::Heading(name.as_bytes()[1] - b'0'));
            }
            "pre" | "listing" | "xmp" | "plaintext" => {
                self.pre += 1;
                self.flush(Kind::Pre);
            }
            "blockquote" => {
                self.quote += 1;
                self.flush(Kind::Para);
            }
            "ul" | "ol" => {
                if self.lists.len() < 64 {
                    let start = attr(tag, "start").and_then(|s| s.trim().parse::<u32>().ok());
                    self.lists.push(if name == "ol" {
                        Some(start.unwrap_or(1))
                    } else {
                        None
                    });
                }
                self.flush(Kind::Para);
            }
            "li" => {
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = *n;
                        *n = n.saturating_add(1);
                        Some(m)
                    }
                    _ => None,
                };
                self.flush(Kind::Item(marker));
            }
            "table" => {
                self.flush(Kind::Para);
                self.table_cols = 0;
            }
            "tr" => {
                self.cell = false;
                self.flush(Kind::Row { header: false });
            }
            "td" | "th" => {
                if !matches!(self.cur.kind, Kind::Row { .. }) {
                    self.flush(Kind::Row { header: false });
                }
                if self.cur.cells.len() < 32 {
                    self.cur.cells.push(Vec::new());
                }
                if name == "th" {
                    self.cur.kind = Kind::Row { header: true };
                }
                self.cell = true;
                self.table_cols = self.table_cols.max(self.cur.cells.len());
            }
            "a" => {
                let mut s = self.style();
                if let Some(h) = attr(tag, "href") {
                    self.doc.links.push(h);
                    s.link = Some(self.doc.links.len() - 1);
                }
                if let Some(n) = attr(tag, "name").filter(|s| !s.is_empty()) {
                    self.pending_anchors.push(n);
                    let a = core::mem::take(&mut self.pending_anchors);
                    self.cur.anchors.extend(a);
                }
                self.styles.push((String::from("a"), s));
            }
            "img" => {
                let src = attr(tag, "src").unwrap_or_default();
                let alt = attr(tag, "alt").unwrap_or_default();
                let dim = |n| {
                    attr(tag, n)
                        .and_then(|v| v.trim().trim_end_matches("px").parse::<i32>().ok())
                        .filter(|v| *v > 0)
                        .map(|v| v.min(4000))
                };
                let url = if src.is_empty() {
                    String::new()
                } else {
                    self.abs(&src)
                };
                if !url.is_empty() && !self.doc.images.contains(&url) && self.doc.images.len() < 200
                {
                    self.doc.images.push(url.clone());
                }
                let link = self.style().link;
                self.push(Inline::Image {
                    url,
                    alt,
                    w: dim("width"),
                    h: dim("height"),
                    link,
                });
            }
            "form" => {
                self.flush(Kind::Para);
                let method = match attr(tag, "method") {
                    Some(m) if m.eq_ignore_ascii_case("post") => Method::Post,
                    _ => Method::Get,
                };
                if self.doc.forms.len() < 500 {
                    self.doc.forms.push(Form {
                        action: attr(tag, "action").unwrap_or_default(),
                        method,
                    });
                    self.form = Some(self.doc.forms.len() - 1);
                }
            }
            "input" => self.input(tag),
            "textarea" => {} // handled as raw text by the tokenizer
            "select" => {
                let c = self.new_control(ControlKind::Select, tag);
                self.select = self.control(c);
                if let Some(id) = self.select {
                    self.push(Inline::Control(id));
                }
            }
            "option" => {
                self.end_option();
                if self.select.is_some() {
                    let selected = attr(tag, "selected").is_some();
                    self.option = Some((String::new(), attr(tag, "value")));
                    if selected && let Some(id) = self.select {
                        let n = self.doc.controls[id].options.len();
                        self.doc.controls[id].selected = n;
                    }
                }
            }
            "button" => {
                let kind = match attr(tag, "type").map(|t| t.to_ascii_lowercase()).as_deref() {
                    Some("reset") => ControlKind::Reset,
                    Some("button") => ControlKind::Button,
                    _ => ControlKind::Submit,
                };
                let c = self.new_control(kind, tag);
                self.button = self.control(c);
                if let Some(id) = self.button {
                    self.push(Inline::Control(id));
                }
            }
            _ => {
                if BLOCK_TAGS.contains(&name) {
                    self.flush(Kind::Para);
                } else if let Some(s) = style_for(name, self.style())
                    && self.styles.len() < 256
                {
                    self.styles.push((String::from(name), s));
                }
            }
        }
        if !self.pending_anchors.is_empty() && !BLOCK_TAGS.contains(&name) {
            let a = core::mem::take(&mut self.pending_anchors);
            self.cur.anchors.extend(a);
        }
    }

    fn new_control(&self, kind: ControlKind, tag: &str) -> Control {
        let num = |n: &str, d: i32| {
            attr(tag, n)
                .and_then(|v| v.trim().parse::<i32>().ok())
                .filter(|v| *v > 0)
                .map_or(d, |v| v.min(200))
        };
        Control {
            kind,
            name: attr(tag, "name").unwrap_or_default(),
            value: attr(tag, "value").unwrap_or_default(),
            checked: attr(tag, "checked").is_some(),
            options: Vec::new(),
            selected: 0,
            form: self.form,
            label: attr(tag, "placeholder").unwrap_or_default(),
            disabled: attr(tag, "disabled").is_some(),
            cols: num(
                if kind == ControlKind::TextArea {
                    "cols"
                } else {
                    "size"
                },
                20,
            ),
            rows: num("rows", 3),
            max_len: attr(tag, "maxlength")
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0),
        }
    }

    fn input(&mut self, tag: &str) {
        let ty = attr(tag, "type").unwrap_or_default().to_ascii_lowercase();
        let kind = match ty.as_str() {
            "password" => ControlKind::Password,
            "checkbox" => ControlKind::Checkbox,
            "radio" => ControlKind::Radio,
            "submit" | "image" => ControlKind::Submit,
            "reset" => ControlKind::Reset,
            "button" => ControlKind::Button,
            "hidden" => ControlKind::Hidden,
            // file, color, range, date...: typed as text (a known gap).
            _ => ControlKind::Text,
        };
        let mut c = self.new_control(kind, tag);
        match kind {
            ControlKind::Submit if c.value.is_empty() => c.label = String::from("Submit"),
            ControlKind::Reset if c.value.is_empty() => c.label = String::from("Reset"),
            ControlKind::Submit | ControlKind::Reset | ControlKind::Button => {
                c.label = c.value.clone()
            }
            ControlKind::Checkbox | ControlKind::Radio if c.value.is_empty() => {
                c.value = String::from("on")
            }
            _ => {}
        }
        if c.max_len > 0 && c.kind.is_text() {
            c.value = c.value.chars().take(c.max_len).collect();
        }
        if let Some(id) = self.control(c)
            && kind != ControlKind::Hidden
        {
            self.push(Inline::Control(id));
        }
    }

    fn textarea(&mut self, tag: &str, content: &str) {
        let mut c = self.new_control(ControlKind::TextArea, tag);
        // A newline right after the start tag is dropped.
        let v = decode_entities(content);
        c.value = String::from(v.strip_prefix('\n').unwrap_or(&v));
        if let Some(id) = self.control(c) {
            self.push(Inline::Control(id));
        }
    }

    fn end_option(&mut self) {
        if let (Some((label, value)), Some(id)) = (self.option.take(), self.select) {
            let label = String::from(label.trim());
            let value = value.unwrap_or_else(|| label.clone());
            let c = &mut self.doc.controls[id];
            if c.options.len() < 500 {
                c.options.push((label, value));
            }
        }
    }

    fn end(&mut self, name: &str) {
        match name {
            "title" => self.in_title = false,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "li" | "dt" | "dd" => {
                self.flush(Kind::Para)
            }
            "pre" | "listing" | "xmp" => {
                self.pre = self.pre.saturating_sub(1);
                self.flush(Kind::Para);
            }
            "blockquote" => {
                self.quote = (self.quote - 1).max(0);
                self.flush(Kind::Para);
            }
            "ul" | "ol" => {
                self.lists.pop();
                self.flush(Kind::Para);
            }
            "td" | "th" => self.cell = false,
            "tr" => {
                self.cell = false;
                self.cur.table_cols = self.table_cols;
            }
            "table" => {
                self.cell = false;
                if matches!(self.cur.kind, Kind::Row { .. }) {
                    self.cur.table_cols = self.table_cols;
                }
                // Every row of the table gets the widest row's column count.
                let cols = self.table_cols;
                for b in self.doc.blocks.iter_mut().rev() {
                    match b.kind {
                        Kind::Row { .. } if b.table_cols <= cols => b.table_cols = cols.max(1),
                        _ => break,
                    }
                }
                self.flush(Kind::Para);
            }
            "form" => {
                self.flush(Kind::Para);
                self.form = None;
            }
            "select" => {
                self.end_option();
                if let Some(id) = self.select.take() {
                    let c = &mut self.doc.controls[id];
                    if c.selected >= c.options.len() {
                        c.selected = 0;
                    }
                }
            }
            "option" => self.end_option(),
            "button" => {
                if let Some(id) = self.button.take() {
                    let c = &mut self.doc.controls[id];
                    c.label = String::from(c.label.trim());
                    if c.label.is_empty() {
                        c.label = String::from("Submit");
                    }
                }
            }
            _ => {
                if BLOCK_TAGS.contains(&name) {
                    self.flush(Kind::Para);
                } else if let Some(i) = self.styles.iter().rposition(|(n, _)| n == name) {
                    self.styles.truncate(i);
                }
            }
        }
    }
}

/// Find `</name` (any case) at or after `from`: the index of the `<`.
fn find_close(s: &str, from: usize, name: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut k = from;
    while let Some(i) = s.get(k..)?.find("</") {
        let at = k + i;
        let end = at + 2 + name.len();
        if end <= b.len()
            && s.is_char_boundary(end)
            && s[at + 2..end].eq_ignore_ascii_case(name)
            && b.get(end)
                .is_none_or(|c| *c == b'>' || c.is_ascii_whitespace() || *c == b'/')
        {
            return Some(at);
        }
        k = at + 2;
    }
    None
}

/// The tag name at the start of a tag's inside, lower case.
fn tag_name(inside: &str) -> String {
    inside
        .trim_start_matches('/')
        .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub fn parse(bytes: &[u8], content_type: &str, url: &str) -> Document {
    let text = decode_bytes(bytes, content_type);
    let page_url = Url::parse(url).ok();
    let mut b = Builder {
        doc: Document {
            url: page_url,
            base: String::new(),
            title: String::new(),
            blocks: Vec::new(),
            links: Vec::new(),
            controls: Vec::new(),
            forms: Vec::new(),
            images: Vec::new(),
            list: DisplayList::default(),
            link_boxes: Vec::new(),
            control_boxes: Vec::new(),
            anchor_y: Vec::new(),
            main_y: None,
            link_color: LINK,
        },
        cur: Block::new(Kind::Para, 0, false),
        styles: Vec::new(),
        lists: Vec::new(),
        quote: 0,
        pre: 0,
        form: None,
        select: None,
        option: None,
        button: None,
        cell: false,
        table_cols: 0,
        in_title: false,
        pending_anchors: Vec::new(),
        pending_main: false,
        inline_count: 0,
    };
    if !is_html(content_type, &text) {
        b.flush(Kind::Pre);
        b.pre = 1;
        b.push(Inline::Text(
            text.replace('\t', "    "),
            Style {
                mono: true,
                ..Style::default()
            },
        ));
        b.flush(Kind::Para);
        return b.doc;
    }
    let s = text.as_str();
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut text_from = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let next = bytes.get(i + 1).copied().unwrap_or(0);
        if !(next.is_ascii_alphabetic() || next == b'/' || next == b'!' || next == b'?') {
            i += 1;
            continue;
        }
        if text_from < i {
            b.text(&s[text_from..i]);
        }
        // Comments, doctype, processing instructions.
        if s[i..].starts_with("<!--") {
            i = s[i + 4..]
                .find("-->")
                .map_or(bytes.len(), |e| i + 4 + e + 3);
            text_from = i;
            continue;
        }
        if next == b'!' || next == b'?' {
            i = s[i..].find('>').map_or(bytes.len(), |e| i + e + 1);
            text_from = i;
            continue;
        }
        // The tag's inside, up to a '>' outside quotes.
        let mut j = i + 1;
        let mut q = 0u8;
        while j < bytes.len() {
            let c = bytes[j];
            if q != 0 {
                if c == q {
                    q = 0;
                }
            } else if c == b'"' || c == b'\'' {
                q = c;
            } else if c == b'>' {
                break;
            }
            j += 1;
        }
        let inside = &s[i + 1..j.min(bytes.len())];
        i = (j + 1).min(bytes.len());
        text_from = i;
        let name = tag_name(inside);
        if name.is_empty() {
            continue;
        }
        if inside.starts_with('/') {
            b.end(&name);
            continue;
        }
        // Raw text: skipped elements, textarea's value, title.
        if SKIP.contains(&name.as_str()) && !inside.ends_with('/') {
            i = find_close(s, i, &name).map_or(bytes.len(), |e| {
                s[e..].find('>').map_or(bytes.len(), |g| e + g + 1)
            });
            text_from = i;
            continue;
        }
        if name == "textarea" {
            let end = find_close(s, i, "textarea").unwrap_or(bytes.len());
            b.textarea(inside, &s[i..end]);
            i = s[end..].find('>').map_or(bytes.len(), |g| end + g + 1);
            text_from = i;
            continue;
        }
        if name == "title" {
            let end = find_close(s, i, "title").unwrap_or(bytes.len());
            if b.doc.title.is_empty() {
                b.doc.title = decode_entities(&s[i..end])
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                b.doc.title.truncate(
                    b.doc
                        .title
                        .char_indices()
                        .nth(300)
                        .map_or(b.doc.title.len(), |(k, _)| k),
                );
            }
            i = s[end..].find('>').map_or(bytes.len(), |g| end + g + 1);
            text_from = i;
            continue;
        }
        b.start(&name, inside);
    }
    if text_from < bytes.len() {
        b.text(&s[text_from..]);
    }
    b.end_option();
    b.flush(Kind::Para);
    b.doc
}

// ---- layout -------------------------------------------------------------------------------------

fn font_of(s: &Style, kind: Kind) -> Font {
    let (size, bold) = match kind {
        Kind::Heading(1) => (32, true),
        Kind::Heading(2) => (24, true),
        Kind::Heading(3) => (19, true),
        Kind::Heading(4) => (16, true),
        Kind::Heading(5) => (14, true),
        Kind::Heading(_) => (13, true),
        Kind::Row { header: true } => (16, true),
        _ => (16, false),
    };
    Font {
        size: if s.small && size == 16 { 13 } else { size },
        bold: bold || s.bold,
        italic: s.italic,
        mono: s.mono || kind == Kind::Pre,
    }
}

/// One piece of a line being built.
enum Piece {
    Text {
        text: String,
        w: i32,
        font: Font,
        color: Color,
        underline: bool,
        link: Option<usize>,
    },
    Image {
        w: i32,
        h: i32,
        url: String,
        alt: String,
        link: Option<usize>,
    },
    Control {
        w: i32,
        h: i32,
        id: usize,
    },
}

impl Piece {
    fn size(&self, host: &dyn Host) -> (i32, i32) {
        match self {
            Piece::Text { w, font, .. } => (*w, host.line_height(*font)),
            Piece::Image { w, h, .. } | Piece::Control { w, h, .. } => (*w, *h),
        }
    }
}

struct Flow<'a> {
    host: &'a dyn Host,
    out: &'a mut Vec<Item>,
    links: &'a mut Vec<(Rect, usize)>,
    controls: &'a mut Vec<(Rect, usize)>,
    x0: i32,
    width: i32,
    y: i32,
    x: i32,
    line: Vec<(i32, Piece)>,
    pre: bool,
    /// The line height of an empty line (a `<br>` on its own).
    min_h: i32,
    /// Where on the line the word being placed starts (pieces glued together move as one).
    group: usize,
}

impl Flow<'_> {
    fn end_line(&mut self, forced: bool) {
        // Spaces at the end of a line aren't drawn (or matched by find).
        if !self.pre {
            while let Some((_, Piece::Text { text, w, font, .. })) = self.line.last_mut() {
                let t = text.trim_end_matches(' ');
                if t.len() == text.len() {
                    break;
                }
                if t.is_empty() {
                    self.line.pop();
                    continue;
                }
                let cut = text.len() - t.len();
                *w -= self.host.text_width(&text[text.len() - cut..], *font);
                text.truncate(t.len());
                break;
            }
        }
        if self.line.is_empty() {
            if forced {
                self.y += self.min_h;
            }
            self.x = self.x0;
            return;
        }
        let h = self
            .line
            .iter()
            .map(|(_, p)| p.size(self.host).1)
            .max()
            .unwrap_or(self.min_h)
            .max(1);
        // The last run emitted on this line, and its link: neighbouring text in the same style
        // joins into one run (find in page matches across words).
        let mut last: Option<(usize, Option<usize>)> = None;
        for (x, p) in self.line.drain(..) {
            let (w, ph) = p.size(self.host);
            let top = self.y + h - ph;
            match p {
                Piece::Text {
                    text,
                    font,
                    color,
                    underline,
                    link,
                    ..
                } => {
                    if let Some((i, l)) = last
                        && l == link
                        && let Some(Item::Text {
                            x: lx,
                            y: ly,
                            width: lw,
                            text: lt,
                            font: lf,
                            color: lc,
                            underline: lu,
                        }) = self.out.get_mut(i)
                        && *lf == font
                        && *lc == color
                        && *lu == underline
                        && *ly == top
                        && *lx + *lw == x
                    {
                        lt.push_str(&text);
                        *lw += w;
                        if let Some(l) = link
                            && let Some((r, _)) = self.links.iter_mut().rev().find(|(_, k)| *k == l)
                        {
                            r.w += w;
                        }
                        continue;
                    }
                    if let Some(l) = link {
                        self.links.push((Rect::new(x, top, w, ph), l));
                    }
                    last = Some((self.out.len(), link));
                    self.out.push(Item::Text {
                        x,
                        y: top,
                        width: w,
                        text,
                        font,
                        color,
                        underline,
                    });
                }
                Piece::Image { url, alt, link, .. } => {
                    last = None;
                    let r = Rect::new(x, top, w, ph);
                    if let Some(l) = link {
                        self.links.push((r, l));
                    }
                    self.out.push(Item::Image { rect: r, url, alt });
                }
                Piece::Control { id, .. } => {
                    last = None;
                    let r = Rect::new(x, top, w, ph);
                    self.controls.push((r, id));
                    self.out.push(Item::Control { rect: r, id });
                }
            }
        }
        self.y += h;
        self.x = self.x0;
    }

    /// Put `p` on the line, starting a new line first if it doesn't fit. `glued`: no break
    /// is allowed before it (punctuation right after a link), so the word it is glued to moves
    /// down with it.
    fn place(&mut self, p: Piece, glued: bool) {
        let (w, _) = p.size(self.host);
        if !glued {
            self.group = self.line.len();
        }
        if self.x + w > self.x0 + self.width && self.x > self.x0 && !self.pre {
            if glued && self.group > 0 && self.group < self.line.len() {
                let moved: Vec<(i32, Piece)> = self.line.drain(self.group..).collect();
                self.end_line(false);
                let shift = moved[0].0 - self.x0;
                for (x, q) in moved {
                    self.x = x - shift + q.size(self.host).0;
                    self.line.push((x - shift, q));
                }
            } else {
                self.end_line(false);
            }
            self.group = 0;
        }
        let x = self.x;
        self.x += w;
        self.line.push((x, p));
    }

    fn text(&mut self, t: &str, font: Font, color: Color, underline: bool, link: Option<usize>) {
        if self.pre {
            for (k, l) in t.split('\n').enumerate() {
                if k > 0 {
                    self.end_line(true);
                }
                let l = l.replace('\t', "    ");
                // Very long preformatted lines are broken at the width, not lost.
                let mut rest = l.as_str();
                while !rest.is_empty() {
                    let mut cut = rest.len();
                    let full = self.host.text_width(rest, font);
                    if self.x + full > self.x0 + self.width && self.width > 0 {
                        let avail = (self.x0 + self.width - self.x).max(8);
                        let per = (full / rest.chars().count().max(1) as i32).max(1);
                        let n = (avail / per).max(1) as usize;
                        cut = rest.char_indices().nth(n).map_or(rest.len(), |(i, _)| i);
                    }
                    let (a, b) = rest.split_at(cut);
                    let w = self.host.text_width(a, font);
                    self.place(
                        Piece::Text {
                            text: String::from(a),
                            w,
                            font,
                            color,
                            underline,
                            link,
                        },
                        false,
                    );
                    rest = b;
                    if !rest.is_empty() {
                        self.end_line(true);
                    }
                }
            }
            return;
        }
        let space = self.host.text_width(" ", font);
        // The first word sticks to what came before if no space separates them.
        let mut glue = !t.starts_with(' ')
            && matches!(self.line.last(), Some((_, Piece::Text { text, .. })) if !text.ends_with(' '));
        let mut first = true;
        for word in t.split(' ') {
            if !first {
                // A space, unless at the start of a line.
                if self.x > self.x0 {
                    if self.x + space > self.x0 + self.width {
                        self.end_line(false);
                    } else {
                        self.place(
                            Piece::Text {
                                text: String::from(" "),
                                w: space,
                                font,
                                color,
                                underline,
                                link,
                            },
                            false,
                        );
                    }
                }
            }
            first = false;
            if word.is_empty() {
                continue;
            }
            let mut w = self.host.text_width(word, font);
            let mut word = String::from(word);
            // A word wider than the whole line is cut.
            if w > self.width && self.width > 8 {
                while w > self.width && word.chars().count() > 1 {
                    let n = word.chars().count() * self.width as usize / w as usize;
                    let at = word
                        .char_indices()
                        .nth(n.max(1))
                        .map_or(word.len(), |(i, _)| i);
                    let rest = word.split_off(at);
                    let ww = self.host.text_width(&word, font);
                    if self.x > self.x0 {
                        self.end_line(false);
                    }
                    self.place(
                        Piece::Text {
                            text: word,
                            w: ww,
                            font,
                            color,
                            underline,
                            link,
                        },
                        false,
                    );
                    self.end_line(false);
                    word = rest;
                    w = self.host.text_width(&word, font);
                }
            }
            self.place(
                Piece::Text {
                    text: word,
                    w,
                    font,
                    color,
                    underline,
                    link,
                },
                glue,
            );
            glue = false;
        }
    }
}

/// The size a control is drawn at.
pub fn control_size(c: &Control, host: &dyn Host) -> (i32, i32) {
    let f = Font::BODY;
    let lh = host.line_height(f);
    let ch = host.text_width("m", f).max(1);
    match c.kind {
        ControlKind::Text | ControlKind::Password => (c.cols.clamp(4, 60) * ch + 16, lh + 10),
        ControlKind::TextArea => (c.cols.clamp(4, 80) * ch + 16, c.rows.clamp(1, 30) * lh + 10),
        ControlKind::Checkbox | ControlKind::Radio => (lh + 4, lh),
        ControlKind::Select => {
            let w = c
                .options
                .iter()
                .map(|(l, _)| host.text_width(l, f))
                .max()
                .unwrap_or(ch * 4)
                .min(400);
            (w + 36, lh + 10)
        }
        ControlKind::Submit | ControlKind::Reset | ControlKind::Button => (
            host.text_width(&c.label, Font { bold: true, ..f }).min(400) + 28,
            lh + 10,
        ),
        ControlKind::Hidden => (0, 0),
    }
}

impl Document {
    pub fn title(&self) -> &str {
        self.title.trim()
    }

    /// Absolute addresses of the images the page shows (to fetch, then lay out again).
    pub fn images(&self) -> &[String] {
        &self.images
    }

    pub fn controls(&self) -> &[Control] {
        &self.controls
    }

    pub fn forms(&self) -> &[Form] {
        &self.forms
    }

    /// The page's address for links: its `<base href>` against its own address.
    pub fn base_url(&self) -> Option<Url> {
        match &self.url {
            Some(u) if !self.base.is_empty() => u.join(&self.base).ok().or_else(|| Some(u.clone())),
            Some(u) => Some(u.clone()),
            None => None,
        }
    }

    /// The colour links are drawn in (they are always underlined).
    pub fn set_link_color(&mut self, c: Color) {
        self.link_color = c;
    }

    /// The last layout.
    pub fn display_list(&self) -> &DisplayList {
        &self.list
    }

    /// Lay the page out `width` pixels wide.
    pub fn layout(&mut self, width: i32, host: &dyn Host) -> &DisplayList {
        let mut items = Vec::new();
        let mut links = Vec::new();
        let mut controls = Vec::new();
        let mut anchors = Vec::new();
        let mut main_y = None;
        let inner = (width - 2 * PAD).max(40);
        let mut y = PAD;
        let body_lh = host.line_height(Font::BODY);
        let mut prev_row = false;
        for b in &self.blocks {
            if y > MAX_HEIGHT {
                break;
            }
            let is_row = matches!(b.kind, Kind::Row { .. });
            if prev_row && !is_row {
                y += 12;
            }
            prev_row = is_row;
            for a in &b.anchors {
                anchors.push((a.clone(), y));
            }
            if b.main && main_y.is_none() {
                main_y = Some(y);
            }
            let indent = b.depth.min(MAX_DEPTH as i32) * 24;
            let x0 = PAD + indent;
            let w = (inner - indent).max(40);
            match b.kind {
                Kind::Rule => {
                    items.push(Item::Fill {
                        rect: Rect::new(PAD, y + 4, inner, 1),
                        color: RULE,
                    });
                    y += 21;
                    continue;
                }
                Kind::Row { header } => {
                    let cols = b.table_cols.max(b.cells.len()).max(1) as i32;
                    let cw = (w / cols).max(20);
                    let top = y;
                    let mut bottom = y;
                    let first = items.len();
                    for (k, cell) in b.cells.iter().enumerate() {
                        let cx = x0 + k as i32 * cw;
                        let yy = self.flow_into(
                            cell,
                            b.kind,
                            cx + 4,
                            cw - 8,
                            top + 4,
                            host,
                            &mut items,
                            &mut links,
                            &mut controls,
                        );
                        bottom = bottom.max(yy + 4);
                    }
                    if bottom == top {
                        bottom = top + body_lh + 8;
                    }
                    if header {
                        let r = Rect::new(x0, top, cw * cols, bottom - top);
                        items.insert(
                            first,
                            Item::Fill {
                                rect: r,
                                color: CODE_BG,
                            },
                        );
                    }
                    items.push(Item::Fill {
                        rect: Rect::new(x0, bottom, cw * cols, 1),
                        color: RULE,
                    });
                    y = bottom + 1;
                    continue;
                }
                _ => {}
            }
            let (gap_before, gap_after) = match b.kind {
                Kind::Heading(1) => (8, 14),
                Kind::Heading(2) => (8, 12),
                Kind::Heading(_) => (6, 8),
                Kind::Item(_) => (0, 6),
                Kind::Pre => (0, 14),
                _ => (0, 12),
            };
            y += gap_before;
            let start = y;
            if let Kind::Item(marker) = b.kind {
                let f = Font::BODY;
                match marker {
                    None => {
                        let r = (body_lh / 6).max(2);
                        items.push(Item::Fill {
                            rect: Rect::new(x0 - 14, y + body_lh / 2 - r, 2 * r, 2 * r),
                            color: MUTED,
                        });
                    }
                    Some(n) => {
                        let t = alloc::format!("{n}.");
                        let tw = host.text_width(&t, f);
                        items.push(Item::Text {
                            x: x0 - tw - 6,
                            y,
                            width: tw,
                            text: t,
                            font: f,
                            color: MUTED,
                            underline: false,
                        });
                    }
                }
            }
            let pre_box = if b.kind == Kind::Pre {
                let i = items.len();
                items.push(Item::Fill {
                    rect: Rect::default(),
                    color: CODE_BG,
                });
                Some(i)
            } else {
                None
            };
            let (fx, fw, fy) = if pre_box.is_some() {
                (x0 + 10, w - 20, y + 8)
            } else {
                (x0, w, y)
            };
            y = self.flow_into(
                &b.inlines,
                b.kind,
                fx,
                fw,
                fy,
                host,
                &mut items,
                &mut links,
                &mut controls,
            );
            if let Some(i) = pre_box {
                y += 8;
                items[i] = Item::Fill {
                    rect: Rect::new(x0, start, w, y - start),
                    color: CODE_BG,
                };
            }
            if b.quote {
                items.push(Item::Fill {
                    rect: Rect::new(x0 - 14, start, 3, (y - start).max(4)),
                    color: RULE,
                });
            }
            y += gap_after;
        }
        y += PAD;
        self.list = DisplayList {
            width,
            height: y.max(PAD * 2),
            background: BACKGROUND,
            items,
        };
        self.link_boxes = links;
        self.control_boxes = controls;
        self.anchor_y = anchors;
        self.main_y = main_y;
        &self.list
    }

    #[allow(clippy::too_many_arguments)]
    fn flow_into(
        &self,
        inlines: &[Inline],
        kind: Kind,
        x0: i32,
        width: i32,
        y: i32,
        host: &dyn Host,
        items: &mut Vec<Item>,
        links: &mut Vec<(Rect, usize)>,
        controls: &mut Vec<(Rect, usize)>,
    ) -> i32 {
        let mut f = Flow {
            host,
            out: items,
            links,
            controls,
            x0,
            width,
            y,
            x: x0,
            line: Vec::new(),
            pre: kind == Kind::Pre,
            group: 0,
            min_h: host.line_height(font_of(&Style::default(), kind)),
        };
        for i in inlines {
            if f.y > MAX_HEIGHT {
                break;
            }
            match i {
                Inline::Text(t, s) => {
                    let font = font_of(s, kind);
                    let color = if s.link.is_some() {
                        self.link_color
                    } else {
                        TEXT
                    };
                    f.text(t, font, color, s.underline || s.link.is_some(), s.link);
                }
                Inline::Break => f.end_line(true),
                Inline::Image {
                    url,
                    alt,
                    w,
                    h,
                    link,
                } => {
                    let natural = host.image_size(url);
                    let (iw, ih) = match (natural, *w, *h) {
                        (_, Some(w), Some(h)) => (w, h),
                        (Some((nw, nh)), Some(w), None) => {
                            (w, (nh as i64 * w as i64 / nw.max(1) as i64) as i32)
                        }
                        (Some((nw, nh)), None, Some(h)) => {
                            ((nw as i64 * h as i64 / nh.max(1) as i64) as i32, h)
                        }
                        (Some((nw, nh)), None, None) => (nw, nh),
                        (None, w, h) => {
                            if alt.is_empty() && w.is_none() {
                                continue;
                            }
                            let aw = host.text_width(alt, Font::BODY) + 12;
                            (
                                w.unwrap_or(aw.min(width)),
                                h.unwrap_or(host.line_height(Font::BODY) + 8),
                            )
                        }
                    };
                    // Wider than the column: scaled down to fit.
                    let (iw, ih) = if iw > width && iw > 0 {
                        (width, (ih as i64 * width as i64 / iw as i64) as i32)
                    } else {
                        (iw, ih)
                    };
                    f.place(
                        Piece::Image {
                            w: iw.clamp(1, 4000),
                            h: ih.clamp(1, 4000),
                            url: url.clone(),
                            alt: alt.clone(),
                            link: *link,
                        },
                        false,
                    );
                }
                Inline::Control(id) => {
                    let (w, h) = control_size(&self.controls[*id], host);
                    if w > 0 {
                        f.place(Piece::Control { w, h, id: *id }, false);
                        // A little room after it.
                        f.x += 6;
                    }
                }
            }
        }
        f.end_line(false);
        f.y
    }

    /// What is under page point (x, y): a control, else a link.
    pub fn hit_test(&self, x: i32, y: i32) -> Option<Hit> {
        if let Some((_, id)) = self
            .control_boxes
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
        {
            return Some(Hit::Control(*id));
        }
        self.link_boxes
            .iter()
            .rev()
            .find(|(r, _)| r.contains(x, y))
            .and_then(|(_, l)| self.links.get(*l))
            .map(|h| Hit::Link(h.clone()))
    }

    /// Where a control is on the page (after layout).
    pub fn control_rect(&self, id: usize) -> Option<Rect> {
        self.control_boxes
            .iter()
            .find(|(_, i)| *i == id)
            .map(|(r, _)| *r)
    }

    /// Where `#name` is (after layout).
    pub fn anchor(&self, name: &str) -> Option<i32> {
        self.anchor_y
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, y)| *y)
    }

    /// Where the main content starts (`<main>` or role=main), for "skip to content".
    pub fn main_content(&self) -> Option<i32> {
        self.main_y
    }

    /// Type into a text control (`maxlength` holds).
    pub fn set_value(&mut self, id: usize, v: &str) {
        if let Some(c) = self.controls.get_mut(id)
            && !c.disabled
        {
            c.value = if c.max_len > 0 {
                v.chars().take(c.max_len).collect()
            } else {
                String::from(v)
            };
        }
    }

    /// A click on a checkbox (toggles) or a radio (on, and the rest of its group off).
    pub fn click_check(&mut self, id: usize) {
        let Some(c) = self.controls.get(id) else {
            return;
        };
        if c.disabled {
            return;
        }
        match c.kind {
            ControlKind::Checkbox => self.controls[id].checked = !self.controls[id].checked,
            ControlKind::Radio => {
                let (name, form) = (c.name.clone(), c.form);
                for o in self.controls.iter_mut() {
                    if o.kind == ControlKind::Radio
                        && o.name == name
                        && o.form == form
                        && !name.is_empty()
                    {
                        o.checked = false;
                    }
                }
                self.controls[id].checked = true;
            }
            _ => {}
        }
    }

    pub fn select(&mut self, id: usize, option: usize) {
        if let Some(c) = self.controls.get_mut(id)
            && option < c.options.len()
            && !c.disabled
        {
            c.selected = option;
        }
    }

    /// Put a form back as the page had it (a Reset button).
    pub fn reset(&mut self, form: Option<usize>, original: &Document) {
        for (i, c) in self.controls.iter_mut().enumerate() {
            if c.form == form
                && let Some(o) = original.controls.get(i)
            {
                *c = o.clone();
            }
        }
    }

    /// What pressing `id` sends (a submit button, or Enter in a text field): the form's
    /// successful controls in order, the button itself included. None if it isn't in a form.
    pub fn submission(&self, id: usize) -> Option<Submission> {
        let c = self.controls.get(id)?;
        let fi = c.form?;
        let form = self.forms.get(fi)?;
        let mut pairs = Vec::new();
        for (i, o) in self.controls.iter().enumerate() {
            if o.form != Some(fi) || o.disabled || o.name.is_empty() {
                continue;
            }
            match o.kind {
                ControlKind::Text
                | ControlKind::Password
                | ControlKind::Hidden
                | ControlKind::TextArea => pairs.push((o.name.clone(), o.value.clone())),
                ControlKind::Checkbox | ControlKind::Radio if o.checked => {
                    pairs.push((o.name.clone(), o.value.clone()))
                }
                ControlKind::Select => {
                    if let Some((_, v)) = o.options.get(o.selected) {
                        pairs.push((o.name.clone(), v.clone()));
                    }
                }
                ControlKind::Submit if i == id => pairs.push((o.name.clone(), o.value.clone())),
                _ => {}
            }
        }
        Some(Submission {
            method: form.method,
            action: form.action.clone(),
            pairs,
        })
    }

    /// The text of the page, one block a line (for tests and "copy all").
    pub fn text(&self) -> String {
        let mut s = String::new();
        for b in &self.blocks {
            for i in b.inlines.iter().chain(b.cells.iter().flatten()) {
                if let Inline::Text(t, _) = i {
                    s.push_str(t);
                }
            }
            s.push('\n');
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 8 px a character, 16 px lines (Unifont).
    struct Mono;
    impl Host for Mono {
        fn text_width(&self, t: &str, f: Font) -> i32 {
            t.chars().count() as i32 * if f.size >= 24 { 16 } else { 8 }
        }
        fn line_height(&self, f: Font) -> i32 {
            if f.size >= 24 { 36 } else { 20 }
        }
        fn image_size(&self, url: &str) -> Option<(i32, i32)> {
            url.ends_with("cow.png").then_some((200, 100))
        }
    }

    fn doc(html: &str) -> Document {
        parse(
            html.as_bytes(),
            "text/html",
            "http://milk.test/dir/page.html",
        )
    }

    fn texts(d: &DisplayList) -> Vec<String> {
        d.items
            .iter()
            .filter_map(|i| match i {
                Item::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn title_blocks_and_entities() {
        let mut d = doc(
            "<!DOCTYPE html><html><head><title> Milk &amp;\n the Web </title><style>p{}</style><script>x<y</script></head><body><h1>Hi</h1><p>One &lt;two&gt;   three</p><!-- <p>no</p> --></body></html>",
        );
        assert_eq!(d.title(), "Milk & the Web");
        let l = d.layout(600, &Mono).clone();
        assert_eq!(texts(&l), ["Hi", "One <two> three"]);
        assert!(l.height > 40);
    }

    #[test]
    fn wrapping_and_links() {
        let mut d = doc("<p>aaaa bbbb <a href=\"next.html#x\">cccc dddd</a> eeee</p>");
        // 120 px page: 80 inner, 10 characters a line.
        let l = d.layout(120, &Mono).clone();
        let t = texts(&l);
        assert_eq!(t, ["aaaa bbbb", "cccc dddd", "eeee"], "{t:?}");
        let (x, y) = match &l.items[1] {
            Item::Text { x, y, .. } => (*x, *y),
            _ => unreachable!(),
        };
        assert_eq!(
            d.hit_test(x + 2, y + 2),
            Some(Hit::Link(String::from("next.html#x")))
        );
        assert_eq!(d.hit_test(x + 2, y - 30), None);
    }

    #[test]
    fn punctuation_stays_with_its_word() {
        // "cccc," must not break before the comma.
        let mut d = doc("<p>aaaa bbbb <a href=x>cccc</a>, dddd</p>");
        let l = d.layout(140, &Mono).clone();
        assert_eq!(texts(&l), ["aaaa bbbb", "cccc", ", dddd"]);
        let y = |s: &str| {
            l.items.iter().find_map(|i| match i {
                Item::Text { y, text, .. } if text == s => Some(*y),
                _ => None,
            })
        };
        assert_eq!(y("cccc"), y(", dddd"));
        assert!(y("cccc") > y("aaaa bbbb"));
        // The link's box covers just the link.
        let r = d.link_boxes[0].0;
        assert_eq!(r.w, 32);
    }

    #[test]
    fn lists_quotes_pre_tables() {
        let mut d = doc(
            "<ul><li>a<li>b<ol start=3><li>c</ol></ul><blockquote>q</blockquote><pre>x  y\n  z</pre><table><tr><th>H1<th>H2<tr><td>1<td>2</table><hr>",
        );
        let l = d.layout(600, &Mono).clone();
        let t = texts(&l);
        assert!(t.contains(&String::from("3.")), "{t:?}");
        assert!(
            t.contains(&String::from("x  y")) && t.contains(&String::from("  z")),
            "{t:?}"
        );
        assert!(t.contains(&String::from("H1")) && t.contains(&String::from("2")));
        let x_of = |s: &str| {
            l.items.iter().find_map(|i| match i {
                Item::Text { x, text, .. } if text == s => Some(*x),
                _ => None,
            })
        };
        // The nested item is indented further, the second cell is in the second column.
        assert!(x_of("c").unwrap() > x_of("a").unwrap());
        assert!(x_of("2").unwrap() > x_of("1").unwrap() + 100);
    }

    #[test]
    fn images() {
        let mut d = doc(
            "<p><img src=\"cow.png\" alt=\"a cow\"> <img src=\"/gone.png\" alt=\"gone\"> <img src=x.png width=50 height=40></p>",
        );
        assert_eq!(
            d.images(),
            [
                "http://milk.test/dir/cow.png",
                "http://milk.test/gone.png",
                "http://milk.test/dir/x.png"
            ]
        );
        let l = d.layout(1000, &Mono).clone();
        let imgs: Vec<Rect> = l
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Image { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!((imgs[0].w, imgs[0].h), (200, 100));
        assert_eq!((imgs[1].w, imgs[1].h), (4 * 8 + 12, 28));
        assert_eq!((imgs[2].w, imgs[2].h), (50, 40));
        // Too wide: scaled to the column.
        let l = d.layout(140, &Mono).clone();
        let w = l.items.iter().find_map(|i| match i {
            Item::Image { rect, .. } => Some(rect.w),
            _ => None,
        });
        assert_eq!(w, Some(100));
    }

    #[test]
    fn forms_and_submission() {
        let mut d = doc(
            "<form action=\"/go\" method=post><input name=user value=milk>\
             <input type=password name=pw><input type=checkbox name=c checked><input type=checkbox name=d value=dv>\
             <input type=radio name=r value=1 checked><input type=radio name=r value=2>\
             <select name=s><option>One<option value=two selected>Two</select>\
             <textarea name=t>\nhello &amp; bye</textarea><input type=hidden name=h value=hid>\
             <input type=submit name=go value=Send><button name=b value=bv>Other</button></form>\
             <input name=orphan>",
        );
        let c = d.controls();
        assert_eq!(c.len(), 12);
        assert_eq!(c[7].value, "hello & bye");
        assert_eq!(c[6].selected, 1);
        assert_eq!(c[11].form, None);
        assert_eq!(c[10].label, "Other");
        d.set_value(1, "secret");
        d.click_check(5); // radio 2
        d.click_check(2); // uncheck c
        d.click_check(3); // check d
        let s = d.submission(9).unwrap();
        assert_eq!(s.method, Method::Post);
        assert_eq!(s.action, "/go");
        let got: Vec<(&str, &str)> = s
            .pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("user", "milk"),
                ("pw", "secret"),
                ("d", "dv"),
                ("r", "2"),
                ("s", "two"),
                ("t", "hello & bye"),
                ("h", "hid"),
                ("go", "Send")
            ]
        );
        // The other button sends its own name instead.
        let s2 = d.submission(10).unwrap();
        assert!(!s2.pairs.iter().any(|(k, _)| k == "go"));
        assert!(d.submission(11).is_none());
        // Laid out: control boxes hit.
        d.layout(800, &Mono);
        let r = d.control_rect(0).unwrap();
        assert_eq!(d.hit_test(r.x + 1, r.y + 1), Some(Hit::Control(0)));
        assert!(d.control_rect(9).is_some());
        assert!(d.control_rect(8).is_none());
    }

    #[test]
    fn anchors_main_and_base() {
        let mut d = doc(
            "<base href=\"http://other.test/b/\"><p>top</p><main><h2 id=sec>Section</h2><p><a name=old>x</a></p></main>",
        );
        d.layout(600, &Mono);
        assert!(d.anchor("sec").unwrap() > 0);
        assert!(d.anchor("old").unwrap() > d.anchor("sec").unwrap());
        assert_eq!(d.main_content(), d.anchor("sec"));
        assert_eq!(d.base_url().unwrap().to_text(), "http://other.test/b/");
        assert_eq!(d.anchor("none"), None);
    }

    #[test]
    fn plain_text() {
        let mut d = parse(
            b"line one\n\tline <b>two</b>",
            "text/plain",
            "http://milk.test/a.txt",
        );
        let l = d.layout(600, &Mono).clone();
        assert_eq!(texts(&l), ["line one", "    line <b>two</b>"]);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut seed = 12345u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let parts = [
            "<",
            ">",
            "</",
            "<p>",
            "<a href=",
            "\"",
            "'",
            "<table>",
            "<td>",
            "<tr>",
            "<li>",
            "<ul>",
            "</ul>",
            "<ol start=99999999999>",
            "<select>",
            "<option>",
            "</select>",
            "<textarea>",
            "<img src=",
            "width=999999",
            "<form>",
            "<input type=radio name=r>",
            "&#x110000;",
            "&#0;",
            "&amp",
            "<!--",
            "-->",
            "<pre>",
            "\n",
            "é",
            "\u{1F404}",
            "<br>",
            "<h1>",
            "<script>",
            "<title>",
            "<button>",
            "x",
            " ",
            "<blockquote>",
            "<base href=",
            "<main>",
            "id=a",
        ];
        for _ in 0..300 {
            let mut s = String::new();
            for _ in 0..(rnd() % 200) {
                s.push_str(parts[(rnd() as usize) % parts.len()]);
            }
            let mut d = doc(&s);
            d.layout((rnd() % 900) as i32 - 50, &Mono);
            let _ = d.hit_test(30, 30);
            for i in 0..d.controls().len() {
                let _ = d.submission(i);
                d.click_check(i);
                d.select(i, 3);
            }
        }
        // Raw bytes too.
        for _ in 0..100 {
            let b: Vec<u8> = (0..(rnd() % 500)).map(|_| rnd() as u8).collect();
            let mut d = parse(&b, "text/html", "http://x/");
            d.layout(300, &Mono);
        }
    }

    #[test]
    fn deep_nesting_and_size_are_bounded() {
        let s = "<ul><li>x".repeat(5000) + &"<b>".repeat(5000) + &"<p>y</p>".repeat(30_000);
        let mut d = doc(&s);
        assert!(d.blocks.len() <= MAX_BLOCKS);
        let l = d.layout(600, &Mono);
        assert!(l.height <= MAX_HEIGHT + 2000);
        assert!(l.items.iter().all(|i| match i {
            Item::Text { x, .. } => *x < 600,
            _ => true,
        }));
    }
}
