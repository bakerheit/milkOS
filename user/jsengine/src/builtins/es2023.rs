//! Newer library functions pages use: Array.prototype.toSorted / toReversed / toSpliced /
//! with, Promise.withResolvers, Object.groupBy / Map.groupBy, and String.prototype
//! isWellFormed / toWellFormed.

use alloc::vec::Vec;

use crate::object::{ObjKind, WEC};
use crate::realm::{Realm, Res};
use crate::value::Val;

use super::arg;

/// The elements 0..len of an array-like as values (holes read as undefined).
fn collect(r: &mut Realm, o: crate::value::ObjId, len: f64) -> Res<Vec<Val>> {
    if len > 4294967295.0 {
        return Err(r.range_error("Invalid array length"));
    }
    let mut out = Vec::new();
    let mut i = 0.0;
    while i < len {
        let v = r.get_index(Val::Obj(o), i)?;
        r.temps.push(v);
        out.push(v);
        i += 1.0;
        if (i as u64) & 4095 == 0 {
            r.check_mem()?;
        }
    }
    Ok(out)
}

fn to_sorted(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let cmpfn = arg(args, 0);
    if !cmpfn.is_undef() && !r.callable(cmpfn) {
        return Err(r.type_error("The comparison function must be either a function or undefined"));
    }
    let o = r.this_obj(this, "Array.prototype.toSorted")?;
    let len = r.length_of(Val::Obj(o))?;
    let mark = r.temps.len();
    let res: Res<Val> = (|| {
        let mut vals = collect(r, o, len)?;
        let mut undefs = 0;
        vals.retain(|v| {
            if v.is_undef() {
                undefs += 1;
                false
            } else {
                true
            }
        });
        super::array::sort_values(r, &mut vals, cmpfn)?;
        vals.extend((0..undefs).map(|_| Val::Undef));
        Ok(Val::Obj(r.new_array_from(vals)))
    })();
    r.temps.truncate(mark);
    res
}

fn to_reversed(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.toReversed")?;
    let len = r.length_of(Val::Obj(o))?;
    let mark = r.temps.len();
    let res: Res<Val> = (|| {
        let mut vals = collect(r, o, len)?;
        vals.reverse();
        Ok(Val::Obj(r.new_array_from(vals)))
    })();
    r.temps.truncate(mark);
    res
}

fn to_spliced(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.toSpliced")?;
    let len = r.length_of(Val::Obj(o))?;
    let start = r.rel_index(arg(args, 0), len, 0.0)?;
    let skip = if args.is_empty() {
        0.0
    } else if args.len() == 1 {
        len - start
    } else {
        r.to_integer(args[1])?.clamp(0.0, len - start)
    };
    let items: &[Val] = if args.len() > 2 { &args[2..] } else { &[] };
    let new_len = len + items.len() as f64 - skip;
    if new_len > 9007199254740991.0 {
        return Err(r.type_error("Invalid array length"));
    }
    let mark = r.temps.len();
    let res: Res<Val> = (|| {
        let vals = collect(r, o, len)?;
        let mut out: Vec<Val> = Vec::with_capacity(new_len as usize);
        out.extend_from_slice(&vals[..start as usize]);
        out.extend_from_slice(items);
        out.extend_from_slice(&vals[(start + skip) as usize..]);
        Ok(Val::Obj(r.new_array_from(out)))
    })();
    r.temps.truncate(mark);
    res
}

fn with(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_obj(this, "Array.prototype.with")?;
    let len = r.length_of(Val::Obj(o))?;
    let rel = r.to_integer(arg(args, 0))?;
    let idx = if rel >= 0.0 { rel } else { len + rel };
    if idx < 0.0 || idx >= len {
        return Err(r.range_error("Invalid index"));
    }
    let mark = r.temps.len();
    let res: Res<Val> = (|| {
        let mut vals = collect(r, o, len)?;
        vals[idx as usize] = arg(args, 1);
        Ok(Val::Obj(r.new_array_from(vals)))
    })();
    r.temps.truncate(mark);
    res
}

fn with_resolvers(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if !r.constructible(this) {
        return Err(r.type_error("Promise.withResolvers called on non-object"));
    }
    let (p, res, rej) = r.promise_with_resolvers();
    let o = r.new_plain();
    for (n, v) in [("promise", Val::Obj(p)), ("resolve", res), ("reject", rej)] {
        let k = r.key_str(n);
        r.put_prop(o, k, v, WEC);
    }
    Ok(Val::Obj(o))
}

