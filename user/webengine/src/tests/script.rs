//! The DOM bindings over fixture pages, driven from Rust through the engine's API (calls of
//! the bound natives, native listeners): what scripts can reach, events and their order,
//! the checkbox and form behaviour, timers, storage, the slow-script stop. The tests of
//! script text itself are in tests/scripts.rs.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use jsengine::{Realm, Value};

use crate::api::*;

/// A host for tests: a settable clock, a storage area, recorded dialogs.
pub struct Host {
    pub now: f64,
    pub local: Mem,
    pub session: Mem,
    pub alerts: Vec<String>,
    pub confirm_answer: bool,
    pub cookies: String,
    pub scroll: (i32, i32),
    /// After this many slow() calls, say stop.
    pub stop_after: Option<u32>,
    pub slow_calls: u32,
    /// The clock moves this much each time it is read (for runaway-script tests).
    pub tick: f64,
    pub clock: core::cell::Cell<f64>,
}

impl Host {
    pub fn new() -> Host {
        Host {
            now: 1000.0,
            local: Mem::default(),
            session: Mem::default(),
            alerts: Vec::new(),
            confirm_answer: true,
            cookies: String::new(),
            scroll: (0, 0),
            stop_after: None,
            slow_calls: 0,
            tick: 0.0,
            clock: core::cell::Cell::new(0.0),
        }
    }
}

#[derive(Default)]
pub struct Mem {
    pub items: Vec<(String, String)>,
}

impl Storage for Mem {
    fn len(&self) -> usize {
        self.items.len()
    }
    fn key(&self, i: usize) -> Option<String> {
        self.items.get(i).map(|x| x.0.clone())
    }
    fn get(&self, k: &str) -> Option<String> {
        self.items.iter().find(|x| x.0 == k).map(|x| x.1.clone())
    }
    fn set(&mut self, k: &str, v: &str) -> bool {
        if v.len() > 1000 {
            return false;
        }
        match self.items.iter_mut().find(|x| x.0 == k) {
            Some(x) => x.1 = String::from(v),
            None => self.items.push((String::from(k), String::from(v))),
        }
        true
    }
    fn remove(&mut self, k: &str) {
        self.items.retain(|x| x.0 != k);
    }
    fn clear(&mut self) {
        self.items.clear();
    }
}

impl ScriptHost for Host {
    fn measurer(&self) -> &dyn TextMeasurer {
        &ApproxMeasurer
    }
    fn viewport(&self) -> Viewport {
        Viewport::new(800, 600)
    }
    fn scroll(&self) -> (i32, i32) {
        self.scroll
    }
    fn now(&self) -> f64 {
        let c = self.clock.get() + self.tick;
        self.clock.set(c);
        self.now + c
    }
    fn time(&self) -> f64 {
        1_800_000_000_000.0 + self.now
    }
    fn cookies(&mut self) -> String {
        self.cookies.clone()
    }
    fn set_cookie(&mut self, line: &str) {
        let pair = line.split(';').next().unwrap_or("");
        if !self.cookies.is_empty() {
            self.cookies.push_str("; ");
        }
        self.cookies.push_str(pair);
    }
    fn storage(&mut self, session: bool) -> Option<&mut dyn Storage> {
        Some(if session {
            &mut self.session
        } else {
            &mut self.local
        })
    }
    fn alert(&mut self, msg: &str) {
        self.alerts.push(String::from(msg));
    }
    fn confirm(&mut self, msg: &str) -> bool {
        self.alerts.push(alloc::format!("confirm: {msg}"));
        self.confirm_answer
    }
    fn slow(&mut self, _ms: f64) -> bool {
        self.slow_calls += 1;
        match self.stop_after {
            Some(n) => self.slow_calls < n,
            None => true,
        }
    }
}

pub fn doc(html: &str) -> Document {
    let mut d = Document::parse_scripted(
        html.as_bytes(),
        "http://www.milk.test/page.html",
        Some("text/html"),
        Limits::default(),
        Some(ScriptLimits::default()),
    );
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    d
}

