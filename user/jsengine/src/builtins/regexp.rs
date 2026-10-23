//! RegExp objects: the constructor, exec/test, the flag getters, and the Symbol methods
//! String.prototype.match/matchAll/replace/search/split call.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use crate::names::N;
use crate::object::{ObjKind, PropKey, W};
use crate::realm::{Realm, Res};
use crate::regexp::{MatchResult, Program, RegExpData, STEP_LIMIT};
use crate::value::{ObjId, StrId, Val};

use super::arg;

pub(crate) struct MatchAllData {
    pub re: Val,
    pub s: StrId,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

impl Realm {
    /// RegExpCreate / the RegExp constructor's core.
    pub(crate) fn regexp_create(
        &mut self,
        pattern: Val,
        flags: Val,
        new_target: Val,
    ) -> Res<ObjId> {
        let (src, fl) = match pattern {
            Val::Obj(o) if matches!(self.o(o).kind, ObjKind::RegExp(_)) => {
                let (s, f) = match &self.o(o).kind {
                    ObjKind::RegExp(d) => (d.source, d.flags),
                    _ => (self.nstr(N::empty), self.nstr(N::empty)),
                };
                let f = if flags.is_undef() {
                    f
                } else {
                    self.to_str(flags)?
                };
                (s, f)
            }
            _ => {
                let s = if pattern.is_undef() {
                    self.nstr(N::empty)
                } else {
                    self.to_str(pattern)?
                };
                let f = if flags.is_undef() {
                    self.nstr(N::empty)
                } else {
                    self.to_str(flags)?
                };
                (s, f)
            }
        };
        let su = self.heap.units(src).to_vec();
        let fu = self.heap.units(fl).to_vec();
        let prog = match crate::regexp::compile(&su, &fu) {
            Ok(p) => p,
            Err(m) => {
                let ps = crate::string::to_rust(&su);
                return Err(
                    self.syntax_error(&alloc::format!("Invalid regular expression: /{ps}/: {m}"))
                );
            }
        };
        let proto = if new_target.is_undef() {
            self.intr.regexp_proto
        } else {
            self.proto_from_ctor(new_target, |r| r.intr.regexp_proto)?
        };
        let o = self.alloc(
            Some(proto),
            ObjKind::RegExp(Box::new(RegExpData {
                source: src,
                flags: fl,
                prog: Rc::new(prog),
            })),
        );
        let lk = self.key(N::lastIndex);
        self.put_prop(o, lk, Val::Int(0), W);
        Ok(o)
    }

    fn re_data(&self, o: ObjId) -> Option<(Rc<Program>, StrId, StrId)> {
        match &self.o(o).kind {
            ObjKind::RegExp(d) => Some((d.prog.clone(), d.source, d.flags)),
            _ => None,
        }
    }

    fn this_re(&mut self, this: Val, what: &str) -> Res<ObjId> {
        if let Val::Obj(o) = this
            && matches!(self.o(o).kind, ObjKind::RegExp(_))
        {
            return Ok(o);
        }
        Err(self.type_error(&alloc::format!(
            "RegExp.prototype.{what} requires that 'this' be a RegExp object"
        )))
    }

    fn get_last_index(&mut self, o: ObjId) -> Res<f64> {
        let lk = self.key(N::lastIndex);
        if let Some(p) = self.o(o).props.get(lk)
            && let Val::Int(i) = p.v
        {
            return Ok(i.max(0) as f64);
        }
        let v = self.get_p(o, lk, Val::Obj(o))?;
        self.to_length(v)
    }

    fn set_last_index(&mut self, o: ObjId, v: f64) -> Res<()> {
        let lk = self.key(N::lastIndex);
        self.set_v(Val::Obj(o), lk, Val::num(v), true)
    }

