//! Scripts and frames: what the bindings of one window need from the frame tree, the `message`
//! event, and the queue of messages a window posted. The objects scripts see across frames
//! (`window.parent`, `contentWindow`, ...) are built on this by the bindings; this file has
//! the engine side:
//!
//! - `window.postMessage` queues an [`OutMessage`] (the data as JSON text) for the host
//!   ([`Document::take_messages`]); the host checks the origins and calls
//!   [`Document::deliver_message`] on the target frame's document, which fires `message` at
//!   its window, a `MessageEvent` with `data`, `origin`, `source` and `lastEventId`.
//! - [`window_proxy`] is the one place that turns a frame id into the object a script sees
//!   (`event.source`); the bindings that make `window.parent` and friends replace its body.
//! - [`own_frame`] and [`window_name`] read where this window sits in the tree.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Attrs, JsResult, Pinned, Realm, Value};

use super::bridge::{Bridge, arg, arg_str, dom_error, type_error};
use super::events::WINDOW;
use super::events_js::{self, copy_init, init_flags, new_event_on};
use crate::api::*;

/// Messages one window may have queued for the host at a time, and the size of one.
pub const MAX_QUEUED: usize = 256;
pub const MAX_MESSAGE_BYTES: usize = 4 << 20;

/// The rest of the page's frames, as a call from the app armed them for the scripts of one
/// document: raw pointers valid for the length of that call only (the bridge clears them).
#[derive(Clone, Copy)]
pub struct Peers {
    pub(crate) frames: *mut crate::frames::Frames,
    pub(crate) root: *mut Document,
}

/// Run `f` on the document of frame `id`, another frame of this page, from a native of this
/// window, with that frame's host (see [`ScriptHost::frame_host`]). None if there is no such
/// frame, no host for it, or it is busy (it is running right now: a script of it called into
/// this one). `f` is the way to enter the other realm: it gets the [`Document`], runs
/// whatever it needs through `doc.js` and `doc.page` and hands back a plain Rust value.
pub fn with_peer<R>(
    b: &Bridge,
    id: FrameId,
    f: impl FnOnce(&mut Document, &mut dyn ScriptHost) -> R,
) -> Option<R> {
    let peers = b.peers()?;
    let me = b.host()?;
    let host = me.frame_host(id)?;
    // SAFETY: the pointers were made from `&mut` borrows that the call from the app is
    // holding without using until it returns (it is waiting on us); a document is taken out
    // of its slot before it is used, so two uses of one can't overlap, and the root is only
    // used when it is not running a script itself.
    unsafe {
        if id.is_top() {
            let root = &mut *peers.root;
            if root.js.as_ref().is_none_or(|j| j.busy()) {
                return None;
            }
            let saved = root.peers;
            root.peers = Some(peers);
            let out = f(root, host);
            root.peers = saved;
            return Some(out);
        }
        let frames = &mut *peers.frames;
        let mut doc = frames.slot_mut(id)?.doc.take()?;
        if doc.js.as_ref().is_some_and(|j| j.busy()) {
            if let Some(s) = frames.slot_mut(id) {
                s.doc = Some(doc);
            }
            return None;
        }
        doc.peers = Some(peers);
        let out = f(&mut doc, host);
        doc.peers = None;
        let frames = &mut *peers.frames;
        if let Some(s) = frames.slot_mut(id)
            && s.doc.is_none()
        {
            s.doc = Some(doc);
        }
        Some(out)
    }
}

/// What the window has queued, and the `MessageEvent` prototype.
#[derive(Default)]
pub struct Outbox {
    pub q: Vec<OutMessage>,
    pub message_proto: Option<Pinned>,
}

/// This window's own frame id (the top document's is [`FrameId::TOP`]).
pub fn own_frame(b: &Bridge) -> FrameId {
    b.page().map_or(FrameId::TOP, |p| p.frame.id)
}

/// This window's `window.name`.
pub fn window_name(b: &Bridge) -> String {
    b.page().map(|p| p.window_name.clone()).unwrap_or_default()
}

/// Set this window's `window.name`.
pub fn set_window_name(b: &Bridge, name: &str) {
    if let Some(p) = b.page() {
        p.window_name = String::from(name);
    }
}

/// The script object for frame `id` as seen from this window: `window` for itself. Other frames
/// are `null` until the bindings of window proxies replace this.
pub fn window_proxy(b: &Rc<Bridge>, r: &mut Realm, id: FrameId) -> Value {
    if id == own_frame(b) {
        r.global()
    } else {
        Value::NULL
    }
}

/// `JSON.stringify(v)` as text; None for `undefined` (and for things JSON has no form for).
fn to_json(r: &mut Realm, v: Value) -> JsResult<Option<String>> {
    let g = r.global();
    let json = r.get(g, "JSON")?;
    let out = r.call_method(json, "stringify", &[v])?;
    Ok(r.get_string(out))
}

fn from_json(r: &mut Realm, text: &str) -> Value {
    let g = r.global();
    let Ok(json) = r.get(g, "JSON") else {
        return Value::NULL;
    };
    let s = r.string(text);
    r.call_method(json, "parse", &[s]).unwrap_or(Value::NULL)
}

/// `window.postMessage(message, targetOrigin)` / `(message, {targetOrigin})` on this window.
fn post_message(b: &Rc<Bridge>, r: &mut Realm, a: &[Value]) -> JsResult<Value> {
    let target = own_frame(b);
    queue(b, r, target, a)
}

