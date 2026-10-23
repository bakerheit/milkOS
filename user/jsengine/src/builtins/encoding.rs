//! TextEncoder, TextDecoder (utf-8, utf-16le, and latin1 as windows-1252) and `atob`/`btoa`.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::api::{Exc, Exception};
use crate::names::N;
use crate::object::{ObjKind, WC, WEC};
use crate::realm::{Realm, Res};
use crate::typed::TaKind;
use crate::value::{ObjId, Val};

use super::arg;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Enc {
    Utf8,
    Utf16le,
    /// windows-1252 (what the "latin1" and "iso-8859-1" labels mean on the web).
    Latin1,
}

pub(crate) struct DecoderData {
    enc: Enc,
    fatal: bool,
    ignore_bom: bool,
    /// Bytes of an unfinished sequence between `decode(…, {stream: true})` calls.
    pub pending: Vec<u8>,
    /// The start of the stream was seen (so a later U+FEFF is data, not a BOM).
    started: bool,
}

/// An Error named like a DOMException (`InvalidCharacterError`, `DataCloneError`, ...).
pub(crate) fn dom_exception(r: &mut Realm, name: &str, msg: &str) -> Exception {
    let m = r.str_val(msg);
    let p = r.intr.error_proto;
    let e = r.make_error_with_proto(p, name, Some(m));
    let nk = r.key(N::name);
    let nv = r.str_val(name);
    r.put_prop(e, nk, nv, WC);
    Exception(Exc::Throw(Val::Obj(e)))
}

// ---- TextEncoder ----

fn encoder_ctor(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error("Constructor TextEncoder requires 'new'"));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.text_encoder_proto)?;
    Ok(Val::Obj(r.alloc(Some(proto), ObjKind::Ordinary)))
}

fn this_encoder(r: &mut Realm, this: Val) -> Res<()> {
    if let Val::Obj(o) = this
        && r.o(o).proto.is_some()
        && r.is_proto_chain_has(o, r.intr.text_encoder_proto)
    {
        return Ok(());
    }
    Err(r.type_error("Receiver is not a TextEncoder"))
}

impl Realm {
    fn is_proto_chain_has(&self, mut o: ObjId, p: ObjId) -> bool {
        let mut n = 0;
        while let Some(q) = self.o(o).proto {
            if q == p {
                return true;
            }
            o = q;
            n += 1;
            if n > 10_000 {
                break;
            }
        }
        false
    }
}

fn encoder_encoding(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    this_encoder(r, this)?;
    Ok(r.str_val("utf-8"))
}

/// UTF-16 to UTF-8; a lone surrogate becomes U+FFFD.
fn utf8_encode(u: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(u.len());
    for c in char::decode_utf16(u.iter().copied()) {
        let ch = c.unwrap_or('\u{FFFD}');
        let mut b = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
    }
    out
}

fn encoder_encode(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    this_encoder(r, this)?;
    let u = if arg(args, 0).is_undef() {
        Vec::new()
    } else {
        r.to_units(arg(args, 0))?
    };
    r.check_buffer_size(u.len().saturating_mul(3))?;
    r.heap.work += (u.len() >> 3) as u64;
    let bytes = utf8_encode(&u);
    let n = bytes.len();
    let buf = r.buffer_from_vec(bytes);
    Ok(Val::Obj(r.typed_new(TaKind::Uint8, buf, 0, n, None)))
}

fn encoder_encode_into(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    this_encoder(r, this)?;
    let u = r.to_units(arg(args, 0))?;
    let Val::Obj(dest) = arg(args, 1) else {
        return Err(r.type_error("encodeInto: the destination must be a Uint8Array"));
    };
    match r.typed_info(dest) {
        Some((TaKind::Uint8, ..)) => {}
        _ => return Err(r.type_error("encodeInto: the destination must be a Uint8Array")),
    }
    let (mut read, mut written) = (0usize, 0usize);
    let room = r.typed_len(dest);
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < u.len() {
        let (ch, units) = match char::decode_utf16(u[i..].iter().copied()).next() {
            Some(Ok(c)) => (c, c.len_utf16()),
            _ => ('\u{FFFD}', 1),
        };
        let l = ch.len_utf8();
        if written + l > room {
            break;
        }
        let mut b = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
        written += l;
        read += units;
        i += units;
    }
    if let Some(d) = r.typed_bytes_mut(dest).get_mut(..written) {
        d.copy_from_slice(&out);
    }
    let o = r.new_plain();
    let rk = r.key_str("read");
    let wk = r.key_str("written");
    r.put_prop(o, rk, Val::num(read as f64), WEC);
    r.put_prop(o, wk, Val::num(written as f64), WEC);
    Ok(Val::Obj(o))
}

