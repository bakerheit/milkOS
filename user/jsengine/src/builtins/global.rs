//! The global functions (parseInt, parseFloat, isNaN, isFinite, the URI functions, escape,
//! unescape, eval), the global value properties, and Reflect.

use alloc::vec::Vec;

use crate::api::ScriptOptions;
use crate::names::N;
use crate::object::{ObjKind, PropKey, WC};
use crate::realm::{Realm, Res};
use crate::value::Val;

use super::arg;

fn parse_int(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let s = r.to_units(arg(args, 0))?;
    let radix = r.to_i32(arg(args, 1))?;
    Ok(Val::num(crate::numconv::parse_int(&s, radix)))
}

fn parse_float(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let s = r.to_units(arg(args, 0))?;
    Ok(Val::num(crate::numconv::parse_float(&s)))
}

fn is_nan(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(r.to_num(arg(args, 0))?.is_nan()))
}

fn is_finite(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(r.to_num(arg(args, 0))?.is_finite()))
}

const URI_RESERVED: &str = ";/?:@&=+$,#";
const URI_UNESCAPED_EXTRA: &str = "-_.!~*'()";

fn encode(r: &mut Realm, args: &[Val], keep: &str) -> Res<Val> {
    let u = r.to_units(arg(args, 0))?;
    let mut out: Vec<u16> = Vec::with_capacity(u.len());
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        if c < 128 {
            let ch = c as u8 as char;
            if ch.is_ascii_alphanumeric() || URI_UNESCAPED_EXTRA.contains(ch) || keep.contains(ch) {
                out.push(c);
                i += 1;
                continue;
            }
        }
        let cp = if (0xDC00..0xE000).contains(&c) {
            return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed"));
        } else if (0xD800..0xDC00).contains(&c) {
            match u.get(i + 1) {
                Some(&d) if (0xDC00..0xE000).contains(&d) => {
                    i += 1;
                    0x10000 + (((c as u32) - 0xD800) << 10) + (d as u32 - 0xDC00)
                }
                _ => return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed")),
            }
        } else {
            c as u32
        };
        let mut buf = [0u8; 4];
        let s = char::from_u32(cp)
            .unwrap_or('\u{FFFD}')
            .encode_utf8(&mut buf);
        for b in s.bytes() {
            out.extend(alloc::format!("%{:02X}", b).encode_utf16());
        }
        i += 1;
    }
    Ok(r.units_val(out))
}

fn decode(r: &mut Realm, args: &[Val], reserved: &str) -> Res<Val> {
    let u = r.to_units(arg(args, 0))?;
    let mut out: Vec<u16> = Vec::with_capacity(u.len());
    let hexv = |c: u16| -> Option<u8> {
        char::from_u32(c as u32)
            .and_then(|c| c.to_digit(16))
            .map(|d| d as u8)
    };
    let mut i = 0;
    while i < u.len() {
        if u[i] != b'%' as u16 {
            out.push(u[i]);
            i += 1;
            continue;
        }
        let byte = |i: usize| -> Option<u8> {
            let a = hexv(*u.get(i + 1)?)?;
            let b = hexv(*u.get(i + 2)?)?;
            Some(a * 16 + b)
        };
        let Some(b0) = byte(i) else {
            return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed"));
        };
        if b0 < 0x80 {
            let ch = b0 as char;
            if reserved.contains(ch) {
                out.extend_from_slice(&u[i..i + 3]);
            } else {
                out.push(b0 as u16);
            }
            i += 3;
            continue;
        }
        let n = if b0 & 0xE0 == 0xC0 {
            2
        } else if b0 & 0xF0 == 0xE0 {
            3
        } else if b0 & 0xF8 == 0xF0 {
            4
        } else {
            return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed"));
        };
        let mut bytes = alloc::vec![b0];
        let mut j = i + 3;
        for _ in 1..n {
            if u.get(j) != Some(&(b'%' as u16)) {
                return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed"));
            }
            let Some(b) = byte(j) else {
                return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed"));
            };
            if b & 0xC0 != 0x80 {
                return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed"));
            }
            bytes.push(b);
            j += 3;
        }
        match core::str::from_utf8(&bytes) {
            Ok(s) => out.extend(s.encode_utf16()),
            Err(_) => return Err(r.throw_kind(crate::api::ErrorKind::URIError, "URI malformed")),
        }
        i = j;
    }
    Ok(r.units_val(out))
}

