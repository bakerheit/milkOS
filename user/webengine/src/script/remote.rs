//! Proxies for the objects of other frames' realms (see [`super::hub`] for the idea): values
//! crossing a frame boundary ([`Xv`]), the host class that forwards every property operation
//! ([`RemoteClass`]), function proxies, and the window proxies with the same-origin policy
//! ([`super::xorigin`]) in front of them.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{
    Attrs, ClassId, ErrorKind, Exception, HostClass, Intrinsic, JsResult, Key, OwnedKey, Realm,
    Value, WellKnown, hooks,
};

use super::bridge::{Bridge, arg, dom_error};
use super::hub::{self, Adjust, Gone, RemoteKey, RemoteRef, Target};
use super::xorigin::{self, Use};
use crate::api::*;

/// A value on its way to another realm.
#[derive(Clone, Debug)]
pub enum Xv {
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Ref(XRef),
}

#[derive(Clone, Copy, Debug)]
pub struct XRef {
    pub key: RemoteKey,
    pub callable: bool,
    /// The sender took a reference on the handle for this transfer: the receiver either keeps it
    /// (its new proxy) or gives it back.
    pub counted: bool,
}

/// An error that happened in another realm, carried over by name and message.
#[derive(Clone, Debug)]
pub struct XErr {
    pub name: String,
    pub message: String,
}

pub type XRes = Result<Xv, XErr>;

fn xerr_of(r: &mut Realm, e: &Exception) -> XErr {
    if let Some(v) = e.value() {
        if v.is_object() {
            let n = r.get(v, "name").ok().and_then(|n| r.to_string(n).ok());
            let m = r.get(v, "message").ok().and_then(|m| r.to_string(m).ok());
            return XErr {
                name: n.unwrap_or_else(|| String::from("Error")),
                message: m.unwrap_or_default(),
            };
        }
        let s = r.to_string(v).unwrap_or_default();
        return XErr {
            name: String::from("Error"),
            message: s,
        };
    }
    XErr {
        name: String::from("Error"),
        message: String::from("the other frame's script was stopped"),
    }
}

fn throw_xerr(r: &mut Realm, e: XErr) -> Exception {
    dom_error(r, &e.name, &e.message)
}

fn gone_error(r: &mut Realm, g: Gone) -> Exception {
    match g {
        Gone::Dead => dom_error(
            r,
            "InvalidAccessError",
            "The other frame is gone (it was removed, or went to another page).",
        ),
        Gone::Busy => dom_error(
            r,
            "InvalidStateError",
            "The other frame is busy (it is waiting for this one).",
        ),
    }
}

fn security_error(b: &Bridge, r: &mut Realm, other: &xorigin::Origin) -> Exception {
    let me = b.origin();
    let msg = format!(
        "Blocked a frame with origin \"{}\" from accessing a cross-origin frame (\"{}\").",
        me.text, other.text
    );
    dom_error(r, "SecurityError", &msg)
}

// ---- Values across ----

/// Make `v` (a value of this realm) ready to cross.
pub fn export(b: &Rc<Bridge>, r: &mut Realm, v: Value) -> Result<Xv, XErr> {
    use jsengine::Kind;
    Ok(match v.kind() {
        Kind::Undefined | Kind::Symbol => Xv::Undef,
        Kind::Null => Xv::Null,
        Kind::Boolean => Xv::Bool(v.as_bool().unwrap_or(false)),
        Kind::Number => Xv::Num(v.as_number().unwrap_or(f64::NAN)),
        Kind::String => Xv::Str(r.get_string(v).unwrap_or_default()),
        Kind::Object => {
            let me = b.frame_id();
            let uid = b.xf.uid;
            // A proxy of another realm's object goes back as what it stands for.
            if let Some(cls) = b.xf.class.get()
                && let Some(idx) = r.host_data(v, cls)
            {
                let rr = b.xf.remotes.borrow().get(idx as usize).copied().flatten();
                if let Some(rr) = rr {
                    if let Target::Handle(h) = rr.key.target
                        && let Some(hub) = b.hub()
                    {
                        hub.push(Adjust {
                            frame: rr.key.frame,
                            uid: rr.key.uid,
                            handle: h,
                            delta: 1,
                        });
                    }
                    return Ok(Xv::Ref(XRef {
                        key: rr.key,
                        callable: false,
                        counted: matches!(rr.key.target, Target::Handle(_)),
                    }));
                }
            }
            let g = r.global();
            if v.is_same_handle(&g) {
                return Ok(Xv::Ref(XRef {
                    key: RemoteKey {
                        frame: me,
                        uid,
                        target: Target::Window,
                    },
                    callable: false,
                    counted: false,
                }));
            }
            let callable = r.is_callable(v);
            let h = b.xf.exports.borrow_mut().add(r, v);
            let Some(h) = h else {
                return Err(XErr {
                    name: String::from("QuotaExceededError"),
                    message: String::from("too many objects shared with other frames"),
                });
            };
            Xv::Ref(XRef {
                key: RemoteKey {
                    frame: me,
                    uid,
                    target: Target::Handle(h),
                },
                callable,
                counted: true,
            })
        }
    })
}

