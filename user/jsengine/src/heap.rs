//! The heap: arenas of objects and strings addressed by index + generation, the intern table,
//! memory accounting, and a precise mark-sweep collector.
//!
//! The collector runs only at the VM's safe points (see `Realm::maybe_gc`), never in the middle
//! of a builtin, so Rust code that holds a `Val` in a local is safe until it calls back into
//! script. Builtins that do call back into script while holding values root them on
//! `Realm::temps` first. Roots: the realm's intrinsics, interned engine names, the global
//! object and environment, the VM stack and frames, temporaries, pins and the job queue.
//! WeakMap/WeakSet entries are ephemerons (a value is kept only while its key is).

use alloc::vec::Vec;

use crate::object::{Obj, ObjKind};
use crate::string::{FlatStr, JsStr, hash_units};
use crate::value::{ObjId, StrId, SymId, Val};

pub(crate) struct ObjSlot {
    pub g: u32,
    pub mark: bool,
    pub obj: Obj,
}

pub(crate) struct StrSlot {
    pub g: u32,
    pub mark: bool,
    pub free: bool,
    pub s: JsStr,
}

pub(crate) struct SymData {
    pub desc: Option<StrId>,
    /// Symbol.for key, if registered.
    pub registered: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum GcRef {
    O(u32),
    S(u32),
}

/// Gathers the references a heap thing holds, for marking.
pub(crate) struct Tracer {
    pub stack: Vec<GcRef>,
}

impl Tracer {
    #[inline]
    pub fn val(&mut self, v: Val) {
        match v {
            Val::Obj(o) => self.stack.push(GcRef::O(o.idx)),
            Val::Str(s) => self.stack.push(GcRef::S(s.idx)),
            _ => {}
        }
    }
    #[inline]
    pub fn obj(&mut self, o: ObjId) {
        self.stack.push(GcRef::O(o.idx));
    }
    #[inline]
    pub fn opt_obj(&mut self, o: Option<ObjId>) {
        if let Some(o) = o {
            self.stack.push(GcRef::O(o.idx));
        }
    }
    #[inline]
    pub fn str(&mut self, s: StrId) {
        self.stack.push(GcRef::S(s.idx));
    }
}

pub(crate) struct Heap {
    pub objs: Vec<ObjSlot>,
    free_objs: Vec<u32>,
    pub strs: Vec<StrSlot>,
    free_strs: Vec<u32>,
    pub syms: Vec<SymData>,
    /// Open addressing over interned string indices (u32::MAX = empty).
    intern: Vec<u32>,
    intern_count: usize,
    /// Estimated bytes live after the last collection, and allocated since.
    pub live_bytes: usize,
    pub since_gc: usize,
    pub gc_threshold: usize,
    pub max_bytes: usize,
    pub gc_wanted: bool,
    pub gc_count: u64,
    /// Code-constant marking epoch (see `Code::marked`).
    pub epoch: u32,
}

const MIN_GC_THRESHOLD: usize = 2 << 20;

impl Heap {
    pub fn new(max_bytes: usize) -> Heap {
        Heap {
            objs: Vec::new(),
            free_objs: Vec::new(),
            strs: Vec::new(),
            free_strs: Vec::new(),
            syms: Vec::new(),
            intern: alloc::vec![u32::MAX; 1024],
            intern_count: 0,
            live_bytes: 0,
            since_gc: 0,
            gc_threshold: MIN_GC_THRESHOLD,
            max_bytes,
            gc_wanted: false,
            gc_count: 0,
            epoch: 1,
        }
    }

    #[inline]
    pub fn charge(&mut self, bytes: usize) {
        self.since_gc = self.since_gc.saturating_add(bytes);
        if self.since_gc > self.gc_threshold {
            self.gc_wanted = true;
        }
    }

    /// Bytes in use as far as we know (live at the last GC plus everything since).
    pub fn used(&self) -> usize {
        self.live_bytes.saturating_add(self.since_gc)
    }

    /// True when even garbage can't explain the usage any more: builtins that allocate in a
    /// loop check this and stop with an out-of-memory termination.
    #[inline]
    pub fn over_hard_limit(&self) -> bool {
        self.used() > self.max_bytes.saturating_mul(2)
    }

    /// Whether `bytes` more could ever fit (a single allocation larger than what's left after
    /// the last collection can't succeed even after another one).
    pub fn could_fit(&self, bytes: usize) -> bool {
        self.live_bytes.saturating_add(bytes) <= self.max_bytes
    }

    // ---- objects ----

