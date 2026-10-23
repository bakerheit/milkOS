//! Promises: states and reactions, the resolving functions, the job queue hooks, the
//! internal reactions that resume async functions and async generators, and the Promise
//! constructor and its methods (then/catch/finally, resolve/reject/all/allSettled/race/any).

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::api::{Exc, Exception};
use crate::heap::Tracer;
use crate::interp::{CoKind, CoState, Resume, Signal};
use crate::names::N;
use crate::object::{ObjKind, PropKey};
use crate::realm::{Job, Realm, Res};
use crate::value::{ObjId, Val};

use super::arg;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PState {
    Pending,
    Fulfilled,
    Rejected,
}

pub(crate) struct PromiseData {
    pub state: PState,
    pub result: Val,
    pub fulfill: Vec<Val>,
    pub reject: Vec<Val>,
    pub handled: bool,
}

impl PromiseData {
    pub fn trace(&self, t: &mut Tracer) {
        t.val(self.result);
        for v in &self.fulfill {
            t.val(*v);
        }
        for v in &self.reject {
            t.val(*v);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Handler {
    Js(Val),
    /// No handler: pass the value (fulfill) or the reason (reject) on.
    Default,
    /// Resume an async function / async generator at its await.
    AsyncResume(ObjId),
    /// Promise.all / allSettled / any element functions are ordinary natives (Js).
    Noop,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Capability {
    pub promise: Val,
    pub resolve: Val,
    pub reject: Val,
}

pub(crate) struct ReactionData {
    pub cap: Option<Capability>,
    pub reject: bool,
    pub handler: Handler,
}

impl ReactionData {
    pub fn trace(&self, t: &mut Tracer) {
        if let Some(c) = &self.cap {
            t.val(c.promise);
            t.val(c.resolve);
            t.val(c.reject);
        }
        match self.handler {
            Handler::Js(v) => t.val(v),
            Handler::AsyncResume(o) => t.obj(o),
            _ => {}
        }
    }
}

impl Realm {
    pub(crate) fn new_promise_obj(&mut self, proto: Option<ObjId>) -> ObjId {
        let p = proto.unwrap_or(self.intr.promise_proto);
        self.alloc(
            Some(p),
            ObjKind::Promise(Box::new(PromiseData {
                state: PState::Pending,
                result: Val::Undef,
                fulfill: Vec::new(),
                reject: Vec::new(),
                handled: false,
            })),
        )
    }

    /// A pending promise and its resolving functions.
    pub(crate) fn promise_with_resolvers(&mut self) -> (ObjId, Val, Val) {
        let p = self.new_promise_obj(None);
        let (res, rej) = self.resolving_functions(p);
        (p, res, rej)
    }

    /// CreateResolvingFunctions: each knows its promise and its sibling, and calling either
    /// disarms both.
    pub(crate) fn resolving_functions(&mut self, p: ObjId) -> (Val, Val) {
        let e = Val::Str(self.nstr(N::empty));
        let res = self.new_builtin_slots(e, 1, resolve_fn, alloc::vec![Val::Obj(p), Val::Undef]);
        let rej = self.new_builtin_slots(e, 1, reject_fn, alloc::vec![Val::Obj(p), Val::Obj(res)]);
        self.set_native_slot(res, 1, Val::Obj(rej));
        (Val::Obj(res), Val::Obj(rej))
    }

    fn promise_state(&self, p: ObjId) -> Option<(PState, Val)> {
        match &self.o(p).kind {
            ObjKind::Promise(d) => Some((d.state, d.result)),
            _ => None,
        }
    }

    /// The promise resolve function's algorithm.
    pub(crate) fn resolve_promise(&mut self, p: ObjId, v: Val) -> Res<()> {
        if let Val::Obj(vo) = v {
            if vo == p {
                let e = self.make_error(
                    crate::api::ErrorKind::TypeError,
                    "Chaining cycle detected for promise",
                );
                self.reject_promise(p, Val::Obj(e));
                return Ok(());
            }
            let tk = self.key(N::then);
            let then = match self.get_p(vo, tk, v) {
                Ok(t) => t,
                Err(Exception(Exc::Throw(e))) => {
                    self.reject_promise(p, e);
                    return Ok(());
                }
                Err(e) => return Err(e),
            };
            if self.callable(then) {
                self.jobs.push_back(Job::Thenable {
                    promise: Val::Obj(p),
                    thenable: v,
                    then,
                });
                return Ok(());
            }
        }
        self.fulfill_promise(p, v);
        Ok(())
    }

    pub(crate) fn fulfill_promise(&mut self, p: ObjId, v: Val) {
        let reactions = match &mut self.om(p).kind {
            ObjKind::Promise(d) if d.state == PState::Pending => {
                d.state = PState::Fulfilled;
                d.result = v;
                d.reject.clear();
                core::mem::take(&mut d.fulfill)
            }
            _ => return,
        };
        for r in reactions {
            self.jobs.push_back(Job::Reaction {
                reaction: r,
                arg: v,
            });
        }
    }

    pub(crate) fn reject_promise(&mut self, p: ObjId, reason: Val) {
        let (reactions, handled) = match &mut self.om(p).kind {
            ObjKind::Promise(d) if d.state == PState::Pending => {
                d.state = PState::Rejected;
                d.result = reason;
                d.fulfill.clear();
                (core::mem::take(&mut d.reject), d.handled)
            }
            _ => return,
        };
        if !handled {
            self.pending_rejections.push(p);
        }
        for r in reactions {
            self.jobs.push_back(Job::Reaction {
                reaction: r,
                arg: reason,
            });
        }
    }

    fn new_reaction(&mut self, cap: Option<Capability>, reject: bool, handler: Handler) -> Val {
        Val::Obj(self.alloc(
            None,
            ObjKind::Reaction(Box::new(ReactionData {
                cap,
                reject,
                handler,
            })),
        ))
    }

    /// PerformPromiseThen.
    pub(crate) fn perform_then(
        &mut self,
        p: ObjId,
        on_ful: Handler,
        on_rej: Handler,
        cap: Option<Capability>,
    ) {
        let fr = self.new_reaction(cap, false, on_ful);
        let rr = self.new_reaction(cap, true, on_rej);
        let state = match &mut self.om(p).kind {
            ObjKind::Promise(d) => {
                let was_handled = d.handled;
                d.handled = true;
                match d.state {
                    PState::Pending => {
                        d.fulfill.push(fr);
                        d.reject.push(rr);
                        None
                    }
                    s => Some((s, d.result, was_handled)),
                }
            }
            _ => return,
        };
        if let Some((s, result, was_handled)) = state {
            if s == PState::Fulfilled {
                self.jobs.push_back(Job::Reaction {
                    reaction: fr,
                    arg: result,
                });
            } else {
                if !was_handled {
                    self.pending_rejections.retain(|x| *x != p);
                    self.unhandled_retract(result);
                }
                self.jobs.push_back(Job::Reaction {
                    reaction: rr,
                    arg: result,
                });
            }
        }
    }

    fn unhandled_retract(&mut self, _reason: Val) {}

    /// Moves rejections still unhandled into the report list.
    pub(crate) fn track_rejections(&mut self) {
        let pend = core::mem::take(&mut self.pending_rejections);
        for p in pend {
            if let ObjKind::Promise(d) = &self.o(p).kind
                && !d.handled
                && d.state == PState::Rejected
            {
                let r = d.result;
                self.unhandled.push(r);
            }
        }
    }

    pub(crate) fn run_reaction_job(&mut self, reaction: Val, arg: Val) -> Res<()> {
        let Val::Obj(ro) = reaction else {
            return Ok(());
        };
        let (cap, reject, handler) = match &self.o(ro).kind {
            ObjKind::Reaction(d) => (d.cap, d.reject, d.handler),
            _ => return Ok(()),
        };
        let result: Result<Val, Exception> = match handler {
            Handler::Default => {
                if reject {
                    Err(Exception(Exc::Throw(arg)))
                } else {
                    Ok(arg)
                }
            }
            Handler::Noop => Ok(Val::Undef),
            Handler::Js(f) => self.call_v(f, Val::Undef, &[arg]),
            Handler::AsyncResume(co) => {
                let mode = if reject { Resume::Throw } else { Resume::Next };
                let r = self.co_resume(co, arg, mode);
                let is_gen =
                    matches!(&self.o(co).kind, ObjKind::Coroutine(c) if c.kind == CoKind::AsyncGen);
                if is_gen {
                    return self.async_gen_after(co, r);
                }
                return match r {
                    Ok(_) => Ok(()),
                    Err(e) if e.is_uncatchable() => Err(e),
                    // An async function turns its exceptions into rejections; anything else
                    // reaching here is a bug in the engine, not the page.
                    Err(_) => Ok(()),
                };
            }
        };
        let Some(cap) = cap else {
            return match result {
                Err(e) if e.is_uncatchable() => Err(e),
                _ => Ok(()),
            };
        };
        match result {
            Ok(v) => {
                self.call_v(cap.resolve, Val::Undef, &[v])?;
            }
            Err(Exception(Exc::Throw(e))) => {
                self.call_v(cap.reject, Val::Undef, &[e])?;
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }

    pub(crate) fn run_thenable_job(&mut self, promise: Val, thenable: Val, then: Val) -> Res<()> {
        let Val::Obj(p) = promise else { return Ok(()) };
        let (res, rej) = self.resolving_functions(p);
        match self.call_v(then, thenable, &[res, rej]) {
            Ok(_) => Ok(()),
            Err(Exception(Exc::Throw(e))) => {
                self.call_v(rej, Val::Undef, &[e])?;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// PromiseResolve(%Promise%, v).
    pub(crate) fn promise_resolve_intrinsic(&mut self, v: Val) -> Res<ObjId> {
        if let Val::Obj(o) = v
            && let ObjKind::Promise(_) = self.o(o).kind
        {
            let ck = self.key(N::constructor);
            let c = self.get_p(o, ck, v)?;
            if let Val::Obj(c) = c
                && c == self.intr.promise_ctor
            {
                return Ok(o);
            }
        }
        let p = self.new_promise_obj(None);
        self.resolve_promise(p, v)?;
        Ok(p)
    }

    /// Resumes `co` when `p` settles.
    pub(crate) fn await_then(&mut self, p: ObjId, co: ObjId) {
        self.perform_then(p, Handler::AsyncResume(co), Handler::AsyncResume(co), None);
    }

    /// NewPromiseCapability(C) for a constructor C.
    pub(crate) fn new_capability(&mut self, c: Val) -> Res<Capability> {
        if let Val::Obj(co) = c
            && co == self.intr.promise_ctor
        {
            let (p, res, rej) = self.promise_with_resolvers();
            return Ok(Capability {
                promise: Val::Obj(p),
                resolve: res,
                reject: rej,
            });
        }
        if !self.constructible(c) {
            return Err(self.type_error("Promise resolver is not a constructor"));
        }
        // A subclass or foreign constructor: capture its resolve/reject via an executor.
        let e = Val::Str(self.nstr(N::empty));
        let holder = self.new_plain();
        let exec = self.new_builtin_slots(e, 2, capability_executor, alloc::vec![Val::Obj(holder)]);
        let mark = self.temps.len();
        self.temps.push(Val::Obj(holder));
        let p = self.construct_v(c, &[Val::Obj(exec)], c);
        self.temps.truncate(mark);
        let p = p?;
        let rk = self.key_str("resolve");
        let jk = self.key_str("reject");
        let res = self.get_p(holder, rk, Val::Obj(holder))?;
        let rej = self.get_p(holder, jk, Val::Obj(holder))?;
        if !self.callable(res) || !self.callable(rej) {
            return Err(self.type_error("Promise resolve or reject function is not callable"));
        }
        Ok(Capability {
            promise: p,
            resolve: res,
            reject: rej,
        })
    }

    // ---- async generators ----

    /// Settles requests after an async generator step, and keeps resuming while requests
    /// are queued and it isn't waiting on an await.
    pub(crate) fn async_gen_after(&mut self, co: ObjId, r: Res<Val>) -> Res<()> {
        let signal = self.vm.signal;
        match r {
            Ok(v) => match signal {
                Signal::Await => return Ok(()),
                Signal::Yield => {
                    if let Some(req) = self.ag_pop(co) {
                        let res = self.iter_result(v, false);
                        self.resolve_promise(req, res)?;
                    }
                }
                Signal::Return => {
                    self.ag_set_state(co, CoState::Done);
                    if let Some(req) = self.ag_pop(co) {
                        let res = self.iter_result(v, true);
                        self.resolve_promise(req, res)?;
                    }
                }
            },
            Err(Exception(Exc::Throw(e))) => {
                self.ag_set_state(co, CoState::Done);
                if let Some(req) = self.ag_pop(co) {
                    self.reject_promise(req, e);
                }
            }
            Err(e) => return Err(e),
        }
        self.async_gen_drain(co)
    }

    fn ag_pop(&mut self, co: ObjId) -> Option<ObjId> {
        match &mut self.om(co).kind {
            ObjKind::Coroutine(c) => c.queue.pop_front().map(|r| r.promise),
            _ => None,
        }
    }

    fn ag_set_state(&mut self, co: ObjId, s: CoState) {
        if let ObjKind::Coroutine(c) = &mut self.om(co).kind {
            c.state = s;
        }
    }

    /// Runs queued requests of an idle async generator.
    pub(crate) fn async_gen_drain(&mut self, co: ObjId) -> Res<()> {
        for _ in 0..1_000_000 {
            let (state, front) = match &self.o(co).kind {
                ObjKind::Coroutine(c) => (
                    c.state,
                    c.queue.front().map(|r| (r.mode, r.value, r.promise)),
                ),
                _ => return Ok(()),
            };
            let Some((mode, value, promise)) = front else {
                return Ok(());
            };
            if matches!(state, CoState::Running | CoState::Awaiting) {
                return Ok(());
            }
            let finished =
                state == CoState::Done || (state == CoState::Start && mode != Resume::Next);
            if finished {
                if state == CoState::Start {
                    self.ag_set_state(co, CoState::Done);
                }
                self.ag_pop(co);
                match mode {
                    Resume::Next => {
                        let res = self.iter_result(Val::Undef, true);
                        self.resolve_promise(promise, res)?;
                    }
                    Resume::Return => {
                        let res = self.iter_result(value, true);
                        self.resolve_promise(promise, res)?;
                    }
                    Resume::Throw => self.reject_promise(promise, value),
                }
                continue;
            }
            let r = self.co_resume(co, value, mode);
            let signal = self.vm.signal;
            if let (Ok(_), Signal::Await) = (&r, signal) {
                return Ok(());
            }
            match r {
                Ok(v) => match signal {
                    Signal::Yield => {
                        self.ag_pop(co);
                        let res = self.iter_result(v, false);
                        self.resolve_promise(promise, res)?;
                    }
                    _ => {
                        self.ag_set_state(co, CoState::Done);
                        self.ag_pop(co);
                        let res = self.iter_result(v, true);
                        self.resolve_promise(promise, res)?;
                    }
                },
                Err(Exception(Exc::Throw(e))) => {
                    self.ag_set_state(co, CoState::Done);
                    self.ag_pop(co);
                    self.reject_promise(promise, e);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

// ---- resolving functions ----

fn resolve_fn(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    let p = r.native_slot(f, 0);
    let sib = r.native_slot(f, 1);
    let Val::Obj(p) = p else {
        return Ok(Val::Undef);
    };
    r.set_native_slot(f, 0, Val::Undef);
    if let Val::Obj(s) = sib {
        r.set_native_slot(s, 0, Val::Undef);
    }
    r.resolve_promise(p, arg(args, 0))?;
    Ok(Val::Undef)
}

fn reject_fn(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    let p = r.native_slot(f, 0);
    let sib = r.native_slot(f, 1);
    let Val::Obj(p) = p else {
        return Ok(Val::Undef);
    };
    r.set_native_slot(f, 0, Val::Undef);
    if let Val::Obj(s) = sib {
        r.set_native_slot(s, 0, Val::Undef);
    }
    r.reject_promise(p, arg(args, 0));
    Ok(Val::Undef)
}

fn capability_executor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    if let Val::Obj(h) = r.native_slot(f, 0) {
        let rk = r.key_str("resolve");
        let jk = r.key_str("reject");
        r.put_prop(h, rk, arg(args, 0), crate::object::WEC);
        r.put_prop(h, jk, arg(args, 1), crate::object::WEC);
    }
    Ok(Val::Undef)
}

// ---- the constructor and prototype ----

fn promise_ctor(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let nt = r.cur_new_target();
    if nt.is_undef() {
        return Err(r.type_error("Promise constructor cannot be invoked without 'new'"));
    }
    let exec = arg(args, 0);
    if !r.callable(exec) {
        let d = r.short_display(exec);
        return Err(r.type_error(&alloc::format!("Promise resolver {d} is not a function")));
    }
    let proto = r.proto_from_ctor(nt, |r| r.intr.promise_proto)?;
    let p = r.new_promise_obj(Some(proto));
    let (res, rej) = r.resolving_functions(p);
    let mark = r.temps.len();
    r.temps.push(Val::Obj(p));
    let out = r.call_v(exec, Val::Undef, &[res, rej]);
    r.temps.truncate(mark);
    match out {
        Ok(_) => {}
        Err(Exception(Exc::Throw(e))) => {
            r.call_v(rej, Val::Undef, &[e])?;
        }
        Err(e) => return Err(e),
    }
    Ok(Val::Obj(p))
}

fn this_promise(r: &mut Realm, this: Val, what: &str) -> Res<ObjId> {
    if let Val::Obj(o) = this
        && let ObjKind::Promise(_) = r.o(o).kind
    {
        return Ok(o);
    }
    Err(r.type_error(&alloc::format!(
        "Method Promise.prototype.{what} called on incompatible receiver"
    )))
}

fn handler_of(r: &Realm, f: Val) -> Handler {
    if r.callable(f) {
        Handler::Js(f)
    } else {
        Handler::Default
    }
}

fn then(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let p = this_promise(r, this, "then")?;
    let ck = r.key(N::constructor);
    let c = r.get_p(p, ck, this)?;
    let c = if c.is_undef() {
        Val::Obj(r.intr.promise_ctor)
    } else {
        c
    };
    let cap = r.new_capability(c)?;
    let on_f = handler_of(r, arg(args, 0));
    let on_r = handler_of(r, arg(args, 1));
    r.perform_then(p, on_f, on_r, Some(cap));
    Ok(cap.promise)
}

fn catch(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let tk = r.key(N::then);
    let t = r.get_v(this, tk)?;
    r.call_v(t, this, &[Val::Undef, arg(args, 0)])
}

fn finally(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let on_finally = arg(args, 0);
    let tk = r.key(N::then);
    let t = r.get_v(this, tk)?;
    if !r.callable(on_finally) {
        return r.call_v(t, this, &[on_finally, on_finally]);
    }
    let e = Val::Str(r.nstr(N::empty));
    let f1 = r.new_builtin_slots(e, 1, finally_then, alloc::vec![on_finally]);
    let f2 = r.new_builtin_slots(e, 1, finally_catch, alloc::vec![on_finally]);
    r.call_v(t, this, &[Val::Obj(f1), Val::Obj(f2)])
}

fn finally_then(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    let onf = r.native_slot(f, 0);
    let res = r.call_v(onf, Val::Undef, &[])?;
    let p = r.promise_resolve_intrinsic(res)?;
    let e = Val::Str(r.nstr(N::empty));
    let value_thunk = r.new_builtin_slots(e, 0, return_slot, alloc::vec![arg(args, 0)]);
    let tk = r.key(N::then);
    let t = r.get_p(p, tk, Val::Obj(p))?;
    r.call_v(t, Val::Obj(p), &[Val::Obj(value_thunk)])
}

fn finally_catch(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    let onf = r.native_slot(f, 0);
    let res = r.call_v(onf, Val::Undef, &[])?;
    let p = r.promise_resolve_intrinsic(res)?;
    let e = Val::Str(r.nstr(N::empty));
    let thrower = r.new_builtin_slots(e, 0, throw_slot, alloc::vec![arg(args, 0)]);
    let tk = r.key(N::then);
    let t = r.get_p(p, tk, Val::Obj(p))?;
    r.call_v(t, Val::Obj(p), &[Val::Obj(thrower)])
}

fn return_slot(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    Ok(r.native_slot(f, 0))
}

fn throw_slot(r: &mut Realm, _this: Val, _args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    Err(Exception(Exc::Throw(r.native_slot(f, 0))))
}

fn promise_resolve(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let v = arg(args, 0);
    if let Val::Obj(o) = v
        && let ObjKind::Promise(_) = r.o(o).kind
    {
        let ck = r.key(N::constructor);
        let c = r.get_p(o, ck, v)?;
        if r.same_val(c, this) {
            return Ok(v);
        }
    }
    let cap = r.new_capability(this)?;
    r.call_v(cap.resolve, Val::Undef, &[v])?;
    Ok(cap.promise)
}

fn promise_reject(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    let cap = r.new_capability(this)?;
    r.call_v(cap.reject, Val::Undef, &[arg(args, 0)])?;
    Ok(cap.promise)
}

/// Shared state of a combinator: [values array, remaining count, capability resolve/reject,
/// mode]. Element functions keep [state, index, already-called].
fn combinator(r: &mut Realm, this: Val, args: &[Val], mode: u8) -> Res<Val> {
    let cap = r.new_capability(this)?;
    let mark = r.temps.len();
    r.temps.push(cap.promise);
    r.temps.push(cap.resolve);
    r.temps.push(cap.reject);
    let out = combinator_inner(r, this, arg(args, 0), cap, mode);
    r.temps.truncate(mark);
    match out {
        Ok(()) => Ok(cap.promise),
        Err(Exception(Exc::Throw(e))) => {
            r.call_v(cap.reject, Val::Undef, &[e])?;
            Ok(cap.promise)
        }
        Err(e) => Err(e),
    }
}

fn combinator_inner(r: &mut Realm, c: Val, iterable: Val, cap: Capability, mode: u8) -> Res<()> {
    let rk = r.key_str("resolve");
    let resolve = r.get_v(c, rk)?;
    if !r.callable(resolve) {
        return Err(r.type_error("Promise resolve is not a function"));
    }
    let items = r.iterate_to_vec(iterable)?;
    let mark = r.temps.len();
    r.temps.extend_from_slice(&items);
    let values = r.new_array_from(alloc::vec![Val::Undef; items.len()]);
    // state: [values, remaining, resolve, reject]
    let state = r.new_array_from(alloc::vec![
        Val::Obj(values),
        Val::Int(1),
        cap.resolve,
        cap.reject
    ]);
    r.temps.push(Val::Obj(state));
    let e = Val::Str(r.nstr(N::empty));
    let res: Res<()> = (|| {
        for (i, item) in items.iter().enumerate() {
            let p = r.call_v(resolve, c, &[*item])?;
            let (onf, onr) = match mode {
                // all
                0 => {
                    let f = r.new_builtin_slots(
                        e,
                        1,
                        all_element,
                        alloc::vec![
                            Val::Obj(state),
                            Val::Int(i as i32),
                            Val::Bool(false),
                            Val::Undef
                        ],
                    );
                    (Val::Obj(f), cap.reject)
                }
                // allSettled
                1 => {
                    let f = r.new_builtin_slots(
                        e,
                        1,
                        settled_fulfilled,
                        alloc::vec![
                            Val::Obj(state),
                            Val::Int(i as i32),
                            Val::Bool(false),
                            Val::Undef
                        ],
                    );
                    let g = r.new_builtin_slots(
                        e,
                        1,
                        settled_rejected,
                        alloc::vec![
                            Val::Obj(state),
                            Val::Int(i as i32),
                            Val::Bool(false),
                            Val::Undef
                        ],
                    );
                    r.set_native_slot(f, 3, Val::Obj(g));
                    r.set_native_slot(g, 3, Val::Obj(f));
                    (Val::Obj(f), Val::Obj(g))
                }
                // any
                2 => {
                    let g = r.new_builtin_slots(
                        e,
                        1,
                        any_rejected,
                        alloc::vec![
                            Val::Obj(state),
                            Val::Int(i as i32),
                            Val::Bool(false),
                            Val::Undef
                        ],
                    );
                    (cap.resolve, Val::Obj(g))
                }
                // race
                _ => (cap.resolve, cap.reject),
            };
            if mode != 3 {
                bump_remaining(r, state, 1);
            }
            let tk = r.key(N::then);
            let t = r.get_v(p, tk)?;
            r.call_v(t, p, &[onf, onr])?;
        }
        if mode != 3 && bump_remaining(r, state, -1) == 0 {
            finish_combinator(r, state, mode)?;
        }
        Ok(())
    })();
    r.temps.truncate(mark);
    res
}

fn bump_remaining(r: &mut Realm, state: ObjId, d: i32) -> i32 {
    let n = match &mut r.om(state).kind {
        ObjKind::Array(a) => {
            let cur = match a.dense.get(1) {
                Some(Val::Int(i)) => *i,
                _ => 0,
            };
            let n = cur + d;
            if a.dense.len() > 1 {
                a.dense[1] = Val::Int(n);
            }
            n
        }
        _ => 0,
    };
    n
}

fn state_slot(r: &Realm, state: ObjId, i: usize) -> Val {
    match &r.o(state).kind {
        ObjKind::Array(a) => a.dense.get(i).copied().unwrap_or(Val::Undef),
        _ => Val::Undef,
    }
}

fn finish_combinator(r: &mut Realm, state: ObjId, mode: u8) -> Res<()> {
    let values = state_slot(r, state, 0);
    if mode == 2 {
        // any: every promise rejected.
        let e = r.make_error(crate::api::ErrorKind::Error, "All promises were rejected");
        let proto = r.intr.aggregate_error_proto;
        r.om(e).proto = Some(proto);
        let ek = r.key(N::errors);
        r.put_prop(e, ek, values, crate::object::WC);
        let rej = state_slot(r, state, 3);
        r.call_v(rej, Val::Undef, &[Val::Obj(e)])?;
    } else {
        let res = state_slot(r, state, 2);
        r.call_v(res, Val::Undef, &[values])?;
    }
    Ok(())
}

fn element_common(r: &mut Realm, v: Val, mode: u8) -> Res<Val> {
    let f = r.cur_callee();
    if matches!(r.native_slot(f, 2), Val::Bool(true)) {
        return Ok(Val::Undef);
    }
    r.set_native_slot(f, 2, Val::Bool(true));
    // allSettled: the sibling element function shares the "called" flag.
    if let Val::Obj(sib) = r.native_slot(f, 3) {
        r.set_native_slot(sib, 2, Val::Bool(true));
    }
    let Val::Obj(state) = r.native_slot(f, 0) else {
        return Ok(Val::Undef);
    };
    let idx = match r.native_slot(f, 1) {
        Val::Int(i) => i as u32,
        _ => 0,
    };
    if let Val::Obj(values) = state_slot(r, state, 0) {
        r.put_prop(values, PropKey::Idx(idx), v, crate::object::WEC);
    }
    if bump_remaining(r, state, -1) == 0 {
        finish_combinator(r, state, mode)?;
    }
    Ok(Val::Undef)
}

fn all_element(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    element_common(r, arg(args, 0), 0)
}

fn settled_obj(r: &mut Realm, fulfilled: bool, v: Val) -> Val {
    let o = r.new_plain();
    let sk = r.key(N::status);
    let st = Val::Str(r.nstr(if fulfilled { N::fulfilled } else { N::rejected }));
    r.put_prop(o, sk, st, crate::object::WEC);
    let k = r.key(if fulfilled { N::value } else { N::reason });
    r.put_prop(o, k, v, crate::object::WEC);
    Val::Obj(o)
}

fn settled_fulfilled(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = settled_obj(r, true, arg(args, 0));
    element_common(r, o, 1)
}

fn settled_rejected(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let o = settled_obj(r, false, arg(args, 0));
    element_common(r, o, 1)
}

fn any_rejected(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    element_common(r, arg(args, 0), 2)
}

fn all(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    combinator(r, this, args, 0)
}
fn all_settled(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    combinator(r, this, args, 1)
}
fn any(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    combinator(r, this, args, 2)
}
fn race(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    combinator(r, this, args, 3)
}

fn species_getter(_r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(this)
}

pub(crate) fn init(r: &mut Realm) {
    let pp = r.intr.promise_proto;
    let c = r.ctor("Promise", 1, promise_ctor, pp);
    r.intr.promise_ctor = c;
    let t = r.method(pp, "then", 2, then);
    r.intr.promise_then = t;
    r.method(pp, "catch", 1, catch);
    r.method(pp, "finally", 1, finally);
    r.to_string_tag(pp, "Promise");
    r.method(c, "resolve", 1, promise_resolve);
    r.method(c, "reject", 1, promise_reject);
    r.method(c, "all", 1, all);
    r.method(c, "allSettled", 1, all_settled);
    r.method(c, "any", 1, any);
    r.method(c, "race", 1, race);
    let sp = r.sym.species;
    r.getter_sym(c, sp, "[Symbol.species]", species_getter);
}

/// Silences unused warnings for state helpers kept for completeness.
#[allow(dead_code)]
fn _states(r: &Realm, p: ObjId) -> Option<(PState, Val)> {
    r.promise_state(p)
}
