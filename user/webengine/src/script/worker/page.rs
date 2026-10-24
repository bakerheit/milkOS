//! The page's half of workers: `new Worker(url, options)`, `worker.postMessage`,
//! `worker.terminate`, and the events the app delivers from the worker (see the module docs
//! of [`super`]). The class itself is script (`js/workers.js`); this is the natives it calls,
//! what the page asks of the app ([`take_out`]) and what the app tells it ([`event`]).
//!
//! `new Worker(url)` checks the address (same origin as the page; `blob:` addresses the page
//! made with `URL.createObjectURL` and `data:` ones are allowed, a `data:` worker having an
//! origin of its own), the count, and asks the app to start it with a
//! [`WorkerOut::Spawn`]. A worker that is refused gets its `error` event as a task of the page
//! (and a console line) without the app ever hearing of it.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use jsengine::{JsResult, Realm, Value};

use super::{MAX_PER_REALM, xfer};
use crate::api::*;
use crate::script::bridge::{Bridge, arg, arg_str, dom_error, type_error};
use crate::script::{net, netbytes, ports};

/// Worker numbers are unique in the process (so the app can key them without caring which
/// page or frame made them, and two frames of one page cannot collide on a port link).
static NEXT_WORKER: AtomicU32 = AtomicU32::new(1);

/// Messages the page may have queued for the app and not handed over.
const MAX_OUT: usize = 20_000;

fn refuse(b: &Rc<Bridge>, r: &mut Realm, id: u32, obj: Value, url: &str, why: String) {
    b.console(ConsoleLevel::Error, why);
    if let Ok(mut st) = b.st.try_borrow_mut() {
        let pin = r.pin(obj);
        st.workers.objs.insert(id, pin);
        st.workers.live += 1;
        st.workers.local.push_back((
            id,
            WorkerEvent::Error { message: String::new(), filename: String::from(url), line: 0, column: 0 },
        ));
        st.workers.local.push_back((id, WorkerEvent::Closed));
    }
}

/// `native.workerNew(worker, url, type, name, blobBytes)` -> the worker's number.
fn worker_new(b: &Rc<Bridge>, r: &mut Realm, a: &[Value]) -> JsResult<Value> {
    let obj = arg(a, 0);
    let raw = arg_str(r, a, 1)?;
    let ty = arg_str(r, a, 2)?;
    let mut name = arg_str(r, a, 3)?;
    let lookup = arg(a, 4);
    if name.len() > 1024 {
        name.truncate(1024);
        while !name.is_char_boundary(name.len()) {
            name.pop();
        }
    }
    let abs = b.page_or(r)?.resolve(raw.trim());
    let Some(u) = net::parse_url(&abs, None) else {
        return Err(dom_error(
            r,
            "SyntaxError",
            &format!("Failed to construct 'Worker': Script at '{raw}' cannot be parsed."),
        ));
    };
    let id = NEXT_WORKER.fetch_add(1, Ordering::Relaxed);
    let url = u.href_no_fragment();
    let page_origin = b.origin().text;
    let secure = crate::script::window::is_secure(b);
    let kind = if ty == "module" { WorkerType::Module } else { WorkerType::Classic };
    if b.st(r)?.workers.live >= MAX_PER_REALM {
        refuse(b, r, id, obj, &url, format!("Failed to construct 'Worker': too many workers are running in this page (the limit is {MAX_PER_REALM})."));
        return Ok(Value::number(id as f64));
    }
    let (origin, source) = match u.scheme.as_str() {
        "http" | "https" => {
            if page_origin == "null" || u.origin() != page_origin {
                refuse(
                    b,
                    r,
                    id,
                    obj,
                    &url,
                    format!("Failed to construct 'Worker': Script at '{url}' cannot be accessed from origin '{page_origin}'."),
                );
                return Ok(Value::number(id as f64));
            }
            (u.origin(), None)
        }
        "blob" => {
            let mut bytes: Option<Vec<u8>> = None;
            if r.is_callable(lookup) && u.origin() == page_origin && page_origin != "null" {
                let key = r.string(&url);
                let found = r.call(lookup, Value::UNDEFINED, &[key])?;
                if found.is_object() {
                    bytes = netbytes::get(b, r, found).map(|x| (*x).clone());
                }
            }
            match bytes {
                Some(by) => (page_origin.clone(), Some(by)),
                None => {
                    refuse(
                        b,
                        r,
                        id,
                        obj,
                        &url,
                        format!("Failed to construct 'Worker': Script at '{url}' cannot be loaded (the blob address is not one this page made, or it was revoked)."),
                    );
                    return Ok(Value::number(id as f64));
                }
            }
        }
        // A data: script is its own origin: it can fetch nothing but what CORS lets any
        // origin read.
        "data" => (String::from("null"), None),
        other => {
            refuse(
                b,
                r,
                id,
                obj,
                &url,
                format!("Failed to construct 'Worker': Script at '{url}' cannot be loaded: only http, https, blob and data addresses can be used (not {other})."),
            );
            return Ok(Value::number(id as f64));
        }
    };
    let pin = r.pin(obj);
    let mut st = b.st(r)?;
    st.workers.objs.insert(id, pin);
    st.workers.live += 1;
    st.workers.out.push(WorkerOut::Spawn(WorkerSpawn { id, url, kind, name, origin, source, secure }));
    Ok(Value::number(id as f64))
}

