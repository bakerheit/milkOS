//! ArrayBuffer, the typed arrays (%TypedArray% and its nine concrete constructors) and
//! DataView. The storage and element codecs are in `typed.rs`.
//!
//! Left out on purpose: resizable buffers and growable SharedArrayBuffer, BigInt64Array and
//! BigUint64Array (the engine has no BigInt), `Atomics`.

use alloc::vec::Vec;

use crate::names::N;
use crate::object::{CONSTRUCTOR, ObjKind, PropKey, WC};
use crate::realm::{Realm, Res};
use crate::typed::{TA_KINDS, TaKind, ViewData};
use crate::value::{ObjId, Val};

use super::arg;

// ---- helpers ----

/// ToIndex: a non-negative integer up to 2^53 - 1 (undefined is 0), else a RangeError.
fn to_index(r: &mut Realm, v: Val, what: &str) -> Res<usize> {
    if v.is_undef() {
        return Ok(0);
    }
    let n = r.to_integer(v)?;
    if !(0.0..=9007199254740991.0).contains(&n) {
        return Err(r.range_error(what));
    }
    Ok(n as usize)
}

#[inline]
fn num_of(r: &mut Realm, v: Val) -> Res<f64> {
    match v {
        Val::Int(i) => Ok(i as f64),
        Val::Num(n) => Ok(n),
        _ => r.to_num(v),
    }
}

fn kind_of_callee(r: &Realm) -> TaKind {
    match r.native_slot(r.cur_callee(), 0) {
        Val::Int(i) => TaKind::from_index(i),
        _ => TaKind::Uint8,
    }
}

/// A validated typed array `this`: the object, its element type and its length. A detached
/// one is a TypeError.
fn this_typed(r: &mut Realm, this: Val, what: &str) -> Res<(ObjId, TaKind, usize)> {
    if let Val::Obj(o) = this
        && let Some((k, buf, _, len)) = r.typed_info(o)
    {
        if r.buf_detached(buf) {
            return Err(r.type_error(&alloc::format!(
                "Cannot perform {what} on a detached ArrayBuffer"
            )));
        }
        return Ok((o, k, len));
    }
    Err(r.type_error("this is not a typed array."))
}

fn this_buffer(r: &mut Realm, this: Val, what: &str) -> Res<ObjId> {
    if let Val::Obj(o) = this
        && let ObjKind::ArrayBuffer(_) = r.o(o).kind
    {
        return Ok(o);
    }
    Err(r.type_error(&alloc::format!(
        "Method ArrayBuffer.prototype.{what} called on incompatible receiver"
    )))
}

fn callback(r: &mut Realm, v: Val) -> Res<Val> {
    if !r.callable(v) {
        let d = r.short_display(v);
        return Err(r.type_error(&alloc::format!("{d} is not a function")));
    }
    Ok(v)
}

/// A constructor function object with its prototype wired up and (if `global`) bound on the
/// global object. `slots` become the function's internal slots.
fn make_ctor(
    r: &mut Realm,
    name: &str,
    arity: u32,
    f: crate::object::BuiltinFn,
    proto: ObjId,
    slots: Vec<Val>,
    global: bool,
) -> ObjId {
    let nv = r.str_val(name);
    let fo = r.new_builtin_slots(nv, arity, f, slots);
    r.om(fo).flags |= CONSTRUCTOR;
    let pk = r.key(N::prototype);
    r.put_prop(fo, pk, Val::Obj(proto), 0);
    let ck = r.key(N::constructor);
    r.put_prop(proto, ck, Val::Obj(fo), WC);
    if global {
        let g = r.global;
        r.value(g, name, Val::Obj(fo), WC);
    }
    fo
}

fn method_slots(
    r: &mut Realm,
    obj: ObjId,
    name: &str,
    arity: u32,
    f: crate::object::BuiltinFn,
    slots: Vec<Val>,
) {
    let k = r.key_str(name);
    let nv = r.str_val(name);
    let fo = r.new_builtin_slots(nv, arity, f, slots);
    r.put_prop(obj, k, Val::Obj(fo), WC);
}

// ---- ArrayBuffer ----

fn array_buffer_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error("Constructor ArrayBuffer requires 'new'"));
    }
    let len = to_index(r, arg(args, 0), "Invalid array buffer length")?;
    let proto = r.proto_from_ctor(nt, |r| r.intr.array_buffer_proto)?;
    let id = r.alloc_buffer(len, Some(proto))?;
    Ok(Val::Obj(id))
}

fn ab_is_view(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(
        matches!(arg(args, 0), Val::Obj(o) if matches!(r.o(o).kind, ObjKind::Typed(_) | ObjKind::DataView(_))),
    ))
}

fn species_getter(_r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(this)
}

fn ab_byte_length(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let b = this_buffer(r, this, "byteLength")?;
    let n = r.buf_data(b).map_or(0, |d| d.bytes.len());
    Ok(Val::num(n as f64))
}

fn ab_detached(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let b = this_buffer(r, this, "detached")?;
    Ok(Val::Bool(r.buf_detached(b)))
}

fn ab_slice(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let b = this_buffer(r, this, "slice")?;
    if r.buf_detached(b) {
        return Err(
            r.type_error("Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer")
        );
    }
    let len = r.buf_data(b).map_or(0, |d| d.bytes.len()) as f64;
    let first = r.rel_index(arg(args, 0), len, 0.0)?;
    let fin = r.rel_index(arg(args, 1), len, len)?;
    let n = (fin - first).max(0.0) as usize;
    r.check_buffer_size(n)?;
    if r.buf_detached(b) {
        return Err(
            r.type_error("Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer")
        );
    }
    let src = r.buf_data(b).map(|d| {
        let s = first as usize;
        d.bytes.get(s..s + n).map(|x| x.to_vec())
    });
    let bytes = match src {
        Some(Some(v)) => v,
        _ => alloc::vec![0u8; n],
    };
    Ok(Val::Obj(r.buffer_from_vec(bytes)))
}

