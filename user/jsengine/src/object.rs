//! The object model: objects, their property maps, arrays' dense elements, and the internal
//! methods ([[GetOwnProperty]], [[DefineOwnProperty]], [[Get]], [[Set]], [[Delete]],
//! [[OwnPropertyKeys]]) including host objects' hooks.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use crate::api::{HostFn, Key};
use crate::heap::Tracer;
use crate::names::N;
use crate::realm::{Realm, Res};
use crate::value::{ObjId, StrId, SymId, Val};

// ---- keys and properties ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PropKey {
    /// An interned string that is not a canonical array index.
    Str(StrId),
    Sym(SymId),
    /// A canonical array index (0 ..= 2^32 - 2).
    Idx(u32),
}

/// A deleted entry in a property map.
const TOMB: PropKey = PropKey::Idx(u32::MAX);

impl PropKey {
    #[inline]
    fn hash(self) -> u32 {
        match self {
            PropKey::Str(s) => s.idx.wrapping_mul(0x9E37_79B1),
            PropKey::Sym(s) => (s.0 ^ 0x5bd1_e995).wrapping_mul(0x85EB_CA77),
            PropKey::Idx(i) => (i ^ 0x2545_F491).wrapping_mul(0xC2B2_AE3D),
        }
    }
}

pub(crate) const W: u8 = 1;
pub(crate) const E: u8 = 2;
pub(crate) const C: u8 = 4;
pub(crate) const ACCESSOR: u8 = 8;
pub(crate) const WEC: u8 = W | E | C;
pub(crate) const WC: u8 = W | C;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Prop {
    /// The value, or for an accessor property an `ObjKind::Accessor` object.
    pub v: Val,
    pub f: u8,
}

/// Properties in insertion order, with a hash index once there are more than a few.
#[derive(Default)]
pub(crate) struct PropMap {
    pub ents: Vec<(PropKey, Prop)>,
    index: Option<Box<[u32]>>,
    dead: u32,
}

const INDEX_AT: usize = 8;

