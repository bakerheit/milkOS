//! Values: the internal [`Val`] the VM works with, and the public [`Value`] wrapper the
//! embedder sees. Heap things (objects, strings) are handles into the realm's heap: an index
//! plus a generation, so a stale handle (one kept past its life by an embedder) is detected
//! instead of reaching whatever now lives in that slot.

/// A handle to an object in a realm's heap.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ObjId {
    pub(crate) idx: u32,
    pub(crate) g: u32,
}

/// A handle to a string in a realm's heap.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct StrId {
    pub(crate) idx: u32,
    pub(crate) g: u32,
}

/// A symbol (symbols live as long as the realm).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SymId(pub(crate) u32);

/// The VM's value. `Int` is a fast path for numbers that are integers in i32 range (never
/// -0); every operation treats `Int(n)` and `Num(n as f64)` alike. `Empty` never reaches
/// script code: it marks array holes, uninitialised `let`/`const` bindings (the TDZ) and
/// "no value" in internal slots.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Val {
    Undef,
    Null,
    Bool(bool),
    Int(i32),
    Num(f64),
    Str(StrId),
    Sym(SymId),
    Obj(ObjId),
    Empty,
}

impl Val {
    #[inline]
    pub fn num(x: f64) -> Val {
        // Keep integers in the Int fast path (but never -0).
        if (-2147483648.0..=2147483647.0).contains(&x) {
            let i = x as i32;
            if i as f64 == x && !(i == 0 && x.is_sign_negative()) {
                return Val::Int(i);
            }
        }
        Val::Num(x)
    }
    #[inline]
    pub fn is_undef(self) -> bool {
        matches!(self, Val::Undef)
    }
    #[inline]
    pub fn is_null(self) -> bool {
        matches!(self, Val::Null)
    }
    #[inline]
    pub fn is_nullish(self) -> bool {
        matches!(self, Val::Undef | Val::Null)
    }
    #[inline]
    pub fn is_empty(self) -> bool {
        matches!(self, Val::Empty)
    }
    #[inline]
    pub fn as_obj(self) -> Option<ObjId> {
        match self {
            Val::Obj(o) => Some(o),
            _ => None,
        }
    }
    #[inline]
    pub fn is_number(self) -> bool {
        matches!(self, Val::Int(_) | Val::Num(_))
    }
    #[inline]
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Val::Int(i) => Some(i as f64),
            Val::Num(n) => Some(n),
            _ => None,
        }
    }
    /// Empty (hole / no value) reads as undefined.
    #[inline]
    pub fn or_undef(self) -> Val {
        if let Val::Empty = self {
            Val::Undef
        } else {
            self
        }
    }
}

/// What kind of value a [`Value`] is (`typeof`, roughly; functions are `Object`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    Symbol,
    Object,
}

/// A JavaScript value, as the embedder sees it. Copy, cheap, and opaque: numbers and booleans
/// can be made and read directly; strings, symbols and objects go through the [`Realm`] that
/// made them.
///
/// A `Value` that refers to a string or an object stays valid while it is reachable from
/// script, pinned ([`Realm::pin`]), or a temporary of the current scope (see
/// [`Realm::release_temps`]). A stale one reads as `undefined` (and throws a TypeError when
/// used as an object); it never reaches another object.
///
/// [`Realm`]: crate::Realm
/// [`Realm::pin`]: crate::Realm::pin
/// [`Realm::release_temps`]: crate::Realm::release_temps
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub struct Value(pub(crate) Val);

impl Value {
    pub const UNDEFINED: Value = Value(Val::Undef);
    pub const NULL: Value = Value(Val::Null);
    pub const TRUE: Value = Value(Val::Bool(true));
    pub const FALSE: Value = Value(Val::Bool(false));

    pub fn number(x: f64) -> Value {
        Value(Val::num(x))
    }
    pub fn int(i: i32) -> Value {
        Value(Val::Int(i))
    }
    pub fn bool(b: bool) -> Value {
        Value(Val::Bool(b))
    }
    pub fn kind(&self) -> Kind {
        match self.0 {
            Val::Undef | Val::Empty => Kind::Undefined,
            Val::Null => Kind::Null,
            Val::Bool(_) => Kind::Boolean,
            Val::Int(_) | Val::Num(_) => Kind::Number,
            Val::Str(_) => Kind::String,
            Val::Sym(_) => Kind::Symbol,
            Val::Obj(_) => Kind::Object,
        }
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self.0, Val::Undef | Val::Empty)
    }
    pub fn is_null(&self) -> bool {
        matches!(self.0, Val::Null)
    }
    /// undefined or null.
    pub fn is_nullish(&self) -> bool {
        matches!(self.0, Val::Undef | Val::Null | Val::Empty)
    }
    pub fn is_object(&self) -> bool {
        matches!(self.0, Val::Obj(_))
    }
    pub fn is_string(&self) -> bool {
        matches!(self.0, Val::Str(_))
    }
    pub fn is_number(&self) -> bool {
        matches!(self.0, Val::Int(_) | Val::Num(_))
    }
    pub fn is_bool(&self) -> bool {
        matches!(self.0, Val::Bool(_))
    }
    pub fn is_symbol(&self) -> bool {
        matches!(self.0, Val::Sym(_))
    }
    /// The number, if this is a number (no conversion; see [`Realm::to_number`]).
    ///
    /// [`Realm::to_number`]: crate::Realm::to_number
    pub fn as_number(&self) -> Option<f64> {
        self.0.as_f64()
    }
    /// The number as an i32 if it is one exactly (no conversion).
    pub fn as_i32(&self) -> Option<i32> {
        match self.0 {
            Val::Int(i) => Some(i),
            _ => None,
        }
    }
    /// The boolean, if this is a boolean (no conversion; see [`Realm::to_boolean`]).
    ///
    /// [`Realm::to_boolean`]: crate::Realm::to_boolean
    pub fn as_bool(&self) -> Option<bool> {
        match self.0 {
            Val::Bool(b) => Some(b),
            _ => None,
        }
    }
    /// Same handle or same primitive (strings compare by handle, not by content: use
    /// [`Realm::strict_equals`] for `===`).
    ///
    /// [`Realm::strict_equals`]: crate::Realm::strict_equals
    pub fn is_same_handle(&self, other: &Value) -> bool {
        match (self.0, other.0) {
            (Val::Obj(a), Val::Obj(b)) => a == b,
            (Val::Str(a), Val::Str(b)) => a == b,
            (Val::Sym(a), Val::Sym(b)) => a == b,
            (Val::Undef, Val::Undef) | (Val::Null, Val::Null) => true,
            (Val::Bool(a), Val::Bool(b)) => a == b,
            (a, b) => match (a.as_f64(), b.as_f64()) {
                (Some(x), Some(y)) => x.to_bits() == y.to_bits(),
                _ => false,
            },
        }
    }
}

impl From<f64> for Value {
    fn from(x: f64) -> Value {
        Value::number(x)
    }
}
impl From<i32> for Value {
    fn from(i: i32) -> Value {
        Value::int(i)
    }
}
impl From<u32> for Value {
    fn from(i: u32) -> Value {
        Value::number(i as f64)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::bool(b)
    }
}
