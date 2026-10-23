//! String, String.prototype, string iterators, and the string halves of match/replace/
//! search/split (RegExp arguments dispatch to their Symbol methods).

use alloc::string::String;
use alloc::vec::Vec;

use crate::names::N;
use crate::object::{ObjKind, PropKey, WC};
use crate::realm::{Realm, Res};
use crate::string::{find, rfind};
use crate::value::{StrId, Val};

use super::arg;

pub(crate) struct StrIterData {
    pub s: StrId,
    pub pos: usize,
}

impl Realm {
    /// RequireObjectCoercible(this) then ToString.
    fn this_str(&mut self, this: Val, name: &str) -> Res<StrId> {
        match this {
            Val::Str(s) => Ok(s),
            Val::Undef | Val::Null | Val::Empty => Err(self.type_error(&alloc::format!(
                "String.prototype.{name} called on null or undefined"
            ))),
            _ => self.to_str(this),
        }
    }

    pub(crate) fn units_val(&mut self, u: Vec<u16>) -> Val {
        Val::Str(self.heap.new_str_vec(u))
    }

    /// GetSubstitution: expands $-patterns in a replacement string.
    pub(crate) fn get_substitution(
        &mut self,
        matched: &[u16],
        s: &[u16],
        pos: usize,
        captures: &[Option<Vec<u16>>],
        named: Val,
        repl: &[u16],
    ) -> Res<Vec<u16>> {
        let mut out = Vec::with_capacity(repl.len());
        let m = captures.len();
        let mut i = 0;
        while i < repl.len() {
            let c = repl[i];
            if c != b'$' as u16 || i + 1 >= repl.len() {
                out.push(c);
                i += 1;
                continue;
            }
            let n = repl[i + 1];
            match n {
                0x24 => {
                    out.push(b'$' as u16);
                    i += 2;
                }
                0x26 => {
                    out.extend_from_slice(matched);
                    i += 2;
                }
                0x60 => {
                    out.extend_from_slice(&s[..pos.min(s.len())]);
                    i += 2;
                }
                0x27 => {
                    let end = (pos + matched.len()).min(s.len());
                    out.extend_from_slice(&s[end..]);
                    i += 2;
                }
                0x30..=0x39 => {
                    let d1 = (n - 0x30) as usize;
                    let two = if i + 2 < repl.len() && crate::string::is_ascii_digit(repl[i + 2]) {
                        Some(d1 * 10 + (repl[i + 2] - 0x30) as usize)
                    } else {
                        None
                    };
                    if let Some(t) = two
                        && t >= 1
                        && t <= m
                    {
                        if let Some(Some(cap)) = captures.get(t - 1) {
                            out.extend_from_slice(cap);
                        }
                        i += 3;
                    } else if d1 >= 1 && d1 <= m {
                        if let Some(Some(cap)) = captures.get(d1 - 1) {
                            out.extend_from_slice(cap);
                        }
                        i += 2;
                    } else {
                        out.push(c);
                        i += 1;
                    }
                }
                0x3c if !named.is_undef() => {
                    // $<name>
                    let rest = &repl[i + 2..];
                    match rest.iter().position(|&x| x == b'>' as u16) {
                        Some(end) => {
                            let name = &rest[..end];
                            let k = self.key_units(name);
                            let v = self.get_v(named, k)?;
                            if !v.is_undef() {
                                let vs = self.to_str(v)?;
                                out.extend_from_slice(self.heap.units(vs));
                            }
                            i += 3 + end;
                        }
                        None => {
                            out.push(c);
                            i += 1;
                        }
                    }
                }
                _ => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        Ok(out)
    }
}

fn string_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    let s = if args.is_empty() {
        r.nstr(N::empty)
    } else {
        match (args[0], nt.is_undef()) {
            (Val::Sym(_), true) => {
                let d = r.symbol_descriptive(args[0]);
                return Ok(r.str_val(&d));
            }
            (v, _) => r.to_str(v)?,
        }
    };
    if nt.is_undef() {
        return Ok(Val::Str(s));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.string_proto)?;
    Ok(Val::Obj(r.alloc(Some(proto), ObjKind::StrBox(s))))
}

fn from_char_code(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let mut u = Vec::with_capacity(args.len());
    for a in args {
        u.push(r.to_uint32(*a)? as u16);
    }
    Ok(r.units_val(u))
}

fn from_code_point(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let mut u = Vec::with_capacity(args.len());
    for a in args {
        let n = r.to_num(*a)?;
        if !(0.0..=1114111.0).contains(&n) || crate::math::trunc(n) != n {
            let d = crate::numconv::number_to_string(n);
            return Err(r.range_error(&alloc::format!("Invalid code point {d}")));
        }
        let cp = n as u32;
        if cp >= 0x10000 {
            let c = cp - 0x10000;
            u.push(0xD800 + (c >> 10) as u16);
            u.push(0xDC00 + (c & 0x3ff) as u16);
        } else {
            u.push(cp as u16);
        }
    }
    Ok(r.units_val(u))
}

fn raw(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let cooked = r.to_object(arg(args, 0))?;
    let rk = r.key(N::raw);
    let rawv = r.get_p(cooked, rk, Val::Obj(cooked))?;
    let ro = r.to_object(rawv)?;
    let len = r.length_of(Val::Obj(ro))?;
    let mut out = Vec::new();
    let mut i = 0.0;
    while i < len {
        let v = r.get_index(Val::Obj(ro), i)?;
        let s = r.to_str(v)?;
        out.extend_from_slice(r.heap.units(s));
        let j = (i as usize) + 1;
        if (j as f64) < len && j < args.len() {
            let s = r.to_str(args[j])?;
            out.extend_from_slice(r.heap.units(s));
        }
        i += 1.0;
    }
    Ok(r.units_val(out))
}

// ---- prototype ----

fn to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    match this {
        Val::Str(_) => Ok(this),
        Val::Obj(o) => match r.o(o).kind {
            ObjKind::StrBox(s) => Ok(Val::Str(s)),
            _ => Err(r.type_error("String.prototype.toString requires that 'this' be a String")),
        },
        _ => Err(r.type_error("String.prototype.toString requires that 'this' be a String")),
    }
}