impl PropMap {
    pub fn new() -> PropMap {
        PropMap {
            ents: Vec::new(),
            index: None,
            dead: 0,
        }
    }
    pub fn with_capacity(n: usize) -> PropMap {
        PropMap {
            ents: Vec::with_capacity(n),
            index: None,
            dead: 0,
        }
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.ents.len() - self.dead as usize
    }
    #[inline]
    pub fn find(&self, k: PropKey) -> Option<usize> {
        match &self.index {
            None => self.ents.iter().position(|e| e.0 == k),
            Some(ix) => {
                let mask = ix.len() - 1;
                let mut i = k.hash() as usize & mask;
                loop {
                    let e = ix[i];
                    if e == 0 {
                        return None;
                    }
                    let p = (e - 1) as usize;
                    if self.ents[p].0 == k {
                        return Some(p);
                    }
                    i = (i + 1) & mask;
                }
            }
        }
    }
    #[inline]
    pub fn get(&self, k: PropKey) -> Option<&Prop> {
        self.find(k).map(|i| &self.ents[i].1)
    }
    pub fn insert(&mut self, k: PropKey, p: Prop) {
        let pos = self.ents.len();
        self.ents.push((k, p));
        match &mut self.index {
            Some(ix) if (self.ents.len() * 2) <= ix.len() => {
                let mask = ix.len() - 1;
                let mut i = k.hash() as usize & mask;
                while ix[i] != 0 {
                    i = (i + 1) & mask;
                }
                ix[i] = pos as u32 + 1;
            }
            _ => {
                if self.ents.len() > INDEX_AT {
                    self.rebuild_index();
                }
            }
        }
    }
    fn rebuild_index(&mut self) {
        let n = (self.ents.len() * 2).next_power_of_two().max(16) * 2;
        let mut ix = alloc::vec![0u32; n].into_boxed_slice();
        let mask = n - 1;
        for (pos, e) in self.ents.iter().enumerate() {
            if e.0 == TOMB {
                continue;
            }
            let mut i = e.0.hash() as usize & mask;
            while ix[i] != 0 {
                i = (i + 1) & mask;
            }
            ix[i] = pos as u32 + 1;
        }
        self.index = Some(ix);
    }
    pub fn remove_at(&mut self, pos: usize) {
        if self.index.is_none() {
            self.ents.remove(pos);
            return;
        }
        self.ents[pos] = (
            TOMB,
            Prop {
                v: Val::Undef,
                f: 0,
            },
        );
        self.dead += 1;
        if self.dead as usize > 8 && self.dead as usize * 2 > self.ents.len() {
            self.ents.retain(|e| e.0 != TOMB);
            self.dead = 0;
            if self.ents.len() > INDEX_AT {
                self.rebuild_index();
            } else {
                self.index = None;
            }
        } else {
            // Tombstones stay in the index (their key never matches a real one).
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = &(PropKey, Prop)> {
        self.ents.iter().filter(|e| e.0 != TOMB)
    }
    pub fn bytes(&self) -> usize {
        self.ents.capacity() * 32 + self.index.as_ref().map_or(0, |i| i.len() * 4)
    }
}

// ---- objects ----

pub(crate) const EXTENSIBLE: u16 = 1;
/// A function whose `name`, `length` (and `prototype`) properties are not created yet.
pub(crate) const LAZY_FN: u16 = 2;
/// A class constructor: calling it without `new` throws.
pub(crate) const CLASS_CTOR: u16 = 4;
/// An array whose `length` is not writable.
pub(crate) const LEN_RO: u16 = 8;
/// An array in sparse mode: no dense elements, every index lives in the property map.
pub(crate) const SPARSE: u16 = 16;
/// A function that can be called with `new`.
pub(crate) const CONSTRUCTOR: u16 = 32;
/// A lazy function that also wants a fresh `prototype` object.
pub(crate) const LAZY_PROTO: u16 = 64;

pub(crate) struct Obj {
    pub proto: Option<ObjId>,
    pub props: PropMap,
    pub kind: ObjKind,
    pub flags: u16,
}

pub(crate) struct ArrayData {
    /// Elements 0..dense.len() (Empty = hole). Unused in sparse mode.
    pub dense: Vec<Val>,
    pub len: u32,
}

pub(crate) type BuiltinFn = fn(&mut Realm, Val, &[Val]) -> Res<Val>;

pub(crate) enum NativeImpl {
    Builtin(BuiltinFn),
    Host(Rc<HostFn>),
}

pub(crate) struct NativeData {
    pub f: NativeImpl,
    /// The `name` (a string, or a symbol's description in brackets) and `length` to create.
    pub name: Val,
    pub arity: u32,
    /// Internal slots some builtins keep (a promise's resolving functions, and so on).
    pub slots: Vec<Val>,
    /// For a host class constructor: called with a fresh `this` under `new`.
    pub host_ctor: bool,
}

pub(crate) struct BoundData {
    pub target: ObjId,
    pub this: Val,
    pub args: Vec<Val>,
}

/// A small open-addressing map from object handles to values (WeakMap/WeakSet).
#[derive(Default)]
pub(crate) struct WeakMapData {
    ents: Vec<(ObjId, Val)>,
    /// A WeakSet (the values are unused).
    pub is_set: bool,
}

impl WeakMapData {
    pub fn iter(&self) -> impl Iterator<Item = (&ObjId, &Val)> {
        self.ents.iter().map(|(k, v)| (k, v))
    }
    pub fn get(&self, k: ObjId) -> Option<Val> {
        self.ents.iter().find(|e| e.0 == k).map(|e| e.1)
    }
    pub fn set(&mut self, k: ObjId, v: Val) {
        if let Some(e) = self.ents.iter_mut().find(|e| e.0 == k) {
            e.1 = v;
        } else {
            self.ents.push((k, v));
        }
    }
    pub fn remove(&mut self, k: ObjId) -> bool {
        if let Some(p) = self.ents.iter().position(|e| e.0 == k) {
            self.ents.swap_remove(p);
            true
        } else {
            false
        }
    }
}

pub(crate) enum ObjKind {
    /// A freed slot.
    Dead,
    Ordinary,
    Array(ArrayData),
    Native(Box<NativeData>),
    Bound(Box<BoundData>),
    /// An accessor property's getter and setter (undefined when absent).
    Accessor(Val, Val),
    BoolBox(bool),
    NumBox(f64),
    StrBox(StrId),
    SymBox(SymId),
    Date(f64),
    Host {
        class: u32,
        data: u64,
    },
    WeakMap(Box<WeakMapData>),
    Arguments,
    Error(Box<crate::errors::ErrInfo>),
    Closure(Box<crate::interp::Closure>),
    Env(Box<crate::interp::EnvData>),
    /// A generator, async function or async generator: its suspended frame.
    Coroutine(Box<crate::interp::CoData>),
    ArrayIter(Box<crate::interp::ArrayIterData>),
    ForIn(Box<crate::interp::ForInData>),
    Promise(Box<crate::builtins::promise::PromiseData>),
    Reaction(Box<crate::builtins::promise::ReactionData>),
    Map(Box<crate::builtins::map::MapData>),
    Set(Box<crate::builtins::map::MapData>),
    /// A Map/Set iterator.
    MapIter(Box<crate::builtins::map::MapIterData>),
    RegExp(Box<crate::regexp::RegExpData>),
    /// A string iterator (by code points).
    StrIter(Box<crate::builtins::string::StrIterData>),
    /// A matchAll iterator.
    RegExpStrIter(Box<crate::builtins::regexp::MatchAllData>),
    /// %AsyncFromSyncIteratorPrototype% objects: the sync iterator and its next method.
    AsyncFromSync(Val, Val),
}

impl Obj {
    pub fn new(proto: Option<ObjId>, kind: ObjKind) -> Obj {
        Obj {
            proto,
            props: PropMap::new(),
            kind,
            flags: EXTENSIBLE,
        }
    }
    pub fn dead() -> Obj {
        Obj {
            proto: None,
            props: PropMap::new(),
            kind: ObjKind::Dead,
            flags: 0,
        }
    }
    pub fn size_estimate(&self) -> usize {
        let base = core::mem::size_of::<Obj>() + self.props.bytes();
        base + match &self.kind {
            ObjKind::Array(a) => a.dense.capacity() * 16,
            ObjKind::Native(n) => 64 + n.slots.len() * 16,
            ObjKind::Bound(b) => 48 + b.args.len() * 16,
            ObjKind::WeakMap(m) => 32 + m.ents.capacity() * 24,
            ObjKind::Closure(c) => 64 + c.field_keys.len() * 16,
            ObjKind::Env(e) => 32 + e.slots.capacity() * 16,
            ObjKind::Coroutine(c) => c.bytes(),
            ObjKind::ArrayIter(_) => 32,
            ObjKind::ForIn(f) => 48 + f.keys.capacity() * 16,
            ObjKind::Promise(p) => 64 + (p.fulfill.len() + p.reject.len()) * 16,
            ObjKind::Reaction(_) => 64,
            ObjKind::Map(m) | ObjKind::Set(m) => m.bytes(),
            ObjKind::RegExp(r) => r.bytes(),
            ObjKind::StrIter(_) | ObjKind::RegExpStrIter(_) | ObjKind::MapIter(_) => 32,
            ObjKind::Error(e) => 64 + e.frames.len() + e.file.len(),
            _ => 0,
        }
    }
    pub fn trace(&self, t: &mut Tracer, epoch: u32) {
        t.opt_obj(self.proto);
        for (k, p) in self.props.iter() {
            if let PropKey::Str(s) = k {
                t.str(*s);
            }
            t.val(p.v);
        }
        match &self.kind {
            ObjKind::Array(a) => {
                for v in &a.dense {
                    t.val(*v);
                }
            }
            ObjKind::Native(n) => {
                t.val(n.name);
                for v in &n.slots {
                    t.val(*v);
                }
            }
            ObjKind::Bound(b) => {
                t.obj(b.target);
                t.val(b.this);
                for v in &b.args {
                    t.val(*v);
                }
            }
            ObjKind::Accessor(g, s) => {
                t.val(*g);
                t.val(*s);
            }
            ObjKind::StrBox(s) => t.str(*s),
            ObjKind::Closure(c) => c.trace(t, epoch),
            ObjKind::Env(e) => {
                t.opt_obj(e.parent);
                for v in &e.slots {
                    t.val(*v);
                }
            }
            ObjKind::Coroutine(c) => c.trace(t, epoch),
            ObjKind::ArrayIter(a) => t.val(a.obj),
            ObjKind::ForIn(f) => {
                t.val(f.obj);
                for k in &f.keys {
                    t.val(*k);
                }
            }
            ObjKind::Promise(p) => p.trace(t),
            ObjKind::Reaction(r) => r.trace(t),
            ObjKind::Map(m) | ObjKind::Set(m) => m.trace(t),
            ObjKind::MapIter(m) => t.obj(m.coll),
            ObjKind::RegExp(r) => r.trace(t),
            ObjKind::StrIter(s) => t.str(s.s),
            ObjKind::RegExpStrIter(m) => {
                t.val(m.re);
                t.str(m.s);
            }
            ObjKind::AsyncFromSync(a, b) => {
                t.val(*a);
                t.val(*b);
            }
            // WeakMap values are traced as ephemerons by the collector.
            _ => {}
        }
    }
    #[inline]
    pub fn is_callable(&self) -> bool {
        matches!(
            &self.kind,
            ObjKind::Native(_) | ObjKind::Bound(_) | ObjKind::Closure(_)
        )
    }
}

/// An own property as [[GetOwnProperty]] reports it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Own {
    Data(Val, u8),
    Accessor(Val, Val, u8),
}

impl Own {
    pub fn flags(&self) -> u8 {
        match self {
            Own::Data(_, f) | Own::Accessor(_, _, f) => *f,
        }
    }
}

/// A property descriptor for [[DefineOwnProperty]]: absent fields are None.
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Desc {
    pub value: Option<Val>,
    pub get: Option<Val>,
    pub set: Option<Val>,
    pub writable: Option<bool>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl Desc {
    pub fn data(v: Val, f: u8) -> Desc {
        Desc {
            value: Some(v),
            writable: Some(f & W != 0),
            enumerable: Some(f & E != 0),
            configurable: Some(f & C != 0),
            ..Desc::default()
        }
    }
    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }
    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }
}

