//! WeakRef and FinalizationRegistry.
//!
//! Both are accepted and behave as the spec allows, but nothing is ever reclaimed: a WeakRef
//! holds its target for good (`deref()` always returns it), and a FinalizationRegistry never
//! calls its callback (the spec promises no call at all, "there is no guarantee the callback
//! will be called"). Pages use them for caches that must not keep things alive; here the
//! heap budget bounds them like everything else.

use crate::object::{ObjKind, PropKey, W};
use crate::realm::{Realm, Res};
use crate::value::Val;

use super::arg;

/// Can `v` be a WeakRef target or a registry token? Objects, and symbols that are not
/// registered with `Symbol.for`.
fn can_be_held_weakly(r: &Realm, v: Val) -> bool {
    match v {
        Val::Obj(_) => true,
        Val::Sym(s) => !r.sym_registry.iter().any(|(_, x)| *x == s),
        _ => false,
    }
}

fn weakref_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error("Constructor WeakRef requires 'new'"));
    }
    let target = arg(args, 0);
    if !can_be_held_weakly(r, target) {
        return Err(r.type_error("WeakRef: invalid target (an object or a non-registered symbol is needed)"));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.weakref_proto)?;
    let o = r.alloc(Some(proto), ObjKind::Ordinary);
    let k = PropKey::Sym(r.sym.weakref_target);
    r.put_prop(o, k, target, W);
    Ok(Val::Obj(o))
}

fn weakref_deref(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = this {
        let k = PropKey::Sym(r.sym.weakref_target);
        if let Some(p) = r.o(o).props.get(k) {
            return Ok(p.v);
        }
    }
    Err(r.type_error("Method WeakRef.prototype.deref called on incompatible receiver"))
}

fn finreg_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error("Constructor FinalizationRegistry requires 'new'"));
    }
    if !r.callable(arg(args, 0)) {
        return Err(r.type_error("FinalizationRegistry: cleanup must be callable"));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.finreg_proto)?;
    let o = r.alloc(Some(proto), ObjKind::Ordinary);
    let toks = r.new_array_from(alloc::vec::Vec::new());
    let k = PropKey::Sym(r.sym.finreg_tokens);
    r.put_prop(o, k, Val::Obj(toks), W);
    Ok(Val::Obj(o))
}

fn tokens_of(r: &mut Realm, this: Val, what: &str) -> Res<crate::value::ObjId> {
    if let Val::Obj(o) = this {
        let k = PropKey::Sym(r.sym.finreg_tokens);
        if let Some(p) = r.o(o).props.get(k)
            && let Val::Obj(t) = p.v
        {
            return Ok(t);
        }
    }
    Err(r.type_error(&alloc::format!(
        "Method FinalizationRegistry.prototype.{what} called on incompatible receiver"
    )))
}

/// The most registrations with an unregister token one registry keeps (they are never
/// cleaned up, so a page that registers in a loop must not grow without bound).
const MAX_TOKENS: f64 = 100_000.0;

fn finreg_register(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let toks = tokens_of(r, this, "register")?;
    let (target, held, token) = (arg(args, 0), arg(args, 1), arg(args, 2));
    if !can_be_held_weakly(r, target) {
        return Err(r.type_error("FinalizationRegistry.prototype.register: invalid target"));
    }
    if r.strict_eq(target, held) {
        return Err(r.type_error(
            "FinalizationRegistry.prototype.register: target and holdings must not be same",
        ));
    }
    if !token.is_undef() && !can_be_held_weakly(r, token) {
        return Err(r.type_error("FinalizationRegistry.prototype.register: invalid unregister token"));
    }
    if !token.is_undef() && (r.array_len(toks) as f64) < MAX_TOKENS {
        let n = r.array_len(toks);
        r.array_put_dense(toks, n, token);
    }
    Ok(Val::Undef)
}

fn finreg_unregister(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let toks = tokens_of(r, this, "unregister")?;
    let token = arg(args, 0);
    if !can_be_held_weakly(r, token) {
        return Err(r.type_error("Invalid unregisterToken"));
    }
    let n = r.length_of(Val::Obj(toks))? as u32;
    let mut found = false;
    let mut keep = alloc::vec::Vec::new();
    for i in 0..n {
        let t = r.get_elem(Val::Obj(toks), Val::Int(i as i32))?;
        if r.strict_eq(t, token) {
            found = true;
        } else {
            keep.push(t);
        }
    }
    if found {
        let fresh = r.new_array_from(keep);
        let k = PropKey::Sym(r.sym.finreg_tokens);
        if let Val::Obj(o) = this {
            r.put_prop(o, k, Val::Obj(fresh), W);
        }
    }
    Ok(Val::Bool(found))
}

pub(crate) fn init(r: &mut Realm) {
    let p = r.intr.weakref_proto;
    r.ctor("WeakRef", 1, weakref_ctor, p);
    r.method(p, "deref", 0, weakref_deref);
    r.to_string_tag(p, "WeakRef");
    let p = r.intr.finreg_proto;
    r.ctor("FinalizationRegistry", 1, finreg_ctor, p);
    r.method(p, "register", 2, finreg_register);
    r.method(p, "unregister", 1, finreg_unregister);
    r.to_string_tag(p, "FinalizationRegistry");
}
