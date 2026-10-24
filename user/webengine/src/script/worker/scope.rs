//! The worker's global object: `DedicatedWorkerGlobalScope`, built from the same bindings as
//! a page's `window` without the DOM (no `window`, no `document`, no elements, no layout).
//!
//! What it has: `self` / `globalThis`, the event target (`addEventListener`, `onmessage`,
//! `onerror`, `onmessageerror`, `onunhandledrejection`), `postMessage`, `close`, `importScripts`
//! (classic workers), `name`, `location`, `navigator` (the worker subset), `console`, timers,
//! `queueMicrotask`, `performance`, and the platform that does not need a document: `fetch`,
//! `XMLHttpRequest`, `Headers`, `Request`, `Response`, `Blob`, `File`, `FileReader`, `FormData`,
//! `URL`, `URLSearchParams`, `AbortController`, `MessageChannel`, `BroadcastChannel`, `crypto`,
//! `TextEncoder`, `TextDecoder`, `atob`, `btoa`, `structuredClone`, `Intl`, geometry classes,
//! `Proxy` and `Reflect` and the language itself.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Attrs, ErrorKind, JsResult, Realm, Value};

use super::xfer;
use crate::api::*;
use crate::script::bridge::{Bridge, arg, arg_str, dom_error, type_error};
use crate::script::events::WINDOW;
use crate::script::window::{self, Tables};
use crate::script::{events_js, frames, modrt, net, nodes};

/// Messages the worker may have posted and not handed up yet.
pub const MAX_POSTS: usize = 100_000;
/// ... and the bytes of the messages among them.
pub const MAX_POST_BYTES: usize = 64 << 20;

/// The `on*` properties of the worker global.
const HANDLERS: &[&str] = &[
    "error",
    "message",
    "messageerror",
    "unhandledrejection",
    "rejectionhandled",
    "languagechange",
    "online",
    "offline",
];

/// Make the realm a worker's global: classes, prototypes, events and the globals.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) {
    if let Err(e) = install_all(b, r) {
        crate::script::report(b, r, &e, "setting up the worker's scripts");
    }
    modrt::install(b, r);
    // Uncaught errors in jobs and unhandled rejections go through the global's events first.
    let b2 = b.clone();
    r.set_rejection_hook(Some(Box::new(move |r: &mut Realm, ev: jsengine::PromiseRejection| match ev {
        jsengine::PromiseRejection::Unhandled { promise, reason } => {
            if !events_js::fire_rejection(&b2, r, promise, Some(reason)) {
                let rep = r.report_value(reason);
                b2.console(ConsoleLevel::Error, format!("Uncaught (in promise) {rep}"));
                if let Ok(mut st) = b2.st.try_borrow_mut() {
                    st.stats.errors += 1;
                }
            }
        }
        jsengine::PromiseRejection::Handled { promise } => {
            events_js::fire_rejection(&b2, r, promise, None);
        }
    })));
    let b2 = b.clone();
    r.set_error_hook(Some(Box::new(move |r: &mut Realm, e: &jsengine::Exception| {
        crate::script::report(&b2, r, e, "");
    })));
    let b2 = b.clone();
    r.set_clock(Box::new(move || b2.host().map_or(0.0, |h| h.time())));
    let b3 = b.clone();
    r.set_interrupt(Some(Box::new(move || window::interrupt(&b3))));
}

fn this_is_global(r: &mut Realm, t: Value) -> bool {
    let g = r.global();
    t.is_undefined() || r.strict_equals(t, g)
}

