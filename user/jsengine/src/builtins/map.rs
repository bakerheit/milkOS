//! Map, Set, WeakMap, WeakSet and the Map/Set iterators.
//!
//! Keys are canonicalised so equality is a bit comparison: strings are interned, numbers
//! normalised (integers as Int, -0 as 0, one NaN). Entries keep insertion order; deleted
//! ones become holes, compacted now and then, with live iterators remapped.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::heap::Tracer;
use crate::object::{ObjKind, PropKey, WC, WeakMapData};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

use super::arg;

pub(crate) struct MapData {
    /// (key, value); a deleted entry's key is Empty.
    ents: Vec<(Val, Val)>,
    /// Open addressing over entry positions + 1 (0 = empty slot).
    index: Vec<u32>,
    live: usize,
    /// Iterators over this collection (weak: checked when compacting).
    iters: Vec<ObjId>,
}

pub(crate) struct MapIterData {
    pub coll: ObjId,
    pub pos: usize,
    /// 0 keys, 1 values, 2 entries.
    pub kind: u8,
    pub done: bool,
}

fn key_bits(v: Val) -> (u8, u64) {
    match v {
        Val::Undef | Val::Empty => (0, 0),
        Val::Null => (1, 0),
        Val::Bool(b) => (2, b as u64),
        Val::Int(i) => (3, i as u32 as u64),
        Val::Num(n) => (
            4,
            if n.is_nan() {
                0x7ff8000000000000
            } else {
                n.to_bits()
            },
        ),
        Val::Str(s) => (5, ((s.g as u64) << 32) | s.idx as u64),
        Val::Sym(s) => (6, s.0 as u64),
        Val::Obj(o) => (7, ((o.g as u64) << 32) | o.idx as u64),
    }
}

fn same(a: Val, b: Val) -> bool {
    key_bits(a) == key_bits(b)
}

fn hash(v: Val) -> u32 {
    let (t, b) = key_bits(v);
    let x = b ^ ((t as u64) << 59);
    let x = x.wrapping_mul(0x9E3779B97F4A7C15);
    (x >> 32) as u32
}

impl MapData {
    pub fn new() -> MapData {
        MapData {
            ents: Vec::new(),
            index: Vec::new(),
            live: 0,
            iters: Vec::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.live
    }
    pub fn bytes(&self) -> usize {
        48 + self.ents.capacity() * 32 + self.index.capacity() * 4
    }
    pub fn trace(&self, t: &mut Tracer) {
        for (k, v) in &self.ents {
            t.val(*k);
            t.val(*v);
        }
    }
    fn find(&self, k: Val) -> Option<usize> {
        if self.index.is_empty() {
            return self
                .ents
                .iter()
                .position(|e| !e.0.is_empty() && same(e.0, k));
        }
        let mask = self.index.len() - 1;
        let mut i = hash(k) as usize & mask;
        loop {
            let e = self.index[i];
            if e == 0 {
                return None;
            }
            let p = (e - 1) as usize;
            if let Some(ent) = self.ents.get(p)
                && !ent.0.is_empty()
                && same(ent.0, k)
            {
                return Some(p);
            }
            i = (i + 1) & mask;
        }
    }
    fn rebuild_index(&mut self) {
        if self.ents.len() < 8 {
            self.index.clear();
            return;
        }
        let n = (self.ents.len() * 2).next_power_of_two().max(16);
        self.index = alloc::vec![0; n];
        let mask = n - 1;
        for (p, e) in self.ents.iter().enumerate() {
            if e.0.is_empty() {
                continue;
            }
            let mut i = hash(e.0) as usize & mask;
            while self.index[i] != 0 {
                i = (i + 1) & mask;
            }
            self.index[i] = p as u32 + 1;
        }
    }
    fn insert_new(&mut self, k: Val, v: Val) {
        let p = self.ents.len();
        self.ents.push((k, v));
        self.live += 1;
        if self.index.is_empty() {
            if self.ents.len() >= 8 {
                self.rebuild_index();
            }
            return;
        }
        if self.ents.len() * 2 > self.index.len() {
            self.rebuild_index();
            return;
        }
        let mask = self.index.len() - 1;
        let mut i = hash(k) as usize & mask;
        while self.index[i] != 0 {
            i = (i + 1) & mask;
        }
        self.index[i] = p as u32 + 1;
    }
}

impl WeakMapData {
    pub fn index_get(&self, k: ObjId) -> Option<Val> {
        self.get(k)
    }
}

impl Realm {
    /// The canonical form of a key.
    fn canon_key(&mut self, k: Val) -> Val {
        match k {
            Val::Num(n) => {
                if n == 0.0 {
                    Val::Int(0)
                } else {
                    Val::num(n)
                }
            }
            Val::Str(s) => Val::Str(self.heap.intern_id(s)),
            Val::Empty => Val::Undef,
            v => v,
        }
    }