const DENSE_SLACK: u32 = 1024;
const DENSE_MAX_GAP: u32 = 1 << 16;

impl Realm {
    // ---- allocation helpers ----

    pub(crate) fn alloc(&mut self, proto: Option<ObjId>, kind: ObjKind) -> ObjId {
        self.heap.alloc_obj(Obj::new(proto, kind))
    }

    pub(crate) fn new_plain(&mut self) -> ObjId {
        let p = self.intr.object_proto;
        self.alloc(Some(p), ObjKind::Ordinary)
    }

    pub(crate) fn new_array_from(&mut self, items: Vec<Val>) -> ObjId {
        let p = self.intr.array_proto;
        let len = items.len() as u32;
        self.alloc(Some(p), ObjKind::Array(ArrayData { dense: items, len }))
    }

    #[inline]
    pub(crate) fn o(&self, id: ObjId) -> &Obj {
        self.heap.o(id)
    }
    #[inline]
    pub(crate) fn om(&mut self, id: ObjId) -> &mut Obj {
        self.heap.om(id)
    }

    #[inline]
    pub(crate) fn key(&self, n: N) -> PropKey {
        PropKey::Str(self.names[n as usize])
    }
    #[inline]
    pub(crate) fn nstr(&self, n: N) -> StrId {
        self.names[n as usize]
    }

    pub(crate) fn key_str(&mut self, s: &str) -> PropKey {
        let u: Vec<u16> = s.encode_utf16().collect();
        self.key_units(&u)
    }

    pub(crate) fn key_units(&mut self, u: &[u16]) -> PropKey {
        if let Some(i) = crate::string::array_index(u) {
            return PropKey::Idx(i);
        }
        PropKey::Str(self.heap.intern(u))
    }

    /// The property key for a string value.
    pub(crate) fn key_from_str(&mut self, s: StrId) -> PropKey {
        if self.heap.is_interned(s) {
            // Interned strings that are indices are never made (key_units checks first),
            // but a script string may still be one.
            let u = self.heap.flat(s);
            if u.len() <= 10
                && !u.is_empty()
                && crate::string::is_ascii_digit(u[0])
                && let Some(i) = crate::string::array_index(u)
            {
                return PropKey::Idx(i);
            }
            return PropKey::Str(s);
        }
        let len = self.heap.str_len(s);
        if len <= 10 {
            let u = self.heap.units(s);
            if let Some(i) = crate::string::array_index(u) {
                return PropKey::Idx(i);
            }
        }
        PropKey::Str(self.heap.intern_id(s))
    }

    /// A key as a value (for Object.keys and friends).
    pub(crate) fn key_to_val(&mut self, k: PropKey) -> Val {
        match k {
            PropKey::Str(s) => Val::Str(s),
            PropKey::Sym(s) => Val::Sym(s),
            PropKey::Idx(i) => {
                let s = crate::numconv::u32_to_units(i);
                Val::Str(self.heap.new_str_vec(s))
            }
        }
    }

    // ---- lazy function properties ----

    pub(crate) fn materialize_fn(&mut self, id: ObjId) {
        let flags = self.o(id).flags;
        if flags & LAZY_FN == 0 {
            return;
        }
        self.om(id).flags &= !(LAZY_FN | LAZY_PROTO);
        let mut gen_proto = false;
        let (name, arity) = match &self.o(id).kind {
            ObjKind::Native(n) => (n.name, n.arity),
            ObjKind::Closure(c) => {
                gen_proto = c.code.is_generator;
                let n = match c.code.name {
                    Some(s) => Val::Str(s),
                    None => Val::Str(self.nstr(N::empty)),
                };
                (n, c.code.length)
            }
            _ => return,
        };
        let lk = self.key(N::length);
        let nk = self.key(N::name);
        // `length` then `name`, in front of anything already defined (the spec's order).
        let mut front = PropMap::with_capacity(3 + self.o(id).props.len());
        front.insert(
            lk,
            Prop {
                v: Val::num(arity as f64),
                f: C,
            },
        );
        front.insert(nk, Prop { v: name, f: C });
        if flags & LAZY_PROTO != 0 && gen_proto {
            // A generator function's prototype: inherits %GeneratorPrototype%, no constructor.
            let is_async = matches!(&self.o(id).kind, ObjKind::Closure(c) if c.code.is_async);
            let gp = if is_async {
                self.intr.async_generator_proto
            } else {
                self.intr.generator_proto
            };
            let proto = self.alloc(Some(gp), ObjKind::Ordinary);
            let pk = self.key(N::prototype);
            front.insert(
                pk,
                Prop {
                    v: Val::Obj(proto),
                    f: W,
                },
            );
        } else if flags & LAZY_PROTO != 0 {
            let op = self.intr.object_proto;
            let proto = self.alloc(Some(op), ObjKind::Ordinary);
            let ck = self.key(N::constructor);
            self.om(proto).props.insert(
                ck,
                Prop {
                    v: Val::Obj(id),
                    f: WC,
                },
            );
            let pk = self.key(N::prototype);
            front.insert(
                pk,
                Prop {
                    v: Val::Obj(proto),
                    f: W,
                },
            );
        }
        let old = core::mem::take(&mut self.om(id).props);
        for (k, p) in old.iter() {
            if front.find(*k).is_none() {
                front.insert(*k, *p);
            }
        }
        self.om(id).props = front;
    }

    // ---- [[GetOwnProperty]] (no hooks) ----