/// Run `f` inside an entry, with the realm, the global object and the document's wrapper.
pub fn with<R>(
    d: &mut Document,
    h: &mut Host,
    f: impl FnOnce(&mut Realm, Value, Value) -> R,
) -> R {
    let js = d.js.as_mut().expect("scripts on");
    js.test_entry(&mut d.page, h, |_b, r| {
        let g = r.global();
        let document = r.get(g, "document").unwrap();
        f(r, g, document)
    })
    .expect("not stopped")
}

pub fn call(r: &mut Realm, obj: Value, m: &str, args: &[Value]) -> Value {
    match r.call_method(obj, m, args) {
        Ok(v) => v,
        Err(e) => panic!("{m}: {}", r.report(&e)),
    }
}

pub fn s(r: &mut Realm, x: &str) -> Value {
    r.string(x)
}

pub fn get_str(r: &mut Realm, obj: Value, k: &str) -> String {
    let v = r.get(obj, k).unwrap();
    r.to_string(v).unwrap()
}

fn by_id(r: &mut Realm, document: Value, id: &str) -> Value {
    let a = s(r, id);
    call(r, document, "getElementById", &[a])
}

/// A native function that records each call's event type (and currentTarget's id) in `log`.
fn recorder(r: &mut Realm, log: &Rc<RefCell<Vec<String>>>, tag: &'static str) -> Value {
    let log = log.clone();
    r.new_function("rec", 1, move |r, this, args| {
        let e = args.first().copied().unwrap_or(Value::UNDEFINED);
        let ty = r.get(e, "type").unwrap();
        let ty = r.to_string(ty).unwrap();
        let id = r.get(this, "id").ok().and_then(|v| r.get_string(v)).unwrap_or_default();
        let ph = r.get(e, "eventPhase").unwrap().as_number().unwrap_or(-1.0);
        log.borrow_mut().push(alloc::format!("{tag}:{ty}@{id}/{ph}"));
        Ok(Value::UNDEFINED)
    })
}

const PAGE: &str = "<!DOCTYPE html><html><head><title>T</title></head><body>
<div id=outer class='box a'><p id=inner>Hello <b>world</b></p></div>
<ul id=list><li>one</li><li class=x>two</li></ul>
<form id=f action=/send><input id=name name=name required><input type=checkbox id=c name=c>
<label id=lab><input type=checkbox id=c2 name=c2> two</label>
<select id=s name=s><option value=a>A<option value=b>B</select><button id=go>Go</button></form>
<a id=link href=/other>link</a>
</body></html>";

#[test]
fn install_has_no_errors() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    with(&mut d, &mut h, |_r, _g, _d| {});
    let c = d.take_console();
    assert!(c.is_empty(), "{c:?}");
}

