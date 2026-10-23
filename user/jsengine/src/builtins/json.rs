//! JSON.parse (with reviver) and JSON.stringify (replacer function or list, indentation,
//! toJSON, cycle detection).

use alloc::vec::Vec;

use crate::names::N;
use crate::object::{E, ObjKind, PropKey};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, StrId, Val};

use super::arg;

const MAX_DEPTH: u32 = 2000;

struct JsonParser<'a> {
    s: &'a [u16],
    i: usize,
    depth: u32,
}

impl<'a> JsonParser<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], 0x20 | 0x09 | 0x0a | 0x0d) {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u16> {
        self.s.get(self.i).copied()
    }
}

impl Realm {
    fn json_err(&mut self, p: &JsonParser, what: &str) -> crate::api::Exception {
        let msg = match p.peek() {
            None => alloc::format!("Unexpected end of JSON input{what}"),
            Some(c) => {
                let ch = char::from_u32(c as u32).unwrap_or('?');
                alloc::format!("Unexpected token '{ch}' in JSON at position {}{what}", p.i)
            }
        };
        self.syntax_error(&msg)
    }

    pub(crate) fn json_parse_units(&mut self, u: &[u16]) -> Res<Val> {
        let mut p = JsonParser {
            s: u,
            i: 0,
            depth: 0,
        };
        p.ws();
        let v = self.json_value(&mut p)?;
        p.ws();
        if p.i != u.len() {
            return Err(self.json_err(&p, ""));
        }
        Ok(v)
    }

    fn json_value(&mut self, p: &mut JsonParser) -> Res<Val> {
        self.check_stack()?;
        p.ws();
        let Some(c) = p.peek() else {
            return Err(self.json_err(p, ""));
        };
        match c {
            0x7b => {
                p.depth += 1;
                if p.depth > MAX_DEPTH {
                    return Err(self.range_error("JSON nested too deeply"));
                }
                p.i += 1;
                let o = self.new_plain();
                p.ws();
                if p.peek() == Some(0x7d) {
                    p.i += 1;
                    p.depth -= 1;
                    return Ok(Val::Obj(o));
                }
                loop {
                    p.ws();
                    if p.peek() != Some(0x22) {
                        return Err(self.json_err(p, ""));
                    }
                    let k = self.json_string(p)?;
                    p.ws();
                    if p.peek() != Some(0x3a) {
                        return Err(self.json_err(p, ""));
                    }
                    p.i += 1;
                    let v = self.json_value(p)?;
                    let key = self.key_units(&k);
                    self.create_data(o, key, v)?;
                    p.ws();
                    match p.peek() {
                        Some(0x2c) => p.i += 1,
                        Some(0x7d) => {
                            p.i += 1;
                            break;
                        }
                        _ => return Err(self.json_err(p, "")),
                    }
                    if self.heap.over_hard_limit() {
                        return Err(self.oom());
                    }
                }
                p.depth -= 1;
                Ok(Val::Obj(o))
            }
            0x5b => {
                p.depth += 1;
                if p.depth > MAX_DEPTH {
                    return Err(self.range_error("JSON nested too deeply"));
                }
                p.i += 1;
                let mut items = Vec::new();
                p.ws();
                if p.peek() == Some(0x5d) {
                    p.i += 1;
                    p.depth -= 1;
                    return Ok(Val::Obj(self.new_array_from(items)));
                }
                loop {
                    items.push(self.json_value(p)?);
                    p.ws();
                    match p.peek() {
                        Some(0x2c) => p.i += 1,
                        Some(0x5d) => {
                            p.i += 1;
                            break;
                        }
                        _ => return Err(self.json_err(p, "")),
                    }
                    if items.len() & 1023 == 0 && self.heap.over_hard_limit() {
                        return Err(self.oom());
                    }
                }
                p.depth -= 1;
                Ok(Val::Obj(self.new_array_from(items)))
            }
            0x22 => {
                let s = self.json_string(p)?;
                Ok(Val::Str(self.heap.new_str_vec(s)))
            }
            0x74 | 0x66 | 0x6e => {
                for (lit, v) in [
                    ("true", Val::Bool(true)),
                    ("false", Val::Bool(false)),
                    ("null", Val::Null),
                ] {
                    let lu: Vec<u16> = lit.encode_utf16().collect();
                    if p.s[p.i..].starts_with(&lu) {
                        p.i += lu.len();
                        return Ok(v);
                    }
                }
                Err(self.json_err(p, ""))
            }
            _ => {
                // number: -? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE][+-]?[0-9]+)?
                let start = p.i;
                if p.peek() == Some(0x2d) {
                    p.i += 1;
                }
                let digits = |p: &mut JsonParser| {
                    let s = p.i;
                    while p.i < p.s.len() && crate::string::is_ascii_digit(p.s[p.i]) {
                        p.i += 1;
                    }
                    p.i - s
                };
                match p.peek() {
                    Some(0x30) => p.i += 1,
                    Some(c) if crate::string::is_ascii_digit(c) => {
                        digits(p);
                    }
                    _ => return Err(self.json_err(p, "")),
                }
                if p.peek() == Some(0x2e) {
                    p.i += 1;
                    if digits(p) == 0 {
                        return Err(self.json_err(p, ""));
                    }
                }
                if matches!(p.peek(), Some(0x65) | Some(0x45)) {
                    p.i += 1;
                    if matches!(p.peek(), Some(0x2b) | Some(0x2d)) {
                        p.i += 1;
                    }
                    if digits(p) == 0 {
                        return Err(self.json_err(p, ""));
                    }
                }
                let text: alloc::string::String =
                    p.s[start..p.i].iter().map(|&c| c as u8 as char).collect();
                let n: f64 = text.parse().unwrap_or(f64::NAN);
                Ok(Val::num(n))
            }
        }
    }