// ---- TextDecoder ----

fn label_encoding(label: &str) -> Option<(Enc, &'static str)> {
    Some(match label {
        "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8"
        | "x-unicode20utf8" => (Enc::Utf8, "utf-8"),
        "utf-16le" | "utf-16" | "ucs-2" | "unicode" | "csunicode" | "iso-10646-ucs-2"
        | "unicodefeff" => (Enc::Utf16le, "utf-16le"),
        "latin1" | "iso-8859-1" | "iso8859-1" | "l1" | "ascii" | "us-ascii" | "windows-1252"
        | "cp1252" | "x-cp1252" | "iso_8859-1" | "ansi_x3.4-1968" | "iso-ir-100" | "cp819"
        | "ibm819" | "csisolatin1" | "iso88591" | "iso_8859-1:1987" => {
            (Enc::Latin1, "windows-1252")
        }
        _ => return None,
    })
}

fn decoder_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error("Constructor TextDecoder requires 'new'"));
    }
    let label = if arg(args, 0).is_undef() {
        alloc::string::String::from("utf-8")
    } else {
        r.to_rust_string(arg(args, 0))?
    };
    let norm = label
        .trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r'))
        .to_ascii_lowercase();
    let Some((enc, _)) = label_encoding(&norm) else {
        return Err(r.range_error(&alloc::format!(
            "The encoding label provided ('{label}') is invalid."
        )));
    };
    let (mut fatal, mut ignore_bom) = (false, false);
    if let Val::Obj(opts) = arg(args, 1) {
        let fk = r.key_str("fatal");
        let v = r.get_p(opts, fk, Val::Obj(opts))?;
        fatal = r.truthy(v);
        let bk = r.key_str("ignoreBOM");
        let v = r.get_p(opts, bk, Val::Obj(opts))?;
        ignore_bom = r.truthy(v);
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.text_decoder_proto)?;
    Ok(Val::Obj(r.alloc(
        Some(proto),
        ObjKind::TextDecoder(Box::new(DecoderData {
            enc,
            fatal,
            ignore_bom,
            pending: Vec::new(),
            started: false,
        })),
    )))
}

fn this_decoder(r: &mut Realm, this: Val) -> Res<ObjId> {
    if let Val::Obj(o) = this
        && let ObjKind::TextDecoder(_) = r.o(o).kind
    {
        return Ok(o);
    }
    Err(r.type_error("Receiver is not a TextDecoder"))
}

fn decoder_encoding(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = this_decoder(r, this)?;
    let name = match &r.o(o).kind {
        ObjKind::TextDecoder(d) => match d.enc {
            Enc::Utf8 => "utf-8",
            Enc::Utf16le => "utf-16le",
            Enc::Latin1 => "windows-1252",
        },
        _ => "utf-8",
    };
    Ok(r.str_val(name))
}

fn decoder_fatal(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = this_decoder(r, this)?;
    Ok(Val::Bool(
        matches!(&r.o(o).kind, ObjKind::TextDecoder(d) if d.fatal),
    ))
}

fn decoder_ignore_bom(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = this_decoder(r, this)?;
    Ok(Val::Bool(
        matches!(&r.o(o).kind, ObjKind::TextDecoder(d) if d.ignore_bom),
    ))
}

const CP1252: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

/// Decodes UTF-8 into UTF-16 units. `Err(())` on an invalid sequence when `fatal`. With
/// `stream`, an incomplete sequence at the end is returned as the pending bytes.
fn utf8_decode(mut b: &[u8], fatal: bool, stream: bool, out: &mut Vec<u16>) -> Result<Vec<u8>, ()> {
    loop {
        match core::str::from_utf8(b) {
            Ok(s) => {
                out.extend(s.encode_utf16());
                return Ok(Vec::new());
            }
            Err(e) => {
                let good = e.valid_up_to();
                // (valid_up_to marks a valid prefix.)
                if let Ok(s) = core::str::from_utf8(&b[..good]) {
                    out.extend(s.encode_utf16());
                }
                match e.error_len() {
                    Some(n) => {
                        if fatal {
                            return Err(());
                        }
                        out.push(0xFFFD);
                        b = &b[good + n..];
                    }
                    None => {
                        // Truncated at the end of the input.
                        if stream {
                            return Ok(b[good..].to_vec());
                        }
                        if fatal {
                            return Err(());
                        }
                        out.push(0xFFFD);
                        return Ok(Vec::new());
                    }
                }
            }
        }
    }
}

