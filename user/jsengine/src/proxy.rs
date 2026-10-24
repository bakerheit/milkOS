//! `Proxy` and `Proxy.revocable`: objects whose internal methods ([[Get]], [[Set]], [[Has]],
//! [[Delete]], [[OwnKeys]], [[GetOwnProperty]], [[DefineOwnProperty]], [[GetPrototypeOf]],
//! [[SetPrototypeOf]], [[IsExtensible]], [[PreventExtensions]], [[Call]], [[Construct]]) are
//! the handler's traps, or the target's when the handler has none.
//!
//! The invariants the spec lists (a trap may not lie about a non-configurable property of the
//! target, the key list must hold the non-configurable keys and, for a non-extensible target,
//! only the target's keys, ...) are checked where a lie would let script break the engine's own
//! assumptions, as TypeErrors; a few of the rarer cross-checks of descriptors are left out.
//! Recursion through proxies of proxies is bounded by the native stack guard.

use alloc::vec::Vec;

use crate::names::N;
use crate::object::{C, Desc, E, EXTENSIBLE, ObjKind, Own, PropKey, W};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

pub(crate) struct ProxyData {
    /// None once revoked.
    pub target: Option<ObjId>,
    pub handler: Option<ObjId>,
    /// `typeof` and `new` are decided when the proxy is made (they survive revocation).
    pub callable: bool,
    pub constructor: bool,
}

impl Realm {
    /// The target and handler of proxy `id`, or the TypeError of a revoked one.
    fn proxy_parts(&mut self, id: ObjId, what: &str) -> Res<(ObjId, ObjId)> {
        self.check_stack()?;
        if let ObjKind::Proxy(p) = &self.o(id).kind
            && let (Some(t), Some(h)) = (p.target, p.handler)
        {
            return Ok((t, h));
        }
        Err(self.type_error(&alloc::format!(
            "Cannot perform '{what}' on a proxy that has been revoked"
        )))
    }

    /// GetMethod(handler, name): the trap, or None when the handler has none.
    fn trap(&mut self, handler: ObjId, name: &str) -> Res<Option<Val>> {
        let k = self.key_str(name);
        let t = self.get_p(handler, k, Val::Obj(handler))?;
        if t.is_nullish() {
            return Ok(None);
        }
        if !self.callable(t) {
            return Err(self.type_error(&alloc::format!(
                "proxy trap '{name}' is not a function"
            )));
        }
        Ok(Some(t))
    }

    fn call_trap(&mut self, trap: Val, handler: ObjId, args: &[Val]) -> Res<Val> {
        let mark = self.temps.len();
        self.temps.extend_from_slice(args);
        let r = self.call_v(trap, Val::Obj(handler), args);
        self.temps.truncate(mark);
        r
    }

    pub(crate) fn proxy_get(&mut self, id: ObjId, k: PropKey, receiver: Val) -> Res<Val> {
        let (t, h) = self.proxy_parts(id, "get")?;
        let Some(trap) = self.trap(h, "get")? else {
            return self.get_p(t, k, receiver);
        };
        let kv = self.key_to_val(k);
        let v = self.call_trap(trap, h, &[Val::Obj(t), kv, receiver])?;
        match self.get_own(t, k)? {
            Some(Own::Data(tv, f)) if f & (C | W) == 0 && !self.same_val(v, tv) => {
                Err(self.type_error(
                    "'get' on proxy: property is a read-only and non-configurable data property on the proxy target but the proxy did not return its actual value",
                ))
            }
            Some(Own::Accessor(g, _, f)) if f & C == 0 && g.is_undef() && !v.is_undef() => {
                Err(self.type_error(
                    "'get' on proxy: property is a non-configurable accessor property on the proxy target and does not have a getter function, but the trap did not return 'undefined'",
                ))
            }
            _ => Ok(v),
        }
    }

    pub(crate) fn proxy_set(&mut self, id: ObjId, k: PropKey, v: Val, receiver: Val) -> Res<bool> {
        let (t, h) = self.proxy_parts(id, "set")?;
        let Some(trap) = self.trap(h, "set")? else {
            return self.set_p(t, k, v, receiver);
        };
        let kv = self.key_to_val(k);
        let r = self.call_trap(trap, h, &[Val::Obj(t), kv, v, receiver])?;
        if !self.truthy(r) {
            return Ok(false);
        }
        match self.get_own(t, k)? {
            Some(Own::Data(tv, f)) if f & (C | W) == 0 && !self.same_val(v, tv) => Err(self.type_error(
                "'set' on proxy: trap returned truish for property which exists in the proxy target as a non-configurable and non-writable data property with a different value",
            )),
            Some(Own::Accessor(_, s, f)) if f & C == 0 && s.is_undef() => Err(self.type_error(
                "'set' on proxy: trap returned truish for property which exists in the proxy target as a non-configurable and non-writable accessor property without a setter",
            )),
            _ => Ok(true),
        }
    }

