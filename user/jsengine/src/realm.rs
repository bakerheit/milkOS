//! The realm: the heap, the intrinsics, the global object, the VM's stacks, the job queue,
//! limits and the embedder's state. Every internal algorithm is an `impl Realm` method.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

use crate::api::{
    Exc, Exception, HostClass, Intrinsic, JsResult, Key, Limits, Options, Termination, Value,
    WellKnown,
};
use crate::heap::{Heap, Tracer};
use crate::names::{N, NAME_STRS};
use crate::object::{
    CLASS_CTOR, CONSTRUCTOR, LAZY_FN, NativeData, NativeImpl, ObjKind, PropKey, W,
};
use crate::value::{ObjId, StrId, SymId, Val};

pub(crate) type Res<T> = Result<T, Exception>;

/// `unreachable!` without a panic: returns an internal TypeError.
macro_rules! unreachable_err {
    ($r:expr) => {
        return Err($r.type_error("internal error"))
    };
}

macro_rules! intrinsics {
    ($($f:ident),* $(,)?) => {
        pub(crate) struct Intrinsics { $(pub $f: ObjId),* }
        impl Intrinsics {
            fn placeholder() -> Intrinsics {
                Intrinsics { $($f: ObjId { idx: 0, g: 0 }),* }
            }
            pub fn trace(&self, t: &mut Tracer) {
                $( if self.$f.g != 0 { t.obj(self.$f); } )*
            }
        }
    };
}

intrinsics! {
    object_proto, function_proto, array_proto, string_proto, number_proto, boolean_proto,
    symbol_proto, error_proto, type_error_proto, range_error_proto, syntax_error_proto,
    reference_error_proto, uri_error_proto, eval_error_proto, aggregate_error_proto,
    iterator_proto, async_iterator_proto, array_iterator_proto, map_iterator_proto,
    set_iterator_proto, string_iterator_proto, regexp_string_iterator_proto, generator_proto,
    generator_function_proto, async_function_proto, async_generator_proto,
    async_generator_function_proto, async_from_sync_iterator_proto, promise_proto,
    regexp_proto, date_proto, map_proto, set_proto, weakmap_proto, weakset_proto,
    object_ctor, function_ctor, array_ctor, promise_ctor, error_ctor, regexp_ctor,
        array_values, array_for_each, throw_type_error, eval_fn, promise_then,
    generator_function_ctor, async_function_ctor, array_iter_next, regexp_exec,
    array_buffer_proto, array_buffer_ctor, data_view_proto, typed_array_proto, typed_array_ctor,
    int8_proto, uint8_proto, uint8c_proto, int16_proto, uint16_proto, int32_proto, uint32_proto,
    float32_proto, float64_proto,
    int8_ctor, uint8_ctor, uint8c_ctor, int16_ctor, uint16_ctor, int32_ctor, uint32_ctor,
    float32_ctor, float64_ctor,
    text_encoder_proto, text_decoder_proto,
}

impl Intrinsics {
    pub fn ta_proto(&self, k: crate::typed::TaKind) -> ObjId {
        use crate::typed::TaKind as K;
        match k {
            K::Int8 => self.int8_proto,
            K::Uint8 => self.uint8_proto,
            K::Uint8C => self.uint8c_proto,
            K::Int16 => self.int16_proto,
            K::Uint16 => self.uint16_proto,
            K::Int32 => self.int32_proto,
            K::Uint32 => self.uint32_proto,
            K::Float32 => self.float32_proto,
            K::Float64 => self.float64_proto,
        }
    }
    pub fn ta_ctor(&self, k: crate::typed::TaKind) -> ObjId {
        use crate::typed::TaKind as K;
        match k {
            K::Int8 => self.int8_ctor,
            K::Uint8 => self.uint8_ctor,
            K::Uint8C => self.uint8c_ctor,
            K::Int16 => self.int16_ctor,
            K::Uint16 => self.uint16_ctor,
            K::Int32 => self.int32_ctor,
            K::Uint32 => self.uint32_ctor,
            K::Float32 => self.float32_ctor,
            K::Float64 => self.float64_ctor,
        }
    }
    pub fn set_ta(&mut self, k: crate::typed::TaKind, proto: ObjId, ctor: ObjId) {
        use crate::typed::TaKind as K;
        match k {
            K::Int8 => (self.int8_proto, self.int8_ctor) = (proto, ctor),
            K::Uint8 => (self.uint8_proto, self.uint8_ctor) = (proto, ctor),
            K::Uint8C => (self.uint8c_proto, self.uint8c_ctor) = (proto, ctor),
            K::Int16 => (self.int16_proto, self.int16_ctor) = (proto, ctor),
            K::Uint16 => (self.uint16_proto, self.uint16_ctor) = (proto, ctor),
            K::Int32 => (self.int32_proto, self.int32_ctor) = (proto, ctor),
            K::Uint32 => (self.uint32_proto, self.uint32_ctor) = (proto, ctor),
            K::Float32 => (self.float32_proto, self.float32_ctor) = (proto, ctor),
            K::Float64 => (self.float64_proto, self.float64_ctor) = (proto, ctor),
        }
    }
}

