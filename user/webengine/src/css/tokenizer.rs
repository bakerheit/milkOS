//! The CSS tokenizer, after CSS Syntax Level 3 §4 "Tokenization". Comments are dropped;
//! everything else becomes a [`Tok`]. Preprocessing (§3.3: CR/FF to LF, NUL to U+FFFD) is
//! done on the fly.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// `name(`; the arguments follow as tokens up to the matching [`Tok::Close`] `)`.
    Function(String),
    AtKeyword(String),
    /// `#name`; `id` is true when the name would be a valid identifier (type flag "id").
    Hash {
        value: String,
        id: bool,
    },
    Str(String),
    BadString,
    Url(String),
    BadUrl,
    Delim(char),
    Number {
        value: f32,
        int: bool,
    },
    Percentage(f32),
    Dimension {
        value: f32,
        unit: String,
    },
    Whitespace,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    /// `[`, `(`, `{`
    Open(char),
    /// `]`, `)`, `}`
    Close(char),
}

impl Tok {
    pub fn is_ident(&self, s: &str) -> bool {
        matches!(self, Tok::Ident(i) if i.eq_ignore_ascii_case(s))
    }
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_name(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == b'-'
}

fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

impl Lexer<'_> {
    fn at(&self, k: usize) -> u8 {
        self.s.get(self.i + k).copied().unwrap_or(0)
    }
    fn eof(&self, k: usize) -> bool {
        self.i + k >= self.s.len()
    }

    /// §4.3.8 "check if two code points are a valid escape".
    fn valid_escape(&self, k: usize) -> bool {
        self.at(k) == b'\\'
            && !self.eof(k)
            && self.at(k + 1) != b'\n'
            && !(self.eof(k + 1) && self.at(k + 1) == 0)
    }

    /// §4.3.9 "would start an identifier".
    fn starts_ident(&self, k: usize) -> bool {
        let c = self.at(k);
        if self.eof(k) {
            return false;
        }
        if c == b'-' {
            let d = self.at(k + 1);
            (!self.eof(k + 1) && (is_name_start(d) || d == b'-')) || self.valid_escape(k + 1)
        } else if is_name_start(c) {
            true
        } else {
            self.valid_escape(k)
        }
    }

    /// §4.3.10 "would start a number".
    fn starts_number(&self, k: usize) -> bool {
        let c = self.at(k);
        match c {
            b'+' | b'-' => {
                self.at(k + 1).is_ascii_digit()
                    || (self.at(k + 1) == b'.' && self.at(k + 2).is_ascii_digit())
            }
            b'.' => self.at(k + 1).is_ascii_digit(),
            c => c.is_ascii_digit() && !self.eof(k),
        }
    }

    /// One UTF-8 character at the cursor, consumed.
    fn take_char(&mut self) -> char {
        let rest = &self.s[self.i..];
        let len = match rest[0] {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            _ => 4,
        }
        .min(rest.len());
        let c = core::str::from_utf8(&rest[..len])
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or('\u{FFFD}');
        self.i += len;
        if c == '\0' { '\u{FFFD}' } else { c }
    }

    /// §4.3.7 "consume an escaped code point" (after the backslash).
    fn escape(&mut self) -> char {
        if self.eof(0) {
            return '\u{FFFD}';
        }
        let mut v: u32 = 0;
        let mut n = 0;
        while n < 6 && self.at(0).is_ascii_hexdigit() && !self.eof(0) {
            v = v * 16 + (self.at(0) as char).to_digit(16).unwrap();
            self.i += 1;
            n += 1;
        }
        if n > 0 {
            if is_ws(self.at(0)) && !self.eof(0) {
                // CR LF counts as one.
                if self.at(0) == b'\r' && self.at(1) == b'\n' {
                    self.i += 1;
                }
                self.i += 1;
            }
            if v == 0 || (0xD800..=0xDFFF).contains(&v) || v > 0x10FFFF {
                return '\u{FFFD}';
            }
            return char::from_u32(v).unwrap_or('\u{FFFD}');
        }
        self.take_char()
    }

    /// §4.3.11 "consume an ident sequence".
    fn name(&mut self) -> String {
        let mut out = String::new();
        loop {
            if self.eof(0) {
                break;
            }
            let c = self.at(0);
            if is_name(c) {
                if c < 0x80 {
                    out.push(c as char);
                    self.i += 1;
                } else {
                    out.push(self.take_char());
                }
            } else if self.valid_escape(0) {
                self.i += 1;
                out.push(self.escape());
            } else {
                break;
            }
        }
        out
    }

