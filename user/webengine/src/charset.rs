//! Turning response bytes into text: WHATWG HTML §13.2.3 "The input byte stream" (the
//! encoding sniffing algorithm, simplified) and §13.2.3.5 newline normalization.
//!
//! Supported encodings: UTF-8, UTF-16LE/BE (by BOM or label) and windows-1252, which the
//! Encoding Standard also uses for the labels iso-8859-1, latin1, ascii and friends. An
//! unknown label is ignored (the next step decides), as the spec says.

use alloc::string::String;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Windows1252,
}

/// The Encoding Standard's labels for the encodings we have.
pub fn from_label(label: &str) -> Option<Encoding> {
    let l = label.trim_matches(|c: char| c.is_ascii_whitespace() || c == '"' || c == '\'');
    let l = l.to_ascii_lowercase();
    Some(match l.as_str() {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8"
        | "x-unicode20utf8" => Encoding::Utf8,
        "utf-16le" | "utf-16" | "ucs-2" | "unicode" | "csunicode" | "iso-10646-ucs-2"
        | "unicodefeff" => Encoding::Utf16Le,
        "utf-16be" | "unicodefffe" => Encoding::Utf16Be,
        "ansi_x3.4-1968" | "ascii" | "cp1252" | "cp819" | "csisolatin1" | "ibm819"
        | "iso-8859-1" | "iso-ir-100" | "iso8859-1" | "iso88591" | "iso_8859-1"
        | "iso_8859-1:1987" | "l1" | "latin1" | "us-ascii" | "windows-1252" | "x-cp1252"
        | "iso-8859-15" | "latin9" | "l9" | "iso8859-15" | "iso885915" | "iso_8859-15"
        | "csisolatin9" => Encoding::Windows1252,
        _ => return None,
    })
}

/// The charset parameter of a Content-Type value.
pub fn content_type_charset(ct: &str) -> Option<Encoding> {
    for part in ct.split(';').skip(1) {
        let mut kv = part.splitn(2, '=');
        let k = kv.next()?.trim();
        if k.eq_ignore_ascii_case("charset") {
            return from_label(kv.next().unwrap_or(""));
        }
    }
    None
}

/// §13.2.3.2 "prescan a byte stream to determine its encoding", reduced to what pages use:
/// `<meta charset=...>` and `<meta http-equiv=Content-Type content="...; charset=...">` in the
/// first 1024 bytes (comments skipped).
pub fn prescan(b: &[u8]) -> Option<Encoding> {
    let b = &b[..b.len().min(1024)];
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"<!--") {
            {
                let j = find(&b[i + 4..], b"-->")?;
                i += 4 + j + 3
            }
            continue;
        }
        if b[i] == b'<'
            && b.len() >= i + 6
            && b[i + 1..i + 5].eq_ignore_ascii_case(b"meta")
            && matches!(b[i + 5], b' ' | b'\t' | b'\n' | b'\r' | 0x0c | b'/')
        {
            let end = b[i..]
                .iter()
                .position(|&c| c == b'>')
                .map(|p| i + p)
                .unwrap_or(b.len());
            let tag = String::from_utf8_lossy(&b[i + 5..end]).to_ascii_lowercase();
            if let Some(p) = tag.find("charset") {
                let rest = tag[p + 7..].trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    let rest = rest.trim_start();
                    let v: String = rest
                        .trim_start_matches(['"', '\''])
                        .chars()
                        .take_while(|&c| {
                            !matches!(c, '"' | '\'' | ';' | ' ' | '>' | '/' | '\t' | '\n')
                        })
                        .collect();
                    if let Some(e) = from_label(&v) {
                        // §13.2.3.2: a UTF-16 label in a meta means UTF-8.
                        return Some(match e {
                            Encoding::Utf16Le | Encoding::Utf16Be => Encoding::Utf8,
                            e => e,
                        });
                    }
                }
            }
            i = end;
            continue;
        }
        i += 1;
    }
    None
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Pick the encoding (§13.2.3.1 steps: BOM, transport layer, prescan, then a guess: UTF-8
/// when the bytes are valid UTF-8, else windows-1252) and decode, normalizing newlines.
pub fn decode(b: &[u8], content_type: Option<&str>) -> (String, Encoding) {
    let (enc, body) = if let Some(r) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (Encoding::Utf8, r)
    } else if let Some(r) = b.strip_prefix(&[0xFF, 0xFE]) {
        (Encoding::Utf16Le, r)
    } else if let Some(r) = b.strip_prefix(&[0xFE, 0xFF]) {
        (Encoding::Utf16Be, r)
    } else if let Some(e) = content_type.and_then(content_type_charset) {
        (e, b)
    } else if let Some(e) = prescan(b) {
        (e, b)
    } else if core::str::from_utf8(b).is_ok() {
        (Encoding::Utf8, b)
    } else {
        (Encoding::Windows1252, b)
    };
    (decode_with(body, enc), enc)
}

/// Decode `b` as `enc` (malformed sequences become U+FFFD) and normalize newlines.
pub fn decode_with(b: &[u8], enc: Encoding) -> String {
    let mut out = String::with_capacity(b.len() + b.len() / 16);
    let mut last_cr = false;
    let mut push = |out: &mut String, c: char| {
        // §13.2.3.5: CR LF → LF, lone CR → LF.
        if c == '\n' && last_cr {
            last_cr = false;
            return;
        }
        last_cr = c == '\r';
        out.push(if c == '\r' { '\n' } else { c });
    };
    match enc {
        Encoding::Utf8 => {
            let s = String::from_utf8_lossy(b);
            if !s.contains('\r') {
                return s.into_owned();
            }
            for c in s.chars() {
                push(&mut out, c);
            }
        }
        Encoding::Windows1252 => {
            for &x in b {
                push(&mut out, windows1252(x));
            }
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let units = b.as_chunks::<2>().0.iter().map(|p| {
                if enc == Encoding::Utf16Le {
                    u16::from_le_bytes([p[0], p[1]])
                } else {
                    u16::from_be_bytes([p[0], p[1]])
                }
            });
            for c in char::decode_utf16(units) {
                push(&mut out, c.unwrap_or('\u{FFFD}'));
            }
            if b.len() % 2 == 1 {
                out.push('\u{FFFD}');
            }
        }
    }
    out
}

/// windows-1252 byte → character (the Encoding Standard's index; the five undefined bytes
/// map to the C1 controls of the same value).
pub fn windows1252(x: u8) -> char {
    const HI: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    if (0x80..0xA0).contains(&x) {
        char::from_u32(HI[(x - 0x80) as usize] as u32).unwrap_or('\u{FFFD}')
    } else {
        x as char
    }
}
