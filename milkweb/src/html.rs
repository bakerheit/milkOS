//! The Browser's HTML subset: a page becomes a title and a list of blocks (headings,
//! paragraphs, list items, quotes, preformatted text, rules), each a list of styled runs
//! (bold, italic, link). No CSS, no JavaScript.
//!
//! - Blocks: h1-h6, p, div, section/article/main/header/footer/nav/aside, ul/ol/li, dl/dt/dd,
//!   blockquote, pre, hr, table/tr (a row is a block, its cells are separated by " | "),
//!   form, figure, address, center, details/summary. Unknown tags are inline (text kept).
//! - Inline: a href, b/strong, i/em/cite/var, span, font, small, u, code and the like; br;
//!   `<img alt>` as `[alt]` text.
//! - Dropped with their content: script, style, template, svg, math, iframe, object, select,
//!   textarea, head (except the title), and comments.
//! - Entities: the HTML named ones Latin-1 covers plus the usual typographic ones, numeric
//!   (`&#233;`, `&#xE9;`), and the legacy forms without `;` for the common few.
//! - Text: [`decode_bytes`] picks UTF-8 or Latin-1 / Windows-1252 from the Content-Type
//!   charset, a `<meta charset>`, or failing both, whether the bytes are valid UTF-8.
//!   Characters the UI font lacks (it has Latin-1) are folded to near ASCII ([`fold`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind {
    H1,
    H2,
    H3,
    P,
    Li,
    Pre,
    Quote,
    /// A horizontal rule (no runs).
    Rule,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub link: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Block {
    pub kind: BlockKind,
    pub runs: Vec<Run>,
}

impl Block {
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Doc {
    pub title: String,
    pub blocks: Vec<Block>,
}

impl Doc {
    /// The whole text, a block per line (for tests and plain views).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for b in &self.blocks {
            s.push_str(b.text().trim());
            s.push('\n');
        }
        s
    }
}

// ---- characters -------------------------------------------------------------------------------

/// Windows-1252's 0x80-0x9f (what "Latin-1" pages really mean).
const CP1252: [u16; 32] = [
    0x20ac, 0x81, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x8d, 0x017d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
    0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0x9d, 0x017e, 0x0178,
];

fn latin1(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x80..=0x9f => char::from_u32(CP1252[(c - 0x80) as usize] as u32).unwrap_or('?'),
            c => c as char,
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Charset {
    Utf8,
    Latin1,
}

fn charset_name(s: &str) -> Option<Charset> {
    let s = s
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_ascii_lowercase();
    match s.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some(Charset::Utf8),
        "iso-8859-1" | "iso8859-1" | "latin1" | "latin-1" | "l1" | "windows-1252" | "cp1252"
        | "us-ascii" | "ascii" | "iso-8859-15" => Some(Charset::Latin1),
        _ => None,
    }
}

/// `charset=` in a Content-Type value.
pub fn charset_param(content_type: &str) -> Option<Charset> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| charset_name(v))?
    })
}

/// `<meta charset="...">` or `<meta http-equiv="Content-Type" content="...; charset=...">`
/// in the first 2 KiB.
fn meta_charset(b: &[u8]) -> Option<Charset> {
    let head = String::from_utf8_lossy(&b[..b.len().min(2048)]).to_ascii_lowercase();
    let mut rest = head.as_str();
    while let Some(i) = rest.find("<meta") {
        let tag = &rest[i..];
        let end = tag.find('>').unwrap_or(tag.len());
        let tag = &tag[..end];
        if let Some(j) = tag.find("charset=") {
            let v = &tag[j + 8..];
            let v = v.trim_start_matches(['"', '\'']);
            let v: String = v
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect();
            if let Some(c) = charset_name(&v) {
                return Some(c);
            }
        }
        rest = &rest[i + 5..];
    }
    None
}

/// Bytes to text: the declared charset (header, then meta), else UTF-8 if valid, else 1252.
pub fn decode_bytes(b: &[u8], content_type: &str) -> String {
    let b = b.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(b);
    let cs = charset_param(content_type).or_else(|| meta_charset(b));
    match cs {
        Some(Charset::Latin1) => latin1(b),
        Some(Charset::Utf8) => String::from_utf8_lossy(b).into_owned(),
        None => match core::str::from_utf8(b) {
            Ok(s) => String::from(s),
            Err(_) => latin1(b),
        },
    }
}