    pub(crate) fn proxy_has(&mut self, id: ObjId, k: PropKey) -> Res<bool> {
        let (t, h) = self.proxy_parts(id, "has")?;
        let Some(trap) = self.trap(h, "has")? else {
            return self.has_property(t, k);
        };
        let kv = self.key_to_val(k);
        let r = self.call_trap(trap, h, &[Val::Obj(t), kv])?;
        let b = self.truthy(r);
        if !b && let Some(own) = self.get_own(t, k)? {
            if own.flags() & C == 0 {
                return Err(self.type_error(
                    "'has' on proxy: trap returned falsish for property which exists in the proxy target as non-configurable",
                ));
            }
            if self.proxy_target_extensible(t)? {
                return Ok(b);
            }
            return Err(self.type_error(
                "'has' on proxy: trap returned falsish for property but the proxy target is not extensible",
            ));
        }
        Ok(b)
    }

    pub(crate) fn proxy_delete(&mut self, id: ObjId, k: PropKey) -> Res<bool> {
        let (t, h) = self.proxy_parts(id, "deleteProperty")?;
        let Some(trap) = self.trap(h, "deleteProperty")? else {
            return self.delete_p(t, k);
        };
        let kv = self.key_to_val(k);
        let r = self.call_trap(trap, h, &[Val::Obj(t), kv])?;
        if !self.truthy(r) {
            return Ok(false);
        }
        if let Some(own) = self.get_own(t, k)? {
            if own.flags() & C == 0 {
                return Err(self.type_error(
                    "'deleteProperty' on proxy: trap returned truish for property which is non-configurable in the proxy target",
                ));
            }
            if !self.proxy_target_extensible(t)? {
                return Err(self.type_error(
                    "'deleteProperty' on proxy: trap returned truish for property but the proxy target is non-extensible",
                ));
            }
        }
        Ok(true)
    }

    /// All the keys of a proxy, as the trap (or the target) lists them.
    pub(crate) fn proxy_own_keys(&mut self, id: ObjId) -> Res<Vec<PropKey>> {
        let (t, h) = self.proxy_parts(id, "ownKeys")?;
        let Some(trap) = self.trap(h, "ownKeys")? else {
            return self.own_keys_p(t, true, true, false);
        };
        let r = self.call_trap(trap, h, &[Val::Obj(t)])?;
        let Val::Obj(arr) = r else {
            return Err(self.type_error("CreateListFromArrayLike called on non-object"));
        };
        let items = self.array_like_to_vec(Val::Obj(arr))?;
        let mark = self.temps.len();
        self.temps.extend_from_slice(&items);
        let res = (|| {
            let mut keys: Vec<PropKey> = Vec::new();
            for v in items {
                if !matches!(v, Val::Str(_) | Val::Sym(_)) {
                    let d = self.short_display(v);
                    return Err(self.type_error(&alloc::format!(
                        "{d} is not a valid property name"
                    )));
                }
                let k = self.to_key(v)?;
                if keys.contains(&k) {
                    return Err(self.type_error(
                        "'ownKeys' on proxy: trap returned duplicate entries",
                    ));
                }
                keys.push(k);
            }
            // The non-configurable keys of the target have to be there; with a target that
            // cannot grow, there can be no others.
            let tkeys = self.own_keys_p(t, true, true, false)?;
            let extensible = self.proxy_target_extensible(t)?;
            for tk in &tkeys {
                let non_config = self.get_own(t, *tk)?.is_some_and(|o| o.flags() & C == 0);
                if (non_config || !extensible) && !keys.contains(tk) {
                    return Err(self.type_error(
                        "'ownKeys' on proxy: trap result did not include a key of the target that cannot be left out",
                    ));
                }
            }
            if !extensible && keys.iter().any(|k| !tkeys.contains(k)) {
                return Err(self.type_error(
                    "'ownKeys' on proxy: trap returned extra keys but proxy target is non-extensible",
                ));
            }
            if keys.len() > 1_000_000 {
                return Err(self.range_error("Too many keys"));
            }
            Ok(keys)
        })();
        self.temps.truncate(mark);
        res
    }