    fn map_data(&mut self, o: ObjId) -> Option<&mut MapData> {
        match &mut self.om(o).kind {
            ObjKind::Map(m) | ObjKind::Set(m) => Some(m),
            _ => None,
        }
    }

    pub(crate) fn map_get(&mut self, o: ObjId, k: Val) -> Option<Val> {
        let k = self.canon_key(k);
        let m = self.map_data(o)?;
        m.find(k).map(|p| m.ents[p].1)
    }

    pub(crate) fn map_set(&mut self, o: ObjId, k: Val, v: Val) {
        let k = self.canon_key(k);
        let Some(m) = self.map_data(o) else { return };
        match m.find(k) {
            Some(p) => m.ents[p].1 = v,
            None => {
                m.insert_new(k, v);
                self.heap.charge(40);
            }
        }
    }

    pub(crate) fn map_has(&mut self, o: ObjId, k: Val) -> bool {
        let k = self.canon_key(k);
        self.map_data(o).is_some_and(|m| m.find(k).is_some())
    }

    pub(crate) fn map_delete(&mut self, o: ObjId, k: Val) -> bool {
        let k = self.canon_key(k);
        let Some(m) = self.map_data(o) else {
            return false;
        };
        let Some(p) = m.find(k) else { return false };
        m.ents[p] = (Val::Empty, Val::Undef);
        m.live -= 1;
        let holes = m.ents.len() - m.live;
        if holes > 64 && holes > m.live * 2 {
            self.map_compact(o);
        }
        true
    }

    fn map_compact(&mut self, o: ObjId) {
        let (mapping, iters) = {
            let Some(m) = self.map_data(o) else { return };
            // new position of each old position (the count of live entries before it)
            let mut mapping = Vec::with_capacity(m.ents.len() + 1);
            let mut n = 0usize;
            for e in &m.ents {
                mapping.push(n);
                if !e.0.is_empty() {
                    n += 1;
                }
            }
            mapping.push(n);
            m.ents.retain(|e| !e.0.is_empty());
            m.rebuild_index();
            (mapping, core::mem::take(&mut m.iters))
        };
        let mut keep = Vec::new();
        for it in iters {
            if !self.heap.obj_valid(it) {
                continue;
            }
            if let ObjKind::MapIter(d) = &mut self.om(it).kind
                && d.coll == o
                && !d.done
            {
                d.pos = mapping.get(d.pos).copied().unwrap_or(d.pos);
                keep.push(it);
            }
        }
        if let Some(m) = self.map_data(o) {
            m.iters = keep;
        }
    }

    fn map_clear(&mut self, o: ObjId) {
        let iters = match self.map_data(o) {
            Some(m) => {
                m.ents.clear();
                m.index.clear();
                m.live = 0;
                core::mem::take(&mut m.iters)
            }
            None => return,
        };
        for it in iters {
            if self.heap.obj_valid(it)
                && let ObjKind::MapIter(d) = &mut self.om(it).kind
                && d.coll == o
            {
                d.pos = 0;
            }
        }
    }

    fn map_iter(&mut self, o: ObjId, kind: u8, is_set: bool) -> Val {
        let p = if is_set {
            self.intr.set_iterator_proto
        } else {
            self.intr.map_iterator_proto
        };
        let it = self.alloc(
            Some(p),
            ObjKind::MapIter(Box::new(MapIterData {
                coll: o,
                pos: 0,
                kind,
                done: false,
            })),
        );
        if let Some(m) = self.map_data(o) {
            // Drop registrations of iterators that died, now and then.
            if m.iters.len() > 64 && m.iters.len().is_power_of_two() {
                let iters = core::mem::take(&mut m.iters);
                let live: Vec<ObjId> = iters
                    .into_iter()
                    .filter(|i| self.heap.obj_valid(*i))
                    .collect();
                if let Some(m) = self.map_data(o) {
                    m.iters = live;
                }
            }
            if let Some(m) = self.map_data(o) {
                m.iters.push(it);
            }
        }
        Val::Obj(it)
    }

