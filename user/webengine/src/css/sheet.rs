//! Stylesheets: CSS Syntax 3 §5 "Parsing" (rules, at-rules, blocks), with the at-rules we act
//! on: `@media` (Media Queries 4, the screen features), `@supports`, `@import`, `@layer`
//! (its rules are kept, in order; layer precedence is not modelled) and `@container`
//! (dropped). `@font-face`, `@keyframes`, `@page` and unknown at-rules are skipped.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::selector::{self, Selector};
use super::tokenizer::{Tok, tokenize};
use super::values::{self, components, fn_args, trim};
use crate::atoms::Interner;
use crate::style::props::{Decl, ParseCx, parse_block, parse_declaration};

/// One style rule's declarations, shared by all its selectors.
#[derive(Debug, Default)]
pub struct DeclBlock {
    pub normal: Vec<Decl>,
    pub important: Vec<Decl>,
}

#[derive(Debug)]
pub enum Rule {
    Style {
        selectors: Vec<Selector>,
        decls: Rc<DeclBlock>,
    },
    Media {
        query: MediaList,
        rules: Vec<Rule>,
    },
    Import {
        url: String,
        media: MediaList,
    },
}

#[derive(Debug, Default)]
pub struct Sheet {
    pub rules: Vec<Rule>,
}