    pub(crate) fn proxy_get_own(&mut self, id: ObjId, k: PropKey) -> Res<Option<Own>> {
        let (t, h) = self.proxy_parts(id, "getOwnPropertyDescriptor")?;
        let Some(trap) = self.trap(h, "getOwnPropertyDescriptor")? else {
            return self.get_own(t, k);
        };
        let kv = self.key_to_val(k);
        let r = self.call_trap(trap, h, &[Val::Obj(t), kv])?;
        let tdesc = self.get_own(t, k)?;
        if r.is_undef() {
            if let Some(td) = tdesc {
                if td.flags() & C == 0 {
                    return Err(self.type_error(
                        "'getOwnPropertyDescriptor' on proxy: trap returned undefined for property which is non-configurable in the proxy target",
                    ));
                }
                if !self.proxy_target_extensible(t)? {
                    return Err(self.type_error(
                        "'getOwnPropertyDescriptor' on proxy: trap returned undefined for property which exists in the non-extensible proxy target",
                    ));
                }
            }
            return Ok(None);
        }
        if !matches!(r, Val::Obj(_)) {
            return Err(self.type_error(
                "'getOwnPropertyDescriptor' on proxy: trap returned neither object nor undefined",
            ));
        }
        let d = self.to_desc(r)?;
        // CompletePropertyDescriptor.
        let own = if d.is_accessor() {
            let mut f = 0u8;
            if d.enumerable == Some(true) {
                f |= E;
            }
            if d.configurable == Some(true) {
                f |= C;
            }
            Own::Accessor(d.get.unwrap_or(Val::Undef), d.set.unwrap_or(Val::Undef), f)
        } else {
            let mut f = 0u8;
            if d.writable == Some(true) {
                f |= W;
            }
            if d.enumerable == Some(true) {
                f |= E;
            }
            if d.configurable == Some(true) {
                f |= C;
            }
            Own::Data(d.value.unwrap_or(Val::Undef), f)
        };
        if own.flags() & C == 0 && tdesc.is_none_or(|t| t.flags() & C != 0) {
            return Err(self.type_error(
                "'getOwnPropertyDescriptor' on proxy: trap reported non-configurability for property which is either non-existent or configurable in the proxy target",
            ));
        }
        if tdesc.is_none() && !self.proxy_target_extensible(t)? {
            return Err(self.type_error(
                "'getOwnPropertyDescriptor' on proxy: trap returned a descriptor for property that does not exist in the non-extensible proxy target",
            ));
        }
        Ok(Some(own))
    }

    /// FromPropertyDescriptor for a (partial) descriptor.
    fn desc_object(&mut self, d: Desc) -> Val {
        let o = self.new_plain();
        let put = |r: &mut Realm, n: N, v: Val| {
            let k = r.key(n);
            r.put_prop(o, k, v, crate::object::WEC);
        };
        if let Some(v) = d.value {
            put(self, N::value, v);
        }
        if let Some(w) = d.writable {
            put(self, N::writable, Val::Bool(w));
        }
        if let Some(g) = d.get {
            put(self, N::get, g);
        }
        if let Some(s) = d.set {
            put(self, N::set, s);
        }
        if let Some(e) = d.enumerable {
            put(self, N::enumerable, Val::Bool(e));
        }
        if let Some(c) = d.configurable {
            put(self, N::configurable, Val::Bool(c));
        }
        Val::Obj(o)
    }

    pub(crate) fn proxy_define_own(&mut self, id: ObjId, k: PropKey, d: Desc) -> Res<bool> {
        let (t, h) = self.proxy_parts(id, "defineProperty")?;
        let Some(trap) = self.trap(h, "defineProperty")? else {
            return self.define_own(t, k, d);
        };
        let kv = self.key_to_val(k);
        let dobj = self.desc_object(d);
        let r = self.call_trap(trap, h, &[Val::Obj(t), kv, dobj])?;
        if !self.truthy(r) {
            return Ok(false);
        }
        let tdesc = self.get_own(t, k)?;
        match tdesc {
            None => {
                if !self.proxy_target_extensible(t)? {
                    return Err(self.type_error(
                        "'defineProperty' on proxy: trap returned truish for adding property to the non-extensible proxy target",
                    ));
                }
                if d.configurable == Some(false) {
                    return Err(self.type_error(
                        "'defineProperty' on proxy: trap returned truish for defining non-configurable property which is non-existent in the proxy target",
                    ));
                }
            }
            Some(td) => {
                if d.configurable == Some(false) && td.flags() & C != 0 {
                    return Err(self.type_error(
                        "'defineProperty' on proxy: trap returned truish for defining non-configurable property which is configurable in the proxy target",
                    ));
                }
            }
        }
        Ok(true)
    }

