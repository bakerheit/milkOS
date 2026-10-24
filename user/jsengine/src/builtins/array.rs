//! Array, Array.prototype (the ES2020 set plus at/findLast/findLastIndex), array iterators.
//! The methods are generic (they work on any array-like `this`) with fast paths for dense
//! arrays. Callbacks may mutate the array; the algorithms re-read as the spec says.

use alloc::vec::Vec;

use crate::names::N;
use crate::object::{ArrayData, C, ObjKind, PropKey, SPARSE, WC, WEC};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

use super::arg;

const MAX_LEN: f64 = 4294967295.0;

impl Realm {
    /// The element at i if present (HasProperty + Get), fast for dense arrays.
    pub(crate) fn elem(&mut self, o: ObjId, i: f64) -> Res<Option<Val>> {
        self.heap.work += 1;
        if let ObjKind::Array(a) = &self.o(o).kind
            && self.o(o).flags & SPARSE == 0
            && i < a.dense.len() as f64
        {
            let v = a.dense[i as usize];
            if !v.is_empty() {
                return Ok(Some(v));
            }
        }
        let k = self.idx_key(i)?;
        if !self.has_property(o, k)? {
            return Ok(None);
        }
        Ok(Some(self.get_p(o, k, Val::Obj(o))?))
    }

    pub(crate) fn put_index(&mut self, o: ObjId, i: f64, v: Val) -> Res<()> {
        let k = self.idx_key(i)?;
        self.set_v(Val::Obj(o), k, v, true)
    }

    pub(crate) fn create_index(&mut self, o: ObjId, i: f64, v: Val) -> Res<()> {
        let k = self.idx_key(i)?;
        if !self.create_data(o, k, v)? {
            return Err(self.type_error("Cannot add property, object is not extensible"));
        }
        Ok(())
    }

    fn delete_index(&mut self, o: ObjId, i: f64) -> Res<()> {
        let k = self.idx_key(i)?;
        if !self.delete_p(o, k)? {
            let ks = self.key_display(k);
            return Err(self.type_error(&alloc::format!(
                "Cannot delete property '{ks}' of [object Array]"
            )));
        }
        Ok(())
    }

    fn set_length(&mut self, o: ObjId, len: f64) -> Res<()> {
        let lk = self.key(N::length);
        self.set_v(Val::Obj(o), lk, Val::num(len), true)
    }

    /// ArraySpeciesCreate, simplified: a plain array (subclass species are honoured when
    /// `this` is an instance of a class extending Array).
    fn species_create(&mut self, orig: ObjId, len: f64) -> Res<ObjId> {
        if let ObjKind::Array(_) = self.o(orig).kind {
            let ck = self.key(N::constructor);
            let c = self.get_p(orig, ck, Val::Obj(orig))?;
            if let Val::Obj(co) = c
                && co != self.intr.array_ctor
                && self.constructible(c)
            {
                let sk = PropKey::Sym(self.sym.species);
                let s = self.get_p(co, sk, c)?;
                if !s.is_nullish() {
                    if let Val::Obj(so) = s
                        && so == self.intr.array_ctor
                    {
                        return Ok(self.new_array_from(Vec::new()));
                    }
                    let r = self.construct_v(s, &[Val::num(len)], s)?;
                    if let Val::Obj(ro) = r {
                        return Ok(ro);
                    }
                }
            }
        }
        if len > MAX_LEN {
            return Err(self.range_error("Invalid array length"));
        }
        Ok(self.new_array_from(Vec::new()))
    }

    /// Joins with `sep` (cycles join as "").
    pub(crate) fn array_join(&mut self, o: ObjId, sep: &[u16]) -> Res<Val> {
        if self.join_stack.contains(&o) {
            return Ok(Val::Str(self.nstr(N::empty)));
        }
        let len = self.length_of(Val::Obj(o))?;
        if len > 1.0 {
            self.check_str_len(((len - 1.0) * sep.len() as f64).min(1e18) as usize)?;
        }
        self.join_stack.push(o);
        let r: Res<Vec<u16>> = (|| {
            let mut out: Vec<u16> = Vec::new();
            let mut i = 0.0;
            while i < len {
                if i > 0.0 {
                    out.extend_from_slice(sep);
                }
                let v = self.get_index(Val::Obj(o), i)?;
                if !v.is_nullish() {
                    let s = self.to_str(v)?;
                    out.extend_from_slice(self.heap.units(s));
                }
                if (i as u64) & 1023 == 0 {
                    self.check_str_len(out.len())?;
                }
                i += 1.0;
                if (i as u64) & 4095 == 0 {
                    self.check_mem()?;
                }
            }
            Ok(out)
        })();
        self.join_stack.pop();
        let out = r?;
        Ok(Val::Str(self.heap.new_str_vec(out)))
    }
}

