//! What the native functions of the bindings share: the page and the app's [`ScriptHost`]
//! (reachable only while a call from the app is in progress), and the bindings' own state
//! (wrappers, listeners, timers, the console, requests for the app).
//!
//! Every native closure holds an `Rc<Bridge>`. The page and the host are raw pointers set
//! by [`Bridge::enter`] for the length of one call from the app ([`super::Runtime`] makes
//! sure of that) and cleared after; outside that window they read as absent. A native takes
//! what it needs from them in a short scope and never keeps a reference across a call back
//! into script (which could reach another native wanting the same thing). The state is a
//! `RefCell` used the same way: borrowed briefly, never across script; a borrow that would
//! fail becomes a script error, never a panic.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, OnceCell, RefCell, RefMut};

use jsengine::{ClassId, ErrorKind, Exception, JsResult, Pinned, Realm, Value};

use super::events::Registry;
use super::loader::Loader;
use super::timers::Timers;
use crate::api::*;
use crate::dom::{NONE, NodeId};
use crate::page::Page;

/// The host classes (registered once per realm).
#[derive(Clone, Copy, Debug)]
pub struct Classes {
    /// Every DOM node (data: the node id).
    pub node: ClassId,
    /// Events (data: flag bits, see [`super::events_js`]).
    pub event: ClassId,
    /// Live and static node lists (data: an index into [`State::lists`]).
    pub list: ClassId,
    /// classList (data: the element).
    pub tokens: ClassId,
    /// element.style (data: the element).
    pub style: ClassId,
    /// element.dataset (data: the element).
    pub dataset: ClassId,
    /// localStorage (data 0) / sessionStorage (data 1).
    pub storage: ClassId,
    /// getComputedStyle results (data: the element).
    pub computed: ClassId,
}

/// The prototypes, pinned for the realm's life.
pub struct Protos {
    pub event_target: Pinned,
    pub node: Pinned,
    pub document: Pinned,
    pub element: Pinned,
    pub html_element: Pinned,
    pub character_data: Pinned,
    pub text: Pinned,
    pub comment: Pinned,
    pub doc_type: Pinned,
    pub fragment: Pinned,
    /// Per tag name: the HTMLxxxElement prototypes (input, select, form, a, ...).
    pub by_tag: Vec<(crate::atoms::Atom, Pinned)>,
    pub event: Pinned,
    pub ui_event: Pinned,
    pub mouse_event: Pinned,
    pub keyboard_event: Pinned,
    pub focus_event: Pinned,
    pub custom_event: Pinned,
    pub input_event: Pinned,
    pub submit_event: Pinned,
    pub node_list: Pinned,
    pub html_collection: Pinned,
    pub token_list: Pinned,
    pub style_decl: Pinned,
    pub string_map: Pinned,
    pub storage: Pinned,
    pub dom_rect: Pinned,
}

/// What a list object lists.
#[derive(Clone, Debug)]
pub enum ListKind {
    /// A snapshot (querySelectorAll).
    Static(Vec<NodeId>),
    /// node.childNodes.
    ChildNodes(NodeId),
    /// element.children.
    Children(NodeId),
    /// getElementsByTagName ("*" for all; lower-cased for HTML).
    ByTag(NodeId, String),
    /// getElementsByClassName (the class names).
    ByClass(NodeId, Vec<String>),
    /// getElementsByName (document-wide).
    ByName(String),
    /// document.forms.
    Forms,
    /// form.elements (the controls of a form element).
    FormElements(NodeId),
    /// select.options.
    Options(NodeId),
    /// document.images / document.links / document.scripts.
    Images,
    Links,
    Scripts,
}

pub struct List {
    pub kind: ListKind,
    /// The DOM generation the cache is from, and the cached nodes.
    pub seen: u64,
    pub cache: Vec<NodeId>,
    pub cached: bool,
}

