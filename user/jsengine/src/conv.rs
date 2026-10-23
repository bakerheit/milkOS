//! Type conversions and comparisons: ToPrimitive, ToNumber, ToString, ToPropertyKey,
//! ToObject, ToInt32/ToUint32, ToBoolean, typeof, ===, ==, SameValue(Zero), instanceof.

use alloc::string::String;
use alloc::vec::Vec;

use crate::names::N;
use crate::object::{ObjKind, PropKey};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, StrId, Val};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hint {
    Default,
    Number,
    String,
}

impl Realm {
    pub(crate) fn truthy(&self, v: Val) -> bool {
        match v {
            Val::Undef | Val::Null | Val::Empty => false,
            Val::Bool(b) => b,
            Val::Int(i) => i != 0,
            Val::Num(n) => !(n == 0.0 || n.is_nan()),
            Val::Str(s) => self.heap.str_len(s) != 0,
            Val::Sym(_) | Val::Obj(_) => true,
        }
    }

    pub(crate) fn typeof_str(&self, v: Val) -> &'static str {
        match v {
            Val::Undef | Val::Empty => "undefined",
            Val::Null => "object",
            Val::Bool(_) => "boolean",
            Val::Int(_) | Val::Num(_) => "number",
            Val::Str(_) => "string",
            Val::Sym(_) => "symbol",
            Val::Obj(o) => {
                if self.callable(Val::Obj(o)) {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    pub(crate) fn typeof_name(&self, v: Val) -> N {
        match self.typeof_str(v) {
            "undefined" => N::undefined,
            "boolean" => N::boolean,
            "number" => N::number,
            "string" => N::string,
            "symbol" => N::symbol,
            "function" => N::function,
            _ => N::object,
        }
    }

    // ---- ToPrimitive ----

    pub(crate) fn to_primitive(&mut self, v: Val, hint: Hint) -> Res<Val> {
        let Val::Obj(o) = v else { return Ok(v) };
        // Fast paths for wrappers and dates with untouched prototypes are not needed for
        // correctness; go through the protocol.
        let tp = crate::object::PropKey::Sym(self.sym.to_primitive);
        let exotic = self.get_p(o, tp, v)?;
        if !exotic.is_nullish() {
            let h = match hint {
                Hint::Default => N::default,
                Hint::Number => N::number,
                Hint::String => N::string,
            };
            let hs = Val::Str(self.nstr(h));
            let r = self.call_v(exotic, v, &[hs])?;
            if let Val::Obj(_) = r {
                return Err(self.type_error("Cannot convert object to primitive value"));
            }
            return Ok(r);
        }
        let order = if hint == Hint::String {
            [N::toString, N::valueOf]
        } else {
            [N::valueOf, N::toString]
        };
        for nm in order {
            let k = self.key(nm);
            let f = self.get_p(o, k, v)?;
            if self.callable(f) {
                let r = self.call_v(f, v, &[])?;
                if !matches!(r, Val::Obj(_)) {
                    return Ok(r);
                }
            }
        }
        Err(self.type_error("Cannot convert object to primitive value"))
    }

    // ---- ToNumber ----

    pub(crate) fn to_num(&mut self, v: Val) -> Res<f64> {
        match v {
            Val::Int(i) => Ok(i as f64),
            Val::Num(n) => Ok(n),
            Val::Undef | Val::Empty => Ok(f64::NAN),
            Val::Null => Ok(0.0),
            Val::Bool(b) => Ok(if b { 1.0 } else { 0.0 }),
            Val::Str(s) => Ok(self.str_to_number(s)),
            Val::Sym(_) => Err(self.type_error("Cannot convert a Symbol value to a number")),
            Val::Obj(_) => {
                let p = self.to_primitive(v, Hint::Number)?;
                self.to_num(p)
            }
        }
    }

    pub(crate) fn str_to_number(&mut self, s: StrId) -> f64 {
        let u = self.heap.units(s);
        // Fast path: short decimal integers.
        if !u.is_empty() && u.len() <= 9 && u.iter().all(|&c| crate::string::is_ascii_digit(c)) {
            let mut n: u32 = 0;
            for &c in u {
                n = n * 10 + (c - b'0' as u16) as u32;
            }
            return n as f64;
        }
        crate::numconv::string_to_number(u)
    }

    pub(crate) fn to_i32(&mut self, v: Val) -> Res<i32> {
        if let Val::Int(i) = v {
            return Ok(i);
        }
        let n = self.to_num(v)?;
        Ok(f64_to_int32(n))
    }

    pub(crate) fn to_uint32(&mut self, v: Val) -> Res<u32> {
        if let Val::Int(i) = v {
            return Ok(i as u32);
        }
        let n = self.to_num(v)?;
        Ok(f64_to_int32(n) as u32)
    }

    /// ToIntegerOrInfinity.
    pub(crate) fn to_integer(&mut self, v: Val) -> Res<f64> {
        if let Val::Int(i) = v {
            return Ok(i as f64);
        }
        let n = self.to_num(v)?;
        Ok(to_integer_f(n))
    }

    /// ToLength.
    pub(crate) fn to_length(&mut self, v: Val) -> Res<f64> {
        let n = self.to_integer(v)?;
        Ok(n.clamp(0.0, 9007199254740991.0))
    }

    /// ToIndex-like clamp for relative positions (`slice(-2)`): resolves against len.
    pub(crate) fn rel_index(&mut self, v: Val, len: f64, default: f64) -> Res<f64> {
        if v.is_undef() {
            return Ok(default);
        }
        let n = self.to_integer(v)?;
        Ok(if n < 0.0 {
            (len + n).max(0.0)
        } else {
            n.min(len)
        })
    }

    // ---- ToString ----

    pub(crate) fn to_str(&mut self, v: Val) -> Res<StrId> {
        match v {
            Val::Str(s) => Ok(s),
            Val::Int(i) => {
                if (0..10).contains(&i) {
                    return Ok(self.char_str(b'0' as u16 + i as u16));
                }
                let s = crate::numconv::i32_to_string(i);
                Ok(self.heap.new_str_vec(crate::string::utf16(&s)))
            }
            Val::Num(n) => {
                let s = crate::numconv::number_to_string(n);
                Ok(self.heap.new_str_vec(crate::string::utf16(&s)))
            }
            Val::Undef | Val::Empty => Ok(self.nstr(N::undefined)),
            Val::Null => Ok(self.nstr(N::null)),
            Val::Bool(true) => Ok(self.nstr(N::true_)),
            Val::Bool(false) => Ok(self.nstr(N::false_)),
            Val::Sym(_) => Err(self.type_error("Cannot convert a Symbol value to a string")),
            Val::Obj(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                self.to_str(p)
            }
        }
    }

    pub(crate) fn to_rust_string(&mut self, v: Val) -> Res<String> {
        let s = self.to_str(v)?;
        Ok(self.rust_string(s))
    }

    pub(crate) fn to_units(&mut self, v: Val) -> Res<Vec<u16>> {
        let s = self.to_str(v)?;
        Ok(self.heap.units(s).to_vec())
    }

    // ---- ToPropertyKey ----

    pub(crate) fn to_key(&mut self, v: Val) -> Res<PropKey> {
        match v {
            Val::Int(i) if i >= 0 => Ok(PropKey::Idx(i as u32)),
            Val::Num(n) if (0.0..4294967295.0).contains(&n) && (n as u32) as f64 == n => {
                Ok(PropKey::Idx(n as u32))
            }
            Val::Str(s) => Ok(self.key_from_str(s)),
            Val::Sym(s) => Ok(PropKey::Sym(s)),
            Val::Obj(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                self.to_key(p)
            }
            _ => {
                let s = self.to_str(v)?;
                Ok(self.key_from_str(s))
            }
        }
    }

    // ---- ToObject ----

    pub(crate) fn to_object(&mut self, v: Val) -> Res<ObjId> {
        match v {
            Val::Obj(o) => Ok(o),
            Val::Undef | Val::Null | Val::Empty => {
                Err(self.type_error("Cannot convert undefined or null to object"))
            }
            Val::Bool(b) => {
                let p = self.intr.boolean_proto;
                Ok(self.alloc(Some(p), ObjKind::BoolBox(b)))
            }
            Val::Int(_) | Val::Num(_) => {
                let p = self.intr.number_proto;
                let n = v.as_f64().unwrap_or(0.0);
                Ok(self.alloc(Some(p), ObjKind::NumBox(n)))
            }
            Val::Str(s) => {
                let p = self.intr.string_proto;
                Ok(self.alloc(Some(p), ObjKind::StrBox(s)))
            }
            Val::Sym(s) => {
                let p = self.intr.symbol_proto;
                Ok(self.alloc(Some(p), ObjKind::SymBox(s)))
            }
        }
    }

    // ---- equality ----

    pub(crate) fn strict_eq(&mut self, a: Val, b: Val) -> bool {
        match (a, b) {
            (Val::Int(x), Val::Int(y)) => x == y,
            (Val::Str(x), Val::Str(y)) => self.heap.str_eq(x, y),
            (Val::Obj(x), Val::Obj(y)) => x == y,
            (Val::Undef | Val::Empty, Val::Undef | Val::Empty) | (Val::Null, Val::Null) => true,
            (Val::Bool(x), Val::Bool(y)) => x == y,
            (Val::Sym(x), Val::Sym(y)) => x == y,
            _ => match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            },
        }
    }

    pub(crate) fn same_val(&mut self, a: Val, b: Val) -> bool {
        match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => {
                if x.is_nan() && y.is_nan() {
                    return true;
                }
                x == y && x.is_sign_negative() == y.is_sign_negative()
            }
            _ => self.strict_eq(a, b),
        }
    }

    pub(crate) fn same_value_zero(&mut self, a: Val, b: Val) -> bool {
        match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y || (x.is_nan() && y.is_nan()),
            _ => self.strict_eq(a, b),
        }
    }

