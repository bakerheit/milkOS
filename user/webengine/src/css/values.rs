//! CSS value parsing: lengths (CSS Values 4, including `calc()`, `min()`, `max()`,
//! `clamp()`), colours (CSS Color 4: hex, the named colours, `rgb()`/`hsl()` in both
//! syntaxes, system colours), numbers, and splitting a value into component values.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::tokenizer::Tok;

/// A length as written, as a linear combination of units (so `calc(100% - 2em + 3px)` is one
/// value). Resolved to CSS pixels at computed-value time ([`SLen::compute`]); `pct` stays a
/// percentage of whatever the property refers to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SLen {
    pub px: f32,
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
    pub vmin: f32,
    pub vmax: f32,
    pub pct: f32,
}

/// What a length is computed against.
#[derive(Clone, Copy, Debug)]
pub struct LenCx {
    /// The element's font size (for em), CSS px.
    pub em: f32,
    pub rem: f32,
    /// Viewport size, CSS px.
    pub vw: f32,
    pub vh: f32,
}

/// A computed length-percentage: `px + pct% × reference`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LP {
    pub px: f32,
    pub pct: f32,
}

impl LP {
    pub const ZERO: LP = LP { px: 0.0, pct: 0.0 };
    pub fn px(px: f32) -> LP {
        LP { px, pct: 0.0 }
    }
    pub fn has_pct(&self) -> bool {
        self.pct != 0.0
    }
    /// Against a reference in CSS px.
    pub fn resolve(&self, base: f32) -> f32 {
        self.px + self.pct * base / 100.0
    }
}

impl SLen {
    pub fn px(v: f32) -> SLen {
        SLen {
            px: v,
            ..Default::default()
        }
    }
    pub fn pct(v: f32) -> SLen {
        SLen {
            pct: v,
            ..Default::default()
        }
    }
    fn add(&self, o: &SLen, sign: f32) -> SLen {
        SLen {
            px: self.px + sign * o.px,
            em: self.em + sign * o.em,
            rem: self.rem + sign * o.rem,
            vw: self.vw + sign * o.vw,
            vh: self.vh + sign * o.vh,
            vmin: self.vmin + sign * o.vmin,
            vmax: self.vmax + sign * o.vmax,
            pct: self.pct + sign * o.pct,
        }
    }
    fn scale(&self, k: f32) -> SLen {
        SLen {
            px: self.px * k,
            em: self.em * k,
            rem: self.rem * k,
            vw: self.vw * k,
            vh: self.vh * k,
            vmin: self.vmin * k,
            vmax: self.vmax * k,
            pct: self.pct * k,
        }
    }
    pub fn compute(&self, c: &LenCx) -> LP {
        let vmin = if c.vw < c.vh { c.vw } else { c.vh };
        let vmax = if c.vw > c.vh { c.vw } else { c.vh };
        LP {
            px: self.px
                + self.em * c.em
                + self.rem * c.rem
                + self.vw * c.vw / 100.0
                + self.vh * c.vh / 100.0
                + self.vmin * vmin / 100.0
                + self.vmax * vmax / 100.0,
            pct: self.pct,
        }
    }
}

/// A specified length: a linear one, or `min()`/`max()`/`clamp()` over several.
#[derive(Clone, Debug, PartialEq)]
pub enum Len {
    Lin(SLen),
    /// 0 = min, 1 = max, 2 = clamp(lo, val, hi)
    MinMax(u8, Box<[SLen]>),
}

impl Len {
    pub fn px(v: f32) -> Len {
        Len::Lin(SLen::px(v))
    }
    /// Compute. `min()`/`max()` with percentages can't be resolved until layout; then the
    /// first argument is used (an approximation).
    pub fn compute(&self, c: &LenCx) -> LP {
        match self {
            Len::Lin(l) => l.compute(c),
            Len::MinMax(op, args) => {
                let v: Vec<LP> = args.iter().map(|a| a.compute(c)).collect();
                if v.is_empty() {
                    return LP::ZERO;
                }
                if v.iter().any(|x| x.has_pct()) {
                    return if *op == 2 && v.len() == 3 { v[1] } else { v[0] };
                }
                let px = match op {
                    0 => v.iter().map(|x| x.px).fold(f32::INFINITY, f32::min),
                    1 => v.iter().map(|x| x.px).fold(f32::NEG_INFINITY, f32::max),
                    _ => {
                        if v.len() != 3 {
                            v[0].px
                        } else {
                            v[1].px.min(v[2].px).max(v[0].px)
                        }
                    }
                };
                LP::px(px)
            }
        }
    }
}