/// A value of another realm (or this one) arrives.
pub fn import(b: &Rc<Bridge>, r: &mut Realm, x: Xv) -> JsResult<Value> {
    Ok(match x {
        Xv::Undef => Value::UNDEFINED,
        Xv::Null => Value::NULL,
        Xv::Bool(t) => Value::bool(t),
        Xv::Num(n) => Value::number(n),
        Xv::Str(s) => r.string(&s),
        Xv::Ref(rf) => {
            let me = b.frame_id();
            if rf.key.frame == me && rf.key.uid == b.xf.uid {
                // One of ours that went out and came back.
                return match rf.key.target {
                    Target::Window => Ok(r.global()),
                    Target::Location => r.get(r.global(), "location"),
                    Target::Handle(h) => {
                        let v = b.xf.exports.borrow().get(h).map(|p| r.pinned(p));
                        if rf.counted {
                            b.xf.exports.borrow_mut().adjust(r, h, -1);
                        }
                        Ok(v.unwrap_or(Value::UNDEFINED))
                    }
                };
            }
            proxy_for(b, r, rf)?
        }
    })
}

/// The window of frame `frame` as this realm sees it: its own global, or a proxy.
pub fn window_of(b: &Rc<Bridge>, r: &mut Realm, frame: u32) -> JsResult<Value> {
    if frame == b.frame_id() {
        return Ok(r.global());
    }
    let Some(p) = hub::peek(b, frame) else {
        return Ok(Value::NULL);
    };
    proxy_for(
        b,
        r,
        XRef {
            key: RemoteKey {
                frame,
                uid: p.uid,
                target: Target::Window,
            },
            callable: false,
            counted: false,
        },
    )
}

fn ensure_class(b: &Rc<Bridge>, r: &mut Realm) -> ClassId {
    if let Some(c) = b.xf.class.get() {
        return c;
    }
    let c = r.register_class(RemoteClass { b: b.clone() });
    b.xf.class.set(Some(c));
    c
}

/// Hand back what the collector freed.
fn sweep(b: &Rc<Bridge>) {
    let freed: Vec<u64> = core::mem::take(&mut *b.xf.freed.borrow_mut());
    if freed.is_empty() {
        return;
    }
    let hub = b.hub();
    let mut remotes = b.xf.remotes.borrow_mut();
    let mut proxies = b.xf.proxies.borrow_mut();
    for idx in freed {
        let Some(slot) = remotes.get_mut(idx as usize) else {
            continue;
        };
        if let Some(rr) = slot.take() {
            if rr.counted
                && let Target::Handle(h) = rr.key.target
                && let Some(hub) = &hub
            {
                hub.push(Adjust {
                    frame: rr.key.frame,
                    uid: rr.key.uid,
                    handle: h,
                    delta: -1,
                });
            }
            proxies.remove(&rr.key);
        }
    }
}

/// The proxy for `rf` in this realm (the same one every time while it lives).
fn proxy_for(b: &Rc<Bridge>, r: &mut Realm, rf: XRef) -> JsResult<Value> {
    sweep(b);
    let cached = b.xf.proxies.borrow().get(&rf.key).copied_weak();
    if let Some(w) = cached
        && let Some(v) = r.upgrade(&w)
    {
        // This transfer's reference is not needed: the proxy has one.
        if rf.counted
            && let Target::Handle(h) = rf.key.target
            && let Some(hub) = b.hub()
        {
            hub.push(Adjust {
                frame: rf.key.frame,
                uid: rf.key.uid,
                handle: h,
                delta: -1,
            });
        }
        return Ok(v);
    }
    let rr = RemoteRef {
        key: rf.key,
        counted: rf.counted,
    };
    let v = if rf.callable {
        let b2 = b.clone();
        r.new_function("", 0, move |r, this, args| call_remote(&b2, r, rr, this, args))
    } else {
        let cls = ensure_class(b, r);
        let idx = {
            let mut t = b.xf.remotes.borrow_mut();
            match t.iter().position(|s| s.is_none()) {
                Some(i) => {
                    t[i] = Some(rr);
                    i
                }
                None => {
                    t.push(Some(rr));
                    t.len() - 1
                }
            }
        };
        let proto = r.intrinsic(Intrinsic::ObjectPrototype);
        r.new_host_object(cls, idx as u64, proto)
    };
    let w = r.weak(v);
    b.xf.proxies.borrow_mut().insert(rf.key, w);
    Ok(v)
}