pub(crate) struct ClassEntry {
    pub class: Rc<dyn HostClass>,
    pub hooks: u8,
}

pub(crate) struct NativeFrame {
    pub callee: ObjId,
    pub new_target: Val,
    pub temps_base: usize,
}

pub(crate) enum Job {
    Call {
        f: Val,
        this: Val,
        args: Vec<Val>,
    },
    /// A promise reaction (see builtins/promise.rs).
    Reaction {
        reaction: Val,
        arg: Val,
    },
    /// PromiseResolveThenableJob.
    Thenable {
        promise: Val,
        thenable: Val,
        then: Val,
    },
}

pub(crate) struct SymTable {
    pub iterator: SymId,
    pub async_iterator: SymId,
    pub has_instance: SymId,
    pub to_primitive: SymId,
    pub to_string_tag: SymId,
    pub species: SymId,
    pub is_concat_spreadable: SymId,
    pub unscopables: SymId,
    pub match_: SymId,
    pub match_all: SymId,
    pub replace: SymId,
    pub search: SymId,
    pub split: SymId,
}

/// Where the last throw happened (for reports of thrown non-errors).
#[derive(Clone, Default)]
pub(crate) struct Site {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub stack: String,
}

pub struct Realm {
    pub(crate) heap: Heap,
    pub(crate) names: Vec<StrId>,
    pub(crate) intr: Intrinsics,
    pub(crate) sym: SymTable,
    pub(crate) global: ObjId,
    /// Script-level let/const/class bindings (shared by every script): an object whose
    /// properties are the bindings (Empty = not initialized yet, non-writable = const).
    pub(crate) global_lex: ObjId,
    /// Arrays being joined (Array.prototype.join cycles join as "").
    pub(crate) join_stack: Vec<ObjId>,
    /// Single-unit strings 0..256, made once.
    pub(crate) char_cache: Vec<StrId>,
    pub(crate) temps: Vec<Val>,
    pub(crate) pins: Vec<Option<Val>>,
    pub(crate) free_pins: Vec<u32>,
    pub(crate) classes: Vec<ClassEntry>,
    pub(crate) jobs: VecDeque<Job>,
    pub(crate) limits: Limits,
    pub(crate) native_frames: Vec<NativeFrame>,
    pub(crate) host: Option<Box<dyn Any>>,
    pub(crate) interrupt: Option<Box<dyn FnMut() -> bool>>,
    pub(crate) clock: Option<Box<dyn Fn() -> f64>>,
    pub(crate) tz_offset_min: i32,
    pub(crate) rng: [u64; 2],
    /// VM steps in the current entry, and when to poll the interrupt next.
    pub(crate) steps: u64,
    pub(crate) next_poll: u64,
    /// Steps since the realm was made (never reset): what the interrupt polling counts.
    pub(crate) poll_steps: u64,
    /// Calls in progress (script and native).
    pub(crate) depth: u32,
    /// Entries from the host in progress (nested when a native calls the API).
    pub(crate) entries: u32,
    pub(crate) last_site: Site,
    /// Promises rejected with no handler (checked at the end of run_jobs).
    pub(crate) pending_rejections: Vec<ObjId>,
    /// Rejected promises nobody handled by the end of a microtask checkpoint: (promise, reason).
    pub(crate) unhandled: Vec<(Val, Val)>,
    /// Promises reported to the rejection hook as unhandled (weak), and the ones among
    /// them that got a handler since (to report as handled).
    pub(crate) reported: Vec<ObjId>,
    pub(crate) late_handled: Vec<ObjId>,
    pub(crate) rejection_hook: Option<Box<crate::api::RejectionHook>>,
    pub(crate) error_hook: Option<Box<crate::api::ErrorHook>>,
    pub(crate) modules: crate::module::Modules,
    pub(crate) import_meta_hook: Option<Box<crate::api::ImportMetaHook>>,
    pub(crate) dynamic_import_hook: Option<Box<crate::api::DynamicImportHook>>,
    /// Symbol.for registry.
    pub(crate) sym_registry: Vec<(StrId, SymId)>,
    pub(crate) dead_hosts: Vec<(u32, u64)>,
    /// The largest ArrayBuffer, in bytes (see `Realm::set_max_buffer_bytes`).
    pub(crate) max_buffer: usize,
    pub(crate) vm: crate::interp::VmState,
    /// The native stack guard of the current outermost entry.
    pub(crate) stack: crate::stack::StackGuard,
}

