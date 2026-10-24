//! The VM: frames on one value stack, the instruction loop, calls (script calls don't recurse
//! on the Rust stack; native calls and getters do), exceptions through the per-function
//! handler tables, and coroutines (generators, async functions, async generators) whose
//! suspended frames are moved off the stack into their objects.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::{Exc, Exception, Termination};
use crate::bytecode::{Code, CodeKind, Const, ENV_SLOT_BITS, Op};
use crate::heap::Tracer;
use crate::names::N;
use crate::object::{
    ArrayData, CLASS_CTOR, CONSTRUCTOR, Desc, E, LAZY_FN, LAZY_PROTO, ObjKind, PropKey, W, WC, WEC,
};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, StrId, Val};

pub(crate) const MAX_STACK: usize = 1 << 19;

pub(crate) struct Closure {
    pub code: Rc<Code>,
    pub env: Option<ObjId>,
    /// Arrows: the `this` and `new.target` of where they were made.
    pub this: Val,
    pub new_target: Val,
    /// Methods: the object `super` looks up from (arrows copy their maker's).
    pub home: Option<ObjId>,
    /// Arrows: the enclosing function (for `super()` and field initialization).
    pub func_ctx: Option<ObjId>,
    /// A class constructor's instance field initializer.
    pub fields: Option<ObjId>,
    /// A field initializer's computed keys.
    pub field_keys: Vec<Val>,
}

impl Closure {
    pub fn trace(&self, t: &mut Tracer, epoch: u32) {
        self.code.trace(t, epoch);
        t.opt_obj(self.env);
        t.val(self.this);
        t.val(self.new_target);
        t.opt_obj(self.home);
        t.opt_obj(self.func_ctx);
        t.opt_obj(self.fields);
        for k in &self.field_keys {
            t.val(*k);
        }
    }
}

pub(crate) struct EnvData {
    pub slots: Vec<Val>,
    pub parent: Option<ObjId>,
}

pub(crate) struct Frame {
    pub code: Rc<Code>,
    pub pc: u32,
    /// Stack index of local 0 (`this` and the callee are just below).
    pub base: u32,
    pub env: Option<ObjId>,
    pub this: Val,
    pub func: Option<ObjId>,
    pub new_target: Val,
    pub argc: u32,
    /// Arguments past the parameters (kept only when the code needs them).
    pub extra: Vec<Val>,
    /// Return to Rust when this frame completes.
    pub entry: bool,
    /// [[Construct]] semantics for the return value.
    pub construct: bool,
    /// The coroutine this frame belongs to.
    pub co: Option<ObjId>,
    /// This frame was resumed from its coroutine (its completion goes to the resumer).
    pub resumed: bool,
    /// A pending `return` being unwound through finally blocks (generator.return()).
    pub pending_return: Option<Val>,
}

