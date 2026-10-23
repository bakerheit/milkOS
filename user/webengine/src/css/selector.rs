//! Selectors: parsing (Selectors Level 3, plus the level 4 `:is()`, `:where()` and
//! selector-list `:not()`), specificity (§16 of Selectors 4) and right-to-left matching against
//! the DOM.
//!
//! Unknown pseudo-classes or pseudo-elements make the selector invalid, which drops the
//! whole rule (Selectors 4 §3.1: an invalid selector list matches nothing). Dynamic states a
//! static renderer never has (`:hover`, `:focus`, `:active`, `:visited`, `:target`) parse but
//! never match.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use super::tokenizer::Tok;
use crate::atoms::{Atom, Interner, tag};
use crate::dom::{Dom, Element, NONE, NodeData, NodeId, Ns};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    NextSibling,
    SubsequentSibling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttrOp {
    Exists,
    Equals,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    /// ::first-line, ::first-letter, ::marker, ::placeholder, ::selection...: parsed, never
    /// applied.
    Unsupported,
}

#[derive(Clone, Debug)]
pub enum Simple {
    Id(Atom),
    Class(Atom),
    Attr {
        name: String,
        op: AttrOp,
        value: String,
        ci: bool,
    },
    /// `:nth-child(an+b)` and friends: (a, b, of_type, from_end)
    Nth {
        a: i32,
        b: i32,
        of_type: bool,
        last: bool,
    },
    Root,
    Empty,
    Link,
    /// never matches (hover, focus, visited, ...)
    Never,
    Checked,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Lang(String),
    /// `:is()` / `:where()` (specificity 0 for where, computed by the parser) / `:not()`
    Is(Box<[Selector]>),
    Not(Box<[Selector]>),
}

#[derive(Clone, Debug, Default)]
pub struct Compound {
    /// None = any element (universal or no type selector).
    pub tag: Option<Atom>,
    pub simple: Vec<Simple>,
}

#[derive(Clone, Debug)]
pub struct Selector {
    /// Rightmost compound first.
    pub parts: Vec<Compound>,
    /// `combinators[i]` is between `parts[i]` and `parts[i + 1]` (to its left).
    pub combinators: Vec<Combinator>,
    /// a << 20 | b << 10 | c, each saturating at 1023.
    pub specificity: u32,
    pub pseudo: Option<PseudoElement>,
}

fn spec(a: u32, b: u32, c: u32) -> u32 {
    (a.min(1023) << 20) | (b.min(1023) << 10) | c.min(1023)
}

fn spec_add(x: u32, y: u32) -> u32 {
    let a = ((x >> 20) & 1023) + ((y >> 20) & 1023);
    let b = ((x >> 10) & 1023) + ((y >> 10) & 1023);
    let c = (x & 1023) + (y & 1023);
    spec(a, b, c)
}