fn array_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    let proto = if nt.is_undef() {
        r.intr.array_proto
    } else {
        r.proto_from_ctor(nt, |r| r.intr.array_proto)?
    };
    let a = if args.len() == 1 && args[0].is_number() {
        let n = args[0].as_f64().unwrap_or(0.0);
        let len = n as u32;
        if len as f64 != n {
            return Err(r.range_error("Invalid array length"));
        }
        ArrayData {
            dense: Vec::new(),
            len,
        }
    } else {
        ArrayData {
            dense: args.to_vec(),
            len: args.len() as u32,
        }
    };
    r.heap.charge(a.dense.len() * 16);
    Ok(Val::Obj(r.alloc(Some(proto), ObjKind::Array(a))))
}

fn is_array(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(r.is_array_val(arg(args, 0))?))
}

fn from(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let items = arg(args, 0);
    let mapfn = arg(args, 1);
    let this_arg = arg(args, 2);
    if !mapfn.is_undef() && !r.callable(mapfn) {
        return Err(
            r.type_error("Array.from: when provided, the second argument must be a function")
        );
    }
    let use_ctor = matches!(this, Val::Obj(c) if c != r.intr.array_ctor && r.constructible(this));
    let ik = PropKey::Sym(r.sym.iterator);
    let iter_m = if items.is_nullish() {
        return Err(r.type_error("Array.from requires an array-like object"));
    } else {
        r.get_v(items, ik)?
    };
    let vals: Vec<Val> = if !iter_m.is_nullish() {
        r.iterate_to_vec(items)?
    } else {
        let o = r.to_object(items)?;
        let len = r.length_of(Val::Obj(o))?;
        if len > 1e8 {
            return Err(r.range_error("Invalid array length"));
        }
        let mut v = Vec::with_capacity(len as usize);
        let mut i = 0.0;
        while i < len {
            v.push(r.get_index(Val::Obj(o), i)?);
            i += 1.0;
        }
        v
    };
    let out = if use_ctor {
        match r.construct_v(this, &[], this)? {
            Val::Obj(o) => o,
            _ => r.new_array_from(Vec::new()),
        }
    } else {
        r.new_array_from(Vec::with_capacity(vals.len()))
    };
    let mark = r.temps.len();
    r.temps.push(Val::Obj(out));
    r.temps.extend_from_slice(&vals);
    let res: Res<()> = (|| {
        for (i, v) in vals.iter().enumerate() {
            let v = if mapfn.is_undef() {
                *v
            } else {
                r.call_v(mapfn, this_arg, &[*v, Val::num(i as f64)])?
            };
            r.create_index(out, i as f64, v)?;
        }
        r.set_length(out, vals.len() as f64)
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(out))
}

fn of(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Obj(r.new_array_from(args.to_vec())))
}

// ---- prototype ----

fn at(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.at")?;
    let len = r.length_of(Val::Obj(o))?;
    let rel = r.to_integer(arg(args, 0))?;
    let k = if rel >= 0.0 { rel } else { len + rel };
    if k < 0.0 || k >= len {
        return Ok(Val::Undef);
    }
    r.get_index(Val::Obj(o), k)
}

fn is_concat_spreadable(r: &mut Realm, v: Val) -> Res<bool> {
    let Val::Obj(o) = v else { return Ok(false) };
    let k = PropKey::Sym(r.sym.is_concat_spreadable);
    let s = r.get_p(o, k, v)?;
    if !s.is_undef() {
        return Ok(r.truthy(s));
    }
    r.is_array_val(v)
}

fn concat(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.concat")?;
    let out = r.species_create(o, 0.0)?;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(out));
    let res: Res<()> = (|| {
        let mut n = 0.0;
        let mut items = alloc::vec![Val::Obj(o)];
        items.extend_from_slice(args);
        for e in items {
            if is_concat_spreadable(r, e)? {
                let Val::Obj(eo) = e else { continue };
                let len = r.length_of(e)?;
                if n + len > 9007199254740991.0 {
                    return Err(r.type_error("Invalid array length"));
                }
                // Fast path: dense source.
                if let ObjKind::Array(a) = &r.o(eo).kind
                    && r.o(eo).flags & SPARSE == 0
                    && a.dense.len() as f64 == len
                    && !a.dense.iter().any(|v| v.is_empty())
                {
                    let src = a.dense.clone();
                    for v in src {
                        r.create_index(out, n, v)?;
                        n += 1.0;
                    }
                    continue;
                }
                let mut k = 0.0;
                while k < len {
                    if let Some(v) = r.elem(eo, k)? {
                        r.create_index(out, n, v)?;
                    }
                    n += 1.0;
                    k += 1.0;
                }
            } else {
                r.create_index(out, n, e)?;
                n += 1.0;
            }
            r.check_mem()?;
        }
        r.set_length(out, n)
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(out))
}