#[test]
fn document_and_tree() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let t0 = d.text();
    assert!(t0.contains("Hello world") && t0.contains("two"), "{t0}");
    with(&mut d, &mut h, |r, _g, document| {
        let outer = by_id(r, document, "outer");
        assert_eq!(get_str(r, outer, "tagName"), "DIV");
        assert_eq!(get_str(r, outer, "className"), "box a");
        let inner = by_id(r, document, "inner");
        assert_eq!(get_str(r, inner, "textContent"), "Hello world");
        // Same wrapper every time.
        let p = r.get(inner, "parentNode").unwrap();
        assert!(r.strict_equals(p, outer));
        let again = by_id(r, document, "outer");
        assert!(r.strict_equals(again, outer));
        // createElement / appendChild / innerHTML.
        let tag = s(r, "span");
        let sp = call(r, document, "createElement", &[tag]);
        let txt = s(r, "new text");
        let tn = call(r, document, "createTextNode", &[txt]);
        call(r, sp, "appendChild", &[tn]);
        call(r, outer, "appendChild", &[sp]);
        let last = r.get(outer, "lastChild").unwrap();
        assert!(r.strict_equals(last, sp));
        let html = get_str(r, outer, "innerHTML");
        assert!(html.ends_with("<span>new text</span>"), "{html}");
        let v = s(r, "<em>a</em><i>b</i>");
        r.set(inner, "innerHTML", v).unwrap();
        assert_eq!(get_str(r, inner, "innerHTML"), "<em>a</em><i>b</i>");
        let kids = r.get(inner, "children").unwrap();
        assert_eq!(r.get(kids, "length").unwrap().as_number(), Some(2.0));
        let k0 = r.get(kids, 0u32).unwrap();
        assert_eq!(get_str(r, k0, "tagName"), "EM");
        // Live: a third child shows up in the same list object.
        let tag = s(r, "u");
        let u = call(r, document, "createElement", &[tag]);
        call(r, inner, "appendChild", &[u]);
        assert_eq!(r.get(kids, "length").unwrap().as_number(), Some(3.0));
        let kids2 = r.get(inner, "children").unwrap();
        assert!(r.strict_equals(kids, kids2));
        // querySelector(All), matches, closest.
        let sel = s(r, "#list li.x");
        let li = call(r, document, "querySelector", &[sel]);
        assert_eq!(get_str(r, li, "textContent"), "two");
        let sel = s(r, "li");
        let all = call(r, document, "querySelectorAll", &[sel]);
        assert_eq!(r.get(all, "length").unwrap().as_number(), Some(2.0));
        let sel = s(r, "ul");
        let ul = call(r, li, "closest", &[sel]);
        assert_eq!(get_str(r, ul, "id"), "list");
        let bad = s(r, "li[");
        assert!(r.call_method(document, "querySelector", &[bad]).is_err());
        // classList.
        let cl = r.get(outer, "classList").unwrap();
        let c = s(r, "on");
        call(r, cl, "add", &[c]);
        let c = s(r, "a");
        call(r, cl, "remove", &[c]);
        assert_eq!(get_str(r, outer, "className"), "box on");
        let c = s(r, "box");
        let t = call(r, cl, "toggle", &[c]);
        assert_eq!(t.as_bool(), Some(false));
        let c = s(r, "on");
        assert_eq!(call(r, cl, "contains", &[c]).as_bool(), Some(true));
        // style and dataset.
        let st = r.get(outer, "style").unwrap();
        let red = s(r, "red");
        r.set(st, "backgroundColor", red).unwrap();
        let w = s(r, "10px");
        r.set(st, "width", w).unwrap();
        let attr = s(r, "style");
        let sa = call(r, outer, "getAttribute", &[attr]);
        assert_eq!(r.get_string(sa).unwrap(), "background-color: red; width: 10px;");
        assert_eq!(get_str(r, st, "width"), "10px");
        let ds = r.get(outer, "dataset").unwrap();
        let v = s(r, "7");
        r.set(ds, "itemCount", v).unwrap();
        let attr = s(r, "data-item-count");
        let a = call(r, outer, "getAttribute", &[attr]);
        assert_eq!(r.get_string(a).unwrap(), "7");
        // remove, contains.
        call(r, li, "remove", &[]);
        assert_eq!(call(r, ul, "contains", &[li]).as_bool(), Some(false));
        // Hierarchy errors.
        let e = r.call_method(inner, "appendChild", &[outer]).unwrap_err();
        assert!(r.report(&e).to_string().contains("ancestor"));
    });
    // The page sees the changes after layout.
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    let t = d.text();
    assert!(t.contains("new") && t.contains("ab"), "{t}");
    assert!(t.lines().any(|l| l == "one") && !t.lines().any(|l| l == "two"), "{t}");
}