struct P<'a> {
    t: &'a [Tok],
    i: usize,
    atoms: &'a mut Interner,
    depth: u32,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<&'a Tok> {
        self.t.get(self.i)
    }
    fn ws(&mut self) -> bool {
        let mut any = false;
        while matches!(self.peek(), Some(Tok::Whitespace)) {
            self.i += 1;
            any = true;
        }
        any
    }

    /// A selector list up to the end of the slice (or a ')' when nested).
    fn list(&mut self, nested: bool) -> Option<Vec<Selector>> {
        let mut out = Vec::new();
        loop {
            self.ws();
            out.push(self.complex()?);
            self.ws();
            match self.peek() {
                None => break,
                Some(Tok::Comma) => {
                    self.i += 1;
                }
                Some(Tok::Close(')')) if nested => break,
                _ => return None,
            }
        }
        Some(out)
    }

    fn complex(&mut self) -> Option<Selector> {
        let mut parts = Vec::new();
        let mut combs = Vec::new();
        let mut pseudo = None;
        loop {
            let (c, pe) = self.compound()?;
            parts.push(c);
            if pe.is_some() {
                pseudo = pe;
                // A pseudo-element must be last.
                self.ws();
                match self.peek() {
                    None | Some(Tok::Comma) | Some(Tok::Close(')')) => break,
                    _ => return None,
                }
            }
            let had_ws = self.ws();
            let comb = match self.peek() {
                Some(Tok::Delim('>')) => Combinator::Child,
                Some(Tok::Delim('+')) => Combinator::NextSibling,
                Some(Tok::Delim('~')) => Combinator::SubsequentSibling,
                None | Some(Tok::Comma) | Some(Tok::Close(')')) => break,
                _ if had_ws => {
                    combs.push(Combinator::Descendant);
                    continue;
                }
                _ => return None,
            };
            self.i += 1;
            self.ws();
            combs.push(comb);
        }
        if parts.len() > 64 {
            return None;
        }
        parts.reverse();
        combs.reverse();
        let mut s = 0;
        for p in &parts {
            s = spec_add(s, compound_spec(p));
        }
        if matches!(
            pseudo,
            Some(PseudoElement::Before | PseudoElement::After | PseudoElement::Unsupported)
        ) {
            s = spec_add(s, spec(0, 0, 1));
        }
        Some(Selector {
            parts,
            combinators: combs,
            specificity: s,
            pseudo,
        })
    }

    fn compound(&mut self) -> Option<(Compound, Option<PseudoElement>)> {
        let mut c = Compound::default();
        let mut any = false;
        // Type or universal selector (namespace prefixes: `*|x`, `|x` are accepted, ignored).
        match self.peek() {
            Some(Tok::Ident(n)) => {
                let n = n.to_ascii_lowercase();
                self.i += 1;
                c.tag = Some(self.atoms.intern(&n));
                any = true;
            }
            Some(Tok::Delim('*')) => {
                self.i += 1;
                any = true;
            }
            _ => {}
        }
        if matches!(self.peek(), Some(Tok::Delim('|'))) {
            self.i += 1;
            match self.peek() {
                Some(Tok::Ident(n)) => {
                    let n = n.to_ascii_lowercase();
                    self.i += 1;
                    c.tag = Some(self.atoms.intern(&n));
                }
                Some(Tok::Delim('*')) => {
                    self.i += 1;
                    c.tag = None;
                }
                _ => return None,
            }
            any = true;
        }
        let mut pseudo = None;
        loop {
            match self.peek() {
                Some(Tok::Hash { value, .. }) => {
                    let a = self.atoms.intern(value);
                    c.simple.push(Simple::Id(a));
                    self.i += 1;
                }
                Some(Tok::Delim('.')) => {
                    self.i += 1;
                    let Some(Tok::Ident(n)) = self.peek() else {
                        return None;
                    };
                    let a = self.atoms.intern(n);
                    c.simple.push(Simple::Class(a));
                    self.i += 1;
                }
                Some(Tok::Open('[')) => {
                    self.i += 1;
                    c.simple.push(self.attr()?);
                }
                Some(Tok::Colon) => {
                    self.i += 1;
                    if matches!(self.peek(), Some(Tok::Colon)) {
                        self.i += 1;
                        let Some(Tok::Ident(n)) = self.peek() else {
                            // ::something(...) functional pseudo-elements: unsupported.
                            if matches!(self.peek(), Some(Tok::Function(_))) {
                                self.skip_function();
                                pseudo = Some(PseudoElement::Unsupported);
                                any = true;
                                break;
                            }
                            return None;
                        };
                        let n = n.to_ascii_lowercase();
                        self.i += 1;
                        pseudo = Some(match n.as_str() {
                            "before" => PseudoElement::Before,
                            "after" => PseudoElement::After,
                            "first-line"
                            | "first-letter"
                            | "marker"
                            | "placeholder"
                            | "selection"
                            | "backdrop"
                            | "file-selector-button" => PseudoElement::Unsupported,
                            _ => return None,
                        });
                        any = true;
                        break;
                    }
                    match self.peek().cloned() {
                        Some(Tok::Ident(n)) => {
                            self.i += 1;
                            let n = n.to_ascii_lowercase();
                            match n.as_str() {
                                "before" => pseudo = Some(PseudoElement::Before),
                                "after" => pseudo = Some(PseudoElement::After),
                                "first-line" | "first-letter" => {
                                    pseudo = Some(PseudoElement::Unsupported)
                                }
                                _ => c.simple.push(pseudo_class(&n)?),
                            }
                            if pseudo.is_some() {
                                any = true;
                                break;
                            }
                        }
                        Some(Tok::Function(f)) => {
                            self.i += 1;
                            c.simple.push(self.pseudo_fn(&f)?);
                        }
                        _ => return None,
                    }
                }
                _ => break,
            }
            any = true;
        }
        if !any {
            return None;
        }
        Some((c, pseudo))
    }

    fn skip_function(&mut self) {
        self.i += 1;
        let mut d = 1;
        while let Some(t) = self.peek() {
            match t {
                Tok::Function(_) | Tok::Open(_) => d += 1,
                Tok::Close(_) => {
                    d -= 1;
                    if d == 0 {
                        self.i += 1;
                        return;
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    fn attr(&mut self) -> Option<Simple> {
        self.ws();
        // Optional namespace prefix.
        if matches!(self.peek(), Some(Tok::Delim('*')))
            && matches!(self.t.get(self.i + 1), Some(Tok::Delim('|')))
        {
            self.i += 2;
        }
        if matches!(self.peek(), Some(Tok::Delim('|'))) {
            self.i += 1;
        }
        let Some(Tok::Ident(name)) = self.peek() else {
            return None;
        };
        let name = name.to_ascii_lowercase();
        self.i += 1;
        self.ws();
        let op = match self.peek() {
            Some(Tok::Close(']')) => {
                self.i += 1;
                return Some(Simple::Attr {
                    name,
                    op: AttrOp::Exists,
                    value: String::new(),
                    ci: false,
                });
            }
            Some(Tok::Delim('=')) => {
                self.i += 1;
                AttrOp::Equals
            }
            Some(Tok::Delim(d)) => {
                let op = match d {
                    '~' => AttrOp::Includes,
                    '|' => AttrOp::DashMatch,
                    '^' => AttrOp::Prefix,
                    '$' => AttrOp::Suffix,
                    '*' => AttrOp::Substring,
                    _ => return None,
                };
                self.i += 1;
                if !matches!(self.peek(), Some(Tok::Delim('='))) {
                    return None;
                }
                self.i += 1;
                op
            }
            _ => return None,
        };
        self.ws();
        let value = match self.peek() {
            Some(Tok::Ident(v)) | Some(Tok::Str(v)) => v.clone(),
            Some(Tok::Number { value, .. }) => alloc::format!("{value}"),
            _ => return None,
        };
        self.i += 1;
        self.ws();
        let mut ci = false;
        if let Some(Tok::Ident(f)) = self.peek() {
            ci = f.eq_ignore_ascii_case("i");
            self.i += 1;
            self.ws();
        }
        if !matches!(self.peek(), Some(Tok::Close(']'))) {
            return None;
        }
        self.i += 1;
        Some(Simple::Attr {
            name,
            op,
            value,
            ci,
        })
    }

    fn pseudo_fn(&mut self, f: &str) -> Option<Simple> {
        self.depth += 1;
        if self.depth > 16 {
            return None;
        }
        let r = match f {
            "not" | "is" | "where" | "matches" | "-webkit-any" | "any" => {
                let list = self.list(true)?;
                if !matches!(self.peek(), Some(Tok::Close(')'))) {
                    return None;
                }
                self.i += 1;
                if list.iter().any(|s| s.pseudo.is_some()) {
                    return None;
                }
                let mut list = list;
                if f == "where" {
                    for s in &mut list {
                        s.specificity = 0;
                    }
                }
                Some(if f == "not" {
                    Simple::Not(list.into_boxed_slice())
                } else {
                    Simple::Is(list.into_boxed_slice())
                })
            }
            "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                self.ws();
                let (a, b) = self.an_plus_b()?;
                self.ws();
                if !matches!(self.peek(), Some(Tok::Close(')'))) {
                    return None;
                }
                self.i += 1;
                Some(Simple::Nth {
                    a,
                    b,
                    of_type: f.ends_with("of-type"),
                    last: f.contains("last"),
                })
            }
            "lang" => {
                self.ws();
                let v = match self.peek() {
                    Some(Tok::Ident(v)) | Some(Tok::Str(v)) => v.to_ascii_lowercase(),
                    _ => return None,
                };
                self.i += 1;
                self.ws();
                if !matches!(self.peek(), Some(Tok::Close(')'))) {
                    return None;
                }
                self.i += 1;
                Some(Simple::Lang(v))
            }
            "dir" | "host" | "host-context" | "state" => {
                self.i -= 1;
                self.skip_function();
                Some(Simple::Never)
            }
            _ => None,
        };
        self.depth -= 1;
        r
    }

    /// CSS Syntax §6 "The An+B microsyntax", from tokens.
    fn an_plus_b(&mut self) -> Option<(i32, i32)> {
        let clampi = |v: f32| v.clamp(-1.0e6, 1.0e6) as i32;
        let t = self.peek()?.clone();
        self.i += 1;
        let (a, rest_b): (i32, Option<String>) = match t {
            Tok::Ident(s) if s.eq_ignore_ascii_case("odd") => return Some((2, 1)),
            Tok::Ident(s) if s.eq_ignore_ascii_case("even") => return Some((2, 0)),
            Tok::Number { value, int: true } => return Some((0, clampi(value))),
            Tok::Dimension { value, unit } => {
                // "2n", "2n-1", "2n-", ...
                let u = unit.to_ascii_lowercase();
                let rest = u.strip_prefix('n')?;
                (clampi(value), Some(String::from(rest)))
            }
            Tok::Ident(s) => {
                let s = s.to_ascii_lowercase();
                if let Some(r) = s.strip_prefix("-n") {
                    (-1, Some(String::from(r)))
                } else {
                    let r = s.strip_prefix('n')?;
                    (1, Some(String::from(r)))
                }
            }
            Tok::Delim('+') => {
                let Some(Tok::Ident(s)) = self.peek().cloned() else {
                    return None;
                };
                self.i += 1;
                let s = s.to_ascii_lowercase();
                let r = s.strip_prefix('n')?;
                (1, Some(String::from(r)))
            }
            _ => return None,
        };
        let rest = rest_b.unwrap_or_default();
        if !rest.is_empty() {
            // "-1" glued to the n ("n-1"), or a lone "-" with the number next.
            if rest == "-" {
                self.ws();
                if let Some(Tok::Number { value, int: true }) = self.peek().cloned() {
                    self.i += 1;
                    return Some((a, -clampi(value.abs())));
                }
                return None;
            }
            let v: i32 = rest.parse().ok()?;
            return Some((a, v));
        }
        // "an + b" / "an - b" with whitespace.
        let save = self.i;
        self.ws();
        match self.peek().cloned() {
            Some(Tok::Delim(s @ ('+' | '-'))) => {
                self.i += 1;
                self.ws();
                if let Some(Tok::Number { value, int: true }) = self.peek().cloned() {
                    self.i += 1;
                    let v = clampi(value.abs());
                    return Some((a, if s == '-' { -v } else { v }));
                }
                None
            }
            Some(Tok::Number { value, int: true }) if self.t.get(self.i).is_some() => {
                // "+5" / "-5" tokenized as signed numbers.
                self.i += 1;
                Some((a, clampi(value)))
            }
            _ => {
                self.i = save;
                Some((a, 0))
            }
        }
    }
}

fn pseudo_class(n: &str) -> Option<Simple> {
    Some(match n {
        "root" => Simple::Root,
        "empty" => Simple::Empty,
        "first-child" => Simple::Nth {
            a: 0,
            b: 1,
            of_type: false,
            last: false,
        },
        "last-child" => Simple::Nth {
            a: 0,
            b: 1,
            of_type: false,
            last: true,
        },
        "only-child" => Simple::Is(
            alloc::vec![Selector {
                parts: alloc::vec![Compound {
                    tag: None,
                    simple: alloc::vec![
                        Simple::Nth {
                            a: 0,
                            b: 1,
                            of_type: false,
                            last: false
                        },
                        Simple::Nth {
                            a: 0,
                            b: 1,
                            of_type: false,
                            last: true
                        },
                    ],
                }],
                combinators: Vec::new(),
                specificity: spec(0, 1, 0),
                pseudo: None,
            }]
            .into_boxed_slice(),
        ),
        "first-of-type" => Simple::Nth {
            a: 0,
            b: 1,
            of_type: true,
            last: false,
        },
        "last-of-type" => Simple::Nth {
            a: 0,
            b: 1,
            of_type: true,
            last: true,
        },
        "only-of-type" => Simple::Is(
            alloc::vec![Selector {
                parts: alloc::vec![Compound {
                    tag: None,
                    simple: alloc::vec![
                        Simple::Nth {
                            a: 0,
                            b: 1,
                            of_type: true,
                            last: false
                        },
                        Simple::Nth {
                            a: 0,
                            b: 1,
                            of_type: true,
                            last: true
                        },
                    ],
                }],
                combinators: Vec::new(),
                specificity: spec(0, 1, 0),
                pseudo: None,
            }]
            .into_boxed_slice(),
        ),
        "link" | "any-link" | "-webkit-any-link" => Simple::Link,
        "visited" | "hover" | "active" | "focus" | "focus-within" | "focus-visible" | "target"
        | "indeterminate" | "invalid" | "user-invalid" | "autofill" | "-webkit-autofill"
        | "fullscreen" | "modal" | "open" | "popover-open" | "playing" | "paused" | "current"
        | "past" | "future" | "target-within" => Simple::Never,
        "valid" | "defined" | "scope" | "in-range" => Simple::Is(Box::new([])), // handled as "always" below
        "checked" => Simple::Checked,
        "disabled" => Simple::Disabled,
        "enabled" => Simple::Enabled,
        "required" => Simple::Required,
        "optional" => Simple::Optional,
        "read-only" => Simple::ReadOnly,
        "read-write" => Simple::ReadWrite,
        "placeholder-shown" => Simple::PlaceholderShown,
        _ => return None,
    })
}

fn compound_spec(c: &Compound) -> u32 {
    let mut s = if c.tag.is_some() { spec(0, 0, 1) } else { 0 };
    for x in &c.simple {
        s = spec_add(
            s,
            match x {
                Simple::Id(_) => spec(1, 0, 0),
                Simple::Is(l) | Simple::Not(l) => {
                    l.iter().map(|s| s.specificity).max().unwrap_or(0)
                }
                _ => spec(0, 1, 0),
            },
        );
    }
    s
}

/// Parse a selector list from a rule prelude. None if any selector in it is invalid.
pub fn parse_list(toks: &[Tok], atoms: &mut Interner) -> Option<Vec<Selector>> {
    let mut p = P {
        t: toks,
        i: 0,
        atoms,
        depth: 0,
    };
    let l = p.list(false)?;
    if p.i < toks.len() {
        return None;
    }
    Some(l)
}

// ---- matching -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum R {
    Matched,
    RestartFromSibling,
    RestartFromDescendant,
    Global,
}

/// What matching needs besides the DOM: per element, its position among its element siblings
/// (1-based) and how many element siblings it has, precomputed once per style pass.
pub struct MatchCx<'a> {
    pub dom: &'a Dom,
    pub index: &'a [u32],
    pub count: &'a [u32],
}

impl MatchCx<'_> {
    pub fn matches(&self, sel: &Selector, el: NodeId) -> bool {
        self.match_from(sel, 0, el, 0) == R::Matched
    }

    /// Right-to-left matching with the failure states that keep it linear (the approach of
    /// the Servo/Gecko selector matcher): a failed compound tells the combinators further
    /// right how far back to restart, so `a b c d` on a deep tree doesn't backtrack
    /// exponentially.
    fn match_from(&self, sel: &Selector, part: usize, el: NodeId, depth: u32) -> R {
        if depth > 32 || !self.compound(&sel.parts[part], el, depth) {
            return R::RestartFromSibling;
        }
        if part + 1 == sel.parts.len() {
            return R::Matched;
        }
        let comb = sel.combinators[part];
        let not_found = match comb {
            Combinator::NextSibling | Combinator::SubsequentSibling => R::RestartFromDescendant,
            _ => R::Global,
        };
        let mut cand = el;
        loop {
            cand = match comb {
                Combinator::Child | Combinator::Descendant => self.parent_el(cand),
                _ => self.prev_el(cand),
            };
            if cand == NONE {
                return not_found;
            }
            let r = self.match_from(sel, part + 1, cand, depth);
            match (r, comb) {
                (R::Matched, _) | (R::Global, _) | (_, Combinator::NextSibling) => return r,
                (_, Combinator::Child) => return R::RestartFromDescendant,
                (R::RestartFromDescendant, Combinator::SubsequentSibling) => return r,
                _ => {}
            }
        }
    }

    fn parent_el(&self, n: NodeId) -> NodeId {
        let p = self.dom.parent(n);
        if p != NONE && self.dom.element(p).is_some() {
            p
        } else {
            NONE
        }
    }

    fn prev_el(&self, n: NodeId) -> NodeId {
        let mut s = self.dom.node(n).prev;
        while s != NONE {
            if self.dom.element(s).is_some() {
                return s;
            }
            s = self.dom.node(s).prev;
        }
        NONE
    }

    fn compound(&self, c: &Compound, el: NodeId, depth: u32) -> bool {
        let Some(e) = self.dom.element(el) else {
            return false;
        };
        if let Some(t) = c.tag
            && e.name != t
            && !(e.ns != Ns::Html
                && self
                    .dom
                    .atoms
                    .name(e.name)
                    .eq_ignore_ascii_case(self.dom.atoms.name(t)))
        {
            return false;
        }
        c.simple.iter().all(|s| self.simple(s, el, e, depth))
    }

    fn simple(&self, s: &Simple, el: NodeId, e: &Element, depth: u32) -> bool {
        use tag::*;
        match s {
            Simple::Id(a) => e.id == Some(*a),
            Simple::Class(a) => e.classes.contains(a),
            Simple::Attr {
                name,
                op,
                value,
                ci,
            } => {
                let Some(v) = e.attr(name) else { return false };
                attr_match(v, *op, value, *ci || case_insensitive_attr(name))
            }
            Simple::Nth {
                a,
                b,
                of_type,
                last,
            } => {
                let (pos, total) = if *of_type {
                    self.type_position(el, e.name)
                } else {
                    (self.index[el as usize], self.count[el as usize])
                };
                if pos == 0 {
                    return false;
                }
                let n = if *last { total + 1 - pos } else { pos } as i64;
                nth(*a as i64, *b as i64, n)
            }
            Simple::Root => self.dom.parent(el) == 0,
            Simple::Empty => self.dom.children(el).all(|c| match &self.dom.node(c).data {
                NodeData::Element(_) => false,
                NodeData::Text(t) => t.is_empty(),
                _ => true,
            }),
            Simple::Link => e.ns == Ns::Html && matches!(e.name, A | AREA) && e.has_attr("href"),
            Simple::Never => false,
            Simple::Checked => match &self.dom.checked {
                Some(set) => (e.is(INPUT) || e.is(OPTION)) && set.contains(&el),
                None => {
                    (e.is(INPUT) && e.has_attr("checked"))
                        || (e.is(OPTION) && e.has_attr("selected"))
                }
            },
            Simple::Disabled => is_form_el(e) && self.disabled(el, e),
            Simple::Enabled => is_form_el(e) && !self.disabled(el, e),
            Simple::Required => is_form_el(e) && e.has_attr("required"),
            Simple::Optional => is_form_el(e) && !e.has_attr("required"),
            Simple::ReadOnly => !read_write(e),
            Simple::ReadWrite => read_write(e),
            Simple::PlaceholderShown => {
                (e.is(INPUT) || e.is(TEXTAREA))
                    && e.has_attr("placeholder")
                    && e.attr("value").is_none_or(|v| v.is_empty())
            }
            Simple::Lang(l) => {
                let mut n = el;
                while n != NONE {
                    if let Some(x) = self.dom.element(n).and_then(|x| x.attr("lang")) {
                        let x = x.to_ascii_lowercase();
                        return x == *l
                            || (x.starts_with(l.as_str())
                                && x.as_bytes().get(l.len()) == Some(&b'-'));
                    }
                    n = self.dom.parent(n);
                }
                false
            }
            Simple::Is(list) => {
                // An empty list is the "always" placeholder (see pseudo_class).
                list.is_empty()
                    || list
                        .iter()
                        .any(|s| self.match_from(s, 0, el, depth + 1) == R::Matched)
            }
            Simple::Not(list) => !list
                .iter()
                .any(|s| self.match_from(s, 0, el, depth + 1) == R::Matched),
        }
    }

    fn disabled(&self, el: NodeId, e: &Element) -> bool {
        if e.has_attr("disabled") {
            return true;
        }
        // Inside a disabled fieldset (not in its first legend).
        let mut p = self.dom.parent(el);
        while p != NONE {
            if let Some(pe) = self.dom.element(p)
                && pe.is(tag::FIELDSET)
                && pe.has_attr("disabled")
            {
                return true;
            }
            p = self.dom.parent(p);
        }
        false
    }

    /// (position among same-type siblings, how many there are).
    fn type_position(&self, el: NodeId, name: Atom) -> (u32, u32) {
        let p = self.dom.parent(el);
        if p == NONE {
            return (1, 1);
        }
        let mut pos = 0;
        let mut total = 0;
        for c in self.dom.children(p) {
            if self.dom.element(c).is_some_and(|x| x.name == name) {
                total += 1;
                if c == el {
                    pos = total;
                }
            }
        }
        (pos, total)
    }
}