    fn json_string(&mut self, p: &mut JsonParser) -> Res<Vec<u16>> {
        p.i += 1; // opening quote
        let mut out = Vec::new();
        loop {
            let Some(c) = p.peek() else {
                return Err(self.syntax_error("Unterminated string in JSON"));
            };
            p.i += 1;
            match c {
                0x22 => return Ok(out),
                0x5c => {
                    let Some(e) = p.peek() else {
                        return Err(self.syntax_error("Bad escaped character in JSON"));
                    };
                    p.i += 1;
                    match e {
                        0x22 => out.push(0x22),
                        0x5c => out.push(0x5c),
                        0x2f => out.push(0x2f),
                        0x62 => out.push(8),
                        0x66 => out.push(12),
                        0x6e => out.push(10),
                        0x72 => out.push(13),
                        0x74 => out.push(9),
                        0x75 => {
                            let mut v: u16 = 0;
                            for _ in 0..4 {
                                let d = p
                                    .peek()
                                    .and_then(|c| char::from_u32(c as u32))
                                    .and_then(|c| c.to_digit(16));
                                match d {
                                    Some(d) => {
                                        v = v * 16 + d as u16;
                                        p.i += 1;
                                    }
                                    None => {
                                        return Err(self.syntax_error("Bad Unicode escape in JSON"));
                                    }
                                }
                            }
                            out.push(v);
                        }
                        _ => {
                            p.i -= 1;
                            return Err(self.json_err(p, ""));
                        }
                    }
                }
                0..=0x1f => {
                    p.i -= 1;
                    return Err(self.syntax_error(&alloc::format!(
                        "Bad control character in string literal in JSON at position {}",
                        p.i
                    )));
                }
                _ => out.push(c),
            }
        }
    }

    /// InternalizeJSONProperty, post-order.
    fn internalize(&mut self, holder: ObjId, key: PropKey, reviver: Val, depth: u32) -> Res<Val> {
        if depth > MAX_DEPTH {
            return Err(self.range_error("JSON nested too deeply"));
        }
        self.check_stack()?;
        let val = self.get_p(holder, key, Val::Obj(holder))?;
        if let Val::Obj(o) = val {
            let mark = self.temps.len();
            self.temps.push(val);
            let is_arr = matches!(self.o(o).kind, ObjKind::Array(_));
            let keys: Vec<PropKey> = if is_arr {
                let len = self.length_of(val)?;
                (0..len as u32).map(PropKey::Idx).collect()
            } else {
                self.own_keys_p(o, true, false, true)?
            };
            for k in keys {
                let nv = self.internalize(o, k, reviver, depth + 1)?;
                if nv.is_undef() {
                    self.delete_p(o, k)?;
                } else {
                    self.create_data(o, k, nv)?;
                }
            }
            self.temps.truncate(mark);
        }
        let kv = self.key_to_val(key);
        self.call_v(reviver, Val::Obj(holder), &[kv, val])
    }

    // ---- stringify ----