    pub(crate) fn get_own_plain(&mut self, id: ObjId, k: PropKey) -> Option<Own> {
        if self.o(id).flags & LAZY_FN != 0 {
            self.materialize_fn(id);
        }
        let obj = self.o(id);
        match &obj.kind {
            ObjKind::Array(a) => {
                if let PropKey::Idx(i) = k {
                    if obj.flags & SPARSE == 0 {
                        return match a.dense.get(i as usize) {
                            Some(v) if !v.is_empty() => Some(Own::Data(*v, WEC)),
                            _ => None,
                        };
                    }
                } else if k == self.key(N::length) {
                    let f = if obj.flags & LEN_RO != 0 { 0 } else { W };
                    return Some(Own::Data(Val::num(a.len as f64), f));
                }
            }
            ObjKind::StrBox(s) => {
                let s = *s;
                if let PropKey::Idx(i) = k {
                    let len = self.heap.str_len(s);
                    if (i as usize) < len {
                        let c = self.heap.units(s)[i as usize];
                        let cs = self.char_str(c);
                        return Some(Own::Data(Val::Str(cs), E));
                    }
                } else if k == self.key(N::length) {
                    return Some(Own::Data(Val::num(self.heap.str_len(s) as f64), 0));
                }
            }
            _ => {}
        }
        let obj = self.o(id);
        let p = obj.props.get(k)?;
        if p.f & ACCESSOR != 0 {
            let (g, s) = match p.v {
                Val::Obj(a) => match self.o(a).kind {
                    ObjKind::Accessor(g, s) => (g, s),
                    _ => (Val::Undef, Val::Undef),
                },
                _ => (Val::Undef, Val::Undef),
            };
            Some(Own::Accessor(g, s, p.f & (E | C)))
        } else {
            Some(Own::Data(p.v, p.f))
        }
    }

    /// [[GetOwnProperty]] including host hooks.
    pub(crate) fn get_own(&mut self, id: ObjId, k: PropKey) -> Res<Option<Own>> {
        if let ObjKind::Host { class, data } = self.o(id).kind
            && let Some(v) = self.hook_get(id, class, data, k)?
        {
            return Ok(Some(Own::Data(v, WEC)));
        }
        Ok(self.get_own_plain(id, k))
    }

    // ---- [[Get]] ----

    pub(crate) fn get_p(&mut self, id: ObjId, k: PropKey, receiver: Val) -> Res<Val> {
        let mut cur = id;
        let mut depth = 0u32;
        loop {
            // Fast path: a plain data property in the map.
            let obj = self.o(cur);
            if obj.flags & LAZY_FN == 0
                && !matches!(
                    obj.kind,
                    ObjKind::Array(_) | ObjKind::StrBox(_) | ObjKind::Host { .. }
                )
            {
                if let Some(p) = obj.props.get(k) {
                    if p.f & ACCESSOR == 0 {
                        return Ok(p.v);
                    }
                    let a = p.v;
                    return self.call_getter(a, receiver);
                }
            } else {
                if let ObjKind::Host { class, data } = obj.kind
                    && let Some(v) = self.hook_get(cur, class, data, k)?
                {
                    return Ok(v);
                }
                match self.get_own_plain(cur, k) {
                    Some(Own::Data(v, _)) => return Ok(v),
                    Some(Own::Accessor(g, _, _)) => {
                        if g.is_undef() {
                            return Ok(Val::Undef);
                        }
                        return self.call_v(g, receiver, &[]);
                    }
                    None => {}
                }
            }
            match self.o(cur).proto {
                Some(p) => cur = p,
                None => return Ok(Val::Undef),
            }
            depth += 1;
            if depth > 100_000 {
                return Ok(Val::Undef);
            }
        }
    }

    fn call_getter(&mut self, acc: Val, receiver: Val) -> Res<Val> {
        let g = match acc {
            Val::Obj(a) => match self.o(a).kind {
                ObjKind::Accessor(g, _) => g,
                _ => Val::Undef,
            },
            _ => Val::Undef,
        };
        if g.is_undef() {
            return Ok(Val::Undef);
        }
        self.call_v(g, receiver, &[])
    }

    /// Get on any value (primitives use their prototype).
    pub(crate) fn get_v(&mut self, v: Val, k: PropKey) -> Res<Val> {
        match v {
            Val::Obj(o) => self.get_p(o, k, v),
            Val::Str(s) => {
                match k {
                    PropKey::Idx(i) => {
                        let len = self.heap.str_len(s);
                        if (i as usize) < len {
                            let c = self.heap.units(s)[i as usize];
                            return Ok(Val::Str(self.char_str(c)));
                        }
                        return Ok(Val::Undef);
                    }
                    _ if k == self.key(N::length) => {
                        return Ok(Val::Int(self.heap.str_len(s) as i32));
                    }
                    _ => {}
                }
                let p = self.intr.string_proto;
                self.get_p(p, k, v)
            }
            Val::Int(_) | Val::Num(_) => {
                let p = self.intr.number_proto;
                self.get_p(p, k, v)
            }
            Val::Bool(_) => {
                let p = self.intr.boolean_proto;
                self.get_p(p, k, v)
            }
            Val::Sym(_) => {
                let p = self.intr.symbol_proto;
                self.get_p(p, k, v)
            }
            Val::Undef | Val::Null | Val::Empty => {
                let ks = self.key_display(k);
                let what = if v.is_undef() || v.is_empty() {
                    "undefined"
                } else {
                    "null"
                };
                Err(self.type_error(&alloc::format!(
                    "Cannot read properties of {what} (reading '{ks}')"
                )))
            }
        }
    }

    // ---- [[DefineOwnProperty]] ----