fn is_form_el(e: &Element) -> bool {
    use tag::*;
    e.ns == Ns::Html
        && matches!(
            e.name,
            INPUT | BUTTON | SELECT | TEXTAREA | OPTION | OPTGROUP | FIELDSET
        )
}

fn read_write(e: &Element) -> bool {
    use tag::*;
    if e.is(TEXTAREA) {
        return !e.has_attr("readonly") && !e.has_attr("disabled");
    }
    if e.is(INPUT) {
        let t = e.attr("type").unwrap_or("text").to_ascii_lowercase();
        let texty = matches!(
            t.as_str(),
            "text"
                | "search"
                | "url"
                | "tel"
                | "email"
                | "password"
                | "number"
                | "date"
                | "month"
                | "week"
                | "time"
                | "datetime-local"
        );
        return texty && !e.has_attr("readonly") && !e.has_attr("disabled");
    }
    e.has_attr("contenteditable")
}

/// HTML attributes whose values selectors compare case-insensitively (HTML §4.16.2's list,
/// the common ones).
fn case_insensitive_attr(n: &str) -> bool {
    matches!(
        n,
        "type"
            | "lang"
            | "dir"
            | "align"
            | "valign"
            | "method"
            | "rel"
            | "media"
            | "charset"
            | "target"
            | "scope"
            | "shape"
            | "nowrap"
            | "checked"
            | "disabled"
            | "selected"
            | "readonly"
            | "multiple"
    )
}

