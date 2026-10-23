//! The HTML tree builder, after WHATWG HTML §13.2.6 "Tree construction": the insertion modes,
//! the stack of open elements, the list of active formatting elements (with the Noah's Ark
//! clause and the adoption agency algorithm), foster parenting, and the rules for foreign
//! content (SVG and MathML are parsed so they nest correctly; they are not rendered).
//!
//! The scripting flag is off (no scripts run), so `<noscript>` content is parsed and shown.
//!
//! Simplifications:
//! - `<template>` contents are ordinary children of the template element (there is no
//!   separate DocumentFragment) and the "in template" mode is approximated by "in body";
//!   templates are display: none, so this never shows.
//! - No fragment parsing (no innerHTML without scripts).
//! - The SVG/MathML attribute-name adjustments are not applied (nothing reads them).
//!
//! Hostile input is bounded by [`Limits`]: node count, nesting depth (deeper elements are
//! attached at the limit) and a cap on the active formatting elements list.

use alloc::string::String;
use alloc::vec::Vec;

use super::tokenizer::{TagAttr, TextState, Token, Tokenizer};
use crate::api::Limits;
use crate::atoms::{Atom, tag};
use crate::dom::{Attr, Dom, NONE, NodeData, NodeId, Ns, QuirksMode};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InSelect,
    InSelectInTable,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

/// A start tag as the tree builder sees it.
#[derive(Clone, Debug)]
struct Tag {
    name: Atom,
    attrs: Vec<TagAttr>,
    self_closing: bool,
}

impl Tag {
    fn attr(&self, n: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| a.name == n)
            .map(|a| a.value.as_str())
    }
}

#[derive(Clone, Debug)]
enum Afe {
    Marker,
    /// The element and the token that made it (for reconstruction).
    Elem(NodeId, Tag),
}

enum Tok {
    Doctype {
        name: Option<String>,
        public_id: Option<String>,
        system_id: Option<String>,
        force_quirks: bool,
    },
    Start(Tag),
    End(Atom),
    Chars(String),
    Comment(String),
    Eof,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
    Select,
}

/// The result of a parse.
pub struct Parsed {
    pub dom: Dom,
    pub truncated: bool,
}

/// Parse a whole document (§13.2.6 driven by §13.2.5).
pub fn parse_document(input: &str, limits: &Limits) -> Parsed {
    let mut tb = TreeBuilder::new(limits);
    let mut tok = Tokenizer::new(input);
    loop {
        tok.allow_cdata = tb.adjusted_current_is_foreign();
        let t = tok.next_token();
        let t = tb.convert(t);
        let eof = matches!(t, Tok::Eof);
        tb.process(t, &mut tok);
        if eof || tb.stopped {
            break;
        }
        if tb.dom.len() >= limits.max_nodes {
            tb.truncated = true;
            tb.process(Tok::Eof, &mut tok);
            break;
        }
    }
    Parsed {
        dom: tb.dom,
        truncated: tb.truncated,
    }
}

struct TreeBuilder {
    dom: Dom,
    mode: Mode,
    original_mode: Mode,
    open: Vec<NodeId>,
    afe: Vec<Afe>,
    head: NodeId,
    form: NodeId,
    frameset_ok: bool,
    foster: bool,
    pending_table_text: String,
    pending_table_non_ws: bool,
    /// Skip a newline right after `<pre>`, `<listing>`, `<textarea>`.
    skip_lf: bool,
    max_depth: usize,
    truncated: bool,
    stopped: bool,
}

fn is_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ')
}

fn all_ws(s: &str) -> bool {
    s.chars().all(is_ws)
}

/// (leading whitespace, rest)
fn split_ws(s: &str) -> (&str, &str) {
    let i = s.find(|c: char| !is_ws(c)).unwrap_or(s.len());
    s.split_at(i)
}

const AFE_CAP: usize = 512;

impl TreeBuilder {
    fn new(limits: &Limits) -> TreeBuilder {
        TreeBuilder {
            dom: Dom::new(),
            mode: Mode::Initial,
            original_mode: Mode::Initial,
            open: Vec::new(),
            afe: Vec::new(),
            head: NONE,
            form: NONE,
            frameset_ok: true,
            foster: false,
            pending_table_text: String::new(),
            pending_table_non_ws: false,
            skip_lf: false,
            max_depth: limits.max_depth.max(8),
            truncated: false,
            stopped: false,
        }
    }

    fn convert(&mut self, t: Token) -> Tok {
        match t {
            Token::Doctype {
                name,
                public_id,
                system_id,
                force_quirks,
            } => Tok::Doctype {
                name,
                public_id,
                system_id,
                force_quirks,
            },
            Token::StartTag {
                name,
                attrs,
                self_closing,
            } => Tok::Start(Tag {
                name: self.dom.atoms.intern(&name),
                attrs,
                self_closing,
            }),
            Token::EndTag { name } => Tok::End(self.dom.atoms.intern(&name)),
            Token::Chars(s) => Tok::Chars(s),
            Token::Comment(s) => Tok::Comment(s),
            Token::Eof => Tok::Eof,
        }
    }

    // ---- element tests -------------------------------------------------------------------

    fn ns(&self, n: NodeId) -> Ns {
        self.dom.element(n).map(|e| e.ns).unwrap_or(Ns::Html)
    }

    fn name(&self, n: NodeId) -> Atom {
        self.dom.element(n).map(|e| e.name).unwrap_or(tag::EMPTY)
    }

    fn is(&self, n: NodeId, t: Atom) -> bool {
        self.dom.is_html(n, t)
    }

    fn is_one(&self, n: NodeId, ts: &[Atom]) -> bool {
        match self.dom.element(n) {
            Some(e) if e.ns == Ns::Html => ts.contains(&e.name),
            _ => false,
        }
    }

    fn current(&self) -> NodeId {
        self.open.last().copied().unwrap_or(NONE)
    }

    fn adjusted_current_is_foreign(&self) -> bool {
        let c = self.current();
        c != NONE && self.ns(c) != Ns::Html
    }

    /// §13.2.4.2 "special" category.
    fn is_special(&self, n: NodeId) -> bool {
        use tag::*;
        let Some(e) = self.dom.element(n) else {
            return false;
        };
        match e.ns {
            Ns::Html => matches!(
                e.name,
                ADDRESS
                    | APPLET
                    | AREA
                    | ARTICLE
                    | ASIDE
                    | BASE
                    | BASEFONT
                    | BGSOUND
                    | BLOCKQUOTE
                    | BODY
                    | BR
                    | BUTTON
                    | CAPTION
                    | CENTER
                    | COL
                    | COLGROUP
                    | DD
                    | DETAILS
                    | DIR
                    | DIV
                    | DL
                    | DT
                    | EMBED
                    | FIELDSET
                    | FIGCAPTION
                    | FIGURE
                    | FOOTER
                    | FORM
                    | FRAME
                    | FRAMESET
                    | H1
                    | H2
                    | H3
                    | H4
                    | H5
                    | H6
                    | HEAD
                    | HEADER
                    | HGROUP
                    | HR
                    | HTML
                    | IFRAME
                    | IMG
                    | INPUT
                    | KEYGEN
                    | LI
                    | LINK
                    | LISTING
                    | MAIN
                    | MARQUEE
                    | MENU
                    | META
                    | NAV
                    | NOEMBED
                    | NOFRAMES
                    | NOSCRIPT
                    | OBJECT
                    | OL
                    | P
                    | PARAM
                    | PLAINTEXT
                    | PRE
                    | SCRIPT
                    | SEARCH
                    | SECTION
                    | SELECT
                    | SOURCE
                    | STYLE
                    | SUMMARY
                    | TABLE
                    | TBODY
                    | TD
                    | TEMPLATE
                    | TEXTAREA
                    | TFOOT
                    | TH
                    | THEAD
                    | TITLE
                    | TR
                    | TRACK
                    | UL
                    | WBR
                    | XMP
            ),
            Ns::MathMl => matches!(e.name, MI | MO | MN | MS | MTEXT | ANNOTATION_XML),
            Ns::Svg => matches!(e.name, FOREIGNOBJECT | DESC | TITLE),
        }
    }

    /// §13.2.4.2 "has an element in the specific scope".
    fn in_scope_by(&self, matches: impl Fn(&Self, NodeId) -> bool, scope: Scope) -> bool {
        use tag::*;
        for &n in self.open.iter().rev() {
            if matches(self, n) {
                return true;
            }
            let e = match self.dom.element(n) {
                Some(e) => e,
                None => continue,
            };
            let boundary = match scope {
                Scope::Select => !(e.ns == Ns::Html && matches!(e.name, OPTGROUP | OPTION)),
                Scope::Table => e.ns == Ns::Html && matches!(e.name, HTML | TABLE | TEMPLATE),
                _ => {
                    let base = match e.ns {
                        Ns::Html => matches!(
                            e.name,
                            APPLET | CAPTION | HTML | TABLE | TD | TH | MARQUEE | OBJECT | TEMPLATE
                        ),
                        Ns::MathMl => matches!(e.name, MI | MO | MN | MS | MTEXT | ANNOTATION_XML),
                        Ns::Svg => matches!(e.name, FOREIGNOBJECT | DESC | TITLE),
                    };
                    base || (e.ns == Ns::Html
                        && match scope {
                            Scope::ListItem => matches!(e.name, OL | UL),
                            Scope::Button => e.name == BUTTON,
                            _ => false,
                        })
                }
            };
            if boundary {
                return false;
            }
        }
        false
    }

