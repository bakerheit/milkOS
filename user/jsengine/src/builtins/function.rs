//! Function and Function.prototype (call, apply, bind, toString, @@hasInstance).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use crate::names::N;
use crate::object::{BoundData, C, LAZY_FN, ObjKind, PropKey, WC};
use crate::realm::{Realm, Res};
use crate::value::Val;

use super::arg;

/// Function.prototype[Symbol.hasInstance].
pub(crate) fn has_instance(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let v = arg(args, 0);
    Ok(Val::Bool(r.ordinary_has_instance(this, v)?))
}

fn function_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    make_dynamic(r, args, "function")
}

fn make_dynamic(r: &mut Realm, args: &[Val], kind: &str) -> Res<Val> {
    let mut params: Vec<String> = Vec::new();
    let n = args.len();
    for a in args.iter().take(n.saturating_sub(1)) {
        params.push(r.to_rust_string(*a)?);
    }
    let body = if n > 0 {
        r.to_rust_string(args[n - 1])?
    } else {
        String::new()
    };
    let refs: Vec<&str> = params.iter().map(|s| s.as_str()).collect();
    r.make_function_kind(&refs, &body, "anonymous", kind)
}

fn generator_function_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    make_dynamic(r, args, "function*")
}

fn async_function_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    make_dynamic(r, args, "async function")
}

fn call(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if !r.callable(this) {
        return Err(r.type_error("Function.prototype.call called on a non-function"));
    }
    let t = arg(args, 0);
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    r.call_v(this, t, rest)
}

fn apply(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    if !r.callable(this) {
        return Err(r.type_error("Function.prototype.apply was called on a non-function"));
    }
    let t = arg(args, 0);
    let list = r.array_like_to_vec(arg(args, 1))?;
    r.call_v(this, t, &list)
}

fn bind(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let Val::Obj(target) = this else {
        return Err(r.type_error("Bind must be called on a function"));
    };
    if !r.callable(this) {
        return Err(r.type_error("Bind must be called on a function"));
    }
    let bthis = arg(args, 0);
    let bargs: Vec<Val> = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        Vec::new()
    };
    let lk = r.key(N::length);
    let len = if r.has_own_p(target, lk)? {
        match r.get_p(target, lk, this)? {
            Val::Int(i) => (i as f64 - bargs.len() as f64).max(0.0),
            Val::Num(x) if x.is_finite() => {
                (crate::conv::to_integer_f(x) - bargs.len() as f64).max(0.0)
            }
            Val::Num(x) if x == f64::INFINITY => x,
            _ => 0.0,
        }
    } else {
        0.0
    };
    let nk = r.key(N::name);
    let name = match r.get_p(target, nk, this)? {
        Val::Str(s) => r.rust_string(s),
        _ => String::new(),
    };
    let proto = r.o(target).proto;
    let ctor = r.constructible(this);
    let b = r.alloc(
        proto,
        ObjKind::Bound(Box::new(BoundData {
            target,
            this: bthis,
            args: bargs,
        })),
    );
    if ctor {
        r.om(b).flags |= crate::object::CONSTRUCTOR;
    }
    r.put_prop(b, lk, Val::num(len), C);
    let nv = r.str_val(&alloc::format!("bound {name}"));
    r.put_prop(b, nk, nv, C);
    Ok(Val::Obj(b))
}

fn to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(f) = this else {
        return Err(r.type_error("Function.prototype.toString requires that 'this' be a Function"));
    };
    let s = match &r.o(f).kind {
        ObjKind::Closure(c) => {
            let (a, b) = c.code.span;
            let src = &c.code.source.text;
            let a = (a as usize).min(src.len());
            let b = (b as usize).clamp(a, src.len());
            String::from(
                core::str::from_utf8(&src[a..b]).unwrap_or("function () { [native code] }"),
            )
        }
        ObjKind::Native(_) | ObjKind::Bound(_) => {
            let n = r.fn_name(f);
            alloc::format!("function {n}() {{ [native code] }}")
        }
        _ => {
            return Err(
                r.type_error("Function.prototype.toString requires that 'this' be a Function")
            );
        }
    };
    Ok(r.str_val(&s))
}

fn thrower(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    Err(r.type_error("'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them"))
}

pub(crate) fn init(r: &mut Realm) {
    let fp = r.intr.function_proto;
    let c = r.ctor("Function", 1, function_ctor, fp);
    r.intr.function_ctor = c;
    r.method(fp, "call", 1, call);
    r.method(fp, "apply", 2, apply);
    r.method(fp, "bind", 1, bind);
    r.method(fp, "toString", 0, to_string);
    let hi = r.sym.has_instance;
    let hf = r.method_sym(fp, hi, "[Symbol.hasInstance]", 1, has_instance);
    // Non-writable, non-configurable.
    r.put_prop(fp, PropKey::Sym(hi), Val::Obj(hf), 0);
    let e = Val::Str(r.nstr(N::empty));
    let tt = r.new_builtin(e, 0, thrower);
    r.intr.throw_type_error = tt;
    // Generator / async function intrinsics.
    let gfp = r.intr.generator_function_proto;
    let gp = r.intr.generator_proto;
    let pk = r.key(N::prototype);
    r.put_prop(gfp, pk, Val::Obj(gp), C);
    let ck = r.key(N::constructor);
    r.put_prop(gp, ck, Val::Obj(gfp), C);
    r.to_string_tag(gfp, "GeneratorFunction");
    let agfp = r.intr.async_generator_function_proto;
    let agp = r.intr.async_generator_proto;
    r.put_prop(agfp, pk, Val::Obj(agp), C);
    r.put_prop(agp, ck, Val::Obj(agfp), C);
    r.to_string_tag(agfp, "AsyncGeneratorFunction");
    let afp = r.intr.async_function_proto;
    r.to_string_tag(afp, "AsyncFunction");
    // Their constructors (not globals).
    let gn = r.str_val("GeneratorFunction");
    let gfc = r.new_builtin(gn, 1, generator_function_ctor);
    r.om(gfc).flags |= crate::object::CONSTRUCTOR;
    r.om(gfc).proto = Some(c);
    r.put_prop(gfc, pk, Val::Obj(gfp), 0);
    r.put_prop(gfp, ck, Val::Obj(gfc), C);
    r.intr.generator_function_ctor = gfc;
    let an = r.str_val("AsyncFunction");
    let afc = r.new_builtin(an, 1, async_function_ctor);
    r.om(afc).flags |= crate::object::CONSTRUCTOR;
    r.om(afc).proto = Some(c);
    r.put_prop(afc, pk, Val::Obj(afp), 0);
    r.put_prop(afp, ck, Val::Obj(afc), C);
    r.intr.async_function_ctor = afc;
    let _ = (LAZY_FN, WC);
}