fn install_all(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let Tables { et, .. } = window::make_tables(b, r);
    let g = r.global();
    let boot = b
        .st
        .borrow()
        .worker
        .as_ref()
        .map(|w| w.boot.clone())
        .ok_or_else(|| r.throw_error(ErrorKind::Error, "not a worker"))?;
    // DedicatedWorkerGlobalScope.prototype -> WorkerGlobalScope.prototype -> EventTarget.prototype.
    let wgs = r.new_object_with_proto(et);
    let dwgs = r.new_object_with_proto(wgs);
    r.set_prototype(g, dwgs);
    events_js::install(b, r)?;
    frames::install_message_event(b, r)?;
    let tag = r.well_known_symbol(jsengine::WellKnown::ToStringTag);
    let tv = r.string("DedicatedWorkerGlobalScope");
    r.define(dwgs, jsengine::Key::Value(tag), tv, Attrs::CONFIGURABLE)?;
    let wgs_c = r.new_class("WorkerGlobalScope", 0, wgs, None, None);
    r.define(g, "WorkerGlobalScope", wgs_c, Attrs::METHOD)?;
    let dwgs_c = r.new_class("DedicatedWorkerGlobalScope", 0, dwgs, Some(wgs_c), None);
    r.define(g, "DedicatedWorkerGlobalScope", dwgs_c, Attrs::METHOD)?;
    for k in ["self", "globalThis"] {
        r.define(g, k, g, Attrs::DEFAULT)?;
    }

    // on* handlers.
    for &ty in HANDLERS {
        let b2 = b.clone();
        let b3 = b.clone();
        let set: Box<jsengine::NativeFn> = Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
            nodes::handler_set(&b3, r, t, ty, arg(a, 0))
        });
        r.define_getter(
            wgs,
            &format!("on{ty}"),
            move |r, t, _| nodes::handler_get(&b2, r, t, ty),
            Some(set),
        )?;
    }

    // name, origin, isSecureContext, crossOriginIsolated.
    let name = boot.spawn.name.clone();
    r.define_getter(g, "name", move |r, _t, _| Ok(r.string(&name)), None)?;
    let origin = boot.spawn.origin.clone();
    r.define_getter(g, "origin", move |r, _t, _| Ok(r.string(&origin)), None)?;
    r.define(g, "isSecureContext", Value::bool(boot.spawn.secure), Attrs::DEFAULT)?;
    r.define(g, "crossOriginIsolated", Value::FALSE, Attrs::DEFAULT)?;

    install_location(b, r, g)?;
    install_navigator(r, g, &boot)?;
    net::install(b, r, true)?;

    // Timers and microtasks.
    let b2 = b.clone();
    r.define_method(g, "setTimeout", 2, move |r, _t, a| window::timer_add(&b2, r, a, false))?;
    let b2 = b.clone();
    r.define_method(g, "setInterval", 2, move |r, _t, a| window::timer_add(&b2, r, a, true))?;
    let b2 = b.clone();
    r.define_method(g, "clearTimeout", 1, move |r, _t, a| window::timer_clear(&b2, r, a))?;
    let b2 = b.clone();
    r.define_method(g, "clearInterval", 1, move |r, _t, a| window::timer_clear(&b2, r, a))?;
    r.define_method(g, "queueMicrotask", 1, |r, _t, a| {
        let f = arg(a, 0);
        if !r.is_callable(f) {
            return Err(r.throw_error(ErrorKind::TypeError, "queueMicrotask: not a function"));
        }
        r.enqueue_job(f, &[]);
        Ok(Value::UNDEFINED)
    })?;

    // console.
    let con = r.new_object();
    window::console_fn(b, r, con, "log", ConsoleLevel::Log)?;
    window::console_fn(b, r, con, "info", ConsoleLevel::Info)?;
    window::console_fn(b, r, con, "warn", ConsoleLevel::Warn)?;
    window::console_fn(b, r, con, "error", ConsoleLevel::Error)?;
    window::console_fn(b, r, con, "debug", ConsoleLevel::Debug)?;
    window::console_fn(b, r, con, "trace", ConsoleLevel::Debug)?;
    window::console_fn(b, r, con, "dir", ConsoleLevel::Log)?;
    window::console_fn(b, r, con, "table", ConsoleLevel::Log)?;
    let b2 = b.clone();
    r.define_method(con, "assert", 0, move |r, _t, a| {
        if !r.to_boolean(arg(a, 0)) {
            let s = window::console_text(r, a.get(1..).unwrap_or(&[]));
            b2.console(ConsoleLevel::Error, format!("Assertion failed: {s}"));
        }
        Ok(Value::UNDEFINED)
    })?;
    for m in ["group", "groupCollapsed", "groupEnd", "time", "timeEnd", "timeLog", "count", "countReset", "clear"] {
        r.define_method(con, m, 0, |_r, _t, _a| Ok(Value::UNDEFINED))?;
    }
    r.define(g, "console", con, Attrs::METHOD)?;

    // performance.
    let perf = r.new_object();
    let start = b.host().map_or(0.0, |h| h.now());
    let origin_t = b.host().map_or(0.0, |h| h.time());
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.perf_origin = start;
        st.time_origin = origin_t;
    }
    let b2 = b.clone();
    r.define_method(perf, "now", 0, move |_r, _t, _a| {
        let now = b2.host().map_or(start, |h| h.now());
        Ok(Value::number(now - start))
    })?;
    r.define(perf, "timeOrigin", Value::number(origin_t), Attrs::ENUMERABLE)?;
    r.define(g, "performance", perf, Attrs::METHOD)?;

    // postMessage, close, importScripts.
    let b2 = b.clone();
    r.define_method(g, "postMessage", 1, move |r, t, a| {
        if !this_is_global(r, t) {
            return Err(type_error(r, "Illegal invocation"));
        }
        let msg = xfer::encode_post(&b2, r, a, 0, "DedicatedWorkerGlobalScope")?;
        let mut st = b2.st(r)?;
        let Some(w) = st.worker.as_mut() else { return Ok(Value::UNDEFINED) };
        if w.posts.len() >= MAX_POSTS || w.post_bytes + msg.data.len() > MAX_POST_BYTES {
            drop(st);
            xfer::drop_msg(&b2, &msg);
            b2.console(ConsoleLevel::Warn, String::from("Too many messages are waiting to be sent: a postMessage was dropped."));
            return Ok(Value::UNDEFINED);
        }
        w.post_bytes += msg.data.len();
        w.posts.push(super::Post::Message(msg));
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(g, "close", 0, move |r, _t, _a| {
        if let Some(w) = b2.st(r)?.worker.as_mut() {
            w.closing = true;
        }
        Ok(Value::UNDEFINED)
    })?;
    if boot.spawn.kind == WorkerType::Classic {
        let b2 = b.clone();
        r.define_method(g, "importScripts", 0, move |r, _t, a| import_scripts(&b2, r, a))?;
    }

    net::install_members(b, r, true)?;
    net::install_locale_methods(b, r)?;
    net::install_dom_exceptions(b, r)?;
    Ok(())
}

/// `location`: read-only.
fn install_location(b: &Rc<Bridge>, r: &mut Realm, g: Value) -> JsResult<()> {
    let loc = r.new_object();
    type Pick = fn(&window::UrlParts) -> &String;
    let parts: [(&str, Pick); 8] = [
        ("protocol", |u| &u.protocol),
        ("host", |u| &u.host),
        ("hostname", |u| &u.hostname),
        ("port", |u| &u.port),
        ("pathname", |u| &u.pathname),
        ("search", |u| &u.search),
        ("hash", |u| &u.hash),
        ("origin", |u| &u.origin),
    ];
    for (name, pick) in parts {
        let b2 = b.clone();
        r.define_getter(
            loc,
            name,
            move |r, _t, _| {
                let u = window::split_url(&window::location_href(&b2));
                let s = if name == "origin" { b2.origin().text } else { pick(&u).clone() };
                Ok(r.string(&s))
            },
            None,
        )?;
    }
    let b2 = b.clone();
    r.define_getter(
        loc,
        "href",
        move |r, _t, _| {
            let s = window::location_href(&b2);
            Ok(r.string(&s))
        },
        None,
    )?;
    let b2 = b.clone();
    r.define_method(loc, "toString", 0, move |r, _t, _a| {
        let s = window::location_href(&b2);
        Ok(r.string(&s))
    })?;
    let c = r.new_class("WorkerLocation", 0, loc, None, None);
    r.define(g, "WorkerLocation", c, Attrs::METHOD)?;
    r.define(g, "location", loc, Attrs::ENUMERABLE | Attrs::CONFIGURABLE)?;
    Ok(())
}

/// `navigator`: the worker subset.
fn install_navigator(r: &mut Realm, g: Value, boot: &WorkerBoot) -> JsResult<()> {
    let nav = r.new_object();
    let ua = boot.user_agent.clone();
    let uav = r.string(&ua);
    r.define(nav, "userAgent", uav, Attrs::ENUMERABLE)?;
    for (k, v) in [
        ("appName", "Netscape"),
        ("appCodeName", "Mozilla"),
        ("product", "Gecko"),
        ("platform", "milkOS"),
        ("language", "en-US"),
    ] {
        let s = r.string(v);
        r.define(nav, k, s, Attrs::ENUMERABLE)?;
    }
    let av = r.string(ua.strip_prefix("Mozilla/").unwrap_or(&ua));
    r.define(nav, "appVersion", av, Attrs::ENUMERABLE)?;
    let en = r.string("en-US");
    let en2 = r.string("en");
    let langs = r.new_array(&[en, en2]);
    r.define(nav, "languages", langs, Attrs::ENUMERABLE)?;
    r.define(nav, "onLine", Value::TRUE, Attrs::ENUMERABLE)?;
    r.define(nav, "hardwareConcurrency", Value::int(1), Attrs::ENUMERABLE)?;
    r.define(nav, "webdriver", Value::FALSE, Attrs::ENUMERABLE)?;
    let c = r.new_class("WorkerNavigator", 0, nav, None, None);
    r.define(g, "WorkerNavigator", c, Attrs::METHOD)?;
    r.define(g, "navigator", nav, Attrs::ENUMERABLE | Attrs::CONFIGURABLE)?;
    Ok(())
}

/// `importScripts(...urls)`: fetch each in order (the thread waits), run it in the global.
fn import_scripts(b: &Rc<Bridge>, r: &mut Realm, a: &[Value]) -> JsResult<Value> {
    for i in 0..a.len().min(64) {
        let raw = arg_str(r, a, i)?;
        let abs = {
            let p = b.page_or(r)?;
            p.resolve(raw.trim())
        };
        let Some(u) = net::parse_url(&abs, None) else {
            return Err(dom_error(
                r,
                "SyntaxError",
                &format!("Failed to execute 'importScripts' on 'WorkerGlobalScope': The URL '{raw}' is invalid."),
            ));
        };
        if u.scheme != "http" && u.scheme != "https" && u.scheme != "data" {
            b.console(
                ConsoleLevel::Error,
                format!("importScripts: only http, https and data addresses can be loaded, not {}", u.scheme),
            );
            return Err(dom_error(
                r,
                "NetworkError",
                &format!("Failed to execute 'importScripts' on 'WorkerGlobalScope': The script at '{}' failed to load.", u.href_no_fragment()),
            ));
        }
        let url = u.href_no_fragment();
        let id = b.st(r)?.net.alloc_id();
        let req = NetRequest {
            id,
            kind: NetKind::Script,
            url: url.clone(),
            method: String::from("GET"),
            headers: Vec::new(),
            body: Vec::new(),
            mode: RequestMode::Cors,
            credentials: CredentialsMode::SameOrigin,
            redirect: RedirectMode::Follow,
            referrer: String::from("about:client"),
            referrer_policy: String::new(),
            cache: String::from("default"),
            timeout_ms: 0,
            force_preflight: false,
        };
        let answer = match b.host() {
            Some(h) => h.fetch_blocking(req),
            None => None,
        };
        let fail = |b: &Rc<Bridge>, r: &mut Realm, why: &str| -> jsengine::Exception {
            b.console(ConsoleLevel::Error, format!("importScripts: {why}"));
            dom_error(
                r,
                "NetworkError",
                &format!("Failed to execute 'importScripts' on 'WorkerGlobalScope': The script at '{url}' failed to load."),
            )
        };
        let (head, body) = match answer {
            None => return Err(fail(b, r, "not available here")),
            Some(Err(why)) => return Err(fail(b, r, &why)),
            Some(Ok(x)) => x,
        };
        if !(200..300).contains(&head.status) {
            return Err(fail(b, r, &format!("{url} answered {} {}", head.status, head.status_text)));
        }
        let body = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&body);
        let max = b.st(r)?.limits.max_source;
        let body = &body[..body.len().min(max)];
        let text = match core::str::from_utf8(body) {
            Ok(s) => String::from(s),
            Err(_) => body.iter().map(|&x| crate::charset::windows1252(x)).collect(),
        };
        b.st(r)?.stats.scripts += 1;
        r.eval(&text, &url)?;
    }
    let _ = WINDOW;
    Ok(Value::UNDEFINED)
}