    /// RegExpBuiltinExec. Returns the match captures (as unit offsets) or None.
    fn re_builtin_exec_raw(&mut self, o: ObjId, s: StrId) -> Res<Option<Vec<usize>>> {
        let Some((prog, _, _)) = self.re_data(o) else {
            return Err(self.type_error("not a RegExp"));
        };
        let g = prog.flags.global;
        let y = prog.flags.sticky;
        let mut last = if g || y { self.get_last_index(o)? } else { 0.0 };
        if !(g || y) {
            // Reading lastIndex is observable (ToLength) only when global or sticky.
            let _ = self.get_last_index(o)?;
            last = 0.0;
        }
        let len = self.heap.str_len(s);
        if last > len as f64 {
            if g || y {
                self.set_last_index(o, 0.0)?;
            }
            return Ok(None);
        }
        self.heap.flatten(s);
        let (res, steps) = prog.exec(self.heap.flat(s), last as usize, y, STEP_LIMIT);
        self.steps += steps / 64;
        self.poll_steps += steps / 64;
        if self.poll_steps >= self.next_poll
            || (self.limits.max_steps != 0 && self.steps > self.limits.max_steps)
        {
            self.poll()?;
        }
        match res {
            MatchResult::Found(caps) => {
                if g || y {
                    self.set_last_index(o, caps[1] as f64)?;
                }
                Ok(Some(caps))
            }
            MatchResult::NotFound => {
                if g || y {
                    self.set_last_index(o, 0.0)?;
                }
                Ok(None)
            }
            MatchResult::TooComplex => {
                Err(self.range_error("Maximum regular expression complexity exceeded"))
            }
        }
    }

    /// Builds the exec result array from captures.
    fn re_result(&mut self, o: ObjId, s: StrId, caps: &[usize]) -> Res<Val> {
        let Some((prog, _, _)) = self.re_data(o) else {
            return Ok(Val::Null);
        };
        self.heap.flatten(s);
        let slen = self.heap.str_len(s);
        let mut items = Vec::with_capacity(prog.ncaps as usize + 1);
        for g in 0..=prog.ncaps as usize {
            let (a, b) = (caps[g * 2], caps[g * 2 + 1]);
            if a == usize::MAX || b == usize::MAX || b < a || b > slen {
                items.push(Val::Undef);
            } else {
                let piece = self.heap.flat(s)[a..b].to_vec();
                items.push(self.units_val(piece));
            }
        }
        let arr = self.new_array_from(items.clone());
        let ik = self.key(N::index);
        self.put_prop(arr, ik, Val::num(caps[0] as f64), crate::object::WEC);
        let ink = self.key(N::input);
        self.put_prop(arr, ink, Val::Str(s), crate::object::WEC);
        let groups = if prog.names.is_empty() {
            Val::Undef
        } else {
            let go = self.alloc(None, ObjKind::Ordinary);
            for (name, idx) in prog.names.iter() {
                let k = self.key_str(name);
                let v = items.get(*idx as usize).copied().unwrap_or(Val::Undef);
                self.put_prop(go, k, v, crate::object::WEC);
            }
            Val::Obj(go)
        };
        let gk = self.key(N::groups);
        self.put_prop(arr, gk, groups, crate::object::WEC);
        if prog.flags.has_indices {
            // The `d` flag: the [start, end] of every group, and of the named ones by name.
            let mut pairs: Vec<Val> = Vec::with_capacity(items.len());
            for g in 0..=prog.ncaps as usize {
                let (a, b) = (caps[g * 2], caps[g * 2 + 1]);
                if a == usize::MAX || b == usize::MAX || b < a || b > slen {
                    pairs.push(Val::Undef);
                } else {
                    let p =
                        self.new_array_from(alloc::vec![Val::num(a as f64), Val::num(b as f64)]);
                    pairs.push(Val::Obj(p));
                }
            }
            let ig = if prog.names.is_empty() {
                Val::Undef
            } else {
                let go = self.alloc(None, ObjKind::Ordinary);
                for (name, idx) in prog.names.iter() {
                    let k = self.key_str(name);
                    let v = pairs.get(*idx as usize).copied().unwrap_or(Val::Undef);
                    self.put_prop(go, k, v, crate::object::WEC);
                }
                Val::Obj(go)
            };
            let ia = self.new_array_from(pairs);
            self.put_prop(ia, gk, ig, crate::object::WEC);
            let ik2 = self.key_str("indices");
            self.put_prop(arr, ik2, Val::Obj(ia), crate::object::WEC);
        }
        Ok(Val::Obj(arr))
    }

    /// RegExpExec: a user `exec` if there is one, else the builtin.
    pub(crate) fn re_exec(&mut self, r: Val, s: StrId) -> Res<Val> {
        let Val::Obj(o) = r else {
            return Err(self.type_error("RegExp exec called on non-object"));
        };
        let ek = self.key_str("exec");
        let exec = self.get_p(o, ek, r)?;
        let builtin = matches!(exec, Val::Obj(f) if f == self.intr.regexp_exec);
        if !builtin && self.callable(exec) {
            let res = self.call_v(exec, r, &[Val::Str(s)])?;
            if !matches!(res, Val::Obj(_) | Val::Null) {
                return Err(self.type_error(
                    "RegExp exec method returned something other than an Object or null",
                ));
            }
            return Ok(res);
        }
        if !matches!(self.o(o).kind, ObjKind::RegExp(_)) {
            return Err(self.type_error("RegExp.prototype.exec called on incompatible receiver"));
        }
        match self.re_builtin_exec_raw(o, s)? {
            Some(caps) => self.re_result(o, s, &caps),
            None => Ok(Val::Null),
        }
    }

