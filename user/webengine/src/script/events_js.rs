//! Events in script (DOM §2): EventTarget's methods, the Event family's constructors and
//! objects, dispatching along the propagation path, `on*` handlers (properties and
//! attributes), and the events the app's input turns into (clicks with the checkbox and
//! label behaviour, keys, focus, input, submit).
//!
//! An event is a host object whose data holds its flags ([`BUBBLES`] ...); `type`, `target`,
//! `currentTarget`, `eventPhase`, `timeStamp` and `isTrusted` are own read-only properties
//! the dispatcher redefines as it goes.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Attrs, JsResult, Realm, Value};

use super::bridge::{Bridge, Handler, arg, arg_bool, arg_str, dom_error, type_error};
use super::events::{self as ev, WINDOW};
use super::nodes::{self, wrap};
use super::{call_report, report};
use crate::api::{ControlKind, KeyInput, Mods, PageRequest};
use crate::atoms::tag;
use crate::dom::{NONE, NodeId};
use crate::page::Page;

pub const BUBBLES: u64 = 1;
pub const CANCELABLE: u64 = 2;
pub const CANCELED: u64 = 4;
pub const STOP: u64 = 8;
pub const STOP_NOW: u64 = 16;
pub const DISPATCHING: u64 = 32;
pub const TRUSTED: u64 = 64;
pub const PASSIVE: u64 = 128;
pub const COMPOSED: u64 = 256;

/// The listener handle that stands for "the `on*` handler of this target and type".
pub const HANDLER_CB: u32 = 0;

/// Nested dispatches (a listener dispatching, whose listener dispatches...) before more
/// are refused.
const MAX_DISPATCH_DEPTH: u32 = 32;

/// The event types with `on*` handlers (properties on elements, the document and window).
pub const HANDLER_TYPES: &[&str] = &[
    "abort",
    "animationend",
    "beforeinput",
    "blur",
    "cancel",
    "change",
    "click",
    "close",
    "contextmenu",
    "dblclick",
    "drag",
    "dragend",
    "dragenter",
    "dragleave",
    "dragover",
    "dragstart",
    "drop",
    "error",
    "focus",
    "focusin",
    "focusout",
    "hashchange",
    "input",
    "invalid",
    "keydown",
    "keypress",
    "keyup",
    "load",
    "message",
    "mousedown",
    "mouseenter",
    "mouseleave",
    "mousemove",
    "mouseout",
    "mouseover",
    "mouseup",
    "pagehide",
    "pageshow",
    "pointerdown",
    "pointerup",
    "popstate",
    "readystatechange",
    "reset",
    "resize",
    "scroll",
    "select",
    "storage",
    "submit",
    "toggle",
    "transitionend",
    "unload",
    "wheel",
];

fn ro() -> Attrs {
    Attrs::CONFIGURABLE | Attrs::ENUMERABLE
}

/// The script value of an event target id.
pub fn target_value(b: &Rc<Bridge>, r: &mut Realm, t: u32) -> Value {
    if t == WINDOW {
        r.global()
    } else if t == NONE {
        Value::NULL
    } else {
        wrap(b, r, t)
    }
}

/// The event target id of a script value (a node wrapper, or window).
pub fn target_of(b: &Bridge, r: &mut Realm, v: Value) -> Option<u32> {
    if let Some(n) = r.host_data(v, b.classes().node) {
        return Some(n as u32);
    }
    let g = r.global();
    if r.strict_equals(v, g) {
        return Some(WINDOW);
    }
    None
}

/// A new event object (not dispatched).
pub fn new_event(
    b: &Rc<Bridge>,
    r: &mut Realm,
    proto: &jsengine::Pinned,
    ty: &str,
    flags: u64,
) -> Value {
    let p = r.pinned(proto);
    let e = r.new_host_object(b.classes().event, flags, p);
    let t = r.string(ty);
    let _ = r.define(e, "type", t, ro());
    let _ = r.define(e, "target", Value::NULL, ro());
    let _ = r.define(e, "currentTarget", Value::NULL, ro());
    let _ = r.define(e, "srcElement", Value::NULL, ro());
    let _ = r.define(e, "eventPhase", Value::int(0), ro());
    let now = b.host().map_or(0.0, |h| h.now());
    let _ = r.define(e, "timeStamp", Value::number(now), ro());
    let _ = r.define(e, "isTrusted", Value::bool(flags & TRUSTED != 0), Attrs::ENUMERABLE);
    e
}

fn flags(b: &Bridge, r: &Realm, e: Value) -> u64 {
    r.host_data(e, b.classes().event).unwrap_or(0)
}

fn set_flags(r: &mut Realm, e: Value, f: u64) {
    r.set_host_data(e, f);
}