    fn in_scope(&self, t: Atom, scope: Scope) -> bool {
        self.in_scope_by(|s, n| s.is(n, t), scope)
    }

    fn in_scope_any(&self, ts: &[Atom], scope: Scope) -> bool {
        self.in_scope_by(|s, n| s.is_one(n, ts), scope)
    }

    fn in_open(&self, n: NodeId) -> bool {
        self.open.contains(&n)
    }

    fn open_has(&self, t: Atom) -> bool {
        self.open.iter().any(|&n| self.is(n, t))
    }

    // ---- inserting -----------------------------------------------------------------------

    /// §13.2.6.1 "appropriate place for inserting a node": (parent, insert before).
    fn place(&self, target: NodeId) -> (NodeId, NodeId) {
        use tag::*;
        if self.foster && self.is_one(target, &[TABLE, TBODY, TFOOT, THEAD, TR]) {
            let last_table = self.open.iter().rposition(|&n| self.is(n, TABLE));
            let last_template = self.open.iter().rposition(|&n| self.is(n, TEMPLATE));
            match (last_table, last_template) {
                (_, Some(tp)) if last_table.is_none_or(|t| tp > t) => return (self.open[tp], NONE),
                (None, _) => return (self.open[0], NONE),
                (Some(t), _) => {
                    let table = self.open[t];
                    let p = self.dom.parent(table);
                    if p != NONE {
                        return (p, table);
                    }
                    return (self.open[t.saturating_sub(1)], NONE);
                }
            }
        }
        (target, NONE)
    }

    fn insertion_target(&self) -> NodeId {
        // Past the depth limit, attach at the limit (the tree stays bounded).
        if self.open.len() > self.max_depth {
            self.open[self.max_depth - 1]
        } else {
            self.current()
        }
    }

    fn create(&mut self, t: &Tag, ns: Ns) -> NodeId {
        let attrs = t
            .attrs
            .iter()
            .map(|a| Attr {
                name: a.name.clone(),
                value: a.value.clone(),
            })
            .collect();
        self.dom.create_element(t.name, ns, attrs)
    }

    /// "insert a foreign element" / "insert an HTML element".
    fn insert_element(&mut self, t: &Tag, ns: Ns) -> NodeId {
        let el = self.create(t, ns);
        let target = self.insertion_target();
        let (p, before) = self.place(target);
        if p != NONE {
            self.dom.insert_before(p, el, before);
        }
        if self.open.len() > self.max_depth {
            self.truncated = true;
        }
        self.open.push(el);
        el
    }

    fn insert_html(&mut self, t: &Tag) -> NodeId {
        self.insert_element(t, Ns::Html)
    }

    fn insert_void(&mut self, t: &Tag) -> NodeId {
        let n = self.insert_html(t);
        self.open.pop();
        n
    }

    fn insert_chars(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let target = self.insertion_target();
        let (p, before) = self.place(target);
        if p == NONE || matches!(self.dom.node(p).data, NodeData::Document) {
            return;
        }
        self.dom.insert_text(p, before, s);
    }

    fn insert_comment(&mut self, s: String) {
        let target = self.insertion_target();
        let (p, before) = self.place(target);
        if p == NONE {
            return;
        }
        let c = self.dom.create_comment(s);
        self.dom.insert_before(p, c, before);
    }

    fn comment_to(&mut self, parent: NodeId, s: String) {
        let c = self.dom.create_comment(s);
        self.dom.append(parent, c);
    }

    fn tag(name: Atom) -> Tag {
        Tag {
            name,
            attrs: Vec::new(),
            self_closing: false,
        }
    }

    // ---- popping -------------------------------------------------------------------------

    fn pop(&mut self) -> NodeId {
        self.open.pop().unwrap_or(NONE)
    }

    /// Pop until an HTML element `t` has been popped.
    fn pop_until(&mut self, t: Atom) {
        while let Some(n) = self.open.pop() {
            if self.is(n, t) {
                break;
            }
        }
    }

    fn pop_until_any(&mut self, ts: &[Atom]) {
        while let Some(n) = self.open.pop() {
            if self.is_one(n, ts) {
                break;
            }
        }
    }

    /// §13.2.6.3 "generate implied end tags", except for `except`.
    fn implied_end_tags(&mut self, except: Atom) {
        use tag::*;
        loop {
            let c = self.current();
            if c == NONE || self.is(c, except) {
                return;
            }
            if self.is_one(c, &[DD, DT, LI, OPTGROUP, OPTION, P, RB, RP, RT, RTC]) {
                self.pop();
            } else {
                return;
            }
        }
    }

    fn implied_end_tags_thoroughly(&mut self) {
        use tag::*;
        while self.is_one(
            self.current(),
            &[
                CAPTION, COLGROUP, DD, DT, LI, OPTGROUP, OPTION, P, RB, RP, RT, RTC, TBODY, TD,
                TFOOT, TH, THEAD, TR,
            ],
        ) {
            self.pop();
        }
    }

    fn close_p(&mut self) {
        self.implied_end_tags(tag::P);
        self.pop_until(tag::P);
    }

    fn close_p_in_button_scope(&mut self) {
        if self.in_scope(tag::P, Scope::Button) {
            self.close_p();
        }
    }

    // ---- active formatting elements (§13.2.4.3) -------------------------------------------

    fn push_afe(&mut self, n: NodeId, t: &Tag) {
        // The Noah's Ark clause: at most three identical entries after the last marker.
        let mut same = Vec::new();
        for (i, e) in self.afe.iter().enumerate().rev() {
            match e {
                Afe::Marker => break,
                Afe::Elem(_, et) => {
                    if et.name == t.name && same_attrs(&et.attrs, &t.attrs) {
                        same.push(i);
                    }
                }
            }
        }
        if same.len() >= 3 {
            let earliest = *same.last().unwrap();
            self.afe.remove(earliest);
        }
        if self.afe.len() >= AFE_CAP {
            // Bounded work for hostile pages: forget the oldest entry.
            if let Some(i) = self.afe.iter().position(|e| matches!(e, Afe::Elem(..))) {
                self.afe.remove(i);
            }
        }
        self.afe.push(Afe::Elem(n, t.clone()));
    }

    fn reconstruct_afe(&mut self) {
        let Some(last) = self.afe.last() else { return };
        match last {
            Afe::Marker => return,
            Afe::Elem(n, _) if self.in_open(*n) => return,
            _ => {}
        }
        let mut i = self.afe.len() - 1;
        // Rewind.
        while i > 0 {
            match &self.afe[i - 1] {
                Afe::Marker => break,
                Afe::Elem(n, _) if self.in_open(*n) => break,
                _ => i -= 1,
            }
        }
        // Advance and create.
        while i < self.afe.len() {
            let t = match &self.afe[i] {
                Afe::Elem(_, t) => t.clone(),
                Afe::Marker => break,
            };
            let n = self.insert_html(&t);
            self.afe[i] = Afe::Elem(n, t);
            i += 1;
        }
    }

    fn clear_afe_to_marker(&mut self) {
        while let Some(e) = self.afe.pop() {
            if matches!(e, Afe::Marker) {
                break;
            }
        }
    }

    fn afe_index(&self, n: NodeId) -> Option<usize> {
        self.afe
            .iter()
            .position(|e| matches!(e, Afe::Elem(x, _) if *x == n))
    }

    /// The last formatting element named `t` after the last marker.
    fn afe_find_after_marker(&self, t: Atom) -> Option<usize> {
        for (i, e) in self.afe.iter().enumerate().rev() {
            match e {
                Afe::Marker => return None,
                Afe::Elem(n, _) if self.is(*n, t) => return Some(i),
                _ => {}
            }
        }
        None
    }