    /// ValidateAndApplyPropertyDescriptor. Returns false where the spec says reject.
    pub(crate) fn define_own(&mut self, id: ObjId, k: PropKey, d: Desc) -> Res<bool> {
        if self.o(id).flags & LAZY_FN != 0 {
            self.materialize_fn(id);
        }
        if let ObjKind::Array(_) = self.o(id).kind {
            if k == self.key(N::length) {
                return self.array_define_length(id, d);
            }
            if let PropKey::Idx(i) = k {
                let len = match &self.o(id).kind {
                    ObjKind::Array(a) => a.len,
                    _ => 0,
                };
                if i >= len && self.o(id).flags & LEN_RO != 0 {
                    return Ok(false);
                }
                // Fast path: a default data element in dense mode.
                let is_default = !d.is_accessor()
                    && d.writable != Some(false)
                    && d.enumerable != Some(false)
                    && d.configurable != Some(false);
                if self.o(id).flags & SPARSE == 0 {
                    let existing = self.get_own_plain(id, k);
                    if is_default
                        && (existing.is_some()
                            || (d.writable.is_some()
                                && d.enumerable.is_some()
                                && d.configurable.is_some()))
                    {
                        let v = d.value.unwrap_or(match existing {
                            Some(Own::Data(v, _)) => v,
                            _ => Val::Undef,
                        });
                        if existing.is_none() && self.o(id).flags & EXTENSIBLE == 0 {
                            return Ok(false);
                        }
                        if self.array_put_dense(id, i, v) {
                            return Ok(true);
                        }
                    }
                    self.array_to_sparse(id);
                }
                let ok = self.define_plain(id, k, d)?;
                if ok
                    && i >= len
                    && let ObjKind::Array(a) = &mut self.om(id).kind
                {
                    a.len = i + 1;
                }
                return Ok(ok);
            }
        }
        if let ObjKind::StrBox(s) = self.o(id).kind {
            let len = self.heap.str_len(s);
            let is_str_own = match k {
                PropKey::Idx(i) => (i as usize) < len,
                _ => k == self.key(N::length),
            };
            if is_str_own {
                // Non-writable, non-configurable: only a compatible redefinition succeeds.
                let cur = self.get_own_plain(id, k);
                return Ok(match cur {
                    Some(Own::Data(v, f)) => {
                        !d.is_accessor()
                            && d.configurable != Some(true)
                            && d.writable != Some(true)
                            && d.enumerable.is_none_or(|e| e == (f & E != 0))
                            && d.value.is_none_or(|nv| self.same_val(nv, v))
                    }
                    _ => false,
                });
            }
        }
        if let ObjKind::Host { class, data } = self.o(id).kind
            && d.value.is_some()
            && !d.is_accessor()
            && self.hook_set(id, class, data, k, d.value.unwrap_or(Val::Undef))?
        {
            return Ok(true);
        }
        self.define_plain(id, k, d)
    }

    /// ValidateAndApplyPropertyDescriptor over the property map only.
    pub(crate) fn define_plain(&mut self, id: ObjId, k: PropKey, d: Desc) -> Res<bool> {
        let pos = self.o(id).props.find(k);
        let Some(pos) = pos else {
            if self.o(id).flags & EXTENSIBLE == 0 {
                return Ok(false);
            }
            let mut f = 0u8;
            if d.enumerable == Some(true) {
                f |= E;
            }
            if d.configurable == Some(true) {
                f |= C;
            }
            let prop = if d.is_accessor() {
                let g = d.get.unwrap_or(Val::Undef);
                let s = d.set.unwrap_or(Val::Undef);
                let a = self.alloc(None, ObjKind::Accessor(g, s));
                Prop {
                    v: Val::Obj(a),
                    f: f | ACCESSOR,
                }
            } else {
                if d.writable == Some(true) {
                    f |= W;
                }
                Prop {
                    v: d.value.unwrap_or(Val::Undef),
                    f,
                }
            };
            self.heap.charge(32);
            self.om(id).props.insert(k, prop);
            return Ok(true);
        };
        let cur = self.o(id).props.ents[pos].1;
        let cur_acc = cur.f & ACCESSOR != 0;
        let (cg, cs) = if cur_acc {
            match cur.v {
                Val::Obj(a) => match self.o(a).kind {
                    ObjKind::Accessor(g, s) => (g, s),
                    _ => (Val::Undef, Val::Undef),
                },
                _ => (Val::Undef, Val::Undef),
            }
        } else {
            (Val::Undef, Val::Undef)
        };
        if cur.f & C == 0 {
            if d.configurable == Some(true) {
                return Ok(false);
            }
            if let Some(e) = d.enumerable
                && e != (cur.f & E != 0)
            {
                return Ok(false);
            }
            if d.is_accessor() != cur_acc && (d.is_accessor() || d.is_data()) {
                return Ok(false);
            }
            if cur_acc {
                if let Some(g) = d.get
                    && !self.same_val(g, cg)
                {
                    return Ok(false);
                }
                if let Some(s) = d.set
                    && !self.same_val(s, cs)
                {
                    return Ok(false);
                }
            } else if cur.f & W == 0 {
                if d.writable == Some(true) {
                    return Ok(false);
                }
                if let Some(v) = d.value
                    && !self.same_val(v, cur.v)
                {
                    return Ok(false);
                }
            }
        }
        // Apply.
        let mut f = cur.f & (E | C);
        if let Some(e) = d.enumerable {
            if e { f |= E } else { f &= !E }
        }
        if let Some(c) = d.configurable {
            if c { f |= C } else { f &= !C }
        }
        let newp = if d.is_accessor() || (cur_acc && !d.is_data()) {
            let g = d.get.unwrap_or(if cur_acc { cg } else { Val::Undef });
            let s = d.set.unwrap_or(if cur_acc { cs } else { Val::Undef });
            let a = self.alloc(None, ObjKind::Accessor(g, s));
            Prop {
                v: Val::Obj(a),
                f: f | ACCESSOR,
            }
        } else {
            let wr = match d.writable {
                Some(w) => w,
                None => !cur_acc && cur.f & W != 0,
            };
            if wr {
                f |= W;
            }
            let v = d.value.unwrap_or(if cur_acc { Val::Undef } else { cur.v });
            Prop { v, f }
        };
        self.om(id).props.ents[pos].1 = newp;
        Ok(true)
    }

    /// CreateDataProperty (a default data property), ignoring failure.
    pub(crate) fn create_data(&mut self, id: ObjId, k: PropKey, v: Val) -> Res<bool> {
        // Fast path for ordinary extensible objects.
        let obj = self.o(id);
        if matches!(obj.kind, ObjKind::Ordinary)
            && obj.flags & EXTENSIBLE != 0
            && obj.flags & LAZY_FN == 0
        {
            match obj.props.find(k) {
                Some(pos) => {
                    let p = obj.props.ents[pos].1;
                    if p.f & C != 0 {
                        self.om(id).props.ents[pos].1 = Prop { v, f: WEC };
                        return Ok(true);
                    }
                }
                None => {
                    self.heap.charge(32);
                    self.om(id).props.insert(k, Prop { v, f: WEC });
                    return Ok(true);
                }
            }
        }
        self.define_own(id, k, Desc::data(v, WEC))
    }

    /// Defines a property directly (builtins' setup): replaces whatever is there.
    pub(crate) fn put_prop(&mut self, id: ObjId, k: PropKey, v: Val, f: u8) {
        if self.o(id).flags & LAZY_FN != 0 {
            self.materialize_fn(id);
        }
        if let ObjKind::Array(_) = self.o(id).kind
            && let PropKey::Idx(i) = k
        {
            if f == WEC && self.o(id).flags & SPARSE == 0 && self.array_put_dense(id, i, v) {
                return;
            }
            self.array_to_sparse(id);
            if let ObjKind::Array(a) = &mut self.om(id).kind
                && i >= a.len
            {
                a.len = i + 1;
            }
        }
        let obj = self.om(id);
        match obj.props.find(k) {
            Some(pos) => obj.props.ents[pos].1 = Prop { v, f },
            None => obj.props.insert(k, Prop { v, f }),
        }
    }