fn id_arg(r: &mut Realm, a: &[Value]) -> JsResult<u32> {
    let n = r.to_number(arg(a, 0))?;
    Ok(if n >= 0.0 && n <= u32::MAX as f64 { n as u32 } else { 0 })
}

/// The natives of the script side (on its `native` object).
pub fn define_natives(b: &Rc<Bridge>, r: &mut Realm, n: Value) -> JsResult<()> {
    let b2 = b.clone();
    r.define_method(n, "workerNew", 5, move |r, _t, a| worker_new(&b2, r, a))?;
    let b2 = b.clone();
    r.define_method(n, "workerPost", 3, move |r, _t, a| {
        let id = id_arg(r, a)?;
        if !b2.st(r)?.workers.objs.contains_key(&id) {
            // Terminated (or never started): a post goes nowhere, as in browsers.
            return Ok(Value::UNDEFINED);
        }
        let msg = xfer::encode_post(&b2, r, &a[1.min(a.len())..], id, "Worker")?;
        let mut st = b2.st(r)?;
        if st.workers.out.len() >= MAX_OUT {
            drop(st);
            xfer::drop_msg(&b2, &msg);
            b2.console(ConsoleLevel::Warn, String::from("Too many messages are waiting for the worker: a postMessage was dropped."));
            return Ok(Value::UNDEFINED);
        }
        st.workers.out.push(WorkerOut::Post { worker: id, msg });
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(n, "workerTerminate", 1, move |r, _t, a| {
        let id = id_arg(r, a)?;
        terminate(&b2, r, id);
        Ok(Value::UNDEFINED)
    })?;
    Ok(())
}

/// End worker `id` from the page's side: the app is told, the object goes quiet, the ports
/// that were given to it close.
pub fn terminate(b: &Rc<Bridge>, r: &mut Realm, id: u32) {
    let pin = {
        let Ok(mut st) = b.st.try_borrow_mut() else { return };
        let pin = st.workers.objs.remove(&id);
        if pin.is_some() {
            st.workers.live = st.workers.live.saturating_sub(1);
            st.workers.out.push(WorkerOut::Terminate(id));
        }
        // (Local events for it are dropped with it.)
        st.workers.local.retain(|(w, _)| *w != id);
        pin
    };
    if let Some(p) = pin {
        r.unpin(p);
    }
    b.ports().close_link(id);
}

/// The page's realm ends: every worker it made is terminated.
pub fn terminate_all(b: &Rc<Bridge>, r: Option<&mut Realm>) {
    let ids: Vec<u32> = match b.st.try_borrow() {
        Ok(st) => st.workers.objs.keys().copied().collect(),
        Err(_) => return,
    };
    let mut r = r;
    for id in ids {
        match r.as_deref_mut() {
            Some(rr) => terminate(b, rr, id),
            None => {
                if let Ok(mut st) = b.st.try_borrow_mut() {
                    st.workers.objs.remove(&id);
                    st.workers.out.push(WorkerOut::Terminate(id));
                }
                b.ports().close_link(id);
            }
        }
    }
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.workers.live = 0;
    }
}

/// What the page asks of the app since the last call: its own requests, then what the ports
/// that crossed to workers have to send.
pub fn take_out(b: &Bridge) -> Vec<WorkerOut> {
    let (mut v, ids) = match b.st.try_borrow_mut() {
        Ok(mut st) => (core::mem::take(&mut st.workers.out), st.workers.objs.keys().copied().collect::<Vec<u32>>()),
        Err(_) => return Vec::new(),
    };
    let table = b.ports();
    if table.has_links() {
        for id in ids {
            for lo in table.take_link_out(id, 1024) {
                v.push(match lo {
                    ports::LinkOut::Msg { port, data, ports, .. } => WorkerOut::Port {
                        worker: id,
                        port,
                        msg: Some(WireMsg { data, xfer: ports.into_iter().map(Xfer::Port).collect() }),
                    },
                    ports::LinkOut::Close { port, .. } => WorkerOut::Port { worker: id, port, msg: None },
                });
            }
        }
    }
    v
}

/// Events the engine itself has for workers (a refused script) wait for a turn.
pub fn has_local(b: &Bridge) -> bool {
    b.st.try_borrow().map(|st| !st.workers.local.is_empty()).unwrap_or(false)
}

/// Deliver the engine's own events (one task each).
pub fn pump_local(b: &Rc<Bridge>, r: &mut Realm) -> bool {
    let mut did = false;
    for _ in 0..64 {
        let next = match b.st.try_borrow_mut() {
            Ok(mut st) => st.workers.local.pop_front(),
            Err(_) => None,
        };
        let Some((id, ev)) = next else { break };
        did |= event(b, r, id, ev);
        crate::script::checkpoint(b, r);
    }
    did
}

/// An event for worker `id`'s `Worker` object. True if it was one the page still listens to.
pub fn event(b: &Rc<Bridge>, r: &mut Realm, id: u32, ev: WorkerEvent) -> bool {
    let obj = match b.st.try_borrow() {
        Ok(st) => st.workers.objs.get(&id).map(|p| r.pinned(p)),
        Err(_) => None,
    };
    let Some(obj) = obj else {
        // The page let it go (terminated): what it carried is dropped.
        match &ev {
            WorkerEvent::Message(m) => xfer::drop_msg(b, m),
            WorkerEvent::Port { msg: Some(m), .. } => xfer::drop_msg(b, m),
            _ => {}
        }
        return false;
    };
    let table = b.ports();
    match ev {
        WorkerEvent::Closed => {
            let pin = {
                let Ok(mut st) = b.st.try_borrow_mut() else { return false };
                st.workers.live = st.workers.live.saturating_sub(1);
                st.workers.objs.remove(&id)
            };
            if let Some(p) = pin {
                r.unpin(p);
            }
            table.close_link(id);
            true
        }
        WorkerEvent::Port { port, msg } => {
            match msg {
                Some(m) => deliver_port(b, id, port, m),
                None => table.close_from_link(port),
            }
            true
        }
        WorkerEvent::Message(m) => {
            let (kind, data, ports_v) = match xfer::decode_post(b, r, &m, id) {
                Ok((d, p)) => ("message", d, p),
                Err(_) => ("messageerror", Value::NULL, r.new_array(&[])),
            };
            deliver_js(b, r, obj, kind, &[data, ports_v]);
            true
        }
        WorkerEvent::Error { message, filename, line, column } => {
            let m = r.string(&message);
            let f = r.string(&filename);
            let handled = deliver_js(
                b,
                r,
                obj,
                "error",
                &[m, f, Value::int(line as i32), Value::int(column as i32)],
            );
            if !handled && !message.is_empty() {
                let at = if line > 0 { format!(" ({filename}:{line}:{column})") } else { String::new() };
                b.console(ConsoleLevel::Error, format!("{message}{at}"));
                if let Ok(mut st) = b.st.try_borrow_mut() {
                    st.stats.errors += 1;
                }
            }
            true
        }
    }
}

/// A message for a bridged port: its carried ports are imported, then it is queued.
pub fn deliver_port(b: &Bridge, link: u32, port: u64, m: WireMsg) {
    let table = b.ports();
    let mut ids = Vec::new();
    for x in &m.xfer {
        if let Xfer::Port(id) = x
            && table.import(*id, link)
        {
            ids.push(*id);
        }
    }
    table.deliver_link(port, m.data, &ids);
}

/// `deliverWorker(worker, kind, ...)` of the script side. True if the event was handled
/// (an `error` event that was cancelled).
fn deliver_js(b: &Rc<Bridge>, r: &mut Realm, obj: Value, kind: &str, args: &[Value]) -> bool {
    let Ok(ex) = net::exports(b, r) else { return false };
    let Ok(f) = r.get(ex, "deliverWorker") else { return false };
    if !r.is_callable(f) {
        return false;
    }
    let k = r.string(kind);
    let mut argv = alloc::vec![obj, k];
    argv.extend_from_slice(args);
    match r.call(f, Value::UNDEFINED, &argv) {
        Ok(v) => r.to_boolean(v),
        Err(e) => {
            crate::script::report(b, r, &e, "");
            false
        }
    }
}

#[allow(dead_code)]
fn _unused(r: &mut Realm) -> jsengine::Exception {
    type_error(r, "")
}