/// What media queries are evaluated against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaEnv {
    /// Viewport, CSS px.
    pub width: f32,
    pub height: f32,
    pub dark: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum MediaType {
    All,
    Screen,
    Print,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
enum Cond {
    Feature(String, Option<FeatureValue>, Cmp),
    /// `(400px <= width <= 700px)`
    Range(
        Option<(FeatureValue, Cmp)>,
        String,
        Option<(Cmp, FeatureValue)>,
    ),
    And(Vec<Cond>),
    Or(Vec<Cond>),
    Not(alloc::boxed::Box<Cond>),
    /// Something we don't understand: false (Media Queries 4 §3.2: "unknown" is false).
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cmp {
    Eq,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
enum FeatureValue {
    Len(values::Len),
    Num(f32),
    Ratio(f32),
    Ident(String),
}

#[derive(Clone, Debug, PartialEq)]
struct MediaQuery {
    not: bool,
    ty: MediaType,
    cond: Option<Cond>,
}

/// A comma-separated media query list. Empty matches everything.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MediaList(Vec<MediaQuery>);

impl MediaList {
    pub fn matches(&self, env: &MediaEnv) -> bool {
        self.0.is_empty() || self.0.iter().any(|q| q.matches(env))
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl MediaQuery {
    fn matches(&self, env: &MediaEnv) -> bool {
        let ty = matches!(self.ty, MediaType::All | MediaType::Screen);
        let c = self.cond.as_ref().is_none_or(|c| c.eval(env));
        (ty && c) != self.not
    }
}

impl Cond {
    fn eval(&self, env: &MediaEnv) -> bool {
        match self {
            Cond::And(v) => v.iter().all(|c| c.eval(env)),
            Cond::Or(v) => v.iter().any(|c| c.eval(env)),
            Cond::Not(c) => !c.eval(env),
            Cond::Unknown => false,
            Cond::Feature(name, val, cmp) => feature(env, name, val.as_ref(), *cmp),
            Cond::Range(lo, name, hi) => {
                // "a <= width" is "width >= a".
                let ok_lo = lo.as_ref().is_none_or(|(v, c)| {
                    let flipped = match c {
                        Cmp::Lt => Cmp::Gt,
                        Cmp::Le => Cmp::Ge,
                        Cmp::Gt => Cmp::Lt,
                        Cmp::Ge => Cmp::Le,
                        Cmp::Eq => Cmp::Eq,
                    };
                    feature(env, name, Some(v), flipped)
                });
                let ok_hi = hi
                    .as_ref()
                    .is_none_or(|(c, v)| feature(env, name, Some(v), *c));
                ok_lo && ok_hi
            }
        }
    }
}

fn len_px(v: &FeatureValue) -> Option<f32> {
    match v {
        FeatureValue::Len(l) => {
            let lp = l.compute(&values::LenCx {
                em: 16.0,
                rem: 16.0,
                vw: 0.0,
                vh: 0.0,
            });
            Some(lp.px)
        }
        FeatureValue::Num(n) if *n == 0.0 => Some(0.0),
        _ => None,
    }
}

fn cmp(a: f32, b: f32, c: Cmp) -> bool {
    match c {
        Cmp::Eq => (a - b).abs() < 0.01,
        Cmp::Lt => a < b,
        Cmp::Le => a <= b,
        Cmp::Gt => a > b,
        Cmp::Ge => a >= b,
    }
}

/// Media Queries 4 §6 features for a screen: width/height (and min-/max-), aspect-ratio,
/// orientation, resolution (1x), colour, hover/pointer (a mouse), prefers-*.
fn feature(env: &MediaEnv, name: &str, v: Option<&FeatureValue>, c: Cmp) -> bool {
    let (base, c) = if let Some(b) = name.strip_prefix("min-") {
        (b, Cmp::Ge)
    } else if let Some(b) = name.strip_prefix("max-") {
        (b, Cmp::Le)
    } else {
        (name, c)
    };
    let base = base
        .trim_start_matches("-webkit-")
        .trim_start_matches("device-");
    fn ident(v: Option<&FeatureValue>) -> Option<&str> {
        match v {
            Some(FeatureValue::Ident(s)) => Some(s.as_str()),
            _ => None,
        }
    }
    match base {
        "width" | "height" => {
            let have = if base == "width" {
                env.width
            } else {
                env.height
            };
            match v {
                None => have > 0.0,
                Some(v) => len_px(v).is_some_and(|w| cmp(have, w, c)),
            }
        }
        "aspect-ratio" => match v {
            Some(FeatureValue::Ratio(r)) | Some(FeatureValue::Num(r)) => {
                cmp(env.width / env.height.max(1.0), *r, c)
            }
            None => true,
            _ => false,
        },
        "orientation" => match ident(v) {
            Some("portrait") => env.height >= env.width,
            Some("landscape") => env.width > env.height,
            _ => false,
        },
        "pixel-ratio" | "resolution" => {
            let r = match v {
                Some(FeatureValue::Num(n)) => *n,
                Some(FeatureValue::Ident(_)) => return false,
                Some(FeatureValue::Len(_)) | Some(FeatureValue::Ratio(_)) => 1.0,
                None => return true,
            };
            cmp(1.0, r, c)
        }
        "color" => match v {
            None => true,
            Some(FeatureValue::Num(n)) => cmp(8.0, *n, c),
            _ => false,
        },
        "monochrome" => match v {
            None => false,
            Some(FeatureValue::Num(n)) => cmp(0.0, *n, c),
            _ => false,
        },
        "hover" | "any-hover" => matches!(ident(v), Some("hover") | None),
        "pointer" | "any-pointer" => matches!(ident(v), Some("fine") | None),
        "prefers-color-scheme" => match ident(v) {
            Some("dark") => env.dark,
            Some("light") => !env.dark,
            _ => false,
        },
        "prefers-reduced-motion" => matches!(ident(v), Some("reduce")),
        "prefers-contrast" | "forced-colors" | "inverted-colors" => {
            matches!(ident(v), Some("no-preference") | Some("none"))
        }
        "prefers-reduced-transparency" | "prefers-reduced-data" => {
            matches!(ident(v), Some("no-preference"))
        }
        "scripting" => matches!(ident(v), Some("none")),
        "update" => matches!(ident(v), Some("fast") | None),
        "display-mode" => matches!(ident(v), Some("browser")),
        "grid" => matches!(v, Some(FeatureValue::Num(n)) if *n == 0.0),
        _ => false,
    }
}

fn feature_value(t: &[Tok]) -> Option<FeatureValue> {
    let t = trim(t);
    let c = components(t);
    if c.len() == 3 && matches!(c[1].first(), Some(Tok::Delim('/'))) {
        let a = values::number(c[0])?;
        let b = values::number(c[2])?;
        return Some(FeatureValue::Ratio(if b == 0.0 { 0.0 } else { a / b }));
    }
    if c.len() != 1 {
        return None;
    }
    let x = c[0];
    match x.first()? {
        Tok::Ident(s) => Some(FeatureValue::Ident(s.to_ascii_lowercase())),
        Tok::Number { value, .. } => Some(FeatureValue::Num(*value)),
        Tok::Dimension { value, unit } if matches!(unit.as_str(), "dppx" | "x") => {
            Some(FeatureValue::Num(*value))
        }
        Tok::Dimension { value, unit } if unit == "dpi" => Some(FeatureValue::Num(*value / 96.0)),
        Tok::Dimension { value, unit } if unit == "dpcm" => {
            Some(FeatureValue::Num(*value * 2.54 / 96.0))
        }
        _ => values::length(x, false).map(FeatureValue::Len),
    }
}

fn cmp_at(t: &[Tok], i: usize) -> Option<(Cmp, usize)> {
    match (t.get(i), t.get(i + 1)) {
        (Some(Tok::Delim('<')), Some(Tok::Delim('='))) => Some((Cmp::Le, 2)),
        (Some(Tok::Delim('>')), Some(Tok::Delim('='))) => Some((Cmp::Ge, 2)),
        (Some(Tok::Delim('<')), _) => Some((Cmp::Lt, 1)),
        (Some(Tok::Delim('>')), _) => Some((Cmp::Gt, 1)),
        (Some(Tok::Delim('=')), _) => Some((Cmp::Eq, 1)),
        _ => None,
    }
}

/// A `( ... )` media feature: `(name)`, `(name: value)`, or the range forms.
fn parse_feature(inner: &[Tok]) -> Cond {
    let inner = trim(inner);
    // Nested condition: "((a) and (b))" or "(not (a))".
    if matches!(inner.first(), Some(Tok::Open('(')))
        || inner.first().is_some_and(|t| t.is_ident("not"))
    {
        return parse_cond(inner).unwrap_or(Cond::Unknown);
    }
    if let Some(colon) = inner.iter().position(|t| matches!(t, Tok::Colon)) {
        let Some(Tok::Ident(name)) = trim(&inner[..colon]).first() else {
            return Cond::Unknown;
        };
        if trim(&inner[..colon]).len() != 1 {
            return Cond::Unknown;
        }
        return match feature_value(&inner[colon + 1..]) {
            Some(v) => Cond::Feature(name.to_ascii_lowercase(), Some(v), Cmp::Eq),
            None => Cond::Unknown,
        };
    }
    if inner.len() == 1
        && let Tok::Ident(n) = &inner[0]
    {
        return Cond::Feature(n.to_ascii_lowercase(), None, Cmp::Eq);
    }
    // Range: find the comparison operators.
    let mut ops = Vec::new();
    let mut i = 0;
    while i < inner.len() {
        if let Some((c, n)) = cmp_at(inner, i) {
            ops.push((i, c, n));
            i += n;
        } else {
            i += 1;
        }
    }
    match ops.len() {
        1 => {
            let (p, c, n) = ops[0];
            let left = trim(&inner[..p]);
            let right = trim(&inner[p + n..]);
            if let [Tok::Ident(name)] = left {
                match feature_value(right) {
                    Some(v) => Cond::Range(None, name.to_ascii_lowercase(), Some((c, v))),
                    None => Cond::Unknown,
                }
            } else if let [Tok::Ident(name)] = right {
                match feature_value(left) {
                    Some(v) => Cond::Range(Some((v, c)), name.to_ascii_lowercase(), None),
                    None => Cond::Unknown,
                }
            } else {
                Cond::Unknown
            }
        }
        2 => {
            let (p1, c1, n1) = ops[0];
            let (p2, c2, n2) = ops[1];
            let a = feature_value(&inner[..p1]);
            let name = trim(&inner[p1 + n1..p2]);
            let b = feature_value(&inner[p2 + n2..]);
            match (a, name, b) {
                (Some(a), [Tok::Ident(n)], Some(b)) => {
                    Cond::Range(Some((a, c1)), n.to_ascii_lowercase(), Some((c2, b)))
                }
                _ => Cond::Unknown,
            }
        }
        _ => Cond::Unknown,
    }
}

/// `<media-condition>`: features joined by and / or, with not.
fn parse_cond(t: &[Tok]) -> Option<Cond> {
    let c = components(t);
    if c.is_empty() {
        return None;
    }
    if c[0].first()?.is_ident("not") {
        let rest_start = 1;
        let inner = c.get(rest_start)?;
        let f = match inner.first()? {
            Tok::Open('(') => parse_feature(fn_args(inner)),
            _ => return None,
        };
        return Some(Cond::Not(alloc::boxed::Box::new(f)));
    }
    let mut terms = Vec::new();
    let mut joiner: Option<bool> = None; // true = and
    for (i, x) in c.iter().enumerate() {
        if i % 2 == 1 {
            let and = if x.first()?.is_ident("and") {
                true
            } else if x.first()?.is_ident("or") {
                false
            } else {
                return None;
            };
            if joiner.is_some_and(|j| j != and) {
                return None;
            }
            joiner = Some(and);
            continue;
        }
        match x.first()? {
            Tok::Open('(') => terms.push(parse_feature(fn_args(x))),
            Tok::Function(_) => terms.push(Cond::Unknown),
            _ => return None,
        }
    }
    Some(match (terms.len(), joiner) {
        (1, _) => terms.pop()?,
        (_, Some(false)) => Cond::Or(terms),
        _ => Cond::And(terms),
    })
}

/// Media Queries 4 §3 `<media-query-list>`.
pub fn parse_media_list(t: &[Tok]) -> MediaList {
    let mut out = Vec::new();
    let t = trim(t);
    if t.is_empty() {
        return MediaList(out);
    }
    for q in values::split_commas(t) {
        let c = components(q);
        let mut i = 0;
        let mut not = false;
        let mut ty = MediaType::All;
        let bad = MediaQuery {
            not: true,
            ty: MediaType::All,
            cond: None,
        };
        if c.is_empty() {
            out.push(bad);
            continue;
        }
        if let [Tok::Ident(k)] = c[0] {
            let k = k.to_ascii_lowercase();
            if k == "not" || k == "only" {
                if matches!(c.get(1).and_then(|x| x.first()), Some(Tok::Ident(_))) {
                    not = k == "not";
                    i = 1;
                }
            }
            if let Some([Tok::Ident(t)]) = c.get(i).copied() {
                let t = t.to_ascii_lowercase();
                if t != "not" {
                    ty = match t.as_str() {
                        "all" => MediaType::All,
                        "screen" => MediaType::Screen,
                        "print" => MediaType::Print,
                        _ => MediaType::Other,
                    };
                    i += 1;
                    if i < c.len() {
                        if !c[i].first().is_some_and(|x| x.is_ident("and")) {
                            out.push(bad);
                            continue;
                        }
                        i += 1;
                    }
                }
            }
        }
        let cond = if i < c.len() {
            let start = c[i].as_ptr() as usize - q.as_ptr() as usize;
            match parse_cond(&q[start / core::mem::size_of::<Tok>()..]) {
                Some(c) => Some(c),
                None => {
                    out.push(bad);
                    continue;
                }
            }
        } else {
            None
        };
        out.push(MediaQuery { not, ty, cond });
    }
    MediaList(out)
}

/// CSS Conditional 3 §6.1 `@supports` conditions: `(prop: value)`, `not`, `and`, `or`,
/// `selector()`.
fn supports(t: &[Tok], cx: &ParseCx, atoms: &mut Interner) -> bool {
    let c = components(trim(t));
    if c.is_empty() {
        return false;
    }
    if c[0].first().is_some_and(|x| x.is_ident("not")) {
        return c.get(1).is_some_and(|x| !supports_in_parens(x, cx, atoms));
    }
    let mut result = supports_in_parens(c[0], cx, atoms);
    let mut i = 1;
    while i + 1 < c.len() {
        let and = c[i].first().is_some_and(|x| x.is_ident("and"));
        let v = supports_in_parens(c[i + 1], cx, atoms);
        result = if and { result && v } else { result || v };
        i += 2;
    }
    result
}

fn supports_in_parens(c: &[Tok], cx: &ParseCx, atoms: &mut Interner) -> bool {
    match c.first() {
        Some(Tok::Open('(')) => {
            let inner = trim(fn_args(c));
            if let Some(Tok::Ident(name)) = inner.first() {
                let k = values::skip_ws(inner, 1);
                if matches!(inner.get(k), Some(Tok::Colon)) {
                    let mut tmp = Vec::new();
                    return parse_declaration(name, &inner[k + 1..], cx, &mut tmp);
                }
            }
            supports(inner, cx, atoms)
        }
        Some(Tok::Function(f)) if f == "selector" => {
            selector::parse_list(trim(fn_args(c)), atoms).is_some()
        }
        _ => false,
    }
}

/// Limits for one parse.
pub struct SheetLimits {
    pub max_tokens: usize,
    pub max_rules: usize,
}

struct Parser<'a, 'c> {
    t: &'a [Tok],
    cx: &'a ParseCx<'c>,
    atoms: &'a mut Interner,
    rules_left: usize,
}

impl Parser<'_, '_> {
    /// §5.4.1 "consume a list of rules", up to `end` (exclusive).
    fn rules(&mut self, mut i: usize, end: usize, depth: u32, out: &mut Vec<Rule>) {
        while i < end {
            match &self.t[i] {
                Tok::Whitespace | Tok::Cdo | Tok::Cdc | Tok::Semicolon => i += 1,
                Tok::AtKeyword(_) => i = self.at_rule(i, end, depth, out),
                _ => i = self.qualified_rule(i, end, depth, out),
            }
        }
    }

    /// The end of a prelude: the index of the `{` (or `;`/end for at-rules).
    fn prelude_end(&self, mut i: usize, end: usize, at: bool) -> usize {
        while i < end {
            match &self.t[i] {
                Tok::Open('{') => return i,
                Tok::Semicolon if at => return i,
                _ => i = values::component_end(self.t, i).max(i + 1).min(end),
            }
        }
        end
    }

    /// The inside of the block opened at `open` and ending before `b` (without its `}` when
    /// it has one: a block cut off by the end of the sheet has none).
    fn body_range(&self, open: usize, b: usize) -> (usize, usize) {
        let e = if b > open + 1 && matches!(self.t[b - 1], Tok::Close('}')) {
            b - 1
        } else {
            b
        };
        (open + 1, e.max(open + 1))
    }

    fn block_end(&self, open: usize, end: usize) -> usize {
        values::component_end(self.t, open).min(end)
    }

    fn qualified_rule(&mut self, i: usize, end: usize, _depth: u32, out: &mut Vec<Rule>) -> usize {
        let p = self.prelude_end(i, end, false);
        if p >= end {
            return end;
        }
        let b = self.block_end(p, end);
        let (b0, b1) = self.body_range(p, b);
        let body = &self.t[b0..b1];
        if self.rules_left == 0 {
            return b;
        }
        if let Some(selectors) = selector::parse_list(trim(&self.t[i..p]), self.atoms) {
            let mut block = DeclBlock::default();
            parse_block(body, self.cx, &mut block.normal, &mut block.important);
            if !block.normal.is_empty() || !block.important.is_empty() {
                self.rules_left -= 1;
                out.push(Rule::Style {
                    selectors,
                    decls: Rc::new(block),
                });
            }
        }
        b
    }

    fn at_rule(&mut self, i: usize, end: usize, depth: u32, out: &mut Vec<Rule>) -> usize {
        let Tok::AtKeyword(name) = &self.t[i] else {
            return i + 1;
        };
        let name = name.clone();
        let p = self.prelude_end(i + 1, end, true);
        let prelude = trim(&self.t[i + 1..p]);
        let has_block = p < end && matches!(self.t[p], Tok::Open('{'));
        let next = if has_block {
            self.block_end(p, end)
        } else {
            (p + 1).min(end)
        };
        let body = if has_block {
            self.body_range(p, next)
        } else {
            (p, p)
        };
        if depth > 16 {
            return next;
        }
        match name.as_str() {
            "media" if has_block => {
                let query = parse_media_list(prelude);
                let mut rules = Vec::new();
                self.rules(body.0, body.1, depth + 1, &mut rules);
                out.push(Rule::Media { query, rules });
            }
            "supports" if has_block => {
                if supports(prelude, self.cx, self.atoms) {
                    self.rules(body.0, body.1, depth + 1, out);
                }
            }
            "layer" if has_block => self.rules(body.0, body.1, depth + 1, out),
            "document" | "-moz-document" if has_block => {}
            "import" if !has_block => {
                let c = components(prelude);
                let url = match c.first().and_then(|x| x.first()) {
                    Some(Tok::Url(u)) | Some(Tok::Str(u)) => Some(u.clone()),
                    Some(Tok::Function(f)) if f == "url" => match trim(fn_args(c[0])).first() {
                        Some(Tok::Str(u)) => Some(u.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(u) = url {
                    // Skip "layer(...)" / "supports(...)" before the media list.
                    let mut k = 1;
                    while k < c.len()
                        && (c[k].first().is_some_and(|t| t.is_ident("layer"))
                            || matches!(c[k].first(), Some(Tok::Function(f)) if f == "layer" || f == "supports"))
                    {
                        k += 1;
                    }
                    let media = if k < c.len() {
                        let start = c[k].as_ptr() as usize - prelude.as_ptr() as usize;
                        parse_media_list(&prelude[start / core::mem::size_of::<Tok>()..])
                    } else {
                        MediaList::default()
                    };
                    out.push(Rule::Import {
                        url: (self.cx.resolve)(&u),
                        media,
                    });
                }
            }
            _ => {}
        }
        next
    }
}

/// Parse a stylesheet. `atoms` is the document's interner (selectors intern names into it).
pub fn parse(css: &str, cx: &ParseCx, atoms: &mut Interner, limits: &SheetLimits) -> Sheet {
    let toks = tokenize(css, limits.max_tokens);
    let mut p = Parser {
        t: &toks,
        cx,
        atoms,
        rules_left: limits.max_rules,
    };
    let mut rules = Vec::new();
    p.rules(0, toks.len(), 0, &mut rules);
    Sheet { rules }
}

/// Parse a `style` attribute.
pub fn parse_inline(css: &str, cx: &ParseCx) -> DeclBlock {
    let toks = tokenize(css, 1 << 16);
    let mut b = DeclBlock::default();
    parse_block(&toks, cx, &mut b.normal, &mut b.important);
    b
}

/// The number of style rules in a sheet (for limits and stats).
pub fn count_rules(rules: &[Rule]) -> usize {
    rules
        .iter()
        .map(|r| match r {
            Rule::Style { .. } => 1,
            Rule::Media { rules, .. } => count_rules(rules),
            Rule::Import { .. } => 0,
        })
        .sum()
}