#[test]
fn events_capture_bubble_once_and_cancel() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    with(&mut d, &mut h, |r, g, document| {
        let outer = by_id(r, document, "outer");
        let inner = by_id(r, document, "inner");
        let click = s(r, "click");
        let f1 = recorder(r, &log, "cap");
        let opts = r.new_object();
        r.set(opts, "capture", Value::TRUE).unwrap();
        call(r, outer, "addEventListener", &[click, f1, opts]);
        let f2 = recorder(r, &log, "bub");
        call(r, outer, "addEventListener", &[click, f2]);
        // The same listener twice is one listener.
        call(r, outer, "addEventListener", &[click, f2]);
        let f3 = recorder(r, &log, "tgt");
        let once = r.new_object();
        r.set(once, "once", Value::TRUE).unwrap();
        call(r, inner, "addEventListener", &[click, f3, once]);
        let f4 = recorder(r, &log, "win");
        call(r, g, "addEventListener", &[click, f4]);
        // A synthetic click from script.
        call(r, inner, "click", &[]);
        call(r, inner, "click", &[]);
        // dispatchEvent with a non-bubbling event: no bubble listener runs.
        let ctor = r.get(g, "Event").unwrap();
        let ty = s(r, "click");
        let e = r.construct(ctor, &[ty]).unwrap();
        let ok = call(r, inner, "dispatchEvent", &[e]);
        assert_eq!(ok.as_bool(), Some(true));
        // preventDefault on a cancelable event.
        let stopper = r.new_function("stop", 1, |r, _t, a| {
            r.call_method(a[0], "preventDefault", &[])?;
            r.call_method(a[0], "stopPropagation", &[])?;
            Ok(Value::UNDEFINED)
        });
        let ty = s(r, "custom");
        call(r, inner, "addEventListener", &[ty, stopper]);
        let f5 = recorder(r, &log, "never");
        call(r, outer, "addEventListener", &[ty, f5]);
        let init = r.new_object();
        r.set(init, "bubbles", Value::TRUE).unwrap();
        r.set(init, "cancelable", Value::TRUE).unwrap();
        let e = r.construct(ctor, &[ty, init]).unwrap();
        let ok = call(r, inner, "dispatchEvent", &[e]);
        assert_eq!(ok.as_bool(), Some(false));
        assert_eq!(r.get(e, "defaultPrevented").unwrap().as_bool(), Some(true));
        // removeEventListener.
        call(r, outer, "removeEventListener", &[click, f2]);
        call(r, inner, "click", &[]);
    });
    let l = log.borrow();
    assert_eq!(
        l.as_slice(),
        [
            "cap:click@outer/1",
            "tgt:click@inner/2",
            "bub:click@outer/3",
            "win:click@/3",
            // second click: the once listener is gone
            "cap:click@outer/1",
            "bub:click@outer/3",
            "win:click@/3",
            // non-bubbling dispatch: capture only
            "cap:click@outer/1",
            // after removeEventListener
            "cap:click@outer/1",
            "win:click@/3",
        ]
    );
}

#[test]
fn user_clicks_toggle_checkboxes_and_labels() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let ids: Vec<ControlId> = d.controls().iter().map(|c| c.id).collect();
    let c = ids[1];
    let r = d.controls()[c.0 as usize].rect;
    assert!(!r.is_empty());
    assert!(d.click(&mut h, r.x + 2, r.y + 2, (0, 0), Mods::default()));
    assert!(d.controls()[c.0 as usize].checked);
    // A listener that cancels: the toggle is undone.
    with(&mut d, &mut h, |r, _g, document| {
        let el = by_id(r, document, "c");
        let ty = s(r, "click");
        let f = r.new_function("no", 1, |r, _t, a| {
            r.call_method(a[0], "preventDefault", &[])?;
            Ok(Value::UNDEFINED)
        });
        call(r, el, "addEventListener", &[ty, f]);
    });
    assert!(!d.click(&mut h, r.x + 2, r.y + 2, (0, 0), Mods::default()));
    assert!(d.controls()[c.0 as usize].checked, "cancelled click leaves it checked");
    // The label's text toggles its checkbox.
    let lab = d.page.rect_of_id("lab").unwrap();
    let c2 = ids[2];
    assert!(!d.controls()[c2.0 as usize].checked);
    d.click(&mut h, lab.right() - 4, lab.y + 4, (0, 0), Mods::default());
    assert!(d.controls()[c2.0 as usize].checked);
}