    fn re_flag(&mut self, r: Val, n: &str) -> Res<bool> {
        let k = self.key_str(n);
        let v = self.get_v(r, k)?;
        Ok(self.truthy(v))
    }

    fn advance_index(&mut self, s: StrId, i: f64, unicode: bool) -> f64 {
        if !unicode {
            return i + 1.0;
        }
        let u = self.heap.units(s);
        let idx = i as usize;
        if idx + 1 < u.len()
            && (0xD800..0xDC00).contains(&u[idx])
            && (0xDC00..0xE000).contains(&u[idx + 1])
        {
            i + 2.0
        } else {
            i + 1.0
        }
    }
}

fn regexp_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let p = arg(args, 0);
    let f = arg(args, 1);
    let nt = r.cur_new_target();
    if nt.is_undef()
        && let Val::Obj(po) = p
        && matches!(r.o(po).kind, ObjKind::RegExp(_))
        && f.is_undef()
    {
        let ck = r.key(N::constructor);
        let c = r.get_p(po, ck, p)?;
        if matches!(c, Val::Obj(co) if co == r.intr.regexp_ctor) {
            return Ok(p);
        }
    }
    let nt = if nt.is_undef() {
        Val::Obj(r.intr.regexp_ctor)
    } else {
        nt
    };
    Ok(Val::Obj(r.regexp_create(p, f, nt)?))
}

fn exec(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_re(this, "exec")?;
    let s = r.to_str(arg(args, 0))?;
    match r.re_builtin_exec_raw(o, s)? {
        Some(caps) => r.re_result(o, s, &caps),
        None => Ok(Val::Null),
    }
}

fn test(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("RegExp.prototype.test called on non-object"));
    };
    let s = r.to_str(arg(args, 0))?;
    // Fast path: the builtin exec on a real RegExp.
    let ek = r.key_str("exec");
    let exec = r.get_p(o, ek, this)?;
    if matches!(exec, Val::Obj(f) if f == r.intr.regexp_exec)
        && matches!(r.o(o).kind, ObjKind::RegExp(_))
    {
        return Ok(Val::Bool(r.re_builtin_exec_raw(o, s)?.is_some()));
    }
    let m = r.re_exec(this, s)?;
    Ok(Val::Bool(!m.is_null()))
}