impl Frame {
    fn trace(&self, t: &mut Tracer, epoch: u32) {
        self.code.trace(t, epoch);
        t.opt_obj(self.env);
        t.val(self.this);
        t.opt_obj(self.func);
        t.val(self.new_target);
        for v in &self.extra {
            t.val(*v);
        }
        t.opt_obj(self.co);
        if let Some(v) = self.pending_return {
            t.val(v);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CoKind {
    Generator,
    Async,
    AsyncGen,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CoState {
    /// Created, not started (generators).
    Start,
    /// Suspended at a yield.
    Yielded,
    /// Suspended at an await.
    Awaiting,
    Running,
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Resume {
    Next,
    Throw,
    Return,
}

pub(crate) struct AgRequest {
    pub mode: Resume,
    pub value: Val,
    /// The promise next()/throw()/return() returned.
    pub promise: ObjId,
}

pub(crate) struct CoData {
    pub kind: CoKind,
    pub state: CoState,
    pub frame: Option<Frame>,
    /// The saved stack segment: this, callee, locals, operands.
    pub stack: Vec<Val>,
    /// Async functions: the result promise.
    pub promise: Val,
    /// Async generators: pending requests.
    pub queue: VecDeque<AgRequest>,
}

impl CoData {
    pub fn trace(&self, t: &mut Tracer, epoch: u32) {
        if let Some(f) = &self.frame {
            f.trace(t, epoch);
        }
        for v in &self.stack {
            t.val(*v);
        }
        t.val(self.promise);
        for r in &self.queue {
            t.val(r.value);
            t.obj(r.promise);
        }
    }
    pub fn bytes(&self) -> usize {
        96 + self.stack.capacity() * 16 + self.queue.len() * 32
    }
}

pub(crate) struct ArrayIterData {
    pub obj: Val,
    pub idx: u32,
    /// 0 keys, 1 values, 2 entries.
    pub kind: u8,
    pub done: bool,
}

pub(crate) struct ForInData {
    pub obj: Val,
    pub keys: Vec<Val>,
    pub pos: usize,
}

/// How the last `run` ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Signal {
    Return,
    Yield,
    Await,
}

pub(crate) struct VmState {
    pub frames: Vec<Frame>,
    pub stack: Vec<Val>,
    pub signal: Signal,
}

impl VmState {
    pub fn new() -> VmState {
        VmState {
            frames: Vec::new(),
            stack: Vec::new(),
            signal: Signal::Return,
        }
    }
    pub fn trace(&self, t: &mut Tracer, epoch: u32) {
        for v in &self.stack {
            t.val(*v);
        }
        for f in &self.frames {
            f.trace(t, epoch);
        }
    }
}

/// The marker a generator `return()` unwinds with (only finally handlers see it).
const RETURN_MARK: Val = Val::Empty;

fn is_return_mark(e: &Exception) -> bool {
    matches!(e.0, Exc::Throw(Val::Empty))
}

impl Realm {
    // ---- small accessors ----

    pub(crate) fn vm_callable(&self, o: ObjId) -> bool {
        matches!(self.o(o).kind, ObjKind::Closure(_))
    }

    pub(crate) fn vm_constructible(&self, o: ObjId) -> bool {
        matches!(self.o(o).kind, ObjKind::Closure(_)) && self.o(o).flags & CONSTRUCTOR != 0
    }

    pub(crate) fn is_class_ctor(&self, o: ObjId) -> bool {
        self.o(o).flags & CLASS_CTOR != 0
    }

    fn closure(&self, f: ObjId) -> Option<&Closure> {
        match &self.o(f).kind {
            ObjKind::Closure(c) => Some(c),
            _ => None,
        }
    }

    fn closure_mut(&mut self, f: ObjId) -> Option<&mut Closure> {
        match &mut self.om(f).kind {
            ObjKind::Closure(c) => Some(c),
            _ => None,
        }
    }

    #[inline]
    fn top(&mut self) -> &mut Frame {
        let n = self.vm.frames.len();
        &mut self.vm.frames[n - 1]
    }

    #[inline]
    fn pop(&mut self) -> Val {
        self.vm.stack.pop().unwrap_or(Val::Undef)
    }

    #[inline]
    fn push(&mut self, v: Val) {
        self.vm.stack.push(v);
    }

    #[inline]
    fn peek(&self, from_top: usize) -> Val {
        let n = self.vm.stack.len();
        if from_top < n {
            self.vm.stack[n - 1 - from_top]
        } else {
            Val::Undef
        }
    }

    fn env_at(&self, depth: u32) -> Option<ObjId> {
        let n = self.vm.frames.len();
        let mut e = self.vm.frames[n - 1].env;
        for _ in 0..depth {
            e = match e {
                Some(id) => match &self.o(id).kind {
                    ObjKind::Env(d) => d.parent,
                    _ => None,
                },
                None => None,
            };
        }
        e
    }

    fn env_get(&self, r: u32) -> Val {
        let depth = r >> ENV_SLOT_BITS;
        let slot = (r & ((1 << ENV_SLOT_BITS) - 1)) as usize;
        match self.env_at(depth) {
            Some(id) => match &self.o(id).kind {
                ObjKind::Env(d) => d.slots.get(slot).copied().unwrap_or(Val::Undef),
                _ => Val::Undef,
            },
            None => Val::Undef,
        }
    }

    /// The value an imported binding currently has (Empty before its module initialized it).
    fn import_get(&self, r: u32) -> Val {
        match self.env_get(r) {
            Val::Obj(o) => match &self.o(o).kind {
                ObjKind::ImportRef { env, slot } => match &self.o(*env).kind {
                    ObjKind::Env(d) => d.slots.get(*slot as usize).copied().unwrap_or(Val::Empty),
                    _ => Val::Empty,
                },
                _ => Val::Empty,
            },
            _ => Val::Empty,
        }
    }

    fn env_set(&mut self, r: u32, v: Val) {
        let depth = r >> ENV_SLOT_BITS;
        let slot = (r & ((1 << ENV_SLOT_BITS) - 1)) as usize;
        if let Some(id) = self.env_at(depth)
            && let ObjKind::Env(d) = &mut self.om(id).kind
            && let Some(s) = d.slots.get_mut(slot)
        {
            *s = v;
        }
    }

    pub(crate) fn const_str_val(&self, code: &Code, i: u32) -> StrId {
        match code.consts.get(i as usize) {
            Some(Const::Str(s)) => *s,
            _ => self.nstr(N::empty),
        }
    }

    // ---- entering the VM from Rust ----

    /// [[Call]] / [[Construct]] of a script function, from Rust.
    pub(crate) fn call_closure(
        &mut self,
        f: ObjId,
        this: Val,
        args: &[Val],
        new_target: Val,
    ) -> Res<Val> {
        if !matches!(self.o(f).kind, ObjKind::Closure(_)) {
            return Err(self.not_callable(Val::Obj(f)));
        }
        self.check_stack()?;
        if self.vm.stack.len() + args.len() + 2 > MAX_STACK {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let construct = !new_target.is_undef();
        let this = if construct {
            if self.o(f).flags & CONSTRUCTOR == 0 {
                let d = self.short_display(Val::Obj(f));
                return Err(self.type_error(&alloc::format!("{d} is not a constructor")));
            }
            self.construct_this(f, new_target)?
        } else {
            if self.o(f).flags & CLASS_CTOR != 0 {
                let n = self.fn_name(f);
                return Err(self.type_error(&alloc::format!(
                    "Class constructor {n} cannot be invoked without 'new'"
                )));
            }
            this
        };
        let mark = self.vm.stack.len();
        self.vm.stack.push(this);
        self.vm.stack.push(Val::Obj(f));
        self.vm.stack.extend_from_slice(args);
        // (A native that calls script over and over - a proxy trap, a getter, a callback - pays
        // steps for each call, so the budget and the interrupt still end such a loop.)
        if let Err(e) = self.safe_point() {
            self.vm.stack.truncate(mark);
            return Err(e);
        }
        if let Err(e) = self.push_frame(f, args.len(), new_target, true, construct) {
            self.vm.stack.truncate(mark);
            return Err(e);
        }
        let r = self.run(None);
        self.vm.stack.truncate(mark);
        r
    }

    /// `this` for [[Construct]]: a new object from new.target.prototype (Empty in derived
    /// constructors).
    fn construct_this(&mut self, f: ObjId, new_target: Val) -> Res<Val> {
        let derived =
            matches!(&self.o(f).kind, ObjKind::Closure(c) if c.code.kind == CodeKind::DerivedCtor);
        if derived {
            return Ok(Val::Empty);
        }
        let proto = self.proto_from_ctor(new_target, |r| r.intr.object_proto)?;
        Ok(Val::Obj(self.alloc(Some(proto), ObjKind::Ordinary)))
    }

    /// Sets up a frame for closure `f` whose this, callee and `argc` arguments are on top of
    /// the stack.
    fn push_frame(
        &mut self,
        f: ObjId,
        argc: usize,
        new_target: Val,
        entry: bool,
        construct: bool,
    ) -> Res<()> {
        if self.vm.frames.len() as u32 >= self.limits.max_call_depth.max(16) {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let (mut code, env, cthis, cnt) = match &self.o(f).kind {
            ObjKind::Closure(c) => (c.code.clone(), c.env, c.this, c.new_target),
            _ => return Err(self.not_callable(Val::Obj(f))),
        };
        if code.lazy.is_some() {
            // First call of a function left uncompiled: compile it now.
            code = self.force_lazy(&code)?;
            if let ObjKind::Closure(c) = &mut self.om(f).kind {
                c.code = code.clone();
            }
        }
        let nparams = code.nparams as usize;
        let nlocals = code.nlocals as usize;
        if self.vm.stack.len() + nlocals + 64 > MAX_STACK {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let sp = self.vm.stack.len();
        let base = sp - argc;
        let mut extra = Vec::new();
        if argc > nparams {
            if code.keeps_extra_args {
                extra = self.vm.stack[base + nparams..sp].to_vec();
            }
            self.vm.stack.truncate(base + nparams);
        }
        // Missing parameters and the other locals start undefined.
        self.vm
            .stack
            .resize(base + nlocals.max(nparams), Val::Undef);
        let given_this = self.vm.stack[base - 2];
        let (this, nt) = match code.kind {
            CodeKind::Arrow => (cthis, cnt),
            _ => {
                let t = if construct || code.strict {
                    given_this
                } else {
                    match given_this {
                        Val::Undef | Val::Null | Val::Empty => Val::Obj(self.global),
                        Val::Obj(_) => given_this,
                        p => Val::Obj(self.to_object(p)?),
                    }
                };
                (t, new_target)
            }
        };
        let co = if code.is_async && !code.is_generator {
            let (p, _, _) = self.promise_with_resolvers();
            Some(self.alloc(
                None,
                ObjKind::Coroutine(Box::new(CoData {
                    kind: CoKind::Async,
                    state: CoState::Running,
                    frame: None,
                    stack: Vec::new(),
                    promise: Val::Obj(p),
                    queue: VecDeque::new(),
                })),
            ))
        } else {
            None
        };
        self.vm.frames.push(Frame {
            code,
            pc: 0,
            base: base as u32,
            env,
            this,
            func: Some(f),
            new_target: nt,
            argc: argc as u32,
            extra,
            entry,
            construct,
            co,
            resumed: false,
            pending_return: None,
        });
        Ok(())
    }

    /// Runs a script's top-level code.
    pub(crate) fn run_script_code(&mut self, code: Rc<Code>) -> Res<Val> {
        let mark = self.vm.stack.len();
        let g = Val::Obj(self.global);
        self.vm.stack.push(g);
        self.vm.stack.push(Val::Undef);
        let base = self.vm.stack.len();
        for _ in 0..code.nlocals {
            self.vm.stack.push(Val::Undef);
        }
        self.vm.frames.push(Frame {
            code,
            pc: 0,
            base: base as u32,
            env: None,
            this: g,
            func: None,
            new_target: Val::Undef,
            argc: 0,
            extra: Vec::new(),
            entry: true,
            construct: false,
            co: None,
            resumed: false,
            pending_return: None,
        });
        let r = self.run(None);
        self.vm.stack.truncate(mark);
        r
    }

    // ---- coroutines ----

    /// Resumes a suspended coroutine. The VM's signal says how it stopped.
    pub(crate) fn co_resume(&mut self, co: ObjId, value: Val, mode: Resume) -> Res<Val> {
        self.check_stack()?;
        let (frame, stack) = match &mut self.om(co).kind {
            ObjKind::Coroutine(c) => {
                c.state = CoState::Running;
                (c.frame.take(), core::mem::take(&mut c.stack))
            }
            _ => return Err(self.type_error("not a coroutine")),
        };
        let Some(mut frame) = frame else {
            return Err(self.type_error("Generator is already running"));
        };
        if self.vm.frames.len() as u32 >= self.limits.max_call_depth.max(16)
            || self.vm.stack.len() + stack.len() > MAX_STACK
        {
            if let ObjKind::Coroutine(c) = &mut self.om(co).kind {
                c.frame = Some(frame);
                c.stack = stack;
            }
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let mark = self.vm.stack.len();
        let old_base = frame.base;
        frame.base = (mark + 2) as u32;
        let _ = old_base;
        frame.entry = true;
        frame.resumed = true;
        self.vm.stack.extend_from_slice(&stack);
        self.vm.frames.push(frame);
        // Suspended inside `yield*`: throw/return go to the inner iterator.
        if mode != Resume::Next {
            let (pc, base) = {
                let f = self.top();
                (f.pc as usize, f.base as usize)
            };
            let code = self.top().code.clone();
            if pc >= 1
                && let Some(Op::Jump(_)) = code.ops.get(pc)
                && let Some(Op::YieldStar(s)) = code.ops.get(pc - 1)
            {
                let r = self.yield_star_step(base + *s as usize, value, mode);
                let pending = match r {
                    Ok(YieldStar::Done(v)) => {
                        self.vm.stack.push(v);
                        self.top().pc = (pc + 1) as u32;
                        None
                    }
                    Ok(YieldStar::Yield(v)) => {
                        self.suspend_top(CoState::Yielded);
                        self.vm.signal = Signal::Yield;
                        self.vm.stack.truncate(mark);
                        return Ok(v);
                    }
                    Ok(YieldStar::Return(v)) => {
                        self.top().pending_return = Some(v);
                        Some(Exception(Exc::Throw(RETURN_MARK)))
                    }
                    Err(e) => Some(e),
                };
                let r = self.run(pending);
                self.vm.stack.truncate(mark);
                return r;
            }
        }
        let pending = match mode {
            Resume::Next => {
                self.vm.stack.push(value);
                None
            }
            Resume::Throw => Some(Exception(Exc::Throw(value))),
            Resume::Return => {
                self.top().pending_return = Some(value);
                Some(Exception(Exc::Throw(RETURN_MARK)))
            }
        };
        let r = self.run(pending);
        self.vm.stack.truncate(mark);
        r
    }

    /// Moves the top frame (and its stack segment) into its coroutine.
    fn suspend_top(&mut self, state: CoState) -> Option<(ObjId, Frame)> {
        let frame = self.vm.frames.pop()?;
        let seg_start = frame.base as usize - 2;
        let seg: Vec<Val> = self.vm.stack.drain(seg_start..).collect();
        let co = frame.co?;
        let saved = Frame {
            code: frame.code.clone(),
            pc: frame.pc,
            base: 2,
            env: frame.env,
            this: frame.this,
            func: frame.func,
            new_target: frame.new_target,
            argc: frame.argc,
            extra: frame.extra.clone(),
            entry: false,
            construct: false,
            co: frame.co,
            resumed: false,
            pending_return: frame.pending_return,
        };
        if let ObjKind::Coroutine(c) = &mut self.om(co).kind {
            c.frame = Some(saved);
            c.stack = seg;
            c.state = state;
        }
        Some((co, frame))
    }

    fn set_co_state(&mut self, co: ObjId, st: CoState) {
        if let ObjKind::Coroutine(c) = &mut self.om(co).kind {
            c.state = st;
        }
    }

    fn co_kind(&self, co: ObjId) -> Option<CoKind> {
        match &self.o(co).kind {
            ObjKind::Coroutine(c) => Some(c.kind),
            _ => None,
        }
    }

    fn co_promise(&self, co: ObjId) -> Val {
        match &self.o(co).kind {
            ObjKind::Coroutine(c) => c.promise,
            _ => Val::Undef,
        }
    }

    // ---- frame completion ----

    /// The top frame completed with `v` (a return). Returns Some(value) when `run` should
    /// return it to Rust.
    fn complete_frame(&mut self, v: Val) -> Res<Option<Val>> {
        let Some(frame) = self.vm.frames.pop() else {
            return Ok(Some(v));
        };
        self.vm.stack.truncate(frame.base as usize - 2);
        let mut result = v;
        if frame.construct && !matches!(v, Val::Obj(_)) {
            if frame.code.kind == CodeKind::DerivedCtor && !v.is_undef() {
                let e = self.type_error("Derived constructors may only return object or undefined");
                return self.abrupt_from(frame, Err(e));
            }
            if frame.this.is_empty() {
                let e = self.reference_error(
                        "Must call super constructor in derived class before accessing 'this' or returning from derived constructor",
                    );
                return self.abrupt_from(frame, Err(e));
            }
            result = frame.this;
        }
        if let Some(co) = frame.co {
            match self.co_kind(co) {
                Some(CoKind::Async) => {
                    self.set_co_state(co, CoState::Done);
                    let p = self.co_promise(co);
                    if let Val::Obj(p) = p {
                        self.resolve_promise(p, v)?;
                    }
                    if frame.resumed {
                        self.vm.signal = Signal::Return;
                        return Ok(Some(Val::Undef));
                    }
                    result = p;
                }
                Some(CoKind::Generator) | Some(CoKind::AsyncGen) => {
                    self.set_co_state(co, CoState::Done);
                    self.vm.signal = Signal::Return;
                    return Ok(Some(v));
                }
                None => {}
            }
        }
        if frame.entry {
            self.vm.signal = Signal::Return;
            return Ok(Some(result));
        }
        self.vm.stack.push(result);
        Ok(None)
    }

    /// An error raised while completing a popped frame: rethrow it in the caller.
    fn abrupt_from(&mut self, frame: Frame, e: Res<Option<Val>>) -> Res<Option<Val>> {
        if frame.entry {
            return e;
        }
        // Not an entry frame: the caller is on the VM stack; let the loop unwind there.
        e
    }

    // ---- the loop ----

    /// Runs the top frame (an entry frame) until it completes, suspends, or throws.
    pub(crate) fn run(&mut self, pending: Option<Exception>) -> Res<Val> {
        let mut pending = pending;
        let entry_depth = self.vm.frames.len();
        loop {
            let exc = match pending.take() {
                Some(e) => e,
                None => match self.run_ops() {
                    Ok(v) => return Ok(v),
                    Err(e) => e,
                },
            };
            match self.unwind(exc, entry_depth) {
                Ok(None) => continue,
                Ok(Some(v)) => return Ok(v),
                Err(e) => return Err(e),
            }
        }
    }

    /// Finds a handler for `exc`. Ok(None): resume in the top frame. Ok(Some(v)): an entry
    /// frame completed normally (a generator return unwound, or an async function turned
    /// the exception into a rejection). Err: the exception leaves the entry frame.
    fn unwind(&mut self, exc: Exception, entry_depth: usize) -> Res<Option<Val>> {
        let mut exc = exc;
        loop {
            if self.vm.frames.len() < entry_depth {
                // The entry frame is gone already (an error while completing it).
                return Err(exc);
            }
            let Some(frame) = self.vm.frames.last() else {
                return Err(exc);
            };
            let catchable = matches!(exc.0, Exc::Throw(_));
            if catchable {
                let is_ret = is_return_mark(&exc);
                let fault = frame.pc.saturating_sub(1);
                let found = frame
                    .code
                    .handlers
                    .iter()
                    .find(|h| h.start <= fault && fault < h.end && (!is_ret || h.finally))
                    .copied();
                if let Some(h) = found {
                    let base = frame.base as usize;
                    let nl = frame.code.nlocals as usize;
                    self.vm.stack.truncate(base + nl + h.depth as usize);
                    let v = match exc.0 {
                        Exc::Throw(v) => v,
                        _ => Val::Undef,
                    };
                    self.vm.stack.push(v);
                    self.top().pc = h.target;
                    return Ok(None);
                }
            }
            // No handler here: the frame ends abruptly.
            let is_ret = is_return_mark(&exc);
            let pending_ret = frame.pending_return;
            if is_ret {
                let v = pending_ret.unwrap_or(Val::Undef);
                match self.complete_frame(v) {
                    Ok(Some(v)) => return Ok(Some(v)),
                    Ok(None) => return Ok(None),
                    Err(e) => {
                        exc = e;
                        continue;
                    }
                }
            }
            let Some(frame) = self.vm.frames.pop() else {
                return Err(exc);
            };
            self.vm
                .stack
                .truncate((frame.base as usize).saturating_sub(2));
            if let Some(co) = frame.co {
                match self.co_kind(co) {
                    Some(CoKind::Async) if catchable => {
                        self.set_co_state(co, CoState::Done);
                        let p = self.co_promise(co);
                        if let (Val::Obj(p), Exc::Throw(v)) = (p, exc.0) {
                            self.reject_promise(p, v);
                        }
                        if frame.resumed {
                            self.vm.signal = Signal::Return;
                            return Ok(Some(Val::Undef));
                        }
                        if frame.entry {
                            self.vm.signal = Signal::Return;
                            return Ok(Some(p));
                        }
                        self.vm.stack.push(p);
                        return Ok(None);
                    }
                    Some(_) => self.set_co_state(co, CoState::Done),
                    None => {}
                }
            }
            if frame.entry {
                return Err(exc);
            }
        }
    }

    /// The callee's source text at the current call site ("foo.bar"), if it is a simple
    /// name or member chain.
    fn callee_text(&self) -> Option<String> {
        let n = self.vm.frames.len();
        if n == 0 {
            return None;
        }
        let fr = &self.vm.frames[n - 1];
        let mut pos = fr.code.pos_at(fr.pc.saturating_sub(1)) as usize;
        let src = &fr.code.source.text;
        if src.get(pos..pos + 3) == Some(b"new")
            && src.get(pos + 3).is_some_and(|c| c.is_ascii_whitespace())
        {
            pos += 3;
            while pos < src.len() && src[pos].is_ascii_whitespace() {
                pos += 1;
            }
        }
        let mut end = pos;
        while end < src.len()
            && (src[end].is_ascii_alphanumeric()
                || src[end] == b'_'
                || src[end] == b'$'
                || src[end] == b'.'
                || src[end] >= 0x80)
        {
            end += 1;
        }
        if end == pos || end - pos > 80 {
            return None;
        }
        let s = core::str::from_utf8(&src[pos..end]).ok()?;
        let s = s.trim_end_matches('.');
        if s.is_empty()
            || s.starts_with(|c: char| c.is_ascii_digit())
            || s == "function"
            || s == "class"
        {
            return None;
        }
        Some(String::from(s))
    }

    fn not_callable_at(&mut self, f: Val) -> Exception {
        match self.callee_text() {
            Some(s) => self.type_error(&alloc::format!("{s} is not a function")),
            None => self.not_callable(f),
        }
    }

    fn not_constructor_at(&mut self, f: Val) -> Exception {
        let d = match self.callee_text() {
            Some(s) => s,
            None => self.short_display(f),
        };
        self.type_error(&alloc::format!("{d} is not a constructor"))
    }

    /// The instruction loop for the top frame.
    fn run_ops(&mut self) -> Res<Val> {
        let mut code: Rc<Code> = self.top().code.clone();
        let mut pc: usize = self.top().pc as usize;
        let mut base: usize = self.top().base as usize;
        // The running frame's index: its pc is kept current on every instruction, so errors
        // made anywhere below (and stack traces) see the right position.
        let mut fi: usize = self.vm.frames.len() - 1;
        macro_rules! sync {
            () => {
                let n = self.vm.frames.len();
                self.vm.frames[n - 1].pc = pc as u32;
            };
        }
        macro_rules! reload {
            () => {
                let n = self.vm.frames.len();
                fi = n - 1;
                let f = &self.vm.frames[n - 1];
                code = f.code.clone();
                pc = f.pc as usize;
                base = f.base as usize;
            };
        }
        macro_rules! tri {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => {
                        sync!();
                        return Err(e);
                    }
                }
            };
        }
        macro_rules! throw {
            ($e:expr) => {{
                sync!();
                return Err($e);
            }};
        }
        macro_rules! key_of {
            ($c:expr) => {{
                match code.keys.get($c as usize) {
                    Some(k) => *k,
                    None => {
                        let s = self.const_str_val(&code, $c);
                        self.key_from_str(s)
                    }
                }
            }};
        }
        loop {
            let Some(&op) = code.ops.get(pc) else {
                // Fell off the end (shouldn't happen: code ends in a return).
                sync!();
                match self.complete_frame(Val::Undef)? {
                    Some(v) => return Ok(v),
                    None => {
                        reload!();
                        continue;
                    }
                }
            };
            pc += 1;
            if let Some(f) = self.vm.frames.get_mut(fi) {
                f.pc = pc as u32;
            }
            match op {
                Op::Undef => self.push(Val::Undef),
                Op::Null => self.push(Val::Null),
                Op::True => self.push(Val::Bool(true)),
                Op::False => self.push(Val::Bool(false)),
                Op::Empty => self.push(Val::Empty),
                Op::Int(i) => self.push(Val::Int(i)),
                Op::Const(c) => {
                    let v = match code.consts.get(c as usize) {
                        Some(Const::Num(n)) => Val::num(*n),
                        Some(Const::Str(s)) => Val::Str(*s),
                        _ => Val::Undef,
                    };
                    self.push(v);
                }
                Op::Pop => {
                    self.vm.stack.pop();
                }
                Op::Dup => {
                    let v = self.peek(0);
                    self.push(v);
                }
                Op::Dup2 => {
                    let a = self.peek(1);
                    let b = self.peek(0);
                    self.push(a);
                    self.push(b);
                }
                Op::Swap => {
                    let n = self.vm.stack.len();
                    if n >= 2 {
                        self.vm.stack.swap(n - 1, n - 2);
                    }
                }
                Op::Rot3 => {
                    let n = self.vm.stack.len();
                    if n >= 3 {
                        let c = self.vm.stack[n - 1];
                        self.vm.stack[n - 1] = self.vm.stack[n - 2];
                        self.vm.stack[n - 2] = self.vm.stack[n - 3];
                        self.vm.stack[n - 3] = c;
                    }
                }
                Op::Rot4 => {
                    let n = self.vm.stack.len();
                    if n >= 4 {
                        let d = self.vm.stack[n - 1];
                        self.vm.stack[n - 1] = self.vm.stack[n - 2];
                        self.vm.stack[n - 2] = self.vm.stack[n - 3];
                        self.vm.stack[n - 3] = self.vm.stack[n - 4];
                        self.vm.stack[n - 4] = d;
                    }
                }
                Op::GetLocal(l) => {
                    let v = self
                        .vm
                        .stack
                        .get(base + l as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    self.push(v);
                }
                Op::SetLocal(l) => {
                    let v = self.peek(0);
                    if let Some(s) = self.vm.stack.get_mut(base + l as usize) {
                        *s = v;
                    }
                }
                Op::PutLocal(l) => {
                    let v = self.pop();
                    if let Some(s) = self.vm.stack.get_mut(base + l as usize) {
                        *s = v;
                    }
                }
                Op::GetLocalChk(l, n) => {
                    let v = self
                        .vm
                        .stack
                        .get(base + l as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    if v.is_empty() {
                        let e = self.tdz_error(&code, n);
                        throw!(e);
                    }
                    self.push(v);
                }
                Op::SetLocalChk(l, n) => {
                    let cur = self
                        .vm
                        .stack
                        .get(base + l as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    if cur.is_empty() {
                        let e = self.tdz_error(&code, n);
                        throw!(e);
                    }
                    let v = self.peek(0);
                    if let Some(s) = self.vm.stack.get_mut(base + l as usize) {
                        *s = v;
                    }
                }
                Op::GetEnv(r) => {
                    let v = self.env_get(r);
                    self.push(v);
                }
                Op::SetEnv(r) => {
                    let v = self.peek(0);
                    self.env_set(r, v);
                }
                Op::PutEnv(r) => {
                    let v = self.pop();
                    self.env_set(r, v);
                }
                Op::GetEnvChk(r, n) => {
                    let v = self.env_get(r);
                    if v.is_empty() {
                        let e = self.tdz_error(&code, n);
                        throw!(e);
                    }
                    self.push(v);
                }
                Op::NewPrivName(c) => {
                    let d = self.const_str_val(&code, c);
                    let v = self.new_private_name(d);
                    self.push(v);
                }
                Op::GetPriv => {
                    let k = self.pop();
                    let o = self.pop();
                    let v = tri!(self.private_get(o, k));
                    self.push(v);
                }
                Op::GetPrivKeep => {
                    let k = self.pop();
                    let o = self.peek(0);
                    let v = tri!(self.private_get(o, k));
                    self.push(v);
                }
                Op::SetPriv => {
                    let v = self.pop();
                    let k = self.pop();
                    let o = self.pop();
                    tri!(self.private_set(o, k, v));
                    self.push(v);
                }
                Op::PrivIn => {
                    let o = self.pop();
                    let k = self.pop();
                    let b = tri!(self.private_in(k, o));
                    self.push(Val::Bool(b));
                }
                Op::DefinePrivField => {
                    let v = self.pop();
                    let k = self.pop();
                    let o = self.peek(0);
                    tri!(self.private_add_field(o, k, v));
                }
                Op::DefinePrivMethod(kind) => {
                    let f = self.pop();
                    let k = self.pop();
                    let o = self.peek(0);
                    tri!(self.private_add_method(o, k, f, kind));
                }
                Op::GetImport(r, n) => {
                    let v = self.import_get(r);
                    if v.is_empty() {
                        let e = self.tdz_error(&code, n);
                        throw!(e);
                    }
                    self.push(v);
                }
                Op::ImportMeta => {
                    sync!();
                    let m = code.module;
                    let v = tri!(self.import_meta(m));
                    self.push(v);
                }
                Op::ImportCall => {
                    sync!();
                    let opts = self.pop();
                    let spec = self.pop();
                    self.temps.push(spec);
                    self.temps.push(opts);
                    let referrer = code.module;
                    let file = code.source.file.clone();
                    let r = self.import_call(referrer, file, spec, opts);
                    self.temps.truncate(self.temps.len() - 2);
                    let v = tri!(r);
                    self.push(v);
                }
                Op::SetEnvChk(r, n) => {
                    if self.env_get(r).is_empty() {
                        let e = self.tdz_error(&code, n);
                        throw!(e);
                    }
                    let v = self.peek(0);
                    self.env_set(r, v);
                }
                Op::PushEnv(n) => {
                    let parent = self.top().env;
                    let e = self.alloc(
                        None,
                        ObjKind::Env(Box::new(EnvData {
                            slots: alloc::vec![Val::Undef; n as usize],
                            parent,
                        })),
                    );
                    self.top().env = Some(e);
                }
                Op::PopEnv => {
                    let cur = self.top().env;
                    let parent = cur.and_then(|e| match &self.o(e).kind {
                        ObjKind::Env(d) => d.parent,
                        _ => None,
                    });
                    self.top().env = parent;
                }
                Op::CopyEnv => {
                    if let Some(cur) = self.top().env {
                        let (slots, parent) = match &self.o(cur).kind {
                            ObjKind::Env(d) => (d.slots.clone(), d.parent),
                            _ => (Vec::new(), None),
                        };
                        let e = self.alloc(None, ObjKind::Env(Box::new(EnvData { slots, parent })));
                        self.top().env = Some(e);
                    }
                }
                Op::SaveEnv(l) => {
                    let e = self.top().env.map_or(Val::Undef, Val::Obj);
                    if let Some(s) = self.vm.stack.get_mut(base + l as usize) {
                        *s = e;
                    }
                }
                Op::RestoreEnv(l) => {
                    let e = self
                        .vm
                        .stack
                        .get(base + l as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    self.top().env = e.as_obj();
                }
                Op::GetGlobal(c) => {
                    let k = key_of!(c);
                    let v = tri!(self.global_get(k, false));
                    self.push(v);
                }
                Op::TypeofGlobal(c) => {
                    let k = key_of!(c);
                    let v = tri!(self.global_get(k, true));
                    let t = self.typeof_name(v);
                    let s = self.nstr(t);
                    self.push(Val::Str(s));
                }
                Op::SetGlobal(c) => {
                    let k = key_of!(c);
                    let v = self.peek(0);
                    tri!(self.global_set(k, v, code.strict));
                }
                Op::DeclareVar(c) => {
                    let k = key_of!(c);
                    tri!(self.declare_global_var(k));
                }
                Op::DeclareLex(c, is_const) => {
                    let k = key_of!(c);
                    tri!(self.declare_global_lex(k, is_const != 0));
                }
                Op::InitLex(c) => {
                    let k = key_of!(c);
                    let v = self.pop();
                    let gl = self.global_lex;
                    if let Some(pos) = self.o(gl).props.find(k) {
                        self.om(gl).props.ents[pos].1.v = v;
                    }
                }
                Op::GetProp(c) => {
                    let o = self.pop();
                    let k = key_of!(c);
                    let v = match self.get_fast(o, k) {
                        Some(v) => v,
                        None => tri!(self.get_v(o, k)),
                    };
                    self.push(v);
                }
                Op::GetPropKeep(c) => {
                    let o = self.peek(0);
                    let k = key_of!(c);
                    let v = match self.get_fast(o, k) {
                        Some(v) => v,
                        None => tri!(self.get_v(o, k)),
                    };
                    self.push(v);
                }
                Op::SetProp(c) => {
                    let v = self.pop();
                    let o = self.pop();
                    let k = key_of!(c);
                    if !self.set_fast(o, k, v) {
                        tri!(self.set_v(o, k, v, code.strict));
                    }
                    self.push(v);
                }
                Op::GetElem => {
                    let k = self.pop();
                    let o = self.pop();
                    let v = tri!(self.get_elem(o, k));
                    self.push(v);
                }
                Op::GetElemKeep => {
                    let k = self.pop();
                    let o = self.peek(0);
                    let v = tri!(self.get_elem(o, k));
                    self.push(v);
                }
                Op::SetElem => {
                    let v = self.pop();
                    let k = self.pop();
                    let o = self.pop();
                    tri!(self.set_elem(o, k, v, code.strict));
                    self.push(v);
                }
                Op::DelProp(c) => {
                    let o = self.pop();
                    let k = key_of!(c);
                    let r = tri!(self.delete_v(o, k, code.strict));
                    self.push(Val::Bool(r));
                }
                Op::DelElem => {
                    let k = self.pop();
                    let o = self.pop();
                    let k = tri!(self.to_key(k));
                    let r = tri!(self.delete_v(o, k, code.strict));
                    self.push(Val::Bool(r));
                }
                Op::In => {
                    let o = self.pop();
                    let k = self.pop();
                    let Val::Obj(oid) = o else {
                        let ks = self.short_display(k);
                        let os = self.short_display(o);
                        let e = self.type_error(&alloc::format!(
                            "Cannot use 'in' operator to search for {ks} in {os}"
                        ));
                        throw!(e);
                    };
                    let k = tri!(self.to_key(k));
                    let r = tri!(self.has_property(oid, k));
                    self.push(Val::Bool(r));
                }
                Op::InstanceOf => {
                    let c = self.pop();
                    let v = self.pop();
                    let r = tri!(self.instance_of_v(v, c));
                    self.push(Val::Bool(r));
                }
                Op::GetSuper(c) => {
                    let k = key_of!(c);
                    let v = tri!(self.super_get(k));
                    self.push(v);
                }
                Op::GetSuperElem => {
                    let k = self.pop();
                    let k = tri!(self.to_key(k));
                    let v = tri!(self.super_get(k));
                    self.push(v);
                }
                Op::SetSuper(c) => {
                    let v = self.pop();
                    let k = key_of!(c);
                    tri!(self.super_set(k, v));
                    self.push(v);
                }
                Op::SetSuperElem => {
                    let v = self.pop();
                    let k = self.pop();
                    let k = tri!(self.to_key(k));
                    tri!(self.super_set(k, v));
                    self.push(v);
                }
                Op::Call(argc) | Op::New(argc) => {
                    let is_new = matches!(op, Op::New(_));
                    sync!();
                    tri!(self.safe_point());
                    let argc = argc as usize;
                    let n = self.vm.stack.len();
                    if n < argc + 2 {
                        let e = self.type_error("internal error: call stack");
                        throw!(e);
                    }
                    let fpos = n - argc - 1;
                    let f = self.vm.stack[fpos];
                    match self.dispatch_call(f, fpos, argc, is_new) {
                        Ok(true) => {
                            reload!();
                        }
                        Ok(false) => {}
                        Err(e) => throw!(e),
                    }
                }
                Op::CallSpread | Op::NewSpread => {
                    let is_new = matches!(op, Op::NewSpread);
                    sync!();
                    tri!(self.safe_point());
                    let arr = self.pop();
                    let args = tri!(self.array_like_to_vec(arr));
                    if self.vm.stack.len() + args.len() > MAX_STACK {
                        let e = self.range_error("Maximum call stack size exceeded");
                        throw!(e);
                    }
                    let argc = args.len();
                    self.vm.stack.extend_from_slice(&args);
                    let n = self.vm.stack.len();
                    let fpos = n - argc - 1;
                    let f = self.vm.stack[fpos];
                    match self.dispatch_call(f, fpos, argc, is_new) {
                        Ok(true) => {
                            reload!();
                        }
                        Ok(false) => {}
                        Err(e) => throw!(e),
                    }
                }
                Op::SuperCall(argc) => {
                    sync!();
                    let n = self.vm.stack.len();
                    let args: Vec<Val> = self.vm.stack[n - argc as usize..].to_vec();
                    self.vm.stack.truncate(n - argc as usize);
                    let r = tri!(self.super_call(&args));
                    self.push(r);
                }
                Op::SuperCallSpread => {
                    sync!();
                    let arr = self.pop();
                    let args = tri!(self.array_like_to_vec(arr));
                    let r = tri!(self.super_call(&args));
                    self.push(r);
                }
                Op::Add => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (a, b) {
                        (Val::Int(x), Val::Int(y)) => match x.checked_add(y) {
                            Some(z) => Val::Int(z),
                            None => Val::Num(x as f64 + y as f64),
                        },
                        (Val::Num(x), Val::Num(y)) => Val::num(x + y),
                        (Val::Int(x), Val::Num(y)) => Val::num(x as f64 + y),
                        (Val::Num(x), Val::Int(y)) => Val::num(x + y as f64),
                        _ => tri!(self.add_slow(a, b)),
                    };
                    self.push(r);
                }
                Op::Sub => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (a, b) {
                        (Val::Int(x), Val::Int(y)) => match x.checked_sub(y) {
                            Some(z) => Val::Int(z),
                            None => Val::Num(x as f64 - y as f64),
                        },
                        _ => {
                            let x = tri!(self.to_num(a));
                            let y = tri!(self.to_num(b));
                            Val::num(x - y)
                        }
                    };
                    self.push(r);
                }
                Op::Mul => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (a, b) {
                        (Val::Int(x), Val::Int(y)) => match x.checked_mul(y) {
                            Some(0) if x < 0 || y < 0 => Val::Num(-0.0),
                            Some(z) => Val::Int(z),
                            None => Val::Num(x as f64 * y as f64),
                        },
                        _ => {
                            let x = tri!(self.to_num(a));
                            let y = tri!(self.to_num(b));
                            Val::num(x * y)
                        }
                    };
                    self.push(r);
                }
                Op::Div => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (a, b) {
                        (Val::Int(x), Val::Int(y))
                            if y != 0
                                && x % y == 0
                                && !(x == 0 && y < 0)
                                && !(x == i32::MIN && y == -1) =>
                        {
                            Val::Int(x / y)
                        }
                        _ => {
                            let x = tri!(self.to_num(a));
                            let y = tri!(self.to_num(b));
                            Val::num(x / y)
                        }
                    };
                    self.push(r);
                }
                Op::Mod => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (a, b) {
                        (Val::Int(x), Val::Int(y)) if y > 0 && x >= 0 => Val::Int(x % y),
                        _ => {
                            let x = tri!(self.to_num(a));
                            let y = tri!(self.to_num(b));
                            Val::num(crate::math::fmod(x, y))
                        }
                    };
                    self.push(r);
                }
                Op::Exp => {
                    let b = self.pop();
                    let a = self.pop();
                    let x = tri!(self.to_num(a));
                    let y = tri!(self.to_num(b));
                    self.push(Val::num(crate::libm::js_pow(x, y)));
                }
                Op::BitAnd | Op::BitOr | Op::BitXor | Op::Shl | Op::Shr | Op::UShr => {
                    let b = self.pop();
                    let a = self.pop();
                    let x = tri!(self.to_i32(a));
                    let y = tri!(self.to_i32(b));
                    let r = match op {
                        Op::BitAnd => Val::Int(x & y),
                        Op::BitOr => Val::Int(x | y),
                        Op::BitXor => Val::Int(x ^ y),
                        Op::Shl => Val::Int(x.wrapping_shl(y as u32 & 31)),
                        Op::Shr => Val::Int(x >> (y as u32 & 31)),
                        _ => Val::num(((x as u32) >> (y as u32 & 31)) as f64),
                    };
                    self.push(r);
                }
                Op::Eq | Op::Ne => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = tri!(self.loose_eq(a, b));
                    self.push(Val::Bool(if matches!(op, Op::Eq) { r } else { !r }));
                }
                Op::StrictEq | Op::StrictNe => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = self.strict_eq(a, b);
                    self.push(Val::Bool(if matches!(op, Op::StrictEq) { r } else { !r }));
                }
                Op::Lt | Op::Gt | Op::Le | Op::Ge => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (a, b) {
                        (Val::Int(x), Val::Int(y)) => match op {
                            Op::Lt => x < y,
                            Op::Gt => x > y,
                            Op::Le => x <= y,
                            _ => x >= y,
                        },
                        _ => tri!(self.compare_slow(op, a, b)),
                    };
                    self.push(Val::Bool(r));
                }
                Op::Not => {
                    let v = self.pop();
                    let t = self.truthy(v);
                    self.push(Val::Bool(!t));
                }
                Op::Neg => {
                    let v = self.pop();
                    let r = match v {
                        Val::Int(0) => Val::Num(-0.0),
                        Val::Int(i) if i != i32::MIN => Val::Int(-i),
                        _ => {
                            let x = tri!(self.to_num(v));
                            Val::num(-x)
                        }
                    };
                    self.push(r);
                }
                Op::Plus | Op::ToNumeric => {
                    let v = self.pop();
                    let r = match v {
                        Val::Int(_) | Val::Num(_) => v,
                        _ => Val::num(tri!(self.to_num(v))),
                    };
                    self.push(r);
                }
                Op::BitNot => {
                    let v = self.pop();
                    let x = tri!(self.to_i32(v));
                    self.push(Val::Int(!x));
                }
                Op::Typeof => {
                    let v = self.pop();
                    let t = self.typeof_name(v);
                    let s = self.nstr(t);
                    self.push(Val::Str(s));
                }
                Op::Inc | Op::Dec => {
                    let v = self.pop();
                    let d = if matches!(op, Op::Inc) { 1 } else { -1 };
                    let r = match v {
                        Val::Int(i) => match i.checked_add(d) {
                            Some(z) => Val::Int(z),
                            None => Val::Num(i as f64 + d as f64),
                        },
                        _ => {
                            let x = tri!(self.to_num(v));
                            Val::num(x + d as f64)
                        }
                    };
                    self.push(r);
                }
                Op::ToPropKey => {
                    let v = self.pop();
                    let r = match v {
                        Val::Int(_) | Val::Str(_) | Val::Sym(_) => v,
                        _ => {
                            let k = tri!(self.to_key(v));
                            self.key_to_val(k)
                        }
                    };
                    self.push(r);
                }
                Op::ToStr => {
                    let v = self.pop();
                    let s = tri!(self.to_str(v));
                    self.push(Val::Str(s));
                }
                Op::Concat(n) => {
                    let n = n as usize;
                    let len = self.vm.stack.len();
                    let parts: Vec<Val> = self.vm.stack[len.saturating_sub(n)..].to_vec();
                    self.vm.stack.truncate(len.saturating_sub(n));
                    let s = tri!(self.concat_many(&parts));
                    self.push(Val::Str(s));
                }
                Op::Jump(t) => {
                    if (t as usize) < pc {
                        sync!();
                        tri!(self.safe_point());
                    }
                    pc = t as usize;
                }
                Op::JumpIfFalse(t) => {
                    let v = self.pop();
                    let c = match v {
                        Val::Bool(b) => b,
                        _ => self.truthy(v),
                    };
                    if !c {
                        pc = t as usize;
                    }
                }
                Op::JumpIfTrue(t) => {
                    let v = self.pop();
                    let c = match v {
                        Val::Bool(b) => b,
                        _ => self.truthy(v),
                    };
                    if c {
                        if (t as usize) < pc {
                            sync!();
                            tri!(self.safe_point());
                        }
                        pc = t as usize;
                    }
                }
                Op::JumpIfFalseKeep(t) => {
                    let v = self.peek(0);
                    if !self.truthy(v) {
                        pc = t as usize;
                    } else {
                        self.vm.stack.pop();
                    }
                }
                Op::JumpIfTrueKeep(t) => {
                    let v = self.peek(0);
                    if self.truthy(v) {
                        pc = t as usize;
                    } else {
                        self.vm.stack.pop();
                    }
                }
                Op::JumpIfNotNullishKeep(t) => {
                    let v = self.peek(0);
                    if !v.is_nullish() {
                        pc = t as usize;
                    } else {
                        self.vm.stack.pop();
                    }
                }
                Op::JumpIfNullish(t) => {
                    if self.peek(0).is_nullish() {
                        pc = t as usize;
                    }
                }
                Op::JumpIfNotUndef(t) => {
                    let v = self.pop();
                    if !v.is_undef() {
                        pc = t as usize;
                    }
                }
                Op::NewObject => {
                    let o = self.new_plain();
                    self.push(Val::Obj(o));
                }
                Op::NewArray(n) => {
                    let p = self.intr.array_proto;
                    let o = self.alloc(
                        Some(p),
                        ObjKind::Array(ArrayData {
                            dense: Vec::with_capacity((n as usize).min(1024)),
                            len: 0,
                        }),
                    );
                    self.push(Val::Obj(o));
                }
                Op::ArrPush => {
                    let v = self.pop();
                    if let Val::Obj(a) = self.peek(0) {
                        let len = self.array_len(a);
                        if !self.array_put_dense(a, len, v) {
                            tri!(self.create_data(a, PropKey::Idx(len), v));
                        }
                    }
                }
                Op::Unsupported(c) => {
                    let msg = match code.consts.get(c as usize) {
                        Some(Const::Str(s)) => self.rust_string(*s),
                        _ => alloc::string::String::from("not supported"),
                    };
                    tri!(Err(self.type_error(&msg)));
                }
                Op::ConstArray(ci) => {
                    let n = match code.consts.get(ci as usize) {
                        Some(Const::Nums(v)) => v.len(),
                        Some(Const::Strs(v)) => v.len(),
                        _ => 0,
                    };
                    let mut dense: Vec<Val> = Vec::with_capacity(n);
                    match code.consts.get(ci as usize) {
                        Some(Const::Nums(v)) => dense.extend(v.iter().map(|x| Val::num(*x))),
                        Some(Const::Strs(v)) => dense.extend(v.iter().map(|s| Val::Str(*s))),
                        _ => {}
                    }
                    self.heap.charge(n * 16);
                    tri!(self.check_mem());
                    let p = self.intr.array_proto;
                    let o = self.alloc(
                        Some(p),
                        ObjKind::Array(ArrayData {
                            dense,
                            len: n as u32,
                        }),
                    );
                    self.push(Val::Obj(o));
                }
                Op::ArrHole => {
                    if let Val::Obj(a) = self.peek(0)
                        && let ObjKind::Array(d) = &mut self.om(a).kind
                    {
                        d.dense.push(Val::Empty);
                        d.len += 1;
                    }
                }
                Op::ArrSpread => {
                    let it = self.pop();
                    if let Val::Obj(a) = self.peek(0) {
                        sync!();
                        tri!(self.spread_into(a, it));
                    }
                }
                Op::InitProp(c) => {
                    let v = self.pop();
                    let k = key_of!(c);
                    if let Val::Obj(o) = self.peek(0) {
                        tri!(self.create_data(o, k, v));
                    }
                }
                Op::InitElem => {
                    let v = self.pop();
                    let k = self.pop();
                    let k = tri!(self.to_key(k));
                    if let Val::Obj(o) = self.peek(0) {
                        tri!(self.create_data(o, k, v));
                    }
                }
                Op::InitMethod(c, kind) => {
                    let f = self.pop();
                    let k = key_of!(c);
                    if let Val::Obj(o) = self.peek(0) {
                        tri!(self.define_method_v(o, k, f, kind));
                    }
                }
                Op::InitMethodElem(kind) => {
                    let f = self.pop();
                    let k = self.pop();
                    let k = tri!(self.to_key(k));
                    if let Val::Obj(o) = self.peek(0) {
                        self.set_fn_name(f, k, kind & 3);
                        tri!(self.define_method_v(o, k, f, kind));
                    }
                }
                Op::CopyProps => {
                    let src = self.pop();
                    if let Val::Obj(o) = self.peek(0) {
                        tri!(self.copy_data_props(o, src, &[]));
                    }
                }
                Op::SetProtoLit => {
                    let p = self.pop();
                    if let Val::Obj(o) = self.peek(0) {
                        match p {
                            Val::Obj(p) => {
                                self.set_proto(o, Some(p));
                            }
                            Val::Null => {
                                self.set_proto(o, None);
                            }
                            _ => {}
                        }
                    }
                }
                Op::ObjRest => {
                    let keys = self.pop();
                    let src = self.pop();
                    let ex = tri!(self.array_like_to_vec(keys));
                    let mut exk = Vec::new();
                    for k in ex {
                        exk.push(tri!(self.to_key(k)));
                    }
                    let o = self.new_plain();
                    tri!(self.copy_data_props(o, src, &exk));
                    self.push(Val::Obj(o));
                }
                Op::SetFnName => {
                    let f = self.peek(0);
                    let k = self.peek(1);
                    let k = tri!(self.to_key(k));
                    self.set_fn_name(f, k, 0);
                }
                Op::Closure(c) => {
                    let v = self.make_closure(&code, c);
                    self.push(v);
                }
                Op::MakeClass(c, has_heritage) => {
                    let heritage = if has_heritage != 0 {
                        self.pop()
                    } else {
                        Val::Empty
                    };
                    sync!();
                    let (f, p) = tri!(self.make_class(&code, c, heritage));
                    self.push(Val::Obj(f));
                    self.push(Val::Obj(p));
                }
                Op::SetFieldInit(c) => {
                    let keys = self.pop();
                    if let Val::Obj(f) = self.peek(0) {
                        let ks = tri!(self.array_like_to_vec(keys));
                        let home = {
                            let pk = self.key(N::prototype);
                            match self.get_own_plain(f, pk) {
                                Some(crate::object::Own::Data(Val::Obj(p), _)) => Some(p),
                                _ => None,
                            }
                        };
                        let init = self.make_closure(&code, c);
                        if let Val::Obj(i) = init
                            && let Some(cl) = self.closure_mut(i)
                        {
                            cl.home = home;
                            cl.field_keys = ks;
                        }
                        if let Some(cl) = self.closure_mut(f) {
                            cl.fields = init.as_obj();
                        }
                    }
                }
                Op::RunStaticInit(c) => {
                    let keys = self.pop();
                    if let Val::Obj(f) = self.peek(0) {
                        let ks = tri!(self.array_like_to_vec(keys));
                        let init = self.make_closure(&code, c);
                        if let Val::Obj(i) = init
                            && let Some(cl) = self.closure_mut(i)
                        {
                            cl.home = Some(f);
                            cl.field_keys = ks;
                        }
                        sync!();
                        tri!(self.call_v(init, Val::Obj(f), &[]));
                    }
                }
                Op::InitFields => {
                    sync!();
                    let this = self.top().this;
                    let func = self.top().func;
                    if let Some(f) = func {
                        tri!(self.run_field_init(f, this));
                    }
                }
                Op::FieldKey(i) => {
                    let func = self.top().func;
                    let v = func
                        .and_then(|f| {
                            self.closure(f)
                                .and_then(|c| c.field_keys.get(i as usize).copied())
                        })
                        .unwrap_or(Val::Undef);
                    self.push(v);
                }
                Op::DefineField(c) => {
                    let v = self.pop();
                    let k = key_of!(c);
                    if let Val::Obj(o) = self.peek(0)
                        && !tri!(self.create_data(o, k, v))
                    {
                        let ks = self.key_display(k);
                        let e = self.type_error(&alloc::format!("Cannot define property {ks}"));
                        throw!(e);
                    }
                }
                Op::DefineFieldElem => {
                    let v = self.pop();
                    let k = self.pop();
                    let k = tri!(self.to_key(k));
                    if let Val::Obj(o) = self.peek(0)
                        && !tri!(self.create_data(o, k, v))
                    {
                        let ks = self.key_display(k);
                        let e = self.type_error(&alloc::format!("Cannot define property {ks}"));
                        throw!(e);
                    }
                }
                Op::GetIter(s) | Op::GetAsyncIter(s) => {
                    let v = self.pop();
                    sync!();
                    let (it, next) = if matches!(op, Op::GetAsyncIter(_)) {
                        tri!(self.get_async_iterator(v))
                    } else {
                        tri!(self.get_iterator(v))
                    };
                    let b = base + s as usize;
                    if b + 2 < self.vm.stack.len() {
                        self.vm.stack[b] = it;
                        self.vm.stack[b + 1] = next;
                        self.vm.stack[b + 2] = Val::Bool(false);
                    }
                }
                Op::IterStep(s, t) => {
                    sync!();
                    match tri!(self.iter_step(base + s as usize)) {
                        Some(v) => self.push(v),
                        None => pc = t as usize,
                    }
                }
                Op::IterValue(s) => {
                    sync!();
                    let v = tri!(self.iter_step(base + s as usize)).unwrap_or(Val::Undef);
                    self.push(v);
                }
                Op::IterClose(s) => {
                    sync!();
                    tri!(self.iter_close(base + s as usize, false));
                }
                Op::IterCloseQuiet(s) => {
                    sync!();
                    let _ = self.iter_close(base + s as usize, true);
                }
                Op::IterRest(s) => {
                    sync!();
                    let mut out = Vec::new();
                    while let Some(v) = tri!(self.iter_step(base + s as usize)) {
                        out.push(v);
                        if out.len() & 1023 == 0 {
                            tri!(self.check_mem());
                        }
                    }
                    let a = self.new_array_from(out);
                    self.push(Val::Obj(a));
                }
                Op::ForInStart(s) => {
                    let v = self.pop();
                    sync!();
                    let it = tri!(self.for_in_start(v));
                    if let Some(slot) = self.vm.stack.get_mut(base + s as usize) {
                        *slot = it;
                    }
                }
                Op::ForInNext(s, t) => {
                    let it = self
                        .vm
                        .stack
                        .get(base + s as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    sync!();
                    match tri!(self.for_in_next(it)) {
                        Some(k) => self.push(k),
                        None => pc = t as usize,
                    }
                }
                Op::AsyncIterStep(s, t) => {
                    sync!();
                    let b = base + s as usize;
                    if t == u32::MAX {
                        // Call next(); the result (a promise) is awaited by the next op.
                        let it = self.vm.stack.get(b).copied().unwrap_or(Val::Undef);
                        let next = self.vm.stack.get(b + 1).copied().unwrap_or(Val::Undef);
                        let r = tri!(self.call_v(next, it, &[]));
                        self.push(r);
                    } else {
                        let r = self.pop();
                        let Val::Obj(ro) = r else {
                            let e = self.type_error("Iterator result is not an object");
                            throw!(e);
                        };
                        let dk = self.key(N::done);
                        let d = tri!(self.get_p(ro, dk, r));
                        if self.truthy(d) {
                            if let Some(x) = self.vm.stack.get_mut(b + 2) {
                                *x = Val::Bool(true);
                            }
                            pc = t as usize;
                        } else {
                            let vk = self.key(N::value);
                            let v = tri!(self.get_p(ro, vk, r));
                            self.push(v);
                        }
                    }
                }
                Op::Throw => {
                    let v = self.pop();
                    sync!();
                    self.record_throw_site(v);
                    return Err(Exception(Exc::Throw(v)));
                }
                Op::Rethrow(l) => {
                    let v = self
                        .vm
                        .stack
                        .get(base + l as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    sync!();
                    return Err(Exception(Exc::Throw(v)));
                }
                Op::ThrowConst(_) => {
                    let e = self.type_error("Assignment to constant variable.");
                    throw!(e);
                }
                Op::ThrowRef(c) => {
                    let s = self.const_str_val(&code, c);
                    let m = self.rust_string(s);
                    let e = self.reference_error(&m);
                    throw!(e);
                }
                Op::Return | Op::ReturnUndef => {
                    let v = if matches!(op, Op::Return) {
                        self.pop()
                    } else {
                        Val::Undef
                    };
                    sync!();
                    match self.complete_frame(v) {
                        Ok(Some(v)) => return Ok(v),
                        Ok(None) => {
                            reload!();
                        }
                        Err(e) => return Err(e),
                    }
                }
                Op::GenStart => {
                    sync!();
                    let r = tri!(self.gen_start());
                    match r {
                        Some(v) => return Ok(v),
                        None => {
                            reload!();
                        }
                    }
                }
                Op::Yield => {
                    let v = self.pop();
                    sync!();
                    let is_entry = self.top().entry && self.top().co.is_some();
                    if !is_entry {
                        let e = self.type_error("yield outside a running generator");
                        throw!(e);
                    }
                    self.suspend_top(CoState::Yielded);
                    self.vm.signal = Signal::Yield;
                    return Ok(v);
                }
                Op::YieldStar(s) => {
                    // The received value is on the stack; the resume mode is in the frame's
                    // pending state (Next here; throw/return arrive as exceptions and are
                    // forwarded by the handler-less path below).
                    let received = self.pop();
                    sync!();
                    match tri!(self.yield_star_step(base + s as usize, received, Resume::Next)) {
                        YieldStar::Done(v) | YieldStar::Return(v) => {
                            self.push(v);
                            pc += 1; // skip the loop's Jump
                        }
                        YieldStar::Yield(v) => {
                            self.top().pc = pc as u32;
                            self.suspend_top(CoState::Yielded);
                            self.vm.signal = Signal::Yield;
                            return Ok(v);
                        }
                    }
                }
                Op::Await => {
                    let v = self.pop();
                    sync!();
                    match tri!(self.await_value(v)) {
                        Some(r) => return Ok(r),
                        None => {
                            reload!();
                        }
                    }
                }
                Op::This => {
                    let t = self.top().this;
                    self.push(t.or_undef());
                }
                Op::ThisChk => {
                    let t = self.top().this;
                    if t.is_empty() {
                        let e = self.reference_error(
                            "Must call super constructor in derived class before accessing 'this' or returning from derived constructor",
                        );
                        throw!(e);
                    }
                    self.push(t);
                }
                Op::NewTarget => {
                    let t = self.top().new_target;
                    self.push(t);
                }
                Op::ArgumentsObj => {
                    let a = self.make_arguments(base);
                    self.push(Val::Obj(a));
                }
                Op::RestArgs(n) => {
                    let a = self.rest_args(base, n as usize);
                    self.push(Val::Obj(a));
                }
                Op::TemplateObj(c) => {
                    let v = self.template_object(&code, c);
                    self.push(v);
                }
                Op::RegExp(c) => {
                    let (p, f) = match code.consts.get(c as usize) {
                        Some(Const::Regex(p, f)) => (*p, *f),
                        _ => (self.nstr(N::empty), self.nstr(N::empty)),
                    };
                    let r = tri!(self.regexp_create(Val::Str(p), Val::Str(f), Val::Undef));
                    self.push(Val::Obj(r));
                }
                Op::CheckCoercible => {
                    let v = self.peek(0);
                    if v.is_nullish() {
                        let what = if v.is_null() { "null" } else { "undefined" };
                        let e = self.type_error(&alloc::format!(
                            "Cannot destructure '{what}' as it is {what}."
                        ));
                        throw!(e);
                    }
                }
                Op::Nop => {}
                Op::ThisProp(c) | Op::ThisPropKeep(c) => {
                    let o = self.top().this.or_undef();
                    let k = key_of!(c);
                    pc += 1;
                    let v = match self.get_fast(o, k) {
                        Some(v) => v,
                        None => {
                            sync!();
                            tri!(self.get_v(o, k))
                        }
                    };
                    if matches!(op, Op::ThisPropKeep(_)) {
                        self.push(o);
                    }
                    self.push(v);
                }
                Op::LocalProp(l, c) | Op::LocalPropKeep(l, c) => {
                    let o = self
                        .vm
                        .stack
                        .get(base + l as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    let k = key_of!(c);
                    pc += 1;
                    let v = match self.get_fast(o, k) {
                        Some(v) => v,
                        None => {
                            sync!();
                            tri!(self.get_v(o, k))
                        }
                    };
                    if matches!(op, Op::LocalPropKeep(..)) {
                        self.push(o);
                    }
                    self.push(v);
                }
                Op::Local2(a, b) => {
                    let va = self
                        .vm
                        .stack
                        .get(base + a as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    let vb = self
                        .vm
                        .stack
                        .get(base + b as usize)
                        .copied()
                        .unwrap_or(Val::Undef);
                    pc += 1;
                    self.push(va);
                    self.push(vb);
                }
                Op::PropProp(c, d) => {
                    let o = self.pop();
                    let k = key_of!(c);
                    let v = match self.get_fast(o, k) {
                        Some(v) => v,
                        None => {
                            sync!();
                            tri!(self.get_v(o, k))
                        }
                    };
                    pc += 1;
                    let k2 = key_of!(d);
                    let v2 = match self.get_fast(v, k2) {
                        Some(v) => v,
                        None => {
                            sync!();
                            tri!(self.get_v(v, k2))
                        }
                    };
                    self.push(v2);
                }
                Op::SetPropPop(c) => {
                    let v = self.pop();
                    let o = self.pop();
                    let k = key_of!(c);
                    if !self.set_fast(o, k, v) {
                        tri!(self.set_v(o, k, v, code.strict));
                    }
                    pc += 1;
                }
                Op::StrictEqJf | Op::StrictNeJf => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = self.strict_eq(a, b);
                    let r = if matches!(op, Op::StrictEqJf) { r } else { !r };
                    match code.ops.get(pc) {
                        Some(Op::JumpIfFalse(t)) if !r => pc = *t as usize,
                        _ => pc += 1,
                    }
                }
                Op::LtJf | Op::GtJf | Op::LeJf | Op::GeJf => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (a, b) {
                        (Val::Int(x), Val::Int(y)) => match op {
                            Op::LtJf => x < y,
                            Op::GtJf => x > y,
                            Op::LeJf => x <= y,
                            _ => x >= y,
                        },
                        _ => {
                            let plain = match op {
                                Op::LtJf => Op::Lt,
                                Op::GtJf => Op::Gt,
                                Op::LeJf => Op::Le,
                                _ => Op::Ge,
                            };
                            sync!();
                            tri!(self.compare_slow(plain, a, b))
                        }
                    };
                    match code.ops.get(pc) {
                        Some(Op::JumpIfFalse(t)) if !r => pc = *t as usize,
                        _ => pc += 1,
                    }
                }
                Op::StrictEqInt(n) => {
                    let a = self.pop();
                    let r = self.strict_eq(a, Val::Int(n));
                    pc += 1;
                    self.push(Val::Bool(r));
                }
                Op::StrictEqNull | Op::StrictNeNull => {
                    let a = self.pop();
                    let r = self.strict_eq(a, Val::Null);
                    pc += 1;
                    self.push(Val::Bool(if matches!(op, Op::StrictEqNull) {
                        r
                    } else {
                        !r
                    }));
                }
            }
        }
    }

    // ---- the pieces of the loop ----

    /// Steps, interrupts, the collector. Called on calls and backward jumps.
    #[inline]
    fn safe_point(&mut self) -> Res<()> {
        let w = core::mem::take(&mut self.heap.work) >> 4;
        self.steps += 1 + w;
        self.poll_steps += 1 + w;
        if self.heap.gc_wanted {
            self.maybe_gc()?;
        }
        if self.poll_steps >= self.next_poll
            || (self.limits.max_steps != 0 && self.steps > self.limits.max_steps)
        {
            self.poll()?;
        }
        Ok(())
    }

    fn tdz_error(&mut self, code: &Code, n: u32) -> Exception {
        let s = self.const_str_val(code, n);
        let name = self.rust_string(s);
        self.reference_error(&alloc::format!(
            "Cannot access '{name}' before initialization"
        ))
    }

    fn record_throw_site(&mut self, v: Val) {
        if let Val::Obj(o) = v
            && let ObjKind::Error(_) = self.o(o).kind
        {
            return;
        }
        self.last_site = self.current_site();
    }

    /// Calls the function at stack[fpos] with argc arguments (this at fpos-1). Ok(true) when a
    /// script frame was pushed (the loop reloads), Ok(false) when the result is on the stack.
    fn dispatch_call(&mut self, f: Val, fpos: usize, argc: usize, is_new: bool) -> Res<bool> {
        let mut f = f;
        let mut argc = argc;
        let mut new_target = if is_new { f } else { Val::Undef };
        for _ in 0..64 {
            let Val::Obj(fo) = f else {
                if is_new {
                    return Err(self.not_constructor_at(f));
                }
                return Err(self.not_callable_at(f));
            };
            match &self.o(fo).kind {
                ObjKind::Closure(_) => {
                    let flags = self.o(fo).flags;
                    if is_new {
                        if flags & CONSTRUCTOR == 0 {
                            return Err(self.not_constructor_at(f));
                        }
                        let this = self.construct_this(fo, new_target)?;
                        self.vm.stack[fpos - 1] = this;
                        self.push_frame(fo, argc, new_target, false, true)?;
                    } else {
                        if flags & CLASS_CTOR != 0 {
                            let n = self.fn_name(fo);
                            return Err(self.type_error(&alloc::format!(
                                "Class constructor {n} cannot be invoked without 'new'"
                            )));
                        }
                        self.push_frame(fo, argc, Val::Undef, false, false)?;
                    }
                    return Ok(true);
                }
                ObjKind::Bound(b) => {
                    let (target, bthis, bargs) = (b.target, b.this, b.args.clone());
                    if !bargs.is_empty() {
                        if self.vm.stack.len() + bargs.len() > MAX_STACK {
                            return Err(self.range_error("Maximum call stack size exceeded"));
                        }
                        let at = fpos + 1;
                        let tail: Vec<Val> = self.vm.stack.drain(at..).collect();
                        self.vm.stack.extend_from_slice(&bargs);
                        self.vm.stack.extend_from_slice(&tail);
                        argc += bargs.len();
                    }
                    self.vm.stack[fpos] = Val::Obj(target);
                    if !is_new {
                        self.vm.stack[fpos - 1] = bthis;
                    } else if let Val::Obj(nt) = new_target
                        && nt == fo
                    {
                        new_target = Val::Obj(target);
                    }
                    f = Val::Obj(target);
                    continue;
                }
                ObjKind::Proxy(p) => {
                    if (is_new && !p.constructor) || (!is_new && !p.callable) {
                        return Err(if is_new {
                            self.not_constructor_at(f)
                        } else {
                            self.not_callable_at(f)
                        });
                    }
                    let this = self.vm.stack[fpos - 1];
                    let args: Vec<Val> = self.vm.stack[fpos + 1..].to_vec();
                    self.enter_call()?;
                    let r = if is_new {
                        self.construct_v(f, &args, new_target)
                    } else {
                        self.proxy_call(fo, this, &args)
                    };
                    self.depth -= 1;
                    self.vm.stack.truncate(fpos - 1);
                    self.push(r?);
                    return Ok(false);
                }
                ObjKind::Native(_) => {
                    if is_new && !self.constructible(f) {
                        return Err(self.not_constructor_at(f));
                    }
                    let this = self.vm.stack[fpos - 1];
                    // Small argument lists go through a buffer on the Rust stack.
                    let mut buf = [Val::Undef; 6];
                    let heap_args: Vec<Val>;
                    let args: &[Val] = if argc <= buf.len() {
                        buf[..argc].copy_from_slice(&self.vm.stack[fpos + 1..fpos + 1 + argc]);
                        &buf[..argc]
                    } else {
                        heap_args = self.vm.stack[fpos + 1..].to_vec();
                        &heap_args
                    };
                    let r = if is_new {
                        self.construct_v(f, args, new_target)
                    } else {
                        self.call_native(fo, this, args, Val::Undef)
                    };
                    self.vm.stack.truncate(fpos - 1);
                    self.push(r?);
                    return Ok(false);
                }
                _ => {
                    if is_new {
                        return Err(self.not_constructor_at(f));
                    }
                    return Err(self.not_callable_at(f));
                }
            }
        }
        Err(self.range_error("Maximum call stack size exceeded"))
    }

    pub(crate) fn add_slow(&mut self, a: Val, b: Val) -> Res<Val> {
        if let (Val::Str(x), Val::Str(y)) = (a, b) {
            return Ok(Val::Str(self.concat(x, y)?));
        }
        let pa = self.to_primitive(a, crate::conv::Hint::Default)?;
        let pb = self.to_primitive(b, crate::conv::Hint::Default)?;
        if matches!(pa, Val::Str(_)) || matches!(pb, Val::Str(_)) {
            let x = self.to_str(pa)?;
            let y = self.to_str(pb)?;
            return Ok(Val::Str(self.concat(x, y)?));
        }
        let x = self.to_num(pa)?;
        let y = self.to_num(pb)?;
        Ok(Val::num(x + y))
    }

    pub(crate) fn concat(&mut self, a: StrId, b: StrId) -> Res<StrId> {
        let la = self.heap.str_len(a);
        let lb = self.heap.str_len(b);
        if la == 0 {
            return Ok(b);
        }
        if lb == 0 {
            return Ok(a);
        }
        let total = la + lb;
        self.check_str_len(total)?;
        if total <= 32 {
            let mut v = Vec::with_capacity(total);
            v.extend_from_slice(self.heap.units(a));
            v.extend_from_slice(self.heap.units(b));
            return Ok(self.heap.new_str_vec(v));
        }
        Ok(self.heap.alloc_str(crate::string::JsStr::Rope {
            l: a,
            r: b,
            len: total as u32,
        }))
    }

    fn concat_many(&mut self, parts: &[Val]) -> Res<StrId> {
        let mut total = 0usize;
        for p in parts {
            if let Val::Str(s) = p {
                total += self.heap.str_len(*s);
            }
        }
        self.check_str_len(total)?;
        let mut v = Vec::with_capacity(total);
        for p in parts {
            if let Val::Str(s) = p {
                v.extend_from_slice(self.heap.units(*s));
            }
        }
        Ok(self.heap.new_str_vec(v))
    }

    pub(crate) fn compare_slow(&mut self, op: Op, a: Val, b: Val) -> Res<bool> {
        // (left-first evaluation of ToPrimitive)
        let pa = self.to_primitive(a, crate::conv::Hint::Number)?;
        let pb = self.to_primitive(b, crate::conv::Hint::Number)?;
        if let (Val::Str(x), Val::Str(y)) = (pa, pb) {
            let ux = self.heap.units(x).to_vec();
            let uy = self.heap.units(y);
            let ord = ux.as_slice().cmp(uy);
            return Ok(match op {
                Op::Lt => ord.is_lt(),
                Op::Gt => ord.is_gt(),
                Op::Le => ord.is_le(),
                _ => ord.is_ge(),
            });
        }
        let x = self.to_num(pa)?;
        let y = self.to_num(pb)?;
        Ok(match op {
            Op::Lt => x < y,
            Op::Gt => x > y,
            Op::Le => x <= y,
            _ => x >= y,
        })
    }

    pub(crate) fn get_elem(&mut self, o: Val, k: Val) -> Res<Val> {
        // Fast paths: dense arrays and strings by integer index.
        if let Val::Int(i) = k
            && i >= 0
        {
            match o {
                Val::Obj(id) => {
                    let obj = self.o(id);
                    if let ObjKind::Array(a) = &obj.kind
                        && let Some(v) = a.dense.get(i as usize)
                        && !v.is_empty()
                    {
                        return Ok(*v);
                    }
                    if let ObjKind::Typed(_) = obj.kind {
                        return Ok(self.typed_get(id, i as usize).unwrap_or(Val::Undef));
                    }
                    return self.get_p(id, PropKey::Idx(i as u32), o);
                }
                Val::Str(s) if (i as usize) < self.heap.str_len(s) => {
                    let c = self.heap.units(s)[i as usize];
                    return Ok(Val::Str(self.char_str(c)));
                }
                _ => {}
            }
        }
        if o.is_nullish() {
            let ks = match k {
                Val::Obj(_) => String::from("[object]"),
                _ => {
                    let pk = self.to_key(k)?;
                    self.key_display(pk)
                }
            };
            let what = if o.is_null() { "null" } else { "undefined" };
            return Err(self.type_error(&alloc::format!(
                "Cannot read properties of {what} (reading '{ks}')"
            )));
        }
        let key = self.to_key(k)?;
        self.get_v(o, key)
    }

    pub(crate) fn set_elem(&mut self, o: Val, k: Val, v: Val, strict: bool) -> Res<()> {
        if let (Val::Obj(id), Val::Int(i)) = (o, k)
            && i >= 0
        {
            let obj = self.o(id);
            if let ObjKind::Array(a) = &obj.kind
                && obj.flags & crate::object::SPARSE == 0
                && (i as usize) < a.dense.len()
                && !a.dense[i as usize].is_empty()
            {
                if let ObjKind::Array(a) = &mut self.om(id).kind {
                    a.dense[i as usize] = v;
                }
                return Ok(());
            }
            if let ObjKind::Typed(_) = obj.kind {
                let n = match v {
                    Val::Int(x) => x as f64,
                    Val::Num(x) => x,
                    _ => self.to_num(v)?,
                };
                self.typed_put(id, i as usize, n);
                return Ok(());
            }
        }
        if o.is_nullish() {
            let ks = match k {
                Val::Obj(_) => String::from("[object]"),
                _ => {
                    let pk = self.to_key(k)?;
                    self.key_display(pk)
                }
            };
            let what = if o.is_null() { "null" } else { "undefined" };
            return Err(self.type_error(&alloc::format!(
                "Cannot set properties of {what} (setting '{ks}')"
            )));
        }
        let key = self.to_key(k)?;
        self.set_v(o, key, v, strict)
    }

    fn delete_v(&mut self, o: Val, k: PropKey, strict: bool) -> Res<bool> {
        let id = match o {
            Val::Obj(id) => id,
            Val::Undef | Val::Null | Val::Empty => {
                return Err(self.type_error("Cannot convert undefined or null to object"));
            }
            _ => self.to_object(o)?,
        };
        let r = self.delete_p(id, k)?;
        if !r && strict {
            let ks = self.key_display(k);
            return Err(self.type_error(&alloc::format!("Cannot delete property '{ks}' of object")));
        }
        Ok(r)
    }

    // ---- globals ----

    fn global_get(&mut self, k: PropKey, for_typeof: bool) -> Res<Val> {
        let gl = self.global_lex;
        if let Some(p) = self.o(gl).props.get(k) {
            let v = p.v;
            if v.is_empty() {
                let ks = self.key_display(k);
                return Err(self.reference_error(&alloc::format!(
                    "Cannot access '{ks}' before initialization"
                )));
            }
            return Ok(v);
        }
        let g = self.global;
        // Fast path: a data property on the global object itself.
        let obj = self.o(g);
        if obj.flags & LAZY_FN == 0
            && !matches!(obj.kind, ObjKind::Host { .. })
            && let Some(p) = obj.props.get(k)
            && p.f & crate::object::ACCESSOR == 0
        {
            return Ok(p.v);
        }
        if self.has_property(g, k)? {
            return self.get_p(g, k, Val::Obj(g));
        }
        if for_typeof {
            return Ok(Val::Undef);
        }
        let ks = self.key_display(k);
        Err(self.reference_error(&alloc::format!("{ks} is not defined")))
    }

    fn global_set(&mut self, k: PropKey, v: Val, strict: bool) -> Res<()> {
        let gl = self.global_lex;
        if let Some(pos) = self.o(gl).props.find(k) {
            let p = self.o(gl).props.ents[pos].1;
            if p.v.is_empty() {
                let ks = self.key_display(k);
                return Err(self.reference_error(&alloc::format!(
                    "Cannot access '{ks}' before initialization"
                )));
            }
            if p.f & W == 0 {
                return Err(self.type_error("Assignment to constant variable."));
            }
            self.om(gl).props.ents[pos].1.v = v;
            return Ok(());
        }
        let g = self.global;
        let obj = self.o(g);
        if obj.flags & LAZY_FN == 0
            && !matches!(obj.kind, ObjKind::Host { .. })
            && let Some(pos) = obj.props.find(k)
        {
            let p = obj.props.ents[pos].1;
            if p.f & (W | crate::object::ACCESSOR) == W {
                self.om(g).props.ents[pos].1.v = v;
                return Ok(());
            }
        }
        if strict && !self.has_property(g, k)? {
            let ks = self.key_display(k);
            return Err(self.reference_error(&alloc::format!("{ks} is not defined")));
        }
        self.set_v(Val::Obj(g), k, v, strict)
    }

    fn declare_global_var(&mut self, k: PropKey) -> Res<()> {
        let gl = self.global_lex;
        if self.o(gl).props.find(k).is_some() {
            let ks = self.key_display(k);
            return Err(self.syntax_error(&alloc::format!(
                "Identifier '{ks}' has already been declared"
            )));
        }
        let g = self.global;
        if !self.has_own_p(g, k)? {
            self.define_own(
                g,
                k,
                Desc {
                    value: Some(Val::Undef),
                    writable: Some(true),
                    enumerable: Some(true),
                    configurable: Some(false),
                    ..Desc::default()
                },
            )?;
        }
        Ok(())
    }

    fn declare_global_lex(&mut self, k: PropKey, is_const: bool) -> Res<()> {
        let gl = self.global_lex;
        let g = self.global;
        let clash = self.o(gl).props.find(k).is_some()
            || matches!(self.get_own_plain(g, k), Some(o) if o.flags() & crate::object::C == 0);
        if clash {
            let ks = self.key_display(k);
            return Err(self.syntax_error(&alloc::format!(
                "Identifier '{ks}' has already been declared"
            )));
        }
        let f = if is_const { 0 } else { W };
        self.om(gl)
            .props
            .insert(k, crate::object::Prop { v: Val::Empty, f });
        Ok(())
    }

    // ---- functions and classes ----

    fn make_closure(&mut self, code: &Code, c: u32) -> Val {
        let Some(Const::Code(fc)) = code.consts.get(c as usize) else {
            return Val::Undef;
        };
        let fc = fc.clone();
        let (env, this, nt, home, func) = {
            let n = self.vm.frames.len();
            let f = &self.vm.frames[n - 1];
            let home = f.func.and_then(|x| self.closure(x).and_then(|c| c.home));
            let func_ctx = match f.func {
                Some(x) => match self.closure(x) {
                    Some(c) if c.code.kind == CodeKind::Arrow => c.func_ctx,
                    _ => Some(x),
                },
                None => None,
            };
            (f.env, f.this, f.new_target, home, func_ctx)
        };
        let is_arrow = fc.kind == CodeKind::Arrow;
        let proto = if fc.is_async && fc.is_generator {
            self.intr.async_generator_function_proto
        } else if fc.is_generator {
            self.intr.generator_function_proto
        } else if fc.is_async {
            self.intr.async_function_proto
        } else {
            self.intr.function_proto
        };
        let ctor = fc.kind == CodeKind::Normal && !fc.is_async && !fc.is_generator;
        let lazy_proto = ctor || fc.is_generator;
        let cl = Closure {
            code: fc,
            env,
            this: if is_arrow { this } else { Val::Undef },
            new_target: if is_arrow { nt } else { Val::Undef },
            home: if is_arrow { home } else { None },
            func_ctx: if is_arrow { func } else { None },
            fields: None,
            field_keys: Vec::new(),
        };
        let id = self.alloc(Some(proto), ObjKind::Closure(Box::new(cl)));
        let obj = self.om(id);
        obj.flags |= LAZY_FN;
        if ctor {
            obj.flags |= CONSTRUCTOR;
        }
        if lazy_proto {
            obj.flags |= LAZY_PROTO;
        }
        Val::Obj(id)
    }

    fn make_class(&mut self, code: &Code, c: u32, heritage: Val) -> Res<(ObjId, ObjId)> {
        let (proto_parent, ctor_parent) = match heritage {
            Val::Empty => (Some(self.intr.object_proto), self.intr.function_proto),
            Val::Null => (None, self.intr.function_proto),
            h => {
                if !self.constructible(h) {
                    let d = self.short_display(h);
                    return Err(self.type_error(&alloc::format!(
                        "Class extends value {d} is not a constructor or null"
                    )));
                }
                let Val::Obj(ho) = h else {
                    return Err(self.type_error("bad heritage"));
                };
                let pk = self.key(N::prototype);
                let pp = self.get_p(ho, pk, h)?;
                let pp = match pp {
                    Val::Obj(p) => Some(p),
                    Val::Null => None,
                    _ => {
                        return Err(self.type_error(
                            "Class extends value does not have valid prototype property",
                        ));
                    }
                };
                (pp, ho)
            }
        };
        let proto = self.alloc(proto_parent, ObjKind::Ordinary);
        let f = self.make_closure(code, c);
        let Val::Obj(fo) = f else {
            return Err(self.type_error("bad class"));
        };
        {
            let obj = self.om(fo);
            obj.proto = Some(ctor_parent);
            obj.flags |= CONSTRUCTOR | CLASS_CTOR;
            obj.flags &= !LAZY_PROTO;
        }
        if let Some(cl) = self.closure_mut(fo) {
            cl.home = Some(proto);
        }
        self.materialize_fn(fo);
        let pk = self.key(N::prototype);
        self.put_prop(fo, pk, Val::Obj(proto), 0);
        let ck = self.key(N::constructor);
        self.put_prop(proto, ck, Val::Obj(fo), WC);
        Ok((fo, proto))
    }

    fn define_method_v(&mut self, o: ObjId, k: PropKey, f: Val, kind: u32) -> Res<()> {
        if let Val::Obj(fo) = f
            && let Some(cl) = self.closure_mut(fo)
        {
            cl.home = Some(o);
        }
        let enumerable = kind & 4 != 0;
        let d = match kind & 3 {
            1 => {
                self.prefix_fn_name(f, "get ");
                Desc {
                    get: Some(f),
                    enumerable: Some(enumerable),
                    configurable: Some(true),
                    ..Desc::default()
                }
            }
            2 => {
                self.prefix_fn_name(f, "set ");
                Desc {
                    set: Some(f),
                    enumerable: Some(enumerable),
                    configurable: Some(true),
                    ..Desc::default()
                }
            }
            _ => Desc {
                value: Some(f),
                writable: Some(true),
                enumerable: Some(enumerable),
                configurable: Some(true),
                ..Desc::default()
            },
        };
        self.define_own(o, k, d)?;
        Ok(())
    }

    fn prefix_fn_name(&mut self, f: Val, prefix: &str) {
        let Val::Obj(fo) = f else { return };
        let nk = self.key(N::name);
        let cur = match self.get_own_plain(fo, nk) {
            Some(crate::object::Own::Data(Val::Str(s), _)) => self.rust_string(s),
            _ => String::new(),
        };
        if cur.starts_with(prefix) {
            return;
        }
        let v = self.str_val(&alloc::format!("{prefix}{cur}"));
        self.put_prop(fo, nk, v, crate::object::C);
    }

    /// Names an anonymous function after a key (`[k]: function(){}`); kind 1/2 add get/set.
    pub(crate) fn set_fn_name(&mut self, f: Val, k: PropKey, kind: u32) {
        let Val::Obj(fo) = f else { return };
        let anon = match &self.o(fo).kind {
            ObjKind::Closure(c) => c.code.name.is_none(),
            _ => false,
        };
        if !anon {
            return;
        }
        let base = match k {
            PropKey::Sym(s) => match self.heap.syms.get(s.0 as usize).and_then(|d| d.desc) {
                Some(d) => alloc::format!("[{}]", self.rust_string(d)),
                None => String::new(),
            },
            _ => self.key_display(k),
        };
        let name = match kind {
            1 => alloc::format!("get {base}"),
            2 => alloc::format!("set {base}"),
            _ => base,
        };
        let nk = self.key(N::name);
        let v = self.str_val(&name);
        self.put_prop(fo, nk, v, crate::object::C);
    }

    fn active_function(&self) -> Option<ObjId> {
        let n = self.vm.frames.len();
        let f = self.vm.frames[n - 1].func?;
        match self.closure(f) {
            Some(c) if c.code.kind == CodeKind::Arrow => c.func_ctx,
            _ => Some(f),
        }
    }

    fn home_object(&self) -> Option<ObjId> {
        let n = self.vm.frames.len();
        let f = self.vm.frames[n - 1].func?;
        self.closure(f).and_then(|c| c.home)
    }

    fn super_get(&mut self, k: PropKey) -> Res<Val> {
        let this = self.top().this;
        if this.is_empty() {
            return Err(self.reference_error(
                "Must call super constructor in derived class before accessing 'this'",
            ));
        }
        let Some(home) = self.home_object() else {
            return Err(self.syntax_error("'super' keyword unexpected here"));
        };
        match self.o(home).proto {
            Some(p) => self.get_p(p, k, this),
            None => Ok(Val::Undef),
        }
    }

    fn super_set(&mut self, k: PropKey, v: Val) -> Res<()> {
        let this = self.top().this;
        let Some(home) = self.home_object() else {
            return Err(self.syntax_error("'super' keyword unexpected here"));
        };
        match self.o(home).proto {
            Some(p) => {
                if !self.set_p(p, k, v, this)? {
                    let ks = self.key_display(k);
                    return Err(self.type_error(&alloc::format!(
                        "Cannot assign to read only property '{ks}' of object"
                    )));
                }
                Ok(())
            }
            None => Err(self.type_error("Cannot set property on null prototype")),
        }
    }

    fn super_call(&mut self, args: &[Val]) -> Res<Val> {
        let Some(f) = self.active_function() else {
            return Err(self.syntax_error("'super' keyword unexpected here"));
        };
        let parent = self.o(f).proto.map_or(Val::Null, Val::Obj);
        if !self.constructible(parent) {
            return Err(self.type_error("Super constructor is not a constructor"));
        }
        let nt = self.top().new_target;
        let r = self.construct_v(parent, args, nt)?;
        if !self.top().this.is_empty() {
            return Err(self.reference_error("Super constructor may only be called once"));
        }
        self.top().this = r;
        self.run_field_init(f, r)?;
        Ok(r)
    }

    fn run_field_init(&mut self, f: ObjId, this: Val) -> Res<()> {
        let init = self.closure(f).and_then(|c| c.fields);
        if let Some(i) = init {
            self.call_v(Val::Obj(i), this, &[])?;
        }
        Ok(())
    }

    fn make_arguments(&mut self, base: usize) -> ObjId {
        let n = self.vm.frames.len();
        let (argc, nparams, extra, func, strict) = {
            let f = &self.vm.frames[n - 1];
            (
                f.argc as usize,
                f.code.nparams as usize,
                f.extra.clone(),
                f.func,
                f.code.strict,
            )
        };
        let mut vals: Vec<Val> = Vec::with_capacity(argc);
        for i in 0..argc.min(nparams) {
            vals.push(self.vm.stack.get(base + i).copied().unwrap_or(Val::Undef));
        }
        vals.extend_from_slice(&extra);
        let op = self.intr.object_proto;
        let a = self.alloc(Some(op), ObjKind::Arguments);
        for (i, v) in vals.iter().enumerate() {
            self.om(a).props.insert(
                PropKey::Idx(i as u32),
                crate::object::Prop { v: *v, f: WEC },
            );
        }
        let lk = self.key(N::length);
        self.put_prop(a, lk, Val::num(vals.len() as f64), WC);
        let ik = PropKey::Sym(self.sym.iterator);
        let av = Val::Obj(self.intr.array_values);
        self.put_prop(a, ik, av, WC);
        if !strict && let Some(f) = func {
            let ck = self.key(N::callee);
            self.put_prop(a, ck, Val::Obj(f), WC);
        }
        a
    }

    fn rest_args(&mut self, base: usize, from: usize) -> ObjId {
        let n = self.vm.frames.len();
        let (argc, nparams, extra) = {
            let f = &self.vm.frames[n - 1];
            (f.argc as usize, f.code.nparams as usize, f.extra.clone())
        };
        let mut vals = Vec::new();
        for i in from..argc.min(nparams) {
            vals.push(self.vm.stack.get(base + i).copied().unwrap_or(Val::Undef));
        }
        if argc > nparams {
            vals.extend_from_slice(&extra);
        }
        self.new_array_from(vals)
    }

    fn template_object(&mut self, code: &Code, c: u32) -> Val {
        let Some(Const::Template(cooked, raw, cache)) = code.consts.get(c as usize) else {
            return Val::Undef;
        };
        if let Some(o) = *cache.borrow() {
            return Val::Obj(o);
        }
        let cv: Vec<Val> = cooked
            .iter()
            .map(|s| s.map_or(Val::Undef, Val::Str))
            .collect();
        let rv: Vec<Val> = raw.iter().map(|s| Val::Str(*s)).collect();
        let arr = self.new_array_from(cv);
        let rarr = self.new_array_from(rv);
        let _ = self.set_integrity(rarr, true);
        let rk = self.key(N::raw);
        self.put_prop(arr, rk, Val::Obj(rarr), 0);
        let _ = self.set_integrity(arr, true);
        *cache.borrow_mut() = Some(arr);
        Val::Obj(arr)
    }

    // ---- generators and async ----

    /// GenStart: the generator object is the call's result.
    fn gen_start(&mut self) -> Res<Option<Val>> {
        let n = self.vm.frames.len();
        let (func, is_async) = {
            let f = &self.vm.frames[n - 1];
            (f.func, f.code.is_async)
        };
        let default = if is_async {
            self.intr.async_generator_proto
        } else {
            self.intr.generator_proto
        };
        let proto = match func {
            Some(f) => {
                let pk = self.key(N::prototype);
                match self.get_p(f, pk, Val::Obj(f))? {
                    Val::Obj(p) => p,
                    _ => default,
                }
            }
            None => default,
        };
        let g = self.alloc(
            Some(proto),
            ObjKind::Coroutine(Box::new(CoData {
                kind: if is_async {
                    CoKind::AsyncGen
                } else {
                    CoKind::Generator
                },
                state: CoState::Start,
                frame: None,
                stack: Vec::new(),
                promise: Val::Undef,
                queue: VecDeque::new(),
            })),
        );
        self.top().co = Some(g);
        let entry = self.top().entry;
        self.suspend_top(CoState::Start);
        if entry {
            self.vm.signal = Signal::Return;
            return Ok(Some(Val::Obj(g)));
        }
        self.push(Val::Obj(g));
        Ok(None)
    }

    /// Await: suspends the frame until the promise settles. Some(v) when `run` returns.
    fn await_value(&mut self, v: Val) -> Res<Option<Val>> {
        let n = self.vm.frames.len();
        let (co, resumed, entry) = {
            let f = &self.vm.frames[n - 1];
            (f.co, f.resumed, f.entry)
        };
        let Some(co) = co else {
            return Err(self.syntax_error("await is only valid in async functions"));
        };
        let p = self.promise_resolve_intrinsic(v)?;
        self.suspend_top(CoState::Awaiting);
        self.await_then(p, co);
        if resumed {
            self.vm.signal = Signal::Await;
            return Ok(Some(Val::Undef));
        }
        // The first await of an async function: the call returns its promise.
        let result = self.co_promise(co);
        if entry {
            self.vm.signal = Signal::Return;
            return Ok(Some(result));
        }
        self.push(result);
        Ok(None)
    }

    /// One step of `yield*` with the received value.
    pub(crate) fn yield_star_step(
        &mut self,
        slot: usize,
        received: Val,
        mode: Resume,
    ) -> Res<YieldStar> {
        let it = self.vm.stack.get(slot).copied().unwrap_or(Val::Undef);
        let next = self.vm.stack.get(slot + 1).copied().unwrap_or(Val::Undef);
        let r = match mode {
            Resume::Next => self.call_v(next, it, &[received])?,
            Resume::Throw => {
                let tk = self.key(N::throw_);
                let t = self.get_v(it, tk)?;
                if t.is_nullish() {
                    let _ = self.iter_close(slot, true);
                    return Err(self.type_error("The iterator does not provide a 'throw' method"));
                }
                self.call_v(t, it, &[received])?
            }
            Resume::Return => {
                let rk = self.key(N::return_);
                let f = self.get_v(it, rk)?;
                if f.is_nullish() {
                    return Ok(YieldStar::Return(received));
                }
                self.call_v(f, it, &[received])?
            }
        };
        let Val::Obj(ro) = r else {
            return Err(self.type_error("Iterator result is not an object"));
        };
        let dk = self.key(N::done);
        let d = self.get_p(ro, dk, r)?;
        if self.truthy(d) {
            let vk = self.key(N::value);
            let v = self.get_p(ro, vk, r)?;
            if mode == Resume::Return {
                return Ok(YieldStar::Return(v));
            }
            return Ok(YieldStar::Done(v));
        }
        // Sync generators pass the inner result through; we yield its value (the outer
        // next() wraps it again), which only differs for exotic result objects.
        let vk = self.key(N::value);
        let v = self.get_p(ro, vk, r)?;
        Ok(YieldStar::Yield(v))
    }

    // ---- iteration ----

    pub(crate) fn get_iterator(&mut self, v: Val) -> Res<(Val, Val)> {
        // Fast path: arrays whose iterator is the builtin one.
        if let Val::Obj(o) = v
            && matches!(self.o(o).kind, ObjKind::Array(_))
            && self.array_iteration_pristine(o)
        {
            let it = self.make_array_iter(v, 1);
            return Ok((Val::Obj(it), Val::Obj(self.intr.array_iter_next)));
        }
        let ik = PropKey::Sym(self.sym.iterator);
        let m = self.get_v(v, ik)?;
        if !self.callable(m) {
            let d = self.short_display(v);
            return Err(self.type_error(&alloc::format!("{d} is not iterable")));
        }
        let it = self.call_v(m, v, &[])?;
        if !matches!(it, Val::Obj(_)) {
            return Err(self.type_error("Result of the Symbol.iterator method is not an object"));
        }
        let nk = self.key(N::next);
        let next = self.get_v(it, nk)?;
        Ok((it, next))
    }

    pub(crate) fn get_async_iterator(&mut self, v: Val) -> Res<(Val, Val)> {
        let ak = PropKey::Sym(self.sym.async_iterator);
        let m = self.get_v(v, ak)?;
        if m.is_nullish() {
            let (it, next) = self.get_iterator(v)?;
            let p = self.intr.async_from_sync_iterator_proto;
            let o = self.alloc(Some(p), ObjKind::AsyncFromSync(it, next));
            let nk = self.key(N::next);
            let n = self.get_v(Val::Obj(o), nk)?;
            return Ok((Val::Obj(o), n));
        }
        let it = self.call_v(m, v, &[])?;
        if !matches!(it, Val::Obj(_)) {
            return Err(
                self.type_error("Result of the Symbol.asyncIterator method is not an object")
            );
        }
        let nk = self.key(N::next);
        let next = self.get_v(it, nk)?;
        Ok((it, next))
    }

    pub(crate) fn make_array_iter(&mut self, v: Val, kind: u8) -> ObjId {
        let p = self.intr.array_iterator_proto;
        self.alloc(
            Some(p),
            ObjKind::ArrayIter(Box::new(ArrayIterData {
                obj: v,
                idx: 0,
                kind,
                done: false,
            })),
        )
    }

    /// Whether array iteration on `o` is the builtin (so for-of can index directly).
    fn array_iteration_pristine(&mut self, o: ObjId) -> bool {
        let ik = PropKey::Sym(self.sym.iterator);
        if self.o(o).props.get(ik).is_some() || self.o(o).proto != Some(self.intr.array_proto) {
            return false;
        }
        let ap = self.intr.array_proto;
        let ok = matches!(self.o(ap).props.get(ik), Some(p) if matches!(p.v, Val::Obj(f) if f == self.intr.array_values));
        if !ok {
            return false;
        }
        let aip = self.intr.array_iterator_proto;
        let nk = self.key(N::next);
        matches!(self.o(aip).props.get(nk), Some(p) if matches!(p.v, Val::Obj(f) if f == self.intr.array_iter_next))
    }

    /// The next value of the iterator record at stack[slot..slot+3], or None when done.
    pub(crate) fn iter_step(&mut self, slot: usize) -> Res<Option<Val>> {
        let it = self.vm.stack.get(slot).copied().unwrap_or(Val::Undef);
        let next = self.vm.stack.get(slot + 1).copied().unwrap_or(Val::Undef);
        if matches!(self.vm.stack.get(slot + 2), Some(Val::Bool(true))) {
            return Ok(None);
        }
        // Mark done while next() runs: if it throws, the iterator isn't closed.
        if let Some(d) = self.vm.stack.get_mut(slot + 2) {
            *d = Val::Bool(true);
        }
        // Fast path: a builtin array iterator.
        if let (Val::Obj(io), Val::Obj(no)) = (it, next)
            && no == self.intr.array_iter_next
            && let ObjKind::ArrayIter(_) = self.o(io).kind
        {
            let r = self.array_iter_advance(io)?;
            if r.is_some()
                && let Some(d) = self.vm.stack.get_mut(slot + 2)
            {
                *d = Val::Bool(false);
            }
            return Ok(r);
        }
        let r = self.call_v(next, it, &[])?;
        let Val::Obj(ro) = r else {
            let d = self.short_display(r);
            return Err(self.type_error(&alloc::format!("Iterator result {d} is not an object")));
        };
        let dk = self.key(N::done);
        let d = self.get_p(ro, dk, r)?;
        if self.truthy(d) {
            return Ok(None);
        }
        let vk = self.key(N::value);
        let v = self.get_p(ro, vk, r)?;
        if let Some(d) = self.vm.stack.get_mut(slot + 2) {
            *d = Val::Bool(false);
        }
        Ok(Some(v))
    }

    /// Advances a builtin array iterator: Some(value) or None when done.
    pub(crate) fn array_iter_advance(&mut self, io: ObjId) -> Res<Option<Val>> {
        let (obj, idx, kind, done) = match &self.o(io).kind {
            ObjKind::ArrayIter(a) => (a.obj, a.idx, a.kind, a.done),
            _ => return Ok(None),
        };
        if done {
            return Ok(None);
        }
        let len = match obj {
            Val::Obj(o) => match &self.o(o).kind {
                ObjKind::Array(a) => a.len as f64,
                ObjKind::Typed(t) => {
                    // Iterating a detached typed array is an error.
                    if self.buf_detached(t.buf) {
                        return Err(self.type_error(
                            "Cannot perform %ArrayIteratorPrototype%.next on a detached ArrayBuffer",
                        ));
                    }
                    t.len as f64
                }
                _ => {
                    let lk = self.key(N::length);
                    let l = self.get_p(o, lk, obj)?;
                    self.to_length(l)?
                }
            },
            _ => 0.0,
        };
        if (idx as f64) >= len {
            if let ObjKind::ArrayIter(a) = &mut self.om(io).kind {
                a.done = true;
            }
            return Ok(None);
        }
        if let ObjKind::ArrayIter(a) = &mut self.om(io).kind {
            a.idx += 1;
        }
        let v = match kind {
            0 => Val::Int(idx as i32),
            _ => {
                let v = self.get_elem(obj, Val::num(idx as f64))?;
                if kind == 1 {
                    v
                } else {
                    let pair = alloc::vec![Val::num(idx as f64), v];
                    Val::Obj(self.new_array_from(pair))
                }
            }
        };
        Ok(Some(v))
    }

    pub(crate) fn iter_close(&mut self, slot: usize, quiet: bool) -> Res<()> {
        if matches!(self.vm.stack.get(slot + 2), Some(Val::Bool(true))) {
            return Ok(());
        }
        if let Some(d) = self.vm.stack.get_mut(slot + 2) {
            *d = Val::Bool(true);
        }
        let it = self.vm.stack.get(slot).copied().unwrap_or(Val::Undef);
        if let Val::Obj(io) = it
            && let ObjKind::ArrayIter(_) = self.o(io).kind
        {
            let rk = self.key(N::return_);
            if self.o(io).props.get(rk).is_none()
                && self
                    .o(self.intr.array_iterator_proto)
                    .props
                    .get(rk)
                    .is_none()
            {
                return Ok(());
            }
        }
        let rk = self.key(N::return_);
        let r = match self.get_v(it, rk) {
            Ok(r) => r,
            Err(e) => return if quiet { Ok(()) } else { Err(e) },
        };
        if r.is_nullish() {
            return Ok(());
        }
        match self.call_v(r, it, &[]) {
            Ok(res) => {
                if !quiet && !matches!(res, Val::Obj(_)) {
                    return Err(self.type_error("Iterator result is not an object"));
                }
                Ok(())
            }
            Err(e) => {
                if quiet && !e.is_uncatchable() {
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Spreads an iterable into an array.
    pub(crate) fn spread_into(&mut self, arr: ObjId, it: Val) -> Res<()> {
        let items = self.iterate_to_vec(it)?;
        for v in items {
            let len = self.array_len(arr);
            if !self.array_put_dense(arr, len, v) {
                self.create_data(arr, PropKey::Idx(len), v)?;
            }
        }
        Ok(())
    }

    /// All values of an iterable (fast for arrays and strings).
    pub(crate) fn iterate_to_vec(&mut self, it: Val) -> Res<Vec<Val>> {
        if let Val::Obj(o) = it
            && let ObjKind::Array(a) = &self.o(o).kind
            && self.o(o).flags & crate::object::SPARSE == 0
            && a.dense.len() as u32 == a.len
            && self.array_iteration_pristine(o)
            && let ObjKind::Array(a) = &self.o(o).kind
        {
            return Ok(a.dense.iter().map(|v| v.or_undef()).collect());
        }
        let (iter, next) = self.get_iterator(it)?;
        let mark = self.temps.len();
        self.temps.push(iter);
        self.temps.push(next);
        let mut out = Vec::new();
        let r: Res<()> = (|| {
            loop {
                if let (Val::Obj(io), Val::Obj(no)) = (iter, next)
                    && no == self.intr.array_iter_next
                    && let ObjKind::ArrayIter(_) = self.o(io).kind
                {
                    match self.array_iter_advance(io)? {
                        Some(v) => {
                            out.push(v);
                            continue;
                        }
                        None => return Ok(()),
                    }
                }
                let r = self.call_v(next, iter, &[])?;
                let Val::Obj(ro) = r else {
                    return Err(self.type_error("Iterator result is not an object"));
                };
                let dk = self.key(N::done);
                let d = self.get_p(ro, dk, r)?;
                if self.truthy(d) {
                    return Ok(());
                }
                let vk = self.key(N::value);
                let v = self.get_p(ro, vk, r)?;
                // Values gathered so far must survive collections in later next() calls.
                self.temps.push(v);
                out.push(v);
                if out.len() & 1023 == 0 {
                    self.check_mem()?;
                }
            }
        })();
        self.temps.truncate(mark);
        r?;
        Ok(out)
    }

    fn for_in_start(&mut self, v: Val) -> Res<Val> {
        let mut keys: Vec<Val> = Vec::new();
        let obj = match v {
            Val::Undef | Val::Null | Val::Empty => Val::Undef,
            Val::Obj(_) => v,
            _ => Val::Obj(self.to_object(v)?),
        };
        if let Val::Obj(o) = obj {
            let mut seen: alloc::collections::BTreeSet<PropKey> = alloc::collections::BTreeSet::new();
            let mut cur = Some(o);
            let mut depth = 0;
            while let Some(c) = cur {
                let all = self.own_keys_p(c, true, false, false)?;
                for k in all {
                    if !seen.insert(k) {
                        continue;
                    }
                    let enumerable = match self.get_own(c, k)? {
                        Some(own) => own.flags() & E != 0,
                        None => false,
                    };
                    if enumerable {
                        let kv = self.key_to_val(k);
                        self.temps.push(kv);
                        keys.push(kv);
                    }
                }
                cur = self.get_proto_p(c)?;
                depth += 1;
                if depth > 10_000 {
                    break;
                }
            }
        }
        let p = self.intr.object_proto;
        let it = self.alloc(
            Some(p),
            ObjKind::ForIn(Box::new(ForInData { obj, keys, pos: 0 })),
        );
        Ok(Val::Obj(it))
    }

    fn for_in_next(&mut self, it: Val) -> Res<Option<Val>> {
        let Val::Obj(io) = it else { return Ok(None) };
        loop {
            let (obj, k) = match &mut self.om(io).kind {
                ObjKind::ForIn(f) => {
                    if f.pos >= f.keys.len() {
                        return Ok(None);
                    }
                    f.pos += 1;
                    (f.obj, f.keys[f.pos - 1])
                }
                _ => return Ok(None),
            };
            // Skip keys deleted since the loop started.
            if let Val::Obj(o) = obj {
                let pk = self.to_key(k)?;
                if !self.has_property(o, pk)? {
                    continue;
                }
            }
            return Ok(Some(k));
        }
    }

    /// The elements of an array-like (for spread calls and Function.prototype.apply).
    pub(crate) fn array_like_to_vec(&mut self, v: Val) -> Res<Vec<Val>> {
        let Val::Obj(o) = v else {
            if v.is_nullish() {
                return Ok(Vec::new());
            }
            return Err(self.type_error("CreateListFromArrayLike called on non-object"));
        };
        if let ObjKind::Array(a) = &self.o(o).kind
            && self.o(o).flags & crate::object::SPARSE == 0
            && a.dense.len() as u32 == a.len
            && !a.dense.iter().any(|x| x.is_empty())
            && let ObjKind::Array(a) = &self.o(o).kind
        {
            return Ok(a.dense.clone());
        }
        let lk = self.key(N::length);
        let l = self.get_p(o, lk, v)?;
        let len = self.to_length(l)?;
        if len > 10_000_000.0 {
            return Err(self.range_error("Too many arguments"));
        }
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len as u32 {
            let x = self.get_p(o, PropKey::Idx(i), v)?;
            // (rooted until the native call that asked returns)
            self.temps.push(x);
            out.push(x);
        }
        Ok(out)
    }

    /// Copies own enumerable properties of `src` onto `dst` (object spread), skipping `ex`.
    pub(crate) fn copy_data_props(&mut self, dst: ObjId, src: Val, ex: &[PropKey]) -> Res<()> {
        let so = match src {
            Val::Undef | Val::Null | Val::Empty => return Ok(()),
            Val::Obj(o) => o,
            _ => self.to_object(src)?,
        };
        let keys = self.own_keys_p(so, true, true, false)?;
        for k in keys {
            if ex.contains(&k) {
                continue;
            }
            if let Some(own) = self.get_own(so, k)?
                && own.flags() & E != 0
            {
                let v = self.get_p(so, k, Val::Obj(so))?;
                self.create_data(dst, k, v)?;
            }
        }
        Ok(())
    }

    // ---- stack traces ----

    /// (function name, file, line, column), innermost first.
    pub(crate) fn stack_frames(&mut self, max: usize) -> Vec<(String, String, u32, u32)> {
        let mut out = Vec::new();
        for f in self.vm.frames.iter().rev() {
            if out.len() >= max {
                break;
            }
            let pos = f.code.pos_at(f.pc.saturating_sub(1));
            let (line, col) = f.code.source.line_col(pos);
            let name = match f.code.name {
                Some(s) => crate::string::to_rust(self.heap.flat(s)),
                None => String::new(),
            };
            let name = match f.code.kind {
                CodeKind::Script => String::new(),
                _ if name.is_empty() => String::from("<anonymous>"),
                _ => name,
            };
            out.push((name, f.code.source.file.clone(), line, col));
        }
        out
    }
}

pub(crate) enum YieldStar {
    Done(Val),
    Yield(Val),
    Return(Val),
}

/// Not reachable: kept so the enum variant is used in match arms above.
#[allow(dead_code)]
fn _unused(_: Termination) {}