/// Dispatch `e` at `target`: the listeners along the path, capture then target then
/// bubble. True unless it was cancelled.
pub fn dispatch(b: &Rc<Bridge>, r: &mut Realm, target: u32, e: Value) -> bool {
    let Some(f0) = r.host_data(e, b.classes().event) else {
        return true;
    };
    if b.st.borrow().stopped {
        return f0 & CANCELED == 0;
    }
    {
        let mut st = b.st.borrow_mut();
        if st.dispatch_depth >= MAX_DISPATCH_DEPTH {
            return true;
        }
        st.dispatch_depth += 1;
    }
    let ty = r
        .get(e, "type")
        .ok()
        .and_then(|t| r.get_string(t))
        .unwrap_or_default();
    let path = match b.page() {
        Some(p) => ev::path(&p.dom, target),
        None => Vec::new(),
    };
    set_flags(r, e, (f0 | DISPATCHING) & !(STOP | STOP_NOW));
    let tv = target_value(b, r, target);
    let _ = r.define(e, "target", tv, ro());
    let _ = r.define(e, "srcElement", tv, ro());
    let bubbles = f0 & BUBBLES != 0;
    'stops: for (node, phase) in ev::plan(&path, bubbles) {
        if flags(b, r, e) & STOP != 0 {
            break;
        }
        let cur = target_value(b, r, node);
        let _ = r.define(e, "currentTarget", cur, ro());
        let _ = r.define(e, "eventPhase", Value::int(phase as i32), ro());
        let passes: &[bool] = if phase == ev::AT_TARGET {
            &[false, true]
        } else {
            &[false]
        };
        for &second in passes {
            let list = b.st.borrow().listeners.snapshot(node, &ty);
            for l in list {
                if !ev::runs_at(&l, phase, second) {
                    continue;
                }
                if !b.st.borrow().listeners.still_there(node, &ty, l.id) {
                    continue;
                }
                if l.once {
                    b.st.borrow_mut().listeners.remove_id(node, &ty, l.id);
                }
                let fl = flags(b, r, e);
                if l.passive {
                    set_flags(r, e, fl | PASSIVE);
                }
                b.st.borrow_mut().stats.callbacks += 1;
                if l.cb == HANDLER_CB {
                    call_handler(b, r, node, &ty, cur, e);
                } else if let Some((f, _)) = b.cb(r, l.cb) {
                    if r.is_callable(f) {
                        call_report(b, r, f, cur, &[e]);
                    } else if f.is_object() {
                        // An object with handleEvent.
                        match r.get(f, "handleEvent") {
                            Ok(h) if r.is_callable(h) => {
                                call_report(b, r, h, f, &[e]);
                            }
                            Ok(_) => {}
                            Err(x) => report(b, r, &x, ""),
                        }
                    }
                }
                if l.once {
                    b.drop_cb(r, l.cb);
                }
                let fl = flags(b, r, e);
                set_flags(r, e, fl & !PASSIVE);
                if fl & STOP_NOW != 0 || b.st.borrow().stopped {
                    break 'stops;
                }
            }
        }
    }
    let _ = r.define(e, "currentTarget", Value::NULL, ro());
    let _ = r.define(e, "eventPhase", Value::int(0), ro());
    let fl = flags(b, r, e);
    set_flags(r, e, fl & !(DISPATCHING | STOP | STOP_NOW));
    b.st.borrow_mut().dispatch_depth -= 1;
    fl & CANCELED == 0
}

/// The `on*` handler of (target, type), compiled from its attribute the first time.
fn call_handler(b: &Rc<Bridge>, r: &mut Realm, target: u32, ty: &str, this: Value, e: Value) {
    let key = (target, String::from(ty));
    let src = match b.st.borrow().handlers.get(&key) {
        Some(Handler::Source(s)) => Some(s.clone()),
        Some(Handler::Func(_)) => None,
        None => return,
    };
    if let Some(src) = src {
        let file = b
            .page()
            .map(|p| String::from(p.page_url()))
            .unwrap_or_default();
        match r.compile_function(&["event"], &src, &file) {
            Ok(f) => {
                let p = r.pin(f);
                if let Some(old) = b.st.borrow_mut().handlers.insert(key.clone(), Handler::Func(p))
                    && let Handler::Func(o) = old
                {
                    r.unpin(o);
                }
            }
            Err(x) => {
                report(b, r, &x, &format!("on{ty}"));
                b.st.borrow_mut().handlers.remove(&key);
                return;
            }
        }
    }
    let f = match b.st.borrow().handlers.get(&key) {
        Some(Handler::Func(p)) => r.pinned(p),
        _ => return,
    };
    if !r.is_callable(f) {
        return;
    }
    if let Some(ret) = call_report(b, r, f, this, &[e])
        && ret.as_bool() == Some(false)
        && ty != "error"
    {
        let fl = flags(b, r, e);
        if fl & CANCELABLE != 0 {
            set_flags(r, e, fl | CANCELED);
        }
    }
}

/// Set (or with None clear) the `on*` handler of (target, type), keeping its place among
/// the listeners from the first time it was set.
pub fn set_handler(b: &Bridge, r: &mut Realm, target: u32, ty: &str, h: Option<Handler>) {
    let key = (target, String::from(ty));
    let old = b.st.borrow_mut().handlers.remove(&key);
    if let Some(Handler::Func(p)) = old {
        r.unpin(p);
    }
    let mut st = b.st.borrow_mut();
    match h {
        Some(h) => {
            st.handlers.insert(key, h);
            st.listeners
                .add(target, ty, HANDLER_CB, false, false, false);
        }
        None => {
            st.listeners.remove(target, ty, &HANDLER_CB, false);
        }
    }
}