#[test]
fn form_state_and_validation() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let name = d.controls()[0].id;
    // Required and empty: submitting is refused, the field gets the focus.
    assert!(d.submit_form(&mut h, name, None).is_none());
    assert_eq!(d.focused(), Some(name));
    let req = d.take_requests();
    assert!(
        req.contains(&PageRequest::Invalid(String::from("Please fill in this field."))),
        "{req:?}"
    );
    with(&mut d, &mut h, |r, _g, document| {
        let el = by_id(r, document, "name");
        let v = s(r, "Milk");
        r.set(el, "value", v).unwrap();
        assert_eq!(get_str(r, el, "value"), "Milk");
        let sel = by_id(r, document, "s");
        r.set(sel, "selectedIndex", Value::int(1)).unwrap();
        assert_eq!(get_str(r, sel, "value"), "b");
        let c = by_id(r, document, "c");
        r.set(c, "checked", Value::TRUE).unwrap();
        let f = by_id(r, document, "f");
        let els = r.get(f, "elements").unwrap();
        assert_eq!(r.get(els, "length").unwrap().as_number(), Some(5.0));
        let named = r.get(els, "name").unwrap();
        assert!(r.strict_equals(named, el));
    });
    assert_eq!(d.controls()[0].value, "Milk");
    let sub = d.submit_form(&mut h, name, None).unwrap();
    assert_eq!(sub.url, "http://www.milk.test/send?name=Milk&c=on&s=b");
    // A submit listener that cancels.
    with(&mut d, &mut h, |r, _g, document| {
        let f = by_id(r, document, "f");
        let ty = s(r, "submit");
        let no = r.new_function("no", 1, |r, _t, a| {
            r.call_method(a[0], "preventDefault", &[])?;
            Ok(Value::UNDEFINED)
        });
        call(r, f, "addEventListener", &[ty, no]);
    });
    assert!(d.submit_form(&mut h, name, None).is_none());
    // form.submit() from script skips the event.
    with(&mut d, &mut h, |r, _g, document| {
        let f = by_id(r, document, "f");
        call(r, f, "submit", &[]);
    });
    let req = d.take_requests();
    assert!(matches!(req.last(), Some(PageRequest::Submit(_))), "{req:?}");
}

#[test]
fn timers_frames_and_storage() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    with(&mut d, &mut h, |r, g, _document| {
        let l1 = log.clone();
        let f = r.new_function("t", 0, move |_r, _t, _a| {
            l1.borrow_mut().push("timeout");
            Ok(Value::UNDEFINED)
        });
        call(r, g, "setTimeout", &[f, Value::int(50)]);
        let l2 = log.clone();
        let iv = r.new_function("i", 0, move |_r, _t, _a| {
            l2.borrow_mut().push("interval");
            Ok(Value::UNDEFINED)
        });
        let id = call(r, g, "setInterval", &[iv, Value::int(20)]);
        r.set(g, "ivid", id).unwrap();
        let l3 = log.clone();
        let fr = r.new_function("f", 1, move |_r, _t, a| {
            assert!(a[0].is_number());
            l3.borrow_mut().push("frame");
            Ok(Value::UNDEFINED)
        });
        call(r, g, "requestAnimationFrame", &[fr]);
        // Storage.
        let ls = r.get(g, "localStorage").unwrap();
        let k = s(r, "k");
        let v = s(r, "v");
        call(r, ls, "setItem", &[k, v]);
        assert_eq!(get_str(r, ls, "k"), "v");
        let big = s(r, &"x".repeat(2000));
        assert!(r.call_method(ls, "setItem", &[k, big]).is_err(), "over the quota");
        // Cookies (the host keeps them).
        let doc = r.get(g, "document").unwrap();
        let c = s(r, "a=1; Path=/");
        r.set(doc, "cookie", c).unwrap();
        assert_eq!(get_str(r, doc, "cookie"), "a=1");
    });
    assert_eq!(h.local.get("k").as_deref(), Some("v"));
    assert!(d.wants_frame());
    d.frame(&mut h);
    assert!(!d.wants_frame());
    assert_eq!(d.next_wake(), Some(1020.0));
    h.now = 1025.0;
    d.run(&mut h);
    h.now = 1045.0;
    d.run(&mut h);
    h.now = 1060.0;
    d.run(&mut h);
    with(&mut d, &mut h, |r, g, _| {
        let id = r.get(g, "ivid").unwrap();
        call(r, g, "clearInterval", &[id]);
    });
    h.now = 2000.0;
    d.run(&mut h);
    assert_eq!(
        log.borrow().as_slice(),
        ["frame", "interval", "interval", "timeout"]
    );
    assert_eq!(d.next_wake(), None);
}

