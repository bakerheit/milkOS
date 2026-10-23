//! Number, Boolean and Symbol.

use alloc::string::String;

use crate::names::N;
use crate::object::{C, ObjKind, PropKey, WC};
use crate::realm::{Realm, Res};
use crate::value::Val;

use super::arg;

impl Realm {
    fn this_number(&mut self, this: Val, name: &str) -> Res<f64> {
        match this {
            Val::Int(i) => Ok(i as f64),
            Val::Num(n) => Ok(n),
            Val::Obj(o) => match self.o(o).kind {
                ObjKind::NumBox(n) => Ok(n),
                _ => Err(self.type_error(&alloc::format!(
                    "Number.prototype.{name} requires that 'this' be a Number"
                ))),
            },
            _ => Err(self.type_error(&alloc::format!(
                "Number.prototype.{name} requires that 'this' be a Number"
            ))),
        }
    }

    pub(crate) fn symbol_descriptive(&mut self, v: Val) -> String {
        match v {
            Val::Sym(s) => match self.heap.syms.get(s.0 as usize).and_then(|d| d.desc) {
                Some(d) => {
                    let ds = self.rust_string(d);
                    alloc::format!("Symbol({ds})")
                }
                None => String::from("Symbol()"),
            },
            _ => String::new(),
        }
    }
}

/// en-US style grouping: "1,234,567.891" (up to 3 fraction digits, like the default
/// Intl.NumberFormat).
pub(crate) fn locale_number(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            String::from("NaN")
        } else if x > 0.0 {
            String::from("∞")
        } else {
            String::from("-∞")
        };
    }
    let s = crate::numconv::to_fixed(x.abs(), 3);
    let (int, frac) = match s.split_once('.') {
        Some((a, b)) => (String::from(a), String::from(b.trim_end_matches('0'))),
        None => (s, String::new()),
    };
    let mut grouped = String::new();
    let n = int.len();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (n - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let mut out = String::new();
    if x < 0.0 && (grouped != "0" || !frac.is_empty()) {
        out.push('-');
    }
    out.push_str(&grouped);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(&frac);
    }
    out
}

fn number_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let n = if args.is_empty() {
        0.0
    } else {
        r.to_num(args[0])?
    };
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Ok(Val::num(n));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.number_proto)?;
    Ok(Val::Obj(r.alloc(Some(proto), ObjKind::NumBox(n))))
}

fn is_finite(_r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(
        arg(args, 0).as_f64().is_some_and(|x| x.is_finite()),
    ))
}

fn is_integer(_r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(arg(args, 0).as_f64().is_some_and(|x| {
        x.is_finite() && crate::math::trunc(x) == x
    })))
}

fn is_safe_integer(_r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(arg(args, 0).as_f64().is_some_and(|x| {
        x.is_finite() && crate::math::trunc(x) == x && x.abs() <= 9007199254740991.0
    })))
}

fn is_nan(_r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(arg(args, 0).as_f64().is_some_and(|x| x.is_nan())))
}

fn to_string(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let x = r.this_number(this, "toString")?;
    let radix = arg(args, 0);
    let rad = if radix.is_undef() {
        10.0
    } else {
        r.to_integer(radix)?
    };
    if !(2.0..=36.0).contains(&rad) {
        return Err(r.range_error("toString() radix must be between 2 and 36"));
    }
    if rad == 10.0 {
        if let Val::Int(i) = this {
            return r.to_str(Val::Int(i)).map(Val::Str);
        }
        let s = crate::numconv::number_to_string(x);
        return Ok(r.str_val(&s));
    }
    let s = crate::numconv::number_to_radix(x, rad as u32);
    Ok(r.str_val(&s))
}

fn to_locale_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let x = r.this_number(this, "toLocaleString")?;
    let s = locale_number(x);
    Ok(r.str_val(&s))
}

fn value_of(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let x = r.this_number(this, "valueOf")?;
    Ok(Val::num(x))
}

fn to_fixed(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let x = r.this_number(this, "toFixed")?;
    let f = r.to_integer(arg(args, 0))?;
    if !(0.0..=100.0).contains(&f) {
        return Err(r.range_error("toFixed() digits argument must be between 0 and 100"));
    }
    if !x.is_finite() || x.abs() >= 1e21 {
        let s = crate::numconv::number_to_string(x);
        return Ok(r.str_val(&s));
    }
    let s = crate::numconv::to_fixed(x, f as usize);
    Ok(r.str_val(&s))
}

fn to_exponential(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let x = r.this_number(this, "toExponential")?;
    let fv = arg(args, 0);
    let f = r.to_integer(fv)?;
    if !x.is_finite() {
        let s = crate::numconv::number_to_string(x);
        return Ok(r.str_val(&s));
    }
    if !(0.0..=100.0).contains(&f) {
        return Err(r.range_error("toExponential() argument must be between 0 and 100"));
    }
    let s = crate::numconv::to_exponential(
        x,
        if fv.is_undef() {
            None
        } else {
            Some(f as usize)
        },
    );
    Ok(r.str_val(&s))
}

fn to_precision(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let x = r.this_number(this, "toPrecision")?;
    let pv = arg(args, 0);
    if pv.is_undef() || !x.is_finite() {
        let s = crate::numconv::number_to_string(x);
        return Ok(r.str_val(&s));
    }
    let p = r.to_integer(pv)?;
    if !(1.0..=100.0).contains(&p) {
        return Err(r.range_error("toPrecision() argument must be between 1 and 100"));
    }
    let s = crate::numconv::to_precision(x, p as usize);
    Ok(r.str_val(&s))
}

// ---- Boolean ----

fn boolean_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let b = r.truthy(arg(args, 0));
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Ok(Val::Bool(b));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.boolean_proto)?;
    Ok(Val::Obj(r.alloc(Some(proto), ObjKind::BoolBox(b))))
}