impl Realm {
    pub(crate) fn create(options: Options) -> Realm {
        let mut heap = Heap::new(options.limits.max_heap_bytes);
        let names: Vec<StrId> = NAME_STRS.iter().map(|s| heap.intern_str(s)).collect();
        let char_cache: Vec<StrId> = (0u16..256).map(|c| heap.intern(&[c])).collect();
        let mut mk = |d: &str| {
            let s = heap.intern_str(d);
            heap.new_sym(Some(s))
        };
        let sym = SymTable {
            iterator: mk("Symbol.iterator"),
            async_iterator: mk("Symbol.asyncIterator"),
            has_instance: mk("Symbol.hasInstance"),
            to_primitive: mk("Symbol.toPrimitive"),
            to_string_tag: mk("Symbol.toStringTag"),
            species: mk("Symbol.species"),
            is_concat_spreadable: mk("Symbol.isConcatSpreadable"),
            unscopables: mk("Symbol.unscopables"),
            match_: mk("Symbol.match"),
            match_all: mk("Symbol.matchAll"),
            replace: mk("Symbol.replace"),
            search: mk("Symbol.search"),
            split: mk("Symbol.split"),
        };
        let seed = options.random_seed;
        let mut r = Realm {
            heap,
            names,
            intr: Intrinsics::placeholder(),
            sym,
            global: ObjId { idx: 0, g: 0 },
            global_lex: ObjId { idx: 0, g: 0 },
            join_stack: Vec::new(),
            char_cache,
            temps: Vec::new(),
            pins: Vec::new(),
            free_pins: Vec::new(),
            classes: Vec::new(),
            jobs: VecDeque::new(),
            limits: options.limits,
            native_frames: Vec::new(),
            host: None,
            interrupt: None,
            clock: None,
            tz_offset_min: options.tz_offset_minutes,
            rng: [
                seed ^ 0x9E37_79B9_7F4A_7C15,
                seed.wrapping_mul(0xBF58_476D_1CE4_E5B9) | 1,
            ],
            steps: 0,
            next_poll: crate::api::INTERRUPT_STEPS,
            poll_steps: 0,
            depth: 0,
            entries: 0,
            last_site: Site::default(),
            pending_rejections: Vec::new(),
            unhandled: Vec::new(),
            reported: Vec::new(),
            late_handled: Vec::new(),
            rejection_hook: None,
            error_hook: None,
            modules: crate::module::Modules::new(),
            import_meta_hook: None,
            dynamic_import_hook: None,
            sym_registry: Vec::new(),
            dead_hosts: Vec::new(),
            max_buffer: crate::typed::DEFAULT_MAX_BUFFER,
            vm: crate::interp::VmState::new(),
            stack: crate::stack::StackGuard::new(crate::stack::DEFAULT_STACK_LIMIT),
        };
        crate::builtins::init(&mut r);
        r
    }

    // ---- entries from the host ----

    /// Wraps an entry from the host: the step budget starts over at the outermost one, and
    /// pending finalizers run after it.
    pub(crate) fn entry<T>(&mut self, f: impl FnOnce(&mut Realm) -> Res<T>) -> Res<T> {
        if self.entries == 0 && self.native_frames.is_empty() && self.vm.frames.is_empty() {
            self.steps = 0;
            self.stack.base = crate::stack::sp();
        }
        self.entries += 1;
        let r = f(self);
        self.entries -= 1;
        if self.entries == 0 && !self.dead_hosts.is_empty() {
            self.run_finalizers();
        }
        if let Err(Exception(Exc::Throw(v))) = &r {
            self.temps.push(*v);
        }
        r
    }

    fn run_finalizers(&mut self) {
        let dead = core::mem::take(&mut self.dead_hosts);
        for (class, data) in dead {
            if let Some(c) = self.classes.get(class as usize) {
                let c = c.class.clone();
                c.finalize(data);
            }
        }
    }

    pub(crate) fn out(&mut self, r: Res<Val>) -> JsResult<Value> {
        r.map(|v| self.out_val(v))
    }

    /// A value handed to the embedder: a temporary of the current scope.
    pub(crate) fn out_val(&mut self, v: Val) -> Value {
        let v = v.or_undef();
        if matches!(v, Val::Obj(_) | Val::Str(_)) {
            self.temps.push(v);
        }
        Value(v)
    }