fn attr_match(v: &str, op: AttrOp, want: &str, ci: bool) -> bool {
    let eq = |a: &str, b: &str| {
        if ci {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    match op {
        AttrOp::Exists => true,
        AttrOp::Equals => eq(v, want),
        AttrOp::Includes => {
            !want.is_empty() && v.split(crate::dom::is_html_space).any(|w| eq(w, want))
        }
        AttrOp::DashMatch => {
            eq(v, want)
                || (v.len() > want.len()
                    && eq(&v[..want.len()], want)
                    && v.as_bytes()[want.len()] == b'-')
        }
        AttrOp::Prefix => {
            !want.is_empty()
                && v.len() >= want.len()
                && v.is_char_boundary(want.len())
                && eq(&v[..want.len()], want)
        }
        AttrOp::Suffix => {
            !want.is_empty()
                && v.len() >= want.len()
                && v.is_char_boundary(v.len() - want.len())
                && eq(&v[v.len() - want.len()..], want)
        }
        AttrOp::Substring => {
            !want.is_empty()
                && if ci {
                    v.to_ascii_lowercase().contains(&want.to_ascii_lowercase())
                } else {
                    v.contains(want)
                }
        }
    }
}

/// Whether n (1-based) is a + b·k for some k ≥ 0.
fn nth(a: i64, b: i64, n: i64) -> bool {
    if a == 0 {
        return n == b;
    }
    let d = n - b;
    d % a == 0 && d / a >= 0
}

/// The key a rule is bucketed under: the rightmost compound's id, else a class, else its tag.
pub enum Key {
    Id(Atom),
    Class(Atom),
    Tag(Atom),
    Universal,
}

pub fn key_of(sel: &Selector) -> Key {
    let c = &sel.parts[0];
    for s in &c.simple {
        if let Simple::Id(a) = s {
            return Key::Id(*a);
        }
    }
    for s in &c.simple {
        if let Simple::Class(a) = s {
            return Key::Class(*a);
        }
    }
    match c.tag {
        Some(t) => Key::Tag(t),
        None => Key::Universal,
    }
}