    pub fn alloc_obj(&mut self, obj: Obj) -> ObjId {
        let size = obj.size_estimate();
        self.charge(size);
        if let Some(i) = self.free_objs.pop() {
            let slot = &mut self.objs[i as usize];
            slot.obj = obj;
            slot.mark = false;
            ObjId { idx: i, g: slot.g }
        } else {
            let i = self.objs.len() as u32;
            self.objs.push(ObjSlot {
                g: 1,
                mark: false,
                obj,
            });
            ObjId { idx: i, g: 1 }
        }
    }

    #[inline]
    pub fn o(&self, id: ObjId) -> &Obj {
        &self.objs[id.idx as usize].obj
    }
    #[inline]
    pub fn om(&mut self, id: ObjId) -> &mut Obj {
        &mut self.objs[id.idx as usize].obj
    }
    /// Whether a handle still names a live object (for handles from outside the engine).
    pub fn obj_valid(&self, id: ObjId) -> bool {
        match self.objs.get(id.idx as usize) {
            Some(s) => s.g == id.g && !matches!(s.obj.kind, ObjKind::Dead),
            None => false,
        }
    }

    // ---- strings ----

    pub fn alloc_str(&mut self, s: JsStr) -> StrId {
        self.charge(s.bytes());
        if let Some(i) = self.free_strs.pop() {
            let slot = &mut self.strs[i as usize];
            slot.s = s;
            slot.mark = false;
            slot.free = false;
            StrId { idx: i, g: slot.g }
        } else {
            let i = self.strs.len() as u32;
            self.strs.push(StrSlot {
                g: 1,
                mark: false,
                free: false,
                s,
            });
            StrId { idx: i, g: 1 }
        }
    }

    pub fn new_str(&mut self, units: &[u16]) -> StrId {
        self.alloc_str(JsStr::Flat(FlatStr::new(units.into())))
    }

    pub fn new_str_vec(&mut self, units: Vec<u16>) -> StrId {
        self.alloc_str(JsStr::Flat(FlatStr::new(units.into_boxed_slice())))
    }

    pub fn str_valid(&self, id: StrId) -> bool {
        matches!(self.strs.get(id.idx as usize), Some(s) if s.g == id.g && !s.free)
    }

    #[inline]
    pub fn str_len(&self, id: StrId) -> usize {
        self.strs[id.idx as usize].s.len()
    }

    /// The string's units, flattening a rope first.
    pub fn units(&mut self, id: StrId) -> &[u16] {
        self.flatten(id);
        match &self.strs[id.idx as usize].s {
            JsStr::Flat(f) => &f.units,
            JsStr::Rope { .. } => &[],
        }
    }

    /// The units of a string known to be flat (interned names are), else empty.
    #[inline]
    pub fn flat(&self, id: StrId) -> &[u16] {
        match &self.strs[id.idx as usize].s {
            JsStr::Flat(f) => &f.units,
            JsStr::Rope { .. } => &[],
        }
    }

    pub fn is_flat(&self, id: StrId) -> bool {
        matches!(self.strs[id.idx as usize].s, JsStr::Flat(_))
    }

    pub fn flatten(&mut self, id: StrId) {
        let len = match &self.strs[id.idx as usize].s {
            JsStr::Flat(_) => return,
            JsStr::Rope { len, .. } => *len as usize,
        };
        let mut out: Vec<u16> = Vec::with_capacity(len);
        // Iterative in-order walk: ropes built by `s += x` are as deep as they are long.
        let mut stack: Vec<StrId> = alloc::vec![id];
        while let Some(s) = stack.pop() {
            match &self.strs[s.idx as usize].s {
                JsStr::Flat(f) => out.extend_from_slice(&f.units),
                JsStr::Rope { l, r, .. } => {
                    stack.push(*r);
                    stack.push(*l);
                }
            }
        }
        self.strs[id.idx as usize].s = JsStr::Flat(FlatStr::new(out.into_boxed_slice()));
    }

    pub fn str_hash(&mut self, id: StrId) -> u32 {
        self.flatten(id);
        match &self.strs[id.idx as usize].s {
            JsStr::Flat(f) => f.hash(),
            JsStr::Rope { .. } => 0,
        }
    }

    pub fn str_eq(&mut self, a: StrId, b: StrId) -> bool {
        if a == b {
            return true;
        }
        if self.str_len(a) != self.str_len(b) {
            return false;
        }
        self.flatten(a);
        self.flatten(b);
        if let (JsStr::Flat(fa), JsStr::Flat(fb)) =
            (&self.strs[a.idx as usize].s, &self.strs[b.idx as usize].s)
        {
            if fa.interned && fb.interned {
                return false;
            }
            fa.units == fb.units
        } else {
            false
        }
    }