/// An event handler (`onclick = ...` or an `onclick="..."` attribute).
pub enum Handler {
    /// From an attribute, compiled at first use.
    Source(String),
    Func(Pinned),
}

pub struct State {
    pub limits: ScriptLimits,
    pub loader: Loader,
    /// Handles index `cbs`.
    pub timers: Timers<u32>,
    pub listeners: Registry<u32>,
    /// Pinned callbacks and their extra arguments: (function or source string, args array).
    pub cbs: BTreeMap<u32, (Pinned, Option<Pinned>)>,
    pub next_cb: u32,
    pub wrappers: BTreeMap<NodeId, Pinned>,
    /// on* handlers by (target, event type).
    pub handlers: BTreeMap<(u32, String), Handler>,
    pub lists: BTreeMap<u32, List>,
    pub next_list: u32,
    pub console: Vec<ConsoleLine>,
    pub requests: Vec<PageRequest>,
    pub stats: ScriptStats,
    pub stopped: bool,
    /// Why (for the console).
    pub stop_reason: String,
    /// document.currentScript.
    pub current_script: NodeId,
    /// When the current call from the app started (slow-script check) and when the app was
    /// last asked about it.
    pub entry_start: f64,
    pub slow_asked: f64,
    /// Calls from the app in progress (0 outside), and nested dispatches.
    pub depth: u32,
    pub dispatch_depth: u32,
    /// A text control's value when it got the focus (for `change` on blur).
    pub focus_value: Option<(NodeId, String)>,
    /// External scripts a page inserted with `async = false`.
    pub ordered: Vec<NodeId>,
    /// DOMContentLoaded / load fired.
    pub content_loaded: bool,
    pub loaded: bool,
    /// The page's history.state (a JSON string).
    pub history_state: Option<String>,
    /// Bytes of console text kept.
    pub console_bytes: usize,
    /// Live list objects by what they list (so `el.children === el.children`).
    pub list_objs: BTreeMap<String, Pinned>,
    /// Per-element helper objects (classList, style, dataset) by (what, element).
    pub helpers: BTreeMap<(&'static str, NodeId), Pinned>,
    /// setCustomValidity messages.
    pub custom_validity: BTreeMap<NodeId, String>,
    /// A `<template>`'s content fragment.
    pub template_content: BTreeMap<NodeId, NodeId>,
    /// document.write: (the script writing, the last node it wrote).
    pub write_after: (NodeId, NodeId),
    /// Sibling positions for selector matching, by DOM generation.
    pub positions: Option<(u64, alloc::rc::Rc<(Vec<u32>, Vec<u32>)>)>,
    /// Network requests from scripts (script/net.rs).
    pub net: super::net::NetState,
}

/// Lines and bytes of console kept between [`super::Runtime::take_console`] calls.
pub const MAX_CONSOLE_LINES: usize = 2_000;
pub const MAX_CONSOLE_BYTES: usize = 1 << 20;

pub struct Bridge {
    page: Cell<*mut Page>,
    host: Cell<Option<*mut (dyn ScriptHost + 'static)>>,
    pub st: RefCell<State>,
    pub classes: OnceCell<Classes>,
    pub protos: OnceCell<Protos>,
    /// The byte buffers of the network API (script/netbytes.rs).
    pub bytes: OnceCell<super::netbytes::Bytes>,
}

impl Bridge {
    pub fn new(limits: ScriptLimits, loader: Loader) -> Bridge {
        Bridge {
            page: Cell::new(core::ptr::null_mut()),
            host: Cell::new(None),
            st: RefCell::new(State {
                limits,
                loader,
                timers: Timers::new(),
                listeners: Registry::new(),
                cbs: BTreeMap::new(),
                next_cb: 1,
                wrappers: BTreeMap::new(),
                handlers: BTreeMap::new(),
                lists: BTreeMap::new(),
                next_list: 1,
                console: Vec::new(),
                requests: Vec::new(),
                stats: ScriptStats::default(),
                stopped: false,
                stop_reason: String::new(),
                current_script: NONE,
                entry_start: 0.0,
                slow_asked: 0.0,
                depth: 0,
                dispatch_depth: 0,
                focus_value: None,
                ordered: Vec::new(),
                content_loaded: false,
                loaded: false,
                history_state: None,
                console_bytes: 0,
                list_objs: BTreeMap::new(),
                helpers: BTreeMap::new(),
                custom_validity: BTreeMap::new(),
                template_content: BTreeMap::new(),
                write_after: (NONE, NONE),
                positions: None,
                net: super::net::NetState::default(),
            }),
            classes: OnceCell::new(),
            protos: OnceCell::new(),
            bytes: OnceCell::new(),
        }
    }