trait CopiedWeak {
    fn copied_weak(self) -> Option<jsengine::WeakValue>;
}

impl CopiedWeak for Option<&jsengine::WeakValue> {
    fn copied_weak(self) -> Option<jsengine::WeakValue> {
        self.cloned()
    }
}

// ---- Entering the other realm ----

/// The object `t` names, inside its own realm.
fn resolve(b: &Rc<Bridge>, r: &mut Realm, t: Target) -> Result<Value, XErr> {
    match t {
        Target::Window => Ok(r.global()),
        Target::Location => {
            let g = r.global();
            r.get(g, "location").map_err(|e| xerr_of(r, &e))
        }
        Target::Handle(h) => {
            let v = b.xf.exports.borrow().get(h).map(|p| r.pinned(p));
            v.ok_or(XErr {
                name: String::from("InvalidAccessError"),
                message: String::from("that object is gone"),
            })
        }
    }
}

/// Run `f` on the object a remote reference stands for, in its realm.
fn with_remote(
    b: &Rc<Bridge>,
    r: &mut Realm,
    key: RemoteKey,
    f: impl FnOnce(&Rc<Bridge>, &mut Realm, Value) -> Result<Xv, XErr>,
) -> JsResult<Value> {
    sweep(b);
    let res = hub::enter(b, key.frame, key.uid, |bb, rb| {
        let obj = resolve(bb, rb, key.target)?;
        f(bb, rb, obj)
    });
    match res {
        Err(g) => Err(gone_error(r, g)),
        Ok(Err(e)) => Err(throw_xerr(r, e)),
        Ok(Ok(x)) => import(b, r, x),
    }
}

/// A key that can cross realms.
#[derive(Clone, Debug)]
enum OKey {
    Str(String),
    Index(u32),
    Iterator,
}

fn okey(r: &mut Realm, key: Key<'_>) -> Option<OKey> {
    match key {
        Key::Str(s) => Some(OKey::Str(String::from(s))),
        Key::Index(i) => Some(OKey::Index(i)),
        Key::Value(sym) => {
            let it = r.well_known_symbol(WellKnown::Iterator);
            sym.is_same_handle(&it).then_some(OKey::Iterator)
        }
    }
}

fn get_k(r: &mut Realm, obj: Value, k: &OKey) -> Result<Value, XErr> {
    let res = match k {
        OKey::Str(s) => r.get(obj, s.as_str()),
        OKey::Index(i) => r.get(obj, *i),
        OKey::Iterator => {
            let s = r.well_known_symbol(WellKnown::Iterator);
            r.get(obj, s)
        }
    };
    res.map_err(|e| xerr_of(r, &e))
}

// ---- Calling ----

fn call_remote(
    b: &Rc<Bridge>,
    r: &mut Realm,
    rr: RemoteRef,
    this: Value,
    args: &[Value],
) -> JsResult<Value> {
    let construct = r.new_target().is_some();
    let this_x = export(b, r, this).map_err(|e| throw_xerr(r, e))?;
    let mut args_x = Vec::with_capacity(args.len());
    for a in args.iter().take(64) {
        args_x.push(export(b, r, *a).map_err(|e| throw_xerr(r, e))?);
    }
    with_remote(b, r, rr.key, move |bb, rb, f| {
        let mut av = Vec::with_capacity(args_x.len());
        for x in args_x {
            av.push(import(bb, rb, x).map_err(|e| xerr_of(rb, &e))?);
        }
        let out = if construct {
            rb.construct(f, &av)
        } else {
            let t = import(bb, rb, this_x).map_err(|e| xerr_of(rb, &e))?;
            rb.call(f, t, &av)
        };
        let v = out.map_err(|e| xerr_of(rb, &e))?;
        export(bb, rb, v)
    })
}

// ---- The host class ----

pub struct RemoteClass {
    pub b: Rc<Bridge>,
}

