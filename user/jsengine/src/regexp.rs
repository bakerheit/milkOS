//! Regular expressions: a parser for the ES2020 pattern syntax (with the Annex B leniency web
//! pages rely on), a compiler to a small program, and a backtracking matcher with an explicit
//! stack and a step limit (so `(a+)+b` on a long string stops instead of hanging).
//! Flags: g i m s u y (d is accepted). Lookahead, lookbehind, capturing / non-capturing /
//! named groups, backreferences, \p{...} for the common properties.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::heap::Tracer;
use crate::value::{StrId, Val};

/// Steps one exec may take before giving up.
pub(crate) const STEP_LIMIT: u64 = 20_000_000;

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Flags {
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub unicode: bool,
    pub sticky: bool,
    pub has_indices: bool,
}

pub(crate) fn parse_flags(f: &[u16]) -> Result<Flags, String> {
    let mut fl = Flags::default();
    for &c in f {
        let slot = match c {
            0x67 => &mut fl.global,
            0x69 => &mut fl.ignore_case,
            0x6d => &mut fl.multiline,
            0x73 => &mut fl.dot_all,
            0x75 => &mut fl.unicode,
            0x79 => &mut fl.sticky,
            0x64 => &mut fl.has_indices,
            _ => {
                return Err(alloc::format!(
                    "Invalid flags supplied to RegExp constructor '{}'",
                    crate::string::to_rust(f)
                ));
            }
        };
        if *slot {
            return Err(alloc::format!(
                "Invalid flags supplied to RegExp constructor '{}'",
                crate::string::to_rust(f)
            ));
        }
        *slot = true;
    }
    Ok(fl)
}

// ---- syntax ----

#[derive(Clone, Debug)]
enum ClassItem {
    Range(u32, u32),
    Digit(bool),
    Word(bool),
    Space(bool),
    Prop(Prop, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prop {
    Letter,
    Upper,
    Lower,
    Number,
    Digit,
    Punct,
    Space,
    Alpha,
    Emoji,
    Han,
    Latin,
    Greek,
    Cyrillic,
    Any,
    Ascii,
    Symbol,
    Mark,
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(u32),
    Any,
    Class(Vec<ClassItem>, bool),
    Bol,
    Eol,
    WordB(bool),
    Group(Box<Node>, Option<u32>),
    Backref(u32),
    NamedBackref(String),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, u32, u32, bool),
    Look(Box<Node>, bool, bool),
}

struct Parser<'a> {
    s: &'a [u16],
    i: usize,
    unicode: bool,
    ncaps: u32,
    names: Vec<(String, u32)>,
    depth: u32,
    has_named: bool,
}

type PResult<T> = Result<T, String>;