pub fn get_handler(b: &Bridge, r: &mut Realm, target: u32, ty: &str) -> Value {
    match b.st.borrow().handlers.get(&(target, String::from(ty))) {
        Some(Handler::Func(p)) => r.pinned(p),
        // Not compiled yet: compile now so the property reads as a function.
        Some(Handler::Source(_)) => Value::NULL,
        None => Value::NULL,
    }
}

/// Fire a plain Event (trusted) at `target`.
pub fn fire_simple(
    b: &Rc<Bridge>,
    r: &mut Realm,
    target: u32,
    ty: &str,
    bubbles: bool,
    cancelable: bool,
) -> bool {
    let mut f = TRUSTED;
    if bubbles {
        f |= BUBBLES;
    }
    if cancelable {
        f |= CANCELABLE;
    }
    let e = new_event(b, r, &b.protos().event, ty, f);
    dispatch(b, r, target, e)
}

fn define_mods(r: &mut Realm, e: Value, m: Mods) {
    let _ = r.define(e, "altKey", Value::bool(m.alt), Attrs::ENUMERABLE);
    let _ = r.define(e, "ctrlKey", Value::bool(m.ctrl), Attrs::ENUMERABLE);
    let _ = r.define(e, "shiftKey", Value::bool(m.shift), Attrs::ENUMERABLE);
    let _ = r.define(e, "metaKey", Value::bool(m.meta), Attrs::ENUMERABLE);
}

/// The legacy keyCode of a key.
fn key_code(key: &str) -> i32 {
    match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Shift" => 16,
        "Control" => 17,
        "Alt" => 18,
        "Escape" => 27,
        " " => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Delete" => 46,
        k => {
            let mut it = k.chars();
            match (it.next(), it.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => c.to_ascii_uppercase() as i32,
                (Some(c), None) if c.is_ascii_digit() => c as i32,
                (Some(c), None) => match c {
                    ';' | ':' => 186,
                    '=' | '+' => 187,
                    ',' | '<' => 188,
                    '-' | '_' => 189,
                    '.' | '>' => 190,
                    '/' | '?' => 191,
                    '`' | '~' => 192,
                    '[' | '{' => 219,
                    '\\' | '|' => 220,
                    ']' | '}' => 221,
                    '\'' | '"' => 222,
                    _ => 0,
                },
                _ => 0,
            }
        }
    }
}

pub fn fire_key(b: &Rc<Bridge>, r: &mut Realm, target: NodeId, ty: &str, k: &KeyInput) -> bool {
    let e = new_event(
        b,
        r,
        &b.protos().keyboard_event,
        ty,
        TRUSTED | BUBBLES | CANCELABLE | COMPOSED,
    );
    let key = r.string(&k.key);
    let _ = r.define(e, "key", key, Attrs::ENUMERABLE);
    let code = r.string(&k.code);
    let _ = r.define(e, "code", code, Attrs::ENUMERABLE);
    let kc = key_code(&k.key);
    let char_code = if ty == "keypress" {
        k.key.chars().next().map_or(0, |c| c as i32)
    } else {
        0
    };
    let which = if ty == "keypress" { char_code } else { kc };
    let _ = r.define(e, "keyCode", Value::int(which), Attrs::ENUMERABLE);
    let _ = r.define(e, "which", Value::int(which), Attrs::ENUMERABLE);
    let _ = r.define(e, "charCode", Value::int(char_code), Attrs::ENUMERABLE);
    let _ = r.define(e, "repeat", Value::FALSE, Attrs::ENUMERABLE);
    let _ = r.define(e, "isComposing", Value::FALSE, Attrs::ENUMERABLE);
    let _ = r.define(e, "location", Value::int(0), Attrs::ENUMERABLE);
    define_mods(r, e, k.mods);
    dispatch(b, r, target, e)
}

pub fn fire_input(b: &Rc<Bridge>, r: &mut Realm, node: NodeId, ty: &str) {
    let e = new_event(
        b,
        r,
        &b.protos().input_event,
        ty,
        TRUSTED | BUBBLES | COMPOSED,
    );
    let it = r.string("insertText");
    let _ = r.define(e, "inputType", it, Attrs::ENUMERABLE);
    let _ = r.define(e, "data", Value::NULL, Attrs::ENUMERABLE);
    let _ = r.define(e, "isComposing", Value::FALSE, Attrs::ENUMERABLE);
    dispatch(b, r, node, e);
}

pub fn fire_submit(b: &Rc<Bridge>, r: &mut Realm, form: NodeId, submitter: NodeId) -> bool {
    let e = new_event(
        b,
        r,
        &b.protos().submit_event,
        "submit",
        TRUSTED | BUBBLES | CANCELABLE,
    );
    let s = if submitter == NONE {
        Value::NULL
    } else {
        wrap(b, r, submitter)
    };
    let _ = r.define(e, "submitter", s, Attrs::ENUMERABLE);
    dispatch(b, r, form, e)
}

