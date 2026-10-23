//! The builtins: the intrinsic objects and the global object's properties. Each submodule
//! installs one family; this file has the shared helpers and the order they are made in.

pub(crate) mod array;
pub(crate) mod date;
pub(crate) mod encoding;
pub(crate) mod error;
pub(crate) mod function;
pub(crate) mod global;
pub(crate) mod iter;
pub(crate) mod json;
pub(crate) mod map;
pub(crate) mod math;
pub(crate) mod number;
pub(crate) mod object;
pub(crate) mod promise;
pub(crate) mod regexp;
pub(crate) mod string;
pub(crate) mod typedarray;

use alloc::vec::Vec;

use crate::names::N;
use crate::object::{ArrayData, BuiltinFn, C, ObjKind, PropKey, WC};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, SymId, Val};

#[inline]
pub(crate) fn arg(args: &[Val], i: usize) -> Val {
    args.get(i).copied().unwrap_or(Val::Undef)
}

fn noop(_r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    Ok(Val::Undef)
}

impl Realm {
    /// Defines a builtin method (non-enumerable).
    pub(crate) fn method(&mut self, obj: ObjId, name: &str, arity: u32, f: BuiltinFn) -> ObjId {
        let k = self.key_str(name);
        let nv = match k {
            PropKey::Str(s) => Val::Str(s),
            _ => self.str_val(name),
        };
        let fo = self.new_builtin(nv, arity, f);
        self.put_prop(obj, k, Val::Obj(fo), WC);
        fo
    }

    /// A method keyed by a symbol; its name is "[description]".
    pub(crate) fn method_sym(
        &mut self,
        obj: ObjId,
        sym: SymId,
        name: &str,
        arity: u32,
        f: BuiltinFn,
    ) -> ObjId {
        let nv = self.str_val(name);
        let fo = self.new_builtin(nv, arity, f);
        self.put_prop(obj, PropKey::Sym(sym), Val::Obj(fo), WC);
        fo
    }

    /// An accessor with a builtin getter (and optional setter).
    pub(crate) fn getter(&mut self, obj: ObjId, name: &str, g: BuiltinFn, s: Option<BuiltinFn>) {
        let k = self.key_str(name);
        let gn = self.str_val(&alloc::format!("get {name}"));
        let gf = self.new_builtin(gn, 0, g);
        let sf = match s {
            Some(s) => {
                let sn = self.str_val(&alloc::format!("set {name}"));
                Val::Obj(self.new_builtin(sn, 1, s))
            }
            None => Val::Undef,
        };
        self.put_accessor(obj, k, Val::Obj(gf), sf, C);
    }

    pub(crate) fn getter_sym(&mut self, obj: ObjId, sym: SymId, name: &str, g: BuiltinFn) {
        let gn = self.str_val(&alloc::format!("get {name}"));
        let gf = self.new_builtin(gn, 0, g);
        self.put_accessor(obj, PropKey::Sym(sym), Val::Obj(gf), Val::Undef, C);
    }

    /// A data property with attributes.
    pub(crate) fn value(&mut self, obj: ObjId, name: &str, v: Val, f: u8) {
        let k = self.key_str(name);
        self.put_prop(obj, k, v, f);
    }

    /// A constructor: `proto.constructor = F`, `F.prototype = proto`, and a global binding.
    pub(crate) fn ctor(&mut self, name: &str, arity: u32, f: BuiltinFn, proto: ObjId) -> ObjId {
        let nv = self.str_val(name);
        let fo = self.new_builtin(nv, arity, f);
        self.om(fo).flags |= crate::object::CONSTRUCTOR;
        let pk = self.key(N::prototype);
        self.put_prop(fo, pk, Val::Obj(proto), 0);
        let ck = self.key(N::constructor);
        self.put_prop(proto, ck, Val::Obj(fo), WC);
        let g = self.global;
        self.value(g, name, Val::Obj(fo), WC);
        fo
    }

    pub(crate) fn to_string_tag(&mut self, obj: ObjId, tag: &str) {
        let v = self.str_val(tag);
        let k = PropKey::Sym(self.sym.to_string_tag);
        self.put_prop(obj, k, v, C);
    }

    /// `{ value, done }`.
    pub(crate) fn iter_result(&mut self, v: Val, done: bool) -> Val {
        let o = self.new_plain();
        let vk = self.key(N::value);
        let dk = self.key(N::done);
        self.om(o).props.insert(
            vk,
            crate::object::Prop {
                v,
                f: crate::object::WEC,
            },
        );
        self.om(o).props.insert(
            dk,
            crate::object::Prop {
                v: Val::Bool(done),
                f: crate::object::WEC,
            },
        );
        Val::Obj(o)
    }