    pub(crate) fn json_stringify_val(
        &mut self,
        v: Val,
        replacer: Val,
        space: Val,
    ) -> Res<Option<StrId>> {
        let mut st = Stringify {
            replacer_fn: Val::Undef,
            list: None,
            gap: Vec::new(),
            indent: Vec::new(),
            stack: Vec::new(),
        };
        if self.callable(replacer) {
            st.replacer_fn = replacer;
        } else if let Val::Obj(ro) = replacer
            && matches!(self.o(ro).kind, ObjKind::Array(_))
        {
            let len = self.length_of(replacer)?;
            let mut list: Vec<PropKey> = Vec::new();
            let mut i = 0.0;
            while i < len {
                let item = self.get_index(replacer, i)?;
                let k = match item {
                    Val::Str(_) | Val::Int(_) | Val::Num(_) => Some(self.to_key(item)?),
                    Val::Obj(io)
                        if matches!(self.o(io).kind, ObjKind::StrBox(_) | ObjKind::NumBox(_)) =>
                    {
                        let s = self.to_str(item)?;
                        Some(self.key_from_str(s))
                    }
                    _ => None,
                };
                if let Some(k) = k
                    && !list.contains(&k)
                {
                    list.push(k);
                }
                i += 1.0;
            }
            st.list = Some(list);
        }
        let space = match space {
            Val::Obj(so) => match self.o(so).kind {
                ObjKind::NumBox(_) => Val::num(self.to_num(space)?),
                ObjKind::StrBox(_) => Val::Str(self.to_str(space)?),
                _ => space,
            },
            _ => space,
        };
        match space {
            Val::Int(_) | Val::Num(_) => {
                let n = self.to_integer(space)?.clamp(0.0, 10.0) as usize;
                st.gap = alloc::vec![b' ' as u16; n];
            }
            Val::Str(s) => {
                let u = self.heap.units(s);
                st.gap = u[..u.len().min(10)].to_vec();
            }
            _ => {}
        }
        let wrapper = self.new_plain();
        let ek = self.key(N::empty);
        self.create_data(wrapper, ek, v)?;
        let mark = self.temps.len();
        self.temps.push(Val::Obj(wrapper));
        self.temps.push(replacer);
        let mut out = Vec::new();
        let r = self.ser_property(&mut st, wrapper, ek, &mut out);
        self.temps.truncate(mark);
        if r? {
            Ok(Some(self.heap.new_str_vec(out)))
        } else {
            Ok(None)
        }
    }