fn is_syntax_char(c: u32) -> bool {
    matches!(
        c,
        0x5e | 0x24
            | 0x5c
            | 0x2e
            | 0x2a
            | 0x2b
            | 0x3f
            | 0x28
            | 0x29
            | 0x5b
            | 0x5d
            | 0x7b
            | 0x7d
            | 0x7c
            | 0x2f
    )
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u32> {
        self.s.get(self.i).map(|&c| c as u32)
    }
    fn peek2(&self) -> Option<u32> {
        self.s.get(self.i + 1).map(|&c| c as u32)
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c as u32) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    /// One source character (a code point in unicode mode).
    fn next_char(&mut self) -> Option<u32> {
        let c = *self.s.get(self.i)? as u32;
        self.i += 1;
        if self.unicode
            && (0xD800..0xDC00).contains(&c)
            && let Some(&d) = self.s.get(self.i)
            && (0xDC00..0xE000).contains(&d)
        {
            self.i += 1;
            return Some(0x10000 + ((c - 0xD800) << 10) + (d as u32 - 0xDC00));
        }
        Some(c)
    }

    fn disjunction(&mut self) -> PResult<Node> {
        self.depth += 1;
        if self.depth > 200 {
            return Err(String::from("regular expression too deeply nested"));
        }
        let mut alts = alloc::vec![self.alternative()?];
        while self.eat(b'|') {
            alts.push(self.alternative()?);
        }
        self.depth -= 1;
        Ok(if alts.len() == 1 {
            alts.pop().unwrap_or(Node::Empty)
        } else {
            Node::Alt(alts)
        })
    }

    fn alternative(&mut self) -> PResult<Node> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == b'|' as u32 || c == b')' as u32 {
                break;
            }
            let atom = self.term()?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap_or(Node::Empty),
            _ => Node::Concat(items),
        })
    }

    fn term(&mut self) -> PResult<Node> {
        let c = self.peek().unwrap_or(0);
        // Assertions.
        match c {
            0x5e => {
                self.i += 1;
                return Ok(Node::Bol);
            }
            0x24 => {
                self.i += 1;
                return Ok(Node::Eol);
            }
            0x5c if self.peek2() == Some(b'b' as u32) => {
                self.i += 2;
                return Ok(Node::WordB(false));
            }
            0x5c if self.peek2() == Some(b'B' as u32) => {
                self.i += 2;
                return Ok(Node::WordB(true));
            }
            0x28 if self.s.get(self.i + 1) == Some(&(b'?' as u16)) => {
                let k = self.s.get(self.i + 2).copied().unwrap_or(0) as u32;
                let k2 = self.s.get(self.i + 3).copied().unwrap_or(0) as u32;
                let look = match (k, k2) {
                    (0x3d, _) => Some((true, false, 3)),
                    (0x21, _) => Some((true, true, 3)),
                    (0x3c, 0x3d) => Some((false, false, 4)),
                    (0x3c, 0x21) => Some((false, true, 4)),
                    _ => None,
                };
                if let Some((ahead, neg, skip)) = look {
                    self.i += skip;
                    let d = self.disjunction()?;
                    if !self.eat(b')') {
                        return Err(String::from("Unterminated group"));
                    }
                    let node = Node::Look(Box::new(d), ahead, neg);
                    // Annex B: lookaheads may be quantified in non-unicode mode.
                    if ahead && !self.unicode {
                        return self.quantifier(node);
                    }
                    return Ok(node);
                }
            }
            _ => {}
        }
        let atom = self.atom()?;
        self.quantifier(atom)
    }

    fn quantifier(&mut self, atom: Node) -> PResult<Node> {
        let save = self.i;
        let (min, max) = match self.peek() {
            Some(0x2a) => {
                self.i += 1;
                (0, u32::MAX)
            }
            Some(0x2b) => {
                self.i += 1;
                (1, u32::MAX)
            }
            Some(0x3f) => {
                self.i += 1;
                (0, 1)
            }
            Some(0x7b) => match self.braces() {
                Some(mm) => mm,
                None => {
                    if self.unicode {
                        return Err(String::from("Incomplete quantifier"));
                    }
                    self.i = save;
                    return Ok(atom);
                }
            },
            _ => return Ok(atom),
        };
        if min > max {
            return Err(String::from("numbers out of order in {} quantifier"));
        }
        let greedy = !self.eat(b'?');
        if matches!(atom, Node::Bol | Node::Eol | Node::WordB(_))
            || matches!(atom, Node::Look(_, false, _))
        {
            return Err(String::from("Nothing to repeat"));
        }
        Ok(Node::Repeat(Box::new(atom), min, max, greedy))
    }

    /// `{n}`, `{n,}`, `{n,m}`; None (and no progress) if it isn't one.
    fn braces(&mut self) -> Option<(u32, u32)> {
        let start = self.i;
        self.i += 1;
        let n = self.number();
        let Some(n) = n else {
            self.i = start;
            return None;
        };
        let m = if self.eat(b',') {
            if self.peek() == Some(b'}' as u32) {
                u32::MAX
            } else {
                self.number().unwrap_or(u32::MAX - 1)
            }
        } else {
            n
        };
        if !self.eat(b'}') {
            self.i = start;
            return None;
        }
        Some((n, m))
    }

    fn number(&mut self) -> Option<u32> {
        let start = self.i;
        let mut v: u64 = 0;
        while let Some(c) = self.peek() {
            if (0x30..=0x39).contains(&c) {
                v = (v * 10 + (c - 0x30) as u64).min(u32::MAX as u64 - 1);
                self.i += 1;
            } else {
                break;
            }
        }
        if self.i == start {
            None
        } else {
            Some(v as u32)
        }
    }

    fn atom(&mut self) -> PResult<Node> {
        let Some(c) = self.peek() else {
            return Err(String::from("unexpected end"));
        };
        match c {
            0x2e => {
                self.i += 1;
                Ok(Node::Any)
            }
            0x28 => {
                self.i += 1;
                if self.eat(b'?') {
                    if self.eat(b':') {
                        let d = self.disjunction()?;
                        if !self.eat(b')') {
                            return Err(String::from("Unterminated group"));
                        }
                        return Ok(Node::Group(Box::new(d), None));
                    }
                    if self.eat(b'<') {
                        let name = self.group_name()?;
                        self.ncaps += 1;
                        let idx = self.ncaps;
                        if self.names.iter().any(|(n, _)| *n == name) {
                            return Err(String::from("Duplicate capture group name"));
                        }
                        self.names.push((name, idx));
                        let d = self.disjunction()?;
                        if !self.eat(b')') {
                            return Err(String::from("Unterminated group"));
                        }
                        return Ok(Node::Group(Box::new(d), Some(idx)));
                    }
                    return Err(String::from("Invalid group"));
                }
                self.ncaps += 1;
                let idx = self.ncaps;
                let d = self.disjunction()?;
                if !self.eat(b')') {
                    return Err(String::from("Unterminated group"));
                }
                Ok(Node::Group(Box::new(d), Some(idx)))
            }
            0x29 => Err(String::from("Unmatched ')'")),
            0x5b => self.class(),
            0x5c => self.atom_escape(),
            0x2a | 0x2b | 0x3f => Err(String::from("Nothing to repeat")),
            0x7b => {
                if self.unicode {
                    return Err(String::from("Lone quantifier brackets"));
                }
                // Annex B: a `{` that isn't a quantifier is a literal.
                let save = self.i;
                if self.braces().is_some() {
                    self.i = save;
                    return Err(String::from("Nothing to repeat"));
                }
                self.i = save + 1;
                Ok(Node::Char(c))
            }
            0x7d | 0x5d if !self.unicode => {
                self.i += 1;
                Ok(Node::Char(c))
            }
            0x7d | 0x5d => Err(String::from("Lone quantifier brackets")),
            _ => {
                let ch = self.next_char().unwrap_or(0);
                Ok(Node::Char(ch))
            }
        }
    }

    fn group_name(&mut self) -> PResult<String> {
        let mut name = String::new();
        loop {
            let Some(c) = self.next_char() else {
                return Err(String::from("Invalid capture group name"));
            };
            if c == b'>' as u32 {
                break;
            }
            let ch = char::from_u32(c).unwrap_or('\u{FFFD}');
            if !(ch.is_alphanumeric() || ch == '_' || ch == '$')
                || (name.is_empty() && ch.is_ascii_digit())
            {
                return Err(String::from("Invalid capture group name"));
            }
            name.push(ch);
        }
        if name.is_empty() {
            return Err(String::from("Invalid capture group name"));
        }
        Ok(name)
    }

    fn atom_escape(&mut self) -> PResult<Node> {
        self.i += 1; // backslash
        let Some(c) = self.peek() else {
            return Err(String::from("\\ at end of pattern"));
        };
        // Backreferences.
        if (0x31..=0x39).contains(&c) {
            let save = self.i;
            let n = self.number().unwrap_or(0);
            if n <= self.total_caps() {
                return Ok(Node::Backref(n));
            }
            if self.unicode {
                return Err(String::from("Invalid escape"));
            }
            // Annex B: not a backreference: an octal escape (or a literal digit).
            self.i = save;
            if c >= 0x38 {
                self.i += 1;
                return Ok(Node::Char(c));
            }
            return Ok(Node::Char(self.legacy_octal()));
        }
        if c == b'k' as u32 && (self.unicode || self.has_named) {
            self.i += 1;
            if !self.eat(b'<') {
                return Err(String::from("Invalid named reference"));
            }
            let name = self.group_name()?;
            return Ok(Node::NamedBackref(name));
        }
        if let Some(item) = self.class_escape_set()? {
            return Ok(Node::Class(alloc::vec![item], false));
        }
        let ch = self.char_escape(false)?;
        Ok(Node::Char(ch))
    }

    fn total_caps(&self) -> u32 {
        // Count capturing groups in the whole pattern (backreferences may refer forward).
        let mut n = 0;
        let mut i = 0;
        let mut in_class = false;
        while i < self.s.len() {
            let c = self.s[i];
            if c == b'\\' as u16 {
                i += 2;
                continue;
            }
            if in_class {
                if c == b']' as u16 {
                    in_class = false;
                }
            } else if c == b'[' as u16 {
                in_class = true;
            } else if c == b'(' as u16 {
                let next = self.s.get(i + 1).copied();
                if next != Some(b'?' as u16)
                    || (self.s.get(i + 2) == Some(&(b'<' as u16))
                        && !matches!(self.s.get(i + 3), Some(&0x3d) | Some(&0x21)))
                {
                    n += 1;
                }
            }
            i += 1;
        }
        n
    }

    fn legacy_octal(&mut self) -> u32 {
        let mut v = 0u32;
        let mut n = 0;
        while let Some(c) = self.peek() {
            if (0x30..=0x37).contains(&c) && n < 3 && v * 8 + (c - 0x30) <= 0o377 {
                v = v * 8 + (c - 0x30);
                self.i += 1;
                n += 1;
            } else {
                break;
            }
        }
        v
    }

    /// \d \D \w \W \s \S \p{..} \P{..}
    fn class_escape_set(&mut self) -> PResult<Option<ClassItem>> {
        let Some(c) = self.peek() else {
            return Ok(None);
        };
        let item = match c {
            0x64 => ClassItem::Digit(false),
            0x44 => ClassItem::Digit(true),
            0x77 => ClassItem::Word(false),
            0x57 => ClassItem::Word(true),
            0x73 => ClassItem::Space(false),
            0x53 => ClassItem::Space(true),
            0x70 | 0x50 if self.unicode => {
                let neg = c == 0x50;
                self.i += 1;
                if !self.eat(b'{') {
                    return Err(String::from("Invalid property name"));
                }
                let start = self.i;
                while self.peek().is_some_and(|c| c != b'}' as u32) {
                    self.i += 1;
                }
                let name = crate::string::to_rust(&self.s[start..self.i]);
                if !self.eat(b'}') {
                    return Err(String::from("Invalid property name"));
                }
                let p = prop_by_name(&name)
                    .ok_or_else(|| alloc::format!("Invalid property name {name}"))?;
                return Ok(Some(ClassItem::Prop(p, neg)));
            }
            _ => return Ok(None),
        };
        self.i += 1;
        Ok(Some(item))
    }

    /// A character escape after the backslash (at self.i).
    fn char_escape(&mut self, in_class: bool) -> PResult<u32> {
        let Some(c) = self.next_char() else {
            return Err(String::from("\\ at end of pattern"));
        };
        Ok(match c {
            0x74 => 9,
            0x6e => 10,
            0x76 => 11,
            0x66 => 12,
            0x72 => 13,
            0x30 if !self.peek().is_some_and(|d| (0x30..=0x39).contains(&d)) => 0,
            0x30..=0x37 if !self.unicode => {
                self.i -= 1;
                self.legacy_octal()
            }
            0x63 => {
                // \cX
                match self.peek() {
                    Some(l) if (l | 0x20) >= 0x61 && (l | 0x20) <= 0x7a => {
                        self.i += 1;
                        l % 32
                    }
                    Some(l)
                        if in_class
                            && !self.unicode
                            && ((0x30..=0x39).contains(&l) || l == 0x5f) =>
                    {
                        self.i += 1;
                        l % 32
                    }
                    _ => {
                        if self.unicode {
                            return Err(String::from("Invalid unicode escape"));
                        }
                        self.i -= 1;
                        0x5c
                    }
                }
            }
            0x78 => {
                let h1 = self.peek().and_then(hex);
                let h2 = self.peek2().and_then(hex);
                match (h1, h2) {
                    (Some(a), Some(b)) => {
                        self.i += 2;
                        a * 16 + b
                    }
                    _ => {
                        if self.unicode {
                            return Err(String::from("Invalid escape"));
                        }
                        0x78
                    }
                }
            }
            0x75 => match self.unicode_escape() {
                Some(v) => v,
                None => {
                    if self.unicode {
                        return Err(String::from("Invalid Unicode escape"));
                    }
                    0x75
                }
            },
            0x2d if in_class => 0x2d,
            0x62 if in_class => 8,
            c => {
                if self.unicode && !is_syntax_char(c) && c != 0x2d {
                    return Err(String::from("Invalid escape"));
                }
                c
            }
        })
    }

    fn unicode_escape(&mut self) -> Option<u32> {
        let save = self.i;
        if self.eat(b'{') && self.unicode {
            let mut v: u32 = 0;
            let mut n = 0;
            while let Some(d) = self.peek().and_then(hex) {
                v = v.saturating_mul(16).saturating_add(d);
                self.i += 1;
                n += 1;
            }
            if n == 0 || !self.eat(b'}') || v > 0x10FFFF {
                self.i = save;
                return None;
            }
            return Some(v);
        }
        self.i = save;
        let mut v = 0;
        for _ in 0..4 {
            match self.peek().and_then(hex) {
                Some(d) => {
                    v = v * 16 + d;
                    self.i += 1;
                }
                None => {
                    self.i = save;
                    return None;
                }
            }
        }
        // A surrogate pair 😀 in unicode mode is one code point.
        if self.unicode
            && (0xD800..0xDC00).contains(&v)
            && self.peek() == Some(0x5c)
            && self.peek2() == Some(0x75)
        {
            let s2 = self.i;
            self.i += 2;
            let mut w = 0;
            let mut ok = true;
            for _ in 0..4 {
                match self.peek().and_then(hex) {
                    Some(d) => {
                        w = w * 16 + d;
                        self.i += 1;
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok && (0xDC00..0xE000).contains(&w) {
                return Some(0x10000 + ((v - 0xD800) << 10) + (w - 0xDC00));
            }
            self.i = s2;
        }
        Some(v)
    }

    fn class(&mut self) -> PResult<Node> {
        self.i += 1; // [
        let negated = self.eat(b'^');
        let mut items = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(String::from("Unterminated character class"));
            };
            if c == b']' as u32 {
                self.i += 1;
                break;
            }
            let lo = self.class_atom()?;
            if self.peek() == Some(b'-' as u32) && self.peek2().is_some_and(|d| d != b']' as u32) {
                self.i += 1;
                let hi = self.class_atom()?;
                match (&lo, &hi) {
                    (ClassItem::Range(a, _), ClassItem::Range(b, _)) => {
                        if a > b {
                            return Err(String::from("Range out of order in character class"));
                        }
                        items.push(ClassItem::Range(*a, *b));
                    }
                    _ => {
                        if self.unicode {
                            return Err(String::from("Invalid character class"));
                        }
                        items.push(lo);
                        items.push(ClassItem::Range(0x2d, 0x2d));
                        items.push(hi);
                    }
                }
            } else {
                items.push(lo);
            }
        }
        Ok(Node::Class(items, negated))
    }

    fn class_atom(&mut self) -> PResult<ClassItem> {
        let Some(c) = self.peek() else {
            return Err(String::from("Unterminated character class"));
        };
        if c == b'\\' as u32 {
            self.i += 1;
            if let Some(item) = self.class_escape_set()? {
                return Ok(item);
            }
            let ch = self.char_escape(true)?;
            return Ok(ClassItem::Range(ch, ch));
        }
        let ch = self.next_char().unwrap_or(0);
        Ok(ClassItem::Range(ch, ch))
    }
}