fn copy_within(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.copyWithin")?;
    let len = r.length_of(Val::Obj(o))?;
    let to = r.rel_index(arg(args, 0), len, 0.0)?;
    let from = r.rel_index(arg(args, 1), len, 0.0)?;
    let fin = r.rel_index(arg(args, 2), len, len)?;
    let count = (fin - from).min(len - to);
    if count <= 0.0 {
        return Ok(Val::Obj(o));
    }
    let (mut f, mut t, dir) = if from < to && to < from + count {
        (from + count - 1.0, to + count - 1.0, -1.0)
    } else {
        (from, to, 1.0)
    };
    let mut c = count;
    while c > 0.0 {
        match r.elem(o, f)? {
            Some(v) => r.put_index(o, t, v)?,
            None => r.delete_index(o, t)?,
        }
        f += dir;
        t += dir;
        c -= 1.0;
    }
    Ok(Val::Obj(o))
}

fn entries(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.entries")?;
    Ok(Val::Obj(r.make_array_iter(Val::Obj(o), 2)))
}

fn keys(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.keys")?;
    Ok(Val::Obj(r.make_array_iter(Val::Obj(o), 0)))
}

pub(crate) fn values(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.values")?;
    Ok(Val::Obj(r.make_array_iter(Val::Obj(o), 1)))
}

/// The callback-taking methods share one loop. `kind` picks the behaviour.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
enum Each {
    ForEach,
    Map,
    Filter,
    Some,
    Every,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
}

fn each(r: &mut Realm, this: Val, args: &[Val], kind: Each, name: &str) -> Res<Val> {
    let o = r.this_obj(this, name)?;
    let len = r.length_of(Val::Obj(o))?;
    let f = arg(args, 0);
    if !r.callable(f) {
        let d = r.short_display(f);
        return Err(r.type_error(&alloc::format!("{d} is not a function")));
    }
    let t = arg(args, 1);
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    let out = match kind {
        Each::Map => {
            let a = r.species_create(o, len)?;
            if let ObjKind::Array(d) = &mut r.om(a).kind
                && len <= MAX_LEN
            {
                d.len = len as u32;
            }
            Some(a)
        }
        Each::Filter => Some(r.species_create(o, 0.0)?),
        _ => None,
    };
    if let Some(a) = out {
        r.temps.push(Val::Obj(a));
    }
    let res: Res<Val> = (|| {
        let mut n = 0.0;
        let rev = matches!(kind, Each::FindLast | Each::FindLastIndex);
        let mut k = if rev { len - 1.0 } else { 0.0 };
        while if rev { k >= 0.0 } else { k < len } {
            let v = match kind {
                Each::Find | Each::FindIndex | Each::FindLast | Each::FindLastIndex => {
                    Some(r.get_index(Val::Obj(o), k)?)
                }
                _ => r.elem(o, k)?,
            };
            if let Some(v) = v {
                let res = r.call_v(f, t, &[v, Val::num(k), Val::Obj(o)])?;
                match kind {
                    Each::ForEach => {}
                    Each::Map => {
                        if let Some(a) = out {
                            r.create_index(a, k, res)?;
                        }
                    }
                    Each::Filter => {
                        if r.truthy(res)
                            && let Some(a) = out
                        {
                            r.create_index(a, n, v)?;
                            n += 1.0;
                        }
                    }
                    Each::Some => {
                        if r.truthy(res) {
                            return Ok(Val::Bool(true));
                        }
                    }
                    Each::Every => {
                        if !r.truthy(res) {
                            return Ok(Val::Bool(false));
                        }
                    }
                    Each::Find | Each::FindLast => {
                        if r.truthy(res) {
                            return Ok(v);
                        }
                    }
                    Each::FindIndex | Each::FindLastIndex => {
                        if r.truthy(res) {
                            return Ok(Val::num(k));
                        }
                    }
                }
            }
            if rev {
                k -= 1.0;
            } else {
                k += 1.0;
            }
        }
        Ok(match kind {
            Each::ForEach | Each::Find | Each::FindLast => Val::Undef,
            Each::Map | Each::Filter => out.map_or(Val::Undef, Val::Obj),
            Each::Some => Val::Bool(false),
            Each::Every => Val::Bool(true),
            Each::FindIndex | Each::FindLastIndex => Val::Int(-1),
        })
    })();
    r.temps.truncate(mark);
    res
}

fn for_each(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::ForEach, "Array.prototype.forEach")
}
fn map(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::Map, "Array.prototype.map")
}
fn filter(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::Filter, "Array.prototype.filter")
}
fn some(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::Some, "Array.prototype.some")
}
fn every(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::Every, "Array.prototype.every")
}
fn find(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::Find, "Array.prototype.find")
}
fn find_index(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::FindIndex, "Array.prototype.findIndex")
}
fn find_last(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(r, this, args, Each::FindLast, "Array.prototype.findLast")
}
fn find_last_index(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    each(
        r,
        this,
        args,
        Each::FindLastIndex,
        "Array.prototype.findLastIndex",
    )
}