fn fire_mouse(
    b: &Rc<Bridge>,
    r: &mut Realm,
    target: NodeId,
    ty: &str,
    pos: (i32, i32),
    mods: Mods,
    trusted: bool,
) -> bool {
    let mut f = BUBBLES | CANCELABLE | COMPOSED;
    if trusted {
        f |= TRUSTED;
    }
    let e = new_event(b, r, &b.protos().mouse_event, ty, f);
    let (sx, sy) = b.host().map_or((0, 0), |h| h.scroll());
    let (ox, oy) = b
        .page()
        .and_then(|p| p.node_rect(target))
        .map_or((0, 0), |rc| (pos.0 + sx - rc.x, pos.1 + sy - rc.y));
    for (k, v) in [
        ("clientX", pos.0),
        ("clientY", pos.1),
        ("x", pos.0),
        ("y", pos.1),
        ("pageX", pos.0 + sx),
        ("pageY", pos.1 + sy),
        ("screenX", pos.0),
        ("screenY", pos.1),
        ("offsetX", ox),
        ("offsetY", oy),
        ("movementX", 0),
        ("movementY", 0),
        ("button", 0),
        ("buttons", if ty == "mousedown" { 1 } else { 0 }),
        ("detail", 1),
    ] {
        let _ = r.define(e, k, Value::int(v), Attrs::ENUMERABLE);
    }
    let _ = r.define(e, "relatedTarget", Value::NULL, Attrs::ENUMERABLE);
    define_mods(r, e, mods);
    dispatch(b, r, target, e)
}

/// The checkbox or radio button a click at `target` works on: the input itself, or the
/// control of a `<label>` it is in.
pub fn toggle_target(page: &Page, target: NodeId) -> Option<NodeId> {
    let is_check = |n: NodeId| {
        page.dom.element(n).is_some_and(|e| {
            e.is(tag::INPUT)
                && e.attr("type").is_some_and(|t| {
                    let t = t.trim();
                    t.eq_ignore_ascii_case("checkbox") || t.eq_ignore_ascii_case("radio")
                })
        })
    };
    if is_check(target) {
        return Some(target);
    }
    let mut n = target;
    while n != NONE {
        if page.dom.is_html(n, tag::LABEL) {
            let c = nodes::label_control(page, n)?;
            return is_check(c).then_some(c);
        }
        // Not through interactive content (a link or button in the label is its own).
        if page.dom.is_html(n, tag::A) || page.dom.is_html(n, tag::BUTTON) {
            return None;
        }
        n = page.dom.parent(n);
    }
    None
}

/// A click (user or script) on a checkbox / radio button `c`: toggle first, dispatch at
/// `target`, put it back if cancelled, else `input` and `change`. True unless cancelled.
fn check_dance(
    b: &Rc<Bridge>,
    r: &mut Realm,
    c: NodeId,
    target: NodeId,
    pos: (i32, i32),
    mods: Mods,
    trusted: bool,
) -> bool {
    let before: Vec<(crate::api::ControlId, bool)> = match b.page() {
        Some(p) => {
            p.sync_forms();
            let Some(id) = p.control_of_node(c) else {
                return fire_mouse(b, r, target, "click", pos, mods, trusted);
            };
            let ctl = &p.controls()[id.0 as usize];
            if ctl.disabled {
                return false;
            }
            // Everything a radio can change: its group.
            let (name, form, kind) = (ctl.name.clone(), ctl.form, ctl.kind);
            let snap: Vec<_> = p
                .controls()
                .iter()
                .filter(|o| {
                    o.id == id
                        || (kind == ControlKind::Radio
                            && o.kind == ControlKind::Radio
                            && o.name == name
                            && o.form == form)
                })
                .map(|o| (o.id, o.checked))
                .collect();
            let on = kind != ControlKind::Checkbox || !ctl.checked;
            p.forms_mut().set_checked_as(id, on, true);
            snap
        }
        None => return true,
    };
    let changed = match b.page() {
        Some(p) => before
            .iter()
            .any(|&(id, was)| p.controls().get(id.0 as usize).is_some_and(|c| c.checked != was)),
        None => false,
    };
    let ok = fire_mouse(b, r, target, "click", pos, mods, trusted);
    if !ok {
        if let Some(p) = b.page() {
            p.sync_forms();
            for (id, was) in before {
                if let Some(cc) = p.forms_mut().controls.get_mut(id.0 as usize) {
                    cc.checked = was;
                }
            }
        }
        return false;
    }
    if changed {
        fire_input(b, r, c, "input");
        fire_simple(b, r, c, "change", true, false);
    }
    true
}

/// The app's click at `target`: mousedown, mouseup, click (with the checkbox and label
/// behaviour). True unless the click was cancelled.
pub fn user_click(b: &Rc<Bridge>, r: &mut Realm, target: NodeId, pos: (i32, i32), m: Mods) -> bool {
    fire_mouse(b, r, target, "mousedown", pos, m, true);
    fire_mouse(b, r, target, "mouseup", pos, m, true);
    let tog = b.page().and_then(|p| toggle_target(p, target));
    match tog {
        Some(c) if c == target => check_dance(b, r, c, target, pos, m, true),
        Some(c) => {
            // A label: its click, then (unless cancelled) a click on its control.
            if !fire_mouse(b, r, target, "click", pos, m, true) {
                return false;
            }
            check_dance(b, r, c, c, pos, m, false);
            true
        }
        None => fire_mouse(b, r, target, "click", pos, m, true),
    }
}

