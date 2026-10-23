//! The tokenizer. It works on the UTF-8 source bytes and is driven by the parser, which says
//! when a `/` starts a regular expression and when a `}` resumes a template literal. Every
//! token records whether a line terminator came before it (for automatic semicolon insertion
//! and the restricted productions).

use alloc::string::String;
use alloc::vec::Vec;

use crate::atoms::{Atom, Atoms};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum P {
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    Ellipsis,
    Semi,
    Comma,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    Inc,
    Dec,
    Shl,
    Shr,
    UShr,
    Amp,
    Pipe,
    Caret,
    Bang,
    Tilde,
    AndAnd,
    OrOr,
    Nullish,
    Question,
    QDot,
    Colon,
    Assign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    StarStarEq,
    ShlEq,
    ShrEq,
    UShrEq,
    AmpEq,
    PipeEq,
    CaretEq,
    AndAndEq,
    OrOrEq,
    NullishEq,
    Arrow,
    Hash,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Eof,
    /// An identifier or keyword; `escaped` if it was written with \u escapes (then it can't
    /// be a keyword).
    Name(Atom, bool),
    /// `#name` (a private name: reported as unsupported by the parser).
    Private(Atom),
    P(P),
    Num(f64),
    /// A string literal's value (UTF-16); `legacy_octal` if it used an octal escape.
    Str(Vec<u16>, bool),
    /// A template chunk: cooked (None if it has an invalid escape), raw, and whether it ends
    /// the template (`) rather than opening a substitution (${).
    Template(Option<Vec<u16>>, Vec<u16>, bool),
    Regex(Vec<u16>, Vec<u16>),
    BigInt,
}

#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub tok: Tok,
    pub start: u32,
    pub end: u32,
    pub nl_before: bool,
}

pub(crate) struct LexError {
    pub msg: String,
    pub pos: u32,
}

pub(crate) struct Lexer<'a> {
    pub src: &'a [u8],
    pub pos: usize,
    /// True when only whitespace/comments were seen since the last line terminator (for
    /// `-->` comments).
    line_start: bool,
}

fn is_id_start_ascii(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'$' || c == b'_'
}
fn is_id_part_ascii(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'$' || c == b'_'
}