fn ab_transfer_impl(r: &mut Realm, this: Val, args: &[Val], what: &str) -> Res<Val> {
    let b = this_buffer(r, this, what)?;
    let cur = r.buf_data(b).map_or(0, |d| d.bytes.len());
    let nl = if arg(args, 0).is_undef() {
        cur
    } else {
        to_index(r, arg(args, 0), "Invalid array buffer length")?
    };
    if r.buf_detached(b) {
        return Err(r.type_error(&alloc::format!(
            "Cannot perform ArrayBuffer.prototype.{what} on a detached ArrayBuffer"
        )));
    }
    r.check_buffer_size(nl)?;
    let mut bytes = r.detach_buffer(b);
    bytes.resize(nl, 0);
    Ok(Val::Obj(r.buffer_from_vec(bytes)))
}

fn ab_transfer(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    ab_transfer_impl(r, this, args, "transfer")
}

fn ab_transfer_fixed(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    ab_transfer_impl(r, this, args, "transferToFixedLength")
}

fn ab_resizable(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    this_buffer(r, this, "resizable")?;
    Ok(Val::Bool(false))
}

// ---- the typed array constructors ----

fn abstract_ctor(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    Err(r.type_error("Abstract class TypedArray not directly constructable"))
}

fn typed_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let kind = kind_of_callee(r);
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error(&alloc::format!(
            "Constructor {} requires 'new'",
            kind.name()
        )));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.ta_proto(kind))?;
    let first = arg(args, 0);
    let Val::Obj(src) = first else {
        let len = to_index(r, first, "Invalid typed array length")?;
        let id = r.typed_alloc(kind, len, Some(proto))?;
        return Ok(Val::Obj(id));
    };
    let sz = kind.size();
    match r.o(src).kind {
        ObjKind::Typed(_) => {
            let (sk, sbuf, _, slen) = r.typed_info(src).unwrap_or((kind, src, 0, 0));
            if r.buf_detached(sbuf) {
                return Err(r.type_error("Cannot perform Construct on a detached ArrayBuffer"));
            }
            let id = r.typed_alloc(kind, slen, Some(proto))?;
            if sk == kind {
                let bytes = r.typed_bytes(src).to_vec();
                r.typed_bytes_mut(id).copy_from_slice(&bytes);
            } else {
                for i in 0..slen {
                    let v = r.typed_get(src, i).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    r.typed_put(id, i, v);
                }
            }
            r.heap.work += (slen >> 3) as u64;
            Ok(Val::Obj(id))
        }
        ObjKind::ArrayBuffer(_) => {
            let off = to_index(
                r,
                arg(args, 1),
                "Start offset is outside the bounds of the buffer",
            )?;
            if off % sz != 0 {
                return Err(r.range_error(&alloc::format!(
                    "start offset of {} should be a multiple of {sz}",
                    kind.name()
                )));
            }
            let len_arg = arg(args, 2);
            let new_len = if len_arg.is_undef() {
                None
            } else {
                Some(to_index(r, len_arg, "Invalid typed array length")?)
            };
            if r.buf_detached(src) {
                return Err(r.type_error("Cannot perform Construct on a detached ArrayBuffer"));
            }
            let blen = r.buf_data(src).map_or(0, |d| d.bytes.len());
            let n = match new_len {
                None => {
                    if !blen.is_multiple_of(sz) {
                        return Err(r.range_error(&alloc::format!(
                            "byte length of {} should be a multiple of {sz}",
                            kind.name()
                        )));
                    }
                    if off > blen {
                        return Err(r.range_error(&alloc::format!(
                            "Start offset {off} is outside the bounds of the buffer"
                        )));
                    }
                    (blen - off) / sz
                }
                Some(n) => {
                    if off.saturating_add(n.saturating_mul(sz)) > blen {
                        return Err(
                            r.range_error(&alloc::format!("Invalid typed array length: {n}"))
                        );
                    }
                    n
                }
            };
            Ok(Val::Obj(r.typed_new(kind, src, off, n, Some(proto))))
        }
        _ => {
            // An iterable or an array-like.
            let mark = r.temps.len();
            r.temps.push(first);
            let res: Res<ObjId> = (|| {
                let ik = PropKey::Sym(r.sym.iterator);
                let m = r.get_v(first, ik)?;
                if !m.is_nullish() {
                    let vals = r.iterate_to_vec(first)?;
                    r.temps.extend_from_slice(&vals);
                    let id = r.typed_alloc(kind, vals.len(), Some(proto))?;
                    r.temps.push(Val::Obj(id));
                    for (i, v) in vals.iter().enumerate() {
                        let n = num_of(r, *v)?;
                        r.typed_put(id, i, n);
                    }
                    Ok(id)
                } else {
                    let lk = r.key(N::length);
                    let l = r.get_v(first, lk)?;
                    let len = r.to_length(l)?;
                    if len > r.max_buffer as f64 {
                        return Err(r.range_error("Invalid typed array length"));
                    }
                    let len = len as usize;
                    let id = r.typed_alloc(kind, len, Some(proto))?;
                    r.temps.push(Val::Obj(id));
                    for i in 0..len {
                        let v = r.get_elem(first, Val::num(i as f64))?;
                        let n = num_of(r, v)?;
                        r.typed_put(id, i, n);
                        if i & 4095 == 4095 {
                            r.check_mem()?;
                        }
                    }
                    Ok(id)
                }
            })();
            r.temps.truncate(mark);
            Ok(Val::Obj(res?))
        }
    }
}

/// TypedArrayCreate: construct with `ctor` and check the result is a typed array (and
/// long enough if a length was asked for).
fn ta_create(r: &mut Realm, ctor: Val, args: &[Val]) -> Res<ObjId> {
    let v = r.construct_v(ctor, args, ctor)?;
    let Val::Obj(o) = v else {
        return Err(r.type_error("this is not a typed array."));
    };
    let Some((_, buf, _, len)) = r.typed_info(o) else {
        return Err(r.type_error("this is not a typed array."));
    };
    if r.buf_detached(buf) {
        return Err(r.type_error("Cannot perform Construct on a detached ArrayBuffer"));
    }
    if args.len() == 1
        && let Some(want) = args[0].as_f64()
        && (len as f64) < want
    {
        return Err(
            r.type_error("Derived TypedArray constructor created an array which was too small")
        );
    }
    Ok(o)
}