/// Skip whitespace tokens from `i`.
pub fn skip_ws(t: &[Tok], mut i: usize) -> usize {
    while matches!(t.get(i), Some(Tok::Whitespace)) {
        i += 1;
    }
    i
}

/// Trim leading/trailing whitespace tokens.
pub fn trim(t: &[Tok]) -> &[Tok] {
    let mut a = 0;
    let mut b = t.len();
    while a < b && matches!(t[a], Tok::Whitespace) {
        a += 1;
    }
    while b > a && matches!(t[b - 1], Tok::Whitespace) {
        b -= 1;
    }
    &t[a..b]
}

/// The end (exclusive) of the component value starting at `i`: one token, or a whole function
/// or block with its closing token.
pub fn component_end(t: &[Tok], i: usize) -> usize {
    match t.get(i) {
        Some(Tok::Function(_)) | Some(Tok::Open(_)) => {
            let mut d = 0i32;
            let mut j = i;
            while j < t.len() {
                match &t[j] {
                    Tok::Function(_) | Tok::Open(_) => d += 1,
                    Tok::Close(_) => {
                        d -= 1;
                        if d == 0 {
                            return j + 1;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            t.len()
        }
        Some(_) => i + 1,
        None => i,
    }
}

/// Split a value into component values at top-level whitespace (commas and slashes become
/// their own components).
pub fn components(t: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if matches!(t[i], Tok::Whitespace) {
            i += 1;
            continue;
        }
        let e = component_end(t, i);
        out.push(&t[i..e]);
        i = e;
    }
    out
}

/// Split at top-level commas.
pub fn split_commas(t: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < t.len() {
        if matches!(t[i], Tok::Comma) {
            out.push(trim(&t[start..i]));
            start = i + 1;
            i += 1;
            continue;
        }
        i = component_end(t, i).max(i + 1);
    }
    out.push(trim(&t[start..]));
    out
}

/// The arguments of a function component (`f(...)`): the tokens between the name and `)`.
pub fn fn_args(c: &[Tok]) -> &[Tok] {
    if c.len() >= 2 && matches!(c[c.len() - 1], Tok::Close(')')) {
        &c[1..c.len() - 1]
    } else if !c.is_empty() {
        &c[1..]
    } else {
        c
    }
}

pub fn unit_len(v: f32, unit: &str) -> Option<SLen> {
    let mut l = SLen::default();
    match unit {
        "px" => l.px = v,
        "em" => l.em = v,
        "rem" => l.rem = v,
        // Without real font metrics: ex and ch are half an em (what browsers fall back to).
        "ex" | "ch" | "cap" | "ic" => l.em = v * if unit == "ic" { 1.0 } else { 0.5 },
        "rlh" | "lh" => l.em = v * 1.2,
        "pt" => l.px = v * 4.0 / 3.0,
        "pc" => l.px = v * 16.0,
        "in" => l.px = v * 96.0,
        "cm" => l.px = v * 96.0 / 2.54,
        "mm" => l.px = v * 96.0 / 25.4,
        "q" => l.px = v * 96.0 / 101.6,
        "vw" | "svw" | "lvw" | "dvw" | "cqw" | "vi" | "cqi" => l.vw = v,
        "vh" | "svh" | "lvh" | "dvh" | "cqh" | "vb" | "cqb" => l.vh = v,
        "vmin" | "svmin" | "lvmin" | "dvmin" | "cqmin" => l.vmin = v,
        "vmax" | "svmax" | "lvmax" | "dvmax" | "cqmax" => l.vmax = v,
        _ => return None,
    }
    Some(l)
}

/// One length component: `12px`, `0`, `50%`, `calc(...)`, `min(...)`, ... `allow_unitless`
/// lets a bare number be px (quirks-mode style; we accept it only for 0 unless asked).
pub fn length(c: &[Tok], allow_unitless: bool) -> Option<Len> {
    match c.first()? {
        Tok::Dimension { value, unit } if c.len() == 1 => unit_len(*value, unit).map(Len::Lin),
        Tok::Percentage(p) if c.len() == 1 => Some(Len::Lin(SLen::pct(*p))),
        Tok::Number { value, .. } if c.len() == 1 && (*value == 0.0 || allow_unitless) => {
            Some(Len::px(*value))
        }
        Tok::Function(f) => {
            let args = fn_args(c);
            match f.as_str() {
                "calc" | "-webkit-calc" | "-moz-calc" => calc_sum(args, 0).map(Len::Lin),
                "min" | "max" | "clamp" => {
                    let op = match f.as_str() {
                        "min" => 0,
                        "max" => 1,
                        _ => 2,
                    };
                    let mut v = Vec::new();
                    for a in split_commas(args) {
                        v.push(calc_sum(a, 0)?);
                    }
                    if v.is_empty() || (op == 2 && v.len() != 3) {
                        return None;
                    }
                    Some(Len::MinMax(op, v.into_boxed_slice()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// A calc() value: what it is, a length or a plain number (for multiplication).
#[derive(Clone, Copy)]
enum CalcV {
    Num(f32),
    Len(SLen),
}

/// calc-sum: products joined by `+`/`-` (whitespace required around them, CSS Values 4
/// §10.1; we're lenient about it).
fn calc_sum(t: &[Tok], depth: u32) -> Option<SLen> {
    match calc_sum_v(t, depth)? {
        CalcV::Len(l) => Some(l),
        CalcV::Num(0.0) => Some(SLen::default()),
        CalcV::Num(_) => None,
    }
}

fn calc_sum_v(t: &[Tok], depth: u32) -> Option<CalcV> {
    if depth > 16 {
        return None;
    }
    let t = trim(t);
    // Find top-level + and - (as delims, or signs glued to numbers after whitespace).
    let mut terms: Vec<(f32, &[Tok])> = Vec::new();
    let mut start = 0;
    let mut sign = 1.0;
    let mut i = 0;
    while i < t.len() {
        match &t[i] {
            Tok::Delim(d @ ('+' | '-')) if i > start => {
                terms.push((sign, &t[start..i]));
                sign = if *d == '-' { -1.0 } else { 1.0 };
                start = i + 1;
                i += 1;
            }
            Tok::Whitespace => {
                // "a -2px" tokenizes as a, ws, Dimension(-2): a signed number after whitespace
                // in operator position is "- 2px".
                let j = skip_ws(t, i);
                if j > start
                    && i > start
                    && let Some(tok) = t.get(j)
                {
                    let signed = match tok {
                        Tok::Number { value, .. }
                        | Tok::Percentage(value)
                        | Tok::Dimension { value, .. } => *value < 0.0,
                        _ => false,
                    };
                    if signed {
                        terms.push((sign, &t[start..i]));
                        sign = 1.0;
                        start = j;
                        i = j + 1;
                        continue;
                    }
                }
                i += 1;
            }
            _ => i = component_end(t, i).max(i + 1),
        }
    }
    terms.push((sign, &t[start..]));
    let mut acc: Option<CalcV> = None;
    for (s, term) in terms {
        let v = calc_product(trim(term), depth)?;
        acc = Some(match (acc, v) {
            (None, CalcV::Num(n)) => CalcV::Num(s * n),
            (None, CalcV::Len(l)) => CalcV::Len(l.scale(s)),
            (Some(CalcV::Num(a)), CalcV::Num(b)) => CalcV::Num(a + s * b),
            (Some(CalcV::Len(a)), CalcV::Len(b)) => CalcV::Len(a.add(&b, s)),
            // Number + length is invalid, except 0 (lenient).
            (Some(CalcV::Len(a)), CalcV::Num(0.0)) => CalcV::Len(a),
            (Some(CalcV::Num(0.0)), CalcV::Len(b)) => CalcV::Len(b.scale(s)),
            _ => return None,
        });
    }
    acc
}

fn calc_product(t: &[Tok], depth: u32) -> Option<CalcV> {
    let mut acc: Option<CalcV> = None;
    let mut op = '*';
    let mut i = 0;
    while i < t.len() {
        match &t[i] {
            Tok::Whitespace => {
                i += 1;
                continue;
            }
            Tok::Delim(d @ ('*' | '/')) => {
                op = *d;
                i += 1;
                continue;
            }
            _ => {}
        }
        let e = component_end(t, i);
        let v = calc_value(&t[i..e], depth)?;
        i = e;
        acc = Some(match acc {
            None => v,
            Some(a) => match (a, v, op) {
                (CalcV::Num(x), CalcV::Num(y), '*') => CalcV::Num(x * y),
                (CalcV::Num(x), CalcV::Num(y), _) if y != 0.0 => CalcV::Num(x / y),
                (CalcV::Len(l), CalcV::Num(y), '*') | (CalcV::Num(y), CalcV::Len(l), '*') => {
                    CalcV::Len(l.scale(y))
                }
                (CalcV::Len(l), CalcV::Num(y), '/') if y != 0.0 => CalcV::Len(l.scale(1.0 / y)),
                _ => return None,
            },
        });
    }
    acc
}

fn calc_value(c: &[Tok], depth: u32) -> Option<CalcV> {
    match c.first()? {
        Tok::Number { value, .. } => Some(CalcV::Num(*value)),
        Tok::Dimension { value, unit } => unit_len(*value, unit).map(CalcV::Len),
        Tok::Percentage(p) => Some(CalcV::Len(SLen::pct(*p))),
        Tok::Open('(') => calc_sum_v(fn_args(c), depth + 1),
        Tok::Function(f) if f == "calc" => calc_sum_v(fn_args(c), depth + 1),
        Tok::Function(f) if matches!(f.as_str(), "min" | "max" | "clamp") => {
            // Nested min/max: only without percentages.
            let l = length(c, false)?;
            let lp = l.compute(&LenCx {
                em: 16.0,
                rem: 16.0,
                vw: 1024.0,
                vh: 768.0,
            });
            match l {
                Len::Lin(s) => Some(CalcV::Len(s)),
                Len::MinMax(..) => Some(CalcV::Len(SLen::px(lp.px))),
            }
        }
        Tok::Ident(i) if i.eq_ignore_ascii_case("pi") => Some(CalcV::Num(core::f32::consts::PI)),
        Tok::Ident(i) if i.eq_ignore_ascii_case("e") => Some(CalcV::Num(core::f32::consts::E)),
        _ => None,
    }
}

/// A plain number component (also accepts `calc()` of numbers).
pub fn number(c: &[Tok]) -> Option<f32> {
    match c.first()? {
        Tok::Number { value, .. } if c.len() == 1 => Some(*value),
        Tok::Function(f) if f == "calc" => match calc_sum_v(fn_args(c), 0)? {
            CalcV::Num(n) => Some(n),
            _ => None,
        },
        _ => None,
    }
}

pub fn integer(c: &[Tok]) -> Option<i32> {
    match c.first()? {
        Tok::Number { value, int: true } if c.len() == 1 => Some(value.clamp(-1.0e9, 1.0e9) as i32),
        _ => None,
    }
}

// ---- colours ------------------------------------------------------------------------------

/// A specified colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SColor {
    Rgba(u32),
    CurrentColor,
}

pub fn color(c: &[Tok]) -> Option<SColor> {
    match c.first()? {
        Tok::Hash { value, .. } if c.len() == 1 => hex(value).map(SColor::Rgba),
        Tok::Ident(n) if c.len() == 1 => {
            let n = n.to_ascii_lowercase();
            if n == "currentcolor" {
                return Some(SColor::CurrentColor);
            }
            if n == "transparent" {
                return Some(SColor::Rgba(0));
            }
            named(&n)
                .or_else(|| system(&n))
                .map(|rgb| SColor::Rgba(0xFF00_0000 | rgb))
        }
        Tok::Function(f) => {
            let args = fn_args(c);
            match f.as_str() {
                "rgb" | "rgba" => rgb_fn(args).map(SColor::Rgba),
                "hsl" | "hsla" => hsl_fn(args).map(SColor::Rgba),
                _ => None,
            }
        }
        _ => None,
    }
}

fn hex(v: &str) -> Option<u32> {
    let b = v.as_bytes();
    if !b.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize| (b[i] as char).to_digit(16).unwrap();
    let (r, g, bl, a) = match b.len() {
        3 => (d(0) * 17, d(1) * 17, d(2) * 17, 255),
        4 => (d(0) * 17, d(1) * 17, d(2) * 17, d(3) * 17),
        6 => (d(0) * 16 + d(1), d(2) * 16 + d(3), d(4) * 16 + d(5), 255),
        8 => (
            d(0) * 16 + d(1),
            d(2) * 16 + d(3),
            d(4) * 16 + d(5),
            d(6) * 16 + d(7),
        ),
        _ => return None,
    };
    Some((a << 24) | (r << 16) | (g << 8) | bl)
}

/// Colour function arguments: comma-separated (legacy) or space-separated with `/ alpha`.
type ColorArgs<'a> = (Vec<&'a [Tok]>, Option<&'a [Tok]>);

fn color_args(args: &[Tok]) -> Option<ColorArgs<'_>> {
    let commas = split_commas(args);
    if commas.len() >= 3 {
        let alpha = if commas.len() == 4 {
            Some(commas[3])
        } else {
            None
        };
        if commas.len() > 4 {
            return None;
        }
        return Some((commas[..3].to_vec(), alpha));
    }
    let comps = components(args);
    let slash = comps
        .iter()
        .position(|c| matches!(c.first(), Some(Tok::Delim('/'))));
    match slash {
        Some(s) => {
            if s != 3 || comps.len() != 5 {
                return None;
            }
            Some((comps[..3].to_vec(), Some(comps[4])))
        }
        None if comps.len() == 3 => Some((comps, None)),
        _ => None,
    }
}

fn alpha_of(a: Option<&[Tok]>) -> Option<u32> {
    let Some(a) = a else { return Some(255) };
    let v = match a.first()? {
        Tok::Number { value, .. } => *value,
        Tok::Percentage(p) => *p / 100.0,
        Tok::Ident(n) if n.eq_ignore_ascii_case("none") => 0.0,
        _ => return None,
    };
    Some((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32)
}

fn rgb_fn(args: &[Tok]) -> Option<u32> {
    let (ch, a) = color_args(args)?;
    let mut v = [0u32; 3];
    for (i, c) in ch.iter().enumerate() {
        let x = match c.first()? {
            Tok::Number { value, .. } => *value,
            Tok::Percentage(p) => *p * 255.0 / 100.0,
            Tok::Ident(n) if n.eq_ignore_ascii_case("none") => 0.0,
            _ => return None,
        };
        v[i] = (x.clamp(0.0, 255.0) + 0.5) as u32;
    }
    Some((alpha_of(a)? << 24) | (v[0] << 16) | (v[1] << 8) | v[2])
}

fn hsl_fn(args: &[Tok]) -> Option<u32> {
    let (ch, a) = color_args(args)?;
    let h = match ch[0].first()? {
        Tok::Number { value, .. } => *value,
        Tok::Dimension { value, unit } => match unit.as_str() {
            "deg" => *value,
            "rad" => *value * 180.0 / core::f32::consts::PI,
            "grad" => *value * 0.9,
            "turn" => *value * 360.0,
            _ => return None,
        },
        _ => return None,
    };
    let pc = |c: &[Tok]| -> Option<f32> {
        match c.first()? {
            Tok::Percentage(p) => Some((*p / 100.0).clamp(0.0, 1.0)),
            Tok::Number { value, .. } => Some((*value / 100.0).clamp(0.0, 1.0)),
            _ => None,
        }
    };
    let s = pc(ch[1])?;
    let l = pc(ch[2])?;
    // CSS Color 4 §7.1 hslToRgb.
    let h = crate::fmath::fmod(h, 360.0);
    let h = if h < 0.0 { h + 360.0 } else { h };
    let f = |n: f32| {
        let k = crate::fmath::fmod(n + h / 30.0, 12.0);
        let k = if k < 0.0 { k + 12.0 } else { k };
        let a = s * if l < 1.0 - l { l } else { 1.0 - l };
        let m = (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0);
        l - a * m
    };
    let to = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    Some((alpha_of(a)? << 24) | (to(f(0.0)) << 16) | (to(f(8.0)) << 8) | to(f(4.0)))
}

/// CSS Color 4 §6.2 system colours (light-scheme values).
fn system(n: &str) -> Option<u32> {
    Some(match n {
        "canvas" | "field" | "window" | "buttonhighlight" => 0xFFFFFF,
        "canvastext" | "fieldtext" | "windowtext" | "buttontext" | "captiontext" | "infotext"
        | "menutext" => 0x000000,
        "linktext" => 0x0000EE,
        "visitedtext" => 0x551A8B,
        "activetext" => 0xEE0000,
        "buttonface" | "buttonborder" | "threedface" | "menu" => 0xF0F0F0,
        "graytext" => 0x808080,
        "highlight" | "selecteditem" => 0x3875D7,
        "highlighttext" | "selecteditemtext" => 0xFFFFFF,
        "mark" => 0xFFFF00,
        "marktext" => 0x000000,
        "accentcolor" => 0x3875D7,
        "accentcolortext" => 0xFFFFFF,
        _ => return None,
    })
}

/// The CSS named colours (CSS Color 4 §6.1), 0xRRGGBB.
pub fn named(n: &str) -> Option<u32> {
    const T: &[(&str, u32)] = &[
        ("aliceblue", 0xf0f8ff),
        ("antiquewhite", 0xfaebd7),
        ("aqua", 0x00ffff),
        ("aquamarine", 0x7fffd4),
        ("azure", 0xf0ffff),
        ("beige", 0xf5f5dc),
        ("bisque", 0xffe4c4),
        ("black", 0x000000),
        ("blanchedalmond", 0xffebcd),
        ("blue", 0x0000ff),
        ("blueviolet", 0x8a2be2),
        ("brown", 0xa52a2a),
        ("burlywood", 0xdeb887),
        ("cadetblue", 0x5f9ea0),
        ("chartreuse", 0x7fff00),
        ("chocolate", 0xd2691e),
        ("coral", 0xff7f50),
        ("cornflowerblue", 0x6495ed),
        ("cornsilk", 0xfff8dc),
        ("crimson", 0xdc143c),
        ("cyan", 0x00ffff),
        ("darkblue", 0x00008b),
        ("darkcyan", 0x008b8b),
        ("darkgoldenrod", 0xb8860b),
        ("darkgray", 0xa9a9a9),
        ("darkgreen", 0x006400),
        ("darkgrey", 0xa9a9a9),
        ("darkkhaki", 0xbdb76b),
        ("darkmagenta", 0x8b008b),
        ("darkolivegreen", 0x556b2f),
        ("darkorange", 0xff8c00),
        ("darkorchid", 0x9932cc),
        ("darkred", 0x8b0000),
        ("darksalmon", 0xe9967a),
        ("darkseagreen", 0x8fbc8f),
        ("darkslateblue", 0x483d8b),
        ("darkslategray", 0x2f4f4f),
        ("darkslategrey", 0x2f4f4f),
        ("darkturquoise", 0x00ced1),
        ("darkviolet", 0x9400d3),
        ("deeppink", 0xff1493),
        ("deepskyblue", 0x00bfff),
        ("dimgray", 0x696969),
        ("dimgrey", 0x696969),
        ("dodgerblue", 0x1e90ff),
        ("firebrick", 0xb22222),
        ("floralwhite", 0xfffaf0),
        ("forestgreen", 0x228b22),
        ("fuchsia", 0xff00ff),
        ("gainsboro", 0xdcdcdc),
        ("ghostwhite", 0xf8f8ff),
        ("gold", 0xffd700),
        ("goldenrod", 0xdaa520),
        ("gray", 0x808080),
        ("green", 0x008000),
        ("greenyellow", 0xadff2f),
        ("grey", 0x808080),
        ("honeydew", 0xf0fff0),
        ("hotpink", 0xff69b4),
        ("indianred", 0xcd5c5c),
        ("indigo", 0x4b0082),
        ("ivory", 0xfffff0),
        ("khaki", 0xf0e68c),
        ("lavender", 0xe6e6fa),
        ("lavenderblush", 0xfff0f5),
        ("lawngreen", 0x7cfc00),
        ("lemonchiffon", 0xfffacd),
        ("lightblue", 0xadd8e6),
        ("lightcoral", 0xf08080),
        ("lightcyan", 0xe0ffff),
        ("lightgoldenrodyellow", 0xfafad2),
        ("lightgray", 0xd3d3d3),
        ("lightgreen", 0x90ee90),
        ("lightgrey", 0xd3d3d3),
        ("lightpink", 0xffb6c1),
        ("lightsalmon", 0xffa07a),
        ("lightseagreen", 0x20b2aa),
        ("lightskyblue", 0x87cefa),
        ("lightslategray", 0x778899),
        ("lightslategrey", 0x778899),
        ("lightsteelblue", 0xb0c4de),
        ("lightyellow", 0xffffe0),
        ("lime", 0x00ff00),
        ("limegreen", 0x32cd32),
        ("linen", 0xfaf0e6),
        ("magenta", 0xff00ff),
        ("maroon", 0x800000),
        ("mediumaquamarine", 0x66cdaa),
        ("mediumblue", 0x0000cd),
        ("mediumorchid", 0xba55d3),
        ("mediumpurple", 0x9370db),
        ("mediumseagreen", 0x3cb371),
        ("mediumslateblue", 0x7b68ee),
        ("mediumspringgreen", 0x00fa9a),
        ("mediumturquoise", 0x48d1cc),
        ("mediumvioletred", 0xc71585),
        ("midnightblue", 0x191970),
        ("mintcream", 0xf5fffa),
        ("mistyrose", 0xffe4e1),
        ("moccasin", 0xffe4b5),
        ("navajowhite", 0xffdead),
        ("navy", 0x000080),
        ("oldlace", 0xfdf5e6),
        ("olive", 0x808000),
        ("olivedrab", 0x6b8e23),
        ("orange", 0xffa500),
        ("orangered", 0xff4500),
        ("orchid", 0xda70d6),
        ("palegoldenrod", 0xeee8aa),
        ("palegreen", 0x98fb98),
        ("paleturquoise", 0xafeeee),
        ("palevioletred", 0xdb7093),
        ("papayawhip", 0xffefd5),
        ("peachpuff", 0xffdab9),
        ("peru", 0xcd853f),
        ("pink", 0xffc0cb),
        ("plum", 0xdda0dd),
        ("powderblue", 0xb0e0e6),
        ("purple", 0x800080),
        ("rebeccapurple", 0x663399),
        ("red", 0xff0000),
        ("rosybrown", 0xbc8f8f),
        ("royalblue", 0x4169e1),
        ("saddlebrown", 0x8b4513),
        ("salmon", 0xfa8072),
        ("sandybrown", 0xf4a460),
        ("seagreen", 0x2e8b57),
        ("seashell", 0xfff5ee),
        ("sienna", 0xa0522d),
        ("silver", 0xc0c0c0),
        ("skyblue", 0x87ceeb),
        ("slateblue", 0x6a5acd),
        ("slategray", 0x708090),
        ("slategrey", 0x708090),
        ("snow", 0xfffafa),
        ("springgreen", 0x00ff7f),
        ("steelblue", 0x4682b4),
        ("tan", 0xd2b48c),
        ("teal", 0x008080),
        ("thistle", 0xd8bfd8),
        ("tomato", 0xff6347),
        ("turquoise", 0x40e0d0),
        ("violet", 0xee82ee),
        ("wheat", 0xf5deb3),
        ("white", 0xffffff),
        ("whitesmoke", 0xf5f5f5),
        ("yellow", 0xffff00),
        ("yellowgreen", 0x9acd32),
    ];
    T.binary_search_by(|e| e.0.cmp(n)).ok().map(|i| T[i].1)
}

/// The legacy HTML colour parsing (`<font color>`, `bgcolor`), HTML §2.3.6 "rules for parsing
/// a legacy colour value", simplified: named colours, `#rgb`/`#rrggbb`, and bare hex.
pub fn legacy_color(v: &str) -> Option<u32> {
    let v = v.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Some(c) = named(&v.to_ascii_lowercase()) {
        return Some(0xFF00_0000 | c);
    }
    let h = v.strip_prefix('#').unwrap_or(v);
    if h.len() == 3 && h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return hex(h);
    }
    // Pad / mangle the way browsers do: non-hex digits become 0, split in three.
    let mut s: Vec<u8> = h
        .bytes()
        .take(128)
        .map(|c| if c.is_ascii_hexdigit() { c } else { b'0' })
        .collect();
    if s.is_empty() {
        return None;
    }
    while !s.len().is_multiple_of(3) {
        s.push(b'0');
    }
    let n = s.len() / 3;
    let mut parts: Vec<&[u8]> = s.chunks(n).collect();
    let mut len = n;
    if len > 8 {
        for p in parts.iter_mut() {
            *p = &p[len - 8..];
        }
        len = 8;
    }
    while len > 2 && parts.iter().all(|p| p[0] == b'0') {
        for p in parts.iter_mut() {
            *p = &p[1..];
        }
        len -= 1;
    }
    let val = |p: &[u8]| {
        let p = &p[..p.len().min(2)];
        let mut x = 0;
        for &c in p {
            x = x * 16 + (c as char).to_digit(16).unwrap_or(0);
        }
        if p.len() == 1 { x * 16 } else { x }
    };
    Some(0xFF00_0000 | (val(parts[0]) << 16) | (val(parts[1]) << 8) | val(parts[2]))
}