/// A character the UI font can draw (Latin-1): typographic ones folded to ASCII, others
/// left for the font's replacement glyph.
pub fn fold(c: char) -> Option<&'static str> {
    Some(match c as u32 {
        0x2010..=0x2015 | 0x2212 => "-",
        0x2018 | 0x2019 | 0x201a | 0x201b | 0x2032 => "'",
        0x201c | 0x201d | 0x201e | 0x201f | 0x2033 => "\"",
        0x2026 => "...",
        0x2022 | 0x2023 | 0x2043 | 0x25aa | 0x25cf => "\u{b7}",
        0x2039 => "<",
        0x203a => ">",
        0x2190 => "<-",
        0x2192 => "->",
        0x2122 => "TM",
        0x20ac => "EUR",
        0x2002..=0x200a | 0x202f | 0x205f | 0x3000 => " ",
        0x200b..=0x200d | 0x2060 | 0xfeff | 0xad => "",
        0x2020 => "+",
        0x2030 => "%o",
        0x0152 => "OE",
        0x0153 => "oe",
        0x0160 => "S",
        0x0161 => "s",
        0x017d => "Z",
        0x017e => "z",
        0x0178 => "Y",
        0x0192 => "f",
        0x02c6 => "^",
        0x02dc => "~",
        0x2713 | 0x2714 => "v",
        0x00a0 => " ",
        _ => return None,
    })
}

fn push_folded(out: &mut String, c: char) {
    match fold(c) {
        Some(s) => out.push_str(s),
        None => out.push(c),
    }
}

fn entity(name: &str) -> Option<char> {
    let cp: u32 = match name {
        "amp" => 38,
        "lt" => 60,
        "gt" => 62,
        "quot" => 34,
        "apos" => 39,
        "nbsp" => 160,
        "iexcl" => 161,
        "cent" => 162,
        "pound" => 163,
        "curren" => 164,
        "yen" => 165,
        "brvbar" => 166,
        "sect" => 167,
        "uml" => 168,
        "copy" => 169,
        "ordf" => 170,
        "laquo" => 171,
        "not" => 172,
        "shy" => 173,
        "reg" => 174,
        "macr" => 175,
        "deg" => 176,
        "plusmn" => 177,
        "sup2" => 178,
        "sup3" => 179,
        "acute" => 180,
        "micro" => 181,
        "para" => 182,
        "middot" => 183,
        "cedil" => 184,
        "sup1" => 185,
        "ordm" => 186,
        "raquo" => 187,
        "frac14" => 188,
        "frac12" => 189,
        "frac34" => 190,
        "iquest" => 191,
        "times" => 215,
        "divide" => 247,
        "szlig" => 223,
        "ndash" => 0x2013,
        "mdash" => 0x2014,
        "lsquo" => 0x2018,
        "rsquo" => 0x2019,
        "sbquo" => 0x201a,
        "ldquo" => 0x201c,
        "rdquo" => 0x201d,
        "bdquo" => 0x201e,
        "hellip" => 0x2026,
        "bull" => 0x2022,
        "dagger" => 0x2020,
        "permil" => 0x2030,
        "lsaquo" => 0x2039,
        "rsaquo" => 0x203a,
        "euro" => 0x20ac,
        "trade" => 0x2122,
        "larr" => 0x2190,
        "rarr" => 0x2192,
        "minus" => 0x2212,
        "ensp" => 0x2002,
        "emsp" => 0x2003,
        "thinsp" => 0x2009,
        "zwnj" => 0x200c,
        "zwj" => 0x200d,
        "prime" => 0x2032,
        "check" => 0x2713,
        _ => return latin1_letter(name),
    };
    char::from_u32(cp)
}

/// Latin-1's letters 0xC0-0xDF by name (the lower-case ones are 0x20 higher).
const LETTERS: [&str; 32] = [
    "Agrave", "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute",
    "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute",
    "Ocirc", "Otilde", "Ouml", "", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute",
    "THORN", "",
];

/// `eacute`, `Ouml`, `ntilde`, `aring`, `AElig`, `thorn`, `yuml`... (Latin-1's letters).
fn latin1_letter(name: &str) -> Option<char> {
    if name == "yuml" {
        return Some('\u{ff}');
    }
    for (i, n) in LETTERS.iter().enumerate() {
        if n.is_empty() {
            continue;
        }
        if *n == name {
            return char::from_u32(0xc0 + i as u32);
        }
        if n.to_ascii_lowercase() == name {
            return char::from_u32(0xe0 + i as u32);
        }
    }
    None
}