    /// `a == b`.
    pub(crate) fn loose_eq(&mut self, a: Val, b: Val) -> Res<bool> {
        let mut a = a;
        let mut b = b;
        for _ in 0..4 {
            match (a, b) {
                (Val::Undef | Val::Null | Val::Empty, Val::Undef | Val::Null | Val::Empty) => {
                    return Ok(true);
                }
                (Val::Undef | Val::Null | Val::Empty, _)
                | (_, Val::Undef | Val::Null | Val::Empty) => return Ok(false),
                (Val::Int(_) | Val::Num(_), Val::Int(_) | Val::Num(_))
                | (Val::Str(_), Val::Str(_))
                | (Val::Bool(_), Val::Bool(_))
                | (Val::Sym(_), Val::Sym(_))
                | (Val::Obj(_), Val::Obj(_)) => return Ok(self.strict_eq(a, b)),
                (Val::Int(_) | Val::Num(_), Val::Str(s)) => b = Val::num(self.str_to_number(s)),
                (Val::Str(s), Val::Int(_) | Val::Num(_)) => a = Val::num(self.str_to_number(s)),
                (Val::Bool(x), _) => a = Val::Int(x as i32),
                (_, Val::Bool(y)) => b = Val::Int(y as i32),
                (Val::Obj(_), _) => {
                    if let Val::Sym(_) = b {
                        // object == symbol: compare the primitive
                    }
                    a = self.to_primitive(a, Hint::Default)?;
                }
                (_, Val::Obj(_)) => b = self.to_primitive(b, Hint::Default)?,
                _ => return Ok(false),
            }
        }
        Ok(self.strict_eq(a, b))
    }