fn at(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "at")?;
    let len = r.heap.str_len(s) as f64;
    let rel = r.to_integer(arg(args, 0))?;
    let k = if rel >= 0.0 { rel } else { len + rel };
    if k < 0.0 || k >= len {
        return Ok(Val::Undef);
    }
    let c = r.heap.units(s)[k as usize];
    Ok(Val::Str(r.char_str(c)))
}

fn char_at(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "charAt")?;
    let p = r.to_integer(arg(args, 0))?;
    let len = r.heap.str_len(s) as f64;
    if p < 0.0 || p >= len {
        return Ok(Val::Str(r.nstr(N::empty)));
    }
    let c = r.heap.units(s)[p as usize];
    Ok(Val::Str(r.char_str(c)))
}

fn char_code_at(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "charCodeAt")?;
    let p = r.to_integer(arg(args, 0))?;
    let len = r.heap.str_len(s) as f64;
    if p < 0.0 || p >= len {
        return Ok(Val::Num(f64::NAN));
    }
    Ok(Val::Int(r.heap.units(s)[p as usize] as i32))
}

fn code_point_at(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "codePointAt")?;
    let p = r.to_integer(arg(args, 0))?;
    let u = r.heap.units(s);
    if p < 0.0 || p >= u.len() as f64 {
        return Ok(Val::Undef);
    }
    let i = p as usize;
    let c = u[i];
    if (0xD800..0xDC00).contains(&c) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) {
        let cp = 0x10000 + (((c as u32) - 0xD800) << 10) + ((u[i + 1] as u32) - 0xDC00);
        return Ok(Val::Int(cp as i32));
    }
    Ok(Val::Int(c as i32))
}

fn concat(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let mut s = r.this_str(this, "concat")?;
    for a in args {
        let t = r.to_str(*a)?;
        s = r.concat(s, t)?;
    }
    Ok(Val::Str(s))
}

fn is_regexp(r: &mut Realm, v: Val) -> Res<bool> {
    let Val::Obj(o) = v else { return Ok(false) };
    let mk = PropKey::Sym(r.sym.match_);
    let m = r.get_p(o, mk, v)?;
    if !m.is_undef() {
        return Ok(r.truthy(m));
    }
    Ok(matches!(r.o(o).kind, ObjKind::RegExp(_)))
}