    pub fn is_interned(&self, id: StrId) -> bool {
        matches!(&self.strs[id.idx as usize].s, JsStr::Flat(f) if f.interned)
    }

    // ---- interning ----

    fn intern_find(&self, units: &[u16], h: u32) -> Result<StrId, usize> {
        let mask = self.intern.len() - 1;
        let mut i = h as usize & mask;
        loop {
            let e = self.intern[i];
            if e == u32::MAX {
                return Err(i);
            }
            let slot = &self.strs[e as usize];
            if let JsStr::Flat(f) = &slot.s
                && f.hash() == h
                && &*f.units == units
            {
                return Ok(StrId { idx: e, g: slot.g });
            }
            i = (i + 1) & mask;
        }
    }

    fn intern_grow(&mut self) {
        let n = self.intern.len() * 2;
        let old = core::mem::replace(&mut self.intern, alloc::vec![u32::MAX; n]);
        for e in old {
            if e != u32::MAX {
                self.intern_insert_raw(e);
            }
        }
    }

    fn intern_insert_raw(&mut self, e: u32) {
        let h = match &self.strs[e as usize].s {
            JsStr::Flat(f) => f.hash(),
            JsStr::Rope { .. } => return,
        };
        let mask = self.intern.len() - 1;
        let mut i = h as usize & mask;
        while self.intern[i] != u32::MAX {
            i = (i + 1) & mask;
        }
        self.intern[i] = e;
    }

    pub fn intern(&mut self, units: &[u16]) -> StrId {
        let h = hash_units(units);
        match self.intern_find(units, h) {
            Ok(id) => id,
            Err(_) => {
                let mut f = FlatStr::new(units.into());
                f.interned = true;
                f.hash.set(h);
                let id = self.alloc_str(JsStr::Flat(f));
                self.intern_add(id.idx);
                id
            }
        }
    }

    fn intern_add(&mut self, idx: u32) {
        if (self.intern_count + 1) * 2 > self.intern.len() {
            self.intern_grow();
        }
        self.intern_insert_raw(idx);
        self.intern_count += 1;
    }

    pub fn intern_str(&mut self, s: &str) -> StrId {
        let u: Vec<u16> = s.encode_utf16().collect();
        self.intern(&u)
    }

    /// The interned string with the same content as `id` (which may become it).
    pub fn intern_id(&mut self, id: StrId) -> StrId {
        self.flatten(id);
        let h = self.str_hash(id);
        let found = {
            let units = self.flat(id);
            self.intern_find(units, h)
        };
        match found {
            Ok(s) => s,
            Err(_) => {
                if let JsStr::Flat(f) = &mut self.strs[id.idx as usize].s {
                    f.interned = true;
                }
                self.intern_add(id.idx);
                id
            }
        }
    }

    /// Looks up an interned string without creating one.
    pub fn find_interned(&self, units: &[u16]) -> Option<StrId> {
        self.intern_find(units, hash_units(units)).ok()
    }

    // ---- symbols ----

    pub fn new_sym(&mut self, desc: Option<StrId>) -> SymId {
        self.charge(24);
        self.syms.push(SymData {
            desc,
            registered: false,
        });
        SymId(self.syms.len() as u32 - 1)
    }

    // ---- collection ----