fn hex(c: u32) -> Option<u32> {
    char::from_u32(c).and_then(|c| c.to_digit(16))
}

fn prop_by_name(n: &str) -> Option<Prop> {
    let n = n
        .strip_prefix("General_Category=")
        .or_else(|| n.strip_prefix("gc="))
        .unwrap_or(n);
    let n = n
        .strip_prefix("Script=")
        .or_else(|| n.strip_prefix("sc="))
        .or_else(|| n.strip_prefix("Script_Extensions="))
        .or_else(|| n.strip_prefix("scx="))
        .unwrap_or(n);
    Some(match n {
        "L" | "Letter" | "LC" | "Cased_Letter" | "Lo" | "Other_Letter" | "Lm"
        | "Modifier_Letter" | "Lt" | "Titlecase_Letter" => Prop::Letter,
        "Lu" | "Uppercase_Letter" | "Uppercase" => Prop::Upper,
        "Ll" | "Lowercase_Letter" | "Lowercase" => Prop::Lower,
        "N" | "Number" | "Nl" | "Letter_Number" | "No" | "Other_Number" => Prop::Number,
        "Nd" | "Decimal_Number" | "digit" => Prop::Digit,
        "P"
        | "Punctuation"
        | "punct"
        | "Pc"
        | "Pd"
        | "Ps"
        | "Pe"
        | "Pi"
        | "Pf"
        | "Po"
        | "Connector_Punctuation"
        | "Dash_Punctuation"
        | "Open_Punctuation"
        | "Close_Punctuation"
        | "Other_Punctuation" => Prop::Punct,
        "Z" | "Separator" | "Zs" | "Space_Separator" | "White_Space" | "space" | "Zl" | "Zp" => {
            Prop::Space
        }
        "Alphabetic" | "Alpha" => Prop::Alpha,
        "Emoji" | "Emoji_Presentation" | "Extended_Pictographic" => Prop::Emoji,
        "Han" | "Hani" => Prop::Han,
        "Latin" | "Latn" => Prop::Latin,
        "Greek" | "Grek" => Prop::Greek,
        "Cyrillic" | "Cyrl" => Prop::Cyrillic,
        "Any" => Prop::Any,
        "ASCII" => Prop::Ascii,
        "S" | "Symbol" | "Sm" | "Sc" | "Sk" | "So" | "Math_Symbol" | "Currency_Symbol"
        | "Modifier_Symbol" | "Other_Symbol" => Prop::Symbol,
        "M" | "Mark" | "Mn" | "Mc" | "Me" | "Nonspacing_Mark" | "Combining_Mark" => Prop::Mark,
        _ => return None,
    })
}