/// `element.click()` from script: a click with the element's activation behaviour (links
/// navigate, submit buttons submit, checkboxes toggle, labels pass it on).
pub fn script_click(b: &Rc<Bridge>, r: &mut Realm, n: NodeId) -> JsResult<()> {
    let (tog, kind) = match b.page() {
        Some(p) => {
            p.sync_forms();
            let kind = p
                .control_of_node(n)
                .map(|id| (p.controls()[id.0 as usize].kind, p.controls()[id.0 as usize].disabled));
            (toggle_target(p, n), kind)
        }
        None => return Ok(()),
    };
    if kind.is_some_and(|k| k.1) {
        return Ok(()); // disabled controls don't get clicks
    }
    if let Some(c) = tog {
        if c == n {
            check_dance(b, r, c, n, (0, 0), Mods::default(), false);
        } else if fire_mouse(b, r, n, "click", (0, 0), Mods::default(), false) {
            check_dance(b, r, c, c, (0, 0), Mods::default(), false);
        }
        return Ok(());
    }
    if !fire_mouse(b, r, n, "click", (0, 0), Mods::default(), false) {
        return Ok(());
    }
    activate(b, r, n, kind.map(|k| k.0));
    Ok(())
}

/// What a click on `n` does when not cancelled (script clicks only: the app does its own).
fn activate(b: &Rc<Bridge>, r: &mut Realm, n: NodeId, kind: Option<ControlKind>) {
    let Some(p) = b.page() else { return };
    // The nearest link or control.
    let mut a = n;
    while a != NONE {
        if p.dom.is_html(a, tag::A) && p.dom.element(a).is_some_and(|e| e.has_attr("href")) {
            let e = p.dom.element(a).unwrap();
            let href = e.attr("href").unwrap_or("");
            if href.trim_start().to_ascii_lowercase().starts_with("javascript:") {
                let code = String::from(&href.trim_start()[11..]);
                let code = crate::url::percent_decode(&code);
                if let Err(x) = r.eval(&code, "javascript:") {
                    report(b, r, &x, "");
                }
                return;
            }
            let url = p.resolve(href);
            let blank = e.attr("target").is_some_and(|t| t == "_blank");
            b.request(if blank {
                PageRequest::Open(url)
            } else {
                PageRequest::Navigate {
                    url,
                    replace: false,
                }
            });
            return;
        }
        if p.control_of_node(a).is_some() {
            break;
        }
        a = p.dom.parent(a);
    }
    match kind {
        Some(ControlKind::Submit) | Some(ControlKind::ImageSubmit) => {
            if let Some(id) = p.control_of_node(n) {
                super::forms_js::submit_from(b, r, id, true);
            }
        }
        Some(ControlKind::Reset) => {
            if let Some(id) = p.control_of_node(n)
                && let Some(form) = p.form_node_of(id)
                && fire_simple(b, r, form, "reset", true, true)
                && let Some(p) = b.page()
            {
                p.reset(id);
            }
        }
        _ => {}
    }
}

/// Move the focus (blur/focusout on the old element, change for a text field left with a
/// new value, focus/focusin on the new one).
pub fn move_focus(b: &Rc<Bridge>, r: &mut Realm, to: NodeId) {
    let from = match b.page() {
        Some(p) => p.focus,
        None => return,
    };
    if from == to {
        return;
    }
    if from != NONE && b.page().is_some_and(|p| p.dom.is_connected(from)) {
        let fv = b.st.borrow_mut().focus_value.take();
        if let Some((n, v)) = fv
            && n == from
            && nodes::control_value(b, from).is_some_and(|now| now != v)
        {
            fire_simple(b, r, from, "change", true, false);
        }
        if let Some(p) = b.page() {
            p.focus = NONE;
        }
        focus_event(b, r, from, "blur", false, to);
        focus_event(b, r, from, "focusout", true, to);
    }
    if to == NONE {
        if let Some(p) = b.page() {
            p.focus = NONE;
        }
        return;
    }
    if let Some(p) = b.page() {
        p.focus = to;
    }
    if let Some(v) = nodes::control_value(b, to) {
        b.st.borrow_mut().focus_value = Some((to, v));
    }
    focus_event(b, r, to, "focus", false, from);
    focus_event(b, r, to, "focusin", true, from);
}

fn focus_event(b: &Rc<Bridge>, r: &mut Realm, n: NodeId, ty: &str, bubbles: bool, related: NodeId) {
    let mut f = TRUSTED | COMPOSED;
    if bubbles {
        f |= BUBBLES;
    }
    let e = new_event(b, r, &b.protos().focus_event, ty, f);
    let rel = if related == NONE || !b.page().is_some_and(|p| p.dom.is_connected(related)) {
        Value::NULL
    } else {
        wrap(b, r, related)
    };
    let _ = r.define(e, "relatedTarget", rel, Attrs::ENUMERABLE);
    dispatch(b, r, n, e);
}

// ---- the script-facing side ----------------------------------------------------------------

/// The `options` of addEventListener: (capture, once, passive).
fn listener_options(r: &mut Realm, v: Value) -> JsResult<(bool, bool, bool)> {
    if v.is_object() {
        let c = r.get(v, "capture")?;
        let o = r.get(v, "once")?;
        let p = r.get(v, "passive")?;
        Ok((r.to_boolean(c), r.to_boolean(o), r.to_boolean(p)))
    } else {
        Ok((r.to_boolean(v), false, false))
    }
}

