//! Error, its native subclasses, AggregateError, Error.prototype.toString.

use crate::api::ErrorKind;
use crate::names::N;
use crate::object::{ObjKind, WC};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

use super::arg;

const KINDS: [ErrorKind; 7] = [
    ErrorKind::Error,
    ErrorKind::TypeError,
    ErrorKind::RangeError,
    ErrorKind::SyntaxError,
    ErrorKind::ReferenceError,
    ErrorKind::URIError,
    ErrorKind::EvalError,
];

fn make(r: &mut Realm, default_proto: ObjId, name: &str, msg: Val, options: Val) -> Res<ObjId> {
    let nt = r.cur_new_target();
    let proto = if nt.is_undef() {
        default_proto
    } else {
        r.proto_from_ctor(nt, |_| default_proto)?
    };
    let m = if msg.is_undef() {
        None
    } else {
        Some(Val::Str(r.to_str(msg)?))
    };
    let e = r.make_error_with_proto(proto, name, m);
    if let Val::Obj(o) = options {
        let ck = r.key(N::cause);
        if r.has_property(o, ck)? {
            let c = r.get_p(o, ck, options)?;
            r.put_prop(e, ck, c, WC);
        }
    }
    // The stack header uses the subclass's name when there is one.
    if !nt.is_undef()
        && let Some(n) = r.peek_string(e, N::name)
        && n != name
    {
        let sk = r.key(N::stack);
        let s = match r.peek_prop(e, sk) {
            Some(Val::Str(s)) => r.rust_string(s),
            _ => alloc::string::String::new(),
        };
        if let Some(rest) = s.strip_prefix(name) {
            let ns = alloc::format!("{n}{rest}");
            let v = r.str_val(&ns);
            r.put_prop(e, sk, v, WC);
        }
    }
    Ok(e)
}

fn error_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    let kind = match r.native_slot(f, 0) {
        Val::Int(i) => KINDS.get(i as usize).copied().unwrap_or(ErrorKind::Error),
        _ => ErrorKind::Error,
    };
    let proto = r.error_proto_for(kind);
    let e = make(r, proto, kind.name(), arg(args, 0), arg(args, 1))?;
    Ok(Val::Obj(e))
}

fn aggregate_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let proto = r.intr.aggregate_error_proto;
    let e = make(r, proto, "AggregateError", arg(args, 1), arg(args, 2))?;
    let mark = r.temps.len();
    r.temps.push(Val::Obj(e));
    let items = r.iterate_to_vec(arg(args, 0));
    r.temps.truncate(mark);
    let a = r.new_array_from(items?);
    let ek = r.key(N::errors);
    r.put_prop(e, ek, Val::Obj(a), WC);
    Ok(Val::Obj(e))
}

fn to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(o) = this else {
        return Err(r.type_error("Error.prototype.toString called on non-object"));
    };
    let nk = r.key(N::name);
    let n = r.get_p(o, nk, this)?;
    let n = if n.is_undef() {
        alloc::string::String::from("Error")
    } else {
        r.to_rust_string(n)?
    };
    let mk = r.key(N::message);
    let m = r.get_p(o, mk, this)?;
    let m = if m.is_undef() {
        alloc::string::String::new()
    } else {
        r.to_rust_string(m)?
    };
    let s = if n.is_empty() {
        m
    } else if m.is_empty() {
        n
    } else {
        alloc::format!("{n}: {m}")
    };
    Ok(r.str_val(&s))
}

/// Error.captureStackTrace(obj): sets obj.stack (a V8 extension some libraries call).
fn capture_stack_trace(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if let Val::Obj(o) = arg(args, 0) {
        let site = r.current_site();
        let head = r.builtin_tag(Val::Obj(o)).unwrap_or_default();
        let head = match r.peek_string(o, N::name) {
            Some(n) => n,
            None => head,
        };
        let s = if site.stack.is_empty() {
            head
        } else {
            alloc::format!("{head}\n{}", site.stack)
        };
        let sk = r.key(N::stack);
        let v = r.str_val(&s);
        r.put_prop(o, sk, v, WC);
    }
    Ok(Val::Undef)
}

pub(crate) fn init(r: &mut Realm) {
    let ep = r.intr.error_proto;
    let mut base_ctor = ep;
    for (i, kind) in KINDS.iter().enumerate() {
        let proto = if i == 0 {
            ep
        } else {
            let p = r.alloc(Some(ep), ObjKind::Ordinary);
            match kind {
                ErrorKind::TypeError => r.intr.type_error_proto = p,
                ErrorKind::RangeError => r.intr.range_error_proto = p,
                ErrorKind::SyntaxError => r.intr.syntax_error_proto = p,
                ErrorKind::ReferenceError => r.intr.reference_error_proto = p,
                ErrorKind::URIError => r.intr.uri_error_proto = p,
                _ => r.intr.eval_error_proto = p,
            }
            p
        };
        let c = r.ctor(kind.name(), 1, error_ctor, proto);
        if let ObjKind::Native(n) = &mut r.om(c).kind {
            n.slots = alloc::vec![Val::Int(i as i32)];
        }
        if i == 0 {
            r.intr.error_ctor = c;
            base_ctor = c;
            r.method(c, "captureStackTrace", 2, capture_stack_trace);
            let sl = r.key_str("stackTraceLimit");
            r.put_prop(c, sl, Val::Int(10), crate::object::WEC);
        } else {
            r.om(c).proto = Some(base_ctor);
        }
        let nv = r.str_val(kind.name());
        r.value(proto, "name", nv, WC);
        let ev = Val::Str(r.nstr(N::empty));
        r.value(proto, "message", ev, WC);
    }
    r.method(ep, "toString", 0, to_string);
    let ap = r.alloc(Some(ep), ObjKind::Ordinary);
    r.intr.aggregate_error_proto = ap;
    let ac = r.ctor("AggregateError", 2, aggregate_ctor, ap);
    r.om(ac).proto = Some(base_ctor);
    let nv = r.str_val("AggregateError");
    r.value(ap, "name", nv, WC);
    let ev = Val::Str(r.nstr(N::empty));
    r.value(ap, "message", ev, WC);
}