    /// §13.2.6.4.7 the adoption agency algorithm. Returns false for "act as any other end tag".
    fn adoption_agency(&mut self, subject: Atom) -> bool {
        let c = self.current();
        if self.is(c, subject) && self.afe_index(c).is_none() {
            self.pop();
            return true;
        }
        for _outer in 0..8 {
            let Some(fi) = self.afe_find_after_marker(subject) else {
                return false;
            };
            let Afe::Elem(fe, fe_tag) = self.afe[fi].clone() else {
                return false;
            };
            let Some(fe_pos) = self.open.iter().position(|&n| n == fe) else {
                self.afe.remove(fi);
                return true;
            };
            if !self.in_scope_by(|_, n| n == fe, Scope::Default) {
                return true;
            }
            // The furthest block: the topmost special element below fe in the stack.
            let fb_pos = (fe_pos + 1..self.open.len()).find(|&i| self.is_special(self.open[i]));
            let Some(fb_pos) = fb_pos else {
                self.open.truncate(fe_pos);
                self.afe.remove(fi);
                return true;
            };
            let fb = self.open[fb_pos];
            let common = self.open[fe_pos - 1];
            let mut bookmark = fi;
            let mut node_pos = fb_pos;
            let mut last = fb;
            let mut inner = 0;
            loop {
                inner += 1;
                node_pos -= 1;
                let node = self.open[node_pos];
                if node == fe {
                    break;
                }
                let ni = self.afe_index(node);
                if inner > 3
                    && let Some(ni) = ni
                {
                    self.afe.remove(ni);
                    if ni < bookmark {
                        bookmark -= 1;
                    }
                }
                let Some(ni) = self.afe_index(node) else {
                    self.open.remove(node_pos);
                    continue;
                };
                // Replace node with a new element made from its token.
                let Afe::Elem(_, nt) = self.afe[ni].clone() else {
                    break;
                };
                let new = self.create(&nt, Ns::Html);
                self.afe[ni] = Afe::Elem(new, nt);
                self.open[node_pos] = new;
                if last == fb {
                    bookmark = ni + 1;
                }
                self.dom.append(new, last);
                last = new;
            }
            // Insert `last` at the appropriate place for `common`.
            self.dom.detach(last);
            let (p, before) = self.place(common);
            if p != NONE {
                self.dom.insert_before(p, last, before);
            }
            let new = self.create(&fe_tag, Ns::Html);
            self.dom.reparent_children(fb, new);
            self.dom.append(fb, new);
            // Move the formatting entry to the bookmark.
            if let Some(i) = self.afe_index(fe) {
                self.afe.remove(i);
                if i < bookmark {
                    bookmark -= 1;
                }
            }
            let bookmark = bookmark.min(self.afe.len());
            self.afe.insert(bookmark, Afe::Elem(new, fe_tag));
            if let Some(i) = self.open.iter().position(|&n| n == fe) {
                self.open.remove(i);
            }
            if let Some(i) = self.open.iter().position(|&n| n == fb) {
                self.open.insert(i + 1, new);
            }
        }
        true
    }

    // ---- the reset algorithm (§13.2.4.1) ---------------------------------------------------

    fn reset_mode(&mut self) {
        use tag::*;
        for i in (0..self.open.len()).rev() {
            let n = self.open[i];
            let last = i == 0;
            let t = self.dom.html_tag(n);
            let m = match t {
                SELECT => {
                    let mut m = Mode::InSelect;
                    for j in (0..i).rev() {
                        let a = self.open[j];
                        if self.is(a, TEMPLATE) {
                            break;
                        }
                        if self.is(a, TABLE) {
                            m = Mode::InSelectInTable;
                            break;
                        }
                    }
                    Some(m)
                }
                TD | TH if !last => Some(Mode::InCell),
                TR => Some(Mode::InRow),
                TBODY | THEAD | TFOOT => Some(Mode::InTableBody),
                CAPTION => Some(Mode::InCaption),
                COLGROUP => Some(Mode::InColumnGroup),
                TABLE => Some(Mode::InTable),
                TEMPLATE => Some(Mode::InBody),
                HEAD if !last => Some(Mode::InHead),
                BODY => Some(Mode::InBody),
                FRAMESET => Some(Mode::InFrameset),
                HTML => Some(if self.head == NONE {
                    Mode::BeforeHead
                } else {
                    Mode::AfterHead
                }),
                _ => None,
            };
            if let Some(m) = m {
                self.mode = m;
                return;
            }
            if last {
                self.mode = Mode::InBody;
                return;
            }
        }
        self.mode = Mode::InBody;
    }

    // ---- dispatch --------------------------------------------------------------------------

    fn process(&mut self, t: Tok, tz: &mut Tokenizer) {
        let mut t = t;
        if self.skip_lf {
            self.skip_lf = false;
            if let Tok::Chars(s) = &t
                && let Some(r) = s.strip_prefix('\n')
            {
                if r.is_empty() {
                    return;
                }
                t = Tok::Chars(String::from(r));
            }
        }
        // §13.2.6 tree construction dispatcher: foreign content or the current mode.
        if self.use_foreign_rules(&t) {
            self.foreign(t, tz);
        } else {
            self.mode_step(t, tz);
        }
    }

    fn use_foreign_rules(&self, t: &Tok) -> bool {
        let c = self.current();
        if c == NONE || self.ns(c) == Ns::Html {
            return false;
        }
        let e = self.dom.element(c).unwrap();
        // MathML text integration points and HTML integration points.
        let mathml_text = e.ns == Ns::MathMl
            && matches!(e.name, tag::MI | tag::MO | tag::MN | tag::MS | tag::MTEXT);
        let html_ip = (e.ns == Ns::Svg
            && matches!(e.name, tag::FOREIGNOBJECT | tag::DESC | tag::TITLE))
            || (e.ns == Ns::MathMl
                && e.name == tag::ANNOTATION_XML
                && e.attr("encoding").is_some_and(|v| {
                    v.eq_ignore_ascii_case("text/html")
                        || v.eq_ignore_ascii_case("application/xhtml+xml")
                }));
        match t {
            Tok::Eof => false,
            Tok::Start(s)
                if mathml_text
                    && s.name != self.dom.atoms.get("mglyph").unwrap_or(Atom(u32::MAX))
                    && s.name != self.dom.atoms.get("malignmark").unwrap_or(Atom(u32::MAX)) =>
            {
                false
            }
            Tok::Chars(_) if mathml_text || html_ip => false,
            Tok::Start(s)
                if e.ns == Ns::MathMl && e.name == tag::ANNOTATION_XML && s.name == tag::SVG =>
            {
                false
            }
            Tok::Start(_) if html_ip => false,
            _ => true,
        }
    }