impl RemoteClass {
    fn rr(&self, data: u64) -> Option<RemoteRef> {
        self.b.xf.remotes.borrow().get(data as usize).copied().flatten()
    }
}

impl HostClass for RemoteClass {
    fn name(&self) -> &str {
        "Object"
    }

    fn hooks(&self) -> u8 {
        hooks::INDEXED | hooks::NAMED | hooks::SYMBOLS
    }

    fn get(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<Value>> {
        let Some(rr) = self.rr(data) else {
            return Ok(None);
        };
        let b = &self.b;
        match rr.key.target {
            Target::Window => window_get(b, r, rr.key, key),
            Target::Location => location_get(b, r, rr.key, key),
            Target::Handle(_) => {
                let Some(k) = okey(r, key) else {
                    return Ok(None);
                };
                with_remote(b, r, rr.key, move |bb, rb, obj| {
                    let v = get_k(rb, obj, &k)?;
                    export(bb, rb, v)
                })
                .map(Some)
            }
        }
    }

    fn set(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>, value: Value) -> JsResult<bool> {
        let Some(rr) = self.rr(data) else {
            return Ok(false);
        };
        let b = &self.b;
        let name = match key {
            Key::Str(s) => Some(String::from(s)),
            _ => None,
        };
        match rr.key.target {
            Target::Window => {
                let other = hub::peek(b, rr.key.frame).map(|p| p.origin);
                let same = other.as_ref().is_some_and(|o| b.origin().same(o));
                if name.as_deref() == Some("location") {
                    let url = r.to_string(value)?;
                    navigate_other(b, r, rr.key.frame, &url, false)?;
                    return Ok(true);
                }
                if !same {
                    let o = other.unwrap_or(xorigin::Origin {
                        text: String::from("null"),
                        doc: 0,
                    });
                    return Err(security_error(b, r, &o));
                }
            }
            Target::Location => return location_set(b, r, rr.key, key, value),
            Target::Handle(_) => {}
        }
        let Some(k) = okey(r, key) else {
            return Ok(false);
        };
        let vx = export(b, r, value).map_err(|e| throw_xerr(r, e))?;
        with_remote(b, r, rr.key, move |bb, rb, obj| {
            let v = import(bb, rb, vx).map_err(|e| xerr_of(rb, &e))?;
            let res = match &k {
                OKey::Str(s) => rb.set(obj, s.as_str(), v),
                OKey::Index(i) => rb.set(obj, *i, v),
                OKey::Iterator => return Ok(Xv::Undef),
            };
            res.map_err(|e| xerr_of(rb, &e))?;
            Ok(Xv::Undef)
        })?;
        Ok(true)
    }

    fn has(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<bool>> {
        let Some(rr) = self.rr(data) else {
            return Ok(None);
        };
        let b = &self.b;
        if rr.key.target == Target::Window {
            let same = hub::peek(b, rr.key.frame).is_some_and(|p| b.origin().same(&p.origin));
            if !same {
                return Ok(match key {
                    Key::Str(s) => Some(xorigin::window_allows(s, Use::Get)),
                    Key::Index(_) => Some(true),
                    _ => None,
                });
            }
        }
        let Some(k) = okey(r, key) else {
            return Ok(None);
        };
        let v = with_remote(b, r, rr.key, move |_bb, rb, obj| {
            let has = match &k {
                OKey::Str(s) => rb.has(obj, s.as_str()),
                OKey::Index(i) => rb.has(obj, *i),
                OKey::Iterator => {
                    let s = rb.well_known_symbol(WellKnown::Iterator);
                    rb.has(obj, s)
                }
            }
            .map_err(|e| xerr_of(rb, &e))?;
            Ok(Xv::Bool(has))
        })?;
        Ok(Some(r.to_boolean(v)))
    }

    fn delete(&self, r: &mut Realm, _o: Value, data: u64, key: Key<'_>) -> JsResult<Option<bool>> {
        let Some(rr) = self.rr(data) else {
            return Ok(None);
        };
        let b = &self.b;
        if rr.key.target != Target::Handle(0) && !matches!(rr.key.target, Target::Handle(_)) {
            let same = hub::peek(b, rr.key.frame).is_some_and(|p| b.origin().same(&p.origin));
            if !same {
                return Ok(Some(false));
            }
        }
        let Some(k) = okey(r, key) else {
            return Ok(None);
        };
        let v = with_remote(b, r, rr.key, move |_bb, rb, obj| {
            let ok = match &k {
                OKey::Str(s) => rb.delete(obj, s.as_str()),
                OKey::Index(i) => rb.delete(obj, *i),
                OKey::Iterator => return Ok(Xv::Bool(false)),
            }
            .map_err(|e| xerr_of(rb, &e))?;
            Ok(Xv::Bool(ok))
        })?;
        Ok(Some(r.to_boolean(v)))
    }

    fn keys(&self, r: &mut Realm, _o: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        let Some(rr) = self.rr(data) else {
            return Ok(Vec::new());
        };
        let b = &self.b;
        if rr.key.target == Target::Window {
            let same = hub::peek(b, rr.key.frame).is_some_and(|p| b.origin().same(&p.origin));
            if !same {
                return Ok(Vec::new());
            }
        }
        // The keys travel as one string, newline separated (keys may not hold newlines here).
        let v = with_remote(b, r, rr.key, move |_bb, rb, obj| {
            let ks = rb.own_key_strings(obj).map_err(|e| xerr_of(rb, &e))?;
            let mut out = String::new();
            for (i, k) in ks.iter().take(10_000).enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str(&k.replace('\n', " "));
            }
            Ok(Xv::Str(out))
        })?;
        let s = r.get_string(v).unwrap_or_default();
        Ok(s.split('\n')
            .filter(|k| !k.is_empty())
            .map(|k| match k.parse::<u32>() {
                Ok(i) if format!("{i}") == k => OwnedKey::Index(i),
                _ => OwnedKey::Str(String::from(k)),
            })
            .collect())
    }

    fn finalize(&self, data: u64) {
        if let Ok(mut f) = self.b.xf.freed.try_borrow_mut() {
            f.push(data);
        }
    }
}

// ---- Windows ----

fn noop(r: &mut Realm, name: &str) -> Value {
    r.new_function(name, 0, |_r, _t, _a| Ok(Value::UNDEFINED))
}

/// Navigate frame `target` (another window): the request goes into the caller's queue, so the
/// Browser knows who asked (sandboxes decide what may navigate what).
pub fn navigate_other(b: &Rc<Bridge>, r: &mut Realm, target: u32, url: &str, replace: bool) -> JsResult<()> {
    let _ = r;
    let Some(p) = b.page() else { return Ok(()) };
    let abs = p.resolve(url.trim());
    if abs.trim_start().to_ascii_lowercase().starts_with("javascript:") {
        return Ok(());
    }
    b.request(PageRequest::NavigateOther {
        target: FrameId(target),
        url: abs,
        replace,
    });
    Ok(())
}

fn window_get(b: &Rc<Bridge>, r: &mut Realm, key: RemoteKey, k: Key<'_>) -> JsResult<Option<Value>> {
    let frame = key.frame;
    let Some(peek) = hub::peek(b, frame).filter(|p| p.uid == key.uid) else {
        // The frame's document is gone: a closed window.
        return match k {
            Key::Str("closed") => Ok(Some(Value::TRUE)),
            Key::Str("parent" | "top" | "self" | "window" | "frames") => Ok(Some(Value::NULL)),
            Key::Str("length") => Ok(Some(Value::int(0))),
            Key::Str(_) | Key::Index(_) => Ok(Some(Value::UNDEFINED)),
            Key::Value(_) => Ok(None),
        };
    };
    let hub = b.hub();
    let same = b.origin().same(&peek.origin);
    match k {
        Key::Index(i) => {
            let kids = hub.map(|h| h.children(frame)).unwrap_or_default();
            return match kids.get(i as usize) {
                Some(c) => window_of(b, r, *c).map(Some),
                None => Ok(Some(Value::UNDEFINED)),
            };
        }
        Key::Value(_) => {
            if !same {
                return Ok(None);
            }
        }
        Key::Str(name) => {
            match name {
                "postMessage" => {
                    let b2 = b.clone();
                    return Ok(Some(r.new_function("postMessage", 2, move |r, _t, a| {
                        super::postmsg::post_message(&b2, r, frame, a)
                    })));
                }
                "window" | "self" | "frames" => return window_of(b, r, frame).map(Some),
                "parent" => {
                    let p = peek.parent.unwrap_or(frame);
                    return window_of(b, r, p).map(Some);
                }
                "top" => return window_of(b, r, 0).map(Some),
                "length" => {
                    let n = hub.map(|h| h.children(frame).len()).unwrap_or(0);
                    return Ok(Some(Value::int(n as i32)));
                }
                "closed" => return Ok(Some(Value::FALSE)),
                "opener" => return Ok(Some(Value::NULL)),
                "focus" | "blur" | "close" => return Ok(Some(noop(r, name))),
                "location" => {
                    return proxy_for(
                        b,
                        r,
                        XRef {
                            key: RemoteKey {
                                frame,
                                uid: key.uid,
                                target: Target::Location,
                            },
                            callable: false,
                            counted: false,
                        },
                    )
                    .map(Some);
                }
                _ => {}
            }
            if !same {
                return Err(security_error(b, r, &peek.origin));
            }
            // Named child frames.
            if let Some(h) = &hub {
                for c in h.children(frame) {
                    if h.name_of(c).as_deref() == Some(name) {
                        return window_of(b, r, c).map(Some);
                    }
                }
            }
        }
    }
    let Some(ok) = okey(r, k) else {
        return Ok(None);
    };
    with_remote(b, r, key, move |bb, rb, obj| {
        let v = get_k(rb, obj, &ok)?;
        export(bb, rb, v)
    })
    .map(Some)
}

fn location_get(b: &Rc<Bridge>, r: &mut Realm, key: RemoteKey, k: Key<'_>) -> JsResult<Option<Value>> {
    let frame = key.frame;
    let Some(peek) = hub::peek(b, frame).filter(|p| p.uid == key.uid) else {
        return Ok(Some(Value::UNDEFINED));
    };
    let same = b.origin().same(&peek.origin);
    if let Key::Str(name) = k {
        match name {
            "replace" | "assign" if same || name == "replace" => {
                let b2 = b.clone();
                let replace = name == "replace";
                return Ok(Some(r.new_function(name, 1, move |r, _t, a| {
                    let u = super::bridge::arg_str(r, a, 0)?;
                    navigate_other(&b2, r, frame, &u, replace)?;
                    Ok(Value::UNDEFINED)
                })));
            }
            _ => {}
        }
        if !same {
            if !xorigin::location_allows(name, Use::Get) {
                return Err(security_error(b, r, &peek.origin));
            }
        }
    } else if !same {
        return Ok(None);
    }
    let Some(ok) = okey(r, k) else {
        return Ok(None);
    };
    let t = Target::Location;
    with_remote(b, r, RemoteKey { target: t, ..key }, move |bb, rb, obj| {
        let v = get_k(rb, obj, &ok)?;
        export(bb, rb, v)
    })
    .map(Some)
}

fn location_set(b: &Rc<Bridge>, r: &mut Realm, key: RemoteKey, k: Key<'_>, value: Value) -> JsResult<bool> {
    let frame = key.frame;
    let peek = hub::peek(b, frame);
    let same = peek.as_ref().is_some_and(|p| b.origin().same(&p.origin));
    let name = match k {
        Key::Str(s) => s,
        _ => return Ok(false),
    };
    if name == "href" {
        let url = r.to_string(value)?;
        navigate_other(b, r, frame, &url, false)?;
        return Ok(true);
    }
    if !same {
        let o = peek.map(|p| p.origin).unwrap_or(xorigin::Origin {
            text: String::from("null"),
            doc: 0,
        });
        return Err(security_error(b, r, &o));
    }
    // Same origin: the other parts (hash, search, pathname, ...) navigate it too, through the
    // other realm's own setters; the frame asks for the navigation in its own queue. That is
    // the one way past the caller's sandbox, so only the fragment is allowed: it never leaves
    // the document.
    if name == "hash" {
        let v = r.to_string(value)?;
        let ok = String::from(name);
        let vx = Xv::Str(v);
        with_remote(b, r, RemoteKey { target: Target::Location, ..key }, move |bb, rb, obj| {
            let v = import(bb, rb, vx).map_err(|e| xerr_of(rb, &e))?;
            rb.set(obj, ok.as_str(), v).map_err(|e| xerr_of(rb, &e))?;
            Ok(Xv::Undef)
        })?;
        return Ok(true);
    }
    Ok(true)
}

/// `window.frames`, `window.length`: the own window's children as seen from itself.
pub fn child_window(b: &Rc<Bridge>, r: &mut Realm, index: u32) -> JsResult<Value> {
    let me = b.frame_id();
    let Some(h) = b.hub() else {
        return Ok(Value::UNDEFINED);
    };
    match h.children(me).get(index as usize) {
        Some(c) => window_of(b, r, *c),
        None => Ok(Value::UNDEFINED),
    }
}

#[allow(dead_code)]
fn _unused(_: ErrorKind, _: Attrs) {
    let _ = arg;
}
