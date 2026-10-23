//! Object and Object.prototype, property descriptors, integrity levels.

use alloc::vec::Vec;

use crate::names::N;
use crate::object::{ACCESSOR, C, Desc, E, EXTENSIBLE, ObjKind, Own, PropKey, SPARSE, W, WEC};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

use super::arg;

impl Realm {
    /// FromPropertyDescriptor.
    pub(crate) fn from_desc(&mut self, own: Own) -> Val {
        let o = self.new_plain();
        match own {
            Own::Data(v, f) => {
                let k = self.key(N::value);
                self.put_prop(o, k, v, WEC);
                let k = self.key(N::writable);
                self.put_prop(o, k, Val::Bool(f & W != 0), WEC);
            }
            Own::Accessor(g, s, _) => {
                let k = self.key(N::get);
                self.put_prop(o, k, g, WEC);
                let k = self.key(N::set);
                self.put_prop(o, k, s, WEC);
            }
        }
        let f = own.flags();
        let k = self.key(N::enumerable);
        self.put_prop(o, k, Val::Bool(f & E != 0), WEC);
        let k = self.key(N::configurable);
        self.put_prop(o, k, Val::Bool(f & C != 0), WEC);
        Val::Obj(o)
    }

    /// ToPropertyDescriptor.
    pub(crate) fn to_desc(&mut self, v: Val) -> Res<Desc> {
        let Val::Obj(o) = v else {
            return Err(self.type_error("Property description must be an object"));
        };
        let mut d = Desc::default();
        for (n, slot) in [
            (N::enumerable, 0),
            (N::configurable, 1),
            (N::value, 2),
            (N::writable, 3),
            (N::get, 4),
            (N::set, 5),
        ] {
            let k = self.key(n);
            if self.has_property(o, k)? {
                let x = self.get_p(o, k, v)?;
                match slot {
                    0 => d.enumerable = Some(self.truthy(x)),
                    1 => d.configurable = Some(self.truthy(x)),
                    2 => d.value = Some(x),
                    3 => d.writable = Some(self.truthy(x)),
                    4 => {
                        if !x.is_undef() && !self.callable(x) {
                            return Err(self.type_error("Getter must be a function"));
                        }
                        d.get = Some(x);
                    }
                    _ => {
                        if !x.is_undef() && !self.callable(x) {
                            return Err(self.type_error("Setter must be a function"));
                        }
                        d.set = Some(x);
                    }
                }
            }
        }
        if d.is_accessor() && d.is_data() {
            return Err(self.type_error("Invalid property descriptor. Cannot both specify accessors and a value or writable attribute"));
        }
        Ok(d)
    }

    /// SetIntegrityLevel (sealed or frozen).
    pub(crate) fn set_integrity(&mut self, o: ObjId, frozen: bool) -> Res<()> {
        if self.o(o).flags & crate::object::LAZY_FN != 0 {
            self.materialize_fn(o);
        }
        if matches!(self.o(o).kind, ObjKind::Array(_)) {
            self.array_to_sparse(o);
            if frozen {
                self.om(o).flags |= crate::object::LEN_RO;
            }
        }
        self.om(o).flags &= !EXTENSIBLE;
        let n = self.o(o).props.ents.len();
        for i in 0..n {
            let p = &mut self.om(o).props.ents[i].1;
            p.f &= !C;
            if frozen && p.f & ACCESSOR == 0 {
                p.f &= !W;
            }
        }
        Ok(())
    }