fn fill(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.fill")?;
    let len = r.length_of(Val::Obj(o))?;
    let v = arg(args, 0);
    let mut k = r.rel_index(arg(args, 1), len, 0.0)?;
    let fin = r.rel_index(arg(args, 2), len, len)?;
    if fin - k > 1e8 && !r.heap.could_fit(((fin - k) as usize).saturating_mul(16)) {
        return Err(r.oom());
    }
    while k < fin {
        r.put_index(o, k, v)?;
        k += 1.0;
        if (k as u64) & 4095 == 0 {
            r.check_mem()?;
        }
    }
    Ok(Val::Obj(o))
}

fn flatten_into(
    r: &mut Realm,
    target: ObjId,
    src: ObjId,
    len: f64,
    start: f64,
    depth: f64,
    mapper: Option<(Val, Val)>,
) -> Res<f64> {
    r.check_stack()?;
    let mut t = start;
    let mut k = 0.0;
    while k < len {
        if let Some(mut v) = r.elem(src, k)? {
            if let Some((f, this_arg)) = mapper {
                v = r.call_v(f, this_arg, &[v, Val::num(k), Val::Obj(src)])?;
            }
            let flatten =
                depth > 0.0 && matches!(v, Val::Obj(o) if matches!(r.o(o).kind, ObjKind::Array(_)));
            if flatten {
                let Val::Obj(vo) = v else { continue };
                let l = r.length_of(v)?;
                if r.temps.len() > 100_000 {
                    return Err(r.range_error("Maximum call stack size exceeded"));
                }
                r.temps.push(v);
                t = flatten_into(r, target, vo, l, t, depth - 1.0, None)?;
            } else {
                r.create_index(target, t, v)?;
                t += 1.0;
            }
        }
        k += 1.0;
    }
    Ok(t)
}

fn flat(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.flat")?;
    let len = r.length_of(Val::Obj(o))?;
    let d = arg(args, 0);
    let depth = if d.is_undef() {
        1.0
    } else {
        r.to_integer(d)?.max(0.0)
    };
    let out = r.species_create(o, 0.0)?;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(out));
    r.temps.push(Val::Obj(o));
    let res = flatten_into(r, out, o, len, 0.0, depth.min(10_000.0), None);
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(out))
}

fn flat_map(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.flatMap")?;
    let len = r.length_of(Val::Obj(o))?;
    let f = arg(args, 0);
    if !r.callable(f) {
        return Err(r.type_error("flatMap mapper function is not callable"));
    }
    let out = r.species_create(o, 0.0)?;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(out));
    r.temps.push(Val::Obj(o));
    let res = flatten_into(r, out, o, len, 0.0, 1.0, Some((f, arg(args, 1))));
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(out))
}

fn includes(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.includes")?;
    let len = r.length_of(Val::Obj(o))?;
    if len == 0.0 {
        return Ok(Val::Bool(false));
    }
    let mut k = r.rel_index(arg(args, 1), len, 0.0)?;
    let target = arg(args, 0);
    while k < len {
        let v = r.get_index(Val::Obj(o), k)?;
        if r.same_value_zero(v, target) {
            return Ok(Val::Bool(true));
        }
        k += 1.0;
    }
    Ok(Val::Bool(false))
}

fn index_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.indexOf")?;
    let len = r.length_of(Val::Obj(o))?;
    if len == 0.0 {
        return Ok(Val::Int(-1));
    }
    let mut k = r.rel_index(arg(args, 1), len, 0.0)?;
    let target = arg(args, 0);
    while k < len {
        if let Some(v) = r.elem(o, k)?
            && r.strict_eq(v, target)
        {
            return Ok(Val::num(k));
        }
        k += 1.0;
    }
    Ok(Val::Int(-1))
}

fn last_index_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.lastIndexOf")?;
    let len = r.length_of(Val::Obj(o))?;
    if len == 0.0 {
        return Ok(Val::Int(-1));
    }
    let mut k = if args.len() > 1 {
        let n = r.to_integer(args[1])?;
        if n >= 0.0 { n.min(len - 1.0) } else { len + n }
    } else {
        len - 1.0
    };
    let target = arg(args, 0);
    while k >= 0.0 {
        if let Some(v) = r.elem(o, k)?
            && r.strict_eq(v, target)
        {
            return Ok(Val::num(k));
        }
        k -= 1.0;
    }
    Ok(Val::Int(-1))
}

fn join(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.join")?;
    let sep = match arg(args, 0) {
        Val::Undef => alloc::vec![b',' as u16],
        s => r.to_units(s)?,
    };
    r.array_join(o, &sep)
}

fn to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.toString")?;
    let jk = r.key(N::join);
    let j = r.get_p(o, jk, Val::Obj(o))?;
    if r.callable(j) {
        return r.call_v(j, Val::Obj(o), &[]);
    }
    let s = r.builtin_tag(Val::Obj(o))?;
    Ok(r.str_val(&s))
}