fn this_target(b: &Bridge, r: &mut Realm, this: Value) -> JsResult<u32> {
    // Calls with no receiver (`addEventListener(...)` at the top level) mean window.
    if this.is_undefined() {
        return Ok(WINDOW);
    }
    target_of(b, r, this).ok_or_else(|| type_error(r, "Illegal invocation"))
}

pub fn add_event_listener(b: &Rc<Bridge>, r: &mut Realm, this: Value, args: &[Value]) -> JsResult<Value> {
    let t = this_target(b, r, this)?;
    let ty = arg_str(r, args, 0)?;
    let cb = arg(args, 1);
    if !cb.is_object() {
        return Ok(Value::UNDEFINED); // null / undefined callbacks are ignored
    }
    let (capture, once, passive) = listener_options(r, arg(args, 2))?;
    // Already there (same callback, same capture)?
    let list = b.st(r)?.listeners.snapshot(t, &ty);
    for l in list {
        if l.capture == capture
            && l.cb != HANDLER_CB
            && let Some((f, _)) = b.cb(r, l.cb)
            && r.strict_equals(f, cb)
        {
            return Ok(Value::UNDEFINED);
        }
    }
    let h = b.keep_cb(r, cb, None)?;
    let added = b.st(r)?.listeners.add(t, &ty, h, capture, once, passive);
    if !added {
        b.drop_cb(r, h);
    }
    Ok(Value::UNDEFINED)
}

pub fn remove_event_listener(
    b: &Rc<Bridge>,
    r: &mut Realm,
    this: Value,
    args: &[Value],
) -> JsResult<Value> {
    let t = this_target(b, r, this)?;
    let ty = arg_str(r, args, 0)?;
    let cb = arg(args, 1);
    let (capture, _, _) = listener_options(r, arg(args, 2))?;
    let list = b.st(r)?.listeners.snapshot(t, &ty);
    for l in list {
        if l.capture == capture
            && l.cb != HANDLER_CB
            && let Some((f, _)) = b.cb(r, l.cb)
            && r.strict_equals(f, cb)
        {
            b.st(r)?.listeners.remove_id(t, &ty, l.id);
            b.drop_cb(r, l.cb);
            break;
        }
    }
    Ok(Value::UNDEFINED)
}

pub fn dispatch_event(b: &Rc<Bridge>, r: &mut Realm, this: Value, args: &[Value]) -> JsResult<Value> {
    let t = this_target(b, r, this)?;
    let e = arg(args, 0);
    let Some(f) = r.host_data(e, b.classes().event) else {
        return Err(type_error(r, "dispatchEvent: not an Event"));
    };
    if f & DISPATCHING != 0 {
        return Err(dom_error(r, "InvalidStateError", "The event is already being dispatched"));
    }
    // Script-dispatched events aren't trusted.
    r.set_host_data(e, f & !TRUSTED);
    let _ = r.define(e, "isTrusted", Value::FALSE, Attrs::ENUMERABLE);
    Ok(Value::bool(dispatch(b, r, t, e)))
}

/// `new Event(type, init)` and the subclasses: the flags from the init dictionary.
fn init_flags(r: &mut Realm, init: Value) -> JsResult<u64> {
    let mut f = 0;
    if init.is_object() {
        let bu = r.get(init, "bubbles")?;
        if r.to_boolean(bu) {
            f |= BUBBLES;
        }
        let c = r.get(init, "cancelable")?;
        if r.to_boolean(c) {
            f |= CANCELABLE;
        }
        let c = r.get(init, "composed")?;
        if r.to_boolean(c) {
            f |= COMPOSED;
        }
    }
    Ok(f)
}

/// Copy these init members onto the event (with defaults).
fn copy_init(r: &mut Realm, e: Value, init: Value, keys: &[(&str, Value)]) -> JsResult<()> {
    for &(k, def) in keys {
        let v = if init.is_object() {
            let v = r.get(init, k)?;
            if v.is_undefined() { def } else { v }
        } else {
            def
        };
        r.define(e, k, v, Attrs::ENUMERABLE)?;
    }
    Ok(())
}