fn is_id_start(c: char) -> bool {
    if c.is_ascii() {
        return is_id_start_ascii(c as u8);
    }
    c.is_alphabetic()
}
fn is_id_part(c: char) -> bool {
    if c.is_ascii() {
        return is_id_part_ascii(c as u8);
    }
    c.is_alphanumeric() || c == '\u{200C}' || c == '\u{200D}' || is_combining(c)
}
fn is_combining(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

pub(crate) fn is_line_term(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{0B}' | '\u{0C}' | ' ' | '\u{A0}' | '\u{FEFF}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8]) -> Lexer<'a> {
        let mut l = Lexer {
            src,
            pos: 0,
            line_start: true,
        };
        // Hashbang comment.
        if src.starts_with(b"#!") {
            while l.pos < src.len() && src[l.pos] != b'\n' && src[l.pos] != b'\r' {
                l.pos += 1;
            }
        }
        l
    }

    pub fn state(&self) -> (usize, bool) {
        (self.pos, self.line_start)
    }

    pub fn set_state(&mut self, st: (usize, bool)) {
        self.pos = st.0;
        self.line_start = st.1;
    }

    fn peek_char(&self) -> Option<char> {
        let b = *self.src.get(self.pos)?;
        if b < 0x80 {
            return Some(b as char);
        }
        decode_utf8(&self.src[self.pos..]).map(|(c, _)| c)
    }

    fn peek_char_at(&self, at: usize) -> Option<(char, usize)> {
        let b = *self.src.get(at)?;
        if b < 0x80 {
            return Some((b as char, 1));
        }
        decode_utf8(&self.src[at..])
    }

    fn bump_char(&mut self) -> Option<char> {
        let (c, n) = self.peek_char_at(self.pos)?;
        self.pos += n;
        Some(c)
    }

    #[inline]
    fn byte(&self, off: usize) -> u8 {
        *self.src.get(self.pos + off).unwrap_or(&0)
    }

    fn err<T>(&self, msg: &str, pos: usize) -> Result<T, LexError> {
        Err(LexError {
            msg: String::from(msg),
            pos: pos as u32,
        })
    }

    /// Skips whitespace and comments; returns whether a line terminator was crossed.
    fn skip_trivia(&mut self) -> Result<bool, LexError> {
        let mut nl = false;
        loop {
            let Some(b) = self.src.get(self.pos).copied() else {
                return Ok(nl);
            };
            match b {
                b' ' | b'\t' | 0x0b | 0x0c => self.pos += 1,
                b'\n' | b'\r' => {
                    self.pos += 1;
                    nl = true;
                    self.line_start = true;
                }
                b'/' if self.byte(1) == b'/' => self.skip_line(),
                b'/' if self.byte(1) == b'*' => {
                    let start = self.pos;
                    self.pos += 2;
                    loop {
                        if self.pos >= self.src.len() {
                            return self.err("unterminated comment", start);
                        }
                        let c = self.src[self.pos];
                        if c == b'*' && self.byte(1) == b'/' {
                            self.pos += 2;
                            break;
                        }
                        if c == b'\n' || c == b'\r' {
                            nl = true;
                            self.line_start = true;
                            self.pos += 1;
                        } else if c >= 0x80 {
                            match self.bump_char() {
                                Some(ch) if is_line_term(ch) => {
                                    nl = true;
                                    self.line_start = true;
                                }
                                Some(_) => {}
                                None => self.pos += 1,
                            }
                        } else {
                            self.pos += 1;
                        }
                    }
                }
                // HTML-like comments (Annex B): `<!--` anywhere, `-->` at the start of a line.
                b'<' if self.src[self.pos..].starts_with(b"<!--") => self.skip_line(),
                b'-' if self.line_start && self.src[self.pos..].starts_with(b"-->") => {
                    self.skip_line()
                }
                0x80.. => match self.peek_char() {
                    Some(c) if is_space(c) => {
                        self.bump_char();
                    }
                    Some(c) if is_line_term(c) => {
                        self.bump_char();
                        nl = true;
                        self.line_start = true;
                    }
                    _ => return Ok(nl),
                },
                _ => return Ok(nl),
            }
        }
    }

    fn skip_line(&mut self) {
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == b'\n' || c == b'\r' {
                return;
            }
            if c >= 0x80
                && let Some((ch, n)) = self.peek_char_at(self.pos)
            {
                if is_line_term(ch) {
                    return;
                }
                self.pos += n;
                continue;
            }
            self.pos += 1;
        }
    }

    /// The next token (a `/` is division; the parser rescans it as a regex when it wants one).
    pub fn next(&mut self, atoms: &mut Atoms) -> Result<Token, LexError> {
        let nl = self.skip_trivia()?;
        let start = self.pos;
        let tok = self.scan(atoms)?;
        self.line_start = false;
        Ok(Token {
            tok,
            start: start as u32,
            end: self.pos as u32,
            nl_before: nl,
        })
    }

    fn scan(&mut self, atoms: &mut Atoms) -> Result<Tok, LexError> {
        let start = self.pos;
        let Some(b) = self.src.get(self.pos).copied() else {
            return Ok(Tok::Eof);
        };
        if is_id_start_ascii(b) {
            // Fast path: a plain ASCII identifier.
            let mut e = self.pos + 1;
            while e < self.src.len() && is_id_part_ascii(self.src[e]) {
                e += 1;
            }
            if e >= self.src.len() || (self.src[e] < 0x80 && self.src[e] != b'\\') {
                let s = &self.src[self.pos..e];
                self.pos = e;
                // SAFETY-free: ASCII is valid UTF-8.
                let st = core::str::from_utf8(s).unwrap_or("");
                return Ok(Tok::Name(atoms.intern(st), false));
            }
            return self.scan_ident(atoms);
        }
        if b.is_ascii_digit() || (b == b'.' && self.byte(1).is_ascii_digit()) {
            return self.scan_number();
        }
        if b == b'"' || b == b'\'' {
            return self.scan_string(b);
        }
        if b == b'`' {
            self.pos += 1;
            return self.scan_template();
        }
        if b == b'\\' || b >= 0x80 {
            if b >= 0x80 {
                match self.peek_char() {
                    Some(c) if is_id_start(c) => return self.scan_ident(atoms),
                    _ => return self.err("unexpected character", start),
                }
            }
            return self.scan_ident(atoms);
        }
        if b == b'#' {
            self.pos += 1;
            match self.peek_char() {
                Some(c) if is_id_start(c) || c == '\\' => {
                    if let Tok::Name(a, _) = self.scan_ident(atoms)? {
                        return Ok(Tok::Private(a));
                    }
                    return self.err("bad private name", start);
                }
                _ => return Ok(Tok::P(P::Hash)),
            }
        }
        self.pos += 1;
        let c1 = self.byte(0);
        let c2 = self.byte(1);
        let c3 = self.byte(2);
        let p = match b {
            b'{' => P::LBrace,
            b'}' => P::RBrace,
            b'(' => P::LParen,
            b')' => P::RParen,
            b'[' => P::LBracket,
            b']' => P::RBracket,
            b';' => P::Semi,
            b',' => P::Comma,
            b':' => P::Colon,
            b'~' => P::Tilde,
            b'.' => {
                if c1 == b'.' && c2 == b'.' {
                    self.pos += 2;
                    P::Ellipsis
                } else {
                    P::Dot
                }
            }
            b'<' => match (c1, c2) {
                (b'<', b'=') => {
                    self.pos += 2;
                    P::ShlEq
                }
                (b'<', _) => {
                    self.pos += 1;
                    P::Shl
                }
                (b'=', _) => {
                    self.pos += 1;
                    P::Le
                }
                _ => P::Lt,
            },
            b'>' => match (c1, c2, c3) {
                (b'>', b'>', b'=') => {
                    self.pos += 3;
                    P::UShrEq
                }
                (b'>', b'>', _) => {
                    self.pos += 2;
                    P::UShr
                }
                (b'>', b'=', _) => {
                    self.pos += 2;
                    P::ShrEq
                }
                (b'>', _, _) => {
                    self.pos += 1;
                    P::Shr
                }
                (b'=', _, _) => {
                    self.pos += 1;
                    P::Ge
                }
                _ => P::Gt,
            },
            b'=' => match (c1, c2) {
                (b'=', b'=') => {
                    self.pos += 2;
                    P::StrictEq
                }
                (b'=', _) => {
                    self.pos += 1;
                    P::Eq
                }
                (b'>', _) => {
                    self.pos += 1;
                    P::Arrow
                }
                _ => P::Assign,
            },
            b'!' => match (c1, c2) {
                (b'=', b'=') => {
                    self.pos += 2;
                    P::StrictNe
                }
                (b'=', _) => {
                    self.pos += 1;
                    P::Ne
                }
                _ => P::Bang,
            },
            b'+' => match c1 {
                b'+' => {
                    self.pos += 1;
                    P::Inc
                }
                b'=' => {
                    self.pos += 1;
                    P::PlusEq
                }
                _ => P::Plus,
            },
            b'-' => match c1 {
                b'-' => {
                    self.pos += 1;
                    P::Dec
                }
                b'=' => {
                    self.pos += 1;
                    P::MinusEq
                }
                _ => P::Minus,
            },
            b'*' => match (c1, c2) {
                (b'*', b'=') => {
                    self.pos += 2;
                    P::StarStarEq
                }
                (b'*', _) => {
                    self.pos += 1;
                    P::StarStar
                }
                (b'=', _) => {
                    self.pos += 1;
                    P::StarEq
                }
                _ => P::Star,
            },
            b'/' => match c1 {
                b'=' => {
                    self.pos += 1;
                    P::SlashEq
                }
                _ => P::Slash,
            },
            b'%' => match c1 {
                b'=' => {
                    self.pos += 1;
                    P::PercentEq
                }
                _ => P::Percent,
            },
            b'&' => match (c1, c2) {
                (b'&', b'=') => {
                    self.pos += 2;
                    P::AndAndEq
                }
                (b'&', _) => {
                    self.pos += 1;
                    P::AndAnd
                }
                (b'=', _) => {
                    self.pos += 1;
                    P::AmpEq
                }
                _ => P::Amp,
            },
            b'|' => match (c1, c2) {
                (b'|', b'=') => {
                    self.pos += 2;
                    P::OrOrEq
                }
                (b'|', _) => {
                    self.pos += 1;
                    P::OrOr
                }
                (b'=', _) => {
                    self.pos += 1;
                    P::PipeEq
                }
                _ => P::Pipe,
            },
            b'^' => match c1 {
                b'=' => {
                    self.pos += 1;
                    P::CaretEq
                }
                _ => P::Caret,
            },
            b'?' => match (c1, c2) {
                (b'?', b'=') => {
                    self.pos += 2;
                    P::NullishEq
                }
                (b'?', _) => {
                    self.pos += 1;
                    P::Nullish
                }
                (b'.', d) if !d.is_ascii_digit() => {
                    self.pos += 1;
                    P::QDot
                }
                _ => P::Question,
            },
            _ => return self.err("unexpected character", start),
        };
        Ok(Tok::P(p))
    }

    fn scan_ident(&mut self, atoms: &mut Atoms) -> Result<Tok, LexError> {
        let start = self.pos;
        let mut s = String::new();
        let mut escaped = false;
        let mut first = true;
        while let Some((c, n)) = self.peek_char_at(self.pos) {
            if c == '\\' {
                if self.byte(1) != b'u' {
                    return self.err("invalid escape in identifier", self.pos);
                }
                self.pos += 2;
                let cp = self.scan_unicode_escape_body()?;
                let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                if (first && !is_id_start(ch)) || (!first && !is_id_part(ch)) {
                    return self.err("invalid identifier escape", start);
                }
                s.push(ch);
                escaped = true;
            } else if (first && is_id_start(c)) || (!first && is_id_part(c)) {
                s.push(c);
                self.pos += n;
            } else {
                break;
            }
            first = false;
        }
        if s.is_empty() {
            return self.err("unexpected character", start);
        }
        Ok(Tok::Name(atoms.intern(&s), escaped))
    }

    /// After `\u`: `XXXX` or `{X...}`.
    fn scan_unicode_escape_body(&mut self) -> Result<u32, LexError> {
        let start = self.pos;
        if self.byte(0) == b'{' {
            self.pos += 1;
            let mut v: u32 = 0;
            let mut n = 0;
            while self.byte(0) != b'}' {
                let d = (self.byte(0) as char).to_digit(16);
                match d {
                    Some(d) => {
                        v = v.saturating_mul(16).saturating_add(d);
                        n += 1;
                        self.pos += 1;
                    }
                    None => return self.err("invalid Unicode escape", start),
                }
                if v > 0x10FFFF {
                    return self.err("Unicode escape out of range", start);
                }
            }
            self.pos += 1;
            if n == 0 {
                return self.err("invalid Unicode escape", start);
            }
            return Ok(v);
        }
        let mut v = 0;
        for _ in 0..4 {
            match (self.byte(0) as char).to_digit(16) {
                Some(d) => {
                    v = v * 16 + d;
                    self.pos += 1;
                }
                None => return self.err("invalid Unicode escape", start),
            }
        }
        Ok(v)
    }

    fn scan_number(&mut self) -> Result<Tok, LexError> {
        let start = self.pos;
        let b0 = self.byte(0);
        let b1 = self.byte(1) | 0x20;
        if b0 == b'0' && (b1 == b'x' || b1 == b'o' || b1 == b'b') {
            let radix = match b1 {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            self.pos += 2;
            let mut v = 0.0f64;
            let mut n = 0;
            loop {
                let c = self.byte(0);
                if c == b'_' && n > 0 && (self.byte(1) as char).is_digit(radix) {
                    self.pos += 1;
                    continue;
                }
                match (c as char).to_digit(radix) {
                    Some(d) => {
                        v = v * radix as f64 + d as f64;
                        n += 1;
                        self.pos += 1;
                    }
                    None => break,
                }
            }
            if n == 0 {
                return self.err("invalid number", start);
            }
            if self.byte(0) == b'n' {
                self.pos += 1;
                return Ok(Tok::BigInt);
            }
            self.check_after_number(start)?;
            return Ok(Tok::Num(v));
        }
        // Legacy octal (sloppy): 0 followed by octal digits only.
        if b0 == b'0' && self.byte(1).is_ascii_digit() {
            let mut e = self.pos + 1;
            let mut octal = true;
            while e < self.src.len() && self.src[e].is_ascii_digit() {
                if self.src[e] >= b'8' {
                    octal = false;
                }
                e += 1;
            }
            if octal {
                let mut v = 0.0f64;
                for &c in &self.src[self.pos + 1..e] {
                    v = v * 8.0 + (c - b'0') as f64;
                }
                self.pos = e;
                self.check_after_number(start)?;
                return Ok(Tok::Num(v));
            }
        }
        let mut s = String::new();
        let digits = |l: &mut Lexer, s: &mut String| {
            let mut any = false;
            loop {
                let c = l.byte(0);
                if c.is_ascii_digit() {
                    s.push(c as char);
                    l.pos += 1;
                    any = true;
                } else if c == b'_' && any && l.byte(1).is_ascii_digit() {
                    l.pos += 1;
                } else {
                    break;
                }
            }
            any
        };
        digits(self, &mut s);
        if self.byte(0) == b'n' {
            self.pos += 1;
            return Ok(Tok::BigInt);
        }
        if self.byte(0) == b'.' {
            self.pos += 1;
            s.push('.');
            digits(self, &mut s);
        }
        if self.byte(0) | 0x20 == b'e' {
            let save = self.pos;
            self.pos += 1;
            let mut e = String::from("e");
            if self.byte(0) == b'+' || self.byte(0) == b'-' {
                e.push(self.byte(0) as char);
                self.pos += 1;
            }
            if digits(self, &mut e) {
                s.push_str(&e);
            } else {
                self.pos = save;
                return self.err("invalid number", start);
            }
        }
        if s.starts_with('.') {
            s.insert(0, '0');
        }
        if s.ends_with('.') {
            s.push('0');
        }
        if s.contains(".e") {
            s = s.replace(".e", ".0e");
        }
        self.check_after_number(start)?;
        let v: f64 = s.parse().unwrap_or(f64::NAN);
        Ok(Tok::Num(v))
    }

    fn check_after_number(&self, start: usize) -> Result<(), LexError> {
        if let Some(c) = self.peek_char()
            && (is_id_start(c) || c.is_ascii_digit() || c == '\\')
        {
            return self.err("identifier starts immediately after numeric literal", start);
        }
        Ok(())
    }

    fn scan_string(&mut self, q: u8) -> Result<Tok, LexError> {
        let start = self.pos;
        self.pos += 1;
        let mut out: Vec<u16> = Vec::new();
        let mut legacy = false;
        loop {
            let Some(b) = self.src.get(self.pos).copied() else {
                return self.err("unterminated string literal", start);
            };
            if b == q {
                self.pos += 1;
                break;
            }
            match b {
                b'\\' => {
                    self.pos += 1;
                    if self.escape(&mut out, false, &mut legacy)?.is_none() {
                        return self.err("invalid escape sequence", self.pos);
                    }
                }
                b'\n' | b'\r' => return self.err("unterminated string literal", start),
                0x80.. => {
                    let c = self.bump_char().unwrap_or('\u{FFFD}');
                    let mut buf = [0u16; 2];
                    out.extend_from_slice(c.encode_utf16(&mut buf));
                }
                _ => {
                    out.push(b as u16);
                    self.pos += 1;
                }
            }
        }
        Ok(Tok::Str(out, legacy))
    }

    /// One escape after the backslash. Ok(None) means invalid (templates keep going with a
    /// cooked value of undefined).
    fn escape(
        &mut self,
        out: &mut Vec<u16>,
        template: bool,
        legacy: &mut bool,
    ) -> Result<Option<()>, LexError> {
        let Some(c) = self.bump_char() else {
            return self.err("unterminated escape", self.pos);
        };
        match c {
            'n' => out.push(10),
            't' => out.push(9),
            'r' => out.push(13),
            'b' => out.push(8),
            'f' => out.push(12),
            'v' => out.push(11),
            '0' if !self.byte(0).is_ascii_digit() => out.push(0),
            '0'..='7' => {
                if template {
                    return Ok(None);
                }
                *legacy = true;
                let mut v = c as u32 - '0' as u32;
                let max_len = if c <= '3' { 2 } else { 1 };
                for _ in 0..max_len {
                    let d = self.byte(0);
                    if (b'0'..=b'7').contains(&d) {
                        v = v * 8 + (d - b'0') as u32;
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                out.push(v as u16);
            }
            '8' | '9' => {
                if template {
                    return Ok(None);
                }
                out.push(c as u16);
            }
            'x' => {
                let h1 = (self.byte(0) as char).to_digit(16);
                let h2 = (self.byte(1) as char).to_digit(16);
                match (h1, h2) {
                    (Some(a), Some(b)) => {
                        self.pos += 2;
                        out.push((a * 16 + b) as u16);
                    }
                    _ => return Ok(None),
                }
            }
            'u' => {
                let save = self.pos;
                match self.scan_unicode_escape_body() {
                    Ok(cp) => push_cp(out, cp),
                    Err(e) => {
                        if template {
                            self.pos = save;
                            return Ok(None);
                        }
                        return Err(e);
                    }
                }
            }
            '\r' => {
                if self.byte(0) == b'\n' {
                    self.pos += 1;
                }
            }
            '\n' | '\u{2028}' | '\u{2029}' => {}
            c => {
                let mut buf = [0u16; 2];
                out.extend_from_slice(c.encode_utf16(&mut buf));
            }
        }
        Ok(Some(()))
    }

    /// A template chunk, after '`' or after the '}' that closes a substitution.
    pub fn scan_template(&mut self) -> Result<Tok, LexError> {
        let start = self.pos;
        let mut cooked: Option<Vec<u16>> = Some(Vec::new());
        let mut raw: Vec<u16> = Vec::new();
        loop {
            let Some(b) = self.src.get(self.pos).copied() else {
                return self.err("unterminated template literal", start);
            };
            match b {
                b'`' => {
                    self.pos += 1;
                    return Ok(Tok::Template(cooked, raw, true));
                }
                b'$' if self.byte(1) == b'{' => {
                    self.pos += 2;
                    return Ok(Tok::Template(cooked, raw, false));
                }
                b'\\' => {
                    let esc_start = self.pos;
                    self.pos += 1;
                    let mut tmp = Vec::new();
                    let mut legacy = false;
                    let ok = self.escape(&mut tmp, true, &mut legacy)?;
                    // Raw: the source text, with CR/CRLF normalised to LF.
                    let rs = &self.src[esc_start..self.pos];
                    let rs = core::str::from_utf8(rs).unwrap_or("");
                    push_raw(&mut raw, rs);
                    match (ok, &mut cooked) {
                        (Some(()), Some(c)) => c.extend_from_slice(&tmp),
                        _ => cooked = None,
                    }
                }
                b'\r' => {
                    self.pos += 1;
                    if self.byte(0) == b'\n' {
                        self.pos += 1;
                    }
                    raw.push(10);
                    if let Some(c) = &mut cooked {
                        c.push(10);
                    }
                }
                0x80.. => {
                    let c = self.bump_char().unwrap_or('\u{FFFD}');
                    let mut buf = [0u16; 2];
                    let u = c.encode_utf16(&mut buf);
                    raw.extend_from_slice(u);
                    if let Some(cv) = &mut cooked {
                        cv.extend_from_slice(u);
                    }
                }
                _ => {
                    self.pos += 1;
                    raw.push(b as u16);
                    if let Some(c) = &mut cooked {
                        c.push(b as u16);
                    }
                }
            }
        }
    }

    /// Rescans from `start` (a `/` or `/=` token) as a regular expression literal.
    pub fn scan_regex(&mut self, start: u32) -> Result<Tok, LexError> {
        self.pos = start as usize + 1;
        let mut pat: Vec<u16> = Vec::new();
        let mut in_class = false;
        loop {
            let Some(c) = self.bump_char() else {
                return self.err("unterminated regular expression", start as usize);
            };
            if is_line_term(c) {
                return self.err("unterminated regular expression", start as usize);
            }
            match c {
                '\\' => {
                    let mut buf = [0u16; 2];
                    pat.push(b'\\' as u16);
                    match self.bump_char() {
                        Some(d) if !is_line_term(d) => {
                            pat.extend_from_slice(d.encode_utf16(&mut buf))
                        }
                        _ => return self.err("unterminated regular expression", start as usize),
                    }
                    continue;
                }
                '[' => in_class = true,
                ']' => in_class = false,
                '/' if !in_class => break,
                _ => {}
            }
            let mut buf = [0u16; 2];
            pat.extend_from_slice(c.encode_utf16(&mut buf));
        }
        let mut flags: Vec<u16> = Vec::new();
        while let Some(c) = self.peek_char() {
            if is_id_part(c) {
                self.bump_char();
                let mut buf = [0u16; 2];
                flags.extend_from_slice(c.encode_utf16(&mut buf));
            } else {
                break;
            }
        }
        Ok(Tok::Regex(pat, flags))
    }
}

fn push_cp(out: &mut Vec<u16>, cp: u32) {
    if cp >= 0x10000 {
        let c = cp - 0x10000;
        out.push(0xD800 + (c >> 10) as u16);
        out.push(0xDC00 + (c & 0x3ff) as u16);
    } else {
        out.push(cp as u16);
    }
}

fn push_raw(raw: &mut Vec<u16>, s: &str) {
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            if it.peek() == Some(&'\n') {
                it.next();
            }
            raw.push(10);
        } else {
            let mut buf = [0u16; 2];
            raw.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
}

pub(crate) fn decode_utf8(s: &[u8]) -> Option<(char, usize)> {
    let b0 = *s.first()?;
    let (n, init) = match b0 {
        0x00..=0x7f => return Some((b0 as char, 1)),
        0xc2..=0xdf => (2, (b0 & 0x1f) as u32),
        0xe0..=0xef => (3, (b0 & 0x0f) as u32),
        0xf0..=0xf4 => (4, (b0 & 0x07) as u32),
        _ => return Some(('\u{FFFD}', 1)),
    };
    if s.len() < n {
        return Some(('\u{FFFD}', 1));
    }
    let mut v = init;
    for &b in &s[1..n] {
        if b & 0xc0 != 0x80 {
            return Some(('\u{FFFD}', 1));
        }
        v = (v << 6) | (b & 0x3f) as u32;
    }
    Some((char::from_u32(v).unwrap_or('\u{FFFD}'), n))
}

/// Line starts of a source, for turning byte offsets into line:column.
pub(crate) fn line_starts(src: &[u8]) -> Vec<u32> {
    let mut v = alloc::vec![0u32];
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            b'\n' => v.push(i as u32 + 1),
            b'\r' => {
                if src.get(i + 1) != Some(&b'\n') {
                    v.push(i as u32 + 1);
                }
            }
            0xe2 if src.get(i + 1) == Some(&0x80)
                && matches!(src.get(i + 2), Some(0xa8) | Some(0xa9)) =>
            {
                v.push(i as u32 + 3);
            }
            _ => {}
        }
        i += 1;
    }
    v
}

/// (line, column), both 1-based; the column counts characters.
pub(crate) fn line_col(src: &[u8], starts: &[u32], pos: u32) -> (u32, u32) {
    let line = match starts.binary_search(&pos) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    };
    let ls = starts.get(line).copied().unwrap_or(0) as usize;
    let end = (pos as usize).min(src.len());
    let col = if ls <= end {
        core::str::from_utf8(&src[ls..end]).map_or(end - ls, |s| s.chars().count())
    } else {
        0
    };
    (line as u32 + 1, col as u32 + 1)
}