fn to_locale_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.toLocaleString")?;
    if r.join_stack.contains(&o) {
        return Ok(Val::Str(r.nstr(N::empty)));
    }
    let len = r.length_of(Val::Obj(o))?;
    r.join_stack.push(o);
    let res: Res<Vec<u16>> = (|| {
        let mut out = Vec::new();
        let mut i = 0.0;
        while i < len {
            if i > 0.0 {
                out.push(b',' as u16);
            }
            let v = r.get_index(Val::Obj(o), i)?;
            if !v.is_nullish() {
                let k = r.key_str("toLocaleString");
                let f = r.get_v(v, k)?;
                let s = r.call_v(f, v, &[])?;
                let s = r.to_str(s)?;
                out.extend_from_slice(r.heap.units(s));
            }
            i += 1.0;
        }
        Ok(out)
    })();
    r.join_stack.pop();
    let out = res?;
    Ok(Val::Str(r.heap.new_str_vec(out)))
}

fn pop(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.pop")?;
    // Fast path.
    if let ObjKind::Array(a) = &mut r.heap.om(o).kind
        && a.dense.len() as u32 == a.len
        && let Some(v) = a.dense.last().copied()
        && !v.is_empty()
    {
        let flags = r.o(o).flags;
        if flags & (SPARSE | crate::object::LEN_RO) == 0 {
            if let ObjKind::Array(a) = &mut r.om(o).kind {
                a.dense.pop();
                a.len -= 1;
            }
            return Ok(v);
        }
    }
    let len = r.length_of(Val::Obj(o))?;
    if len == 0.0 {
        r.set_length(o, 0.0)?;
        return Ok(Val::Undef);
    }
    let v = r.get_index(Val::Obj(o), len - 1.0)?;
    r.delete_index(o, len - 1.0)?;
    r.set_length(o, len - 1.0)?;
    Ok(v)
}

fn push(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.push")?;
    // Fast path: dense array.
    let flags = r.o(o).flags;
    if let ObjKind::Array(a) = &mut r.heap.om(o).kind
        && flags & (SPARSE | crate::object::LEN_RO) == 0
        && flags & crate::object::EXTENSIBLE != 0
        && a.dense.len() as u32 == a.len
        && (a.len as usize) + args.len() < u32::MAX as usize
    {
        a.dense.extend_from_slice(args);
        a.len = a.dense.len() as u32;
        let n = a.len;
        r.heap.charge(args.len() * 16);
        return Ok(Val::num(n as f64));
    }
    let len = r.length_of(Val::Obj(o))?;
    if len + args.len() as f64 > 9007199254740991.0 {
        return Err(r.type_error("Pushing too many elements"));
    }
    let mut n = len;
    for v in args {
        r.put_index(o, n, *v)?;
        n += 1.0;
    }
    r.set_length(o, n)?;
    Ok(Val::num(n))
}

fn reduce_impl(r: &mut Realm, this: Val, args: &[Val], right: bool) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.reduce")?;
    let len = r.length_of(Val::Obj(o))?;
    let f = arg(args, 0);
    if !r.callable(f) {
        let d = r.short_display(f);
        return Err(r.type_error(&alloc::format!("{d} is not a function")));
    }
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    let res: Res<Val> = (|| {
        let mut k = if right { len - 1.0 } else { 0.0 };
        let step = if right { -1.0 } else { 1.0 };
        let in_range = |k: f64| if right { k >= 0.0 } else { k < len };
        let mut acc = if args.len() >= 2 {
            args[1]
        } else {
            let mut found = None;
            while in_range(k) {
                if let Some(v) = r.elem(o, k)? {
                    found = Some(v);
                    k += step;
                    break;
                }
                k += step;
            }
            match found {
                Some(v) => v,
                None => return Err(r.type_error("Reduce of empty array with no initial value")),
            }
        };
        let acc_slot = r.temps.len();
        r.temps.push(acc);
        while in_range(k) {
            if let Some(v) = r.elem(o, k)? {
                acc = r.call_v(f, Val::Undef, &[acc, v, Val::num(k), Val::Obj(o)])?;
                r.temps[acc_slot] = acc;
            }
            k += step;
        }
        Ok(acc)
    })();
    r.temps.truncate(mark);
    res
}

fn reduce(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    reduce_impl(r, this, args, false)
}
fn reduce_right(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    reduce_impl(r, this, args, true)
}