    /// A value from the embedder: stale or foreign handles become undefined.
    pub(crate) fn check_val(&self, v: Val) -> Val {
        match v {
            Val::Obj(o) if !self.heap.obj_valid(o) => Val::Undef,
            Val::Str(s) if !self.heap.str_valid(s) => Val::Undef,
            Val::Sym(s) if s.0 as usize >= self.heap.syms.len() => Val::Undef,
            Val::Empty => Val::Undef,
            v => v,
        }
    }

    pub(crate) fn api_key(&mut self, k: Key<'_>) -> Res<PropKey> {
        Ok(match k {
            Key::Str(s) => self.key_str(s),
            Key::Index(i) if i != u32::MAX => PropKey::Idx(i),
            Key::Index(i) => {
                let u = crate::numconv::u32_to_units(i);
                PropKey::Str(self.heap.intern(&u))
            }
            Key::Value(v) => {
                let v = self.check_val(v.0);
                self.to_key(v)?
            }
        })
    }

    pub(crate) fn pin_val(&mut self, v: Val) -> u32 {
        if let Some(i) = self.free_pins.pop() {
            self.pins[i as usize] = Some(v);
            i
        } else {
            self.pins.push(Some(v));
            self.pins.len() as u32 - 1
        }
    }

    pub(crate) fn frames_empty(&self) -> bool {
        self.vm.frames.is_empty()
    }

    // ---- small helpers ----

    pub(crate) fn char_str(&mut self, c: u16) -> StrId {
        if (c as usize) < self.char_cache.len() {
            self.char_cache[c as usize]
        } else {
            self.heap.new_str(&[c])
        }
    }

    pub(crate) fn str_val(&mut self, s: &str) -> Val {
        Val::Str(self.heap.new_str_vec(crate::string::utf16(s)))
    }

    pub(crate) fn rust_string(&mut self, s: StrId) -> String {
        crate::string::to_rust(self.heap.units(s))
    }

    /// A key, for messages.
    pub(crate) fn key_display(&mut self, k: PropKey) -> String {
        match k {
            PropKey::Str(s) => self.rust_string(s),
            PropKey::Idx(i) => alloc::format!("{i}"),
            PropKey::Sym(s) => {
                let d = self.heap.syms.get(s.0 as usize).and_then(|d| d.desc);
                match d {
                    Some(d) => alloc::format!("Symbol({})", self.rust_string(d)),
                    None => String::from("Symbol()"),
                }
            }
        }
    }

    pub(crate) fn well_known(&self, w: WellKnown) -> SymId {
        let s = &self.sym;
        match w {
            WellKnown::Iterator => s.iterator,
            WellKnown::AsyncIterator => s.async_iterator,
            WellKnown::HasInstance => s.has_instance,
            WellKnown::ToPrimitive => s.to_primitive,
            WellKnown::ToStringTag => s.to_string_tag,
            WellKnown::Species => s.species,
            WellKnown::IsConcatSpreadable => s.is_concat_spreadable,
            WellKnown::Unscopables => s.unscopables,
            WellKnown::Match => s.match_,
            WellKnown::MatchAll => s.match_all,
            WellKnown::Replace => s.replace,
            WellKnown::Search => s.search,
            WellKnown::Split => s.split,
        }
    }

    pub(crate) fn intrinsic_obj(&self, i: Intrinsic) -> ObjId {
        let t = &self.intr;
        match i {
            Intrinsic::ObjectPrototype => t.object_proto,
            Intrinsic::FunctionPrototype => t.function_proto,
            Intrinsic::ArrayPrototype => t.array_proto,
            Intrinsic::ErrorPrototype => t.error_proto,
            Intrinsic::IteratorPrototype => t.iterator_proto,
            Intrinsic::PromisePrototype => t.promise_proto,
            Intrinsic::ArrayValues => t.array_values,
            Intrinsic::ArrayForEach => t.array_for_each,
            Intrinsic::Object => t.object_ctor,
            Intrinsic::Function => t.function_ctor,
            Intrinsic::Array => t.array_ctor,
            Intrinsic::Promise => t.promise_ctor,
            Intrinsic::Error => t.error_ctor,
            Intrinsic::ArrayBuffer => t.array_buffer_ctor,
            Intrinsic::ArrayBufferPrototype => t.array_buffer_proto,
            Intrinsic::Uint8Array => t.uint8_ctor,
            Intrinsic::Uint8ArrayPrototype => t.uint8_proto,
            Intrinsic::TypedArrayPrototype => t.typed_array_proto,
        }
    }