    pub(crate) fn test_integrity(&mut self, o: ObjId, frozen: bool) -> Res<bool> {
        if self.o(o).flags & EXTENSIBLE != 0 {
            return Ok(false);
        }
        let keys = self.own_keys_p(o, true, true, false)?;
        for k in keys {
            if let Some(own) = self.get_own(o, k)? {
                let f = own.flags();
                if f & C != 0 {
                    return Ok(false);
                }
                if frozen && matches!(own, Own::Data(..)) && f & W != 0 {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Object.prototype.toString's tag.
    pub(crate) fn builtin_tag(&mut self, v: Val) -> Res<alloc::string::String> {
        let o = match v {
            Val::Undef | Val::Empty => return Ok("[object Undefined]".into()),
            Val::Null => return Ok("[object Null]".into()),
            _ => self.to_object(v)?,
        };
        let base = match &self.o(o).kind {
            ObjKind::Array(_) => "Array",
            ObjKind::Arguments => "Arguments",
            ObjKind::Error(_) => "Error",
            ObjKind::BoolBox(_) => "Boolean",
            ObjKind::NumBox(_) => "Number",
            ObjKind::StrBox(_) => "String",
            ObjKind::Date(_) => "Date",
            ObjKind::RegExp(_) => "RegExp",
            _ if self.callable(Val::Obj(o)) => "Function",
            _ => "Object",
        };
        let tk = PropKey::Sym(self.sym.to_string_tag);
        let tag = self.get_p(o, tk, Val::Obj(o))?;
        if let Val::Str(s) = tag {
            let t = self.rust_string(s);
            return Ok(alloc::format!("[object {t}]"));
        }
        if let ObjKind::Host { class, .. } = self.o(o).kind
            && let Some(c) = self.classes.get(class as usize)
        {
            return Ok(alloc::format!("[object {}]", c.class.name()));
        }
        Ok(alloc::format!("[object {base}]"))
    }
}

fn object_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if let Val::Obj(n) = nt
        && n != r.intr.object_ctor
    {
        let p = r.proto_from_ctor(nt, |r| r.intr.object_proto)?;
        return Ok(Val::Obj(r.alloc(Some(p), ObjKind::Ordinary)));
    }
    let v = arg(args, 0);
    if v.is_nullish() {
        return Ok(Val::Obj(r.new_plain()));
    }
    Ok(Val::Obj(r.to_object(v)?))
}

fn keys_of(r: &mut Realm, v: Val, mode: u8) -> Res<Val> {
    let o = r.to_object(v)?;
    let keys = r.own_keys_p(o, true, false, false)?;
    let mut out = Vec::with_capacity(keys.len());
    for k in keys {
        let Some(own) = r.get_own(o, k)? else {
            continue;
        };
        if own.flags() & E == 0 {
            continue;
        }
        match mode {
            0 => out.push(r.key_to_val(k)),
            1 => out.push(r.get_p(o, k, Val::Obj(o))?),
            _ => {
                let kv = r.key_to_val(k);
                let vv = r.get_p(o, k, Val::Obj(o))?;
                let pair = r.new_array_from(alloc::vec![kv, vv]);
                out.push(Val::Obj(pair));
            }
        }
    }
    Ok(Val::Obj(r.new_array_from(out)))
}

fn keys(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    keys_of(r, arg(args, 0), 0)
}
fn values(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    keys_of(r, arg(args, 0), 1)
}
fn entries(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    keys_of(r, arg(args, 0), 2)
}

fn assign(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let to = r.to_object(arg(args, 0))?;
    for src in args.iter().skip(1) {
        let so = match src {
            Val::Undef | Val::Null => continue,
            Val::Obj(o) => *o,
            v => r.to_object(*v)?,
        };
        let keys = r.own_keys_p(so, true, true, false)?;
        for k in keys {
            if let Some(own) = r.get_own(so, k)?
                && own.flags() & E != 0
            {
                let v = r.get_p(so, k, Val::Obj(so))?;
                r.set_v(Val::Obj(to), k, v, true)?;
            }
        }
    }
    Ok(Val::Obj(to))
}

fn define_properties_from(r: &mut Realm, o: ObjId, props: Val) -> Res<()> {
    let po = r.to_object(props)?;
    let keys = r.own_keys_p(po, true, true, false)?;
    let mut descs = Vec::new();
    for k in keys {
        if let Some(own) = r.get_own(po, k)?
            && own.flags() & E != 0
        {
            let dv = r.get_p(po, k, Val::Obj(po))?;
            let d = r.to_desc(dv)?;
            descs.push((k, d));
        }
    }
    for (k, d) in descs {
        if !r.define_own(o, k, d)? {
            let ks = r.key_display(k);
            return Err(r.type_error(&alloc::format!("Cannot redefine property: {ks}")));
        }
    }
    Ok(())
}

fn create(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let p = match arg(args, 0) {
        Val::Obj(p) => Some(p),
        Val::Null => None,
        _ => return Err(r.type_error("Object prototype may only be an Object or null")),
    };
    let o = r.alloc(p, ObjKind::Ordinary);
    let props = arg(args, 1);
    if !props.is_undef() {
        define_properties_from(r, o, props)?;
    }
    Ok(Val::Obj(o))
}

fn define_property(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = arg(args, 0) else {
        return Err(r.type_error("Object.defineProperty called on non-object"));
    };
    let k = r.to_key(arg(args, 1))?;
    let d = r.to_desc(arg(args, 2))?;
    if !r.define_own(o, k, d)? {
        let ks = r.key_display(k);
        return Err(r.type_error(&alloc::format!("Cannot redefine property: {ks}")));
    }
    Ok(Val::Obj(o))
}

fn define_properties(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = arg(args, 0) else {
        return Err(r.type_error("Object.defineProperties called on non-object"));
    };
    define_properties_from(r, o, arg(args, 1))?;
    Ok(Val::Obj(o))
}

fn get_own_property_names(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(arg(args, 0))?;
    let keys = r.own_keys_p(o, true, false, false)?;
    let vals: Vec<Val> = keys.into_iter().map(|k| r.key_to_val(k)).collect();
    Ok(Val::Obj(r.new_array_from(vals)))
}

fn get_own_property_symbols(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(arg(args, 0))?;
    let keys = r.own_keys_p(o, false, true, false)?;
    let vals: Vec<Val> = keys.into_iter().map(|k| r.key_to_val(k)).collect();
    Ok(Val::Obj(r.new_array_from(vals)))
}

fn get_own_property_descriptor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(arg(args, 0))?;
    let k = r.to_key(arg(args, 1))?;
    match r.get_own(o, k)? {
        Some(own) => Ok(r.from_desc(own)),
        None => Ok(Val::Undef),
    }
}

fn get_own_property_descriptors(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(arg(args, 0))?;
    let keys = r.own_keys_p(o, true, true, false)?;
    let out = r.new_plain();
    for k in keys {
        if let Some(own) = r.get_own(o, k)? {
            let d = r.from_desc(own);
            r.create_data(out, k, d)?;
        }
    }
    Ok(Val::Obj(out))
}

fn get_prototype_of(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(arg(args, 0))?;
    Ok(r.o(o).proto.map_or(Val::Null, Val::Obj))
}

fn set_prototype_of(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let v = arg(args, 0);
    if v.is_nullish() {
        return Err(r.type_error("Object.setPrototypeOf called on null or undefined"));
    }
    let p = match arg(args, 1) {
        Val::Obj(p) => Some(p),
        Val::Null => None,
        _ => return Err(r.type_error("Object prototype may only be an Object or null")),
    };
    if let Val::Obj(o) = v
        && !r.set_proto(o, p)
    {
        return Err(r.type_error("Cyclic __proto__ value or non-extensible object"));
    }
    Ok(v)
}

fn freeze(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let v = arg(args, 0);
    if let Val::Obj(o) = v {
        r.set_integrity(o, true)?;
    }
    Ok(v)
}

fn seal(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let v = arg(args, 0);
    if let Val::Obj(o) = v {
        r.set_integrity(o, false)?;
    }
    Ok(v)
}

fn prevent_extensions(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let v = arg(args, 0);
    if let Val::Obj(o) = v {
        if matches!(r.o(o).kind, ObjKind::Array(_)) {
            r.array_to_sparse(o);
        }
        r.om(o).flags &= !EXTENSIBLE;
    }
    Ok(v)
}

fn is_frozen(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    match arg(args, 0) {
        Val::Obj(o) => Ok(Val::Bool(r.test_integrity(o, true)?)),
        _ => Ok(Val::Bool(true)),
    }
}

fn is_sealed(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    match arg(args, 0) {
        Val::Obj(o) => Ok(Val::Bool(r.test_integrity(o, false)?)),
        _ => Ok(Val::Bool(true)),
    }
}

fn is_extensible(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    match arg(args, 0) {
        Val::Obj(o) => Ok(Val::Bool(r.o(o).flags & EXTENSIBLE != 0)),
        _ => Ok(Val::Bool(false)),
    }
}

fn is(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(r.same_val(arg(args, 0), arg(args, 1))))
}

fn from_entries(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let items = r.iterate_to_vec(arg(args, 0))?;
    let o = r.new_plain();
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    r.temps.extend_from_slice(&items);
    let res: Res<()> = (|| {
        for it in items {
            if !matches!(it, Val::Obj(_)) {
                return Err(r.type_error("Iterator value is not an entry object"));
            }
            let k = r.get_elem(it, Val::Int(0))?;
            let v = r.get_elem(it, Val::Int(1))?;
            let k = r.to_key(k)?;
            r.create_data(o, k, v)?;
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(o))
}

fn has_own(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(arg(args, 0))?;
    let k = r.to_key(arg(args, 1))?;
    Ok(Val::Bool(r.has_own_p(o, k)?))
}

// ---- Object.prototype ----

fn has_own_property(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let k = r.to_key(arg(args, 0))?;
    let o = r.to_object(this)?;
    Ok(Val::Bool(r.has_own_p(o, k)?))
}

fn is_prototype_of(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(mut v) = arg(args, 0) else {
        return Ok(Val::Bool(false));
    };
    let o = r.to_object(this)?;
    let mut n = 0;
    while let Some(p) = r.o(v).proto {
        if p == o {
            return Ok(Val::Bool(true));
        }
        v = p;
        n += 1;
        if n > 100_000 {
            break;
        }
    }
    Ok(Val::Bool(false))
}

fn property_is_enumerable(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let k = r.to_key(arg(args, 0))?;
    let o = r.to_object(this)?;
    Ok(Val::Bool(
        matches!(r.get_own(o, k)?, Some(own) if own.flags() & E != 0),
    ))
}

fn to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let s = r.builtin_tag(this)?;
    Ok(r.str_val(&s))
}

fn to_locale_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let k = r.key(N::toString);
    let f = r.get_v(this, k)?;
    r.call_v(f, this, &[])
}

fn value_of(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::Obj(r.to_object(this)?))
}