    // ---- instanceof ----

    pub(crate) fn instance_of_v(&mut self, v: Val, target: Val) -> Res<bool> {
        let Val::Obj(t) = target else {
            return Err(self.type_error("Right-hand side of 'instanceof' is not an object"));
        };
        let hi = crate::object::PropKey::Sym(self.sym.has_instance);
        let h = self.get_p(t, hi, target)?;
        if !h.is_nullish() && !self.is_default_has_instance(h) {
            let r = self.call_v(h, target, &[v])?;
            return Ok(self.truthy(r));
        }
        if !self.callable(target) {
            return Err(self.type_error("Right-hand side of 'instanceof' is not callable"));
        }
        self.ordinary_has_instance(target, v)
    }

    fn is_default_has_instance(&self, h: Val) -> bool {
        match h {
            Val::Obj(o) => match &self.o(o).kind {
                ObjKind::Native(n) => {
                    matches!(n.f, crate::object::NativeImpl::Builtin(f) if core::ptr::eq(f as *const (), crate::builtins::function::has_instance as *const ()))
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub(crate) fn ordinary_has_instance(&mut self, c: Val, v: Val) -> Res<bool> {
        if !self.callable(c) {
            return Ok(false);
        }
        let Val::Obj(co) = c else { return Ok(false) };
        if let ObjKind::Bound(b) = &self.o(co).kind {
            let t = b.target;
            return self.instance_of_v(v, Val::Obj(t));
        }
        let Val::Obj(mut o) = v else { return Ok(false) };
        let pk = self.key(N::prototype);
        let p = self.get_p(co, pk, c)?;
        let Val::Obj(p) = p else {
            return Err(self.type_error("Function has non-object prototype in instanceof check"));
        };
        let mut n = 0;
        loop {
            match self.o(o).proto {
                None => return Ok(false),
                Some(q) => {
                    if q == p {
                        return Ok(true);
                    }
                    o = q;
                }
            }
            n += 1;
            if n > 100_000 {
                return Ok(false);
            }
        }
    }

    /// A short description of a value for error messages ("undefined", "foo", "[object
    /// Object]", "function bar").
    pub(crate) fn short_display(&mut self, v: Val) -> String {
        match v {
            Val::Str(s) => {
                let mut r = self.rust_string(s);
                if r.len() > 40 {
                    let mut cut = 40;
                    while !r.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    r.truncate(cut);
                    r.push('…');
                }
                alloc::format!("\"{r}\"")
            }
            Val::Obj(o) => {
                if self.callable(v) {
                    let n = self.fn_name(o);
                    if n.is_empty() {
                        String::from("function")
                    } else {
                        n
                    }
                } else if let ObjKind::Array(_) = self.o(o).kind {
                    String::from("array")
                } else {
                    String::from("object")
                }
            }
            Val::Sym(_) => String::from("Symbol()"),
            _ => self.inspect_val(v),
        }
    }
}

pub(crate) fn f64_to_int32(n: f64) -> i32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    if (-2147483648.0..=2147483647.0).contains(&n) {
        return n as i32;
    }
    let t = crate::math::trunc(n);
    let m = crate::math::fmod(t, 4294967296.0);
    let m = if m < 0.0 { m + 4294967296.0 } else { m };
    (m as u64 as u32) as i32
}

pub(crate) fn to_integer_f(n: f64) -> f64 {
    if n.is_nan() {
        return 0.0;
    }
    if n.is_infinite() {
        return n;
    }
    let t = crate::math::trunc(n);
    if t == 0.0 { 0.0 } else { t }
}