/// TypedArraySpeciesCreate.
fn ta_species_create(r: &mut Realm, ex: ObjId, kind: TaKind, args: &[Val]) -> Res<ObjId> {
    let default = r.intr.ta_ctor(kind);
    let ck = r.key(N::constructor);
    let c = r.get_p(ex, ck, Val::Obj(ex))?;
    let ctor = if c.is_undef() {
        Val::Obj(default)
    } else if let Val::Obj(co) = c {
        let sk = PropKey::Sym(r.sym.species);
        let s = r.get_p(co, sk, c)?;
        if s.is_nullish() {
            Val::Obj(default)
        } else if r.constructible(s) {
            s
        } else {
            return Err(r.type_error("object.constructor[Symbol.species] is not a constructor"));
        }
    } else {
        return Err(r.type_error("The .constructor property is not an object"));
    };
    if let Val::Obj(co) = ctor
        && co == default
        && args.len() == 1
        && let Some(n) = args[0].as_f64()
        && n >= 0.0
        && n <= r.max_buffer as f64
    {
        return r.typed_alloc(kind, n as usize, None);
    }
    ta_create(r, ctor, args)
}

fn ta_from(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if !r.constructible(this) {
        return Err(r.type_error("TypedArray.from: this is not a constructor"));
    }
    let mapfn = arg(args, 1);
    let this_arg = arg(args, 2);
    if !mapfn.is_undef() {
        callback(r, mapfn)?;
    }
    let src = arg(args, 0);
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[this, src, mapfn, this_arg]);
    let res: Res<ObjId> = (|| {
        let ik = PropKey::Sym(r.sym.iterator);
        let m = if src.is_nullish() {
            Val::Undef
        } else {
            r.get_v(src, ik)?
        };
        let vals: Vec<Val> = if !m.is_nullish() {
            let v = r.iterate_to_vec(src)?;
            r.temps.extend_from_slice(&v);
            v
        } else {
            let o = r.to_object(src)?;
            r.temps.push(Val::Obj(o));
            let lk = r.key(N::length);
            let l = r.get_p(o, lk, Val::Obj(o))?;
            let len = r.to_length(l)?;
            if len > r.max_buffer as f64 {
                return Err(r.range_error("Invalid typed array length"));
            }
            let mut v = Vec::new();
            for i in 0..len as usize {
                let x = r.get_elem(Val::Obj(o), Val::num(i as f64))?;
                r.temps.push(x);
                v.push(x);
            }
            v
        };
        let target = ta_create(r, this, &[Val::num(vals.len() as f64)])?;
        r.temps.push(Val::Obj(target));
        for (k, v) in vals.iter().enumerate() {
            let mapped = if mapfn.is_undef() {
                *v
            } else {
                r.call_v(mapfn, this_arg, &[*v, Val::num(k as f64)])?
            };
            r.set_elem(Val::Obj(target), Val::num(k as f64), mapped, true)?;
        }
        Ok(target)
    })();
    r.temps.truncate(mark);
    Ok(Val::Obj(res?))
}

fn ta_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if !r.constructible(this) {
        return Err(r.type_error("TypedArray.of: this is not a constructor"));
    }
    let mark = r.temps.len();
    r.temps.push(this);
    let res: Res<ObjId> = (|| {
        let target = ta_create(r, this, &[Val::num(args.len() as f64)])?;
        r.temps.push(Val::Obj(target));
        for (k, v) in args.iter().enumerate() {
            r.set_elem(Val::Obj(target), Val::num(k as f64), *v, true)?;
        }
        Ok(target)
    })();
    r.temps.truncate(mark);
    Ok(Val::Obj(res?))
}

// ---- %TypedArray%.prototype accessors ----

fn proto_buffer(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = this
        && let Some((_, buf, _, _)) = r.typed_info(o)
    {
        return Ok(Val::Obj(buf));
    }
    Err(r.type_error("Receiver is not a typed array"))
}

fn proto_byte_length(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = this
        && let Some((k, _, _, _)) = r.typed_info(o)
    {
        return Ok(Val::num((r.typed_len(o) * k.size()) as f64));
    }
    Err(r.type_error("Receiver is not a typed array"))
}

fn proto_byte_offset(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = this
        && let Some((_, buf, off, _)) = r.typed_info(o)
    {
        return Ok(Val::num(if r.buf_detached(buf) { 0.0 } else { off as f64 }));
    }
    Err(r.type_error("Receiver is not a typed array"))
}

fn proto_length(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = this
        && r.typed_info(o).is_some()
    {
        return Ok(Val::num(r.typed_len(o) as f64));
    }
    Err(r.type_error("Receiver is not a typed array"))
}

fn proto_tag(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = this
        && let Some((k, _, _, _)) = r.typed_info(o)
    {
        return Ok(r.str_val(k.name()));
    }
    Ok(Val::Undef)
}

// ---- %TypedArray%.prototype methods ----

fn at(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, _, len) = this_typed(r, this, "%TypedArray%.prototype.at")?;
    let rel = r.to_integer(arg(args, 0))?;
    let k = if rel >= 0.0 { rel } else { len as f64 + rel };
    if k < 0.0 || k >= len as f64 {
        return Ok(Val::Undef);
    }
    Ok(r.typed_get(o, k as usize).unwrap_or(Val::Undef))
}