/// Groups the items of an iterable by the key `f(item, index)` returns: a list of
/// (key, items) in first-seen order.
fn group(r: &mut Realm, items: Val, f: Val, property_keys: bool) -> Res<Vec<(Val, Vec<Val>)>> {
    if items.is_nullish() {
        return Err(r.type_error("groupBy called on null or undefined"));
    }
    if !r.callable(f) {
        return Err(r.type_error("callback is not a function"));
    }
    let vals = r.iterate_to_vec(items)?;
    let mark = r.temps.len();
    r.temps.push(f);
    r.temps.extend_from_slice(&vals);
    let res: Res<Vec<(Val, Vec<Val>)>> = (|| {
        let mut groups: Vec<(Val, Vec<Val>)> = Vec::new();
        for (i, v) in vals.iter().enumerate() {
            let k = r.call_v(f, Val::Undef, &[*v, Val::num(i as f64)])?;
            let k = if property_keys {
                let pk = r.to_key(k)?;
                r.key_to_val(pk)
            } else if let Val::Num(n) = k
                && n == 0.0
            {
                Val::Int(0)
            } else {
                k
            };
            r.temps.push(k);
            let mut found = None;
            for (gi, (gk, _)) in groups.iter().enumerate() {
                let same = if property_keys {
                    r.strict_eq(*gk, k)
                } else {
                    r.same_value_zero(*gk, k)
                };
                if same {
                    found = Some(gi);
                    break;
                }
            }
            match found {
                Some(gi) => groups[gi].1.push(*v),
                None => groups.push((k, alloc::vec![*v])),
            }
            if i & 1023 == 1023 {
                r.check_mem()?;
            }
        }
        Ok(groups)
    })();
    r.temps.truncate(mark);
    res
}

fn object_group_by(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let groups = group(r, arg(args, 0), arg(args, 1), true)?;
    let o = r.alloc(None, ObjKind::Ordinary);
    r.temps.push(Val::Obj(o));
    for (k, items) in groups {
        let arr = r.new_array_from(items);
        let pk = r.to_key(k)?;
        r.create_data(o, pk, Val::Obj(arr))?;
    }
    Ok(Val::Obj(o))
}

fn map_group_by(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let groups = group(r, arg(args, 0), arg(args, 1), false)?;
    let p = r.intr.map_proto;
    let m = r.alloc(
        Some(p),
        ObjKind::Map(alloc::boxed::Box::new(super::map::MapData::new())),
    );
    r.temps.push(Val::Obj(m));
    for (k, items) in groups {
        let arr = r.new_array_from(items);
        r.temps.push(Val::Obj(arr));
        r.map_set(m, k, Val::Obj(arr));
    }
    Ok(Val::Obj(m))
}

fn is_well_formed(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "isWellFormed")?;
    let ok = char::decode_utf16(r.heap.units(s).iter().copied()).all(|c| c.is_ok());
    Ok(Val::Bool(ok))
}

fn to_well_formed(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let s = r.this_str(this, "toWellFormed")?;
    let out: Vec<u16> = {
        let mut v = Vec::new();
        for c in char::decode_utf16(r.heap.units(s).iter().copied()) {
            let ch = c.unwrap_or('\u{FFFD}');
            let mut b = [0u16; 2];
            v.extend_from_slice(ch.encode_utf16(&mut b));
        }
        v
    };
    Ok(r.units_val(out))
}

pub(crate) fn init(r: &mut Realm) {
    let ap = r.intr.array_proto;
    r.method(ap, "toSorted", 1, to_sorted);
    r.method(ap, "toReversed", 0, to_reversed);
    r.method(ap, "toSpliced", 2, to_spliced);
    r.method(ap, "with", 2, with);
    // (not in @@unscopables for the ES2023 additions that browsers also leave out)
    let pc = r.intr.promise_ctor;
    r.method(pc, "withResolvers", 0, with_resolvers);
    let oc = r.intr.object_ctor;
    r.method(oc, "groupBy", 2, object_group_by);
    let g = r.global;
    let mk = r.key_str("Map");
    if let Some(Val::Obj(mc)) = r.peek_prop(g, mk) {
        r.method(mc, "groupBy", 2, map_group_by);
    }
    let sp = r.intr.string_proto;
    r.method(sp, "isWellFormed", 0, is_well_formed);
    r.method(sp, "toWellFormed", 0, to_well_formed);
}