fn escape_source(u: &[u16]) -> Vec<u16> {
    if u.is_empty() {
        return "(?:)".encode_utf16().collect();
    }
    let mut out = Vec::with_capacity(u.len());
    let mut in_class = false;
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        match c {
            0x5c => {
                out.push(c);
                if i + 1 < u.len() {
                    out.push(u[i + 1]);
                    i += 1;
                }
            }
            0x5b => {
                in_class = true;
                out.push(c);
            }
            0x5d => {
                in_class = false;
                out.push(c);
            }
            0x2f if !in_class => {
                out.push(0x5c);
                out.push(c);
            }
            10 => out.extend("\\n".encode_utf16()),
            13 => out.extend("\\r".encode_utf16()),
            0x2028 => out.extend("\\u2028".encode_utf16()),
            0x2029 => out.extend("\\u2029".encode_utf16()),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

fn source(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = this {
        if let Some((_, s, _)) = r.re_data(o) {
            let u = escape_source(r.heap.units(s));
            return Ok(r.units_val(u));
        }
        if o == r.intr.regexp_proto {
            return Ok(r.str_val("(?:)"));
        }
    }
    Err(r.type_error("RegExp.prototype.source getter called on non-RegExp"))
}

fn flags(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if !matches!(this, Val::Obj(_)) {
        return Err(r.type_error("RegExp.prototype.flags getter called on non-object"));
    }
    let mut out = alloc::string::String::new();
    for (name, ch) in [
        ("hasIndices", 'd'),
        ("global", 'g'),
        ("ignoreCase", 'i'),
        ("multiline", 'm'),
        ("dotAll", 's'),
        ("unicode", 'u'),
        ("sticky", 'y'),
    ] {
        if r.re_flag(this, name)? {
            out.push(ch);
        }
    }
    Ok(r.str_val(&out))
}

macro_rules! flag_getter {
    ($name:ident, $field:ident) => {
        fn $name(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
            if let Val::Obj(o) = this {
                if let Some((p, _, _)) = r.re_data(o) {
                    return Ok(Val::Bool(p.flags.$field));
                }
                if o == r.intr.regexp_proto {
                    return Ok(Val::Undef);
                }
            }
            Err(r.type_error("RegExp flag getter called on non-RegExp"))
        }
    };
}

flag_getter!(global, global);
flag_getter!(ignore_case, ignore_case);
flag_getter!(multiline, multiline);
flag_getter!(dot_all, dot_all);
flag_getter!(unicode, unicode);
flag_getter!(sticky, sticky);
flag_getter!(has_indices, has_indices);

fn to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if !matches!(this, Val::Obj(_)) {
        return Err(r.type_error("RegExp.prototype.toString called on non-object"));
    }
    let sk = r.key(N::source);
    let s = r.get_v(this, sk)?;
    let s = r.to_units(s)?;
    let fk = r.key(N::flags);
    let f = r.get_v(this, fk)?;
    let f = r.to_units(f)?;
    let mut out = alloc::vec![b'/' as u16];
    out.extend_from_slice(&s);
    out.push(b'/' as u16);
    out.extend_from_slice(&f);
    Ok(r.units_val(out))
}

fn sym_match(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if !matches!(this, Val::Obj(_)) {
        return Err(r.type_error("RegExp.prototype[Symbol.match] called on non-object"));
    }
    let s = r.to_str(arg(args, 0))?;
    if !r.re_flag(this, "global")? {
        return r.re_exec(this, s);
    }
    let unicode = r.re_flag(this, "unicode")?;
    let Val::Obj(o) = this else {
        return Ok(Val::Null);
    };
    r.set_last_index(o, 0.0)?;
    let mut out = Vec::new();
    loop {
        let m = r.re_exec(this, s)?;
        if m.is_null() {
            break;
        }
        let m0 = r.get_elem(m, Val::Int(0))?;
        let ms = r.to_str(m0)?;
        if r.heap.str_len(ms) == 0 {
            let li = r.get_last_index(o)?;
            let ni = r.advance_index(s, li, unicode);
            r.set_last_index(o, ni)?;
        }
        out.push(Val::Str(ms));
        r.temps.push(Val::Str(ms));
        if out.len() & 1023 == 0 {
            r.check_mem()?;
        }
    }
    if out.is_empty() {
        return Ok(Val::Null);
    }
    Ok(Val::Obj(r.new_array_from(out)))
}

fn sym_match_all(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("RegExp.prototype[Symbol.matchAll] called on non-object"));
    };
    let s = r.to_str(arg(args, 0))?;
    let fk = r.key(N::flags);
    let f = r.get_v(this, fk)?;
    let fs = r.to_str(f)?;
    let rc = Val::Obj(r.intr.regexp_ctor);
    let clone = r.construct_v(rc, &[this, Val::Str(fs)], rc)?;
    let li = r.get_last_index(o)?;
    if let Val::Obj(c) = clone {
        r.set_last_index(c, li)?;
    }
    let fu = r.heap.units(fs).to_vec();
    let global = fu.contains(&(b'g' as u16));
    let unicode = fu.contains(&(b'u' as u16));
    let p = r.intr.regexp_string_iterator_proto;
    Ok(Val::Obj(r.alloc(
        Some(p),
        ObjKind::RegExpStrIter(Box::new(MatchAllData {
            re: clone,
            s,
            global,
            unicode,
            done: false,
        })),
    )))
}

fn match_all_next(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(io) = this else {
        return Err(r.type_error("next called on incompatible receiver"));
    };
    let (re, s, global, unicode, done) = match &r.o(io).kind {
        ObjKind::RegExpStrIter(d) => (d.re, d.s, d.global, d.unicode, d.done),
        _ => return Err(r.type_error("next called on incompatible receiver")),
    };
    if done {
        return Ok(r.iter_result(Val::Undef, true));
    }
    let m = r.re_exec(re, s)?;
    if m.is_null() {
        if let ObjKind::RegExpStrIter(d) = &mut r.om(io).kind {
            d.done = true;
        }
        return Ok(r.iter_result(Val::Undef, true));
    }
    if !global {
        if let ObjKind::RegExpStrIter(d) = &mut r.om(io).kind {
            d.done = true;
        }
        return Ok(r.iter_result(m, false));
    }
    let m0 = r.get_elem(m, Val::Int(0))?;
    let ms = r.to_str(m0)?;
    if r.heap.str_len(ms) == 0
        && let Val::Obj(ro) = re
    {
        let li = r.get_last_index(ro)?;
        let ni = r.advance_index(s, li, unicode);
        r.set_last_index(ro, ni)?;
    }
    Ok(r.iter_result(m, false))
}