fn copy_within(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.copyWithin")?;
    let lf = len as f64;
    let to = r.rel_index(arg(args, 0), lf, 0.0)?;
    let from = r.rel_index(arg(args, 1), lf, 0.0)?;
    let fin = r.rel_index(arg(args, 2), lf, lf)?;
    let count = (fin - from).min(lf - to);
    if count > 0.0 {
        if r.buf_detached(r.typed_info(o).map_or(o, |t| t.1)) {
            return Err(r.type_error(
                "Cannot perform %TypedArray%.prototype.copyWithin on a detached ArrayBuffer",
            ));
        }
        let nlen = r.typed_len(o) as f64;
        let count = count.min(nlen - from).min(nlen - to);
        if count > 0.0 {
            let sz = kind.size();
            let bytes = r.typed_bytes_mut(o);
            let (f, t, c) = (from as usize * sz, to as usize * sz, count as usize * sz);
            if f + c <= bytes.len() && t + c <= bytes.len() {
                bytes.copy_within(f..f + c, t);
            }
            r.heap.work += (c >> 4) as u64;
        }
    }
    Ok(Val::Obj(o))
}

fn make_iter(r: &mut Realm, this: Val, kind: u8, what: &str) -> Res<Val> {
    let (o, _, _) = this_typed(r, this, what)?;
    Ok(Val::Obj(r.make_array_iter(Val::Obj(o), kind)))
}

fn entries(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    make_iter(r, this, 2, "%TypedArray%.prototype.entries")
}
fn keys(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    make_iter(r, this, 0, "%TypedArray%.prototype.keys")
}
fn values(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    make_iter(r, this, 1, "%TypedArray%.prototype.values")
}

/// Which result a predicate-style scan wants.
#[derive(Clone, Copy, PartialEq)]
enum Scan {
    Every,
    Some,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
}

fn scan(r: &mut Realm, this: Val, args: &[Val], mode: Scan, what: &str) -> Res<Val> {
    let (o, _, len) = this_typed(r, this, what)?;
    let f = callback(r, arg(args, 0))?;
    let t = arg(args, 1);
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[Val::Obj(o), f, t]);
    let res: Res<Val> = (|| {
        let backwards = matches!(mode, Scan::FindLast | Scan::FindLastIndex);
        for n in 0..len {
            let k = if backwards { len - 1 - n } else { n };
            let v = r.typed_get(o, k).unwrap_or(Val::Undef);
            let res = r.call_v(f, t, &[v, Val::num(k as f64), Val::Obj(o)])?;
            let b = r.truthy(res);
            match mode {
                Scan::Every if !b => return Ok(Val::Bool(false)),
                Scan::Some if b => return Ok(Val::Bool(true)),
                Scan::Find | Scan::FindLast if b => return Ok(v),
                Scan::FindIndex | Scan::FindLastIndex if b => return Ok(Val::num(k as f64)),
                _ => {}
            }
        }
        Ok(match mode {
            Scan::Every => Val::Bool(true),
            Scan::Some => Val::Bool(false),
            Scan::Find | Scan::FindLast => Val::Undef,
            Scan::FindIndex | Scan::FindLastIndex => Val::Int(-1),
        })
    })();
    r.temps.truncate(mark);
    res
}

fn every(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    scan(r, this, args, Scan::Every, "%TypedArray%.prototype.every")
}
fn some(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    scan(r, this, args, Scan::Some, "%TypedArray%.prototype.some")
}
fn find(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    scan(r, this, args, Scan::Find, "%TypedArray%.prototype.find")
}
fn find_index(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    scan(
        r,
        this,
        args,
        Scan::FindIndex,
        "%TypedArray%.prototype.findIndex",
    )
}
fn find_last(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    scan(
        r,
        this,
        args,
        Scan::FindLast,
        "%TypedArray%.prototype.findLast",
    )
}
fn find_last_index(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    scan(
        r,
        this,
        args,
        Scan::FindLastIndex,
        "%TypedArray%.prototype.findLastIndex",
    )
}

fn for_each(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, _, len) = this_typed(r, this, "%TypedArray%.prototype.forEach")?;
    let f = callback(r, arg(args, 0))?;
    let t = arg(args, 1);
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[Val::Obj(o), f, t]);
    let res: Res<()> = (|| {
        for k in 0..len {
            let v = r.typed_get(o, k).unwrap_or(Val::Undef);
            r.call_v(f, t, &[v, Val::num(k as f64), Val::Obj(o)])?;
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Undef)
}

fn map(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.map")?;
    let f = callback(r, arg(args, 0))?;
    let t = arg(args, 1);
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[Val::Obj(o), f, t]);
    let res: Res<ObjId> = (|| {
        let a = ta_species_create(r, o, kind, &[Val::num(len as f64)])?;
        r.temps.push(Val::Obj(a));
        for k in 0..len {
            let v = r.typed_get(o, k).unwrap_or(Val::Undef);
            let m = r.call_v(f, t, &[v, Val::num(k as f64), Val::Obj(o)])?;
            r.set_elem(Val::Obj(a), Val::num(k as f64), m, true)?;
        }
        Ok(a)
    })();
    r.temps.truncate(mark);
    Ok(Val::Obj(res?))
}

fn filter(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.filter")?;
    let f = callback(r, arg(args, 0))?;
    let t = arg(args, 1);
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[Val::Obj(o), f, t]);
    let res: Res<ObjId> = (|| {
        let mut kept: Vec<Val> = Vec::new();
        for k in 0..len {
            let v = r.typed_get(o, k).unwrap_or(Val::Undef);
            let b = r.call_v(f, t, &[v, Val::num(k as f64), Val::Obj(o)])?;
            if r.truthy(b) {
                kept.push(v);
            }
        }
        let a = ta_species_create(r, o, kind, &[Val::num(kept.len() as f64)])?;
        r.temps.push(Val::Obj(a));
        for (n, v) in kept.iter().enumerate() {
            r.set_elem(Val::Obj(a), Val::num(n as f64), *v, true)?;
        }
        Ok(a)
    })();
    r.temps.truncate(mark);
    Ok(Val::Obj(res?))
}