    /// The next (key, value) of a Map/Set iterator.
    pub(crate) fn map_iter_next(&mut self, it: ObjId) -> Option<(Val, Val, u8)> {
        let (coll, pos, kind, done) = match &self.o(it).kind {
            ObjKind::MapIter(d) => (d.coll, d.pos, d.kind, d.done),
            _ => return None,
        };
        if done {
            return None;
        }
        let mut p = pos;
        let found = match &self.o(coll).kind {
            ObjKind::Map(m) | ObjKind::Set(m) => loop {
                match m.ents.get(p) {
                    None => break None,
                    Some(e) if e.0.is_empty() => p += 1,
                    Some(e) => break Some(*e),
                }
            },
            _ => None,
        };
        if let ObjKind::MapIter(d) = &mut self.om(it).kind {
            match found {
                Some(_) => d.pos = p + 1,
                None => d.done = true,
            }
        }
        found.map(|(k, v)| (k, v, kind))
    }

    fn this_map(&mut self, this: Val, set: bool, what: &str) -> Res<ObjId> {
        if let Val::Obj(o) = this {
            let ok = match self.o(o).kind {
                ObjKind::Map(_) => !set,
                ObjKind::Set(_) => set,
                _ => false,
            };
            if ok {
                return Ok(o);
            }
        }
        let t = if set { "Set" } else { "Map" };
        Err(self.type_error(&alloc::format!(
            "Method {t}.prototype.{what} called on incompatible receiver"
        )))
    }

    fn this_weak(&mut self, this: Val, set: bool, what: &str) -> Res<ObjId> {
        if let Val::Obj(o) = this
            && let ObjKind::WeakMap(w) = &self.o(o).kind
            && w.is_set == set
        {
            return Ok(o);
        }
        let t = if set { "WeakSet" } else { "WeakMap" };
        Err(self.type_error(&alloc::format!(
            "Method {t}.prototype.{what} called on incompatible receiver"
        )))
    }
}

fn collection_ctor(r: &mut Realm, args: &[Val], set: bool) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        let n = if set { "Set" } else { "Map" };
        return Err(r.type_error(&alloc::format!("Constructor {n} requires 'new'")));
    }
    let proto = r.proto_from_ctor(nt, |r| {
        if set {
            r.intr.set_proto
        } else {
            r.intr.map_proto
        }
    })?;
    let kind = if set {
        ObjKind::Set(Box::new(MapData::new()))
    } else {
        ObjKind::Map(Box::new(MapData::new()))
    };
    let o = r.alloc(Some(proto), kind);
    let it = arg(args, 0);
    if it.is_nullish() {
        return Ok(Val::Obj(o));
    }
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    let res: Res<()> = (|| {
        let adder_name = if set { "add" } else { "set" };
        let ak = r.key_str(adder_name);
        let adder = r.get_p(o, ak, Val::Obj(o))?;
        if !r.callable(adder) {
            return Err(r.type_error("'add'/'set' is not a function"));
        }
        let items = r.iterate_to_vec(it)?;
        r.temps.extend_from_slice(&items);
        for item in items {
            if set {
                r.call_v(adder, Val::Obj(o), &[item])?;
            } else {
                if !matches!(item, Val::Obj(_)) {
                    let d = r.short_display(item);
                    return Err(
                        r.type_error(&alloc::format!("Iterator value {d} is not an entry object"))
                    );
                }
                let k = r.get_elem(item, Val::Int(0))?;
                let v = r.get_elem(item, Val::Int(1))?;
                r.call_v(adder, Val::Obj(o), &[k, v])?;
            }
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(o))
}

fn map_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    collection_ctor(r, args, false)
}
fn set_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    collection_ctor(r, args, true)
}

fn m_get(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "get")?;
    Ok(r.map_get(o, arg(args, 0)).unwrap_or(Val::Undef))
}
fn m_set(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "set")?;
    r.map_set(o, arg(args, 0), arg(args, 1));
    Ok(this)
}
fn m_has(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "has")?;
    Ok(Val::Bool(r.map_has(o, arg(args, 0))))
}
fn m_delete(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "delete")?;
    Ok(Val::Bool(r.map_delete(o, arg(args, 0))))
}
fn m_clear(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "clear")?;
    r.map_clear(o);
    Ok(Val::Undef)
}
fn m_size(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "size")?;
    Ok(Val::num(r.map_data(o).map_or(0, |m| m.len()) as f64))
}
fn m_for_each(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let set = matches!(this, Val::Obj(o) if matches!(r.o(o).kind, ObjKind::Set(_)));
    let o = r.this_map(this, set, "forEach")?;
    let f = arg(args, 0);
    if !r.callable(f) {
        let d = r.short_display(f);
        return Err(r.type_error(&alloc::format!("{d} is not a function")));
    }
    let t = arg(args, 1);
    let it = r.map_iter(o, 2, set);
    let mark = r.temps.len();
    r.temps.push(it);
    let Val::Obj(io) = it else {
        return Ok(Val::Undef);
    };
    let res: Res<()> = (|| {
        while let Some((k, v, _)) = r.map_iter_next(io) {
            if set {
                r.call_v(f, t, &[k, k, this])?;
            } else {
                r.call_v(f, t, &[v, k, this])?;
            }
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Undef)
}
fn m_entries(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "entries")?;
    Ok(r.map_iter(o, 2, false))
}
fn m_keys(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "keys")?;
    Ok(r.map_iter(o, 0, false))
}
fn m_values(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, false, "values")?;
    Ok(r.map_iter(o, 1, false))
}