#[test]
fn location_history_and_dialogs() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    with(&mut d, &mut h, |r, g, _| {
        let loc = r.get(g, "location").unwrap();
        assert_eq!(get_str(r, loc, "hostname"), "www.milk.test");
        assert_eq!(get_str(r, loc, "pathname"), "/page.html");
        let hist = r.get(g, "history").unwrap();
        let u = s(r, "/next?x=1");
        call(r, hist, "pushState", &[Value::NULL, Value::NULL, u]);
        assert_eq!(get_str(r, loc, "href"), "http://www.milk.test/next?x=1");
        assert_eq!(get_str(r, loc, "search"), "?x=1");
        let u = s(r, "elsewhere.html");
        r.set(loc, "href", u).unwrap();
        let m = s(r, "hi");
        call(r, g, "alert", &[m]);
        let m = s(r, "sure?");
        assert_eq!(call(r, g, "confirm", &[m]).as_bool(), Some(true));
        let nav = r.get(g, "navigator").unwrap();
        assert!(get_str(r, nav, "userAgent").contains("milkOS"));
    });
    assert_eq!(h.alerts, ["hi", "confirm: sure?"]);
    let req = d.take_requests();
    assert_eq!(
        req,
        [
            PageRequest::SetAddress {
                url: String::from("http://www.milk.test/next?x=1"),
                replace: false
            },
            PageRequest::Navigate {
                url: String::from("http://www.milk.test/elsewhere.html"),
                replace: false
            },
        ]
    );
}

#[test]
fn focus_events_and_change_on_blur() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let log = Rc::new(RefCell::new(Vec::new()));
    with(&mut d, &mut h, |r, _g, document| {
        let el = by_id(r, document, "name");
        for t in ["focus", "blur", "change", "input", "keydown"] {
            let ty = s(r, t);
            let f = recorder(r, &log, "n");
            call(r, el, "addEventListener", &[ty, f]);
        }
    });
    let name = d.controls()[0].id;
    d.set_focus(&mut h, Some(name));
    assert!(d.key(
        &mut h,
        &KeyInput {
            key: String::from("a"),
            code: String::from("KeyA"),
            mods: Mods::default()
        },
        false
    ));
    d.set_value(name, "a");
    d.input(&mut h, name);
    d.set_focus(&mut h, None);
    assert_eq!(d.focused(), None);
    assert_eq!(
        log.borrow().as_slice(),
        [
            "n:focus@name/2",
            "n:keydown@name/2",
            "n:input@name/2",
            "n:change@name/2",
            "n:blur@name/2"
        ]
    );
}

#[test]
fn dom_nodes_stay_bounded_under_churn() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let before = d.page.dom.live();
    with(&mut d, &mut h, |r, _g, document| {
        let el = by_id(r, document, "outer");
        for i in 0..2000 {
            let v = s(r, &alloc::format!("<p>tick <b>{i}</b></p><ul><li>a<li>b</ul>"));
            r.set(el, "innerHTML", v).unwrap();
        }
    });
    let after = d.page.dom.live();
    assert!(after < before + 50, "{before} -> {after}");
    assert!(d.page.dom.len() < before + 100, "arena {}", d.page.dom.len());
}