fn proto_get(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.to_object(this)?;
    Ok(r.o(o).proto.map_or(Val::Null, Val::Obj))
}

fn proto_set(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if this.is_nullish() {
        return Err(r.type_error("Object.prototype.__proto__ called on null or undefined"));
    }
    let p = match arg(args, 0) {
        Val::Obj(p) => Some(p),
        Val::Null => None,
        _ => return Ok(Val::Undef),
    };
    if let Val::Obj(o) = this
        && !r.set_proto(o, p)
    {
        return Err(r.type_error("Cyclic __proto__ value"));
    }
    Ok(Val::Undef)
}

fn define_getter_legacy(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(this)?;
    let f = arg(args, 1);
    if !r.callable(f) {
        return Err(r.type_error("Object.prototype.__defineGetter__: Expecting function"));
    }
    let k = r.to_key(arg(args, 0))?;
    r.define_own(
        o,
        k,
        Desc {
            get: Some(f),
            enumerable: Some(true),
            configurable: Some(true),
            ..Desc::default()
        },
    )?;
    Ok(Val::Undef)
}

fn define_setter_legacy(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.to_object(this)?;
    let f = arg(args, 1);
    if !r.callable(f) {
        return Err(r.type_error("Object.prototype.__defineSetter__: Expecting function"));
    }
    let k = r.to_key(arg(args, 0))?;
    r.define_own(
        o,
        k,
        Desc {
            set: Some(f),
            enumerable: Some(true),
            configurable: Some(true),
            ..Desc::default()
        },
    )?;
    Ok(Val::Undef)
}