fn s_add(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, true, "add")?;
    let k = arg(args, 0);
    r.map_set(o, k, k);
    Ok(this)
}
fn s_has(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, true, "has")?;
    Ok(Val::Bool(r.map_has(o, arg(args, 0))))
}
fn s_delete(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, true, "delete")?;
    Ok(Val::Bool(r.map_delete(o, arg(args, 0))))
}
fn s_clear(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, true, "clear")?;
    r.map_clear(o);
    Ok(Val::Undef)
}
fn s_size(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, true, "size")?;
    Ok(Val::num(r.map_data(o).map_or(0, |m| m.len()) as f64))
}
fn s_values(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, true, "values")?;
    Ok(r.map_iter(o, 1, true))
}
fn s_entries(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let o = r.this_map(this, true, "entries")?;
    Ok(r.map_iter(o, 2, true))
}

fn iter_next(r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    let Val::Obj(io) = this else {
        return Err(r.type_error("next called on incompatible receiver"));
    };
    if !matches!(r.o(io).kind, ObjKind::MapIter(_)) {
        return Err(r.type_error("next called on incompatible receiver"));
    }
    match r.map_iter_next(io) {
        None => Ok(r.iter_result(Val::Undef, true)),
        Some((k, v, kind)) => {
            let out = match kind {
                0 => k,
                1 => v,
                _ => Val::Obj(r.new_array_from(alloc::vec![k, v])),
            };
            Ok(r.iter_result(out, false))
        }
    }
}

// ---- weak collections ----

fn weak_ctor(r: &mut Realm, args: &[Val], set: bool) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        let n = if set { "WeakSet" } else { "WeakMap" };
        return Err(r.type_error(&alloc::format!("Constructor {n} requires 'new'")));
    }
    let proto = r.proto_from_ctor(nt, |r| {
        if set {
            r.intr.weakset_proto
        } else {
            r.intr.weakmap_proto
        }
    })?;
    let mut data = WeakMapData::default();
    data.is_set = set;
    let o = r.alloc(Some(proto), ObjKind::WeakMap(Box::new(data)));
    let it = arg(args, 0);
    if it.is_nullish() {
        return Ok(Val::Obj(o));
    }
    let mark = r.temps.len();
    r.temps.push(Val::Obj(o));
    let res: Res<()> = (|| {
        let items = r.iterate_to_vec(it)?;
        r.temps.extend_from_slice(&items);
        for item in items {
            if set {
                let Val::Obj(k) = item else {
                    return Err(r.type_error("Invalid value used in weak set"));
                };
                if let ObjKind::WeakMap(w) = &mut r.om(o).kind {
                    w.set(k, Val::Bool(true));
                }
            } else {
                let k = r.get_elem(item, Val::Int(0))?;
                let v = r.get_elem(item, Val::Int(1))?;
                let Val::Obj(k) = k else {
                    return Err(r.type_error("Invalid value used as weak map key"));
                };
                if let ObjKind::WeakMap(w) = &mut r.om(o).kind {
                    w.set(k, v);
                }
            }
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(o))
}

fn weakmap_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    weak_ctor(r, args, false)
}
fn weakset_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    weak_ctor(r, args, true)
}