/// Fire `message` at the global: a message from the page, one task (the caller runs the
/// microtask checkpoint).
pub fn fire_message(b: &Rc<Bridge>, r: &mut Realm, msg: &WireMsg) {
    let proto = {
        let st = b.st.borrow();
        match st.outbox.message_proto.as_ref() {
            Some(p) => r.pinned(p),
            None => return,
        }
    };
    let (ty, data, ports) = match xfer::decode_post(b, r, msg, 0) {
        Ok((d, p)) => ("message", d, p),
        Err(_) => ("messageerror", Value::NULL, r.new_array(&[])),
    };
    let e = events_js::new_event_on(b, r, proto, ty, events_js::TRUSTED);
    let _ = r.define(e, "data", data, Attrs::ENUMERABLE);
    let empty = r.string("");
    let _ = r.define(e, "origin", empty, Attrs::ENUMERABLE);
    let _ = r.define(e, "lastEventId", empty, Attrs::ENUMERABLE);
    let _ = r.define(e, "source", Value::NULL, Attrs::ENUMERABLE);
    let _ = r.define(e, "ports", ports, Attrs::ENUMERABLE);
    events_js::dispatch(b, r, WINDOW, e);
}

/// What the worker has for the thread to send up.
pub fn take_out(b: &Bridge) -> Vec<FromWorker> {
    let mut v = Vec::new();
    if let Ok(mut st) = b.st.try_borrow_mut()
        && let Some(w) = st.worker.as_mut()
    {
        w.post_bytes = 0;
        for p in core::mem::take(&mut w.posts) {
            v.push(match p {
                super::Post::Message(m) => FromWorker::Message(m),
                super::Post::Error { message, filename, line, column } => {
                    FromWorker::Error { message, filename, line, column }
                }
            });
        }
    }
    let table = b.ports();
    if table.has_links() {
        for lo in table.take_link_out(0, 1024) {
            v.push(match lo {
                crate::script::ports::LinkOut::Msg { port, data, ports, .. } => FromWorker::Port {
                    port,
                    msg: Some(WireMsg { data, xfer: ports.into_iter().map(Xfer::Port).collect() }),
                },
                crate::script::ports::LinkOut::Close { port, .. } => FromWorker::Port { port, msg: None },
            });
        }
    }
    v
}