fn prop_has(p: Prop, c: u32) -> bool {
    let Some(ch) = char::from_u32(c) else {
        return false;
    };
    match p {
        Prop::Letter => ch.is_alphabetic() && !is_mark(c) && !ch.is_numeric(),
        Prop::Upper => ch.is_uppercase(),
        Prop::Lower => ch.is_lowercase(),
        Prop::Number => ch.is_numeric(),
        Prop::Digit => {
            ch.is_ascii_digit()
                || (ch.is_numeric() && ch.is_ascii_digit())
                || matches!(c, 0x0660..=0x0669 | 0x06F0..=0x06F9 | 0x0966..=0x096F | 0xFF10..=0xFF19)
        }
        Prop::Punct => {
            (ch.is_ascii_punctuation()
                && !matches!(ch, '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~'))
                || matches!(c, 0x00A1 | 0x00A7 | 0x00AB | 0x00B6 | 0x00B7 | 0x00BB | 0x00BF | 0x2010..=0x2027 | 0x2030..=0x205E | 0x3001..=0x3003 | 0x3008..=0x3011)
        }
        Prop::Space => ch.is_whitespace(),
        Prop::Alpha => ch.is_alphabetic(),
        Prop::Emoji => {
            matches!(c, 0x23 | 0x2A | 0x30..=0x39 | 0xA9 | 0xAE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x21AA | 0x231A..=0x23FF | 0x24C2 | 0x25AA..=0x27BF | 0x2934 | 0x2935 | 0x2B05..=0x2B55 | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF)
        }
        Prop::Han => {
            matches!(c, 0x2E80..=0x2FDF | 0x3005 | 0x3007 | 0x3021..=0x3029 | 0x3038..=0x303B | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3134F)
        }
        Prop::Latin => {
            ch.is_alphabetic()
                && (c < 0x250 || (0x1E00..0x1F00).contains(&c) || (0xFF21..=0xFF5A).contains(&c))
                && !(0xD7..=0xD7).contains(&c)
                && c != 0xF7
        }
        Prop::Greek => matches!(c, 0x370..=0x3FF | 0x1F00..=0x1FFF) && ch.is_alphabetic(),
        Prop::Cyrillic => {
            matches!(c, 0x400..=0x52F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F)
        }
        Prop::Any => true,
        Prop::Ascii => c < 128,
        Prop::Symbol => {
            matches!(ch, '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~')
                || matches!(c, 0xA2..=0xA9 | 0xAC | 0xAE..=0xB1 | 0xB4 | 0xD7 | 0xF7 | 0x2100..=0x214F | 0x2190..=0x23FF | 0x25A0..=0x27BF | 0x1F300..=0x1FAFF)
        }
        Prop::Mark => is_mark(c),
    }
}

fn is_mark(c: u32) -> bool {
    matches!(c, 0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F | 0x0900..=0x0903 | 0x093A..=0x094F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F)
}

// ---- program ----

