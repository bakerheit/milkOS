//! The Math object.

use crate::libm;
use crate::math as m;
use crate::object::ObjKind;
use crate::realm::{Realm, Res};
use crate::value::Val;

use super::arg;

impl Realm {
    /// xorshift128+ in [0, 1).
    pub(crate) fn random(&mut self) -> f64 {
        let mut s1 = self.rng[0];
        let s0 = self.rng[1];
        self.rng[0] = s0;
        s1 ^= s1 << 23;
        s1 ^= s1 >> 17;
        s1 ^= s0;
        s1 ^= s0 >> 26;
        self.rng[1] = s1;
        let v = self.rng[0].wrapping_add(self.rng[1]);
        (v >> 11) as f64 / 9007199254740992.0
    }
}

fn num1(r: &mut Realm, args: &[Val]) -> Res<f64> {
    match arg(args, 0) {
        Val::Int(i) => Ok(i as f64),
        Val::Num(n) => Ok(n),
        v => r.to_num(v),
    }
}

macro_rules! unary {
    ($name:ident, $f:expr) => {
        fn $name(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
            let x = num1(r, args)?;
            let f: fn(f64) -> f64 = $f;
            Ok(Val::num(f(x)))
        }
    };
}

unary!(abs, m::abs);
unary!(acos, libm::acos);
unary!(acosh, libm::acosh);
unary!(asin, libm::asin);
unary!(asinh, libm::asinh);
unary!(atan, libm::atan);
unary!(atanh, libm::atanh);
unary!(cbrt, libm::cbrt);
unary!(ceil, m::ceil);
unary!(cos, libm::cos);
unary!(cosh, libm::cosh);
unary!(exp, libm::exp);
unary!(expm1, libm::expm1);
unary!(floor, m::floor);
unary!(fround, |x| x as f32 as f64);
unary!(log, libm::log);
unary!(log1p, libm::log1p);
unary!(log10, libm::log10);
unary!(log2, libm::log2);
unary!(round, m::js_round);
unary!(sign, |x| if x.is_nan() {
    f64::NAN
} else if x > 0.0 {
    1.0
} else if x < 0.0 {
    -1.0
} else {
    x
});
unary!(sin, libm::sin);
unary!(sinh, libm::sinh);
unary!(sqrt, m::sqrt);
unary!(tan, libm::tan);
unary!(tanh, libm::tanh);
unary!(trunc, m::trunc);

fn atan2(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let y = r.to_num(arg(args, 0))?;
    let x = r.to_num(arg(args, 1))?;
    Ok(Val::num(libm::atan2(y, x)))
}

fn pow(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let x = r.to_num(arg(args, 0))?;
    let y = r.to_num(arg(args, 1))?;
    Ok(Val::num(libm::js_pow(x, y)))
}

fn clz32(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let x = r.to_uint32(arg(args, 0))?;
    Ok(Val::Int(x.leading_zeros() as i32))
}

fn imul(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let a = r.to_i32(arg(args, 0))?;
    let b = r.to_i32(arg(args, 1))?;
    Ok(Val::Int(a.wrapping_mul(b)))
}

fn max(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    // Fast path: all ints.
    if !args.is_empty() && args.iter().all(|a| matches!(a, Val::Int(_))) {
        let mut best = i32::MIN;
        for a in args {
            if let Val::Int(i) = a {
                best = best.max(*i);
            }
        }
        return Ok(Val::Int(best));
    }
    let mut best = f64::NEG_INFINITY;
    let mut nan = false;
    for a in args {
        let x = r.to_num(*a)?;
        if x.is_nan() {
            nan = true;
        } else if x > best || (x == 0.0 && best == 0.0 && !x.is_sign_negative()) {
            best = x;
        }
    }
    Ok(Val::num(if nan { f64::NAN } else { best }))
}

fn min(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if !args.is_empty() && args.iter().all(|a| matches!(a, Val::Int(_))) {
        let mut best = i32::MAX;
        for a in args {
            if let Val::Int(i) = a {
                best = best.min(*i);
            }
        }
        return Ok(Val::Int(best));
    }
    let mut best = f64::INFINITY;
    let mut nan = false;
    for a in args {
        let x = r.to_num(*a)?;
        if x.is_nan() {
            nan = true;
        } else if x < best || (x == 0.0 && best == 0.0 && x.is_sign_negative()) {
            best = x;
        }
    }
    Ok(Val::num(if nan { f64::NAN } else { best }))
}

fn hypot(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let mut v = alloc::vec::Vec::with_capacity(args.len());
    for a in args {
        v.push(r.to_num(*a)?);
    }
    Ok(Val::num(libm::hypot(&v)))
}

fn random(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::Num(r.random()))
}

pub(crate) fn init(r: &mut Realm) {
    let op = r.intr.object_proto;
    let mo = r.alloc(Some(op), ObjKind::Ordinary);
    let g = r.global;
    r.value(g, "Math", Val::Obj(mo), crate::object::WC);
    for (n, v) in [
        ("E", core::f64::consts::E),
        ("LN10", core::f64::consts::LN_10),
        ("LN2", core::f64::consts::LN_2),
        ("LOG10E", core::f64::consts::LOG10_E),
        ("LOG2E", core::f64::consts::LOG2_E),
        ("PI", core::f64::consts::PI),
        ("SQRT1_2", core::f64::consts::FRAC_1_SQRT_2),
        ("SQRT2", core::f64::consts::SQRT_2),
    ] {
        r.value(mo, n, Val::Num(v), 0);
    }
    for (n, a, f) in [
        ("abs", 1, abs as crate::object::BuiltinFn),
        ("acos", 1, acos),
        ("acosh", 1, acosh),
        ("asin", 1, asin),
        ("asinh", 1, asinh),
        ("atan", 1, atan),
        ("atanh", 1, atanh),
        ("atan2", 2, atan2),
        ("cbrt", 1, cbrt),
        ("ceil", 1, ceil),
        ("clz32", 1, clz32),
        ("cos", 1, cos),
        ("cosh", 1, cosh),
        ("exp", 1, exp),
        ("expm1", 1, expm1),
        ("floor", 1, floor),
        ("fround", 1, fround),
        ("hypot", 2, hypot),
        ("imul", 2, imul),
        ("log", 1, log),
        ("log1p", 1, log1p),
        ("log10", 1, log10),
        ("log2", 1, log2),
        ("max", 2, max),
        ("min", 2, min),
        ("pow", 2, pow),
        ("random", 0, random),
        ("round", 1, round),
        ("sign", 1, sign),
        ("sin", 1, sin),
        ("sinh", 1, sinh),
        ("sqrt", 1, sqrt),
        ("tan", 1, tan),
        ("tanh", 1, tanh),
        ("trunc", 1, trunc),
    ] {
        r.method(mo, n, a, f);
    }
    r.to_string_tag(mo, "Math");
}