    pub(crate) fn put_accessor(&mut self, id: ObjId, k: PropKey, g: Val, s: Val, f: u8) {
        let a = self.alloc(None, ObjKind::Accessor(g, s));
        let obj = self.om(id);
        let p = Prop {
            v: Val::Obj(a),
            f: (f & (E | C)) | ACCESSOR,
        };
        match obj.props.find(k) {
            Some(pos) => obj.props.ents[pos].1 = p,
            None => obj.props.insert(k, p),
        }
    }

    // ---- arrays ----

    /// Writes a default element in dense mode; false if it doesn't fit densely.
    pub(crate) fn array_put_dense(&mut self, id: ObjId, i: u32, v: Val) -> bool {
        let flags = self.o(id).flags;
        if flags & SPARSE != 0 {
            return false;
        }
        let ObjKind::Array(a) = &mut self.heap.om(id).kind else {
            return false;
        };
        let n = a.dense.len() as u32;
        if i < n {
            a.dense[i as usize] = v;
            return true;
        }
        if i >= a.len && flags & LEN_RO != 0 {
            return false;
        }
        if i == n {
            if flags & EXTENSIBLE == 0 {
                return false;
            }
            a.dense.push(v);
            if i >= a.len {
                a.len = i + 1;
            }
            self.heap.charge(16);
            return true;
        }
        if i - n < DENSE_SLACK.max(n) && i < n.saturating_add(DENSE_MAX_GAP) {
            if flags & EXTENSIBLE == 0 {
                return false;
            }
            let grow = (i - n + 1) as usize;
            a.dense.resize(i as usize, Val::Empty);
            a.dense.push(v);
            if i >= a.len {
                a.len = i + 1;
            }
            self.heap.charge(grow * 16);
            return true;
        }
        false
    }

    /// Moves every dense element into the property map (for elements with attributes,
    /// accessors, frozen arrays and very sparse writes).
    pub(crate) fn array_to_sparse(&mut self, id: ObjId) {
        if self.o(id).flags & SPARSE != 0 {
            return;
        }
        let dense = match &mut self.om(id).kind {
            ObjKind::Array(a) => core::mem::take(&mut a.dense),
            _ => return,
        };
        let old = core::mem::take(&mut self.om(id).props);
        let mut pm = PropMap::with_capacity(dense.len() + old.len());
        for (i, v) in dense.into_iter().enumerate() {
            if !v.is_empty() {
                pm.insert(PropKey::Idx(i as u32), Prop { v, f: WEC });
            }
        }
        for (k, p) in old.iter() {
            pm.insert(*k, *p);
        }
        let obj = self.om(id);
        obj.props = pm;
        obj.flags |= SPARSE;
    }

    pub(crate) fn array_len(&self, id: ObjId) -> u32 {
        match &self.o(id).kind {
            ObjKind::Array(a) => a.len,
            _ => 0,
        }
    }

    fn array_define_length(&mut self, id: ObjId, d: Desc) -> Res<bool> {
        if d.is_accessor() || d.configurable == Some(true) || d.enumerable == Some(true) {
            return Ok(false);
        }
        let ro = self.o(id).flags & LEN_RO != 0;
        let Some(v) = d.value else {
            if d.writable == Some(true) && ro {
                return Ok(false);
            }
            if d.writable == Some(false) {
                self.om(id).flags |= LEN_RO;
            }
            return Ok(true);
        };
        let n = self.to_num(v)?;
        let len = n as u32;
        if len as f64 != n {
            return Err(self.range_error("Invalid array length"));
        }
        if ro {
            return Ok(len == self.array_len(id) && d.writable != Some(true));
        }
        let ok = self.array_set_length(id, len);
        if d.writable == Some(false) {
            self.om(id).flags |= LEN_RO;
        }
        Ok(ok)
    }

    /// Sets an array's length, deleting elements at or past it (stops at a
    /// non-configurable one, as the spec says; returns false then).
    pub(crate) fn array_set_length(&mut self, id: ObjId, len: u32) -> bool {
        let sparse = self.o(id).flags & SPARSE != 0;
        let ObjKind::Array(a) = &mut self.heap.om(id).kind else {
            return false;
        };
        if !sparse {
            if (len as usize) < a.dense.len() {
                a.dense.truncate(len as usize);
                a.dense.shrink_to(len as usize * 2 + 4);
            }
            a.len = len;
            return true;
        }
        let old = a.len;
        if len >= old {
            a.len = len;
            return true;
        }
        // Delete indices >= len, highest first, stopping at a non-configurable one.
        let mut idx: Vec<(u32, u8)> = self
            .o(id)
            .props
            .iter()
            .filter_map(|(k, p)| match k {
                PropKey::Idx(i) if *i >= len => Some((*i, p.f)),
                _ => None,
            })
            .collect();
        idx.sort_unstable_by_key(|e| core::cmp::Reverse(e.0));
        let mut new_len = len;
        let mut ok = true;
        for (i, f) in idx {
            if f & C == 0 {
                new_len = i + 1;
                ok = false;
                break;
            }
            if let Some(pos) = self.o(id).props.find(PropKey::Idx(i)) {
                self.om(id).props.remove_at(pos);
            }
        }
        if let ObjKind::Array(a) = &mut self.om(id).kind {
            a.len = new_len;
        }
        ok
    }

    // ---- [[Set]] ----