fn encode_uri_component(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    encode(r, args, "")
}
fn encode_uri(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    encode(r, args, URI_RESERVED)
}
fn decode_uri_component(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    decode(r, args, "")
}
fn decode_uri(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    decode(r, args, URI_RESERVED)
}

fn escape(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let u = r.to_units(arg(args, 0))?;
    let mut out = Vec::new();
    for c in u {
        if c < 128 && ((c as u8).is_ascii_alphanumeric() || "@*_+-./".contains(c as u8 as char)) {
            out.push(c);
        } else if c < 256 {
            out.extend(alloc::format!("%{:02X}", c).encode_utf16());
        } else {
            out.extend(alloc::format!("%u{:04X}", c).encode_utf16());
        }
    }
    Ok(r.units_val(out))
}

fn unescape(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let u = r.to_units(arg(args, 0))?;
    let hexv = |c: u16| -> Option<u16> {
        char::from_u32(c as u32)
            .and_then(|c| c.to_digit(16))
            .map(|d| d as u16)
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < u.len() {
        if u[i] == b'%' as u16 {
            if u.get(i + 1) == Some(&(b'u' as u16)) {
                let v: Option<u16> =
                    (2..6).try_fold(0u16, |acc, k| Some(acc * 16 + hexv(*u.get(i + k)?)?));
                if let Some(v) = v {
                    out.push(v);
                    i += 6;
                    continue;
                }
            }
            if let (Some(a), Some(b)) = (
                u.get(i + 1).and_then(|c| hexv(*c)),
                u.get(i + 2).and_then(|c| hexv(*c)),
            ) {
                out.push(a * 16 + b);
                i += 3;
                continue;
            }
        }
        out.push(u[i]);
        i += 1;
    }
    Ok(r.units_val(out))
}

/// eval(x): an indirect (global) eval of x if it is a string.
fn eval(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let x = arg(args, 0);
    let Val::Str(s) = x else { return Ok(x) };
    let src = r.rust_string(s);
    r.eval_script(
        &src,
        &ScriptOptions {
            file: "eval",
            first_line: 1,
            first_column: 1,
        },
    )
}

// ---- Reflect ----

fn need_obj(r: &mut Realm, v: Val, what: &str) -> Res<crate::value::ObjId> {
    match v {
        Val::Obj(o) => Ok(o),
        _ => Err(r.type_error(&alloc::format!("Reflect.{what} called on non-object"))),
    }
}

fn r_apply(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = arg(args, 0);
    if !r.callable(f) {
        return Err(r.type_error("Reflect.apply target is not callable"));
    }
    let list = r.array_like_to_vec(arg(args, 2))?;
    r.call_v(f, arg(args, 1), &list)
}

fn r_construct(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = arg(args, 0);
    let nt = if args.len() > 2 { args[2] } else { f };
    if !r.constructible(f) || !r.constructible(nt) {
        return Err(r.type_error("Reflect.construct target is not a constructor"));
    }
    let list = r.array_like_to_vec(arg(args, 1))?;
    r.construct_v(f, &list, nt)
}

fn r_define_property(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "defineProperty")?;
    let k = r.to_key(arg(args, 1))?;
    let d = r.to_desc(arg(args, 2))?;
    Ok(Val::Bool(r.define_own(o, k, d)?))
}

fn r_delete_property(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "deleteProperty")?;
    let k = r.to_key(arg(args, 1))?;
    Ok(Val::Bool(r.delete_p(o, k)?))
}

fn r_get(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "get")?;
    let k = r.to_key(arg(args, 1))?;
    let recv = if args.len() > 2 { args[2] } else { Val::Obj(o) };
    r.get_p(o, k, recv)
}

fn r_set(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "set")?;
    let k = r.to_key(arg(args, 1))?;
    let recv = if args.len() > 3 { args[3] } else { Val::Obj(o) };
    Ok(Val::Bool(r.set_p(o, k, arg(args, 2), recv)?))
}