fn ends_with(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "endsWith")?;
    if is_regexp(r, arg(args, 0))? {
        return Err(r.type_error(
            "First argument to String.prototype.endsWith must not be a regular expression",
        ));
    }
    let needle = r.to_units(arg(args, 0))?;
    let u = r.heap.units(s).to_vec();
    let end = if arg(args, 1).is_undef() {
        u.len() as f64
    } else {
        r.to_integer(arg(args, 1))?.clamp(0.0, u.len() as f64)
    };
    let end = end as usize;
    if needle.len() > end {
        return Ok(Val::Bool(false));
    }
    Ok(Val::Bool(u[end - needle.len()..end] == needle[..]))
}

fn starts_with(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "startsWith")?;
    if is_regexp(r, arg(args, 0))? {
        return Err(r.type_error(
            "First argument to String.prototype.startsWith must not be a regular expression",
        ));
    }
    let needle = r.to_units(arg(args, 0))?;
    let start = r.to_integer(arg(args, 1))?;
    let u = r.heap.units(s);
    let start = start.clamp(0.0, u.len() as f64) as usize;
    if start + needle.len() > u.len() {
        return Ok(Val::Bool(false));
    }
    Ok(Val::Bool(u[start..start + needle.len()] == needle[..]))
}

fn includes(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "includes")?;
    if is_regexp(r, arg(args, 0))? {
        return Err(r.type_error(
            "First argument to String.prototype.includes must not be a regular expression",
        ));
    }
    let needle = r.to_units(arg(args, 0))?;
    let start = r.to_integer(arg(args, 1))?;
    let u = r.heap.units(s);
    let start = start.clamp(0.0, u.len() as f64) as usize;
    Ok(Val::Bool(find(u, &needle, start).is_some()))
}

fn index_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "indexOf")?;
    let needle = r.to_units(arg(args, 0))?;
    let start = r.to_integer(arg(args, 1))?;
    let u = r.heap.units(s);
    let start = start.clamp(0.0, u.len() as f64) as usize;
    Ok(Val::Int(find(u, &needle, start).map_or(-1, |i| i as i32)))
}

fn last_index_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "lastIndexOf")?;
    let needle = r.to_units(arg(args, 0))?;
    let n = r.to_num(arg(args, 1))?;
    let u = r.heap.units(s);
    let start = if n.is_nan() {
        u.len()
    } else {
        crate::conv::to_integer_f(n).clamp(0.0, u.len() as f64) as usize
    };
    Ok(Val::Int(rfind(u, &needle, start).map_or(-1, |i| i as i32)))
}

fn fold_char(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn locale_compare(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "localeCompare")?;
    let a = r.rust_string(s);
    let b = r.to_rust_string(arg(args, 0))?;
    // An approximation of a locale collation: compare case-folded first (letters before
    // digits only as Unicode orders them), then lowercase before uppercase.
    let ka: Vec<char> = a.chars().map(fold_char).collect();
    let kb: Vec<char> = b.chars().map(fold_char).collect();
    let ord = ka.cmp(&kb).then_with(|| {
        let ca: Vec<bool> = a.chars().map(|c| c.is_uppercase()).collect();
        let cb: Vec<bool> = b.chars().map(|c| c.is_uppercase()).collect();
        ca.cmp(&cb)
    });
    Ok(Val::Int(match ord {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }))
}

fn normalize(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "normalize")?;
    let f = arg(args, 0);
    if !f.is_undef() {
        let form = r.to_rust_string(f)?;
        if !matches!(form.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
            return Err(
                r.range_error("The normalization form should be one of NFC, NFD, NFKC, NFKD.")
            );
        }
    }
    // Normalization tables are not built in: strings are returned unchanged.
    Ok(Val::Str(s))
}