/// Install EventTarget and the Event family on the global object.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let g = r.global();
    let p = b.protos();
    // EventTarget.prototype.
    let et = r.pinned(&p.event_target);
    let b2 = b.clone();
    r.define_method(et, "addEventListener", 2, move |r, t, a| add_event_listener(&b2, r, t, a))?;
    let b2 = b.clone();
    r.define_method(et, "removeEventListener", 2, move |r, t, a| {
        remove_event_listener(&b2, r, t, a)
    })?;
    let b2 = b.clone();
    r.define_method(et, "dispatchEvent", 1, move |r, t, a| dispatch_event(&b2, r, t, a))?;
    let etc = r.new_class("EventTarget", 0, et, None, None);
    r.define(g, "EventTarget", etc, Attrs::METHOD)?;

    // Event.prototype.
    let ep = r.pinned(&p.event);
    let cls = b.classes().event;
    let flag_getter = |bit: u64| {
        move |r: &mut Realm, this: Value, _: &[Value]| -> JsResult<Value> {
            Ok(Value::bool(r.host_data(this, cls).unwrap_or(0) & bit != 0))
        }
    };
    r.define_getter(ep, "bubbles", flag_getter(BUBBLES), None)?;
    r.define_getter(ep, "cancelable", flag_getter(CANCELABLE), None)?;
    r.define_getter(ep, "composed", flag_getter(COMPOSED), None)?;
    r.define_getter(ep, "defaultPrevented", flag_getter(CANCELED), None)?;
    r.define_getter(
        ep,
        "returnValue",
        move |r, this, _| Ok(Value::bool(r.host_data(this, cls).unwrap_or(0) & CANCELED == 0)),
        Some(alloc::boxed::Box::new(move |r: &mut Realm, this: Value, a: &[Value]| {
            let f = r.host_data(this, cls).unwrap_or(0);
            if !arg_bool(r, a, 0) && f & CANCELABLE != 0 && f & PASSIVE == 0 {
                r.set_host_data(this, f | CANCELED);
            }
            Ok(Value::UNDEFINED)
        })),
    )?;
    r.define_getter(
        ep,
        "cancelBubble",
        move |r, this, _| Ok(Value::bool(r.host_data(this, cls).unwrap_or(0) & STOP != 0)),
        Some(alloc::boxed::Box::new(move |r: &mut Realm, this: Value, a: &[Value]| {
            let f = r.host_data(this, cls).unwrap_or(0);
            if arg_bool(r, a, 0) {
                r.set_host_data(this, f | STOP);
            }
            Ok(Value::UNDEFINED)
        })),
    )?;
    r.define_method(ep, "preventDefault", 0, move |r, this, _| {
        if let Some(f) = r.host_data(this, cls)
            && f & CANCELABLE != 0
            && f & PASSIVE == 0
        {
            r.set_host_data(this, f | CANCELED);
        }
        Ok(Value::UNDEFINED)
    })?;
    r.define_method(ep, "stopPropagation", 0, move |r, this, _| {
        if let Some(f) = r.host_data(this, cls) {
            r.set_host_data(this, f | STOP);
        }
        Ok(Value::UNDEFINED)
    })?;
    r.define_method(ep, "stopImmediatePropagation", 0, move |r, this, _| {
        if let Some(f) = r.host_data(this, cls) {
            r.set_host_data(this, f | STOP | STOP_NOW);
        }
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(ep, "composedPath", 0, move |r, this, _| {
        let t = r.get(this, "currentTarget")?;
        if t.is_null() {
            return Ok(r.new_array(&[]));
        }
        let Some(id) = target_of(&b2, r, t) else {
            return Ok(r.new_array(&[]));
        };
        let path = match b2.page() {
            Some(p) => ev::path(&p.dom, id),
            None => Vec::new(),
        };
        let vals: Vec<Value> = path.iter().map(|&n| target_value(&b2, r, n)).collect();
        Ok(r.new_array(&vals))
    })?;
    r.define_method(ep, "initEvent", 3, move |r, this, a| {
        let Some(f) = r.host_data(this, cls) else {
            return Ok(Value::UNDEFINED);
        };
        if f & DISPATCHING != 0 {
            return Ok(Value::UNDEFINED);
        }
        let ty = arg_str(r, a, 0)?;
        let mut nf = f & TRUSTED;
        if arg_bool(r, a, 1) {
            nf |= BUBBLES;
        }
        if arg_bool(r, a, 2) {
            nf |= CANCELABLE;
        }
        r.set_host_data(this, nf);
        let t = r.string(&ty);
        r.define(this, "type", t, ro())?;
        Ok(Value::UNDEFINED)
    })?;
    for (i, name) in ["NONE", "CAPTURING_PHASE", "AT_TARGET", "BUBBLING_PHASE"].iter().enumerate() {
        r.define(ep, *name, Value::int(i as i32), Attrs::ENUMERABLE)?;
    }

    // The constructors.
    struct Ctor {
        name: &'static str,
        proto: fn(&super::bridge::Protos) -> &jsengine::Pinned,
        parent: Option<&'static str>,
        init: &'static [(&'static str, InitDefault)],
    }
    #[derive(Clone, Copy)]
    enum InitDefault {
        Null,
        False,
        Zero,
        Empty,
    }
    const MOUSE: &[(&str, InitDefault)] = &[
        ("clientX", InitDefault::Zero),
        ("clientY", InitDefault::Zero),
        ("screenX", InitDefault::Zero),
        ("screenY", InitDefault::Zero),
        ("pageX", InitDefault::Zero),
        ("pageY", InitDefault::Zero),
        ("offsetX", InitDefault::Zero),
        ("offsetY", InitDefault::Zero),
        ("button", InitDefault::Zero),
        ("buttons", InitDefault::Zero),
        ("detail", InitDefault::Zero),
        ("relatedTarget", InitDefault::Null),
        ("altKey", InitDefault::False),
        ("ctrlKey", InitDefault::False),
        ("shiftKey", InitDefault::False),
        ("metaKey", InitDefault::False),
    ];
    const KEY: &[(&str, InitDefault)] = &[
        ("key", InitDefault::Empty),
        ("code", InitDefault::Empty),
        ("keyCode", InitDefault::Zero),
        ("which", InitDefault::Zero),
        ("charCode", InitDefault::Zero),
        ("location", InitDefault::Zero),
        ("repeat", InitDefault::False),
        ("isComposing", InitDefault::False),
        ("altKey", InitDefault::False),
        ("ctrlKey", InitDefault::False),
        ("shiftKey", InitDefault::False),
        ("metaKey", InitDefault::False),
    ];
    let ctors: [Ctor; 9] = [
        Ctor { name: "Event", proto: |p| &p.event, parent: None, init: &[] },
        Ctor { name: "UIEvent", proto: |p| &p.ui_event, parent: Some("Event"), init: &[("detail", InitDefault::Zero), ("view", InitDefault::Null)] },
        Ctor { name: "MouseEvent", proto: |p| &p.mouse_event, parent: Some("UIEvent"), init: MOUSE },
        Ctor { name: "PointerEvent", proto: |p| &p.mouse_event, parent: Some("MouseEvent"), init: MOUSE },
        Ctor { name: "KeyboardEvent", proto: |p| &p.keyboard_event, parent: Some("UIEvent"), init: KEY },
        Ctor { name: "FocusEvent", proto: |p| &p.focus_event, parent: Some("UIEvent"), init: &[("relatedTarget", InitDefault::Null)] },
        Ctor { name: "InputEvent", proto: |p| &p.input_event, parent: Some("UIEvent"), init: &[("data", InitDefault::Null), ("inputType", InitDefault::Empty), ("isComposing", InitDefault::False)] },
        Ctor { name: "CustomEvent", proto: |p| &p.custom_event, parent: Some("Event"), init: &[("detail", InitDefault::Null)] },
        Ctor { name: "SubmitEvent", proto: |p| &p.submit_event, parent: Some("Event"), init: &[("submitter", InitDefault::Null)] },
    ];
    for c in ctors {
        let pv = r.pinned((c.proto)(p));
        let parent = match c.parent {
            Some(n) => Some(r.get(g, n)?),
            None => None,
        };
        let b2 = b.clone();
        let protof = c.proto;
        let init = c.init;
        let name = c.name;
        let ctor = r.new_class(
            name,
            1,
            pv,
            parent,
            Some(alloc::boxed::Box::new(move |r: &mut Realm, _this: Value, a: &[Value]| {
                if a.is_empty() {
                    return Err(type_error(r, &format!("{name}: 1 argument required")));
                }
                let ty = arg_str(r, a, 0)?;
                let f = init_flags(r, arg(a, 1))?;
                let e = new_event(&b2, r, protof(b2.protos()), &ty, f);
                // A subclass made with `class X extends Event` keeps its own prototype.
                if let Some(nt) = r.new_target() {
                    let np = r.get(nt, "prototype")?;
                    if np.is_object() {
                        r.set_prototype(e, np);
                    }
                }
                let vals: Vec<(&str, Value)> = init
                    .iter()
                    .map(|&(k, d)| {
                        (
                            k,
                            match d {
                                InitDefault::Null => Value::NULL,
                                InitDefault::False => Value::FALSE,
                                InitDefault::Zero => Value::int(0),
                                InitDefault::Empty => Value::UNDEFINED,
                            },
                        )
                    })
                    .collect();
                copy_init(r, e, arg(a, 1), &vals)?;
                // Empty-string defaults (key, code, inputType).
                for &(k, d) in init {
                    if matches!(d, InitDefault::Empty) {
                        let v = r.get(e, k)?;
                        if v.is_undefined() {
                            let s = r.string("");
                            r.define(e, k, s, Attrs::ENUMERABLE)?;
                        }
                    }
                }
                Ok(e)
            })),
        );
        r.define(g, name, ctor, Attrs::METHOD)?;
    }
    // CustomEvent.prototype.initCustomEvent (old pages).
    let cp = r.pinned(&p.custom_event);
    r.define_method(cp, "initCustomEvent", 4, move |r, this, a| {
        let Some(f) = r.host_data(this, cls) else {
            return Ok(Value::UNDEFINED);
        };
        let ty = arg_str(r, a, 0)?;
        let mut nf = f & TRUSTED;
        if arg_bool(r, a, 1) {
            nf |= BUBBLES;
        }
        if arg_bool(r, a, 2) {
            nf |= CANCELABLE;
        }
        r.set_host_data(this, nf);
        let t = r.string(&ty);
        r.define(this, "type", t, ro())?;
        r.define(this, "detail", arg(a, 3), Attrs::ENUMERABLE)?;
        Ok(Value::UNDEFINED)
    })?;
    // document.createEvent("Event" / "CustomEvent" / "MouseEvents" ...).
    Ok(())
}

/// document.createEvent(name): an uninitialized event of that interface.
pub fn create_event(b: &Rc<Bridge>, r: &mut Realm, name: &str) -> JsResult<Value> {
    let p = b.protos();
    let proto = match name.to_ascii_lowercase().as_str() {
        "event" | "events" | "htmlevents" => &p.event,
        "customevent" => &p.custom_event,
        "mouseevent" | "mouseevents" => &p.mouse_event,
        "keyboardevent" | "keyevents" => &p.keyboard_event,
        "uievent" | "uievents" => &p.ui_event,
        "focusevent" => &p.focus_event,
        _ => {
            return Err(dom_error(
                r,
                "NotSupportedError",
                &format!("createEvent: unknown interface {name}"),
            ));
        }
    };
    Ok(new_event(b, r, proto, "", 0))
}