fn reverse(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.reverse")?;
    if let ObjKind::Array(a) = &mut r.heap.om(o).kind
        && a.dense.len() as u32 == a.len
    {
        let flags = r.o(o).flags;
        if flags & SPARSE == 0
            && let ObjKind::Array(a) = &mut r.om(o).kind
            && !a.dense.iter().any(|v| v.is_empty())
        {
            a.dense.reverse();
            return Ok(Val::Obj(o));
        }
    }
    let len = r.length_of(Val::Obj(o))?;
    let mut lo = 0.0;
    let mut hi = len - 1.0;
    while lo < hi {
        let a = r.elem(o, lo)?;
        let b = r.elem(o, hi)?;
        match b {
            Some(v) => r.put_index(o, lo, v)?,
            None => r.delete_index(o, lo)?,
        }
        match a {
            Some(v) => r.put_index(o, hi, v)?,
            None => r.delete_index(o, hi)?,
        }
        lo += 1.0;
        hi -= 1.0;
    }
    Ok(Val::Obj(o))
}

fn shift(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.shift")?;
    let flags = r.o(o).flags;
    if let ObjKind::Array(a) = &mut r.heap.om(o).kind
        && flags & (SPARSE | crate::object::LEN_RO) == 0
        && a.dense.len() as u32 == a.len
        && !a.dense.is_empty()
        && !a.dense.iter().any(|v| v.is_empty())
    {
        let v = a.dense.remove(0);
        a.len -= 1;
        return Ok(v);
    }
    let len = r.length_of(Val::Obj(o))?;
    if len == 0.0 {
        r.set_length(o, 0.0)?;
        return Ok(Val::Undef);
    }
    let first = r.get_index(Val::Obj(o), 0.0)?;
    let mut k = 1.0;
    while k < len {
        match r.elem(o, k)? {
            Some(v) => r.put_index(o, k - 1.0, v)?,
            None => r.delete_index(o, k - 1.0)?,
        }
        k += 1.0;
    }
    r.delete_index(o, len - 1.0)?;
    r.set_length(o, len - 1.0)?;
    Ok(first)
}

fn unshift(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.unshift")?;
    let flags = r.o(o).flags;
    if let ObjKind::Array(a) = &mut r.heap.om(o).kind
        && flags & (SPARSE | crate::object::LEN_RO) == 0
        && flags & crate::object::EXTENSIBLE != 0
        && a.dense.len() as u32 == a.len
        && !a.dense.iter().any(|v| v.is_empty())
        && (a.len as usize) + args.len() < u32::MAX as usize
    {
        let mut nd = args.to_vec();
        nd.extend_from_slice(&a.dense);
        a.dense = nd;
        a.len = a.dense.len() as u32;
        let n = a.len;
        r.heap.charge(args.len() * 16);
        return Ok(Val::num(n as f64));
    }
    let len = r.length_of(Val::Obj(o))?;
    let n = args.len() as f64;
    if n > 0.0 {
        let mut k = len;
        while k > 0.0 {
            match r.elem(o, k - 1.0)? {
                Some(v) => r.put_index(o, k + n - 1.0, v)?,
                None => r.delete_index(o, k + n - 1.0)?,
            }
            k -= 1.0;
        }
        for (j, v) in args.iter().enumerate() {
            r.put_index(o, j as f64, *v)?;
        }
    }
    r.set_length(o, len + n)?;
    Ok(Val::num(len + n))
}

fn slice(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.slice")?;
    let len = r.length_of(Val::Obj(o))?;
    let k = r.rel_index(arg(args, 0), len, 0.0)?;
    let fin = r.rel_index(arg(args, 1), len, len)?;
    // Fast path: dense array, plain result.
    if let ObjKind::Array(a) = &r.o(o).kind
        && r.o(o).flags & SPARSE == 0
        && a.dense.len() as f64 == len
        && r.o(o).proto == Some(r.intr.array_proto)
    {
        let ck = r.key(N::constructor);
        let plain = r.o(o).props.get(ck).is_none();
        if plain && let ObjKind::Array(a) = &r.o(o).kind {
            let part: Vec<Val> = if fin > k {
                a.dense[k as usize..fin as usize].to_vec()
            } else {
                Vec::new()
            };
            if !part.iter().any(|v| v.is_empty()) {
                return Ok(Val::Obj(r.new_array_from(part)));
            }
        }
    }
    let count = (fin - k).max(0.0);
    let out = r.species_create(o, count)?;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(out));
    let res: Res<()> = (|| {
        let mut n = 0.0;
        let mut i = k;
        while i < fin {
            if let Some(v) = r.elem(o, i)? {
                r.create_index(out, n, v)?;
            }
            n += 1.0;
            i += 1.0;
        }
        r.set_length(out, n)
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(out))
}