fn pad(r: &mut Realm, this: Val, args: &[Val], at_start: bool) -> Res<Val> {
    let s = r.this_str(this, if at_start { "padStart" } else { "padEnd" })?;
    let max = r.to_length(arg(args, 0))?;
    let len = r.heap.str_len(s) as f64;
    if max <= len {
        return Ok(Val::Str(s));
    }
    let filler = match arg(args, 1) {
        Val::Undef => alloc::vec![b' ' as u16],
        v => r.to_units(v)?,
    };
    if filler.is_empty() {
        return Ok(Val::Str(s));
    }
    if max > crate::string::MAX_LEN as f64 {
        return Err(r.range_error("Invalid string length"));
    }
    if !r.heap.could_fit(max as usize * 2) {
        return Err(r.oom());
    }
    let fill_len = (max - len) as usize;
    let mut f = Vec::with_capacity(fill_len);
    while f.len() < fill_len {
        let take = (fill_len - f.len()).min(filler.len());
        f.extend_from_slice(&filler[..take]);
    }
    let u = r.heap.units(s);
    let out = if at_start {
        let mut o = f;
        o.extend_from_slice(u);
        o
    } else {
        let mut o = u.to_vec();
        o.extend_from_slice(&f);
        o
    };
    Ok(r.units_val(out))
}

fn pad_start(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    pad(r, this, args, true)
}
fn pad_end(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    pad(r, this, args, false)
}

fn repeat(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "repeat")?;
    let n = r.to_integer(arg(args, 0))?;
    if n < 0.0 || n == f64::INFINITY {
        let d = crate::numconv::number_to_string(n);
        return Err(r.range_error(&alloc::format!("Invalid count value: {d}")));
    }
    let len = r.heap.str_len(s);
    if len == 0 || n == 0.0 {
        return Ok(Val::Str(r.nstr(N::empty)));
    }
    let total = len as f64 * n;
    if total > crate::string::MAX_LEN as f64 {
        return Err(r.range_error("Invalid string length"));
    }
    if !r.heap.could_fit(total as usize * 2) {
        return Err(r.oom());
    }
    let u = r.heap.units(s).to_vec();
    let mut out = Vec::with_capacity(total as usize);
    for _ in 0..n as usize {
        out.extend_from_slice(&u);
    }
    Ok(r.units_val(out))
}

fn slice(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "slice")?;
    let len = r.heap.str_len(s) as f64;
    let a = r.rel_index(arg(args, 0), len, 0.0)?;
    let b = r.rel_index(arg(args, 1), len, len)?;
    if a >= b {
        return Ok(Val::Str(r.nstr(N::empty)));
    }
    if a == 0.0 && b == len {
        return Ok(Val::Str(s));
    }
    let u = r.heap.units(s)[a as usize..b as usize].to_vec();
    Ok(r.units_val(u))
}

fn substring(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "substring")?;
    let len = r.heap.str_len(s) as f64;
    let a = r.to_integer(arg(args, 0))?.clamp(0.0, len);
    let b = if arg(args, 1).is_undef() {
        len
    } else {
        r.to_integer(arg(args, 1))?.clamp(0.0, len)
    };
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    if lo == 0.0 && hi == len {
        return Ok(Val::Str(s));
    }
    let u = r.heap.units(s)[lo as usize..hi as usize].to_vec();
    Ok(r.units_val(u))
}

fn substr(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "substr")?;
    let len = r.heap.str_len(s) as f64;
    let start = r.rel_index(arg(args, 0), len, 0.0)?;
    let n = if arg(args, 1).is_undef() {
        len - start
    } else {
        r.to_integer(arg(args, 1))?.clamp(0.0, len - start)
    };
    if n <= 0.0 {
        return Ok(Val::Str(r.nstr(N::empty)));
    }
    let u = r.heap.units(s)[start as usize..(start + n) as usize].to_vec();
    Ok(r.units_val(u))
}

pub(crate) fn case_map(u: &[u16], upper: bool) -> Vec<u16> {
    if u.iter().all(|&c| c < 128) {
        return u
            .iter()
            .map(|&c| {
                let b = c as u8;
                (if upper {
                    b.to_ascii_uppercase()
                } else {
                    b.to_ascii_lowercase()
                }) as u16
            })
            .collect();
    }
    let mut out = Vec::with_capacity(u.len());
    let mut buf = [0u16; 2];
    let chars: Vec<Result<char, u16>> = core::char::decode_utf16(u.iter().copied())
        .map(|r| r.map_err(|e| e.unpaired_surrogate()))
        .collect();
    for (i, c) in chars.iter().enumerate() {
        match c {
            Ok(c) => {
                if upper {
                    for m in c.to_uppercase() {
                        out.extend_from_slice(m.encode_utf16(&mut buf));
                    }
                } else if *c == 'Σ' {
                    // Final sigma: at the end of a word.
                    let next_is_letter =
                        matches!(chars.get(i + 1), Some(Ok(n)) if n.is_alphabetic());
                    let prev_is_letter =
                        i > 0 && matches!(chars.get(i - 1), Some(Ok(p)) if p.is_alphabetic());
                    let final_sigma = prev_is_letter && !next_is_letter;
                    out.push(if final_sigma {
                        'ς' as u16
                    } else {
                        'σ' as u16
                    });
                } else {
                    for m in c.to_lowercase() {
                        out.extend_from_slice(m.encode_utf16(&mut buf));
                    }
                }
            }
            Err(s) => out.push(*s),
        }
    }
    out
}