fn sym_replace(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("RegExp.prototype[Symbol.replace] called on non-object"));
    };
    let s = r.to_str(arg(args, 0))?;
    let repl = arg(args, 1);
    let func = r.callable(repl);
    let repl_u = if func { Vec::new() } else { r.to_units(repl)? };
    let global = r.re_flag(this, "global")?;
    let mut unicode = false;
    if global {
        unicode = r.re_flag(this, "unicode")?;
        r.set_last_index(o, 0.0)?;
    }
    let mark = r.temps.len();
    let mut results = Vec::new();
    loop {
        let m = r.re_exec(this, s)?;
        if m.is_null() {
            break;
        }
        r.temps.push(m);
        results.push(m);
        if !global {
            break;
        }
        let m0 = r.get_elem(m, Val::Int(0))?;
        let ms = r.to_str(m0)?;
        if r.heap.str_len(ms) == 0 {
            let li = r.get_last_index(o)?;
            let ni = r.advance_index(s, li, unicode);
            r.set_last_index(o, ni)?;
        }
        if results.len() & 1023 == 0 {
            r.check_mem()?;
        }
    }
    let su = r.heap.units(s).to_vec();
    let mut out: Vec<u16> = Vec::with_capacity(su.len());
    let mut next_pos = 0usize;
    let res: Res<()> = (|| {
        for m in results {
            let ncap = r.length_of(m)?;
            let ncap = (ncap - 1.0).max(0.0) as usize;
            let m0 = r.get_elem(m, Val::Int(0))?;
            let matched = r.to_units(m0)?;
            let ik = r.key(N::index);
            let posv = r.get_v(m, ik)?;
            let pos = r.to_integer(posv)?.clamp(0.0, su.len() as f64) as usize;
            let mut caps: Vec<Option<Vec<u16>>> = Vec::with_capacity(ncap);
            let mut cap_vals: Vec<Val> = Vec::with_capacity(ncap);
            for n in 1..=ncap {
                let c = r.get_elem(m, Val::num(n as f64))?;
                if c.is_undef() {
                    caps.push(None);
                    cap_vals.push(Val::Undef);
                } else {
                    let cs = r.to_str(c)?;
                    caps.push(Some(r.heap.units(cs).to_vec()));
                    cap_vals.push(Val::Str(cs));
                }
            }
            let gk = r.key(N::groups);
            let named = r.get_v(m, gk)?;
            let replacement = if func {
                let mv = r.units_val(matched.clone());
                let mut fargs = alloc::vec![mv];
                fargs.extend_from_slice(&cap_vals);
                fargs.push(Val::num(pos as f64));
                fargs.push(Val::Str(s));
                if !named.is_undef() {
                    fargs.push(named);
                }
                let v = r.call_v(repl, Val::Undef, &fargs)?;
                r.to_units(v)?
            } else {
                let named = if named.is_undef() {
                    Val::Undef
                } else {
                    Val::Obj(r.to_object(named)?)
                };
                r.get_substitution(&matched, &su, pos, &caps, named, &repl_u)?
            };
            if pos >= next_pos {
                out.extend_from_slice(&su[next_pos..pos]);
                out.extend_from_slice(&replacement);
                next_pos = (pos + matched.len()).min(su.len());
            }
            r.check_str_len(out.len())?;
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res?;
    if next_pos < su.len() {
        out.extend_from_slice(&su[next_pos..]);
    }
    Ok(r.units_val(out))
}

fn sym_search(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("RegExp.prototype[Symbol.search] called on non-object"));
    };
    let s = r.to_str(arg(args, 0))?;
    let lk = r.key(N::lastIndex);
    let prev = r.get_p(o, lk, this)?;
    if !r.same_val(prev, Val::Int(0)) {
        r.set_v(this, lk, Val::Int(0), true)?;
    }
    let m = r.re_exec(this, s)?;
    let cur = r.get_p(o, lk, this)?;
    if !r.same_val(cur, prev) {
        r.set_v(this, lk, prev, true)?;
    }
    if m.is_null() {
        return Ok(Val::Int(-1));
    }
    let ik = r.key(N::index);
    r.get_v(m, ik)
}