    pub(crate) fn require_obj(&mut self, v: Val, what: &str) -> Res<ObjId> {
        match v {
            Val::Obj(o) => Ok(o),
            _ => Err(self.type_error(what)),
        }
    }

    // ---- function objects ----

    /// A builtin function object (lazy `name`/`length`).
    pub(crate) fn new_builtin(
        &mut self,
        name: Val,
        arity: u32,
        f: crate::object::BuiltinFn,
    ) -> ObjId {
        let p = self.intr.function_proto;
        let id = self.alloc(
            Some(p),
            ObjKind::Native(Box::new(NativeData {
                f: NativeImpl::Builtin(f),
                name,
                arity,
                slots: Vec::new(),
                host_ctor: false,
            })),
        );
        self.om(id).flags |= LAZY_FN;
        id
    }

    pub(crate) fn new_builtin_slots(
        &mut self,
        name: Val,
        arity: u32,
        f: crate::object::BuiltinFn,
        slots: Vec<Val>,
    ) -> ObjId {
        let id = self.new_builtin(name, arity, f);
        if let ObjKind::Native(n) = &mut self.om(id).kind {
            n.slots = slots;
        }
        id
    }

    pub(crate) fn native_slot(&self, f: ObjId, i: usize) -> Val {
        match &self.o(f).kind {
            ObjKind::Native(n) => n.slots.get(i).copied().unwrap_or(Val::Undef),
            _ => Val::Undef,
        }
    }

    pub(crate) fn set_native_slot(&mut self, f: ObjId, i: usize, v: Val) {
        if let ObjKind::Native(n) = &mut self.om(f).kind
            && i < n.slots.len()
        {
            n.slots[i] = v;
        }
    }

    /// The function being called, inside a builtin.
    pub(crate) fn cur_callee(&self) -> ObjId {
        self.native_frames
            .last()
            .map_or(self.intr.function_proto, |f| f.callee)
    }

    pub(crate) fn cur_new_target(&self) -> Val {
        self.native_frames
            .last()
            .map_or(Val::Undef, |f| f.new_target)
    }

    pub(crate) fn make_host_fn(
        &mut self,
        name: &str,
        arity: u32,
        f: Rc<crate::api::NativeFn>,
        ctor: bool,
    ) -> ObjId {
        let n = self.str_val(name);
        let p = self.intr.function_proto;
        let id = self.alloc(
            Some(p),
            ObjKind::Native(Box::new(NativeData {
                f: NativeImpl::Host(f),
                name: n,
                arity,
                slots: Vec::new(),
                host_ctor: ctor,
            })),
        );
        self.om(id).flags |= LAZY_FN;
        id
    }

    pub(crate) fn make_host_class(
        &mut self,
        name: &str,
        arity: u32,
        proto: Option<ObjId>,
        parent: Option<ObjId>,
        ctor: Option<Rc<crate::api::NativeFn>>,
    ) -> ObjId {
        let id = match ctor {
            Some(f) => self.make_host_fn(name, arity, f, true),
            None => {
                let n = self.str_val(name);
                let id = self.new_builtin(n, arity, illegal_constructor);
                if let ObjKind::Native(nd) = &mut self.om(id).kind {
                    nd.host_ctor = true;
                }
                id
            }
        };
        self.materialize_fn(id);
        self.om(id).flags |= CONSTRUCTOR | CLASS_CTOR;
        if let Some(p) = parent {
            self.om(id).proto = Some(p);
        }
        if let Some(p) = proto {
            let pk = self.key(N::prototype);
            self.put_prop(id, pk, Val::Obj(p), 0);
            let ck = self.key(N::constructor);
            self.put_prop(p, ck, Val::Obj(id), crate::object::WC);
        }
        id
    }

    // ---- calls ----

    pub(crate) fn callable(&self, v: Val) -> bool {
        match v {
            Val::Obj(o) => self.o(o).is_callable() || self.vm_callable(o),
            _ => false,
        }
    }

    pub(crate) fn constructible(&self, v: Val) -> bool {
        match v {
            Val::Obj(o) => {
                let obj = self.o(o);
                match &obj.kind {
                    ObjKind::Native(_) => obj.flags & CONSTRUCTOR != 0,
                    ObjKind::Bound(b) => self.constructible(Val::Obj(b.target)),
                    _ => self.vm_constructible(o),
                }
            }
            _ => false,
        }
    }