    /// The `length` of an array-like, as a usize-safe f64.
    pub(crate) fn length_of(&mut self, o: Val) -> Res<f64> {
        if let Val::Obj(id) = o
            && let ObjKind::Array(a) = &self.o(id).kind
        {
            return Ok(a.len as f64);
        }
        let lk = self.key(N::length);
        let l = self.get_v(o, lk)?;
        self.to_length(l)
    }

    /// Get by numeric index (for generic array methods).
    pub(crate) fn get_index(&mut self, o: Val, i: f64) -> Res<Val> {
        self.get_elem(o, Val::num(i))
    }

    pub(crate) fn idx_key(&mut self, i: f64) -> Res<PropKey> {
        if (0.0..4294967295.0).contains(&i) {
            return Ok(PropKey::Idx(i as u32));
        }
        self.to_key(Val::num(i))
    }

    /// SpeciesConstructor-lite: we always use the intrinsic Array/Promise.
    pub(crate) fn this_obj(&mut self, this: Val, what: &str) -> Res<ObjId> {
        match this {
            Val::Obj(o) => Ok(o),
            Val::Undef | Val::Null | Val::Empty => {
                Err(self.type_error(&alloc::format!("{what} called on null or undefined")))
            }
            _ => self.to_object(this),
        }
    }
}

pub(crate) fn init(r: &mut Realm) {
    // The two roots of everything.
    let op = r.alloc(None, ObjKind::Ordinary);
    r.intr.object_proto = op;
    let empty = Val::Str(r.nstr(N::empty));
    let fp = r.new_builtin(empty, 0, noop);
    r.om(fp).proto = Some(op);
    r.intr.function_proto = fp;
    let ap = r.alloc(
        Some(op),
        ObjKind::Array(ArrayData {
            dense: Vec::new(),
            len: 0,
        }),
    );
    r.intr.array_proto = ap;
    let g = r.alloc(Some(op), ObjKind::Ordinary);
    r.global = g;
    let gl = r.alloc(None, ObjKind::Ordinary);
    r.global_lex = gl;
    macro_rules! plain {
        ($($f:ident),*) => { $( r.intr.$f = r.alloc(Some(op), ObjKind::Ordinary); )* };
    }
    plain!(
        string_proto,
        number_proto,
        boolean_proto,
        symbol_proto,
        error_proto,
        iterator_proto,
        async_iterator_proto,
        promise_proto,
        regexp_proto,
        date_proto,
        map_proto,
        set_proto,
        weakmap_proto,
        weakset_proto
    );
    // Wrapper prototypes are themselves wrappers.
    let e = r.nstr(N::empty);
    r.om(r.intr.string_proto).kind = ObjKind::StrBox(e);
    r.om(r.intr.number_proto).kind = ObjKind::NumBox(0.0);
    r.om(r.intr.boolean_proto).kind = ObjKind::BoolBox(false);
    let ip = r.intr.iterator_proto;
    macro_rules! iters {
        ($($f:ident),*) => { $( r.intr.$f = r.alloc(Some(ip), ObjKind::Ordinary); )* };
    }
    iters!(
        array_iterator_proto,
        map_iterator_proto,
        set_iterator_proto,
        string_iterator_proto,
        regexp_string_iterator_proto,
        generator_proto
    );
    let aip = r.intr.async_iterator_proto;
    r.intr.async_generator_proto = r.alloc(Some(aip), ObjKind::Ordinary);
    r.intr.async_from_sync_iterator_proto = r.alloc(Some(aip), ObjKind::Ordinary);
    r.intr.generator_function_proto = r.alloc(Some(fp), ObjKind::Ordinary);
    r.intr.async_function_proto = r.alloc(Some(fp), ObjKind::Ordinary);
    r.intr.async_generator_function_proto = r.alloc(Some(fp), ObjKind::Ordinary);

    let gt = r.key(N::globalThis);
    r.put_prop(g, gt, Val::Obj(g), WC);

    object::init(r);
    function::init(r);
    error::init(r);
    array::init(r);
    string::init(r);
    number::init(r);
    math::init(r);
    json::init(r);
    iter::init(r);
    promise::init(r);
    map::init(r);
    regexp::init(r);
    date::init(r);
    typedarray::init(r);
    encoding::init(r);
    global::init(r);
}