    /// OrdinarySet. Ok(false) means the assignment was rejected (strict code throws).
    pub(crate) fn set_p(&mut self, id: ObjId, k: PropKey, v: Val, receiver: Val) -> Res<bool> {
        // Fast paths when the receiver is the object itself.
        if let Val::Obj(r) = receiver
            && r == id
        {
            let obj = self.o(id);
            match &obj.kind {
                ObjKind::Ordinary
                | ObjKind::Arguments
                | ObjKind::Native(_)
                | ObjKind::Closure(_)
                    if obj.flags & LAZY_FN == 0 =>
                {
                    if let Some(pos) = obj.props.find(k) {
                        let p = obj.props.ents[pos].1;
                        if p.f & (W | ACCESSOR) == W {
                            self.om(id).props.ents[pos].1.v = v;
                            return Ok(true);
                        }
                    }
                }
                ObjKind::Array(a) if obj.flags & SPARSE == 0 => {
                    if let PropKey::Idx(i) = k {
                        if (i as usize) < a.dense.len() && !a.dense[i as usize].is_empty() {
                            if let ObjKind::Array(a) = &mut self.om(id).kind {
                                a.dense[i as usize] = v;
                            }
                            return Ok(true);
                        }
                    } else if k == self.key(N::length) {
                        if obj.flags & LEN_RO != 0 {
                            return Ok(false);
                        }
                        let n = self.to_num(v)?;
                        let len = n as u32;
                        if len as f64 != n {
                            return Err(self.range_error("Invalid array length"));
                        }
                        return Ok(self.array_set_length(id, len));
                    }
                }
                _ => {}
            }
        }
        // Walk the chain for the property.
        let mut cur = id;
        let mut depth = 0;
        let own: Option<Own> = loop {
            if let ObjKind::Host { class, data } = self.o(cur).kind
                && self.class_has_hooks(class)
            {
                if cur == id && receiver_is(receiver, id) {
                    if self.hook_set(cur, class, data, k, v)? {
                        return Ok(true);
                    }
                } else if let Some(hv) = self.hook_get(cur, class, data, k)? {
                    break Some(Own::Data(hv, WEC));
                }
            }
            if let Some(o) = self.get_own_plain(cur, k) {
                break Some(o);
            }
            match self.o(cur).proto {
                Some(p) => cur = p,
                None => break None,
            }
            depth += 1;
            if depth > 100_000 {
                break None;
            }
        };
        match own {
            Some(Own::Accessor(_, s, _)) => {
                if s.is_undef() {
                    return Ok(false);
                }
                self.call_v(s, receiver, &[v])?;
                Ok(true)
            }
            Some(Own::Data(_, f)) if f & W == 0 => Ok(false),
            _ => {
                let Val::Obj(r) = receiver else {
                    return Ok(false);
                };
                if r != cur || own.is_none() {
                    match self.get_own(r, k)? {
                        Some(Own::Accessor(..)) => return Ok(false),
                        Some(Own::Data(_, f)) => {
                            if f & W == 0 {
                                return Ok(false);
                            }
                            return self.define_own(
                                r,
                                k,
                                Desc {
                                    value: Some(v),
                                    ..Desc::default()
                                },
                            );
                        }
                        None => return self.create_data(r, k, v),
                    }
                }
                // Own writable data property on the receiver itself.
                self.define_own(
                    r,
                    k,
                    Desc {
                        value: Some(v),
                        ..Desc::default()
                    },
                )
            }
        }
    }

    /// Set on any value; `strict` turns a rejected assignment into a TypeError.
    pub(crate) fn set_v(&mut self, target: Val, k: PropKey, v: Val, strict: bool) -> Res<()> {
        let ok = match target {
            Val::Obj(o) => self.set_p(o, k, v, target)?,
            Val::Undef | Val::Null | Val::Empty => {
                let ks = self.key_display(k);
                let what = if matches!(target, Val::Null) {
                    "null"
                } else {
                    "undefined"
                };
                return Err(self.type_error(&alloc::format!(
                    "Cannot set properties of {what} (setting '{ks}')"
                )));
            }
            _ => {
                let proto = self.proto_of_primitive(target);
                self.set_p(proto, k, v, target)?
            }
        };
        if !ok && strict {
            let ks = self.key_display(k);
            return Err(self.type_error(&alloc::format!(
                "Cannot assign to read only property '{ks}' of object"
            )));
        }
        Ok(())
    }

    pub(crate) fn proto_of_primitive(&self, v: Val) -> ObjId {
        match v {
            Val::Str(_) => self.intr.string_proto,
            Val::Int(_) | Val::Num(_) => self.intr.number_proto,
            Val::Bool(_) => self.intr.boolean_proto,
            Val::Sym(_) => self.intr.symbol_proto,
            _ => self.intr.object_proto,
        }
    }

    // ---- [[HasProperty]] / [[Delete]] ----

    pub(crate) fn has_property(&mut self, id: ObjId, k: PropKey) -> Res<bool> {
        let mut cur = id;
        let mut depth = 0;
        loop {
            if let ObjKind::Host { class, data } = self.o(cur).kind
                && let Some(b) = self.hook_has(cur, class, data, k)?
            {
                if b {
                    return Ok(true);
                }
            } else if self.get_own_plain(cur, k).is_some() {
                return Ok(true);
            }
            match self.o(cur).proto {
                Some(p) => cur = p,
                None => return Ok(false),
            }
            depth += 1;
            if depth > 100_000 {
                return Ok(false);
            }
        }
    }

    pub(crate) fn has_own_p(&mut self, id: ObjId, k: PropKey) -> Res<bool> {
        Ok(self.get_own(id, k)?.is_some())
    }

    pub(crate) fn delete_p(&mut self, id: ObjId, k: PropKey) -> Res<bool> {
        if self.o(id).flags & LAZY_FN != 0 {
            self.materialize_fn(id);
        }
        if let ObjKind::Host { class, data } = self.o(id).kind
            && let Some(b) = self.hook_delete(id, class, data, k)?
        {
            return Ok(b);
        }
        let flags = self.o(id).flags;
        match &mut self.heap.om(id).kind {
            ObjKind::Array(a) => {
                if let PropKey::Idx(i) = k
                    && flags & SPARSE == 0
                {
                    let i = i as usize;
                    if i < a.dense.len() {
                        if i + 1 == a.dense.len() {
                            a.dense.pop();
                            while matches!(a.dense.last(), Some(Val::Empty)) {
                                a.dense.pop();
                            }
                        } else {
                            a.dense[i] = Val::Empty;
                        }
                    }
                    return Ok(true);
                }
                if k == self.key(N::length) {
                    return Ok(false);
                }
            }
            ObjKind::StrBox(s) => {
                let s = *s;
                match k {
                    PropKey::Idx(i) if (i as usize) < self.heap.str_len(s) => return Ok(false),
                    _ if k == self.key(N::length) => return Ok(false),
                    _ => {}
                }
            }
            _ => {}
        }
        let obj = self.om(id);
        match obj.props.find(k) {
            None => Ok(true),
            Some(pos) => {
                if obj.props.ents[pos].1.f & C == 0 {
                    return Ok(false);
                }
                obj.props.remove_at(pos);
                Ok(true)
            }
        }
    }

    // ---- [[OwnPropertyKeys]] ----