    /// §4.3.12 "consume a number".
    fn number(&mut self) -> (f32, bool) {
        let start = self.i;
        let mut int = true;
        if matches!(self.at(0), b'+' | b'-') {
            self.i += 1;
        }
        while self.at(0).is_ascii_digit() && !self.eof(0) {
            self.i += 1;
        }
        if self.at(0) == b'.' && self.at(1).is_ascii_digit() {
            int = false;
            self.i += 1;
            while self.at(0).is_ascii_digit() && !self.eof(0) {
                self.i += 1;
            }
        }
        if matches!(self.at(0), b'e' | b'E')
            && (self.at(1).is_ascii_digit()
                || (matches!(self.at(1), b'+' | b'-') && self.at(2).is_ascii_digit()))
        {
            int = false;
            self.i += 2;
            while self.at(0).is_ascii_digit() && !self.eof(0) {
                self.i += 1;
            }
        }
        let text = core::str::from_utf8(&self.s[start..self.i]).unwrap_or("0");
        (parse_f32(text), int)
    }

    /// §4.3.3 "consume a numeric token".
    fn numeric(&mut self) -> Tok {
        let (value, int) = self.number();
        if self.starts_ident(0) {
            let unit = self.name().to_ascii_lowercase();
            Tok::Dimension { value, unit }
        } else if self.at(0) == b'%' && !self.eof(0) {
            self.i += 1;
            Tok::Percentage(value)
        } else {
            Tok::Number { value, int }
        }
    }

    /// §4.3.5 "consume a string token" (the opening quote consumed).
    fn string(&mut self, q: u8) -> Tok {
        let mut out = String::new();
        loop {
            if self.eof(0) {
                return Tok::Str(out);
            }
            let c = self.at(0);
            if c == q {
                self.i += 1;
                return Tok::Str(out);
            }
            if c == b'\n' {
                return Tok::BadString;
            }
            if c == b'\\' {
                if self.eof(1) {
                    self.i += 1;
                    continue;
                }
                if self.at(1) == b'\n' {
                    self.i += 2;
                    continue;
                }
                self.i += 1;
                out.push(self.escape());
                continue;
            }
            out.push(self.take_char());
        }
    }

    /// §4.3.6 "consume a url token" (after `url(`).
    fn url(&mut self) -> Tok {
        while is_ws(self.at(0)) && !self.eof(0) {
            self.i += 1;
        }
        let mut out = String::new();
        loop {
            if self.eof(0) {
                return Tok::Url(out);
            }
            let c = self.at(0);
            match c {
                b')' => {
                    self.i += 1;
                    return Tok::Url(out);
                }
                c if is_ws(c) => {
                    while is_ws(self.at(0)) && !self.eof(0) {
                        self.i += 1;
                    }
                    if self.at(0) == b')' || self.eof(0) {
                        self.i += 1;
                        return Tok::Url(out);
                    }
                    self.bad_url_rest();
                    return Tok::BadUrl;
                }
                b'"' | b'\'' | b'(' => {
                    self.bad_url_rest();
                    return Tok::BadUrl;
                }
                b'\\' => {
                    if self.valid_escape(0) {
                        self.i += 1;
                        out.push(self.escape());
                    } else {
                        self.bad_url_rest();
                        return Tok::BadUrl;
                    }
                }
                _ => out.push(self.take_char()),
            }
        }
    }

    fn bad_url_rest(&mut self) {
        while !self.eof(0) {
            if self.at(0) == b')' {
                self.i += 1;
                return;
            }
            if self.valid_escape(0) {
                self.i += 1;
                self.escape();
            } else {
                self.i += 1;
            }
        }
    }

    /// §4.3.4 "consume an ident-like token".
    fn ident_like(&mut self) -> Tok {
        let name = self.name();
        if name.eq_ignore_ascii_case("url") && self.at(0) == b'(' {
            self.i += 1;
            // url( "x" ) is a function with a string argument; url(x) a url token.
            let mut k = 0;
            while is_ws(self.at(k)) && !self.eof(k) {
                k += 1;
            }
            if matches!(self.at(k), b'"' | b'\'') {
                self.i += k;
                return Tok::Function(name.to_ascii_lowercase());
            }
            return self.url();
        }
        if self.at(0) == b'(' {
            self.i += 1;
            return Tok::Function(name.to_ascii_lowercase());
        }
        Tok::Ident(name)
    }