fn fill(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.fill")?;
    let n = num_of(r, arg(args, 0))?;
    let lf = len as f64;
    let start = r.rel_index(arg(args, 1), lf, 0.0)?;
    let end = r.rel_index(arg(args, 2), lf, lf)?;
    if r.typed_info(o).is_some_and(|t| r.buf_detached(t.1)) {
        return Err(
            r.type_error("Cannot perform %TypedArray%.prototype.fill on a detached ArrayBuffer")
        );
    }
    let end = end.min(r.typed_len(o) as f64);
    if start < end {
        let sz = kind.size();
        let mut one = [0u8; 8];
        kind.write(n, &mut one);
        let (s, e) = (start as usize, end as usize);
        let bytes = r.typed_bytes_mut(o);
        if sz == 1 {
            if let Some(d) = bytes.get_mut(s..e) {
                d.fill(one[0]);
            }
        } else {
            for chunk in bytes
                .get_mut(s * sz..e * sz)
                .unwrap_or(&mut [])
                .chunks_exact_mut(sz)
            {
                chunk.copy_from_slice(&one[..sz]);
            }
        }
        r.heap.work += ((e - s) >> 4) as u64;
    }
    Ok(Val::Obj(o))
}

/// indexOf / lastIndexOf / includes.
fn search(r: &mut Realm, this: Val, args: &[Val], mode: u8, what: &str) -> Res<Val> {
    let (o, _, len) = this_typed(r, this, what)?;
    let none = if mode == 2 {
        Val::Bool(false)
    } else {
        Val::Int(-1)
    };
    if len == 0 {
        return Ok(none);
    }
    let lf = len as f64;
    let (mut k, step): (f64, f64) = if mode == 1 {
        let n = if args.len() > 1 {
            r.to_integer(args[1])?
        } else {
            lf - 1.0
        };
        if n == f64::NEG_INFINITY {
            return Ok(none);
        }
        (if n >= 0.0 { n.min(lf - 1.0) } else { lf + n }, -1.0)
    } else {
        let n = r.to_integer(arg(args, 1))?;
        if n == f64::INFINITY {
            return Ok(none);
        }
        (if n >= 0.0 { n } else { (lf + n).max(0.0) }, 1.0)
    };
    // Only a number can equal an element.
    let Some(x) = arg(args, 0).as_f64() else {
        return Ok(none);
    };
    // A detached buffer ends up with len 0 here: nothing found (includes sees undefined).
    while k >= 0.0 && k < lf {
        match r.typed_get(o, k as usize).and_then(|v| v.as_f64()) {
            Some(e) => {
                if e == x || (mode == 2 && e.is_nan() && x.is_nan()) {
                    return Ok(if mode == 2 {
                        Val::Bool(true)
                    } else {
                        Val::num(k)
                    });
                }
            }
            None => {
                // The array shrank (detached) under a coercion: undefined matches nothing
                // we can search for.
                return Ok(none);
            }
        }
        k += step;
    }
    Ok(none)
}

fn includes(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    search(r, this, args, 2, "%TypedArray%.prototype.includes")
}
fn index_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    search(r, this, args, 0, "%TypedArray%.prototype.indexOf")
}
fn last_index_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    search(r, this, args, 1, "%TypedArray%.prototype.lastIndexOf")
}

fn join(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, _, len) = this_typed(r, this, "%TypedArray%.prototype.join")?;
    let sep: Vec<u16> = if arg(args, 0).is_undef() {
        alloc::vec![b',' as u16]
    } else {
        r.to_units(arg(args, 0))?
    };
    if len > 1 {
        r.check_str_len((len - 1).saturating_mul(sep.len()).saturating_add(len))?;
    }
    let mut out: Vec<u16> = Vec::new();
    for k in 0..len {
        if k > 0 {
            out.extend_from_slice(&sep);
        }
        if let Some(v) = r.typed_get(o, k) {
            match v {
                Val::Int(i) => out.extend(crate::numconv::i32_to_string(i).encode_utf16()),
                Val::Num(n) => out.extend(crate::numconv::number_to_string(n).encode_utf16()),
                _ => {}
            }
        }
        if k & 4095 == 4095 {
            r.check_mem()?;
            r.check_str_len(out.len())?;
        }
    }
    Ok(r.units_val(out))
}