fn utf16le_decode(b: &[u8], fatal: bool, stream: bool, out: &mut Vec<u16>) -> Result<Vec<u8>, ()> {
    let pairs = b.len() / 2;
    let mut units: Vec<u16> = (0..pairs)
        .map(|i| u16::from_le_bytes([b[2 * i], b[2 * i + 1]]))
        .collect();
    let mut pending: Vec<u8> = Vec::new();
    let odd = b.len() % 2 == 1;
    if stream {
        if odd {
            pending.push(b[b.len() - 1]);
        }
        if let Some(&last) = units.last()
            && (0xD800..0xDC00).contains(&last)
        {
            units.pop();
            let mut p = last.to_le_bytes().to_vec();
            p.extend_from_slice(&pending);
            pending = p;
        }
    } else if odd && fatal {
        return Err(());
    }
    let mut i = 0;
    while i < units.len() {
        let c = units[i];
        if (0xD800..0xDC00).contains(&c) {
            match units.get(i + 1) {
                Some(&d) if (0xDC00..0xE000).contains(&d) => {
                    out.push(c);
                    out.push(d);
                    i += 2;
                    continue;
                }
                _ => {
                    if fatal {
                        return Err(());
                    }
                    out.push(0xFFFD);
                }
            }
        } else if (0xDC00..0xE000).contains(&c) {
            if fatal {
                return Err(());
            }
            out.push(0xFFFD);
        } else {
            out.push(c);
        }
        i += 1;
    }
    if !stream && odd {
        out.push(0xFFFD);
    }
    Ok(pending)
}

fn decoder_decode(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = this_decoder(r, this)?;
    let input = arg(args, 0);
    let mut stream = false;
    if let Val::Obj(opts) = arg(args, 1) {
        let sk = r.key_str("stream");
        let v = r.get_p(opts, sk, Val::Obj(opts))?;
        stream = r.truthy(v);
    }
    let bytes: Vec<u8> = match input {
        Val::Undef | Val::Empty => Vec::new(),
        Val::Obj(b) => match r.o(b).kind {
            ObjKind::ArrayBuffer(_) => {
                if r.buf_detached(b) {
                    Vec::new()
                } else {
                    r.buf_data(b).map(|d| d.bytes.clone()).unwrap_or_default()
                }
            }
            ObjKind::Typed(_) => r.typed_bytes(b).to_vec(),
            ObjKind::DataView(ref d) => {
                let (buf, off, len) = (d.buf, d.offset, d.len);
                r.buf_data(buf)
                    .and_then(|d| d.bytes.get(off..off + len))
                    .map(|s| s.to_vec())
                    .unwrap_or_default()
            }
            _ => {
                return Err(r.type_error(
                    "The provided value is not of type '(ArrayBuffer or ArrayBufferView)'",
                ));
            }
        },
        _ => {
            return Err(r.type_error(
                "The provided value is not of type '(ArrayBuffer or ArrayBufferView)'",
            ));
        }
    };
    let (enc, fatal, ignore_bom, mut data, started) = match &mut r.om(o).kind {
        ObjKind::TextDecoder(d) => (
            d.enc,
            d.fatal,
            d.ignore_bom,
            core::mem::take(&mut d.pending),
            d.started,
        ),
        _ => return Err(r.type_error("Receiver is not a TextDecoder")),
    };
    data.extend_from_slice(&bytes);
    r.check_str_len(data.len())?;
    r.heap.work += (data.len() >> 3) as u64;
    let mut out: Vec<u16> = Vec::with_capacity(data.len().min(1 << 24));
    let res = match enc {
        Enc::Utf8 => utf8_decode(&data, fatal, stream, &mut out),
        Enc::Utf16le => utf16le_decode(&data, fatal, stream, &mut out),
        Enc::Latin1 => {
            out.extend(data.iter().map(|&b| {
                if (0x80..0xA0).contains(&b) {
                    CP1252[(b - 0x80) as usize]
                } else {
                    b as u16
                }
            }));
            Ok(Vec::new())
        }
    };
    let pending = match res {
        Ok(p) => p,
        Err(()) => {
            if let ObjKind::TextDecoder(d) = &mut r.om(o).kind {
                d.pending.clear();
                d.started = false;
            }
            return Err(r.type_error("The encoded data was not valid for the encoding"));
        }
    };
    // A BOM at the very start of the stream is dropped (unless ignoreBOM).
    let mut started_now = started;
    if !started && !out.is_empty() {
        started_now = true;
        if !ignore_bom && enc != Enc::Latin1 && out[0] == 0xFEFF {
            out.remove(0);
        }
    }
    if let ObjKind::TextDecoder(d) = &mut r.om(o).kind {
        if stream {
            d.pending = pending;
            d.started = started_now;
        } else {
            d.pending.clear();
            d.started = false;
        }
    }
    Ok(r.units_val(out))
}