    /// §13.2.6.5 "The rules for parsing tokens in foreign content".
    fn foreign(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Chars(s) => {
                let s = s.replace('\0', "\u{FFFD}");
                if !all_ws(&s) {
                    self.frameset_ok = false;
                }
                self.insert_chars(&s);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) => {
                let breakout = matches!(
                    st.name,
                    B | BIG
                        | BLOCKQUOTE
                        | BODY
                        | BR
                        | CENTER
                        | CODE
                        | DD
                        | DIV
                        | DL
                        | DT
                        | EM
                        | EMBED
                        | H1
                        | H2
                        | H3
                        | H4
                        | H5
                        | H6
                        | HEAD
                        | HR
                        | I
                        | IMG
                        | LI
                        | LISTING
                        | MENU
                        | META
                        | NOBR
                        | OL
                        | P
                        | PRE
                        | RUBY
                        | S
                        | SMALL
                        | SPAN
                        | STRONG
                        | STRIKE
                        | SUB
                        | SUP
                        | TABLE
                        | TT
                        | U
                        | UL
                        | VAR
                ) || (st.name == FONT
                    && st
                        .attrs
                        .iter()
                        .any(|a| a.name == "color" || a.name == "face" || a.name == "size"));
                if breakout {
                    // Pop until an HTML element or an integration point, then reprocess.
                    while {
                        let c = self.current();
                        c != NONE && self.ns(c) != Ns::Html && !self.is_integration_point(c)
                    } {
                        self.pop();
                    }
                    self.mode_step(Tok::Start(st), tz);
                    return;
                }
                let ns = self.ns(self.current());
                let mut st = st;
                if ns == Ns::Svg {
                    st.name = self.svg_adjust(st.name);
                }
                self.insert_element(&st, ns);
                if st.self_closing {
                    self.pop();
                }
            }
            Tok::End(name) => {
                if name == SCRIPT
                    && self.ns(self.current()) == Ns::Svg
                    && self.name(self.current()) == SCRIPT
                {
                    self.pop();
                    return;
                }
                // Walk down the stack for an element with this name (ASCII case-insensitive).
                let mut i = self.open.len();
                while i > 0 {
                    i -= 1;
                    let n = self.open[i];
                    if self.ns(n) == Ns::Html {
                        self.mode_step(Tok::End(name), tz);
                        return;
                    }
                    let nn = self.dom.atoms.name(self.name(n));
                    let en = self.dom.atoms.name(name);
                    if nn.eq_ignore_ascii_case(en) {
                        self.open.truncate(i);
                        return;
                    }
                    if i == 0 {
                        return;
                    }
                }
            }
            Tok::Eof => self.mode_step(Tok::Eof, tz),
        }
    }

    fn is_integration_point(&self, n: NodeId) -> bool {
        let Some(e) = self.dom.element(n) else {
            return false;
        };
        (e.ns == Ns::MathMl && matches!(e.name, tag::MI | tag::MO | tag::MN | tag::MS | tag::MTEXT))
            || (e.ns == Ns::Svg && matches!(e.name, tag::FOREIGNOBJECT | tag::DESC | tag::TITLE))
    }

    /// SVG's camelCase tag names (§13.2.6.5 table); only the one the tree builder cares about.
    fn svg_adjust(&mut self, a: Atom) -> Atom {
        if self.dom.atoms.name(a) == "foreignobject" {
            tag::FOREIGNOBJECT
        } else {
            a
        }
    }

    fn mode_step(&mut self, t: Tok, tz: &mut Tokenizer) {
        match self.mode {
            Mode::Initial => self.initial(t, tz),
            Mode::BeforeHtml => self.before_html(t, tz),
            Mode::BeforeHead => self.before_head(t, tz),
            Mode::InHead => self.in_head(t, tz),
            Mode::InHeadNoscript => self.in_head_noscript(t, tz),
            Mode::AfterHead => self.after_head(t, tz),
            Mode::InBody => self.in_body(t, tz),
            Mode::Text => self.text(t, tz),
            Mode::InTable => self.in_table(t, tz),
            Mode::InTableText => self.in_table_text(t, tz),
            Mode::InCaption => self.in_caption(t, tz),
            Mode::InColumnGroup => self.in_column_group(t, tz),
            Mode::InTableBody => self.in_table_body(t, tz),
            Mode::InRow => self.in_row(t, tz),
            Mode::InCell => self.in_cell(t, tz),
            Mode::InSelect => self.in_select(t, tz),
            Mode::InSelectInTable => self.in_select_in_table(t, tz),
            Mode::AfterBody => self.after_body(t, tz),
            Mode::InFrameset => self.in_frameset(t, tz),
            Mode::AfterFrameset => self.after_frameset(t, tz),
            Mode::AfterAfterBody => self.after_after_body(t, tz),
            Mode::AfterAfterFrameset => self.after_after_frameset(t, tz),
        }
    }

    fn reprocess_in(&mut self, m: Mode, t: Tok, tz: &mut Tokenizer) {
        self.mode = m;
        self.mode_step(t, tz);
    }

    // §13.2.6.4.1
    fn initial(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Chars(s) => {
                let (_, rest) = split_ws(&s);
                if rest.is_empty() {
                    return;
                }
                self.dom.quirks = QuirksMode::Quirks;
                self.reprocess_in(Mode::BeforeHtml, Tok::Chars(String::from(rest)), tz);
            }
            Tok::Comment(s) => self.comment_to(0, s),
            Tok::Doctype {
                name,
                public_id,
                system_id,
                force_quirks,
            } => {
                self.dom.quirks = quirks_from_doctype(
                    name.as_deref(),
                    public_id.as_deref(),
                    system_id.as_deref(),
                    force_quirks,
                );
                let d = self.dom.create_doctype(
                    name.unwrap_or_default(),
                    public_id.unwrap_or_default(),
                    system_id.unwrap_or_default(),
                );
                self.dom.append(0, d);
                self.mode = Mode::BeforeHtml;
            }
            t => {
                self.dom.quirks = QuirksMode::Quirks;
                self.reprocess_in(Mode::BeforeHtml, t, tz);
            }
        }
    }

    // §13.2.6.4.2
    fn before_html(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Doctype { .. } => {}
            Tok::Comment(s) => self.comment_to(0, s),
            Tok::Chars(s) => {
                let (_, rest) = split_ws(&s);
                if rest.is_empty() {
                    return;
                }
                self.implicit_html();
                self.reprocess_in(Mode::BeforeHead, Tok::Chars(String::from(rest)), tz);
            }
            Tok::Start(st) if st.name == tag::HTML => {
                let n = self.create(&st, Ns::Html);
                self.dom.append(0, n);
                self.open.push(n);
                self.mode = Mode::BeforeHead;
            }
            Tok::End(n) if !matches!(n, tag::HEAD | tag::BODY | tag::HTML | tag::BR) => {}
            t => {
                self.implicit_html();
                self.reprocess_in(Mode::BeforeHead, t, tz);
            }
        }
    }

    fn implicit_html(&mut self) {
        let n = self.create(&Self::tag(tag::HTML), Ns::Html);
        self.dom.append(0, n);
        self.open.push(n);
    }

    // §13.2.6.4.3
    fn before_head(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Chars(s) => {
                let (_, rest) = split_ws(&s);
                if rest.is_empty() {
                    return;
                }
                self.implicit_head();
                self.reprocess_in(Mode::InHead, Tok::Chars(String::from(rest)), tz);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) if st.name == tag::HTML => self.in_body(Tok::Start(st), tz),
            Tok::Start(st) if st.name == tag::HEAD => {
                self.head = self.insert_html(&st);
                self.mode = Mode::InHead;
            }
            Tok::End(n) if !matches!(n, tag::HEAD | tag::BODY | tag::HTML | tag::BR) => {}
            t => {
                self.implicit_head();
                self.reprocess_in(Mode::InHead, t, tz);
            }
        }
    }

    fn implicit_head(&mut self) {
        self.head = self.insert_html(&Self::tag(tag::HEAD));
    }

    // §13.2.6.4.4
    fn in_head(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Chars(s) => {
                let (ws, rest) = split_ws(&s);
                self.insert_chars(ws);
                if rest.is_empty() {
                    return;
                }
                self.pop();
                self.reprocess_in(Mode::AfterHead, Tok::Chars(String::from(rest)), tz);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) => match st.name {
                HTML => self.in_body(Tok::Start(st), tz),
                BASE | BASEFONT | BGSOUND | LINK | META => {
                    self.insert_void(&st);
                }
                TITLE => self.generic_text(&st, TextState::Rcdata, tz),
                NOFRAMES | STYLE => self.generic_text(&st, TextState::Rawtext, tz),
                NOSCRIPT => {
                    self.insert_html(&st);
                    self.mode = Mode::InHeadNoscript;
                }
                SCRIPT => self.generic_text(&st, TextState::ScriptData, tz),
                TEMPLATE => {
                    self.insert_html(&st);
                    self.afe.push(Afe::Marker);
                    self.frameset_ok = false;
                    self.mode = Mode::InBody;
                }
                HEAD => {}
                _ => {
                    self.pop();
                    self.reprocess_in(Mode::AfterHead, Tok::Start(st), tz);
                }
            },
            Tok::End(n) => match n {
                HEAD => {
                    self.pop();
                    self.mode = Mode::AfterHead;
                }
                BODY | HTML | BR => {
                    self.pop();
                    self.reprocess_in(Mode::AfterHead, Tok::End(n), tz);
                }
                TEMPLATE => self.end_template(),
                _ => {}
            },
            Tok::Eof => {
                self.pop();
                self.reprocess_in(Mode::AfterHead, Tok::Eof, tz);
            }
        }
    }

    fn end_template(&mut self) {
        if !self.open_has(tag::TEMPLATE) {
            return;
        }
        self.implied_end_tags_thoroughly();
        self.pop_until(tag::TEMPLATE);
        self.clear_afe_to_marker();
        self.reset_mode();
    }

    /// §13.2.6.2 the generic raw text / RCDATA element parsing algorithm.
    fn generic_text(&mut self, st: &Tag, state: TextState, tz: &mut Tokenizer) {
        self.insert_html(st);
        tz.set_state(state);
        self.original_mode = self.mode;
        self.mode = Mode::Text;
    }

    // §13.2.6.4.5
    fn in_head_noscript(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Doctype { .. } => {}
            Tok::Start(st) if st.name == HTML => self.in_body(Tok::Start(st), tz),
            Tok::End(NOSCRIPT) => {
                self.pop();
                self.mode = Mode::InHead;
            }
            Tok::Chars(s) if all_ws(&s) => self.in_head(Tok::Chars(s), tz),
            Tok::Comment(s) => self.in_head(Tok::Comment(s), tz),
            Tok::Start(st)
                if matches!(st.name, BASEFONT | BGSOUND | LINK | META | NOFRAMES | STYLE) =>
            {
                self.in_head(Tok::Start(st), tz)
            }
            Tok::Start(st) if matches!(st.name, HEAD | NOSCRIPT) => {}
            Tok::End(n) if n != BR => {}
            t => {
                self.pop();
                self.reprocess_in(Mode::InHead, t, tz);
            }
        }
    }

    // §13.2.6.4.6
    fn after_head(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Chars(s) => {
                let (ws, rest) = split_ws(&s);
                self.insert_chars(ws);
                if rest.is_empty() {
                    return;
                }
                self.implicit_body();
                self.reprocess_in(Mode::InBody, Tok::Chars(String::from(rest)), tz);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) => match st.name {
                HTML => self.in_body(Tok::Start(st), tz),
                BODY => {
                    self.insert_html(&st);
                    self.frameset_ok = false;
                    self.mode = Mode::InBody;
                }
                FRAMESET => {
                    self.insert_html(&st);
                    self.mode = Mode::InFrameset;
                }
                BASE | BASEFONT | BGSOUND | LINK | META | NOFRAMES | SCRIPT | STYLE | TEMPLATE
                | TITLE => {
                    if self.head != NONE {
                        self.open.push(self.head);
                        self.in_head(Tok::Start(st), tz);
                        if let Some(i) = self.open.iter().rposition(|&n| n == self.head) {
                            self.open.remove(i);
                        }
                    }
                }
                HEAD => {}
                _ => {
                    self.implicit_body();
                    self.reprocess_in(Mode::InBody, Tok::Start(st), tz);
                }
            },
            Tok::End(TEMPLATE) => self.in_head(Tok::End(TEMPLATE), tz),
            Tok::End(n) if !matches!(n, BODY | HTML | BR) => {}
            t => {
                self.implicit_body();
                self.reprocess_in(Mode::InBody, t, tz);
            }
        }
    }

    fn implicit_body(&mut self) {
        self.insert_html(&Self::tag(tag::BODY));
    }

    // §13.2.6.4.7 "in body"
    fn in_body(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Chars(s) => {
                let s = if s.contains('\0') {
                    s.replace('\0', "")
                } else {
                    s
                };
                if s.is_empty() {
                    return;
                }
                self.reconstruct_afe();
                if !all_ws(&s) {
                    self.frameset_ok = false;
                }
                self.insert_chars(&s);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) => self.in_body_start(st, tz),
            Tok::End(n) => self.in_body_end(n, tz),
            Tok::Eof => {
                // Stop parsing (§13.2.6.5 "stop parsing": pop everything).
                self.open.clear();
                self.stopped = true;
            }
        }
    }

    fn in_body_start(&mut self, st: Tag, tz: &mut Tokenizer) {
        use tag::*;
        match st.name {
            HTML => {
                if self.open_has(TEMPLATE) {
                    return;
                }
                // Add attributes the html element doesn't have yet.
                if let Some(&root) = self.open.first() {
                    self.merge_attrs(root, &st);
                }
            }
            BASE | BASEFONT | BGSOUND | LINK | META | NOFRAMES | SCRIPT | STYLE | TEMPLATE
            | TITLE => self.in_head(Tok::Start(st), tz),
            BODY => {
                if self.open.len() < 2 || !self.is(self.open[1], BODY) || self.open_has(TEMPLATE) {
                    return;
                }
                self.frameset_ok = false;
                let body = self.open[1];
                self.merge_attrs(body, &st);
            }
            FRAMESET => {
                if self.open.len() < 2 || !self.is(self.open[1], BODY) || !self.frameset_ok {
                    return;
                }
                let body = self.open[1];
                self.dom.detach(body);
                self.open.truncate(1);
                self.insert_html(&st);
                self.mode = Mode::InFrameset;
            }
            ADDRESS | ARTICLE | ASIDE | BLOCKQUOTE | CENTER | DETAILS | DIALOG | DIR | DIV | DL
            | FIELDSET | FIGCAPTION | FIGURE | FOOTER | HEADER | HGROUP | MAIN | MENU | NAV
            | OL | P | SEARCH | SECTION | SUMMARY | UL => {
                self.close_p_in_button_scope();
                self.insert_html(&st);
            }
            H1 | H2 | H3 | H4 | H5 | H6 => {
                self.close_p_in_button_scope();
                if self.is_one(self.current(), &[H1, H2, H3, H4, H5, H6]) {
                    self.pop();
                }
                self.insert_html(&st);
            }
            PRE | LISTING => {
                self.close_p_in_button_scope();
                self.insert_html(&st);
                self.skip_lf = true;
                self.frameset_ok = false;
            }
            FORM => {
                let has_template = self.open_has(TEMPLATE);
                if self.form != NONE && !has_template {
                    return;
                }
                self.close_p_in_button_scope();
                let f = self.insert_html(&st);
                if !has_template {
                    self.form = f;
                }
            }
            LI => {
                self.frameset_ok = false;
                for i in (0..self.open.len()).rev() {
                    let n = self.open[i];
                    if self.is(n, LI) {
                        self.implied_end_tags(LI);
                        self.pop_until(LI);
                        break;
                    }
                    if self.is_special(n) && !self.is_one(n, &[ADDRESS, DIV, P]) {
                        break;
                    }
                }
                self.close_p_in_button_scope();
                self.insert_html(&st);
            }
            DD | DT => {
                self.frameset_ok = false;
                for i in (0..self.open.len()).rev() {
                    let n = self.open[i];
                    if self.is_one(n, &[DD, DT]) {
                        let t = self.name(n);
                        self.implied_end_tags(t);
                        self.pop_until(t);
                        break;
                    }
                    if self.is_special(n) && !self.is_one(n, &[ADDRESS, DIV, P]) {
                        break;
                    }
                }
                self.close_p_in_button_scope();
                self.insert_html(&st);
            }
            PLAINTEXT => {
                self.close_p_in_button_scope();
                self.insert_html(&st);
                tz.set_state(TextState::Plaintext);
            }
            BUTTON => {
                if self.in_scope(BUTTON, Scope::Default) {
                    self.implied_end_tags(EMPTY);
                    self.pop_until(BUTTON);
                }
                self.reconstruct_afe();
                self.insert_html(&st);
                self.frameset_ok = false;
            }
            A => {
                if let Some(i) = self.afe_find_after_marker(A) {
                    let Afe::Elem(a, _) = self.afe[i] else {
                        unreachable!()
                    };
                    self.adoption_agency(A);
                    if let Some(j) = self.afe_index(a) {
                        self.afe.remove(j);
                    }
                    if let Some(j) = self.open.iter().position(|&n| n == a) {
                        self.open.remove(j);
                    }
                }
                self.reconstruct_afe();
                let n = self.insert_html(&st);
                self.push_afe(n, &st);
            }
            B | BIG | CODE | EM | FONT | I | S | SMALL | STRIKE | STRONG | TT | U => {
                self.reconstruct_afe();
                let n = self.insert_html(&st);
                self.push_afe(n, &st);
            }
            NOBR => {
                self.reconstruct_afe();
                if self.in_scope(NOBR, Scope::Default) {
                    self.adoption_agency(NOBR);
                    self.reconstruct_afe();
                }
                let n = self.insert_html(&st);
                self.push_afe(n, &st);
            }
            APPLET | MARQUEE | OBJECT => {
                self.reconstruct_afe();
                self.insert_html(&st);
                self.afe.push(Afe::Marker);
                self.frameset_ok = false;
            }
            TABLE => {
                if self.dom.quirks != QuirksMode::Quirks {
                    self.close_p_in_button_scope();
                }
                self.insert_html(&st);
                self.frameset_ok = false;
                self.mode = Mode::InTable;
            }
            AREA | BR | EMBED | IMG | KEYGEN | WBR => {
                self.reconstruct_afe();
                self.insert_void(&st);
                self.frameset_ok = false;
            }
            INPUT => {
                self.reconstruct_afe();
                self.insert_void(&st);
                if !st
                    .attr("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("hidden"))
                {
                    self.frameset_ok = false;
                }
            }
            PARAM | SOURCE | TRACK => {
                self.insert_void(&st);
            }
            HR => {
                self.close_p_in_button_scope();
                self.insert_void(&st);
                self.frameset_ok = false;
            }
            IMAGE => {
                // "Don't ask."
                let mut st = st;
                st.name = IMG;
                self.in_body_start(st, tz);
            }
            TEXTAREA => {
                self.insert_html(&st);
                self.skip_lf = true;
                tz.set_state(TextState::Rcdata);
                self.original_mode = self.mode;
                self.frameset_ok = false;
                self.mode = Mode::Text;
            }
            XMP => {
                self.close_p_in_button_scope();
                self.reconstruct_afe();
                self.frameset_ok = false;
                self.generic_text(&st, TextState::Rawtext, tz);
            }
            IFRAME => {
                self.frameset_ok = false;
                self.generic_text(&st, TextState::Rawtext, tz);
            }
            NOEMBED => self.generic_text(&st, TextState::Rawtext, tz),
            SELECT => {
                self.reconstruct_afe();
                self.insert_html(&st);
                self.frameset_ok = false;
                self.mode = match self.mode {
                    Mode::InTable
                    | Mode::InCaption
                    | Mode::InTableBody
                    | Mode::InRow
                    | Mode::InCell => Mode::InSelectInTable,
                    _ => Mode::InSelect,
                };
            }
            OPTGROUP | OPTION => {
                if self.is(self.current(), OPTION) {
                    self.pop();
                }
                self.reconstruct_afe();
                self.insert_html(&st);
            }
            RB | RTC => {
                if self.in_scope(RUBY, Scope::Default) {
                    self.implied_end_tags(EMPTY);
                }
                self.insert_html(&st);
            }
            RP | RT => {
                if self.in_scope(RUBY, Scope::Default) {
                    self.implied_end_tags(RTC);
                }
                self.insert_html(&st);
            }
            MATH | SVG => {
                self.reconstruct_afe();
                let ns = if st.name == MATH { Ns::MathMl } else { Ns::Svg };
                self.insert_element(&st, ns);
                if st.self_closing {
                    self.pop();
                }
            }
            CAPTION | COL | COLGROUP | FRAME | HEAD | TBODY | TD | TFOOT | TH | THEAD | TR => {}
            _ => {
                self.reconstruct_afe();
                self.insert_html(&st);
            }
        }
    }

    fn merge_attrs(&mut self, n: NodeId, st: &Tag) {
        if let Some(e) = self.dom.element_mut(n) {
            for a in &st.attrs {
                if !e.attrs.iter().any(|x| x.name == a.name) {
                    e.attrs.push(Attr {
                        name: a.name.clone(),
                        value: a.value.clone(),
                    });
                }
            }
        }
    }

    fn in_body_end(&mut self, name: Atom, tz: &mut Tokenizer) {
        use tag::*;
        match name {
            TEMPLATE => self.end_template(),
            BODY | HTML => {
                if !self.in_scope(BODY, Scope::Default) {
                    return;
                }
                self.mode = Mode::AfterBody;
                if name == HTML {
                    self.after_body(Tok::End(HTML), tz);
                }
            }
            ADDRESS | ARTICLE | ASIDE | BLOCKQUOTE | BUTTON | CENTER | DETAILS | DIALOG | DIR
            | DIV | DL | FIELDSET | FIGCAPTION | FIGURE | FOOTER | HEADER | HGROUP | LISTING
            | MAIN | MENU | NAV | OL | PRE | SEARCH | SECTION | SUMMARY | UL => {
                if !self.in_scope(name, Scope::Default) {
                    return;
                }
                self.implied_end_tags(EMPTY);
                self.pop_until(name);
            }
            FORM => {
                if !self.open_has(TEMPLATE) {
                    let node = self.form;
                    self.form = NONE;
                    if node == NONE || !self.in_scope_by(|_, n| n == node, Scope::Default) {
                        return;
                    }
                    self.implied_end_tags(EMPTY);
                    if let Some(i) = self.open.iter().position(|&n| n == node) {
                        self.open.remove(i);
                    }
                } else {
                    if !self.in_scope(FORM, Scope::Default) {
                        return;
                    }
                    self.implied_end_tags(EMPTY);
                    self.pop_until(FORM);
                }
            }
            P => {
                if !self.in_scope(P, Scope::Button) {
                    self.insert_html(&Self::tag(P));
                }
                self.close_p();
            }
            LI => {
                if !self.in_scope(LI, Scope::ListItem) {
                    return;
                }
                self.implied_end_tags(LI);
                self.pop_until(LI);
            }
            DD | DT => {
                if !self.in_scope(name, Scope::Default) {
                    return;
                }
                self.implied_end_tags(name);
                self.pop_until(name);
            }
            H1 | H2 | H3 | H4 | H5 | H6 => {
                let hs = [H1, H2, H3, H4, H5, H6];
                if !self.in_scope_any(&hs, Scope::Default) {
                    return;
                }
                self.implied_end_tags(EMPTY);
                self.pop_until_any(&hs);
            }
            A | B | BIG | CODE | EM | FONT | I | NOBR | S | SMALL | STRIKE | STRONG | TT | U => {
                if !self.adoption_agency(name) {
                    self.any_other_end_tag(name);
                }
            }
            APPLET | MARQUEE | OBJECT => {
                if !self.in_scope(name, Scope::Default) {
                    return;
                }
                self.implied_end_tags(EMPTY);
                self.pop_until(name);
                self.clear_afe_to_marker();
            }
            BR => {
                // "</br>" acts like "<br>".
                self.in_body_start(Self::tag(BR), tz);
            }
            _ => self.any_other_end_tag(name),
        }
    }

    fn any_other_end_tag(&mut self, name: Atom) {
        for i in (0..self.open.len()).rev() {
            let n = self.open[i];
            if self.is(n, name) {
                self.implied_end_tags(name);
                self.open.truncate(i);
                return;
            }
            if self.is_special(n) {
                return;
            }
        }
    }

    // §13.2.6.4.8
    fn text(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Chars(s) => self.insert_chars(&s),
            Tok::Eof => {
                self.pop();
                self.mode = self.original_mode;
                self.mode_step(Tok::Eof, tz);
            }
            Tok::End(_) => {
                self.pop();
                self.mode = self.original_mode;
            }
            _ => {}
        }
    }

    // §13.2.6.4.9 "in table"
    fn in_table(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Chars(s)
                if self.is_one(self.current(), &[TABLE, TBODY, TEMPLATE, TFOOT, THEAD, TR]) =>
            {
                self.pending_table_text.clear();
                self.pending_table_non_ws = false;
                self.original_mode = self.mode;
                self.mode = Mode::InTableText;
                self.in_table_text(Tok::Chars(s), tz);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) => match st.name {
                CAPTION => {
                    self.clear_to_table_context();
                    self.afe.push(Afe::Marker);
                    self.insert_html(&st);
                    self.mode = Mode::InCaption;
                }
                COLGROUP => {
                    self.clear_to_table_context();
                    self.insert_html(&st);
                    self.mode = Mode::InColumnGroup;
                }
                COL => {
                    self.clear_to_table_context();
                    self.insert_html(&Self::tag(COLGROUP));
                    self.reprocess_in(Mode::InColumnGroup, Tok::Start(st), tz);
                }
                TBODY | TFOOT | THEAD => {
                    self.clear_to_table_context();
                    self.insert_html(&st);
                    self.mode = Mode::InTableBody;
                }
                TD | TH | TR => {
                    self.clear_to_table_context();
                    self.insert_html(&Self::tag(TBODY));
                    self.reprocess_in(Mode::InTableBody, Tok::Start(st), tz);
                }
                TABLE => {
                    if !self.in_scope(TABLE, Scope::Table) {
                        return;
                    }
                    self.pop_until(TABLE);
                    self.reset_mode();
                    self.mode_step(Tok::Start(st), tz);
                }
                STYLE | SCRIPT | TEMPLATE => self.in_head(Tok::Start(st), tz),
                INPUT
                    if st
                        .attr("type")
                        .is_some_and(|t| t.eq_ignore_ascii_case("hidden")) =>
                {
                    self.insert_void(&st);
                }
                FORM => {
                    if self.open_has(TEMPLATE) || self.form != NONE {
                        return;
                    }
                    self.form = self.insert_void(&st);
                }
                _ => self.in_table_anything_else(Tok::Start(st), tz),
            },
            Tok::End(n) => match n {
                TABLE => {
                    if !self.in_scope(TABLE, Scope::Table) {
                        return;
                    }
                    self.pop_until(TABLE);
                    self.reset_mode();
                }
                BODY | CAPTION | COL | COLGROUP | HTML | TBODY | TD | TFOOT | TH | THEAD | TR => {}
                TEMPLATE => self.in_head(Tok::End(n), tz),
                _ => self.in_table_anything_else(Tok::End(n), tz),
            },
            Tok::Eof => self.in_body(Tok::Eof, tz),
            t => self.in_table_anything_else(t, tz),
        }
    }

    fn in_table_anything_else(&mut self, t: Tok, tz: &mut Tokenizer) {
        self.foster = true;
        self.in_body(t, tz);
        self.foster = false;
    }

    fn clear_to_table_context(&mut self) {
        use tag::*;
        while !self.is_one(self.current(), &[TABLE, TEMPLATE, HTML]) && self.current() != NONE {
            self.pop();
        }
    }

    fn clear_to_table_body_context(&mut self) {
        use tag::*;
        while !self.is_one(self.current(), &[TBODY, TFOOT, THEAD, TEMPLATE, HTML])
            && self.current() != NONE
        {
            self.pop();
        }
    }

    fn clear_to_table_row_context(&mut self) {
        use tag::*;
        while !self.is_one(self.current(), &[TR, TEMPLATE, HTML]) && self.current() != NONE {
            self.pop();
        }
    }

    // §13.2.6.4.10
    fn in_table_text(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Chars(s) => {
                let s = s.replace('\0', "");
                if !all_ws(&s) {
                    self.pending_table_non_ws = true;
                }
                self.pending_table_text.push_str(&s);
            }
            t => {
                let text = core::mem::take(&mut self.pending_table_text);
                if self.pending_table_non_ws {
                    // Foster-parent it, as "anything else" in table.
                    self.foster = true;
                    self.reconstruct_afe();
                    self.frameset_ok = false;
                    self.insert_chars(&text);
                    self.foster = false;
                } else {
                    self.insert_chars(&text);
                }
                self.mode = self.original_mode;
                self.mode_step(t, tz);
            }
        }
    }

    // §13.2.6.4.11
    fn in_caption(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        let table_start = matches!(&t, Tok::Start(st) if matches!(st.name, CAPTION | COL | COLGROUP | TBODY | TD | TFOOT | TH | THEAD | TR));
        match t {
            Tok::End(CAPTION) => {
                self.close_caption();
            }
            Tok::End(TABLE) => {
                if self.close_caption() {
                    self.in_table(Tok::End(TABLE), tz);
                }
            }
            t if table_start => {
                if self.close_caption() {
                    self.in_table(t, tz);
                }
            }
            Tok::End(n)
                if matches!(
                    n,
                    BODY | COL | COLGROUP | HTML | TBODY | TD | TFOOT | TH | THEAD | TR
                ) => {}
            t => self.in_body(t, tz),
        }
    }

    fn close_caption(&mut self) -> bool {
        if !self.in_scope(tag::CAPTION, Scope::Table) {
            return false;
        }
        self.implied_end_tags(tag::EMPTY);
        self.pop_until(tag::CAPTION);
        self.clear_afe_to_marker();
        self.mode = Mode::InTable;
        true
    }

    // §13.2.6.4.12
    fn in_column_group(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Chars(s) => {
                let (ws, rest) = split_ws(&s);
                self.insert_chars(ws);
                if !rest.is_empty() {
                    self.column_group_else(Tok::Chars(String::from(rest)), tz);
                }
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) if st.name == HTML => self.in_body(Tok::Start(st), tz),
            Tok::Start(st) if st.name == COL => {
                self.insert_void(&st);
            }
            Tok::Start(st) if st.name == TEMPLATE => self.in_head(Tok::Start(st), tz),
            Tok::End(COLGROUP) => {
                if self.is(self.current(), COLGROUP) {
                    self.pop();
                    self.mode = Mode::InTable;
                }
            }
            Tok::End(COL) => {}
            Tok::End(TEMPLATE) => self.in_head(Tok::End(TEMPLATE), tz),
            Tok::Eof => self.in_body(Tok::Eof, tz),
            t => self.column_group_else(t, tz),
        }
    }

    fn column_group_else(&mut self, t: Tok, tz: &mut Tokenizer) {
        if !self.is(self.current(), tag::COLGROUP) {
            return;
        }
        self.pop();
        self.reprocess_in(Mode::InTable, t, tz);
    }

    // §13.2.6.4.13
    fn in_table_body(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Start(st) if st.name == TR => {
                self.clear_to_table_body_context();
                self.insert_html(&st);
                self.mode = Mode::InRow;
            }
            Tok::Start(st) if matches!(st.name, TH | TD) => {
                self.clear_to_table_body_context();
                self.insert_html(&Self::tag(TR));
                self.reprocess_in(Mode::InRow, Tok::Start(st), tz);
            }
            Tok::End(n) if matches!(n, TBODY | TFOOT | THEAD) => {
                if !self.in_scope(n, Scope::Table) {
                    return;
                }
                self.clear_to_table_body_context();
                self.pop();
                self.mode = Mode::InTable;
            }
            Tok::Start(st)
                if matches!(st.name, CAPTION | COL | COLGROUP | TBODY | TFOOT | THEAD) =>
            {
                if self.end_table_section() {
                    self.in_table(Tok::Start(st), tz);
                }
            }
            Tok::End(TABLE) => {
                if self.end_table_section() {
                    self.in_table(Tok::End(TABLE), tz);
                }
            }
            Tok::End(n) if matches!(n, BODY | CAPTION | COL | COLGROUP | HTML | TD | TH | TR) => {}
            t => self.in_table(t, tz),
        }
    }

    fn end_table_section(&mut self) -> bool {
        use tag::*;
        if !self.in_scope_any(&[TBODY, THEAD, TFOOT], Scope::Table) {
            return false;
        }
        self.clear_to_table_body_context();
        self.pop();
        self.mode = Mode::InTable;
        true
    }

    // §13.2.6.4.14
    fn in_row(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Start(st) if matches!(st.name, TH | TD) => {
                self.clear_to_table_row_context();
                self.insert_html(&st);
                self.mode = Mode::InCell;
                self.afe.push(Afe::Marker);
            }
            Tok::End(TR) => {
                self.end_row();
            }
            Tok::Start(st)
                if matches!(
                    st.name,
                    CAPTION | COL | COLGROUP | TBODY | TFOOT | THEAD | TR
                ) =>
            {
                if self.end_row() {
                    self.in_table_body(Tok::Start(st), tz);
                }
            }
            Tok::End(TABLE) => {
                if self.end_row() {
                    self.in_table_body(Tok::End(TABLE), tz);
                }
            }
            Tok::End(n) if matches!(n, TBODY | TFOOT | THEAD) => {
                if !self.in_scope(n, Scope::Table) {
                    return;
                }
                if self.end_row() {
                    self.in_table_body(Tok::End(n), tz);
                }
            }
            Tok::End(n) if matches!(n, BODY | CAPTION | COL | COLGROUP | HTML | TD | TH) => {}
            t => self.in_table(t, tz),
        }
    }

    fn end_row(&mut self) -> bool {
        if !self.in_scope(tag::TR, Scope::Table) {
            return false;
        }
        self.clear_to_table_row_context();
        self.pop();
        self.mode = Mode::InTableBody;
        true
    }

    // §13.2.6.4.15
    fn in_cell(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::End(n) if matches!(n, TD | TH) => {
                if !self.in_scope(n, Scope::Table) {
                    return;
                }
                self.implied_end_tags(EMPTY);
                self.pop_until(n);
                self.clear_afe_to_marker();
                self.mode = Mode::InRow;
            }
            Tok::Start(st)
                if matches!(
                    st.name,
                    CAPTION | COL | COLGROUP | TBODY | TD | TFOOT | TH | THEAD | TR
                ) =>
            {
                if !self.in_scope_any(&[TD, TH], Scope::Table) {
                    return;
                }
                self.close_cell();
                self.mode_step(Tok::Start(st), tz);
            }
            Tok::End(n) if matches!(n, BODY | CAPTION | COL | COLGROUP | HTML) => {}
            Tok::End(n) if matches!(n, TABLE | TBODY | TFOOT | THEAD | TR) => {
                if !self.in_scope(n, Scope::Table) {
                    return;
                }
                self.close_cell();
                self.mode_step(Tok::End(n), tz);
            }
            t => self.in_body(t, tz),
        }
    }

    fn close_cell(&mut self) {
        use tag::*;
        self.implied_end_tags(EMPTY);
        self.pop_until_any(&[TD, TH]);
        self.clear_afe_to_marker();
        self.mode = Mode::InRow;
    }

    // §13.2.6.4.16
    fn in_select(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Chars(s) => {
                let s = s.replace('\0', "");
                self.insert_chars(&s);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Doctype { .. } => {}
            Tok::Start(st) => match st.name {
                HTML => self.in_body(Tok::Start(st), tz),
                OPTION => {
                    if self.is(self.current(), OPTION) {
                        self.pop();
                    }
                    self.insert_html(&st);
                }
                OPTGROUP => {
                    if self.is(self.current(), OPTION) {
                        self.pop();
                    }
                    if self.is(self.current(), OPTGROUP) {
                        self.pop();
                    }
                    self.insert_html(&st);
                }
                HR => {
                    if self.is(self.current(), OPTION) {
                        self.pop();
                    }
                    if self.is(self.current(), OPTGROUP) {
                        self.pop();
                    }
                    self.insert_void(&st);
                }
                SELECT => {
                    if self.in_scope(SELECT, Scope::Select) {
                        self.pop_until(SELECT);
                        self.reset_mode();
                    }
                }
                INPUT | KEYGEN | TEXTAREA => {
                    if !self.in_scope(SELECT, Scope::Select) {
                        return;
                    }
                    self.pop_until(SELECT);
                    self.reset_mode();
                    self.mode_step(Tok::Start(st), tz);
                }
                SCRIPT | TEMPLATE => self.in_head(Tok::Start(st), tz),
                _ => {}
            },
            Tok::End(n) => match n {
                OPTGROUP => {
                    let c = self.current();
                    if self.is(c, OPTION)
                        && self.open.len() >= 2
                        && self.is(self.open[self.open.len() - 2], OPTGROUP)
                    {
                        self.pop();
                    }
                    if self.is(self.current(), OPTGROUP) {
                        self.pop();
                    }
                }
                OPTION => {
                    if self.is(self.current(), OPTION) {
                        self.pop();
                    }
                }
                SELECT => {
                    if self.in_scope(SELECT, Scope::Select) {
                        self.pop_until(SELECT);
                        self.reset_mode();
                    }
                }
                TEMPLATE => self.in_head(Tok::End(n), tz),
                _ => {}
            },
            Tok::Eof => self.in_body(Tok::Eof, tz),
        }
    }

    // §13.2.6.4.17
    fn in_select_in_table(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        let tablish = [CAPTION, TABLE, TBODY, TFOOT, THEAD, TR, TD, TH];
        match t {
            Tok::Start(st) if tablish.contains(&st.name) => {
                self.pop_until(SELECT);
                self.reset_mode();
                self.mode_step(Tok::Start(st), tz);
            }
            Tok::End(n) if tablish.contains(&n) => {
                if !self.in_scope(n, Scope::Table) {
                    return;
                }
                self.pop_until(SELECT);
                self.reset_mode();
                self.mode_step(Tok::End(n), tz);
            }
            t => self.in_select(t, tz),
        }
    }

    // §13.2.6.4.19
    fn after_body(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Chars(s) if all_ws(&s) => self.in_body(Tok::Chars(s), tz),
            Tok::Comment(s) => {
                let html = self.open.first().copied().unwrap_or(0);
                self.comment_to(html, s);
            }
            Tok::Doctype { .. } => {}
            Tok::Start(st) if st.name == tag::HTML => self.in_body(Tok::Start(st), tz),
            Tok::End(tag::HTML) => self.mode = Mode::AfterAfterBody,
            Tok::Eof => {
                self.open.clear();
                self.stopped = true;
            }
            t => self.reprocess_in(Mode::InBody, t, tz),
        }
    }

    // §13.2.6.4.20
    fn in_frameset(&mut self, t: Tok, tz: &mut Tokenizer) {
        use tag::*;
        match t {
            Tok::Chars(s) => {
                let ws: String = s.chars().filter(|&c| is_ws(c)).collect();
                self.insert_chars(&ws);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Start(st) if st.name == HTML => self.in_body(Tok::Start(st), tz),
            Tok::Start(st) if st.name == FRAMESET => {
                self.insert_html(&st);
            }
            Tok::End(FRAMESET) => {
                if self.open.len() > 1 {
                    self.pop();
                }
                if !self.is(self.current(), FRAMESET) {
                    self.mode = Mode::AfterFrameset;
                }
            }
            Tok::Start(st) if st.name == FRAME => {
                self.insert_void(&st);
            }
            Tok::Start(st) if st.name == NOFRAMES => self.in_head(Tok::Start(st), tz),
            Tok::Eof => {
                self.open.clear();
                self.stopped = true;
            }
            _ => {}
        }
    }

    // §13.2.6.4.21
    fn after_frameset(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Chars(s) => {
                let ws: String = s.chars().filter(|&c| is_ws(c)).collect();
                self.insert_chars(&ws);
            }
            Tok::Comment(s) => self.insert_comment(s),
            Tok::Start(st) if st.name == tag::HTML => self.in_body(Tok::Start(st), tz),
            Tok::End(tag::HTML) => self.mode = Mode::AfterAfterFrameset,
            Tok::Start(st) if st.name == tag::NOFRAMES => self.in_head(Tok::Start(st), tz),
            Tok::Eof => {
                self.open.clear();
                self.stopped = true;
            }
            _ => {}
        }
    }

    // §13.2.6.4.22
    fn after_after_body(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Comment(s) => self.comment_to(0, s),
            Tok::Doctype { .. } => self.in_body(t, tz),
            Tok::Chars(s) if all_ws(&s) => self.in_body(Tok::Chars(s), tz),
            Tok::Start(st) if st.name == tag::HTML => self.in_body(Tok::Start(st), tz),
            Tok::Eof => {
                self.open.clear();
                self.stopped = true;
            }
            t => self.reprocess_in(Mode::InBody, t, tz),
        }
    }

    // §13.2.6.4.23
    fn after_after_frameset(&mut self, t: Tok, tz: &mut Tokenizer) {
        match t {
            Tok::Comment(s) => self.comment_to(0, s),
            Tok::Doctype { .. } => {}
            Tok::Chars(s) if all_ws(&s) => self.in_body(Tok::Chars(s), tz),
            Tok::Start(st) if st.name == tag::HTML => self.in_body(Tok::Start(st), tz),
            Tok::Start(st) if st.name == tag::NOFRAMES => self.in_head(Tok::Start(st), tz),
            Tok::Eof => {
                self.open.clear();
                self.stopped = true;
            }
            _ => {}
        }
    }
}

