//! %IteratorPrototype%, generator objects' next/return/throw, async generators, and
//! %AsyncFromSyncIteratorPrototype%.

use alloc::collections::VecDeque;

use crate::api::{Exc, Exception};
use crate::interp::{AgRequest, CoKind, CoState, Resume, Signal};
use crate::names::N;
use crate::object::{ObjKind, PropKey};
use crate::realm::{Realm, Res};
use crate::value::{ObjId, Val};

use super::arg;

fn return_this(_r: &mut Realm, this: Val, _args: &[Val]) -> Res<Val> {
    Ok(this)
}

fn gen_resume(r: &mut Realm, this: Val, v: Val, mode: Resume) -> Res<Val> {
    let co = match this {
        Val::Obj(o) => match &r.o(o).kind {
            ObjKind::Coroutine(c) if c.kind == CoKind::Generator => Some((o, c.state)),
            _ => None,
        },
        _ => None,
    };
    let Some((co, state)) = co else {
        return Err(r.type_error("next method called on incompatible receiver"));
    };
    match state {
        CoState::Running | CoState::Awaiting => {
            return Err(r.type_error("Generator is already running"));
        }
        CoState::Done => {
            return match mode {
                Resume::Next => Ok(r.iter_result(Val::Undef, true)),
                Resume::Return => Ok(r.iter_result(v, true)),
                Resume::Throw => Err(Exception(Exc::Throw(v))),
            };
        }
        CoState::Start if mode != Resume::Next => {
            if let ObjKind::Coroutine(c) = &mut r.om(co).kind {
                c.state = CoState::Done;
                c.frame = None;
                c.stack.clear();
            }
            return match mode {
                Resume::Return => Ok(r.iter_result(v, true)),
                _ => Err(Exception(Exc::Throw(v))),
            };
        }
        _ => {}
    }
    let mark = r.temps.len();
    r.temps.push(this);
    let out = r.co_resume(co, v, mode);
    r.temps.truncate(mark);
    let out = out?;
    let done = r.vm.signal != Signal::Yield;
    if done && let ObjKind::Coroutine(c) = &mut r.om(co).kind {
        c.state = CoState::Done;
    }
    Ok(r.iter_result(out, done))
}

fn gen_next(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    gen_resume(r, this, arg(args, 0), Resume::Next)
}
fn gen_return(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    gen_resume(r, this, arg(args, 0), Resume::Return)
}
fn gen_throw(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    gen_resume(r, this, arg(args, 0), Resume::Throw)
}

fn async_gen_enqueue(r: &mut Realm, this: Val, v: Val, mode: Resume) -> Res<Val> {
    let p = r.new_promise_obj(None);
    let ok = match this {
        Val::Obj(o) => matches!(&r.o(o).kind, ObjKind::Coroutine(c) if c.kind == CoKind::AsyncGen),
        _ => false,
    };
    if !ok {
        let e = r.make_error(
            crate::api::ErrorKind::TypeError,
            "AsyncGenerator method called on incompatible receiver",
        );
        r.reject_promise(p, Val::Obj(e));
        return Ok(Val::Obj(p));
    }
    let Val::Obj(co) = this else {
        return Ok(Val::Obj(p));
    };
    if let ObjKind::Coroutine(c) = &mut r.om(co).kind {
        c.queue.push_back(AgRequest {
            mode,
            value: v,
            promise: p,
        });
    }
    let mark = r.temps.len();
    r.temps.push(Val::Obj(p));
    r.temps.push(this);
    let res = r.async_gen_drain(co);
    r.temps.truncate(mark);
    res?;
    Ok(Val::Obj(p))
}

fn agen_next(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    async_gen_enqueue(r, this, arg(args, 0), Resume::Next)
}
fn agen_return(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    async_gen_enqueue(r, this, arg(args, 0), Resume::Return)
}
fn agen_throw(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    async_gen_enqueue(r, this, arg(args, 0), Resume::Throw)
}