/// Decode character references in `text` (and fold what the font can't draw).
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        for c in rest[..i].chars() {
            push_folded(&mut out, c);
        }
        let after = &rest[i + 1..];
        let name_len = after
            .bytes()
            .take(32)
            .take_while(|c| c.is_ascii_alphanumeric() || *c == b'#')
            .count();
        let name = &after[..name_len];
        let semi = after[name_len..].starts_with(';');
        let c = if let Some(num) = name.strip_prefix('#') {
            let v = match num.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok(),
                None => num.parse().ok(),
            };
            v.map(|v| match v {
                0x80..=0x9f => CP1252[(v - 0x80) as usize] as u32,
                0 => 0xfffd,
                v => v,
            })
            .and_then(char::from_u32)
            .or(Some('\u{fffd}'))
            .filter(|_| !num.is_empty() && num != "x" && num != "X")
        } else if semi {
            entity(name)
        } else {
            // Legacy forms without the semicolon.
            matches!(name, "amp" | "lt" | "gt" | "quot" | "nbsp" | "copy" | "reg")
                .then(|| entity(name))
                .flatten()
        };
        match c {
            Some(c) => {
                push_folded(&mut out, c);
                rest = &after[name_len + usize::from(semi)..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    for c in rest.chars() {
        push_folded(&mut out, c);
    }
    out
}

// ---- tags -------------------------------------------------------------------------------------

/// The value of attribute `name` in a tag's inside (`a href="x" class=y`), entities decoded.
pub fn attr(tag: &str, name: &str) -> Option<String> {
    let b = tag.as_bytes();
    // Skip the tag name.
    let mut k = b
        .iter()
        .position(|c| c.is_ascii_whitespace() || *c == b'/')?;
    while k < b.len() {
        while k < b.len() && (b[k].is_ascii_whitespace() || b[k] == b'/') {
            k += 1;
        }
        let s = k;
        while k < b.len() && !b[k].is_ascii_whitespace() && b[k] != b'=' && b[k] != b'/' {
            k += 1;
        }
        let key = &tag[s..k];
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        let mut val = None;
        if k < b.len() && b[k] == b'=' {
            k += 1;
            while k < b.len() && b[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < b.len() && (b[k] == b'"' || b[k] == b'\'') {
                let q = b[k];
                let s = k + 1;
                let e = b[s..]
                    .iter()
                    .position(|&c| c == q)
                    .map_or(b.len(), |p| s + p);
                val = Some(&tag[s..e]);
                k = (e + 1).min(b.len());
            } else {
                let s = k;
                while k < b.len() && !b[k].is_ascii_whitespace() {
                    k += 1;
                }
                val = Some(&tag[s..k]);
            }
        }
        if key.eq_ignore_ascii_case(name) {
            return Some(decode_entities(val.unwrap_or("")));
        }
        if key.is_empty() {
            k += 1;
        }
    }
    None
}

/// Find `</name` (any case) in `s`: the index of the `<`.
fn find_close(s: &str, name: &str) -> Option<usize> {
    let b = s.as_bytes();
    let n = name.len();
    let mut k = 0;
    while let Some(i) = s[k..].find("</") {
        let at = k + i;
        if b.len() >= at + 2 + n && s[at + 2..at + 2 + n].eq_ignore_ascii_case(name) {
            let after = b.get(at + 2 + n).copied().unwrap_or(b'>');
            if after == b'>' || after.is_ascii_whitespace() || after == b'/' {
                return Some(at);
            }
        }
        k = at + 2;
    }
    None
}

const SKIP_CONTENT: &[&str] = &[
    "script", "style", "template", "svg", "math", "iframe", "object", "select", "textarea",
    "noembed", "canvas", "video", "audio",
];

const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "header",
    "footer",
    "nav",
    "aside",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "caption",
    "form",
    "fieldset",
    "figure",
    "figcaption",
    "address",
    "center",
    "details",
    "summary",
    "body",
    "html",
    "noscript",
    "legend",
    "menu",
    "hgroup",
    "search",
    "dialog",
];

/// Parse a page.
pub fn parse(src: &str) -> Doc {
    let mut p = Parser {
        doc: Doc::default(),
        cur: Block {
            kind: BlockKind::P,
            runs: Vec::new(),
        },
        bold: 0,
        italic: 0,
        pre: 0,
        quote: 0,
        link: None,
        cell: false,
    };
    let mut rest = src;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = r.find("-->").map_or("", |i| &r[i + 3..]);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |i| &rest[i + 1..]);
            continue;
        }
        let is_tag = rest.starts_with('<')
            && rest[1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '/');
        if is_tag {
            let end = tag_end(rest);
            let tag = &rest[1..end];
            rest = rest.get(end + 1..).unwrap_or("");
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if !closing && SKIP_CONTENT.contains(&name.as_str()) {
                rest = match find_close(rest, &name) {
                    Some(i) => {
                        let r = &rest[i..];
                        r.find('>').map_or("", |j| &r[j + 1..])
                    }
                    None => "",
                };
                continue;
            }
            if !closing && name == "title" {
                let (t, r) = match find_close(rest, "title") {
                    Some(i) => (&rest[..i], &rest[i..]),
                    None => (rest, ""),
                };
                if p.doc.title.is_empty() {
                    p.doc.title = collapse(&decode_entities(t)).trim().into();
                }
                rest = r.find('>').map_or("", |j| &r[j + 1..]);
                continue;
            }
            if !closing && name == "head" {
                // Only the title matters in the head: take it, drop the rest.
                if let Some(i) = find_close(rest, "head") {
                    let head = &rest[..i];
                    if let Some(t) = head.to_ascii_lowercase().find("<title") {
                        let h = &head[t..];
                        if let Some(gt) = h.find('>') {
                            let inner = &h[gt + 1..];
                            let e = find_close(inner, "title").unwrap_or(inner.len());
                            p.doc.title = collapse(&decode_entities(&inner[..e])).trim().into();
                        }
                    }
                    let r = &rest[i..];
                    rest = r.find('>').map_or("", |j| &r[j + 1..]);
                }
                continue;
            }
            p.tag(&name, tag, closing);
            continue;
        }
        // Text up to the next tag (a '<' not starting one is text).
        let mut end = rest.len();
        let mut from = if rest.starts_with('<') { 1 } else { 0 };
        while let Some(i) = rest[from..].find('<') {
            let at = from + i;
            let next = rest[at + 1..].chars().next();
            if next.is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?') {
                end = at;
                break;
            }
            from = at + 1;
        }
        let raw = &rest[..end];
        rest = &rest[end..];
        p.text(raw);
    }
    p.flush(BlockKind::P);
    p.doc
}