    pub(crate) fn enter_call(&mut self) -> Res<()> {
        self.depth += 1;
        if self.depth > self.limits.max_call_depth.max(16) || self.stack.exceeded() {
            self.depth -= 1;
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        Ok(())
    }

    /// A string (or string buffer) of `units` UTF-16 units: a RangeError past the engine's
    /// longest string, an out-of-memory termination past the heap budget.
    pub(crate) fn check_str_len(&mut self, units: usize) -> Res<()> {
        if units > crate::string::MAX_LEN {
            return Err(self.range_error("Invalid string length"));
        }
        if !self.heap.could_fit(units.saturating_mul(2)) {
            return Err(self.oom());
        }
        Ok(())
    }

    /// For recursive builtins (JSON, flat, ...): a RangeError when the native stack is low.
    pub(crate) fn check_stack(&mut self) -> Res<()> {
        if self.stack.exceeded() {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        Ok(())
    }

    /// [[Call]].
    pub(crate) fn call_v(&mut self, f: Val, this: Val, args: &[Val]) -> Res<Val> {
        let Val::Obj(fo) = f else {
            return Err(self.not_callable(f));
        };
        match &self.o(fo).kind {
            ObjKind::Native(_) => {
                if self.o(fo).flags & CLASS_CTOR != 0 {
                    let n = self.fn_name(fo);
                    return Err(self.type_error(&alloc::format!(
                        "Class constructor {n} cannot be invoked without 'new'"
                    )));
                }
                self.call_native(fo, this, args, Val::Undef)
            }
            ObjKind::Bound(b) => {
                let target = b.target;
                let bthis = b.this;
                let mut all = b.args.clone();
                all.extend_from_slice(args);
                self.enter_call()?;
                let r = self.call_v(Val::Obj(target), bthis, &all);
                self.depth -= 1;
                r
            }
            _ => {
                if self.vm_callable(fo) {
                    self.call_closure(fo, this, args, Val::Undef)
                } else {
                    Err(self.not_callable(f))
                }
            }
        }
    }

    pub(crate) fn not_callable(&mut self, f: Val) -> Exception {
        let d = self.short_display(f);
        self.type_error(&alloc::format!("{d} is not a function"))
    }

    pub(crate) fn call_native(
        &mut self,
        fo: ObjId,
        this: Val,
        args: &[Val],
        new_target: Val,
    ) -> Res<Val> {
        self.enter_call()?;
        if self.native_frames.len() > 400 {
            self.depth -= 1;
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let imp = match &self.o(fo).kind {
            ObjKind::Native(n) => match &n.f {
                NativeImpl::Builtin(b) => Ok(*b),
                NativeImpl::Host(h) => Err(h.clone()),
            },
            _ => {
                self.depth -= 1;
                return Err(self.not_callable(Val::Obj(fo)));
            }
        };
        let base = self.temps.len();
        // this and the arguments stay rooted for the whole call, whoever the caller is.
        self.temps.push(this);
        self.temps.extend_from_slice(args);
        self.native_frames.push(NativeFrame {
            callee: fo,
            new_target,
            temps_base: self.temps.len(),
        });
        let r = match imp {
            Ok(b) => b(self, this, args),
            Err(h) => {
                // SAFETY: Value is repr(transparent) over Val.
                let av: &[Value] = unsafe {
                    core::slice::from_raw_parts(args.as_ptr() as *const Value, args.len())
                };
                match h(self, Value(this), av) {
                    Ok(v) => Ok(self.check_val(v.0)),
                    Err(e) => Err(match e.0 {
                        Exc::Throw(v) => Exception(Exc::Throw(self.check_val(v))),
                        t => Exception(t),
                    }),
                }
            }
        };
        self.native_frames.pop();
        self.temps.truncate(base);
        self.depth -= 1;
        r
    }

    /// [[Construct]].
    pub(crate) fn construct_v(&mut self, f: Val, args: &[Val], new_target: Val) -> Res<Val> {
        if !self.constructible(f) {
            let d = self.short_display(f);
            return Err(self.type_error(&alloc::format!("{d} is not a constructor")));
        }
        let Val::Obj(fo) = f else {
            unreachable_err!(self)
        };
        match &self.o(fo).kind {
            ObjKind::Native(n) => {
                if n.host_ctor {
                    // Host class: a fresh this from new.target.prototype.
                    let is_illegal = matches!(n.f, NativeImpl::Builtin(_));
                    if is_illegal {
                        return Err(self.type_error("Illegal constructor"));
                    }
                    let proto = self.proto_from_ctor(new_target, |r| r.intr.object_proto)?;
                    let this = self.alloc(Some(proto), ObjKind::Ordinary);
                    let mark = self.temps.len();
                    self.temps.push(Val::Obj(this));
                    let r = self.call_native(fo, Val::Obj(this), args, new_target);
                    self.temps.truncate(mark);
                    let v = r?;
                    Ok(if let Val::Obj(_) = v {
                        v
                    } else {
                        Val::Obj(this)
                    })
                } else {
                    self.call_native(fo, Val::Undef, args, new_target)
                }
            }
            ObjKind::Bound(b) => {
                let target = b.target;
                let mut all = b.args.clone();
                all.extend_from_slice(args);
                let nt = if let Val::Obj(n) = new_target
                    && n == fo
                {
                    Val::Obj(target)
                } else {
                    new_target
                };
                self.construct_v(Val::Obj(target), &all, nt)
            }
            _ => self.call_closure(fo, Val::Undef, args, new_target),
        }
    }

    /// GetPrototypeFromConstructor.
    pub(crate) fn proto_from_ctor(
        &mut self,
        new_target: Val,
        default: impl Fn(&Realm) -> ObjId,
    ) -> Res<ObjId> {
        if let Val::Obj(nt) = new_target {
            let pk = self.key(N::prototype);
            if let Val::Obj(p) = self.get_p(nt, pk, new_target)? {
                return Ok(p);
            }
        }
        Ok(default(self))
    }

    pub(crate) fn fn_name(&mut self, f: ObjId) -> String {
        let nk = self.key(N::name);
        match self.get_own_plain(f, nk) {
            Some(crate::object::Own::Data(Val::Str(s), _)) => self.rust_string(s),
            _ => String::new(),
        }
    }

    pub(crate) fn set_proto(&mut self, o: ObjId, p: Option<ObjId>) -> bool {
        if self.o(o).proto == p {
            return true;
        }
        if self.o(o).flags & crate::object::EXTENSIBLE == 0 {
            return false;
        }
        // No cycles.
        let mut cur = p;
        let mut n = 0;
        while let Some(c) = cur {
            if c == o {
                return false;
            }
            cur = self.o(c).proto;
            n += 1;
            if n > 100_000 {
                return false;
            }
        }
        self.om(o).proto = p;
        true
    }

    // ---- limits ----

    /// Called by the VM every so many steps: the step budget, the interrupt callback, memory.
    pub(crate) fn poll(&mut self) -> Res<()> {
        if self.limits.max_steps != 0 && self.steps > self.limits.max_steps {
            return Err(Exception(Exc::Term(Termination::StepBudget)));
        }
        if self.poll_steps >= self.next_poll {
            self.next_poll = self.poll_steps + crate::api::INTERRUPT_STEPS;
            if let Some(f) = self.interrupt.as_mut()
                && !f()
            {
                return Err(Exception(Exc::Term(Termination::Interrupted)));
            }
        }
        Ok(())
    }

    pub(crate) fn oom(&self) -> Exception {
        Exception(Exc::Term(Termination::OutOfMemory))
    }

    /// Builtins that allocate in a loop call this now and then.
    #[inline]
    pub(crate) fn check_mem(&self) -> Res<()> {
        if self.heap.over_hard_limit() {
            Err(self.oom())
        } else {
            Ok(())
        }
    }

    /// A safe point: collect if the heap asked for it, and stop if it is over budget.
    pub(crate) fn maybe_gc(&mut self) -> Res<()> {
        if self.heap.gc_wanted {
            self.collect_garbage();
            if self.heap.live_bytes > self.heap.max_bytes {
                return Err(self.oom());
            }
        }
        Ok(())
    }

    pub(crate) fn collect_garbage(&mut self) {
        let mut t = Tracer {
            stack: Vec::with_capacity(1024),
        };
        for &n in &self.names {
            t.str(n);
        }
        for &c in &self.char_cache {
            t.str(c);
        }
        self.intr.trace(&mut t);
        t.obj(self.global);
        t.obj(self.global_lex);
        for o in &self.join_stack {
            t.obj(*o);
        }
        for v in &self.temps {
            t.val(*v);
        }
        for v in self.pins.iter().flatten() {
            t.val(*v);
        }
        for f in &self.native_frames {
            t.obj(f.callee);
            t.val(f.new_target);
        }
        for j in &self.jobs {
            match j {
                Job::Call { f, this, args } => {
                    t.val(*f);
                    t.val(*this);
                    for a in args {
                        t.val(*a);
                    }
                }
                Job::Reaction { reaction, arg } => {
                    t.val(*reaction);
                    t.val(*arg);
                }
                Job::Thenable {
                    promise,
                    thenable,
                    then,
                } => {
                    t.val(*promise);
                    t.val(*thenable);
                    t.val(*then);
                }
            }
        }
        for p in &self.pending_rejections {
            t.obj(*p);
        }
        for (p, v) in &self.unhandled {
            t.val(*p);
            t.val(*v);
        }
        for p in &self.late_handled {
            t.obj(*p);
        }
        for (s, _) in &self.sym_registry {
            t.str(*s);
        }
        let epoch = self.heap.epoch.wrapping_add(1).max(1);
        self.vm.trace(&mut t, epoch);
        let mut dead = core::mem::take(&mut self.dead_hosts);
        self.heap.collect(&mut t, &mut dead);
        self.dead_hosts = dead;
    }

    // ---- jobs ----

    pub(crate) fn push_call_job(&mut self, f: Val, args: Vec<Val>) {
        self.jobs.push_back(Job::Call {
            f,
            this: Val::Undef,
            args,
        });
    }

    pub(crate) fn drain_jobs(&mut self) -> Res<()> {
        while let Some(job) = self.jobs.pop_front() {
            // Each job gets its own step budget, like an entry from the host.
            if self.native_frames.is_empty() && self.vm.frames.is_empty() {
                self.steps = 0;
            }
            // A job counts as a step, so an endless chain of tiny jobs still gets polled.
            self.poll_steps += 1;
            if self.poll_steps >= self.next_poll
                && let Err(e) = self.poll()
            {
                self.jobs.push_front(job);
                return Err(e);
            }
            // The job's values are off the queue now: keep them rooted while it runs.
            let mark = self.temps.len();
            match &job {
                Job::Call { f, this, args } => {
                    self.temps.push(*f);
                    self.temps.push(*this);
                    self.temps.extend_from_slice(args);
                }
                Job::Reaction { reaction, arg } => {
                    self.temps.push(*reaction);
                    self.temps.push(*arg);
                }
                Job::Thenable {
                    promise,
                    thenable,
                    then,
                } => {
                    self.temps.push(*promise);
                    self.temps.push(*thenable);
                    self.temps.push(*then);
                }
            }
            let r = match job {
                Job::Call { f, this, args } => self.call_v(f, this, &args).map(|_| ()),
                Job::Reaction { reaction, arg } => self.run_reaction_job(reaction, arg),
                Job::Thenable {
                    promise,
                    thenable,
                    then,
                } => self.run_thenable_job(promise, thenable, then),
            };
            self.temps.truncate(mark);
            if let Err(e) = r {
                // An error hook takes thrown errors (and the jobs go on); terminations
                // always end the run.
                if let Exception(Exc::Throw(v)) = e
                    && let Some(mut hook) = self.error_hook.take()
                {
                    let m = self.temps.len();
                    self.temps.push(v);
                    hook(self, &e);
                    self.temps.truncate(m);
                    if self.error_hook.is_none() {
                        self.error_hook = Some(hook);
                    }
                    continue;
                }
                self.track_rejections();
                return Err(e);
            }
            if self.native_frames.is_empty() && self.vm.frames.is_empty() {
                self.maybe_gc()?;
            }
        }
        self.track_rejections();
        Ok(())
    }

    pub(crate) fn unhandled_rejections(&mut self) -> Vec<Val> {
        self.track_rejections();
        core::mem::take(&mut self.unhandled)
            .into_iter()
            .map(|(_, reason)| reason)
            .collect()
    }

    /// Tells the rejection hook (if there is one) about promises rejected with no handler
    /// and about ones that got a handler after being reported.
    pub(crate) fn deliver_rejections(&mut self) {
        if self.rejection_hook.is_none() {
            return;
        }
        for _ in 0..1000 {
            self.track_rejections();
            let un = core::mem::take(&mut self.unhandled);
            let late = core::mem::take(&mut self.late_handled);
            if un.is_empty() && late.is_empty() {
                return;
            }
            let Some(mut hook) = self.rejection_hook.take() else {
                // (a nested call is delivering: put them back for it)
                self.unhandled = un;
                self.late_handled = late;
                return;
            };
            // Prune the weak list now and then.
            if self.reported.len() > 256 {
                let heap = &self.heap;
                self.reported.retain(|p| heap.obj_valid(*p));
            }
            let mark = self.temps.len();
            for (p, reason) in un {
                if let Val::Obj(po) = p {
                    self.reported.push(po);
                }
                let ev = crate::api::PromiseRejection::Unhandled {
                    promise: self.out_val(p),
                    reason: self.out_val(reason),
                };
                hook(self, ev);
            }
            for p in late {
                let ev = crate::api::PromiseRejection::Handled {
                    promise: self.out_val(Val::Obj(p)),
                };
                hook(self, ev);
            }
            self.temps.truncate(mark);
            if self.rejection_hook.is_none() {
                self.rejection_hook = Some(hook);
            }
        }
    }
}

fn illegal_constructor(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    Err(r.type_error("Illegal constructor"))
}

#[allow(dead_code)]
const _W: u8 = W;
