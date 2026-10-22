//! A look inside a PDF without rendering it: how many pages, and the text we can read out of
//! the content streams (the strings the page draws with `Tj`, `TJ`, `'` and `"`). Best effort and
//! honest about it: pages drawn with embedded fonts that map glyph numbers to letters (most
//! real-world PDFs) give gibberish or nothing, so `limited` says when the text may be partial.
//! Encrypted files are reported and not read.

use alloc::string::String;
use alloc::vec::Vec;

use crate::inflate::zlib_decompress;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PdfInfo {
    /// Number of pages, if we could tell.
    pub pages: Option<usize>,
    /// The readable text, pages in order of appearance, lines separated by `\n`.
    pub text: String,
    pub encrypted: bool,
    /// Some text may be missing (fonts we can't map, streams we can't unpack).
    pub limited: bool,
}

/// At most this much of any one stream is unpacked.
const MAX_STREAM: usize = 16 << 20;
/// Stop collecting text after this many chars.
const MAX_TEXT: usize = 2 << 20;

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// Does `hay` contain `/Type` + optional space + `/Page` (and not `/Pages`) at `at`?
fn count_pages(b: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while let Some(p) = find(b, b"/Type", i) {
        i = p + 5;
        let mut j = i;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        if b[j..].starts_with(b"/Page") && !b[j..].starts_with(b"/Pages") {
            n += 1;
        }
    }
    n
}

pub fn inspect(file: &[u8]) -> Result<PdfInfo, crate::ImgError> {
    if !file.starts_with(b"%PDF-") {
        return Err(crate::ImgError::Unrecognised("This isn't a PDF file."));
    }
    let mut info = PdfInfo::default();
    if find(file, b"/Encrypt", 0).is_some() {
        info.encrypted = true;
        info.limited = true;
        info.pages = Some(count_pages(file)).filter(|&n| n > 0);
        return Ok(info);
    }
    let mut pages = count_pages(file);
    let mut text = String::new();
    let mut pos = 0;
    while let Some(s) = find(file, b"stream", pos) {
        // "endstream" contains "stream": skip those.
        if s >= 3 && &file[s - 3..s] == b"end" {
            pos = s + 6;
            continue;
        }
        // The data starts after "stream" and one line break.
        let mut d = s + 6;
        if file.get(d) == Some(&b'\r') {
            d += 1;
        }
        if file.get(d) == Some(&b'\n') {
            d += 1;
        }
        let Some(e) = find(file, b"endstream", d) else {
            info.limited = true;
            break;
        };
        // The dictionary: back from "stream" to the previous "obj".
        let dict_start = file[..s]
            .windows(3)
            .rposition(|w| w == b"obj")
            .map_or(0, |p| p + 3);
        let dict = &file[dict_start..s];
        pos = e + 9;
        if find(dict, b"/Image", 0).is_some()
            || find(dict, b"/FontFile", 0).is_some()
            || find(dict, b"/XRef", 0).is_some()
        {
            continue;
        }
        let raw = &file[d..e];
        let data: Vec<u8> = if find(dict, b"/FlateDecode", 0).is_some() {
            match zlib_decompress(raw, MAX_STREAM) {
                Ok(v) => v,
                Err(_) => {
                    info.limited = true;
                    continue;
                }
            }
        } else if find(dict, b"/Filter", 0).is_some() {
            info.limited = true; // another compression we don't read
            continue;
        } else {
            raw.to_vec()
        };
        if find(dict, b"/ObjStm", 0).is_some() {
            // Object streams hold page objects in newer files.
            pages += count_pages(&data);
            continue;
        }
        if text.len() < MAX_TEXT {
            content_text(&data, &mut text, &mut info.limited);
        }
    }
    info.pages = (pages > 0).then_some(pages);
    // Tidy: trim trailing blank lines.
    while text.ends_with('\n') || text.ends_with(' ') {
        text.pop();
    }
    info.text = text;
    Ok(info)
}

fn is_text_char(c: char) -> bool {
    c == '\n' || c == '\t' || (c >= ' ' && c != '\u{7f}')
}