    /// SerializeJSONProperty; false when the value serializes to nothing (undefined).
    fn ser_property(
        &mut self,
        st: &mut Stringify,
        holder: ObjId,
        key: PropKey,
        out: &mut Vec<u16>,
    ) -> Res<bool> {
        self.check_stack()?;
        let mut v = self.get_p(holder, key, Val::Obj(holder))?;
        if matches!(v, Val::Obj(_)) {
            let tk = self.key(N::toJSON);
            let f = self.get_v(v, tk)?;
            if self.callable(f) {
                let kv = self.key_to_val(key);
                v = self.call_v(f, v, &[kv])?;
            }
        }
        if !st.replacer_fn.is_undef() {
            let kv = self.key_to_val(key);
            v = self.call_v(st.replacer_fn, Val::Obj(holder), &[kv, v])?;
        }
        if let Val::Obj(o) = v {
            match self.o(o).kind {
                ObjKind::NumBox(_) => v = Val::num(self.to_num(v)?),
                ObjKind::StrBox(_) => v = Val::Str(self.to_str(v)?),
                ObjKind::BoolBox(b) => v = Val::Bool(b),
                _ => {}
            }
        }
        match v {
            Val::Null => out.extend("null".encode_utf16()),
            Val::Bool(true) => out.extend("true".encode_utf16()),
            Val::Bool(false) => out.extend("false".encode_utf16()),
            Val::Str(s) => {
                let u = self.heap.units(s).to_vec();
                quote(&u, out);
            }
            Val::Int(i) => out.extend(crate::numconv::i32_to_string(i).encode_utf16()),
            Val::Num(n) => {
                if n.is_finite() {
                    out.extend(crate::numconv::number_to_string(n).encode_utf16());
                } else {
                    out.extend("null".encode_utf16());
                }
            }
            Val::Obj(o) if !self.callable(v) => {
                if st.stack.contains(&o) {
                    return Err(self.type_error("Converting circular structure to JSON"));
                }
                if st.stack.len() as u32 > MAX_DEPTH {
                    return Err(self.range_error("Maximum call stack size exceeded"));
                }
                st.stack.push(o);
                self.temps.push(v);
                let r = if matches!(self.o(o).kind, ObjKind::Array(_)) {
                    self.ser_array(st, o, out)
                } else {
                    self.ser_object(st, o, out)
                };
                self.temps.pop();
                st.stack.pop();
                r?;
                self.check_str_len(out.len())?;
                if self.heap.over_hard_limit() {
                    return Err(self.oom());
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn ser_object(&mut self, st: &mut Stringify, o: ObjId, out: &mut Vec<u16>) -> Res<()> {
        let stepback = st.indent.clone();
        st.indent.extend_from_slice(&st.gap.clone());
        let keys = match &st.list {
            Some(l) => l.clone(),
            None => {
                let ks = self.own_keys_p(o, true, false, false)?;
                let mut en = Vec::with_capacity(ks.len());
                for k in ks {
                    if let Some(own) = self.get_own(o, k)?
                        && own.flags() & E != 0
                    {
                        en.push(k);
                    }
                }
                en
            }
        };
        out.push(b'{' as u16);
        let mut any = false;
        for k in keys {
            let mark = out.len();
            if any {
                out.push(b',' as u16);
            }
            if !st.gap.is_empty() {
                out.push(b'\n' as u16);
                out.extend_from_slice(&st.indent);
            }
            let kv = self.key_to_val(k);
            let ks = self.to_str(kv)?;
            let ku = self.heap.units(ks).to_vec();
            quote(&ku, out);
            out.push(b':' as u16);
            if !st.gap.is_empty() {
                out.push(b' ' as u16);
            }
            if self.ser_property(st, o, k, out)? {
                any = true;
            } else {
                out.truncate(mark);
            }
        }
        if any && !st.gap.is_empty() {
            out.push(b'\n' as u16);
            out.extend_from_slice(&stepback);
        }
        out.push(b'}' as u16);
        st.indent = stepback;
        Ok(())
    }

    fn ser_array(&mut self, st: &mut Stringify, o: ObjId, out: &mut Vec<u16>) -> Res<()> {
        let stepback = st.indent.clone();
        st.indent.extend_from_slice(&st.gap.clone());
        let len = self.length_of(Val::Obj(o))?;
        out.push(b'[' as u16);
        let mut i = 0.0;
        while i < len {
            if i > 0.0 {
                out.push(b',' as u16);
            }
            if !st.gap.is_empty() {
                out.push(b'\n' as u16);
                out.extend_from_slice(&st.indent);
            }
            let k = self.idx_key(i)?;
            if !self.ser_property(st, o, k, out)? {
                out.extend("null".encode_utf16());
            }
            i += 1.0;
        }
        if len > 0.0 && !st.gap.is_empty() {
            out.push(b'\n' as u16);
            out.extend_from_slice(&stepback);
        }
        out.push(b']' as u16);
        st.indent = stepback;
        Ok(())
    }
}

struct Stringify {
    replacer_fn: Val,
    list: Option<Vec<PropKey>>,
    gap: Vec<u16>,
    indent: Vec<u16>,
    stack: Vec<ObjId>,
}

fn quote(u: &[u16], out: &mut Vec<u16>) {
    out.push(b'"' as u16);
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        match c {
            0x22 => out.extend("\\\"".encode_utf16()),
            0x5c => out.extend("\\\\".encode_utf16()),
            8 => out.extend("\\b".encode_utf16()),
            12 => out.extend("\\f".encode_utf16()),
            10 => out.extend("\\n".encode_utf16()),
            13 => out.extend("\\r".encode_utf16()),
            9 => out.extend("\\t".encode_utf16()),
            0..=0x1f => out.extend(alloc::format!("\\u{:04x}", c).encode_utf16()),
            0xD800..=0xDBFF if i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) => {
                out.push(c);
                out.push(u[i + 1]);
                i += 1;
            }
            0xD800..=0xDFFF => out.extend(alloc::format!("\\u{:04x}", c).encode_utf16()),
            _ => out.push(c),
        }
        i += 1;
    }
    out.push(b'"' as u16);
}

fn parse(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let text = r.to_str(arg(args, 0))?;
    let u = r.heap.units(text).to_vec();
    let v = r.json_parse_units(&u)?;
    let reviver = arg(args, 1);
    if r.callable(reviver) {
        let root = r.new_plain();
        let ek = r.key(N::empty);
        r.create_data(root, ek, v)?;
        let mark = r.temps.len();
        r.temps.push(Val::Obj(root));
        let out = r.internalize(root, ek, reviver, 0);
        r.temps.truncate(mark);
        return out;
    }
    Ok(v)
}

fn stringify(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    match r.json_stringify_val(arg(args, 0), arg(args, 1), arg(args, 2))? {
        Some(s) => Ok(Val::Str(s)),
        None => Ok(Val::Undef),
    }
}

pub(crate) fn init(r: &mut Realm) {
    let op = r.intr.object_proto;
    let j = r.alloc(Some(op), ObjKind::Ordinary);
    let g = r.global;
    r.value(g, "JSON", Val::Obj(j), crate::object::WC);
    r.method(j, "parse", 2, parse);
    r.method(j, "stringify", 3, stringify);
    r.to_string_tag(j, "JSON");
}