/// AsyncFromSyncIterator: forwards to the sync iterator and wraps results in promises
/// (awaiting the value).
fn afs_step(r: &mut Realm, this: Val, args: &[Val], which: N) -> Res<Val> {
    let (it, next) = match this {
        Val::Obj(o) => match r.o(o).kind {
            ObjKind::AsyncFromSync(i, n) => (i, n),
            _ => (Val::Undef, Val::Undef),
        },
        _ => (Val::Undef, Val::Undef),
    };
    let (p, res, rej) = r.promise_with_resolvers();
    let mark = r.temps.len();
    r.temps.push(Val::Obj(p));
    let out: Res<()> = (|| {
        let result = match which {
            N::next => r.call_v(next, it, &[arg(args, 0)])?,
            _ => {
                let k = r.key(which);
                let m = r.get_v(it, k)?;
                if m.is_nullish() {
                    if which == N::return_ {
                        let v = r.iter_result(arg(args, 0), true);
                        r.call_v(res, Val::Undef, &[v])?;
                    } else {
                        r.call_v(rej, Val::Undef, &[arg(args, 0)])?;
                    }
                    return Ok(());
                }
                r.call_v(m, it, &[arg(args, 0)])?
            }
        };
        let Val::Obj(ro) = result else {
            return Err(r.type_error("Iterator result is not an object"));
        };
        let dk = r.key(N::done);
        let d = r.get_p(ro, dk, result)?;
        let done = r.truthy(d);
        let vk = r.key(N::value);
        let v = r.get_p(ro, vk, result)?;
        // Await the value, then resolve with { value, done }.
        let vp = r.promise_resolve_intrinsic(v)?;
        let e = Val::Str(r.nstr(N::empty));
        let wrap = r.new_builtin_slots(e, 1, afs_wrap, alloc::vec![Val::Bool(done)]);
        r.perform_then(
            vp,
            crate::builtins::promise::Handler::Js(Val::Obj(wrap)),
            crate::builtins::promise::Handler::Default,
            Some(crate::builtins::promise::Capability {
                promise: Val::Obj(p),
                resolve: res,
                reject: rej,
            }),
        );
        Ok(())
    })();
    r.temps.truncate(mark);
    match out {
        Ok(()) => {}
        Err(Exception(Exc::Throw(e))) => {
            r.call_v(rej, Val::Undef, &[e])?;
        }
        Err(e) => return Err(e),
    }
    Ok(Val::Obj(p))
}

fn afs_wrap(r: &mut Realm, _this: Val, args: &[Val]) -> Res<Val> {
    let f = r.cur_callee();
    let done = matches!(r.native_slot(f, 0), Val::Bool(true));
    Ok(r.iter_result(arg(args, 0), done))
}

fn afs_next(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    afs_step(r, this, args, N::next)
}
fn afs_return(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    afs_step(r, this, args, N::return_)
}
fn afs_throw(r: &mut Realm, this: Val, args: &[Val]) -> Res<Val> {
    afs_step(r, this, args, N::throw_)
}

pub(crate) fn init(r: &mut Realm) {
    let ip = r.intr.iterator_proto;
    let ik = r.sym.iterator;
    r.method_sym(ip, ik, "[Symbol.iterator]", 0, return_this);
    let aip = r.intr.async_iterator_proto;
    let aik = r.sym.async_iterator;
    r.method_sym(aip, aik, "[Symbol.asyncIterator]", 0, return_this);
    let gp = r.intr.generator_proto;
    r.method(gp, "next", 1, gen_next);
    r.method(gp, "return", 1, gen_return);
    r.method(gp, "throw", 1, gen_throw);
    r.to_string_tag(gp, "Generator");
    let agp = r.intr.async_generator_proto;
    r.method(agp, "next", 1, agen_next);
    r.method(agp, "return", 1, agen_return);
    r.method(agp, "throw", 1, agen_throw);
    r.to_string_tag(agp, "AsyncGenerator");
    let afs = r.intr.async_from_sync_iterator_proto;
    r.method(afs, "next", 1, afs_next);
    r.method(afs, "return", 1, afs_return);
    r.method(afs, "throw", 1, afs_throw);
    let _ = (
        PropKey::Idx(0),
        VecDeque::<u8>::new(),
        ObjId { idx: 0, g: 0 },
    );
}