fn to_lower(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "toLowerCase")?;
    let u = case_map(r.heap.units(s), false);
    Ok(r.units_val(u))
}

fn to_upper(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "toUpperCase")?;
    let u = case_map(r.heap.units(s), true);
    Ok(r.units_val(u))
}

fn trim_impl(r: &mut Realm, this: Val, start: bool, end: bool) -> Res<Val> {
    let s = r.this_str(this, "trim")?;
    let u = r.heap.units(s);
    let t = crate::string::trim(u, start, end);
    if t.len() == u.len() {
        return Ok(Val::Str(s));
    }
    let t = t.to_vec();
    Ok(r.units_val(t))
}

fn trim(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    trim_impl(r, this, true, true)
}
fn trim_start(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    trim_impl(r, this, true, false)
}
fn trim_end(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    trim_impl(r, this, false, true)
}

/// Calls `regexp[sym](this, ...rest)` if the argument has that method.
fn dispatch_sym(
    r: &mut Realm,
    this: Val,
    re: Val,
    sym: crate::value::SymId,
    rest: &[Val],
) -> Res<Option<Val>> {
    if re.is_nullish() {
        return Ok(None);
    }
    let k = PropKey::Sym(sym);
    let m = r.get_v(re, k)?;
    if m.is_nullish() {
        return Ok(None);
    }
    let mut a = alloc::vec![this];
    a.extend_from_slice(rest);
    Ok(Some(r.call_v(m, re, &a)?))
}

fn match_(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if this.is_nullish() {
        return Err(r.type_error("String.prototype.match called on null or undefined"));
    }
    let re = arg(args, 0);
    let sym = r.sym.match_;
    if let Some(v) = dispatch_sym(r, this, re, sym, &[])? {
        return Ok(v);
    }
    let s = r.to_str(this)?;
    let rx = r.regexp_create(re, Val::Undef, Val::Undef)?;
    let m = r.get_v(Val::Obj(rx), PropKey::Sym(sym))?;
    r.call_v(m, Val::Obj(rx), &[Val::Str(s)])
}

fn match_all(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if this.is_nullish() {
        return Err(r.type_error("String.prototype.matchAll called on null or undefined"));
    }
    let re = arg(args, 0);
    if is_regexp(r, re)? {
        let fk = r.key(N::flags);
        let f = r.get_v(re, fk)?;
        let fu = r.to_units(f)?;
        if !fu.contains(&(b'g' as u16)) {
            return Err(
                r.type_error("String.prototype.matchAll called with a non-global RegExp argument")
            );
        }
    }
    let sym = r.sym.match_all;
    if let Some(v) = dispatch_sym(r, this, re, sym, &[])? {
        return Ok(v);
    }
    let s = r.to_str(this)?;
    let g = r.str_val("g");
    let rx = r.regexp_create(re, g, Val::Undef)?;
    let m = r.get_v(Val::Obj(rx), PropKey::Sym(sym))?;
    r.call_v(m, Val::Obj(rx), &[Val::Str(s)])
}

fn search(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if this.is_nullish() {
        return Err(r.type_error("String.prototype.search called on null or undefined"));
    }
    let re = arg(args, 0);
    let sym = r.sym.search;
    if let Some(v) = dispatch_sym(r, this, re, sym, &[])? {
        return Ok(v);
    }
    let s = r.to_str(this)?;
    let rx = r.regexp_create(re, Val::Undef, Val::Undef)?;
    let m = r.get_v(Val::Obj(rx), PropKey::Sym(sym))?;
    r.call_v(m, Val::Obj(rx), &[Val::Str(s)])
}