fn reduce_impl(r: &mut Realm, this: Val, args: &[Val], right: bool) -> Res<Val> {
    let what = if right {
        "%TypedArray%.prototype.reduceRight"
    } else {
        "%TypedArray%.prototype.reduce"
    };
    let (o, _, len) = this_typed(r, this, what)?;
    let f = callback(r, arg(args, 0))?;
    let mut n = 0usize;
    let mut acc = if args.len() > 1 {
        args[1]
    } else {
        if len == 0 {
            return Err(r.type_error("Reduce of empty array with no initial value"));
        }
        n = 1;
        let k = if right { len - 1 } else { 0 };
        r.typed_get(o, k).unwrap_or(Val::Undef)
    };
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[Val::Obj(o), f]);
    let res: Res<Val> = (|| {
        while n < len {
            let k = if right { len - 1 - n } else { n };
            let v = r.typed_get(o, k).unwrap_or(Val::Undef);
            r.temps.push(acc);
            acc = r.call_v(f, Val::Undef, &[acc, v, Val::num(k as f64), Val::Obj(o)])?;
            n += 1;
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
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.reverse")?;
    let sz = kind.size();
    let bytes = r.typed_bytes_mut(o);
    let (mut lo, mut hi) = (0usize, len);
    while lo + 1 < hi {
        hi -= 1;
        for j in 0..sz {
            bytes.swap(lo * sz + j, hi * sz + j);
        }
        lo += 1;
    }
    r.heap.work += (len >> 4) as u64;
    Ok(Val::Obj(o))
}

fn set(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("this is not a typed array."));
    };
    let Some((kind, buf, _, _)) = r.typed_info(o) else {
        return Err(r.type_error("this is not a typed array."));
    };
    let off = r.to_integer(arg(args, 1))?;
    if off < 0.0 {
        return Err(r.range_error("offset is out of bounds"));
    }
    if r.buf_detached(buf) {
        return Err(
            r.type_error("Cannot perform %TypedArray%.prototype.set on a detached ArrayBuffer")
        );
    }
    let tlen = r.typed_len(o) as f64;
    let src = arg(args, 0);
    if let Val::Obj(so) = src
        && let Some((sk, sbuf, _, slen)) = r.typed_info(so)
    {
        if r.buf_detached(sbuf) {
            return Err(r.type_error(
                "Cannot perform %TypedArray%.prototype.set on a detached ArrayBuffer",
            ));
        }
        if off + slen as f64 > tlen {
            return Err(r.range_error("offset is out of bounds"));
        }
        let off = off as usize;
        if sk == kind {
            let tmp = r.typed_bytes(so).to_vec();
            let sz = kind.size();
            let dst = r.typed_bytes_mut(o);
            if let Some(d) = dst.get_mut(off * sz..off * sz + tmp.len()) {
                d.copy_from_slice(&tmp);
            }
        } else {
            let vals: Vec<f64> = (0..slen)
                .map(|i| r.typed_get(so, i).and_then(|v| v.as_f64()).unwrap_or(0.0))
                .collect();
            for (i, v) in vals.into_iter().enumerate() {
                r.typed_put(o, off + i, v);
            }
        }
        r.heap.work += (slen >> 3) as u64;
        return Ok(Val::Undef);
    }
    let so = r.to_object(src)?;
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[Val::Obj(o), Val::Obj(so)]);
    let res: Res<()> = (|| {
        let lk = r.key(N::length);
        let l = r.get_p(so, lk, Val::Obj(so))?;
        let slen = r.to_length(l)?;
        if off + slen > tlen {
            return Err(r.range_error("offset is out of bounds"));
        }
        let off = off as usize;
        for i in 0..slen as usize {
            let v = r.get_elem(Val::Obj(so), Val::num(i as f64))?;
            let n = num_of(r, v)?;
            r.typed_put(o, off + i, n);
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Undef)
}

fn slice(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.slice")?;
    let lf = len as f64;
    let start = r.rel_index(arg(args, 0), lf, 0.0)?;
    let end = r.rel_index(arg(args, 1), lf, lf)?;
    let count = (end - start).max(0.0) as usize;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    let res: Res<ObjId> = (|| {
        let a = ta_species_create(r, o, kind, &[Val::num(count as f64)])?;
        r.temps.push(Val::Obj(a));
        if count > 0 {
            if r.typed_info(o).is_some_and(|t| r.buf_detached(t.1)) {
                return Err(r.type_error(
                    "Cannot perform %TypedArray%.prototype.slice on a detached ArrayBuffer",
                ));
            }
            let nlen = r.typed_len(o);
            let end = (end as usize).min(nlen);
            let start = start as usize;
            let (tk, ..) = r.typed_info(a).unwrap_or((kind, a, 0, 0));
            if tk == kind {
                let sz = kind.size();
                if start < end {
                    let tmp = r.typed_bytes(o)[start * sz..end * sz].to_vec();
                    let dst = r.typed_bytes_mut(a);
                    let n = tmp.len().min(dst.len());
                    dst[..n].copy_from_slice(&tmp[..n]);
                }
            } else {
                for (n, k) in (start..end).enumerate() {
                    let v = r.typed_get(o, k).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    r.typed_put(a, n, v);
                }
            }
            r.heap.work += (count >> 4) as u64;
        }
        Ok(a)
    })();
    r.temps.truncate(mark);
    Ok(Val::Obj(res?))
}

/// Sorts the elements (a list of numbers) by the default order or a comparator.
fn sorted_values(r: &mut Realm, o: ObjId, len: usize, cmpfn: Val) -> Res<Vec<f64>> {
    let mut vals: Vec<f64> = (0..len)
        .map(|i| r.typed_get(o, i).and_then(|v| v.as_f64()).unwrap_or(0.0))
        .collect();
    r.heap.work += (len >> 2) as u64;
    if cmpfn.is_undef() {
        vals.sort_by(|a, b| {
            // NaN last, -0 before +0.
            match (a.is_nan(), b.is_nan()) {
                (true, true) => core::cmp::Ordering::Equal,
                (true, false) => core::cmp::Ordering::Greater,
                (false, true) => core::cmp::Ordering::Less,
                _ => a
                    .partial_cmp(b)
                    .unwrap_or(core::cmp::Ordering::Equal)
                    .then_with(|| {
                        let (sa, sb) = (a.is_sign_negative(), b.is_sign_negative());
                        sb.cmp(&sa)
                    }),
            }
        });
        return Ok(vals);
    }
    let mut vs: Vec<Val> = vals.iter().map(|x| Val::num(*x)).collect();
    let mark = r.temps.len();
    r.temps.extend_from_slice(&[Val::Obj(o), cmpfn]);
    let res = super::array::sort_values(r, &mut vs, cmpfn);
    r.temps.truncate(mark);
    res?;
    Ok(vs.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
}

fn sort(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let cmpfn = arg(args, 0);
    if !cmpfn.is_undef() && !r.callable(cmpfn) {
        return Err(r.type_error("The comparison function must be either a function or undefined"));
    }
    let (o, _, len) = this_typed(r, this, "%TypedArray%.prototype.sort")?;
    let vals = sorted_values(r, o, len, cmpfn)?;
    for (i, v) in vals.into_iter().enumerate() {
        r.typed_put(o, i, v);
    }
    Ok(Val::Obj(o))
}

fn to_sorted(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let cmpfn = arg(args, 0);
    if !cmpfn.is_undef() && !r.callable(cmpfn) {
        return Err(r.type_error("The comparison function must be either a function or undefined"));
    }
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.toSorted")?;
    let a = r.typed_alloc(kind, len, None)?;
    r.temps.push(Val::Obj(a));
    let vals = sorted_values(r, o, len, cmpfn)?;
    for (i, v) in vals.into_iter().enumerate() {
        r.typed_put(a, i, v);
    }
    Ok(Val::Obj(a))
}

fn to_reversed(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.toReversed")?;
    let a = r.typed_alloc(kind, len, None)?;
    for i in 0..len {
        let v = r
            .typed_get(o, len - 1 - i)
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        r.typed_put(a, i, v);
    }
    r.heap.work += (len >> 4) as u64;
    Ok(Val::Obj(a))
}

fn with(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let (o, kind, len) = this_typed(r, this, "%TypedArray%.prototype.with")?;
    let rel = r.to_integer(arg(args, 0))?;
    let idx = if rel >= 0.0 { rel } else { len as f64 + rel };
    let n = num_of(r, arg(args, 1))?;
    if idx < 0.0 || idx >= r.typed_len(o) as f64 {
        return Err(r.range_error("Invalid typed array index"));
    }
    let a = r.typed_alloc(kind, len, None)?;
    let tmp = r.typed_bytes(o).to_vec();
    r.typed_bytes_mut(a).copy_from_slice(&tmp);
    r.typed_put(a, idx as usize, n);
    Ok(Val::Obj(a))
}

fn subarray(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("this is not a typed array."));
    };
    let Some((kind, buf, off, _)) = r.typed_info(o) else {
        return Err(r.type_error("this is not a typed array."));
    };
    let len = r.typed_len(o) as f64;
    let begin = r.rel_index(arg(args, 0), len, 0.0)?;
    let end = r.rel_index(arg(args, 1), len, len)?;
    let new_len = (end - begin).max(0.0);
    let begin_off = off as f64 + begin * kind.size() as f64;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    let res = ta_species_create(
        r,
        o,
        kind,
        &[Val::Obj(buf), Val::num(begin_off), Val::num(new_len)],
    );
    r.temps.truncate(mark);
    Ok(Val::Obj(res?))
}