#[derive(Clone, Copy, Debug)]
enum Inst {
    Char(u32),
    Any,
    Class(u32),
    Bol,
    Eol,
    WordB(bool),
    Save(u32),
    Split(u32, u32),
    Jmp(u32),
    Backref(u32),
    /// Sub-program at [pc+1, end): lookahead/behind; continue at end.
    Look {
        end: u32,
        ahead: bool,
        negate: bool,
        minlen: u32,
        maxlen: u32,
    },
    /// Registers for loops: remember the position / fail if it hasn't moved.
    RepStart(u32),
    RepCheck(u32),
    /// Resets capture groups lo..hi (a new iteration of a quantified group).
    ClearCaps(u32, u32),
    /// Lookbehind end: succeed only at the position held in a register.
    AtReg(u32),
    Match,
}

struct Class {
    items: Vec<ClassItem>,
    negated: bool,
}

pub(crate) struct Program {
    insts: Vec<Inst>,
    classes: Vec<Class>,
    pub ncaps: u32,
    nregs: u32,
    pub names: Vec<(String, u32)>,
    pub flags: Flags,
}

struct Compiler {
    insts: Vec<Inst>,
    classes: Vec<Class>,
    nregs: u32,
    icase: bool,
}

const MAX_INSTS: usize = 400_000;

/// The smallest and largest number of input units a node can match (None = unbounded).
fn widths(n: &Node, unicode: bool) -> (u32, Option<u32>) {
    match n {
        Node::Empty | Node::Bol | Node::Eol | Node::WordB(_) | Node::Look(..) => (0, Some(0)),
        Node::Char(c) => {
            let w = if unicode && *c >= 0x10000 { 2 } else { 1 };
            (w, Some(w))
        }
        Node::Any | Node::Class(..) => (1, Some(if unicode { 2 } else { 1 })),
        Node::Backref(_) | Node::NamedBackref(_) => (0, None),
        Node::Group(g, _) => widths(g, unicode),
        Node::Concat(v) => {
            let mut lo = 0u32;
            let mut hi = Some(0u32);
            for x in v {
                let (a, b) = widths(x, unicode);
                lo = lo.saturating_add(a);
                hi = match (hi, b) {
                    (Some(h), Some(b)) => h.checked_add(b),
                    _ => None,
                };
            }
            (lo, hi)
        }
        Node::Alt(v) => {
            let mut lo = u32::MAX;
            let mut hi = Some(0u32);
            for x in v {
                let (a, b) = widths(x, unicode);
                lo = lo.min(a);
                hi = match (hi, b) {
                    (Some(h), Some(b)) => Some(h.max(b)),
                    _ => None,
                };
            }
            (if v.is_empty() { 0 } else { lo }, hi)
        }
        Node::Repeat(x, min, max, _) => {
            let (a, b) = widths(x, unicode);
            let lo = a.saturating_mul(*min);
            let hi = match (b, *max) {
                (Some(_), u32::MAX) => None,
                (Some(b), m) => b.checked_mul(m),
                (None, _) => None,
            };
            (lo, if b == Some(0) { Some(0) } else { hi })
        }
    }
}

fn group_range(n: &Node, lo: &mut u32, hi: &mut u32) {
    match n {
        Node::Group(g, cap) => {
            if let Some(c) = cap {
                *lo = (*lo).min(*c);
                *hi = (*hi).max(*c);
            }
            group_range(g, lo, hi);
        }
        Node::Concat(v) | Node::Alt(v) => {
            for x in v {
                group_range(x, lo, hi);
            }
        }
        Node::Repeat(x, ..) | Node::Look(x, ..) => group_range(x, lo, hi),
        _ => {}
    }
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> Result<u32, String> {
        if self.insts.len() >= MAX_INSTS {
            return Err(String::from("regular expression too large"));
        }
        self.insts.push(i);
        Ok(self.insts.len() as u32 - 1)
    }

    fn patch_split(&mut self, at: u32, a: u32, b: u32) {
        self.insts[at as usize] = Inst::Split(a, b);
    }

    fn node(&mut self, n: &Node, names: &[(String, u32)], unicode: bool) -> Result<(), String> {
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                let c = if self.icase { canon(*c, unicode) } else { *c };
                self.emit(Inst::Char(c))?;
            }
            Node::Any => {
                self.emit(Inst::Any)?;
            }
            Node::Class(items, neg) => {
                self.classes.push(Class {
                    items: items.clone(),
                    negated: *neg,
                });
                self.emit(Inst::Class(self.classes.len() as u32 - 1))?;
            }
            Node::Bol => {
                self.emit(Inst::Bol)?;
            }
            Node::Eol => {
                self.emit(Inst::Eol)?;
            }
            Node::WordB(neg) => {
                self.emit(Inst::WordB(*neg))?;
            }
            Node::Group(g, cap) => match cap {
                Some(c) => {
                    self.emit(Inst::Save(c * 2))?;
                    self.node(g, names, unicode)?;
                    self.emit(Inst::Save(c * 2 + 1))?;
                }
                None => self.node(g, names, unicode)?,
            },
            Node::Backref(k) => {
                self.emit(Inst::Backref(*k))?;
            }
            Node::NamedBackref(name) => {
                let k = names
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|x| x.1)
                    .ok_or_else(|| String::from("Invalid named capture referenced"))?;
                self.emit(Inst::Backref(k))?;
            }
            Node::Concat(v) => {
                for x in v {
                    self.node(x, names, unicode)?;
                }
            }
            Node::Alt(v) => {
                // Split(a, next) a Jmp(end) Split(b, next) b Jmp(end) ... last
                let mut jumps = Vec::new();
                for (i, x) in v.iter().enumerate() {
                    if i + 1 < v.len() {
                        let sp = self.emit(Inst::Split(0, 0))?;
                        self.node(x, names, unicode)?;
                        jumps.push(self.emit(Inst::Jmp(0))?);
                        let next = self.insts.len() as u32;
                        self.patch_split(sp, sp + 1, next);
                    } else {
                        self.node(x, names, unicode)?;
                    }
                }
                let end = self.insts.len() as u32;
                for j in jumps {
                    self.insts[j as usize] = Inst::Jmp(end);
                }
            }
            Node::Repeat(x, min, max, greedy) => {
                self.repeat(x, *min, *max, *greedy, names, unicode)?
            }
            Node::Look(x, ahead, negate) => {
                let (minlen, maxlen) = widths(x, unicode);
                let at = self.emit(Inst::Look {
                    end: 0,
                    ahead: *ahead,
                    negate: *negate,
                    minlen,
                    maxlen: maxlen.unwrap_or(u32::MAX),
                })?;
                let reg = if *ahead { 0 } else { self.nregs };
                if !*ahead {
                    self.nregs += 1;
                }
                self.node(x, names, unicode)?;
                if !*ahead {
                    self.emit(Inst::AtReg(reg))?;
                }
                self.emit(Inst::Match)?;
                let end = self.insts.len() as u32;
                self.insts[at as usize] = Inst::Look {
                    end,
                    ahead: *ahead,
                    negate: *negate,
                    minlen,
                    maxlen: maxlen.unwrap_or(u32::MAX),
                };
            }
        }
        Ok(())
    }

    fn repeat(
        &mut self,
        x: &Node,
        min: u32,
        max: u32,
        greedy: bool,
        names: &[(String, u32)],
        unicode: bool,
    ) -> Result<(), String> {
        let (wmin, _) = widths(x, unicode);
        let may_be_empty = wmin == 0;
        let mut glo = u32::MAX;
        let mut ghi = 0;
        group_range(x, &mut glo, &mut ghi);
        let clear = if glo <= ghi { Some((glo, ghi)) } else { None };
        // Mandatory copies.
        let size_guess = 8usize;
        if (min as usize).saturating_mul(size_guess) > MAX_INSTS
            || (max != u32::MAX && (max as usize).saturating_mul(size_guess) > MAX_INSTS)
        {
            return Err(String::from("regular expression too large"));
        }
        for _ in 0..min {
            if let Some((a, b)) = clear {
                self.emit(Inst::ClearCaps(a, b))?;
            }
            self.node(x, names, unicode)?;
        }
        if max == u32::MAX {
            // Loop: L: Split(body, end) body Jmp(L)
            let reg = self.nregs;
            self.nregs += 1;
            let l = self.emit(Inst::Split(0, 0))?;
            if may_be_empty {
                self.emit(Inst::RepStart(reg))?;
            }
            if let Some((a, b)) = clear {
                self.emit(Inst::ClearCaps(a, b))?;
            }
            self.node(x, names, unicode)?;
            if may_be_empty {
                self.emit(Inst::RepCheck(reg))?;
            }
            self.emit(Inst::Jmp(l))?;
            let end = self.insts.len() as u32;
            if greedy {
                self.patch_split(l, l + 1, end);
            } else {
                self.patch_split(l, end, l + 1);
            }
        } else {
            // Optional copies: Split(body, end) body ... nested.
            let mut splits = Vec::new();
            for _ in min..max {
                let sp = self.emit(Inst::Split(0, 0))?;
                splits.push(sp);
                if let Some((a, b)) = clear {
                    self.emit(Inst::ClearCaps(a, b))?;
                }
                self.node(x, names, unicode)?;
            }
            let end = self.insts.len() as u32;
            for sp in splits {
                if greedy {
                    self.patch_split(sp, sp + 1, end);
                } else {
                    self.patch_split(sp, end, sp + 1);
                }
            }
        }
        Ok(())
    }
}

