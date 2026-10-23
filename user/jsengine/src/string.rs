//! Strings: UTF-16 code units, as JavaScript sees them. A string is either flat (its units in
//! one boxed slice) or a rope (the concatenation of two strings, flattened the first time
//! anything but another concatenation looks at it), so `s += x` in a loop is linear.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use crate::value::StrId;

/// The longest string the engine makes (in UTF-16 units), like V8's limit.
pub(crate) const MAX_LEN: usize = (1 << 29) - 24;

pub(crate) enum JsStr {
    Flat(FlatStr),
    Rope { l: StrId, r: StrId, len: u32 },
}

pub(crate) struct FlatStr {
    pub units: Box<[u16]>,
    /// 0 = not computed yet.
    pub hash: Cell<u32>,
    pub interned: bool,
}

impl FlatStr {
    pub fn new(units: Box<[u16]>) -> FlatStr {
        FlatStr {
            units,
            hash: Cell::new(0),
            interned: false,
        }
    }
    pub fn hash(&self) -> u32 {
        let h = self.hash.get();
        if h != 0 {
            return h;
        }
        let h = hash_units(&self.units);
        self.hash.set(h);
        h
    }
}

impl JsStr {
    pub fn len(&self) -> usize {
        match self {
            JsStr::Flat(f) => f.units.len(),
            JsStr::Rope { len, .. } => *len as usize,
        }
    }
    pub fn bytes(&self) -> usize {
        match self {
            JsStr::Flat(f) => 32 + f.units.len() * 2,
            JsStr::Rope { .. } => 32,
        }
    }
}

/// FNV-1a over the code units; never 0.
pub(crate) fn hash_units(u: &[u16]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &c in u {
        h ^= c as u32 & 0xff;
        h = h.wrapping_mul(0x0100_0193);
        h ^= (c >> 8) as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    if h == 0 { 1 } else { h }
}

pub(crate) fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// UTF-16 to UTF-8; a lone surrogate becomes U+FFFD.
pub(crate) fn to_rust(u: &[u16]) -> String {
    let mut out = String::with_capacity(u.len());
    for c in core::char::decode_utf16(u.iter().copied()) {
        out.push(c.unwrap_or('\u{FFFD}'));
    }
    out
}

/// Appends UTF-16 units of `s`.
pub(crate) fn push_str(v: &mut Vec<u16>, s: &str) {
    v.extend(s.encode_utf16());
}

pub(crate) fn eq_str(u: &[u16], s: &str) -> bool {
    let mut it = s.encode_utf16();
    for &c in u {
        match it.next() {
            Some(d) if d == c => {}
            _ => return false,
        }
    }
    it.next().is_none()
}

pub(crate) fn is_ascii_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}

/// The canonical array index a string names ("0".."4294967294", no leading zeros), if any.
pub(crate) fn array_index(u: &[u16]) -> Option<u32> {
    if u.is_empty() || u.len() > 10 {
        return None;
    }
    if u[0] == b'0' as u16 {
        return if u.len() == 1 { Some(0) } else { None };
    }
    let mut n: u64 = 0;
    for &c in u {
        if !is_ascii_digit(c) {
            return None;
        }
        n = n * 10 + (c - b'0' as u16) as u64;
    }
    if n < u32::MAX as u64 {
        Some(n as u32)
    } else {
        None
    }
}

/// JavaScript's WhiteSpace and LineTerminator (what `trim` removes).
pub(crate) fn is_js_space(c: u16) -> bool {
    matches!(
        c,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

pub(crate) fn trim(u: &[u16], start: bool, end: bool) -> &[u16] {
    let mut a = 0;
    let mut b = u.len();
    if start {
        while a < b && is_js_space(u[a]) {
            a += 1;
        }
    }
    if end {
        while b > a && is_js_space(u[b - 1]) {
            b -= 1;
        }
    }
    &u[a..b]
}

pub(crate) fn find(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return if from <= hay.len() { Some(from) } else { None };
    }
    if needle.len() > hay.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let first = needle[0];
    let mut i = from;
    while i <= last {
        if hay[i] == first && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

pub(crate) fn rfind(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    let mut i = from.min(hay.len() - needle.len());
    loop {
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}