/// Every bound function, getter and setter, called with hostile receivers and arguments:
/// errors are fine, panics are not, and the page must still lay out.
#[test]
fn bindings_survive_hostile_calls() {
    let mut d = doc(PAGE);
    let mut h = Host::new();
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut rnd = move |n: usize| -> usize {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    with(&mut d, &mut h, |r, g, document| {
        let object = r.get(g, "Object").unwrap_or(Value::UNDEFINED);
        if !object.is_object() {
            return; // the engine's stub has no builtins yet
        }
        let names_fn = r.get(object, "getOwnPropertyNames").unwrap_or(Value::UNDEFINED);
        let desc_fn = r.get(object, "getOwnPropertyDescriptor").unwrap_or(Value::UNDEFINED);
        if !r.is_callable(names_fn) || !r.is_callable(desc_fn) {
            return; // the engine's stub has no such builtins yet
        }
        // Receivers: nodes of each kind, collections, helpers, odd values.
        let mut thises: Vec<Value> = alloc::vec![document, g, Value::UNDEFINED, Value::NULL, Value::int(3)];
        for id in ["outer", "inner", "list", "f", "name", "c", "s", "go", "link", "lab"] {
            let e = by_id(r, document, id);
            thises.push(e);
        }
        let outer = thises[5];
        for k in ["childNodes", "children", "classList", "style", "dataset", "firstChild"] {
            if let Ok(v) = r.get(outer, k) {
                thises.push(v);
            }
        }
        let plain = r.new_object();
        thises.push(plain);
        let txt = s(r, "x");
        let tn = call(r, document, "createTextNode", &[txt]);
        thises.push(tn);
        let ctor = r.get(g, "Event").unwrap();
        let ty = s(r, "click");
        if let Ok(e) = r.construct(ctor, &[ty]) {
            thises.push(e);
        }
        // Arguments.
        let strs = ["", "div", "#outer", "<b>x</b><script>never()</script>", "a b", "click", "-1", "\u{0}", "beforeend", "999999999", "data-x", "style", "onclick", "javascript:void 0", "background-color"];
        let mut argv: Vec<Value> = Vec::new();
        for st in strs {
            argv.push(r.string(st));
        }
        argv.extend([Value::UNDEFINED, Value::NULL, Value::TRUE, Value::int(-1), Value::int(0), Value::int(7), Value::number(f64::NAN), Value::number(1e300)]);
        argv.extend(thises.iter().copied());
        let f = r.new_function("cb", 1, |_r, _t, _a| Ok(Value::UNDEFINED));
        argv.push(f);
        // The prototypes to walk.
        let mut protos: Vec<Value> = Vec::new();
        for c in ["Node", "Element", "HTMLElement", "Document", "EventTarget", "Event", "CharacterData", "Text", "DocumentFragment", "HTMLInputElement", "HTMLSelectElement", "HTMLFormElement", "HTMLOptionElement", "HTMLAnchorElement", "HTMLTextAreaElement", "HTMLButtonElement", "HTMLScriptElement", "HTMLTemplateElement", "NodeList", "HTMLCollection", "DOMTokenList", "CSSStyleDeclaration", "Storage", "Window", "MouseEvent", "KeyboardEvent", "CustomEvent"] {
            if let Ok(cv) = r.get(g, c)
                && cv.is_object()
                && let Ok(p) = r.get(cv, "prototype")
            {
                protos.push(p);
            }
        }
        for k in ["location", "history", "navigator", "console", "performance", "localStorage"] {
            if let Ok(v) = r.get(g, k) {
                protos.push(v);
            }
        }
        protos.push(g);
        let mut calls = 0;
        for &p in &protos {
            let Ok(names) = r.call(names_fn, object, &[p]) else { continue };
            let n = r.get(names, "length").ok().and_then(|v| v.as_number()).unwrap_or(0.0) as u32;
            for i in 0..n {
                let Ok(name) = r.get(names, i) else { continue };
                let Some(nm) = r.get_string(name) else { continue };
                if matches!(nm.as_str(), "alert" | "confirm" | "prompt" | "constructor") {
                    continue;
                }
                let Ok(desc) = r.call(desc_fn, object, &[p, name]) else { continue };
                for part in ["value", "get", "set"] {
                    let Ok(fv) = r.get(desc, part) else { continue };
                    if !r.is_callable(fv) {
                        continue;
                    }
                    for _ in 0..6 {
                        let this = thises[rnd(thises.len())];
                        let k = rnd(4);
                        let args: Vec<Value> = (0..k).map(|_| argv[rnd(argv.len())]).collect();
                        let _ = r.call(fv, this, &args);
                        calls += 1;
                    }
                }
            }
        }
        assert!(calls > 500, "{calls} calls");
    });
    // Still a page: it lays out and paints.
    d.layout(&Viewport::new(800, 600), &ApproxMeasurer);
    let _ = d.text();
    let _ = d.hit_test(10, 10, (0, 0));
}