/// The `>` ending a tag that starts at `s[0]`, past quoted attribute values.
fn tag_end(s: &str) -> usize {
    let b = s.as_bytes();
    let mut q = 0u8;
    for (i, &c) in b.iter().enumerate().skip(1) {
        if q != 0 {
            if c == q {
                q = 0;
            }
        } else if c == b'"' || c == b'\'' {
            // Only quotes after '=' open a value (a stray quote shouldn't eat the page).
            let prev = b[..i].iter().rev().find(|c| !c.is_ascii_whitespace());
            if prev == Some(&b'=') {
                q = c;
            }
        } else if c == b'>' {
            return i;
        }
    }
    s.len().saturating_sub(1)
}

fn collapse(text: &str) -> String {
    let mut t = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            space = true;
        } else {
            if space {
                t.push(' ');
                space = false;
            }
            t.push(c);
        }
    }
    if space {
        t.push(' ');
    }
    t
}

struct Parser {
    doc: Doc,
    cur: Block,
    bold: i32,
    italic: i32,
    pre: i32,
    quote: i32,
    link: Option<String>,
    /// A table cell has text in this row (the next cell starts with " | ").
    cell: bool,
}

impl Parser {
    fn kind_here(&self) -> BlockKind {
        if self.quote > 0 {
            BlockKind::Quote
        } else {
            BlockKind::P
        }
    }

    fn flush(&mut self, next: BlockKind) {
        let has_text = self.cur.runs.iter().any(|r| !r.text.trim().is_empty());
        if has_text {
            // Trim the edges of the block.
            if self.cur.kind != BlockKind::Pre {
                if let Some(r) = self.cur.runs.first_mut() {
                    r.text = String::from(r.text.trim_start());
                }
                if let Some(r) = self.cur.runs.last_mut() {
                    r.text = String::from(r.text.trim_end());
                }
            }
            let done = core::mem::replace(
                &mut self.cur,
                Block {
                    kind: next,
                    runs: Vec::new(),
                },
            );
            self.doc.blocks.push(done);
        } else {
            self.cur.runs.clear();
            self.cur.kind = next;
        }
        self.cell = false;
    }

    fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        // Don't start a line with a space, or put two in a row.
        let last_space = self
            .cur
            .runs
            .last()
            .is_none_or(|r| r.text.ends_with(' ') || r.text.ends_with('\n'));
        let text = if self.pre == 0 && last_space {
            String::from(text.trim_start())
        } else {
            text
        };
        if text.is_empty() {
            return;
        }
        let (bold, italic, link) = (self.bold > 0, self.italic > 0, self.link.clone());
        if let Some(r) = self.cur.runs.last_mut()
            && r.bold == bold
            && r.italic == italic
            && r.link == link
            && r.text != "\n"
        {
            r.text.push_str(&text);
            return;
        }
        self.cur.runs.push(Run {
            text,
            bold,
            italic,
            link,
        });
    }

    fn text(&mut self, raw: &str) {
        let text = decode_entities(raw);
        let text = if self.pre > 0 { text } else { collapse(&text) };
        self.push(text);
    }

    fn tag(&mut self, name: &str, tag: &str, closing: bool) {
        match (name, closing) {
            ("h1", false) => self.flush(BlockKind::H1),
            ("h2", false) => self.flush(BlockKind::H2),
            ("h3" | "h4" | "h5" | "h6", false) => self.flush(BlockKind::H3),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => {
                let k = self.kind_here();
                self.flush(k)
            }
            ("li", false) => self.flush(BlockKind::Li),
            ("li", true) => {
                let k = self.kind_here();
                self.flush(k)
            }
            ("pre" | "listing" | "xmp" | "plaintext", false) => {
                self.flush(BlockKind::Pre);
                self.pre += 1;
            }
            ("pre" | "listing" | "xmp" | "plaintext", true) => {
                self.pre = (self.pre - 1).max(0);
                let k = self.kind_here();
                self.flush(k);
            }
            ("blockquote", false) => {
                self.quote += 1;
                self.flush(BlockKind::Quote);
            }
            ("blockquote", true) => {
                self.quote = (self.quote - 1).max(0);
                let k = self.kind_here();
                self.flush(k);
            }
            ("hr", _) => {
                let k = self.kind_here();
                self.flush(k);
                self.doc.blocks.push(Block {
                    kind: BlockKind::Rule,
                    runs: Vec::new(),
                });
            }
            ("tr", _) => {
                let k = self.kind_here();
                self.flush(k)
            }
            ("td" | "th", false) => {
                if self.cell {
                    let b = self.bold;
                    self.bold = 0;
                    let l = self.link.take();
                    self.push(String::from(" | "));
                    self.bold = b;
                    self.link = l;
                }
                if name == "th" {
                    self.bold += 1;
                }
            }
            ("td" | "th", true) => {
                if name == "th" {
                    self.bold = (self.bold - 1).max(0);
                }
                self.cell = self.cur.runs.iter().any(|r| !r.text.trim().is_empty());
                self.push(String::from(" "));
            }
            ("br", _) => {
                self.cur.runs.push(Run {
                    text: String::from("\n"),
                    bold: false,
                    italic: false,
                    link: None,
                });
            }
            ("b" | "strong", false) => self.bold += 1,
            ("b" | "strong", true) => self.bold = (self.bold - 1).max(0),
            ("i" | "em" | "cite" | "var" | "dfn", false) => self.italic += 1,
            ("i" | "em" | "cite" | "var" | "dfn", true) => self.italic = (self.italic - 1).max(0),
            ("a", false) => self.link = attr(tag, "href").filter(|h| !h.trim().is_empty()),
            ("a", true) => self.link = None,
            ("img", _) => {
                if let Some(alt) = attr(tag, "alt") {
                    let alt = collapse(&alt);
                    let alt = alt.trim();
                    if !alt.is_empty() {
                        self.italic += 1;
                        self.push(format!(" [{alt}] "));
                        self.italic -= 1;
                    }
                }
            }
            ("input", false) => {
                // A button's label reads as text; other fields don't.
                let ty = attr(tag, "type").unwrap_or_default().to_ascii_lowercase();
                if matches!(ty.as_str(), "submit" | "button")
                    && let Some(v) = attr(tag, "value")
                {
                    self.push(format!(" [{}] ", v.trim()));
                }
            }
            (n, _) if BLOCKS.contains(&n) => {
                let k = self.kind_here();
                self.flush(k)
            }
            _ => {}
        }
    }
}

/// A plain-text page as one preformatted block.
pub fn plain(text: &str) -> Doc {
    let mut t = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\t' {
            t.push_str("    ");
        } else if c != '\r' {
            push_folded(&mut t, c);
        }
    }
    Doc {
        title: String::new(),
        blocks: alloc::vec![Block {
            kind: BlockKind::Pre,
            runs: alloc::vec![Run {
                text: t,
                bold: false,
                italic: false,
                link: None,
            }],
        }],
    }
}

/// Is this body HTML? (The Content-Type, or the first non-space byte is `<`.)
pub fn is_html(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.contains("html") || (ct.is_empty() && body.trim_start().starts_with('<'))
}