/// Canonicalize for ignoreCase: the simple uppercase mapping (non-unicode, as the spec
/// says, never mapping a non-ASCII char to ASCII) or simple case folding (unicode).
pub(crate) fn canon(c: u32, unicode: bool) -> u32 {
    if c < 128 {
        let b = c as u8;
        return if unicode {
            b.to_ascii_lowercase() as u32
        } else {
            b.to_ascii_uppercase() as u32
        };
    }
    let Some(ch) = char::from_u32(c) else {
        return c;
    };
    if unicode {
        let mut it = ch.to_lowercase();
        match (it.next(), it.next()) {
            (Some(l), None) => l as u32,
            _ => c,
        }
    } else {
        let mut it = ch.to_uppercase();
        match (it.next(), it.next()) {
            (Some(u), None) if (u as u32) >= 128 => u as u32,
            _ => c,
        }
    }
}

pub(crate) fn compile(pattern: &[u16], flags: &[u16]) -> Result<Program, String> {
    let fl = parse_flags(flags)?;
    let has_named = {
        let mut i = 0;
        let mut found = false;
        while i + 2 < pattern.len() {
            if pattern[i] == b'\\' as u16 {
                i += 2;
                continue;
            }
            if pattern[i] == b'(' as u16
                && pattern[i + 1] == b'?' as u16
                && pattern[i + 2] == b'<' as u16
            {
                let n = pattern.get(i + 3).copied();
                if n != Some(b'=' as u16) && n != Some(b'!' as u16) {
                    found = true;
                }
            }
            i += 1;
        }
        found
    };
    let mut p = Parser {
        s: pattern,
        i: 0,
        unicode: fl.unicode,
        ncaps: 0,
        names: Vec::new(),
        depth: 0,
        has_named,
    };
    let node = p.disjunction()?;
    if p.i < pattern.len() {
        return Err(String::from(if pattern[p.i] == b')' as u16 {
            "Unmatched ')'"
        } else {
            "Unexpected character"
        }));
    }
    let mut c = Compiler {
        insts: Vec::new(),
        classes: Vec::new(),
        nregs: 0,
        icase: fl.ignore_case,
    };
    c.emit(Inst::Save(0))?;
    c.node(&node, &p.names, fl.unicode)?;
    c.emit(Inst::Save(1))?;
    c.emit(Inst::Match)?;
    Ok(Program {
        insts: c.insts,
        classes: c.classes,
        ncaps: p.ncaps,
        nregs: c.nregs.max(1),
        names: p.names,
        flags: fl,
    })
}

/// Checks a literal's syntax at compile time.
pub(crate) fn validate(pattern: &[u16], flags: &[u16]) -> Result<(), String> {
    compile(pattern, flags).map(|_| ())
}

// ---- matching ----

enum Undo {
    /// Resume here.
    Branch(u32, usize),
    /// Restore a capture slot.
    Cap(u32, usize),
    /// Restore a register.
    Reg(u32, usize),
}

pub(crate) enum MatchResult {
    /// Capture positions: 2 per group (usize::MAX = unset), group 0 the whole match.
    Found(Vec<usize>),
    NotFound,
    /// The step limit was hit.
    TooComplex,
}

struct Matcher<'a> {
    prog: &'a Program,
    s: &'a [u16],
    steps: u64,
    limit: u64,
}

