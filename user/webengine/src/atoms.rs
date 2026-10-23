//! Interned names. Tag names the engine knows are compile-time [`Atom`] constants (so the tree
//! builder and the style system compare integers); anything else (custom elements, class and
//! id values) is interned per document by [`Interner`].

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct Atom(pub u32);

macro_rules! atoms {
    ($($konst:ident = $s:literal,)*) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms, dead_code)]
        #[repr(u32)]
        enum Idx { $($konst,)* __Count }
        #[allow(dead_code)]
        pub mod tag {
            use super::{Atom, Idx};
            $(pub const $konst: Atom = Atom(Idx::$konst as u32);)*
            pub const COUNT: u32 = Idx::__Count as u32;
        }
        /// The static names, index = atom value.
        pub static STATIC: &[&str] = &[$($s,)*];
    };
}

atoms! {
    EMPTY = "",
    A = "a", ABBR = "abbr", ADDRESS = "address", APPLET = "applet", AREA = "area",
    ARTICLE = "article", ASIDE = "aside", AUDIO = "audio", B = "b", BASE = "base",
    BASEFONT = "basefont", BDI = "bdi", BDO = "bdo", BGSOUND = "bgsound", BIG = "big",
    BLINK = "blink", BLOCKQUOTE = "blockquote", BODY = "body", BR = "br", BUTTON = "button",
    CANVAS = "canvas", CAPTION = "caption", CENTER = "center", CITE = "cite", CODE = "code",
    COL = "col", COLGROUP = "colgroup", DATA = "data", DATALIST = "datalist", DD = "dd",
    DEL = "del", DETAILS = "details", DFN = "dfn", DIALOG = "dialog", DIR = "dir", DIV = "div",
    DL = "dl", DT = "dt", EM = "em", EMBED = "embed", FIELDSET = "fieldset",
    FIGCAPTION = "figcaption", FIGURE = "figure", FONT = "font", FOOTER = "footer",
    FORM = "form", FRAME = "frame", FRAMESET = "frameset", H1 = "h1", H2 = "h2", H3 = "h3",
    H4 = "h4", H5 = "h5", H6 = "h6", HEAD = "head", HEADER = "header", HGROUP = "hgroup",
    HR = "hr", HTML = "html", I = "i", IFRAME = "iframe", IMAGE = "image", IMG = "img",
    INPUT = "input", INS = "ins", KBD = "kbd", KEYGEN = "keygen", LABEL = "label",
    LEGEND = "legend", LI = "li", LINK = "link", LISTING = "listing", MAIN = "main",
    MAP = "map", MARK = "mark", MARQUEE = "marquee", MATH = "math", MENU = "menu",
    META = "meta", METER = "meter", NAV = "nav", NOBR = "nobr", NOEMBED = "noembed",
    NOFRAMES = "noframes", NOSCRIPT = "noscript", OBJECT = "object", OL = "ol",
    OPTGROUP = "optgroup", OPTION = "option", OUTPUT = "output", P = "p", PARAM = "param",
    PICTURE = "picture", PLAINTEXT = "plaintext", PRE = "pre", PROGRESS = "progress", Q = "q",
    RB = "rb", RP = "rp", RT = "rt", RTC = "rtc", RUBY = "ruby", S = "s", SAMP = "samp",
    SCRIPT = "script", SEARCH = "search", SECTION = "section", SELECT = "select",
    SLOT = "slot", SMALL = "small", SOURCE = "source", SPAN = "span", STRIKE = "strike",
    STRONG = "strong", STYLE = "style", SUB = "sub", SUMMARY = "summary", SUP = "sup",
    SVG = "svg", TABLE = "table", TBODY = "tbody", TD = "td", TEMPLATE = "template",
    TEXTAREA = "textarea", TFOOT = "tfoot", TH = "th", THEAD = "thead", TIME = "time",
    TITLE = "title", TR = "tr", TRACK = "track", TT = "tt", U = "u", UL = "ul", VAR = "var",
    VIDEO = "video", WBR = "wbr", XMP = "xmp",
    // SVG / MathML names the tree builder looks at.
    FOREIGNOBJECT = "foreignObject", DESC = "desc", MI = "mi", MO = "mo", MN = "mn",
    MS = "ms", MTEXT = "mtext", ANNOTATION_XML = "annotation-xml",
}

/// Names one document may intern.
pub const MAX_ATOMS: usize = 1 << 20;

/// Per-document interner: static atoms first, then whatever the page uses.
pub struct Interner {
    names: Vec<String>,
    map: BTreeMap<String, Atom>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Interner {
        let mut map = BTreeMap::new();
        for (i, s) in STATIC.iter().enumerate() {
            map.insert(String::from(*s), Atom(i as u32));
        }
        Interner {
            names: Vec::new(),
            map,
        }
    }

    /// The atom for `s` (exact bytes; callers lower-case HTML names first). Past
    /// [`MAX_ATOMS`] new names (a script making up class names forever) all get the empty
    /// atom, which no selector matches: memory stays bounded, such names just stop matching.
    pub fn intern(&mut self, s: &str) -> Atom {
        if let Some(a) = self.map.get(s) {
            return *a;
        }
        if self.names.len() >= MAX_ATOMS || s.len() > 4096 {
            return tag::EMPTY;
        }
        let a = Atom(tag::COUNT + self.names.len() as u32);
        self.names.push(String::from(s));
        self.map.insert(String::from(s), a);
        a
    }

    /// The atom for `s` if it has one (no new entry).
    pub fn get(&self, s: &str) -> Option<Atom> {
        self.map.get(s).copied()
    }

    pub fn name(&self, a: Atom) -> &str {
        if a.0 < tag::COUNT {
            STATIC[a.0 as usize]
        } else {
            self.names
                .get((a.0 - tag::COUNT) as usize)
                .map(|s| s.as_str())
                .unwrap_or("")
        }
    }

    pub fn approx_bytes(&self) -> usize {
        self.names.iter().map(|s| s.len() * 2 + 48).sum()
    }
}