// ---- atob / btoa ----

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn btoa(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if args.is_empty() {
        return Err(r.type_error("btoa requires 1 argument"));
    }
    let u = r.to_units(args[0])?;
    if u.iter().any(|&c| c > 0xFF) {
        return Err(dom_exception(
            r,
            "InvalidCharacterError",
            "The string to be encoded contains characters outside of the Latin1 range.",
        ));
    }
    r.check_str_len(u.len() / 3 * 4 + 4)?;
    r.heap.work += (u.len() >> 3) as u64;
    let mut out: Vec<u16> = Vec::with_capacity(u.len() / 3 * 4 + 4);
    for ch in u.chunks(3) {
        let b0 = ch[0] as u32;
        let b1 = ch.get(1).map_or(0, |&c| c as u32);
        let b2 = ch.get(2).map_or(0, |&c| c as u32);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[(n >> 18) as usize & 63] as u16);
        out.push(B64[(n >> 12) as usize & 63] as u16);
        out.push(if ch.len() > 1 {
            B64[(n >> 6) as usize & 63] as u16
        } else {
            b'=' as u16
        });
        out.push(if ch.len() > 2 {
            B64[n as usize & 63] as u16
        } else {
            b'=' as u16
        });
    }
    Ok(r.units_val(out))
}

fn atob(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if args.is_empty() {
        return Err(r.type_error("atob requires 1 argument"));
    }
    let u = r.to_units(args[0])?;
    let bad = |r: &mut Realm| {
        dom_exception(
            r,
            "InvalidCharacterError",
            "The string to be decoded is not correctly encoded.",
        )
    };
    // Forgiving-base64: drop ASCII whitespace, accept a missing or complete '=' padding.
    let mut s: Vec<u16> = u
        .iter()
        .copied()
        .filter(|&c| !matches!(c, 0x20 | 0x09 | 0x0A | 0x0C | 0x0D))
        .collect();
    if s.len() % 4 == 0 && s.last() == Some(&(b'=' as u16)) {
        s.pop();
        if s.last() == Some(&(b'=' as u16)) {
            s.pop();
        }
    }
    if s.len() % 4 == 1 {
        return Err(bad(r));
    }
    let mut out: Vec<u16> = Vec::with_capacity(s.len() / 4 * 3 + 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in &s {
        let v = match c {
            0x41..=0x5A => c - 0x41,
            0x61..=0x7A => c - 0x61 + 26,
            0x30..=0x39 => c - 0x30 + 52,
            0x2B => 62,
            0x2F => 63,
            _ => return Err(bad(r)),
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u16);
            acc &= (1 << bits) - 1;
        }
    }
    r.heap.work += (s.len() >> 3) as u64;
    Ok(r.units_val(out))
}

pub(crate) fn init(r: &mut Realm) {
    let op = r.intr.object_proto;
    let ep = r.alloc(Some(op), ObjKind::Ordinary);
    r.intr.text_encoder_proto = ep;
    let ec = r.ctor("TextEncoder", 0, encoder_ctor, ep);
    let _ = ec;
    r.getter(ep, "encoding", encoder_encoding, None);
    r.method(ep, "encode", 0, encoder_encode);
    r.method(ep, "encodeInto", 2, encoder_encode_into);
    r.to_string_tag(ep, "TextEncoder");

    let dp = r.alloc(Some(op), ObjKind::Ordinary);
    r.intr.text_decoder_proto = dp;
    r.ctor("TextDecoder", 0, decoder_ctor, dp);
    r.getter(dp, "encoding", decoder_encoding, None);
    r.getter(dp, "fatal", decoder_fatal, None);
    r.getter(dp, "ignoreBOM", decoder_ignore_bom, None);
    r.method(dp, "decode", 0, decoder_decode);
    r.to_string_tag(dp, "TextDecoder");

    let g = r.global;
    r.method(g, "btoa", 1, btoa);
    r.method(g, "atob", 1, atob);
}