fn replace_impl(r: &mut Realm, this: Val, args: &[Val], all: bool) -> Res<Val> {
    if this.is_nullish() {
        return Err(r.type_error("String.prototype.replace called on null or undefined"));
    }
    let pat = arg(args, 0);
    let repl = arg(args, 1);
    if all && is_regexp(r, pat)? {
        let fk = r.key(N::flags);
        let f = r.get_v(pat, fk)?;
        let fu = r.to_units(f)?;
        if !fu.contains(&(b'g' as u16)) {
            return Err(r.type_error("replaceAll must be called with a global RegExp"));
        }
    }
    let sym = r.sym.replace;
    if let Some(v) = dispatch_sym(r, this, pat, sym, &[repl])? {
        return Ok(v);
    }
    let s = r.to_str(this)?;
    let su = r.heap.units(s).to_vec();
    let pu = r.to_units(pat)?;
    let func = r.callable(repl);
    let ru = if func { Vec::new() } else { r.to_units(repl)? };
    let mut positions = Vec::new();
    let mut from = 0;
    while let Some(p) = find(&su, &pu, from) {
        positions.push(p);
        if !all {
            break;
        }
        from = p + pu.len().max(1);
        if from > su.len() {
            break;
        }
    }
    if positions.is_empty() {
        return Ok(Val::Str(s));
    }
    let mut out = Vec::with_capacity(su.len());
    let mut last = 0;
    for p in positions {
        out.extend_from_slice(&su[last..p]);
        if func {
            let m = r.units_val(pu.clone());
            let v = r.call_v(repl, Val::Undef, &[m, Val::num(p as f64), Val::Str(s)])?;
            let vs = r.to_str(v)?;
            out.extend_from_slice(r.heap.units(vs));
        } else {
            let sub = r.get_substitution(&pu, &su, p, &[], Val::Undef, &ru)?;
            out.extend_from_slice(&sub);
        }
        last = p + pu.len();
        if out.len() > crate::string::MAX_LEN {
            return Err(r.range_error("Invalid string length"));
        }
    }
    out.extend_from_slice(&su[last..]);
    Ok(r.units_val(out))
}

fn replace(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    replace_impl(r, this, args, false)
}
fn replace_all(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    replace_impl(r, this, args, true)
}

fn split(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if this.is_nullish() {
        return Err(r.type_error("String.prototype.split called on null or undefined"));
    }
    let sep = arg(args, 0);
    let lim = arg(args, 1);
    let sym = r.sym.split;
    if let Some(v) = dispatch_sym(r, this, sep, sym, &[lim])? {
        return Ok(v);
    }
    let s = r.to_str(this)?;
    let limit = if lim.is_undef() {
        u32::MAX
    } else {
        r.to_uint32(lim)?
    };
    let su = r.heap.units(s).to_vec();
    if limit == 0 {
        return Ok(Val::Obj(r.new_array_from(Vec::new())));
    }
    if sep.is_undef() {
        return Ok(Val::Obj(r.new_array_from(alloc::vec![Val::Str(s)])));
    }
    let pu = r.to_units(sep)?;
    let mut parts: Vec<Val> = Vec::new();
    if pu.is_empty() {
        for (i, &c) in su.iter().enumerate() {
            if parts.len() as u32 >= limit {
                break;
            }
            parts.push(Val::Str(r.char_str(c)));
            if i & 4095 == 0 {
                r.check_mem()?;
            }
        }
        return Ok(Val::Obj(r.new_array_from(parts)));
    }
    let mut last = 0;
    let mut from = 0;
    while let Some(p) = find(&su, &pu, from) {
        if parts.len() as u32 >= limit {
            break;
        }
        let piece = su[last..p].to_vec();
        parts.push(r.units_val(piece));
        last = p + pu.len();
        from = last;
        if parts.len() & 4095 == 0 {
            r.check_mem()?;
        }
    }
    if (parts.len() as u32) < limit {
        let piece = su[last..].to_vec();
        parts.push(r.units_val(piece));
    }
    Ok(Val::Obj(r.new_array_from(parts)))
}

fn value_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    to_string(r, this, args)
}