    /// Own keys in spec order: indices ascending, then strings and symbols in insertion order.
    /// `only_enumerable` filters by the enumerable attribute.
    pub(crate) fn own_keys_p(
        &mut self,
        id: ObjId,
        strings: bool,
        symbols: bool,
        only_enum: bool,
    ) -> Res<Vec<PropKey>> {
        if self.o(id).flags & LAZY_FN != 0 {
            self.materialize_fn(id);
        }
        let mut out: Vec<PropKey> = Vec::new();
        if let ObjKind::Host { class, data } = self.o(id).kind
            && strings
        {
            for k in self.hook_keys(id, class, data)? {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        let obj = self.o(id);
        let mut idx: Vec<u32> = Vec::new();
        if strings {
            match &obj.kind {
                ObjKind::Array(a) if obj.flags & SPARSE == 0 => {
                    for (i, v) in a.dense.iter().enumerate() {
                        if !v.is_empty() {
                            idx.push(i as u32);
                        }
                    }
                }
                ObjKind::StrBox(s) => {
                    let n = self.heap.str_len(*s) as u32;
                    idx.extend(0..n);
                }
                _ => {}
            }
            let before = idx.len();
            for (k, p) in obj.props.iter() {
                if let PropKey::Idx(i) = k
                    && (!only_enum || p.f & E != 0)
                {
                    idx.push(*i);
                }
            }
            if idx.len() > before {
                idx.sort_unstable();
                idx.dedup();
            }
            for i in idx {
                let k = PropKey::Idx(i);
                if !out.contains(&k) {
                    out.push(k);
                }
            }
            if !only_enum {
                if let ObjKind::Array(_) = &obj.kind {
                    out.push(self.key(N::length));
                } else if let ObjKind::StrBox(_) = &obj.kind {
                    out.push(self.key(N::length));
                }
            }
            for (k, p) in obj.props.iter() {
                if let PropKey::Str(_) = k
                    && (!only_enum || p.f & E != 0)
                    && !out.contains(k)
                {
                    out.push(*k);
                }
            }
        }
        if symbols {
            for (k, p) in obj.props.iter() {
                if let PropKey::Sym(_) = k
                    && (!only_enum || p.f & E != 0)
                {
                    out.push(*k);
                }
            }
        }
        Ok(out)
    }

    // ---- host hooks ----

    pub(crate) fn class_has_hooks(&self, class: u32) -> bool {
        self.classes
            .get(class as usize)
            .is_some_and(|c| c.hooks != 0)
    }

    fn hook_key<'a>(&mut self, k: PropKey, buf: &'a mut alloc::string::String) -> Option<Key<'a>> {
        match k {
            PropKey::Idx(i) => Some(Key::Index(i)),
            PropKey::Str(s) => {
                let u = self.heap.flat(s);
                *buf = crate::string::to_rust(u);
                Some(Key::Str(buf.as_str()))
            }
            PropKey::Sym(s) => Some(Key::Value(crate::api::Value(Val::Sym(s)))),
        }
    }

    fn hook_wants(&self, class: u32, k: PropKey) -> Option<Rc<dyn crate::api::HostClass>> {
        let c = self.classes.get(class as usize)?;
        let want = match k {
            PropKey::Idx(_) => c.hooks & crate::api::hooks::INDEXED != 0,
            PropKey::Str(_) => c.hooks & crate::api::hooks::NAMED != 0,
            PropKey::Sym(_) => c.hooks & crate::api::hooks::SYMBOLS != 0,
        };
        if want { Some(c.class.clone()) } else { None }
    }

    pub(crate) fn hook_get(
        &mut self,
        id: ObjId,
        class: u32,
        data: u64,
        k: PropKey,
    ) -> Res<Option<Val>> {
        let Some(c) = self.hook_wants(class, k) else {
            return Ok(None);
        };
        let mut buf = alloc::string::String::new();
        let Some(key) = self.hook_key(k, &mut buf) else {
            return Ok(None);
        };
        let mark = self.temps.len();
        self.temps.push(Val::Obj(id));
        let r = c.get(self, crate::api::Value(Val::Obj(id)), data, key);
        self.temps.truncate(mark);
        Ok(r?.map(|v| self.check_val(v.0)))
    }

    pub(crate) fn hook_set(
        &mut self,
        id: ObjId,
        class: u32,
        data: u64,
        k: PropKey,
        v: Val,
    ) -> Res<bool> {
        let Some(c) = self.hook_wants(class, k) else {
            return Ok(false);
        };
        let mut buf = alloc::string::String::new();
        let Some(key) = self.hook_key(k, &mut buf) else {
            return Ok(false);
        };
        let mark = self.temps.len();
        self.temps.push(Val::Obj(id));
        self.temps.push(v);
        let r = c.set(
            self,
            crate::api::Value(Val::Obj(id)),
            data,
            key,
            crate::api::Value(v),
        );
        self.temps.truncate(mark);
        r
    }

    pub(crate) fn hook_has(
        &mut self,
        id: ObjId,
        class: u32,
        data: u64,
        k: PropKey,
    ) -> Res<Option<bool>> {
        let Some(c) = self.hook_wants(class, k) else {
            return Ok(None);
        };
        let mut buf = alloc::string::String::new();
        let Some(key) = self.hook_key(k, &mut buf) else {
            return Ok(None);
        };
        let mark = self.temps.len();
        self.temps.push(Val::Obj(id));
        let r = c.has(self, crate::api::Value(Val::Obj(id)), data, key);
        self.temps.truncate(mark);
        match r? {
            Some(true) => Ok(Some(true)),
            // Not one of the host's: the ordinary properties still count.
            _ => Ok(if self.get_own_plain(id, k).is_some() {
                Some(true)
            } else {
                None
            }),
        }
    }

    pub(crate) fn hook_delete(
        &mut self,
        id: ObjId,
        class: u32,
        data: u64,
        k: PropKey,
    ) -> Res<Option<bool>> {
        let Some(c) = self.hook_wants(class, k) else {
            return Ok(None);
        };
        let mut buf = alloc::string::String::new();
        let Some(key) = self.hook_key(k, &mut buf) else {
            return Ok(None);
        };
        let mark = self.temps.len();
        self.temps.push(Val::Obj(id));
        let r = c.delete(self, crate::api::Value(Val::Obj(id)), data, key);
        self.temps.truncate(mark);
        r
    }

    pub(crate) fn hook_keys(&mut self, id: ObjId, class: u32, data: u64) -> Res<Vec<PropKey>> {
        let Some(c) = self.classes.get(class as usize) else {
            return Ok(Vec::new());
        };
        if c.hooks & (crate::api::hooks::INDEXED | crate::api::hooks::NAMED) == 0 {
            return Ok(Vec::new());
        }
        let c = c.class.clone();
        let mark = self.temps.len();
        self.temps.push(Val::Obj(id));
        let r = c.keys(self, crate::api::Value(Val::Obj(id)), data);
        self.temps.truncate(mark);
        let mut out = Vec::new();
        for k in r? {
            out.push(match k {
                crate::api::OwnedKey::Index(i) => PropKey::Idx(i),
                crate::api::OwnedKey::Str(s) => self.key_str(&s),
            });
        }
        Ok(out)
    }
}

fn receiver_is(receiver: Val, id: ObjId) -> bool {
    matches!(receiver, Val::Obj(r) if r == id)
}