    /// §4.3.1 "consume a token". None at the end.
    fn next(&mut self) -> Option<Tok> {
        loop {
            if self.eof(0) {
                return None;
            }
            // Comments (§4.3.2).
            if self.at(0) == b'/' && self.at(1) == b'*' {
                match self.s[self.i + 2..].windows(2).position(|w| w == b"*/") {
                    Some(p) => self.i += 2 + p + 2,
                    None => self.i = self.s.len(),
                }
                continue;
            }
            break;
        }
        let c = self.at(0);
        Some(match c {
            c if is_ws(c) => {
                while is_ws(self.at(0)) && !self.eof(0) {
                    self.i += 1;
                }
                Tok::Whitespace
            }
            b'"' | b'\'' => {
                self.i += 1;
                self.string(c)
            }
            b'#' => {
                if is_name(self.at(1)) || self.valid_escape(1) {
                    self.i += 1;
                    let id = self.starts_ident(0);
                    Tok::Hash {
                        value: self.name(),
                        id,
                    }
                } else {
                    self.i += 1;
                    Tok::Delim('#')
                }
            }
            b'(' | b'[' | b'{' => {
                self.i += 1;
                Tok::Open(c as char)
            }
            b')' | b']' | b'}' => {
                self.i += 1;
                Tok::Close(c as char)
            }
            b'+' | b'.' => {
                if self.starts_number(0) {
                    self.numeric()
                } else {
                    self.i += 1;
                    Tok::Delim(c as char)
                }
            }
            b',' => {
                self.i += 1;
                Tok::Comma
            }
            b'-' => {
                if self.starts_number(0) {
                    self.numeric()
                } else if self.at(1) == b'-' && self.at(2) == b'>' {
                    self.i += 3;
                    Tok::Cdc
                } else if self.starts_ident(0) {
                    self.ident_like()
                } else {
                    self.i += 1;
                    Tok::Delim('-')
                }
            }
            b':' => {
                self.i += 1;
                Tok::Colon
            }
            b';' => {
                self.i += 1;
                Tok::Semicolon
            }
            b'<' => {
                if self.s[self.i..].starts_with(b"<!--") {
                    self.i += 4;
                    Tok::Cdo
                } else {
                    self.i += 1;
                    Tok::Delim('<')
                }
            }
            b'@' => {
                if self.starts_ident(1) {
                    self.i += 1;
                    Tok::AtKeyword(self.name().to_ascii_lowercase())
                } else {
                    self.i += 1;
                    Tok::Delim('@')
                }
            }
            b'\\' => {
                if self.valid_escape(0) {
                    self.ident_like()
                } else {
                    self.i += 1;
                    Tok::Delim('\\')
                }
            }
            b'0'..=b'9' => self.numeric(),
            c if is_name_start(c) => self.ident_like(),
            _ => Tok::Delim(self.take_char()),
        })
    }
}

/// Tokenize a whole stylesheet (or declaration list). At most `max` tokens (hostile input).
pub fn tokenize(s: &str, max: usize) -> Vec<Tok> {
    let mut lx = Lexer {
        s: s.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    while let Some(t) = lx.next() {
        if out.len() >= max {
            break;
        }
        out.push(t);
    }
    out
}

/// A decimal number (CSS's grammar; no NaN/inf). Saturates on huge exponents.
pub fn parse_f32(s: &str) -> f32 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let mut v: f64 = 0.0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = v * 10.0 + (b[i] - b'0') as f64;
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let mut scale = 0.1;
        while i < b.len() && b[i].is_ascii_digit() {
            v += (b[i] - b'0') as f64 * scale;
            scale *= 0.1;
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let mut eneg = false;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            eneg = b[i] == b'-';
            i += 1;
        }
        let mut e: i32 = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            e = (e * 10 + (b[i] - b'0') as i32).min(400);
            i += 1;
        }
        for _ in 0..e.min(40) {
            if eneg {
                v /= 10.0;
            } else {
                v *= 10.0;
            }
        }
    }
    let v = if neg { -v } else { v };
    v.clamp(-1.0e30, 1.0e30) as f32
}