fn same_attrs(a: &[TagAttr], b: &[TagAttr]) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|x| b.iter().any(|y| y.name == x.name && y.value == x.value))
}

/// §13.2.6.4.1: quirks mode from the DOCTYPE token.
fn quirks_from_doctype(
    name: Option<&str>,
    public: Option<&str>,
    system: Option<&str>,
    force: bool,
) -> QuirksMode {
    if force || name != Some("html") {
        return QuirksMode::Quirks;
    }
    let p = public.map(|s| s.to_ascii_lowercase());
    let s = system.map(|s| s.to_ascii_lowercase());
    if let Some(p) = &p {
        const EXACT: [&str; 3] = [
            "-//w3o//dtd w3 html strict 3.0//en//",
            "-/w3c/dtd html 4.0 transitional/en",
            "html",
        ];
        const PREFIXES: [&str; 55] = [
            "+//silmaril//dtd html pro v0r11 19970101//",
            "-//as//dtd html 3.0 aswedit + extensions//",
            "-//advasoft ltd//dtd html 3.0 aswedit + extensions//",
            "-//ietf//dtd html 2.0 level 1//",
            "-//ietf//dtd html 2.0 level 2//",
            "-//ietf//dtd html 2.0 strict level 1//",
            "-//ietf//dtd html 2.0 strict level 2//",
            "-//ietf//dtd html 2.0 strict//",
            "-//ietf//dtd html 2.0//",
            "-//ietf//dtd html 2.1e//",
            "-//ietf//dtd html 3.0//",
            "-//ietf//dtd html 3.2 final//",
            "-//ietf//dtd html 3.2//",
            "-//ietf//dtd html 3//",
            "-//ietf//dtd html level 0//",
            "-//ietf//dtd html level 1//",
            "-//ietf//dtd html level 2//",
            "-//ietf//dtd html level 3//",
            "-//ietf//dtd html strict level 0//",
            "-//ietf//dtd html strict level 1//",
            "-//ietf//dtd html strict level 2//",
            "-//ietf//dtd html strict level 3//",
            "-//ietf//dtd html strict//",
            "-//ietf//dtd html//",
            "-//metrius//dtd metrius presentational//",
            "-//microsoft//dtd internet explorer 2.0 html strict//",
            "-//microsoft//dtd internet explorer 2.0 html//",
            "-//microsoft//dtd internet explorer 2.0 tables//",
            "-//microsoft//dtd internet explorer 3.0 html strict//",
            "-//microsoft//dtd internet explorer 3.0 html//",
            "-//microsoft//dtd internet explorer 3.0 tables//",
            "-//netscape comm. corp.//dtd html//",
            "-//netscape comm. corp.//dtd strict html//",
            "-//o'reilly and associates//dtd html 2.0//",
            "-//o'reilly and associates//dtd html extended 1.0//",
            "-//o'reilly and associates//dtd html extended relaxed 1.0//",
            "-//sq//dtd html 2.0 hotmetal + extensions//",
            "-//softquad software//dtd hotmetal pro 6.0::19990601::extensions to html 4.0//",
            "-//softquad//dtd hotmetal pro 4.0::19971010::extensions to html 4.0//",
            "-//spyglass//dtd html 2.0 extended//",
            "-//sun microsystems corp.//dtd hotjava html//",
            "-//sun microsystems corp.//dtd hotjava strict html//",
            "-//w3c//dtd html 3 1995-03-24//",
            "-//w3c//dtd html 3.2 draft//",
            "-//w3c//dtd html 3.2 final//",
            "-//w3c//dtd html 3.2//",
            "-//w3c//dtd html 3.2s draft//",
            "-//w3c//dtd html 4.0 frameset//",
            "-//w3c//dtd html 4.0 transitional//",
            "-//w3c//dtd html experimental 19960712//",
            "-//w3c//dtd html experimental 970421//",
            "-//w3c//dtd w3 html//",
            "-//w3o//dtd w3 html 3.0//",
            "-//webtechs//dtd mozilla html 2.0//",
            "-//webtechs//dtd mozilla html//",
        ];
        if EXACT.contains(&p.as_str()) || PREFIXES.iter().any(|x| p.starts_with(x)) {
            return QuirksMode::Quirks;
        }
        let html401 = p.starts_with("-//w3c//dtd html 4.01 frameset//")
            || p.starts_with("-//w3c//dtd html 4.01 transitional//");
        if s.is_none() && html401 {
            return QuirksMode::Quirks;
        }
        if p.starts_with("-//w3c//dtd xhtml 1.0 frameset//")
            || p.starts_with("-//w3c//dtd xhtml 1.0 transitional//")
        {
            return QuirksMode::LimitedQuirks;
        }
        if s.is_some() && html401 {
            return QuirksMode::LimitedQuirks;
        }
    }
    if s.as_deref() == Some("http://www.ibm.com/data/dtd/v11/ibmxhtml1-transitional.dtd") {
        return QuirksMode::Quirks;
    }
    QuirksMode::NoQuirks
}