/// Sorts `v` stably with `cmp` (merge sort; the comparator may throw).
fn merge_sort(
    r: &mut Realm,
    v: &mut Vec<Val>,
    cmp: &mut dyn FnMut(&mut Realm, Val, Val) -> Res<bool>,
) -> Res<()> {
    let n = v.len();
    if n < 2 {
        return Ok(());
    }
    // Insertion sort for small runs, then bottom-up merges.
    const RUN: usize = 16;
    let mut start = 0;
    while start < n {
        let end = (start + RUN).min(n);
        for i in start + 1..end {
            let mut j = i;
            while j > start {
                // v[j] < v[j-1] ?
                if cmp(r, v[j], v[j - 1])? {
                    v.swap(j, j - 1);
                    j -= 1;
                } else {
                    break;
                }
            }
        }
        start = end;
    }
    let mut width = RUN;
    let mut buf: Vec<Val> = alloc::vec![Val::Undef; n];
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let hi = (i + 2 * width).min(n);
            let (mut a, mut b, mut k) = (i, mid, i);
            while a < mid && b < hi {
                // take from b only if b < a (stability)
                if cmp(r, v[b], v[a])? {
                    buf[k] = v[b];
                    b += 1;
                } else {
                    buf[k] = v[a];
                    a += 1;
                }
                k += 1;
            }
            while a < mid {
                buf[k] = v[a];
                a += 1;
                k += 1;
            }
            while b < hi {
                buf[k] = v[b];
                b += 1;
                k += 1;
            }
            i += 2 * width;
        }
        core::mem::swap(v, &mut buf);
        width *= 2;
    }
    Ok(())
}

pub(crate) fn sort_values(r: &mut Realm, vals: &mut Vec<Val>, cmpfn: Val) -> Res<()> {
    if cmpfn.is_undef() {
        // Default: ToString each once, compare by code units.
        let mut keyed: Vec<(Vec<u16>, Val)> = Vec::with_capacity(vals.len());
        for v in vals.iter() {
            let s = r.to_units(*v)?;
            keyed.push((s, *v));
        }
        keyed.sort_by(|a, b| a.0.cmp(&b.0));
        for (i, (_, v)) in keyed.into_iter().enumerate() {
            vals[i] = v;
        }
        return Ok(());
    }
    let mut cmp = |r: &mut Realm, a: Val, b: Val| -> Res<bool> {
        let x = r.call_v(cmpfn, Val::Undef, &[a, b])?;
        let n = match x {
            Val::Int(i) => i as f64,
            _ => r.to_num(x)?,
        };
        Ok(n < 0.0)
    };
    merge_sort(r, vals, &mut cmp)
}

fn sort(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let cmpfn = arg(args, 0);
    if !cmpfn.is_undef() && !r.callable(cmpfn) {
        return Err(r.type_error("The comparison function must be either a function or undefined"));
    }
    let o = r.this_obj(this, "Array.prototype.sort")?;
    let len = r.length_of(Val::Obj(o))?;
    let mut vals = Vec::new();
    let mut holes = 0.0;
    let mut k = 0.0;
    while k < len {
        match r.elem(o, k)? {
            Some(v) => {
                r.temps.push(v);
                vals.push(v);
            }
            None => holes += 1.0,
        }
        k += 1.0;
        if (k as u64) & 4095 == 0 {
            r.check_mem()?;
        }
    }
    // undefined sorts last, without calling the comparator.
    let mut undefs = 0usize;
    vals.retain(|v| {
        if v.is_undef() {
            undefs += 1;
            false
        } else {
            true
        }
    });
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    r.temps.extend_from_slice(&vals);
    let res = sort_values(r, &mut vals, cmpfn);
    r.temps.truncate(mark);
    res?;
    let mut i = 0.0;
    for v in vals {
        r.put_index(o, i, v)?;
        i += 1.0;
    }
    for _ in 0..undefs {
        r.put_index(o, i, Val::Undef)?;
        i += 1.0;
    }
    let end = i + holes;
    while i < end {
        r.delete_index(o, i)?;
        i += 1.0;
    }
    Ok(Val::Obj(o))
}