// ---- DataView ----

fn data_view_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error("Constructor DataView requires 'new'"));
    }
    let Val::Obj(buf) = arg(args, 0) else {
        return Err(r.type_error("First argument to DataView constructor must be an ArrayBuffer"));
    };
    if !matches!(r.o(buf).kind, ObjKind::ArrayBuffer(_)) {
        return Err(r.type_error("First argument to DataView constructor must be an ArrayBuffer"));
    }
    let off = to_index(
        r,
        arg(args, 1),
        "Start offset is outside the bounds of the buffer",
    )?;
    if r.buf_detached(buf) {
        return Err(r.type_error("Cannot perform DataView on a detached ArrayBuffer"));
    }
    let blen = r.buf_data(buf).map_or(0, |d| d.bytes.len());
    if off > blen {
        return Err(r.range_error(&alloc::format!(
            "Start offset {off} is outside the bounds of the buffer"
        )));
    }
    let view_len = if arg(args, 2).is_undef() {
        blen - off
    } else {
        let l = to_index(r, arg(args, 2), "Invalid DataView length")?;
        if off.saturating_add(l) > blen {
            return Err(r.range_error(&alloc::format!("Invalid DataView length {l}")));
        }
        l
    };
    let proto = r.proto_from_ctor(nt, |r| r.intr.data_view_proto)?;
    if r.buf_detached(buf) {
        return Err(r.type_error("Cannot perform DataView on a detached ArrayBuffer"));
    }
    let id = r.alloc(
        Some(proto),
        ObjKind::DataView(alloc::boxed::Box::new(ViewData {
            buf,
            offset: off,
            len: view_len,
        })),
    );
    Ok(Val::Obj(id))
}

fn this_view(r: &mut Realm, this: Val) -> Res<(ObjId, ObjId, usize, usize)> {
    if let Val::Obj(o) = this
        && let ObjKind::DataView(d) = &r.o(o).kind
    {
        return Ok((o, d.buf, d.offset, d.len));
    }
    Err(r.type_error("Receiver is not a DataView"))
}

fn dv_buffer(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let (_, buf, _, _) = this_view(r, this)?;
    Ok(Val::Obj(buf))
}

fn dv_byte_length(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let (_, buf, _, len) = this_view(r, this)?;
    if r.buf_detached(buf) {
        return Err(
            r.type_error("Cannot perform DataView.prototype.byteLength on a detached ArrayBuffer")
        );
    }
    Ok(Val::num(len as f64))
}

fn dv_byte_offset(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let (_, buf, off, _) = this_view(r, this)?;
    if r.buf_detached(buf) {
        return Err(
            r.type_error("Cannot perform DataView.prototype.byteOffset on a detached ArrayBuffer")
        );
    }
    Ok(Val::num(off as f64))
}

fn dv_get(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let kind = kind_of_callee(r);
    let (_, buf, off, vlen) = this_view(r, this)?;
    let idx = to_index(
        r,
        arg(args, 0),
        "Offset is outside the bounds of the DataView",
    )?;
    let little = r.truthy(arg(args, 1));
    if r.buf_detached(buf) {
        return Err(r.type_error("Cannot perform DataView.prototype.get on a detached ArrayBuffer"));
    }
    let sz = kind.size();
    if idx.saturating_add(sz) > vlen {
        return Err(r.range_error("Offset is outside the bounds of the DataView"));
    }
    let mut tmp = [0u8; 8];
    match r
        .buf_data(buf)
        .and_then(|d| d.bytes.get(off + idx..off + idx + sz))
    {
        Some(s) => tmp[..sz].copy_from_slice(s),
        None => return Err(r.range_error("Offset is outside the bounds of the DataView")),
    }
    if !little {
        tmp[..sz].reverse();
    }
    Ok(Val::num(kind.read(&tmp[..sz])))
}

fn dv_set(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let kind = kind_of_callee(r);
    let (_, buf, off, vlen) = this_view(r, this)?;
    let idx = to_index(
        r,
        arg(args, 0),
        "Offset is outside the bounds of the DataView",
    )?;
    let n = num_of(r, arg(args, 1))?;
    let little = r.truthy(arg(args, 2));
    if r.buf_detached(buf) {
        return Err(r.type_error("Cannot perform DataView.prototype.set on a detached ArrayBuffer"));
    }
    let sz = kind.size();
    if idx.saturating_add(sz) > vlen {
        return Err(r.range_error("Offset is outside the bounds of the DataView"));
    }
    let mut tmp = [0u8; 8];
    kind.write(n, &mut tmp);
    if !little {
        tmp[..sz].reverse();
    }
    if let ObjKind::ArrayBuffer(b) = &mut r.heap.om(buf).kind
        && let Some(d) = b.bytes.get_mut(off + idx..off + idx + sz)
    {
        d.copy_from_slice(&tmp[..sz]);
        return Ok(Val::Undef);
    }
    Err(r.range_error("Offset is outside the bounds of the DataView"))
}