    /// Make the page and the host reachable for the length of one call from the app. The
    /// returned guard clears them again (restoring an outer call's, for nesting).
    pub fn enter<'a>(&'a self, page: &mut Page, host: &mut dyn ScriptHost) -> Entered<'a> {
        let old = (self.page.get(), self.host.get());
        self.page.set(page as *mut Page);
        // SAFETY: the lifetime is erased only for the length of the guard: `Entered` puts
        // the old pointers back when it drops, before `page` and `host` can go away (the
        // guard borrows `self`, and the caller holds `page` and `host` borrowed while it lives).
        let h: *mut (dyn ScriptHost + '_) = host;
        let h: *mut (dyn ScriptHost + 'static) = unsafe { core::mem::transmute(h) };
        self.host.set(Some(h));
        Entered { b: self, old }
    }

    /// The page, while a call from the app is in progress. Don't keep the reference across
    /// a call into script.
    #[allow(clippy::mut_from_ref)]
    pub fn page(&self) -> Option<&mut Page> {
        let p = self.page.get();
        // SAFETY: set from a live `&mut Page` by `enter` and cleared when its guard drops;
        // natives use it in short scopes that don't overlap (see the module docs).
        (!p.is_null()).then(|| unsafe { &mut *p })
    }

    #[allow(clippy::mut_from_ref)]
    pub fn host(&self) -> Option<&mut dyn ScriptHost> {
        // SAFETY: as for `page`.
        self.host.get().map(|h| unsafe { &mut *h as &mut dyn ScriptHost })
    }

    /// The state, or a script error if it is borrowed already (re-entrancy we didn't plan).
    pub fn st(&self, r: &mut Realm) -> JsResult<RefMut<'_, State>> {
        self.st
            .try_borrow_mut()
            .map_err(|_| r.throw_error(ErrorKind::Error, "busy (re-entrant call)"))
    }

    pub fn classes(&self) -> Classes {
        *self.classes.get().expect("classes registered at init")
    }

    pub fn protos(&self) -> &Protos {
        self.protos.get().expect("prototypes made at init")
    }

    /// The page, or a script error outside a call from the app.
    #[allow(clippy::mut_from_ref)]
    pub fn page_or(&self, r: &mut Realm) -> JsResult<&mut Page> {
        match self.page() {
            Some(p) => Ok(p),
            None => Err(r.throw_error(ErrorKind::Error, "the page is gone")),
        }
    }

    /// A console line (bounded).
    pub fn console(&self, level: ConsoleLevel, text: String) {
        if let Ok(mut st) = self.st.try_borrow_mut() {
            push_console(&mut st, level, text);
        }
    }

    pub fn request(&self, r: PageRequest) {
        if let Ok(mut st) = self.st.try_borrow_mut()
            && st.requests.len() < 1000
        {
            st.requests.push(r);
        }
    }