fn sym_split(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("RegExp.prototype[Symbol.split] called on non-object"));
    };
    let s = r.to_str(arg(args, 0))?;
    let lim = arg(args, 1);
    let limit = if lim.is_undef() {
        u32::MAX
    } else {
        r.to_uint32(lim)?
    };
    let Some((prog, _, _)) = r.re_data(o) else {
        return Err(r.type_error("not a RegExp"));
    };
    let su = r.heap.units(s).to_vec();
    let mut out: Vec<Val> = Vec::new();
    if limit == 0 {
        return Ok(Val::Obj(r.new_array_from(out)));
    }
    if su.is_empty() {
        let (res, _) = prog.exec(&su, 0, true, STEP_LIMIT);
        if let MatchResult::Found(_) = res {
            return Ok(Val::Obj(r.new_array_from(out)));
        }
        return Ok(Val::Obj(r.new_array_from(alloc::vec![Val::Str(s)])));
    }
    let mut p = 0usize; // start of the current piece
    let mut q = 0usize; // search position
    while q < su.len() {
        let (res, steps) = prog.exec(&su, q, false, STEP_LIMIT);
        r.steps += steps / 64;
        r.poll_steps += steps / 64;
        let caps = match res {
            MatchResult::Found(c) => c,
            MatchResult::NotFound => break,
            MatchResult::TooComplex => {
                return Err(r.range_error("Maximum regular expression complexity exceeded"));
            }
        };
        let (ms, me) = (caps[0], caps[1]);
        if ms >= su.len() {
            break;
        }
        let e = me.min(su.len());
        if e == p {
            q = ms + 1;
            continue;
        }
        let piece = su[p..ms].to_vec();
        out.push(r.units_val(piece));
        if out.len() as u32 == limit {
            return Ok(Val::Obj(r.new_array_from(out)));
        }
        for g in 1..=prog.ncaps as usize {
            let (a, b) = (caps[g * 2], caps[g * 2 + 1]);
            if a == usize::MAX || b == usize::MAX || b < a {
                out.push(Val::Undef);
            } else {
                let c = su[a..b].to_vec();
                out.push(r.units_val(c));
            }
            if out.len() as u32 == limit {
                return Ok(Val::Obj(r.new_array_from(out)));
            }
        }
        p = e;
        q = p;
        if out.len() & 1023 == 0 {
            r.check_mem()?;
        }
    }
    let piece = su[p.min(su.len())..].to_vec();
    out.push(r.units_val(piece));
    Ok(Val::Obj(r.new_array_from(out)))
}

fn species_getter(_r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(this)
}

pub(crate) fn init(r: &mut Realm) {
    let rp = r.intr.regexp_proto;
    let c = r.ctor("RegExp", 2, regexp_ctor, rp);
    r.intr.regexp_ctor = c;
    let ex = r.method(rp, "exec", 1, exec);
    r.intr.regexp_exec = ex;
    r.method(rp, "test", 1, test);
    r.method(rp, "toString", 0, to_string);
    r.getter(rp, "source", source, None);
    r.getter(rp, "flags", flags, None);
    r.getter(rp, "global", global, None);
    r.getter(rp, "ignoreCase", ignore_case, None);
    r.getter(rp, "multiline", multiline, None);
    r.getter(rp, "dotAll", dot_all, None);
    r.getter(rp, "unicode", unicode, None);
    r.getter(rp, "sticky", sticky, None);
    r.getter(rp, "hasIndices", has_indices, None);
    let s = r.sym.match_;
    r.method_sym(rp, s, "[Symbol.match]", 1, sym_match);
    let s = r.sym.match_all;
    r.method_sym(rp, s, "[Symbol.matchAll]", 1, sym_match_all);
    let s = r.sym.replace;
    r.method_sym(rp, s, "[Symbol.replace]", 2, sym_replace);
    let s = r.sym.search;
    r.method_sym(rp, s, "[Symbol.search]", 1, sym_search);
    let s = r.sym.split;
    r.method_sym(rp, s, "[Symbol.split]", 2, sym_split);
    let sp = r.sym.species;
    r.getter_sym(c, sp, "[Symbol.species]", species_getter);
    let ip = r.intr.regexp_string_iterator_proto;
    r.method(ip, "next", 0, match_all_next);
    r.to_string_tag(ip, "RegExp String Iterator");
    let _ = PropKey::Idx(0);
}