    fn proxy_target_extensible(&mut self, t: ObjId) -> Res<bool> {
        self.is_extensible_p(t)
    }

    /// [[IsExtensible]].
    pub(crate) fn is_extensible_p(&mut self, o: ObjId) -> Res<bool> {
        if !matches!(self.o(o).kind, ObjKind::Proxy(_)) {
            return Ok(self.o(o).flags & EXTENSIBLE != 0);
        }
        let (t, h) = self.proxy_parts(o, "isExtensible")?;
        let Some(trap) = self.trap(h, "isExtensible")? else {
            return self.is_extensible_p(t);
        };
        let r = self.call_trap(trap, h, &[Val::Obj(t)])?;
        let b = self.truthy(r);
        if b != self.is_extensible_p(t)? {
            return Err(self.type_error(
                "'isExtensible' on proxy: trap result does not reflect extensibility of proxy target",
            ));
        }
        Ok(b)
    }

    /// [[PreventExtensions]]: false when the proxy refuses.
    pub(crate) fn prevent_extensions_p(&mut self, o: ObjId) -> Res<bool> {
        if !matches!(self.o(o).kind, ObjKind::Proxy(_)) {
            if matches!(self.o(o).kind, ObjKind::Array(_)) {
                self.array_to_sparse(o);
            }
            self.om(o).flags &= !EXTENSIBLE;
            return Ok(true);
        }
        let (t, h) = self.proxy_parts(o, "preventExtensions")?;
        let Some(trap) = self.trap(h, "preventExtensions")? else {
            return self.prevent_extensions_p(t);
        };
        let r = self.call_trap(trap, h, &[Val::Obj(t)])?;
        let b = self.truthy(r);
        if b && self.is_extensible_p(t)? {
            return Err(self.type_error(
                "'preventExtensions' on proxy: trap returned truish but the proxy target is extensible",
            ));
        }
        Ok(b)
    }

    /// [[GetPrototypeOf]].
    pub(crate) fn get_proto_p(&mut self, o: ObjId) -> Res<Option<ObjId>> {
        if !matches!(self.o(o).kind, ObjKind::Proxy(_)) {
            return Ok(self.o(o).proto);
        }
        let (t, h) = self.proxy_parts(o, "getPrototypeOf")?;
        let Some(trap) = self.trap(h, "getPrototypeOf")? else {
            return self.get_proto_p(t);
        };
        let r = self.call_trap(trap, h, &[Val::Obj(t)])?;
        let p = match r {
            Val::Obj(p) => Some(p),
            Val::Null => None,
            _ => {
                return Err(self.type_error(
                    "'getPrototypeOf' on proxy: trap returned neither object nor null",
                ));
            }
        };
        if !self.is_extensible_p(t)? && p != self.get_proto_p(t)? {
            return Err(self.type_error(
                "'getPrototypeOf' on proxy: proxy target is non-extensible but the trap did not return its actual prototype",
            ));
        }
        Ok(p)
    }

    /// [[SetPrototypeOf]]: false when it is refused.
    pub(crate) fn set_proto_p(&mut self, o: ObjId, p: Option<ObjId>) -> Res<bool> {
        if !matches!(self.o(o).kind, ObjKind::Proxy(_)) {
            return Ok(self.set_proto(o, p));
        }
        let (t, h) = self.proxy_parts(o, "setPrototypeOf")?;
        let Some(trap) = self.trap(h, "setPrototypeOf")? else {
            return self.set_proto_p(t, p);
        };
        let pv = p.map_or(Val::Null, Val::Obj);
        let r = self.call_trap(trap, h, &[Val::Obj(t), pv])?;
        if !self.truthy(r) {
            return Ok(false);
        }
        if !self.is_extensible_p(t)? && p != self.get_proto_p(t)? {
            return Err(self.type_error(
                "'setPrototypeOf' on proxy: trap returned truish for setting a new prototype on the non-extensible proxy target",
            ));
        }
        Ok(true)
    }