    /// Keep a callback (a function, or a string for setTimeout's string form) and its
    /// arguments; the handle for the timer list or the listener registry.
    pub fn keep_cb(&self, r: &mut Realm, f: Value, args: Option<Value>) -> JsResult<u32> {
        let pf = r.pin(f);
        let pa = args.map(|a| r.pin(a));
        let mut st = self.st(r)?;
        let id = st.next_cb;
        st.next_cb = st.next_cb.wrapping_add(1).max(1);
        st.cbs.insert(id, (pf, pa));
        Ok(id)
    }

    pub fn drop_cb(&self, r: &mut Realm, id: u32) {
        let gone = self.st.try_borrow_mut().ok().and_then(|mut st| st.cbs.remove(&id));
        if let Some((f, a)) = gone {
            r.unpin(f);
            if let Some(a) = a {
                r.unpin(a);
            }
        }
    }

    /// A kept callback's function (and argument array).
    pub fn cb(&self, r: &mut Realm, id: u32) -> Option<(Value, Option<Value>)> {
        let st = self.st.try_borrow().ok()?;
        let (f, a) = st.cbs.get(&id)?;
        let fv = r.pinned(f);
        let av = a.as_ref().map(|a| r.pinned(a));
        Some((fv, av))
    }
}

pub fn push_console(st: &mut State, level: ConsoleLevel, mut text: String) {
    if st.console.len() >= MAX_CONSOLE_LINES || st.console_bytes > MAX_CONSOLE_BYTES {
        return;
    }
    if text.len() > 16 * 1024 {
        let mut e = 16 * 1024;
        while !text.is_char_boundary(e) {
            e -= 1;
        }
        text.truncate(e);
        text.push_str("...");
    }
    st.console_bytes += text.len();
    st.console.push(ConsoleLine { level, text });
}

/// Clears the page and host pointers when a call from the app ends.
pub struct Entered<'a> {
    b: &'a Bridge,
    old: (*mut Page, Option<*mut (dyn ScriptHost + 'static)>),
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        self.b.page.set(self.old.0);
        self.b.host.set(self.old.1);
    }
}

// ---- argument helpers ----------------------------------------------------------------------

pub fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// ToString of argument `i` (undefined: "undefined", as WebIDL DOMString does).
pub fn arg_str(r: &mut Realm, args: &[Value], i: usize) -> JsResult<String> {
    r.to_string(arg(args, i))
}

/// A DOMString argument that may be null ("[LegacyNullToEmptyString]" style: null is "").
pub fn arg_str_or_empty(r: &mut Realm, args: &[Value], i: usize) -> JsResult<String> {
    let v = arg(args, i);
    if v.is_null() {
        Ok(String::new())
    } else {
        r.to_string(v)
    }
}

pub fn arg_bool(r: &Realm, args: &[Value], i: usize) -> bool {
    r.to_boolean(arg(args, i))
}

pub fn arg_num(r: &mut Realm, args: &[Value], i: usize) -> JsResult<f64> {
    r.to_number(arg(args, i))
}

pub fn type_error(r: &mut Realm, msg: &str) -> Exception {
    r.throw_error(ErrorKind::TypeError, msg)
}

/// A DOMException-like error: an Error whose name is `name` (NotFoundError,
/// HierarchyRequestError, SyntaxError, InvalidCharacterError, QuotaExceededError, ...).
pub fn dom_error(r: &mut Realm, name: &str, msg: &str) -> Exception {
    let e = r.new_error(ErrorKind::Error, msg);
    let n = r.string(name);
    let _ = r.define(e, "name", n, jsengine::Attrs::METHOD);
    r.throw(e)
}

/// Rounding for f64 without std.
pub trait Rnd {
    fn rnd(self) -> f64;
}

impl Rnd for f64 {
    fn rnd(self) -> f64 {
        if !self.is_finite() || self.abs() >= 4.0e15 {
            return self;
        }
        let h = if self >= 0.0 { self + 0.5 } else { self - 0.5 };
        let i = h as i64 as f64;
        // -0.4 rounds to -0, as Math.round would not; fine for sizes.
        i
    }
}