fn str_iter(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "[Symbol.iterator]")?;
    let p = r.intr.string_iterator_proto;
    Ok(Val::Obj(r.alloc(
        Some(p),
        ObjKind::StrIter(alloc::boxed::Box::new(StrIterData { s, pos: 0 })),
    )))
}

fn str_iter_next(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(io) = this else {
        return Err(r.type_error("next called on incompatible receiver"));
    };
    let (s, pos) = match &r.o(io).kind {
        ObjKind::StrIter(d) => (d.s, d.pos),
        _ => return Err(r.type_error("next called on incompatible receiver")),
    };
    let u = r.heap.units(s);
    if pos >= u.len() {
        return Ok(r.iter_result(Val::Undef, true));
    }
    let c = u[pos];
    let n = if (0xD800..0xDC00).contains(&c)
        && pos + 1 < u.len()
        && (0xDC00..0xE000).contains(&u[pos + 1])
    {
        2
    } else {
        1
    };
    let piece = u[pos..pos + n].to_vec();
    if let ObjKind::StrIter(d) = &mut r.om(io).kind {
        d.pos += n;
    }
    let v = if n == 1 {
        Val::Str(r.char_str(piece[0]))
    } else {
        r.units_val(piece)
    };
    Ok(r.iter_result(v, false))
}

fn html(r: &mut Realm, this: Val, tag: &str, attr: Option<(&str, Val)>) -> Res<Val> {
    let s = r.this_str(this, tag)?;
    let body = r.rust_string(s);
    let a = match attr {
        Some((n, v)) => {
            let vs = r.to_rust_string(v)?.replace('"', "&quot;");
            alloc::format!(" {n}=\"{vs}\"")
        }
        None => String::new(),
    };
    Ok(r.str_val(&alloc::format!("<{tag}{a}>{body}</{tag}>")))
}

fn anchor(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    html(r, this, "a", Some(("name", arg(args, 0))))
}
fn link(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    html(r, this, "a", Some(("href", arg(args, 0))))
}
fn bold(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    html(r, this, "b", None)
}

pub(crate) fn init(r: &mut Realm) {
    let sp = r.intr.string_proto;
    let c = r.ctor("String", 1, string_ctor, sp);
    r.method(c, "fromCharCode", 1, from_char_code);
    r.method(c, "fromCodePoint", 1, from_code_point);
    r.method(c, "raw", 1, raw);
    for (n, a, f) in [
        ("at", 1, at as crate::object::BuiltinFn),
        ("charAt", 1, char_at),
        ("charCodeAt", 1, char_code_at),
        ("codePointAt", 1, code_point_at),
        ("concat", 1, concat),
        ("endsWith", 1, ends_with),
        ("includes", 1, includes),
        ("indexOf", 1, index_of),
        ("lastIndexOf", 1, last_index_of),
        ("localeCompare", 1, locale_compare),
        ("match", 1, match_),
        ("matchAll", 1, match_all),
        ("normalize", 0, normalize),
        ("padEnd", 1, pad_end),
        ("padStart", 1, pad_start),
        ("repeat", 1, repeat),
        ("replace", 2, replace),
        ("replaceAll", 2, replace_all),
        ("search", 1, search),
        ("slice", 2, slice),
        ("split", 2, split),
        ("startsWith", 1, starts_with),
        ("substring", 2, substring),
        ("substr", 2, substr),
        ("toLowerCase", 0, to_lower),
        ("toLocaleLowerCase", 0, to_lower),
        ("toUpperCase", 0, to_upper),
        ("toLocaleUpperCase", 0, to_upper),
        ("toString", 0, to_string),
        ("valueOf", 0, value_of),
        ("trim", 0, trim),
        ("anchor", 1, anchor),
        ("link", 1, link),
        ("bold", 0, bold),
    ] {
        r.method(sp, n, a, f);
    }
    let ts = r.method(sp, "trimStart", 0, trim_start);
    let te = r.method(sp, "trimEnd", 0, trim_end);
    r.value(sp, "trimLeft", Val::Obj(ts), WC);
    r.value(sp, "trimRight", Val::Obj(te), WC);
    let ik = r.sym.iterator;
    r.method_sym(sp, ik, "[Symbol.iterator]", 0, str_iter);
    let sip = r.intr.string_iterator_proto;
    r.method(sip, "next", 0, str_iter_next);
    r.to_string_tag(sip, "String Iterator");
}