    pub(crate) fn proxy_call(&mut self, id: ObjId, this: Val, args: &[Val]) -> Res<Val> {
        let (t, h) = self.proxy_parts(id, "apply")?;
        let Some(trap) = self.trap(h, "apply")? else {
            return self.call_v(Val::Obj(t), this, args);
        };
        let arr = self.new_array_from(args.to_vec());
        self.call_trap(trap, h, &[Val::Obj(t), this, Val::Obj(arr)])
    }

    pub(crate) fn proxy_construct(&mut self, id: ObjId, args: &[Val], new_target: Val) -> Res<Val> {
        let (t, h) = self.proxy_parts(id, "construct")?;
        let Some(trap) = self.trap(h, "construct")? else {
            // (new.target is the proxy itself when it was constructed directly.)
            let nt = if matches!(new_target, Val::Obj(n) if n == id) {
                Val::Obj(t)
            } else {
                new_target
            };
            return self.construct_v(Val::Obj(t), args, nt);
        };
        let arr = self.new_array_from(args.to_vec());
        let r = self.call_trap(trap, h, &[Val::Obj(t), Val::Obj(arr), new_target])?;
        if !matches!(r, Val::Obj(_)) {
            return Err(self.type_error("proxy [[Construct]] must return an object"));
        }
        Ok(r)
    }

    /// IsArray: arrays, and proxies of arrays.
    pub(crate) fn is_array_val(&mut self, v: Val) -> Res<bool> {
        let mut cur = v;
        for _ in 0..1000 {
            let Val::Obj(o) = cur else { return Ok(false) };
            match &self.o(o).kind {
                ObjKind::Array(_) => return Ok(true),
                ObjKind::Proxy(p) => match p.target {
                    Some(t) => cur = Val::Obj(t),
                    None => {
                        return Err(self.type_error(
                            "Cannot perform 'IsArray' on a proxy that has been revoked",
                        ));
                    }
                },
                _ => return Ok(false),
            }
        }
        Err(self.range_error("Maximum call stack size exceeded"))
    }
}

// ---- the constructor ----

fn make_proxy(r: &mut Realm, target: Val, handler: Val) -> Res<ObjId> {
    let (Val::Obj(t), Val::Obj(h)) = (target, handler) else {
        return Err(r.type_error("Cannot create proxy with a non-object as target or handler"));
    };
    let data = ProxyData {
        target: Some(t),
        handler: Some(h),
        callable: r.callable(target),
        constructor: r.constructible(target),
    };
    // (A proxy has no prototype of its own; its traps answer.)
    Ok(r.alloc(None, ObjKind::Proxy(alloc::boxed::Box::new(data))))
}

fn proxy_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if r.cur_new_target().is_undef() {
        return Err(r.type_error("Constructor Proxy requires 'new'"));
    }
    let p = make_proxy(r, super::builtins::arg(args, 0), super::builtins::arg(args, 1))?;
    Ok(Val::Obj(p))
}

fn revoke_fn(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    let Some(f) = r.callee_obj() else {
        return Ok(Val::Undef);
    };
    if let Val::Obj(p) = r.native_slot(f, 0) {
        if let ObjKind::Proxy(d) = &mut r.om(p).kind {
            d.target = None;
            d.handler = None;
        }
        r.set_native_slot(f, 0, Val::Undef);
    }
    Ok(Val::Undef)
}

fn proxy_revocable(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let p = make_proxy(r, super::builtins::arg(args, 0), super::builtins::arg(args, 1))?;
    r.temps.push(Val::Obj(p));
    let e = Val::Str(r.nstr(N::empty));
    let revoke = r.new_builtin_slots(e, 0, revoke_fn, alloc::vec![Val::Obj(p)]);
    r.temps.push(Val::Obj(revoke));
    let out = r.new_plain();
    let k = r.key_str("proxy");
    r.put_prop(out, k, Val::Obj(p), crate::object::WEC);
    let k = r.key_str("revoke");
    r.put_prop(out, k, Val::Obj(revoke), crate::object::WEC);
    Ok(Val::Obj(out))
}

pub(crate) fn init(r: &mut Realm) {
    let nv = r.str_val("Proxy");
    let f = r.new_builtin(nv, 2, proxy_ctor);
    r.om(f).flags |= crate::object::CONSTRUCTOR;
    let g = r.global;
    r.value(g, "Proxy", Val::Obj(f), crate::object::WC);
    r.method(f, "revocable", 2, proxy_revocable);
}