fn this_bool(r: &mut Realm, this: Val) -> Res<bool> {
    match this {
        Val::Bool(b) => Ok(b),
        Val::Obj(o) => match r.o(o).kind {
            ObjKind::BoolBox(b) => Ok(b),
            _ => Err(r.type_error("Boolean.prototype method requires that 'this' be a Boolean")),
        },
        _ => Err(r.type_error("Boolean.prototype method requires that 'this' be a Boolean")),
    }
}

fn bool_to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let b = this_bool(r, this)?;
    Ok(Val::Str(r.nstr(if b { N::true_ } else { N::false_ })))
}

fn bool_value_of(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::Bool(this_bool(r, this)?))
}

// ---- Symbol ----

fn symbol_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    if !r.cur_new_target().is_undef() {
        return Err(r.type_error("Symbol is not a constructor"));
    }
    let d = arg(args, 0);
    let desc = if d.is_undef() {
        None
    } else {
        Some(r.to_str(d)?)
    };
    Ok(Val::Sym(r.heap.new_sym(desc)))
}

fn symbol_for(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let s = r.to_str(arg(args, 0))?;
    for i in 0..r.sym_registry.len() {
        let (k, sym) = r.sym_registry[i];
        if r.heap.str_eq(k, s) {
            return Ok(Val::Sym(sym));
        }
    }
    let sym = r.heap.new_sym(Some(s));
    if let Some(d) = r.heap.syms.get_mut(sym.0 as usize) {
        d.registered = true;
    }
    r.sym_registry.push((s, sym));
    Ok(Val::Sym(sym))
}

fn symbol_key_for(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let Val::Sym(s) = arg(args, 0) else {
        let d = r.short_display(arg(args, 0));
        return Err(r.type_error(&alloc::format!("{d} is not a symbol")));
    };
    for (k, sym) in &r.sym_registry {
        if *sym == s {
            return Ok(Val::Str(*k));
        }
    }
    Ok(Val::Undef)
}

fn this_sym(r: &mut Realm, this: Val) -> Res<Val> {
    match this {
        Val::Sym(_) => Ok(this),
        Val::Obj(o) => match r.o(o).kind {
            ObjKind::SymBox(s) => Ok(Val::Sym(s)),
            _ => Err(r.type_error("Symbol.prototype method requires that 'this' be a Symbol")),
        },
        _ => Err(r.type_error("Symbol.prototype method requires that 'this' be a Symbol")),
    }
}

fn sym_to_string(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let s = this_sym(r, this)?;
    let d = r.symbol_descriptive(s);
    Ok(r.str_val(&d))
}

fn sym_value_of(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    this_sym(r, this)
}

fn sym_description(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Sym(s) = this_sym(r, this)? else {
        return Ok(Val::Undef);
    };
    Ok(r.heap
        .syms
        .get(s.0 as usize)
        .and_then(|d| d.desc)
        .map_or(Val::Undef, Val::Str))
}

pub(crate) fn init(r: &mut Realm) {
    let np = r.intr.number_proto;
    let c = r.ctor("Number", 1, number_ctor, np);
    r.method(c, "isFinite", 1, is_finite);
    r.method(c, "isInteger", 1, is_integer);
    r.method(c, "isSafeInteger", 1, is_safe_integer);
    r.method(c, "isNaN", 1, is_nan);
    for (n, v) in [
        ("EPSILON", f64::EPSILON),
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("NaN", f64::NAN),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("POSITIVE_INFINITY", f64::INFINITY),
    ] {
        r.value(c, n, Val::num(v), 0);
    }
    r.method(np, "toString", 1, to_string);
    r.method(np, "toLocaleString", 0, to_locale_string);
    r.method(np, "valueOf", 0, value_of);
    r.method(np, "toFixed", 1, to_fixed);
    r.method(np, "toExponential", 1, to_exponential);
    r.method(np, "toPrecision", 1, to_precision);

    let bp = r.intr.boolean_proto;
    r.ctor("Boolean", 1, boolean_ctor, bp);
    r.method(bp, "toString", 0, bool_to_string);
    r.method(bp, "valueOf", 0, bool_value_of);

    let syp = r.intr.symbol_proto;
    let sc = r.ctor("Symbol", 0, symbol_ctor, syp);
    r.om(sc).flags &= !crate::object::CONSTRUCTOR;
    r.method(sc, "for", 1, symbol_for);
    r.method(sc, "keyFor", 1, symbol_key_for);
    let wk = [
        ("iterator", r.sym.iterator),
        ("asyncIterator", r.sym.async_iterator),
        ("hasInstance", r.sym.has_instance),
        ("toPrimitive", r.sym.to_primitive),
        ("toStringTag", r.sym.to_string_tag),
        ("species", r.sym.species),
        ("isConcatSpreadable", r.sym.is_concat_spreadable),
        ("unscopables", r.sym.unscopables),
        ("match", r.sym.match_),
        ("matchAll", r.sym.match_all),
        ("replace", r.sym.replace),
        ("search", r.sym.search),
        ("split", r.sym.split),
    ];
    for (n, s) in wk {
        r.value(sc, n, Val::Sym(s), 0);
    }
    r.method(syp, "toString", 0, sym_to_string);
    r.method(syp, "valueOf", 0, sym_value_of);
    r.getter(syp, "description", sym_description, None);
    let tp = r.sym.to_primitive;
    let f = r.method_sym(syp, tp, "[Symbol.toPrimitive]", 1, sym_value_of);
    r.put_prop(syp, PropKey::Sym(tp), Val::Obj(f), C);
    r.to_string_tag(syp, "Symbol");
    let _ = WC;
}