    /// Marks everything reachable from `roots` (already pushed on the tracer), then frees the
    /// rest. Returns how many objects were freed, with the finalizer data of host objects
    /// that died in `dead_hosts`.
    pub fn collect(&mut self, t: &mut Tracer, dead_hosts: &mut Vec<(u32, u64)>) {
        // (the realm traced its roots with epoch + 1 already)
        self.epoch = self.epoch.wrapping_add(1).max(1);
        // Symbols' descriptions are always live (symbols are never freed).
        for s in &self.syms {
            if let Some(d) = s.desc {
                t.stack.push(GcRef::S(d.idx));
            }
        }
        let mut weak: Vec<u32> = Vec::new();
        self.drain_marks(t, &mut weak);
        // Ephemerons: keep a weak map's value while its key lives, to a fixed point.
        loop {
            let mut more = false;
            for &w in &weak {
                let obj = &self.objs[w as usize].obj;
                if let ObjKind::WeakMap(m) = &obj.kind {
                    for (k, v) in m.iter() {
                        let km = self
                            .objs
                            .get(k.idx as usize)
                            .is_some_and(|s| s.mark && s.g == k.g);
                        if km {
                            match *v {
                                Val::Obj(o) if !self.objs[o.idx as usize].mark => {
                                    t.stack.push(GcRef::O(o.idx));
                                    more = true;
                                }
                                Val::Str(s) if !self.strs[s.idx as usize].mark => {
                                    t.stack.push(GcRef::S(s.idx));
                                    more = true;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            if !more {
                break;
            }
            self.drain_marks(t, &mut weak);
        }
        // Drop dead keys from weak collections.
        let mut dead_keys: Vec<(u32, ObjId)> = Vec::new();
        for &w in &weak {
            if let ObjKind::WeakMap(m) = &self.objs[w as usize].obj.kind {
                for (k, _) in m.iter() {
                    let alive = self
                        .objs
                        .get(k.idx as usize)
                        .is_some_and(|s| s.mark && s.g == k.g);
                    if !alive {
                        dead_keys.push((w, *k));
                    }
                }
            }
        }
        for (w, k) in dead_keys {
            if let ObjKind::WeakMap(m) = &mut self.objs[w as usize].obj.kind {
                m.remove(k);
            }
        }
        // Sweep objects.
        let mut live = 0usize;
        for (i, slot) in self.objs.iter_mut().enumerate() {
            if slot.mark {
                slot.mark = false;
                live += slot.obj.size_estimate();
            } else if !matches!(slot.obj.kind, ObjKind::Dead) {
                if let ObjKind::Host { class, data } = slot.obj.kind {
                    dead_hosts.push((class, data));
                }
                slot.obj = Obj::dead();
                slot.g = slot.g.wrapping_add(1).max(1);
                self.free_objs.push(i as u32);
            }
        }
        // Sweep strings.
        let mut any_interned_died = false;
        for (i, slot) in self.strs.iter_mut().enumerate() {
            if slot.mark {
                slot.mark = false;
                live += slot.s.bytes();
            } else if !slot.free {
                if let JsStr::Flat(f) = &slot.s
                    && f.interned
                {
                    any_interned_died = true;
                }
                slot.s = JsStr::Flat(FlatStr::new(alloc::boxed::Box::new([])));
                slot.g = slot.g.wrapping_add(1).max(1);
                slot.free = true;
                self.free_strs.push(i as u32);
            }
        }
        if any_interned_died {
            let n = self.intern.len();
            self.intern = alloc::vec![u32::MAX; n];
            self.intern_count = 0;
            let ids: Vec<u32> = self
                .strs
                .iter()
                .enumerate()
                .filter(|(_, s)| matches!(&s.s, JsStr::Flat(f) if f.interned))
                .map(|(i, _)| i as u32)
                .collect();
            for i in ids {
                self.intern_add(i);
            }
        }
        live += syms_bytes(self.syms.len());
        live += (self.objs.capacity() * core::mem::size_of::<ObjSlot>())
            + (self.strs.capacity() * core::mem::size_of::<StrSlot>());
        self.live_bytes = live;
        self.since_gc = 0;
        self.gc_threshold = live.max(MIN_GC_THRESHOLD);
        if live.saturating_add(self.gc_threshold) > self.max_bytes {
            self.gc_threshold = self.max_bytes.saturating_sub(live).max(256 << 10);
        }
        self.gc_wanted = false;
        self.gc_count += 1;
    }

    fn drain_marks(&mut self, t: &mut Tracer, weak: &mut Vec<u32>) {
        while let Some(r) = t.stack.pop() {
            match r {
                GcRef::O(i) => {
                    let Some(slot) = self.objs.get_mut(i as usize) else {
                        continue;
                    };
                    if slot.mark {
                        continue;
                    }
                    slot.mark = true;
                    if matches!(slot.obj.kind, ObjKind::WeakMap(_)) {
                        weak.push(i);
                    }
                    let epoch = self.epoch;
                    self.objs[i as usize].obj.trace(t, epoch);
                }
                GcRef::S(i) => {
                    let Some(slot) = self.strs.get_mut(i as usize) else {
                        continue;
                    };
                    if slot.mark {
                        continue;
                    }
                    slot.mark = true;
                    if let JsStr::Rope { l, r, .. } = slot.s {
                        t.stack.push(GcRef::S(l.idx));
                        t.stack.push(GcRef::S(r.idx));
                    }
                }
            }
        }
    }

    pub fn object_count(&self) -> usize {
        self.objs.len() - self.free_objs.len()
    }
    pub fn string_count(&self) -> usize {
        self.strs.len() - self.free_strs.len()
    }
}

fn syms_bytes(n: usize) -> usize {
    n * 24
}