/// Queue a message for the window of `target`: what `postMessage` on a window proxy calls too.
pub fn queue(b: &Rc<Bridge>, r: &mut Realm, target: FrameId, a: &[Value]) -> JsResult<Value> {
    if a.is_empty() {
        return Err(type_error(
            r,
            "postMessage: 1 argument required, but only 0 present.",
        ));
    }
    let opt = arg(a, 1);
    let target_origin = if opt.is_object() {
        let v = r.get(opt, "targetOrigin")?;
        if v.is_undefined() {
            String::from("/")
        } else {
            r.to_string(v)?
        }
    } else if opt.is_undefined() {
        // The two-argument form needs the origin; the options form defaults to "/".
        String::from("/")
    } else {
        arg_str(r, a, 1)?
    };
    let data = match to_json(r, a[0]) {
        Ok(d) => d.unwrap_or_default(),
        Err(_) => {
            return Err(dom_error(
                r,
                "DataCloneError",
                "Failed to execute 'postMessage': the message could not be cloned.",
            ));
        }
    };
    if data.len() > MAX_MESSAGE_BYTES {
        return Err(dom_error(
            r,
            "DataCloneError",
            "Failed to execute 'postMessage': the message is too large.",
        ));
    }
    let mut st = b.st(r)?;
    if st.outbox.q.len() >= MAX_QUEUED {
        drop(st);
        b.console(
            crate::api::ConsoleLevel::Warn,
            String::from("Too many messages are waiting: a postMessage was dropped."),
        );
        return Ok(Value::UNDEFINED);
    }
    st.outbox.q.push(OutMessage {
        target,
        target_origin,
        data,
    });
    Ok(Value::UNDEFINED)
}

/// Fire `message` at this window.
pub fn fire_message(b: &Rc<Bridge>, r: &mut Realm, msg: &Message) {
    let proto = {
        let st = b.st.borrow();
        match st.outbox.message_proto.as_ref() {
            Some(p) => r.pinned(p),
            None => return,
        }
    };
    let e = new_event_on(b, r, proto, "message", events_js::TRUSTED);
    let data = if msg.data.is_empty() {
        Value::UNDEFINED
    } else {
        from_json(r, &msg.data)
    };
    let _ = r.define(e, "data", data, Attrs::ENUMERABLE);
    let o = r.string(&msg.origin);
    let _ = r.define(e, "origin", o, Attrs::ENUMERABLE);
    let l = r.string("");
    let _ = r.define(e, "lastEventId", l, Attrs::ENUMERABLE);
    let src = window_proxy(b, r, msg.source);
    let _ = r.define(e, "source", src, Attrs::ENUMERABLE);
    let ports = r.new_array(&[]);
    let _ = r.define(e, "ports", ports, Attrs::ENUMERABLE);
    events_js::dispatch(b, r, WINDOW, e);
}

/// The `message` event for `msg`, fired at the window of `d`.
pub fn deliver_message(d: &mut Document, host: &mut dyn ScriptHost, msg: &Message) -> bool {
    d.armed(|d| match d.js.as_mut() {
        Some(j) if !j.stopped() => j.deliver(&mut d.page, host, msg),
        _ => false,
    })
}

/// Define `MessageEvent` and the working `window.postMessage`.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) {
    if let Err(e) = install_all(b, r) {
        super::report(b, r, &e, "setting up messages");
    }
}

fn install_all(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let g = r.global();
    let ep = r.pinned(&b.protos().event);
    let proto = r.new_object_with_proto(ep);
    let pin = r.pin(proto);
    let event_ctor = r.get(g, "Event")?;
    let b2 = b.clone();
    let ctor = r.new_class(
        "MessageEvent",
        1,
        proto,
        Some(event_ctor),
        Some(alloc::boxed::Box::new(move |r: &mut Realm, _this: Value, a: &[Value]| {
            if a.is_empty() {
                return Err(type_error(r, "MessageEvent: 1 argument required"));
            }
            let ty = arg_str(r, a, 0)?;
            let f = init_flags(r, arg(a, 1))?;
            let p = match b2.st.borrow().outbox.message_proto.as_ref() {
                Some(p) => r.pinned(p),
                None => return Err(type_error(r, "MessageEvent is not available")),
            };
            let e = new_event_on(&b2, r, p, &ty, f);
            if let Some(nt) = r.new_target() {
                let np = r.get(nt, "prototype")?;
                if np.is_object() {
                    r.set_prototype(e, np);
                }
            }
            let empty = r.string("");
            let ports = r.new_array(&[]);
            copy_init(
                r,
                e,
                arg(a, 1),
                &[
                    ("data", Value::NULL),
                    ("origin", empty),
                    ("lastEventId", empty),
                    ("source", Value::NULL),
                    ("ports", ports),
                ],
            )?;
            Ok(e)
        })),
    );
    r.define(g, "MessageEvent", ctor, Attrs::METHOD)?;
    b.st.borrow_mut().outbox.message_proto = Some(pin);
    let b3 = b.clone();
    r.define_method(g, "postMessage", 2, move |r, _t, a| post_message(&b3, r, a))?;
    // window.name: the name of this browsing context, kept by the page (the frame table sets
    // it from the element's `name`, the host from where the frame came).
    let b4 = b.clone();
    let b5 = b.clone();
    r.define_getter(
        g,
        "name",
        move |r, _t, _a| Ok(r.string(&window_name(&b4))),
        Some(alloc::boxed::Box::new(move |r: &mut Realm, _t: Value, a: &[Value]| {
            let s = arg_str(r, a, 0)?;
            // Names are short, like browsers' (2 MB there; plenty here).
            let s = if s.len() > 64 << 10 { String::new() } else { s };
            set_window_name(&b5, &s);
            Ok(Value::UNDEFINED)
        })),
    )?;
    Ok(())
}