/// Pull the text out of one content stream.
fn content_text(d: &[u8], out: &mut String, limited: &mut bool) {
    // A content stream is operators and operands; anything with lots of control bytes isn't one.
    let odd = d
        .iter()
        .take(4096)
        .filter(|&&b| b < 9 || (b > 13 && b < 32) || b > 126)
        .count();
    if odd * 8 > d.len().min(4096) {
        return;
    }
    let mut i = 0;
    let mut in_text = false;
    let mut pending: Vec<String> = Vec::new(); // string operands since the last operator
    let mut last_num: Option<f64> = None;
    let mut nums: Vec<f64> = Vec::new();
    while i < d.len() && out.len() < MAX_TEXT {
        let c = d[i];
        match c {
            b'(' => {
                let (s, next) = literal(d, i);
                pending.push(s);
                i = next;
            }
            b'<' if d.get(i + 1) != Some(&b'<') => {
                let (s, next, two_byte) = hex(d, i);
                if two_byte {
                    *limited = true;
                }
                pending.push(s);
                i = next;
            }
            b'[' | b']' | b'{' | b'}' | b'<' | b'>' => i += 1,
            b'%' => {
                while i < d.len() && d[i] != b'\n' && d[i] != b'\r' {
                    i += 1;
                }
            }
            b'/' => {
                i += 1;
                while i < d.len() && !is_delim(d[i]) {
                    i += 1;
                }
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                let start = i;
                while i < d.len() && !is_delim(d[i]) {
                    i += 1;
                }
                if i == start {
                    i += 1;
                    continue;
                }
                let tok = &d[start..i];
                if let Some(n) = number(tok) {
                    // Inside a TJ array a big negative kern is a space.
                    if n < -200.0 && !pending.is_empty() {
                        pending.push(String::from(" "));
                    }
                    last_num = Some(n);
                    nums.push(n);
                    continue;
                }
                match tok {
                    b"BT" => in_text = true,
                    b"ET" => {
                        in_text = false;
                        newline(out);
                    }
                    b"Tj" | b"TJ" | b"'" | b"\"" if in_text => {
                        if tok == b"'" || tok == b"\"" {
                            newline(out);
                        }
                        for s in pending.drain(..) {
                            out.extend(s.chars().filter(|&c| is_text_char(c)));
                        }
                    }
                    b"Td" | b"TD" if in_text => {
                        if nums.last().is_some_and(|&y| y != 0.0) {
                            newline(out);
                        } else if !out.ends_with(' ') && !out.ends_with('\n') && !out.is_empty() {
                            out.push(' ');
                        }
                    }
                    b"T*" if in_text => newline(out),
                    _ => {}
                }
                pending.clear();
                nums.clear();
                last_num = None;
            }
        }
        let _ = last_num;
    }
}

fn newline(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn is_delim(b: u8) -> bool {
    b.is_ascii_whitespace()
        || matches!(
            b,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

fn number(tok: &[u8]) -> Option<f64> {
    let s = core::str::from_utf8(tok).ok()?;
    if !s
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.'))
        || s.bytes().all(|b| !b.is_ascii_digit())
    {
        return None;
    }
    // A tiny parser: sign, integer part, fraction.
    let (neg, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int, frac) = rest.split_once('.').unwrap_or((rest, ""));
    let mut v = 0f64;
    for b in int.bytes() {
        v = v * 10.0 + (b - b'0') as f64;
    }
    let mut scale = 0.1;
    for b in frac.bytes() {
        v += (b - b'0') as f64 * scale;
        scale /= 10.0;
    }
    Some(if neg { -v } else { v })
}

/// A `( ... )` string starting at `i`, with escapes and nested parentheses. Returns the text
/// (bytes read as Latin-1) and the index after it.
fn literal(d: &[u8], mut i: usize) -> (String, usize) {
    let mut s = String::new();
    let mut depth = 0;
    while i < d.len() {
        let c = d[i];
        i += 1;
        match c {
            b'(' => {
                depth += 1;
                if depth > 1 {
                    s.push('(');
                }
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                s.push(')');
            }
            b'\\' if i < d.len() => {
                let e = d[i];
                i += 1;
                match e {
                    b'n' => s.push('\n'),
                    b'r' => s.push('\r'),
                    b't' => s.push('\t'),
                    b'b' | b'f' => {}
                    b'0'..=b'7' => {
                        let mut v = (e - b'0') as u32;
                        for _ in 0..2 {
                            if let Some(&o) = d.get(i).filter(|o| (b'0'..=b'7').contains(o)) {
                                v = v * 8 + (o - b'0') as u32;
                                i += 1;
                            }
                        }
                        s.push(char::from_u32(v & 0xFF).unwrap_or(' '));
                    }
                    b'\r' => {
                        if d.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    b'\n' => {}
                    other => s.push(other as char),
                }
            }
            _ => s.push(c as char),
        }
    }
    (s, i)
}

/// A `<...>` hex string starting at `i`: the bytes as Latin-1, the index after it, and whether it
/// looks like 2-byte glyph ids (so the text is probably not letters).
fn hex(d: &[u8], mut i: usize) -> (String, usize, bool) {
    i += 1;
    let mut bytes: Vec<u8> = Vec::new();
    let mut hi: Option<u8> = None;
    while i < d.len() && d[i] != b'>' {
        let v = match d[i] {
            b'0'..=b'9' => Some(d[i] - b'0'),
            b'a'..=b'f' => Some(d[i] - b'a' + 10),
            b'A'..=b'F' => Some(d[i] - b'A' + 10),
            _ => None,
        };
        if let Some(v) = v {
            match hi.take() {
                Some(h) => bytes.push(h << 4 | v),
                None => hi = Some(v),
            }
        }
        i += 1;
    }
    if let Some(h) = hi {
        bytes.push(h << 4);
    }
    // Two-byte codes with a zero high byte are glyph ids, not letters.
    let two_byte = bytes.len() >= 2 && bytes.len() % 2 == 0 && bytes.chunks(2).all(|c| c[0] == 0);
    if two_byte {
        return (String::new(), i + 1, true);
    }
    (bytes.iter().map(|&b| b as char).collect(), i + 1, false)
}