fn splice(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.splice")?;
    let len = r.length_of(Val::Obj(o))?;
    let start = r.rel_index(arg(args, 0), len, 0.0)?;
    let del = if args.is_empty() {
        0.0
    } else if args.len() == 1 {
        len - start
    } else {
        r.to_integer(args[1])?.clamp(0.0, len - start)
    };
    let items: &[Val] = if args.len() > 2 { &args[2..] } else { &[] };
    let n_items = items.len() as f64;
    if len + n_items - del > 9007199254740991.0 {
        return Err(r.type_error("Invalid array length"));
    }
    // Fast path: dense plain array.
    let flags = r.o(o).flags;
    if let ObjKind::Array(a) = &mut r.heap.om(o).kind
        && flags & (SPARSE | crate::object::LEN_RO) == 0
        && flags & crate::object::EXTENSIBLE != 0
        && a.dense.len() as f64 == len
        && !a.dense.iter().any(|v| v.is_empty())
        && r.o(o).proto == Some(r.intr.array_proto)
        && let ObjKind::Array(a) = &mut r.om(o).kind
    {
        let s = start as usize;
        let d = del as usize;
        let removed: Vec<Val> = a.dense.splice(s..s + d, items.iter().copied()).collect();
        a.len = a.dense.len() as u32;
        r.heap.charge(items.len() * 16);
        return Ok(Val::Obj(r.new_array_from(removed)));
    }
    let out = r.species_create(o, del)?;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(out));
    let res: Res<()> = (|| {
        let mut k = 0.0;
        while k < del {
            if let Some(v) = r.elem(o, start + k)? {
                r.create_index(out, k, v)?;
            }
            k += 1.0;
        }
        r.set_length(out, del)?;
        if n_items < del {
            let mut k = start;
            while k < len - del {
                match r.elem(o, k + del)? {
                    Some(v) => r.put_index(o, k + n_items, v)?,
                    None => r.delete_index(o, k + n_items)?,
                }
                k += 1.0;
            }
            let mut k = len;
            while k > len - del + n_items {
                r.delete_index(o, k - 1.0)?;
                k -= 1.0;
            }
        } else if n_items > del {
            let mut k = len - del;
            while k > start {
                match r.elem(o, k + del - 1.0)? {
                    Some(v) => r.put_index(o, k + n_items - 1.0, v)?,
                    None => r.delete_index(o, k + n_items - 1.0)?,
                }
                k -= 1.0;
            }
        }
        for (j, v) in items.iter().enumerate() {
            r.put_index(o, start + j as f64, *v)?;
        }
        r.set_length(o, len - del + n_items)
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(out))
}

fn array_iter_next(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(io) = this else {
        return Err(r.type_error("next method called on incompatible receiver"));
    };
    if !matches!(r.o(io).kind, ObjKind::ArrayIter(_)) {
        return Err(r.type_error("next method called on incompatible receiver"));
    }
    match r.array_iter_advance(io)? {
        Some(v) => Ok(r.iter_result(v, false)),
        None => Ok(r.iter_result(Val::Undef, true)),
    }
}

fn species_getter(_r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(this)
}

pub(crate) fn init(r: &mut Realm) {
    let ap = r.intr.array_proto;
    let c = r.ctor("Array", 1, array_ctor, ap);
    r.intr.array_ctor = c;
    r.method(c, "isArray", 1, is_array);
    r.method(c, "from", 1, from);
    r.method(c, "of", 0, of);
    let sp = r.sym.species;
    r.getter_sym(c, sp, "[Symbol.species]", species_getter);
    r.method(ap, "at", 1, at);
    r.method(ap, "concat", 1, concat);
    r.method(ap, "copyWithin", 2, copy_within);
    r.method(ap, "entries", 0, entries);
    r.method(ap, "every", 1, every);
    r.method(ap, "fill", 1, fill);
    r.method(ap, "filter", 1, filter);
    r.method(ap, "find", 1, find);
    r.method(ap, "findIndex", 1, find_index);
    r.method(ap, "findLast", 1, find_last);
    r.method(ap, "findLastIndex", 1, find_last_index);
    r.method(ap, "flat", 0, flat);
    r.method(ap, "flatMap", 1, flat_map);
    let fe = r.method(ap, "forEach", 1, for_each);
    r.intr.array_for_each = fe;
    r.method(ap, "includes", 1, includes);
    r.method(ap, "indexOf", 1, index_of);
    r.method(ap, "join", 1, join);
    r.method(ap, "keys", 0, keys);
    r.method(ap, "lastIndexOf", 1, last_index_of);
    r.method(ap, "map", 1, map);
    r.method(ap, "pop", 0, pop);
    r.method(ap, "push", 1, push);
    r.method(ap, "reduce", 1, reduce);
    r.method(ap, "reduceRight", 1, reduce_right);
    r.method(ap, "reverse", 0, reverse);
    r.method(ap, "shift", 0, shift);
    r.method(ap, "slice", 2, slice);
    r.method(ap, "some", 1, some);
    r.method(ap, "sort", 1, sort);
    r.method(ap, "splice", 2, splice);
    r.method(ap, "toLocaleString", 0, to_locale_string);
    r.method(ap, "toString", 0, to_string);
    r.method(ap, "unshift", 1, unshift);
    let v = r.method(ap, "values", 0, values);
    r.intr.array_values = v;
    let ik = PropKey::Sym(r.sym.iterator);
    r.put_prop(ap, ik, Val::Obj(v), WC);
    // @@unscopables
    let un = r.alloc(None, ObjKind::Ordinary);
    for n in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "values",
    ] {
        r.value(un, n, Val::Bool(true), WEC);
    }
    let uk = PropKey::Sym(r.sym.unscopables);
    r.put_prop(ap, uk, Val::Obj(un), C);
    let aip = r.intr.array_iterator_proto;
    let n = r.method(aip, "next", 0, array_iter_next);
    r.intr.array_iter_next = n;
    r.to_string_tag(aip, "Array Iterator");
}