fn r_gopd(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "getOwnPropertyDescriptor")?;
    let k = r.to_key(arg(args, 1))?;
    match r.get_own(o, k)? {
        Some(own) => Ok(r.from_desc(own)),
        None => Ok(Val::Undef),
    }
}

fn r_get_prototype_of(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "getPrototypeOf")?;
    Ok(r.o(o).proto.map_or(Val::Null, Val::Obj))
}

fn r_set_prototype_of(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "setPrototypeOf")?;
    let p = match arg(args, 1) {
        Val::Obj(p) => Some(p),
        Val::Null => None,
        _ => return Err(r.type_error("Object prototype may only be an Object or null")),
    };
    Ok(Val::Bool(r.set_proto(o, p)))
}

fn r_has(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "has")?;
    let k = r.to_key(arg(args, 1))?;
    Ok(Val::Bool(r.has_property(o, k)?))
}

fn r_is_extensible(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "isExtensible")?;
    Ok(Val::Bool(r.o(o).flags & crate::object::EXTENSIBLE != 0))
}

fn r_prevent_extensions(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "preventExtensions")?;
    if matches!(r.o(o).kind, ObjKind::Array(_)) {
        r.array_to_sparse(o);
    }
    r.om(o).flags &= !crate::object::EXTENSIBLE;
    Ok(Val::Bool(true))
}

fn r_own_keys(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = need_obj(r, arg(args, 0), "ownKeys")?;
    let keys = r.own_keys_p(o, true, true, false)?;
    let vals: Vec<Val> = keys.into_iter().map(|k| r.key_to_val(k)).collect();
    Ok(Val::Obj(r.new_array_from(vals)))
}

pub(crate) fn init(r: &mut Realm) {
    let g = r.global;
    r.value(g, "NaN", Val::Num(f64::NAN), 0);
    r.value(g, "Infinity", Val::Num(f64::INFINITY), 0);
    r.value(g, "undefined", Val::Undef, 0);
    let pi = r.method(g, "parseInt", 2, parse_int);
    let pf = r.method(g, "parseFloat", 1, parse_float);
    r.method(g, "isNaN", 1, is_nan);
    r.method(g, "isFinite", 1, is_finite);
    r.method(g, "encodeURIComponent", 1, encode_uri_component);
    r.method(g, "encodeURI", 1, encode_uri);
    r.method(g, "decodeURIComponent", 1, decode_uri_component);
    r.method(g, "decodeURI", 1, decode_uri);
    r.method(g, "escape", 1, escape);
    r.method(g, "unescape", 1, unescape);
    let ev = r.method(g, "eval", 1, eval);
    r.intr.eval_fn = ev;
    // Number.parseInt === parseInt.
    let nk = r.key_str("Number");
    if let Some(p) = r.o(g).props.get(nk)
        && let Val::Obj(nc) = p.v
    {
        r.value(nc, "parseInt", Val::Obj(pi), WC);
        r.value(nc, "parseFloat", Val::Obj(pf), WC);
    }
    let op = r.intr.object_proto;
    let refl = r.alloc(Some(op), ObjKind::Ordinary);
    r.value(g, "Reflect", Val::Obj(refl), WC);
    r.method(refl, "apply", 3, r_apply);
    r.method(refl, "construct", 2, r_construct);
    r.method(refl, "defineProperty", 3, r_define_property);
    r.method(refl, "deleteProperty", 2, r_delete_property);
    r.method(refl, "get", 2, r_get);
    r.method(refl, "set", 3, r_set);
    r.method(refl, "getOwnPropertyDescriptor", 2, r_gopd);
    r.method(refl, "getPrototypeOf", 1, r_get_prototype_of);
    r.method(refl, "setPrototypeOf", 2, r_set_prototype_of);
    r.method(refl, "has", 2, r_has);
    r.method(refl, "isExtensible", 1, r_is_extensible);
    r.method(refl, "preventExtensions", 1, r_prevent_extensions);
    r.method(refl, "ownKeys", 1, r_own_keys);
    r.to_string_tag(refl, "Reflect");
    let _ = (N::empty, PropKey::Idx(0));
}