fn wm_get(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_weak(this, false, "get")?;
    let Val::Obj(k) = arg(args, 0) else {
        return Ok(Val::Undef);
    };
    match &r.o(o).kind {
        ObjKind::WeakMap(w) => Ok(w.get(k).unwrap_or(Val::Undef)),
        _ => Ok(Val::Undef),
    }
}
fn wm_set(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_weak(this, false, "set")?;
    let Val::Obj(k) = arg(args, 0) else {
        return Err(r.type_error("Invalid value used as weak map key"));
    };
    let v = arg(args, 1);
    if let ObjKind::WeakMap(w) = &mut r.om(o).kind {
        w.set(k, v);
    }
    r.heap.charge(24);
    Ok(this)
}
fn wm_has(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let set =
        matches!(this, Val::Obj(o) if matches!(&r.o(o).kind, ObjKind::WeakMap(w) if w.is_set));
    let o = r.this_weak(this, set, "has")?;
    let Val::Obj(k) = arg(args, 0) else {
        return Ok(Val::Bool(false));
    };
    match &r.o(o).kind {
        ObjKind::WeakMap(w) => Ok(Val::Bool(w.get(k).is_some())),
        _ => Ok(Val::Bool(false)),
    }
}
fn wm_delete(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let set =
        matches!(this, Val::Obj(o) if matches!(&r.o(o).kind, ObjKind::WeakMap(w) if w.is_set));
    let o = r.this_weak(this, set, "delete")?;
    let Val::Obj(k) = arg(args, 0) else {
        return Ok(Val::Bool(false));
    };
    match &mut r.om(o).kind {
        ObjKind::WeakMap(w) => Ok(Val::Bool(w.remove(k))),
        _ => Ok(Val::Bool(false)),
    }
}
fn ws_add(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let o = r.this_weak(this, true, "add")?;
    let Val::Obj(k) = arg(args, 0) else {
        return Err(r.type_error("Invalid value used in weak set"));
    };
    if let ObjKind::WeakMap(w) = &mut r.om(o).kind {
        w.set(k, Val::Bool(true));
    }
    r.heap.charge(24);
    Ok(this)
}

fn species_getter(_r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(this)
}

pub(crate) fn init(r: &mut Realm) {
    let mp = r.intr.map_proto;
    let mc = r.ctor("Map", 0, map_ctor, mp);
    r.method(mp, "get", 1, m_get);
    r.method(mp, "set", 2, m_set);
    r.method(mp, "has", 1, m_has);
    r.method(mp, "delete", 1, m_delete);
    r.method(mp, "clear", 0, m_clear);
    r.method(mp, "forEach", 1, m_for_each);
    let me = r.method(mp, "entries", 0, m_entries);
    r.method(mp, "keys", 0, m_keys);
    r.method(mp, "values", 0, m_values);
    r.getter(mp, "size", m_size, None);
    let ik = PropKey::Sym(r.sym.iterator);
    r.put_prop(mp, ik, Val::Obj(me), WC);
    r.to_string_tag(mp, "Map");
    let sp = r.sym.species;
    r.getter_sym(mc, sp, "[Symbol.species]", species_getter);

    let stp = r.intr.set_proto;
    let sc = r.ctor("Set", 0, set_ctor, stp);
    r.method(stp, "add", 1, s_add);
    r.method(stp, "has", 1, s_has);
    r.method(stp, "delete", 1, s_delete);
    r.method(stp, "clear", 0, s_clear);
    r.method(stp, "forEach", 1, m_for_each);
    r.method(stp, "entries", 0, s_entries);
    let sv = r.method(stp, "values", 0, s_values);
    r.value(stp, "keys", Val::Obj(sv), WC);
    r.put_prop(stp, ik, Val::Obj(sv), WC);
    r.getter(stp, "size", s_size, None);
    r.to_string_tag(stp, "Set");
    r.getter_sym(sc, sp, "[Symbol.species]", species_getter);

    for (p, tag) in [
        (r.intr.map_iterator_proto, "Map Iterator"),
        (r.intr.set_iterator_proto, "Set Iterator"),
    ] {
        r.method(p, "next", 0, iter_next);
        r.to_string_tag(p, tag);
    }

    let wmp = r.intr.weakmap_proto;
    r.ctor("WeakMap", 0, weakmap_ctor, wmp);
    r.method(wmp, "get", 1, wm_get);
    r.method(wmp, "set", 2, wm_set);
    r.method(wmp, "has", 1, wm_has);
    r.method(wmp, "delete", 1, wm_delete);
    r.to_string_tag(wmp, "WeakMap");
    let wsp = r.intr.weakset_proto;
    r.ctor("WeakSet", 0, weakset_ctor, wsp);
    r.method(wsp, "add", 1, ws_add);
    r.method(wsp, "has", 1, wm_has);
    r.method(wsp, "delete", 1, wm_delete);
    r.to_string_tag(wsp, "WeakSet");
}