// ---- setup ----

pub(crate) fn init(r: &mut Realm) {
    let op = r.intr.object_proto;
    // ArrayBuffer
    let abp = r.alloc(Some(op), ObjKind::Ordinary);
    r.intr.array_buffer_proto = abp;
    let abc = make_ctor(
        r,
        "ArrayBuffer",
        1,
        array_buffer_ctor,
        abp,
        Vec::new(),
        true,
    );
    r.intr.array_buffer_ctor = abc;
    r.method(abc, "isView", 1, ab_is_view);
    let sp = r.sym.species;
    r.getter_sym(abc, sp, "[Symbol.species]", species_getter);
    r.getter(abp, "byteLength", ab_byte_length, None);
    r.getter(abp, "maxByteLength", ab_byte_length, None);
    r.getter(abp, "resizable", ab_resizable, None);
    r.getter(abp, "detached", ab_detached, None);
    r.method(abp, "slice", 2, ab_slice);
    r.method(abp, "transfer", 0, ab_transfer);
    r.method(abp, "transferToFixedLength", 0, ab_transfer_fixed);
    r.to_string_tag(abp, "ArrayBuffer");

    // %TypedArray%
    let tap = r.alloc(Some(op), ObjKind::Ordinary);
    r.intr.typed_array_proto = tap;
    let tac = make_ctor(r, "TypedArray", 0, abstract_ctor, tap, Vec::new(), false);
    r.intr.typed_array_ctor = tac;
    r.method(tac, "from", 1, ta_from);
    r.method(tac, "of", 0, ta_of);
    r.getter_sym(tac, sp, "[Symbol.species]", species_getter);
    r.getter(tap, "buffer", proto_buffer, None);
    r.getter(tap, "byteLength", proto_byte_length, None);
    r.getter(tap, "byteOffset", proto_byte_offset, None);
    r.getter(tap, "length", proto_length, None);
    let tst = r.sym.to_string_tag;
    r.getter_sym(tap, tst, "[Symbol.toStringTag]", proto_tag);
    r.method(tap, "at", 1, at);
    r.method(tap, "copyWithin", 2, copy_within);
    r.method(tap, "entries", 0, entries);
    r.method(tap, "every", 1, every);
    r.method(tap, "fill", 1, fill);
    r.method(tap, "filter", 1, filter);
    r.method(tap, "find", 1, find);
    r.method(tap, "findIndex", 1, find_index);
    r.method(tap, "findLast", 1, find_last);
    r.method(tap, "findLastIndex", 1, find_last_index);
    r.method(tap, "forEach", 1, for_each);
    r.method(tap, "includes", 1, includes);
    r.method(tap, "indexOf", 1, index_of);
    r.method(tap, "join", 1, join);
    r.method(tap, "keys", 0, keys);
    r.method(tap, "lastIndexOf", 1, last_index_of);
    r.method(tap, "map", 1, map);
    r.method(tap, "reduce", 1, reduce);
    r.method(tap, "reduceRight", 1, reduce_right);
    r.method(tap, "reverse", 0, reverse);
    r.method(tap, "set", 1, set);
    r.method(tap, "slice", 2, slice);
    r.method(tap, "some", 1, some);
    r.method(tap, "sort", 1, sort);
    r.method(tap, "subarray", 2, subarray);
    r.method(tap, "toReversed", 0, to_reversed);
    r.method(tap, "toSorted", 1, to_sorted);
    r.method(tap, "with", 2, with);
    let vf = r.method(tap, "values", 0, values);
    let ik = PropKey::Sym(r.sym.iterator);
    r.put_prop(tap, ik, Val::Obj(vf), WC);
    // toString is Array.prototype.toString itself; toLocaleString reuses the generic one.
    let ap = r.intr.array_proto;
    for name in ["toString", "toLocaleString"] {
        let k = r.key_str(name);
        if let Some(p) = r.o(ap).props.get(k) {
            let v = p.v;
            r.put_prop(tap, k, v, WC);
        }
    }

    // The nine concrete constructors.
    for (i, kind) in TA_KINDS.iter().enumerate() {
        let proto = r.alloc(Some(tap), ObjKind::Ordinary);
        let c = make_ctor(
            r,
            kind.name(),
            3,
            typed_ctor,
            proto,
            alloc::vec![Val::Int(i as i32)],
            true,
        );
        r.om(c).proto = Some(tac);
        let bpe = Val::Int(kind.size() as i32);
        r.value(c, "BYTES_PER_ELEMENT", bpe, 0);
        r.value(proto, "BYTES_PER_ELEMENT", bpe, 0);
        r.intr.set_ta(*kind, proto, c);
    }

    // DataView
    let dvp = r.alloc(Some(op), ObjKind::Ordinary);
    r.intr.data_view_proto = dvp;
    make_ctor(r, "DataView", 1, data_view_ctor, dvp, Vec::new(), true);
    r.getter(dvp, "buffer", dv_buffer, None);
    r.getter(dvp, "byteLength", dv_byte_length, None);
    r.getter(dvp, "byteOffset", dv_byte_offset, None);
    r.to_string_tag(dvp, "DataView");
    for (i, kind) in TA_KINDS.iter().enumerate() {
        if *kind == TaKind::Uint8C {
            continue;
        }
        let base = kind.name().trim_end_matches("Array");
        method_slots(
            r,
            dvp,
            &alloc::format!("get{base}"),
            1,
            dv_get,
            alloc::vec![Val::Int(i as i32)],
        );
        method_slots(
            r,
            dvp,
            &alloc::format!("set{base}"),
            2,
            dv_set,
            alloc::vec![Val::Int(i as i32)],
        );
    }
}
