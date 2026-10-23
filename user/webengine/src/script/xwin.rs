//! The window properties that talk about other frames: `parent`, `top`, `frames`, `length`,
//! `name`, `frameElement`, `postMessage`, and the iframe element's `contentWindow` and
//! `contentDocument`. A page with no frames sees `parent === top === window` and no children,
//! as on the web.

use alloc::rc::Rc;
use alloc::string::String;

use jsengine::{HostClass, JsResult, Key, OwnedKey, Realm, Value, hooks};

use super::bridge::{Bridge, arg, arg_str};
use super::hub::{self};
use super::nodes::{getter, method, this_node};
use super::remote::{self, Xv};
use crate::dom::NodeId;

struct FramesClass {
    b: Rc<Bridge>,
}

impl HostClass for FramesClass {
    fn name(&self) -> &str {
        "Window"
    }
    fn hooks(&self) -> u8 {
        hooks::INDEXED | hooks::NAMED
    }
    fn get(&self, r: &mut Realm, _o: Value, _d: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        let b = &self.b;
        let me = b.frame_id();
        let Some(h) = b.hub() else {
            return Ok(match key {
                Key::Str("length") => Some(Value::int(0)),
                _ => None,
            });
        };
        match key {
            Key::Index(i) => remote::child_window(b, r, i).map(Some),
            Key::Str("length") => Ok(Some(Value::int(h.children(me).len() as i32))),
            Key::Str("window" | "self" | "frames") => Ok(Some(r.global())),
            Key::Str(name) => {
                for c in h.children(me) {
                    if h.name_of(c).as_deref() == Some(name) {
                        return remote::window_of(b, r, c).map(Some);
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
    fn has(&self, r: &mut Realm, o: Value, d: u64, key: Key<'_>) -> JsResult<Option<bool>> {
        Ok(self.get(r, o, d, key)?.map(|v| !v.is_undefined()))
    }
    fn keys(&self, _r: &mut Realm, _o: Value, _d: u64) -> JsResult<alloc::vec::Vec<OwnedKey>> {
        let b = &self.b;
        let n = b.hub().map_or(0, |h| h.children(b.frame_id()).len());
        Ok((0..n as u32).map(OwnedKey::Index).collect())
    }
}

pub fn install(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let g = r.global();
    let cls = r.register_class(FramesClass { b: b.clone() });
    let proto = r.intrinsic(jsengine::Intrinsic::ObjectPrototype);
    let frames = r.new_host_object(cls, 0, proto);
    let pf = r.pin(frames);
    *b.xf.frames_obj.borrow_mut() = Some(pf);
    getter(b, r, g, "frames", |b, r, _t, _a| {
        let v = b.xf.frames_obj.borrow().as_ref().map(|p| r.pinned(p));
        Ok(v.unwrap_or(Value::UNDEFINED))
    }, Some(replace_frames))?;
    getter(b, r, g, "parent", |b, r, _t, _a| {
        let me = b.frame_id();
        match hub::peek(b, me).and_then(|p| p.parent) {
            Some(p) => remote::window_of(b, r, p),
            None => Ok(r.global()),
        }
    }, Some(replace_parent))?;
    getter(b, r, g, "top", |b, r, _t, _a| remote::window_of(b, r, 0), None)?;
    getter(b, r, g, "length", |b, _r, _t, _a| {
        let n = b.hub().map_or(0, |h| h.children(b.frame_id()).len());
        Ok(Value::int(n as i32))
    }, Some(replace_length))?;
    getter(
        b,
        r,
        g,
        "name",
        |b, r, _t, _a| {
            let n = b.page().map(|p| p.window_name.clone()).unwrap_or_default();
            Ok(r.string(&n))
        },
        Some(|b, r, _t, a| {
            let s = arg_str(r, a, 0)?;
            if let Some(p) = b.page() {
                p.window_name = s;
            }
            Ok(Value::UNDEFINED)
        }),
    )?;
    getter(b, r, g, "frameElement", |b, r, _t, _a| frame_element(b, r), None)?;
    method(b, r, g, "postMessage", 2, |b, r, _t, a| {
        let me = b.frame_id();
        super::postmsg::post_message(b, r, me, a)
    })?;
    Ok(())
}

// `frames`, `parent` and `length` are [Replaceable]: `var frames = 0` at the top of a page makes
// an ordinary global variable of that name, as on the web.
fn replace(r: &mut Realm, name: &str, a: &[Value]) -> JsResult<Value> {
    let g = r.global();
    r.define(g, name, arg(a, 0), jsengine::Attrs::DEFAULT)?;
    Ok(Value::UNDEFINED)
}

fn replace_frames(_b: &Rc<Bridge>, r: &mut Realm, _t: Value, a: &[Value]) -> JsResult<Value> {
    replace(r, "frames", a)
}

fn replace_parent(_b: &Rc<Bridge>, r: &mut Realm, _t: Value, a: &[Value]) -> JsResult<Value> {
    replace(r, "parent", a)
}

fn replace_length(_b: &Rc<Bridge>, r: &mut Realm, _t: Value, a: &[Value]) -> JsResult<Value> {
    replace(r, "length", a)
}

/// The `<iframe>` element that holds this window, if the parent is of the same origin.
fn frame_element(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<Value> {
    let me = b.frame_id();
    let Some(h) = b.hub() else {
        return Ok(Value::NULL);
    };
    let Some((parent, node)) = h.element_of(me) else {
        return Ok(Value::NULL);
    };
    let Some(p) = hub::peek(b, parent) else {
        return Ok(Value::NULL);
    };
    if !b.origin().same(&p.origin) {
        return Ok(Value::NULL);
    }
    let x = hub::enter(b, parent, p.uid, |bb, rb| {
        let v = super::nodes::wrap(bb, rb, node as NodeId);
        remote::export(bb, rb, v)
    });
    match x {
        Ok(Ok(x)) => remote::import(b, r, x),
        _ => Ok(Value::NULL),
    }
}

/// `iframe.contentWindow`.
pub fn content_window(b: &Rc<Bridge>, r: &mut Realm, this: Value) -> JsResult<Value> {
    let n = this_node(b, r, this)?;
    let Some(f) = b.page().and_then(|p| p.frame_nodes.get(&n).copied()) else {
        return Ok(Value::NULL);
    };
    if hub::peek(b, f.0).is_none() {
        return Ok(Value::NULL);
    }
    remote::window_of(b, r, f.0)
}

/// `iframe.contentDocument`: the document, if it is of the same origin, else null.
pub fn content_document(b: &Rc<Bridge>, r: &mut Realm, this: Value) -> JsResult<Value> {
    let n = this_node(b, r, this)?;
    let Some(f) = b.page().and_then(|p| p.frame_nodes.get(&n).copied()) else {
        return Ok(Value::NULL);
    };
    let Some(p) = hub::peek(b, f.0) else {
        return Ok(Value::NULL);
    };
    if !b.origin().same(&p.origin) {
        return Ok(Value::NULL);
    }
    let x = hub::enter(b, f.0, p.uid, |bb, rb| {
        let v = super::nodes::wrap(bb, rb, 0);
        remote::export(bb, rb, v)
    });
    match x {
        Ok(Ok(x)) => remote::import(b, r, x),
        _ => Ok(Value::NULL),
    }
}

#[allow(dead_code)]
fn _unused(_: Xv, _: String) {
    let _ = arg;
}