impl Matcher<'_> {
    #[inline]
    fn read(&self, pos: usize) -> Option<(u32, usize)> {
        let c = *self.s.get(pos)? as u32;
        if self.prog.flags.unicode
            && (0xD800..0xDC00).contains(&c)
            && let Some(&d) = self.s.get(pos + 1)
            && (0xDC00..0xE000).contains(&d)
        {
            return Some((0x10000 + ((c - 0xD800) << 10) + (d as u32 - 0xDC00), 2));
        }
        Some((c, 1))
    }

    fn is_word(&self, pos: usize) -> bool {
        match self.s.get(pos) {
            Some(&c) => {
                let c = c as u32;
                let w = c < 128 && ((c as u8).is_ascii_alphanumeric() || c == b'_' as u32);
                if !w && self.prog.flags.ignore_case && self.prog.flags.unicode {
                    // ſ (U+017F) and K (U+212A) fold to word characters.
                    return c == 0x17F || c == 0x212A;
                }
                w
            }
            None => false,
        }
    }

    fn class_has(&self, cls: &Class, c: u32) -> bool {
        let icase = self.prog.flags.ignore_case;
        let unicode = self.prog.flags.unicode;
        let test = |c: u32| -> bool {
            cls.items.iter().any(|it| match it {
                ClassItem::Range(a, b) => *a <= c && c <= *b,
                ClassItem::Digit(n) => ((0x30..=0x39).contains(&c)) != *n,
                ClassItem::Word(n) => {
                    let w = c < 128 && ((c as u8).is_ascii_alphanumeric() || c == b'_' as u32);
                    w != *n
                }
                ClassItem::Space(n) => is_re_space(c) != *n,
                ClassItem::Prop(p, n) => prop_has(*p, c) != *n,
            })
        };
        let mut hit = test(c);
        if !hit && icase {
            if let Some(ch) = char::from_u32(c) {
                for v in ch.to_lowercase().chain(ch.to_uppercase()) {
                    if test(v as u32) {
                        hit = true;
                        break;
                    }
                }
            }
            if !hit {
                let k = canon(c, unicode);
                hit = test(k);
            }
        }
        hit != cls.negated
    }

    /// Runs the program from `pc` at `pos`; on success the captures are in `caps`.
    fn run(
        &mut self,
        pc0: u32,
        pos0: usize,
        caps: &mut [usize],
        regs: &mut [usize],
    ) -> Option<bool> {
        let insts = &self.prog.insts;
        let mut stack: Vec<Undo> = Vec::new();
        let mut pc = pc0 as usize;
        let mut pos = pos0;
        let icase = self.prog.flags.ignore_case;
        let unicode = self.prog.flags.unicode;
        loop {
            self.steps += 1;
            if self.steps > self.limit {
                return None;
            }
            let mut fail = false;
            match insts.get(pc).copied().unwrap_or(Inst::Match) {
                Inst::Match => return Some(true),
                Inst::Char(c) => match self.read(pos) {
                    Some((d, n)) if d == c || (icase && canon(d, unicode) == c) => {
                        pos += n;
                        pc += 1;
                    }
                    _ => fail = true,
                },
                Inst::Any => match self.read(pos) {
                    Some((d, n))
                        if self.prog.flags.dot_all || !matches!(d, 10 | 13 | 0x2028 | 0x2029) =>
                    {
                        pos += n;
                        pc += 1;
                    }
                    _ => fail = true,
                },
                Inst::Class(k) => {
                    let cls = &self.prog.classes[k as usize];
                    match self.read(pos) {
                        Some((d, n)) if self.class_has(cls, d) => {
                            pos += n;
                            pc += 1;
                        }
                        _ => fail = true,
                    }
                }
                Inst::Bol => {
                    if pos == 0
                        || (self.prog.flags.multiline
                            && matches!(self.s[pos - 1], 10 | 13 | 0x2028 | 0x2029))
                    {
                        pc += 1;
                    } else {
                        fail = true;
                    }
                }
                Inst::Eol => {
                    if pos == self.s.len()
                        || (self.prog.flags.multiline
                            && matches!(self.s[pos], 10 | 13 | 0x2028 | 0x2029))
                    {
                        pc += 1;
                    } else {
                        fail = true;
                    }
                }
                Inst::WordB(neg) => {
                    let a = pos > 0 && self.is_word(pos - 1);
                    let b = self.is_word(pos);
                    if (a != b) != neg {
                        pc += 1;
                    } else {
                        fail = true;
                    }
                }
                Inst::Save(slot) => {
                    let s = slot as usize;
                    if s < caps.len() {
                        stack.push(Undo::Cap(slot, caps[s]));
                        caps[s] = pos;
                    }
                    pc += 1;
                }
                Inst::ClearCaps(lo, hi) => {
                    for g in lo..=hi {
                        for s in [g as usize * 2, g as usize * 2 + 1] {
                            if s < caps.len() && caps[s] != usize::MAX {
                                stack.push(Undo::Cap(s as u32, caps[s]));
                                caps[s] = usize::MAX;
                            }
                        }
                    }
                    pc += 1;
                }
                Inst::Split(a, b) => {
                    stack.push(Undo::Branch(b, pos));
                    pc = a as usize;
                }
                Inst::Jmp(t) => pc = t as usize,
                Inst::RepStart(r) => {
                    let r = r as usize;
                    if r < regs.len() {
                        stack.push(Undo::Reg(r as u32, regs[r]));
                        regs[r] = pos;
                    }
                    pc += 1;
                }
                Inst::RepCheck(r) => {
                    if regs.get(r as usize).copied() == Some(pos) {
                        fail = true;
                    } else {
                        pc += 1;
                    }
                }
                Inst::AtReg(r) => {
                    if regs.get(r as usize).copied() == Some(pos) {
                        pc += 1;
                    } else {
                        fail = true;
                    }
                }
                Inst::Backref(k) => {
                    let (a, b) = (
                        caps.get(k as usize * 2).copied().unwrap_or(usize::MAX),
                        caps.get(k as usize * 2 + 1).copied().unwrap_or(usize::MAX),
                    );
                    if a == usize::MAX || b == usize::MAX || b < a {
                        pc += 1;
                    } else {
                        let len = b - a;
                        let ok = pos + len <= self.s.len()
                            && (0..len).all(|i| {
                                let x = self.s[a + i] as u32;
                                let y = self.s[pos + i] as u32;
                                x == y || (icase && canon(x, unicode) == canon(y, unicode))
                            });
                        if ok {
                            pos += len;
                            pc += 1;
                        } else {
                            fail = true;
                        }
                    }
                }
                Inst::Look {
                    end,
                    ahead,
                    negate,
                    minlen,
                    maxlen,
                } => {
                    let mut sub = caps.to_vec();
                    let mut subregs = regs.to_vec();
                    let found = if ahead {
                        self.run(pc as u32 + 1, pos, &mut sub, &mut subregs)?
                    } else {
                        // Lookbehind: find a start s <= pos from which the sub-pattern ends at pos.
                        let reg = match insts.get(end as usize - 2) {
                            Some(Inst::AtReg(r)) => *r as usize,
                            _ => 0,
                        };
                        if reg < subregs.len() {
                            subregs[reg] = pos;
                        }
                        let lo = if maxlen == u32::MAX {
                            0
                        } else {
                            pos.saturating_sub(maxlen as usize)
                        };
                        let hi = pos.saturating_sub(minlen as usize);
                        let mut f = false;
                        let mut st = hi as isize;
                        while st >= lo as isize {
                            let mut c2 = caps.to_vec();
                            let mut r2 = subregs.clone();
                            if self.run(pc as u32 + 1, st as usize, &mut c2, &mut r2)? {
                                sub = c2;
                                f = true;
                                break;
                            }
                            st -= 1;
                        }
                        f
                    };
                    if found != negate {
                        if found && !negate {
                            // Captures from a positive lookaround persist.
                            for (i, v) in sub.iter().enumerate() {
                                if caps[i] != *v {
                                    stack.push(Undo::Cap(i as u32, caps[i]));
                                    caps[i] = *v;
                                }
                            }
                        }
                        pc = end as usize;
                    } else {
                        fail = true;
                    }
                }
            }
            if fail {
                loop {
                    match stack.pop() {
                        None => return Some(false),
                        Some(Undo::Cap(s, v)) => caps[s as usize] = v,
                        Some(Undo::Reg(r, v)) => regs[r as usize] = v,
                        Some(Undo::Branch(t, p)) => {
                            pc = t as usize;
                            pos = p;
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn is_re_space(c: u32) -> bool {
    matches!(c, 9..=13 | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

impl Program {
    /// Tries to match at exactly `start` (sticky) or at the first position from `start`.
    pub(crate) fn exec(
        &self,
        s: &[u16],
        start: usize,
        sticky: bool,
        limit: u64,
    ) -> (MatchResult, u64) {
        let ncap_slots = (self.ncaps as usize + 1) * 2;
        let mut m = Matcher {
            prog: self,
            s,
            steps: 0,
            limit,
        };
        let mut pos = start;
        // A quick filter: the first instruction after Save(0) is a literal char.
        let first = match self.insts.get(1) {
            Some(Inst::Char(c)) if !self.flags.ignore_case && *c < 0x10000 => Some(*c as u16),
            _ => None,
        };
        loop {
            if pos > s.len() {
                return (MatchResult::NotFound, m.steps);
            }
            if let Some(fc) = first
                && !sticky
            {
                match s[pos..].iter().position(|&c| c == fc) {
                    Some(off) => pos += off,
                    None => return (MatchResult::NotFound, m.steps),
                }
            }
            let mut caps = alloc::vec![usize::MAX; ncap_slots];
            let mut regs = alloc::vec![usize::MAX; self.nregs as usize];
            match m.run(0, pos, &mut caps, &mut regs) {
                None => return (MatchResult::TooComplex, m.steps),
                Some(true) => return (MatchResult::Found(caps), m.steps),
                Some(false) => {}
            }
            if sticky {
                return (MatchResult::NotFound, m.steps);
            }
            // Advance by one code point in unicode mode.
            if self.flags.unicode
                && pos < s.len()
                && (0xD800..0xDC00).contains(&s[pos])
                && pos + 1 < s.len()
                && (0xDC00..0xE000).contains(&s[pos + 1])
            {
                pos += 2;
            } else {
                pos += 1;
            }
        }
    }
}

/// The per-object state of a RegExp.
pub(crate) struct RegExpData {
    pub source: StrId,
    pub flags: StrId,
    pub prog: Rc<Program>,
}

impl RegExpData {
    pub fn trace(&self, t: &mut Tracer) {
        t.str(self.source);
        t.str(self.flags);
    }
    pub fn bytes(&self) -> usize {
        64 + self.prog.insts.len() * 12
    }
}

#[allow(dead_code)]
fn _v(_: Val) {}

#[cfg(test)]
mod tests {
    use super::*;
    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    fn m(p: &str, f: &str, s: &str) -> Option<(usize, usize)> {
        let prog = compile(&u(p), &u(f)).unwrap();
        match prog.exec(&u(s), 0, false, STEP_LIMIT).0 {
            MatchResult::Found(c) => Some((c[0], c[1])),
            _ => None,
        }
    }
    #[test]
    fn basics() {
        assert_eq!(m("abc", "", "xxabcx"), Some((2, 5)));
        assert_eq!(m("a+", "", "baaa"), Some((1, 4)));
        assert_eq!(m("a+?", "", "baaa"), Some((1, 2)));
        assert_eq!(m("^a$", "m", "b\na\nc"), Some((2, 3)));
        assert_eq!(m("[a-c]+", "", "xxbcay"), Some((2, 5)));
        assert_eq!(m("\\d{2,3}", "", "a12345"), Some((1, 4)));
        assert_eq!(m("(?<=\\$)\\d+", "", "cost $42"), Some((6, 8)));
        assert_eq!(m("(?<!\\$)\\b\\d+", "", "$42 17"), Some((4, 6)));
        assert_eq!(m("a(?=b)", "", "acab"), Some((2, 3)));
        assert_eq!(m("(a)\\1", "", "xaa"), Some((1, 3)));
        assert_eq!(m("ABC", "i", "xabc"), Some((1, 4)));
        assert_eq!(m("(a*)*b", "", "aaab"), Some((0, 4)));
        assert_eq!(m("\\bfoo\\b", "", "a foo b"), Some((2, 5)));
        assert_eq!(m("x{", "", "ax{"), Some((1, 3)));
        assert_eq!(m("\\p{Lu}+", "u", "abCDe"), Some((2, 4)));
        assert_eq!(m(".", "su", "😀"), Some((0, 2)));
        assert!(compile(&u("(?<n>a)\\k<n>"), &u("")).is_ok());
        assert!(compile(&u("a**"), &u("")).is_err());
        assert!(compile(&u("[b-a]"), &u("")).is_err());
        // Catastrophic backtracking stops.
        let prog = compile(&u("(a+)+b"), &u("")).unwrap();
        let s = u(&"a".repeat(40));
        assert!(matches!(
            prog.exec(&s, 0, false, 1_000_000).0,
            MatchResult::TooComplex
        ));
    }
}