pub(crate) fn init(r: &mut Realm) {
    let op = r.intr.object_proto;
    let c = r.ctor("Object", 1, object_ctor, op);
    r.intr.object_ctor = c;
    r.method(c, "keys", 1, keys);
    r.method(c, "values", 1, values);
    r.method(c, "entries", 1, entries);
    r.method(c, "assign", 2, assign);
    r.method(c, "create", 2, create);
    r.method(c, "defineProperty", 3, define_property);
    r.method(c, "defineProperties", 2, define_properties);
    r.method(c, "getOwnPropertyNames", 1, get_own_property_names);
    r.method(c, "getOwnPropertySymbols", 1, get_own_property_symbols);
    r.method(
        c,
        "getOwnPropertyDescriptor",
        2,
        get_own_property_descriptor,
    );
    r.method(
        c,
        "getOwnPropertyDescriptors",
        1,
        get_own_property_descriptors,
    );
    r.method(c, "getPrototypeOf", 1, get_prototype_of);
    r.method(c, "setPrototypeOf", 2, set_prototype_of);
    r.method(c, "freeze", 1, freeze);
    r.method(c, "seal", 1, seal);
    r.method(c, "preventExtensions", 1, prevent_extensions);
    r.method(c, "isFrozen", 1, is_frozen);
    r.method(c, "isSealed", 1, is_sealed);
    r.method(c, "isExtensible", 1, is_extensible);
    r.method(c, "is", 2, is);
    r.method(c, "fromEntries", 1, from_entries);
    r.method(c, "hasOwn", 2, has_own);
    r.method(op, "hasOwnProperty", 1, has_own_property);
    r.method(op, "isPrototypeOf", 1, is_prototype_of);
    r.method(op, "propertyIsEnumerable", 1, property_is_enumerable);
    r.method(op, "toString", 0, to_string);
    r.method(op, "toLocaleString", 0, to_locale_string);
    r.method(op, "valueOf", 0, value_of);
    r.method(op, "__defineGetter__", 2, define_getter_legacy);
    r.method(op, "__defineSetter__", 2, define_setter_legacy);
    r.getter(op, "__proto__", proto_get, Some(proto_set));
    let _ = SPARSE;
}
