//! The DOM in script (DOM §4, HTML's element interfaces): one wrapper object per node
//! (made when script first sees the node, kept for the page's life, so `a.parentNode ===
//! b` holds), with Node, Element, HTMLElement, Document, CharacterData and DocumentFragment
//! on its prototype chain, live collections, and the tree changes (which also run inserted
//! scripts and pick up `on*` attributes).
//!
//! Nodes script never saw that drop out of the tree (the old children replaced by
//! `innerHTML` or `textContent`) are given back to the DOM, so pages that rebuild content
//! over and over stay in bounded memory.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Attrs, JsResult, Realm, Value};

use super::bridge::{Rnd, 
    Bridge, Handler, ListKind, arg, arg_bool, arg_str, arg_str_or_empty, dom_error, type_error,
};
use super::events_js::{self, HANDLER_TYPES};
use crate::api::{PageRequest, Rect};
use crate::atoms::{Atom, tag};
use crate::dom::{Attr, NONE, NodeData, NodeId, Ns};
use crate::page::Page;

pub const ELEMENT_NODE: i32 = 1;
pub const TEXT_NODE: i32 = 3;
pub const COMMENT_NODE: i32 = 8;
pub const DOCUMENT_NODE: i32 = 9;
pub const DOCUMENT_TYPE_NODE: i32 = 10;
pub const DOCUMENT_FRAGMENT_NODE: i32 = 11;

/// The element name that stands for a DocumentFragment (it can't be a parsed tag name).
pub const FRAGMENT: &str = "#document-fragment";

// ---- wrappers ----------------------------------------------------------------------------

/// The script object for node `n` (the same one every time).
pub fn wrap(b: &Rc<Bridge>, r: &mut Realm, n: NodeId) -> Value {
    if n == NONE {
        return Value::NULL;
    }
    if let Ok(st) = b.st.try_borrow()
        && let Some(p) = st.wrappers.get(&n)
    {
        return r.pinned(p);
    }
    let proto = proto_for(b, r, n);
    let v = r.new_host_object(b.classes().node, n as u64, proto);
    let p = r.pin(v);
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.wrappers.insert(n, p);
    }
    v
}

fn proto_for(b: &Rc<Bridge>, r: &mut Realm, n: NodeId) -> Value {
    let p = b.protos();
    let Some(page) = b.page() else {
        return r.pinned(&p.node);
    };
    let pin = match &page.dom.node(n).data {
        NodeData::Document => &p.document,
        NodeData::Doctype(_) => &p.doc_type,
        NodeData::Text(_) => &p.text,
        NodeData::Comment(_) => &p.comment,
        NodeData::Element(e) => {
            if page.dom.atoms.name(e.name) == FRAGMENT {
                &p.fragment
            } else if e.ns != Ns::Html {
                &p.element
            } else {
                p.by_tag
                    .iter()
                    .find(|(t, _)| *t == e.name)
                    .map(|(_, p)| p)
                    .unwrap_or(&p.html_element)
            }
        }
    };
    r.pinned(pin)
}

/// The node a script value wraps.
pub fn node_of(b: &Bridge, r: &Realm, v: Value) -> Option<NodeId> {
    let n = r.host_data(v, b.classes().node)? as NodeId;
    let ok = b.page().is_some_and(|p| (n as usize) < p.dom.len());
    ok.then_some(n)
}

pub fn this_node(b: &Bridge, r: &mut Realm, this: Value) -> JsResult<NodeId> {
    node_of(b, r, this).ok_or_else(|| type_error(r, "Illegal invocation"))
}

fn this_elem(b: &Bridge, r: &mut Realm, this: Value) -> JsResult<NodeId> {
    let n = this_node(b, r, this)?;
    if b.page().is_some_and(|p| p.dom.element(n).is_some()) {
        Ok(n)
    } else {
        Err(type_error(r, "Illegal invocation (not an element)"))
    }
}

fn arg_node(b: &Bridge, r: &mut Realm, args: &[Value], i: usize) -> JsResult<NodeId> {
    let v = arg(args, i);
    node_of(b, r, v).ok_or_else(|| type_error(r, "Argument is not a Node"))
}

fn opt_node(b: &Bridge, r: &Realm, v: Value) -> Option<NodeId> {
    if v.is_nullish() {
        return None;
    }
    node_of(b, r, v)
}

pub fn is_fragment(page: &Page, n: NodeId) -> bool {
    page.dom
        .element(n)
        .is_some_and(|e| page.dom.atoms.name(e.name) == FRAGMENT)
}

fn node_type(page: &Page, n: NodeId) -> i32 {
    match &page.dom.node(n).data {
        NodeData::Document => DOCUMENT_NODE,
        NodeData::Doctype(_) => DOCUMENT_TYPE_NODE,
        NodeData::Text(_) => TEXT_NODE,
        NodeData::Comment(_) => COMMENT_NODE,
        NodeData::Element(_) if is_fragment(page, n) => DOCUMENT_FRAGMENT_NODE,
        NodeData::Element(_) => ELEMENT_NODE,
    }
}

fn tag_name(page: &Page, n: NodeId) -> String {
    match page.dom.element(n) {
        Some(e) => {
            let s = page.dom.atoms.name(e.name);
            if e.ns == Ns::Html {
                s.to_ascii_uppercase()
            } else {
                String::from(s)
            }
        }
        None => String::new(),
    }
}

// ---- small helpers -----------------------------------------------------------------------

/// The control a `<label>` is for, as an element.
pub fn label_control(page: &Page, label: NodeId) -> Option<NodeId> {
    let id = page.label_control(label)?;
    page.control_node(id)
}

/// A text control's current value (for the change-on-blur check).
pub fn control_value(b: &Bridge, n: NodeId) -> Option<String> {
    let p = b.page()?;
    p.sync_forms();
    let id = p.control_of_node(n)?;
    let c = &p.controls()[id.0 as usize];
    crate::api::ControlKind::Text
        .eq(&c.kind)
        .then_some(())
        .or_else(|| {
            matches!(
                c.kind,
                crate::api::ControlKind::Password | crate::api::ControlKind::TextArea
            )
            .then_some(())
        })
        .map(|_| c.value.clone())
}

/// Lay the page out now if a script's changes need it (for sizes and positions).
pub fn ensure_layout(b: &Bridge) {
    if let (Some(p), Some(h)) = (b.page(), b.host())
        && p.changed()
    {
        let vp = h.viewport();
        p.layout(&vp, h.measurer());
    }
}

/// A node's box in CSS pixels, relative to the viewport (for getBoundingClientRect).
fn client_rect(b: &Bridge, n: NodeId) -> (f64, f64, f64, f64) {
    ensure_layout(b);
    let (Some(p), Some(h)) = (b.page(), b.host()) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let zoom = h.viewport().zoom.clamp(25, 500) as f64 / 100.0;
    let (sx, sy) = h.scroll();
    let rc = p.node_rect(n).unwrap_or(Rect::default());
    if rc == Rect::default() {
        return (0.0, 0.0, 0.0, 0.0);
    }
    (
        (rc.x - sx) as f64 / zoom,
        (rc.y - sy) as f64 / zoom,
        rc.w as f64 / zoom,
        rc.h as f64 / zoom,
    )
}

/// A node's box in CSS pixels, document space.
fn doc_rect(b: &Bridge, n: NodeId) -> (f64, f64, f64, f64) {
    ensure_layout(b);
    let (Some(p), Some(h)) = (b.page(), b.host()) else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let zoom = h.viewport().zoom.clamp(25, 500) as f64 / 100.0;
    let rc = p.node_rect(n).unwrap_or(Rect::default());
    (
        rc.x as f64 / zoom,
        rc.y as f64 / zoom,
        rc.w as f64 / zoom,
        rc.h as f64 / zoom,
    )
}

fn dom_rect(b: &Rc<Bridge>, r: &mut Realm, x: f64, y: f64, w: f64, h: f64) -> JsResult<Value> {
    let p = r.pinned(&b.protos().dom_rect);
    let o = r.new_object_with_proto(p);
    for (k, v) in [
        ("x", x),
        ("y", y),
        ("width", w),
        ("height", h),
        ("top", y),
        ("left", x),
        ("right", x + w),
        ("bottom", y + h),
    ] {
        r.define(o, k, Value::number(v), Attrs::DEFAULT)?;
    }
    Ok(o)
}

/// Which `on*` attributes count.
fn handler_attr(name: &str) -> Option<&'static str> {
    let t = name.strip_prefix("on")?;
    HANDLER_TYPES.iter().find(|&&h| h == t).copied()
}

/// The event target an element's `on*` attribute goes to: `<body>` and `<frameset>` hand
/// window's events (load, unload, ...) to window.
fn handler_target(page: &Page, n: NodeId, ty: &str) -> u32 {
    if (page.dom.is_html(n, tag::BODY) || page.dom.is_html(n, tag::FRAMESET))
        && matches!(
            ty,
            "load" | "unload" | "pagehide" | "pageshow" | "popstate" | "hashchange" | "resize"
                | "storage" | "message" | "error" | "blur" | "focus" | "scroll"
        )
    {
        super::events::WINDOW
    } else {
        n
    }
}

/// Pick up the `on*` attributes in the subtree at `n` (when parsed, inserted, or set).
pub fn register_handlers(b: &Bridge, r: &mut Realm, n: NodeId) {
    let mut found: Vec<(u32, &'static str, String)> = Vec::new();
    if let Some(p) = b.page() {
        let mut c = n;
        let mut k = 0;
        while c != NONE {
            if let Some(e) = p.dom.element(c) {
                for a in &e.attrs {
                    if let Some(ty) = handler_attr(&a.name) {
                        found.push((handler_target(p, c, ty), ty, String::from(&*a.value)));
                    }
                }
            }
            k += 1;
            if k > 1_000_000 {
                break;
            }
            c = p.dom.next_in(c, n);
        }
    }
    for (t, ty, src) in found {
        let exists = b
            .st
            .borrow()
            .handlers
            .contains_key(&(t, String::from(ty)));
        if !exists {
            events_js::set_handler(b, r, t, ty, Some(Handler::Source(src)));
        }
    }
}

/// After nodes went into the tree: their `on*` attributes, and the scripts among them that
/// should run (inline ones now, external ones once fetched).
pub fn inserted(b: &Rc<Bridge>, r: &mut Realm, tops: &[NodeId]) {
    let mut scripts = Vec::new();
    let mut links = Vec::new();
    for &t in tops {
        register_handlers(b, r, t);
        if let Some(p) = b.page()
            && p.dom.is_connected(t)
        {
            let mut c = t;
            while c != NONE {
                if p.dom.is_html(c, tag::SCRIPT) {
                    scripts.push(c);
                } else if p.dom.is_html(c, tag::LINK) {
                    links.push(c);
                }
                c = p.dom.next_in(c, t);
            }
        }
    }
    for s in scripts {
        prepare_inserted(b, r, s);
    }
    // <link rel=modulepreload> starts its fetch when it goes in.
    if !links.is_empty() {
        if let Some(p) = b.page() {
            let base = String::from(p.base_url());
            let resolve = move |h: &str| crate::url::resolve(&base, h);
            let mut st = b.st.borrow_mut();
            for l in links {
                st.loader.prepare_preload(&p.dom, l, &resolve);
            }
        }
        super::modrt::start_work(b, r);
    }
}

/// HTML's "prepare the script element" for a script a page put in the tree.
pub fn prepare_inserted(b: &Rc<Bridge>, r: &mut Realm, s: NodeId) {
    let Some(p) = b.page() else { return };
    let base = String::from(p.base_url());
    let resolve = move |h: &str| crate::url::resolve(&base, h);
    let ordered = b.st.borrow().ordered.contains(&s);
    let idx = b
        .st
        .borrow_mut()
        .loader
        .prepare(&p.dom, s, false, ordered, &resolve);
    // Import maps and module scripts the page inserted: compile and ask for fetches now
    // (they run when their imports are in, at the end of the task).
    if !b.st.borrow().loader.work.is_empty() {
        super::modrt::start_work(b, r);
    }
    if let Some(i) = idx {
        // (An inline script written behind an external one waits for it.)
        let inline = {
            let st = b.st.borrow();
            let sc = &st.loader.scripts[i];
            matches!(sc.body, super::loader::Body::Ready(_)) && sc.when != super::loader::When::Written
        };
        if inline {
            super::run_script(b, r, i, false);
        }
    }
}

/// Give back detached subtrees script never saw (nothing refers to them any more).
pub fn free_unseen(b: &Bridge, roots: &[NodeId]) {
    let Some(p) = b.page() else { return };
    let Ok(mut st) = b.st.try_borrow_mut() else {
        return;
    };
    for &root in roots {
        if p.dom.parent(root) != NONE || root == 0 {
            continue;
        }
        let mut all = Vec::new();
        let mut c = root;
        let mut seen = false;
        while c != NONE {
            if st.wrappers.contains_key(&c) || c == p.focus || c == st.current_script {
                seen = true;
                break;
            }
            all.push(c);
            c = p.dom.next_in(c, root);
        }
        if seen {
            continue;
        }
        for &n in &all {
            st.loader.forget(n);
            let keys: Vec<(u32, String)> = st
                .handlers
                .keys()
                .filter(|(t, _)| *t == n)
                .cloned()
                .collect();
            for k in keys {
                st.handlers.remove(&k);
                st.listeners
                    .remove(k.0, &k.1, &events_js::HANDLER_CB, false);
            }
            st.ordered.retain(|&o| o != n);
        }
        p.dom.free_subtree(root);
    }
}

// ---- tree changes --------------------------------------------------------------------------

fn can_have_children(page: &Page, n: NodeId) -> bool {
    matches!(
        page.dom.node(n).data,
        NodeData::Document | NodeData::Element(_)
    )
}

/// DOM's "pre-insert" validity checks.
fn check_insert(b: &Bridge, r: &mut Realm, parent: NodeId, node: NodeId, before: NodeId) -> JsResult<()> {
    let p = b.page_or(r)?;
    if !can_have_children(p, parent) {
        return Err(dom_error(r, "HierarchyRequestError", "This node can't have children"));
    }
    if node == 0 || p.dom.contains(node, parent) {
        return Err(dom_error(
            r,
            "HierarchyRequestError",
            "The new child is an ancestor of the parent",
        ));
    }
    if before != NONE && p.dom.parent(before) != parent {
        return Err(dom_error(r, "NotFoundError", "The reference node is not a child of this node"));
    }
    if parent == 0 && matches!(p.dom.node(node).data, NodeData::Text(_)) {
        return Err(dom_error(r, "HierarchyRequestError", "A document can't hold text"));
    }
    Ok(())
}

/// Insert `node` (a fragment: its children) into `parent` before `before`, then run what
/// insertion runs. Returns the nodes inserted at the top.
pub fn insert(
    b: &Rc<Bridge>,
    r: &mut Realm,
    parent: NodeId,
    node: NodeId,
    before: NodeId,
) -> JsResult<Vec<NodeId>> {
    check_insert(b, r, parent, node, before)?;
    let tops: Vec<NodeId> = {
        let p = b.page_or(r)?;
        // Inserting a node before itself: before its next sibling instead.
        let before = if before == node {
            p.dom.node(node).next
        } else {
            before
        };
        if is_fragment(p, node) {
            let kids: Vec<NodeId> = p.dom.children(node).collect();
            for &k in &kids {
                p.dom.insert_before(parent, k, before);
            }
            kids
        } else {
            p.dom.insert_before(parent, node, before);
            alloc::vec![node]
        }
    };
    inserted(b, r, &tops);
    Ok(tops)
}

fn remove_node(b: &Bridge, r: &mut Realm, n: NodeId) -> JsResult<()> {
    let p = b.page_or(r)?;
    if p.focus != NONE && p.dom.contains(n, p.focus) {
        p.focus = NONE;
    }
    p.dom.detach(n);
    Ok(())
}

/// Turn the arguments of append / prepend / before / after / replaceWith into one node
/// (strings become text nodes; several make a fragment).
fn nodes_from_args(b: &Rc<Bridge>, r: &mut Realm, args: &[Value]) -> JsResult<NodeId> {
    let mut items = Vec::new();
    for &a in args {
        match node_of(b, r, a) {
            Some(n) => items.push(n),
            None => {
                let s = r.to_string(a)?;
                let p = b.page_or(r)?;
                check_room(p, r)?;
                items.push(p.dom.create_text(s));
            }
        }
    }
    if items.len() == 1 {
        return Ok(items[0]);
    }
    let p = b.page_or(r)?;
    let f = new_fragment(p);
    for n in items {
        if p.dom.contains(n, f) {
            continue;
        }
        p.dom.append(f, n);
    }
    Ok(f)
}

pub fn new_fragment(p: &mut Page) -> NodeId {
    let a = p.dom.atoms.intern(FRAGMENT);
    p.dom.create_element(a, Ns::Html, Vec::new())
}

/// Room for one more node (and its text)?
pub fn check_room(p: &Page, r: &mut Realm) -> JsResult<()> {
    if p.dom.live() >= p.limits().max_nodes {
        return Err(r.throw_error(jsengine::ErrorKind::RangeError, "too many DOM nodes"));
    }
    if p.dom.bytes >= crate::dom::MAX_DOM_BYTES {
        return Err(r.throw_error(jsengine::ErrorKind::RangeError, "the page is too big"));
    }
    Ok(())
}

/// Replace `el`'s children with the parsed `html` (innerHTML); scripts in it don't run.
pub fn set_inner_html(b: &Rc<Bridge>, r: &mut Realm, el: NodeId, html: &str) -> JsResult<()> {
    if !html.is_empty() {
        let p = b.page_or(r)?;
        check_room(p, r)?;
    }
    let tops = {
        let p = b.page_or(r)?;
        let ctx = context_atom(p, el);
        let frag = crate::html::parse_fragment(html, ctx, p.limits(), true);
        let old: Vec<NodeId> = p.dom.children(el).collect();
        if p.focus != NONE && p.dom.contains(el, p.focus) && p.focus != el {
            p.focus = NONE;
        }
        for &c in &old {
            p.dom.detach(c);
        }
        free_unseen(b, &old);
        let p = b.page_or(r)?;
        let room = p.limits().max_nodes.saturating_sub(p.dom.live());
        let root = frag.dom.document_element();
        p.dom.import(&frag.dom, root, el, NONE, room)
    };
    mark_scripts_started(b, &tops);
    for &t in &tops {
        register_handlers(b, r, t);
    }
    Ok(())
}

fn context_atom(p: &Page, el: NodeId) -> Atom {
    match p.dom.element(el) {
        Some(e) if e.ns == Ns::Html && e.name.0 < tag::COUNT => e.name,
        _ => tag::DIV,
    }
}

pub(crate) fn mark_scripts_started(b: &Bridge, tops: &[NodeId]) {
    let Some(p) = b.page() else { return };
    let mut st = b.st.borrow_mut();
    for &t in tops {
        let mut c = t;
        while c != NONE {
            if p.dom.is_html(c, tag::SCRIPT) {
                st.loader.mark_started(c);
            }
            c = p.dom.next_in(c, t);
        }
    }
}

/// Parse `html` as a fragment for `ctx` and insert it into `parent` before `before`.
fn insert_html(
    b: &Rc<Bridge>,
    r: &mut Realm,
    ctx: NodeId,
    parent: NodeId,
    before: NodeId,
    html: &str,
) -> JsResult<()> {
    check_room(b.page_or(r)?, r)?;
    let tops = {
        let p = b.page_or(r)?;
        let ca = context_atom(p, ctx);
        let frag = crate::html::parse_fragment(html, ca, p.limits(), true);
        let room = p.limits().max_nodes.saturating_sub(p.dom.live());
        let root = frag.dom.document_element();
        p.dom.import(&frag.dom, root, parent, before, room)
    };
    mark_scripts_started(b, &tops);
    for &t in &tops {
        register_handlers(b, r, t);
    }
    Ok(())
}

/// `document.write` while the page loads: the markup goes in after the running script.
pub fn document_write(b: &Rc<Bridge>, r: &mut Realm, html: &str) -> JsResult<()> {
    let cur = b.st(r)?.current_script;
    let loading = b.st(r)?.loader.phase == super::loader::Phase::Loading;
    let p = b.page_or(r)?;
    if cur == NONE || !loading || !p.dom.is_connected(cur) {
        b.console(
            crate::api::ConsoleLevel::Warn,
            String::from("document.write after the page loaded is ignored"),
        );
        return Ok(());
    }
    let parent = p.dom.parent(cur);
    // After the script and anything earlier writes put there.
    let mut after = p.dom.node(cur).next;
    let marker = b.st(r)?.write_after;
    if marker.0 == cur && marker.1 != NONE && b.page_or(r)?.dom.parent(marker.1) == parent {
        after = b.page_or(r)?.dom.node(marker.1).next;
    }
    let before_len = b.page_or(r)?.dom.children(parent).count();
    insert_html(b, r, parent, parent, after, html)?;
    // Remember the last node written, so the next write goes after it.
    let p = b.page_or(r)?;
    let last = if after == NONE {
        p.dom.node(parent).last_child
    } else {
        p.dom.node(after).prev
    };
    let _ = before_len;
    b.st(r)?.write_after = (cur, last);
    // Scripts written by document.write do run (they are parser-inserted in browsers).
    let mut scripts = Vec::new();
    let p = b.page_or(r)?;
    let mut c = p.dom.node(cur).next;
    while c != NONE && c != after {
        let mut d = c;
        while d != NONE {
            if p.dom.is_html(d, tag::SCRIPT) {
                scripts.push(d);
            }
            d = p.dom.next_in(d, c);
        }
        c = p.dom.node(c).next;
    }
    for s in scripts {
        let mut st = b.st(r)?;
        st.loader.unmark_started(s);
        st.loader.writing = true;
        drop(st);
        prepare_inserted(b, r, s);
        b.st(r)?.loader.writing = false;
    }
    Ok(())
}

// ---- querySelector -------------------------------------------------------------------------

fn parse_selector(b: &Bridge, r: &mut Realm, s: &str) -> JsResult<Vec<crate::css::selector::Selector>> {
    let p = b.page_or(r)?;
    let toks = crate::css::tokenizer::tokenize(s, 64 * 1024);
    match crate::css::selector::parse_list(&toks, &mut p.dom.atoms) {
        Some(l) if !l.is_empty() && l.iter().all(|x| x.pseudo.is_none()) => Ok(l),
        _ => Err(dom_error(
            r,
            "SyntaxError",
            &format!("'{s}' is not a valid selector"),
        )),
    }
}

/// Sibling positions for :nth-child() and friends, cached by the DOM's generation.
fn positions(b: &Bridge, p: &Page) -> Rc<(Vec<u32>, Vec<u32>)> {
    let gen_now = p.dom.generation;
    if let Ok(st) = b.st.try_borrow()
        && let Some((g, pos)) = &st.positions
        && *g == gen_now
        && pos.0.len() == p.dom.len()
    {
        return pos.clone();
    }
    let pos = Rc::new(crate::style::cascade::sibling_positions(&p.dom));
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.positions = Some((gen_now, pos.clone()));
    }
    pos
}

/// Elements under `root` (not `root` itself) matching `sel`, in tree order; at most `max`.
fn select(b: &Bridge, p: &Page, root: NodeId, sel: &[crate::css::selector::Selector], max: usize) -> Vec<NodeId> {
    let pos = positions(b, p);
    let cx = crate::css::selector::MatchCx {
        dom: &p.dom,
        index: &pos.0,
        count: &pos.1,
    };
    let mut out = Vec::new();
    let mut c = p.dom.next_in(root, root);
    while c != NONE {
        if p.dom.element(c).is_some() && sel.iter().any(|s| cx.matches(s, c)) {
            out.push(c);
            if out.len() >= max {
                break;
            }
        }
        c = p.dom.next_in(c, root);
    }
    out
}

fn matches_sel(b: &Bridge, p: &Page, n: NodeId, sel: &[crate::css::selector::Selector]) -> bool {
    let pos = positions(b, p);
    let cx = crate::css::selector::MatchCx {
        dom: &p.dom,
        index: &pos.0,
        count: &pos.1,
    };
    sel.iter().any(|s| cx.matches(s, n))
}

pub fn get_element_by_id(p: &Page, root: NodeId, id: &str) -> NodeId {
    let Some(a) = p.dom.atoms.get(id) else {
        return NONE;
    };
    let mut c = p.dom.next_in(root, root);
    while c != NONE {
        if p.dom.element(c).is_some_and(|e| e.id == Some(a)) {
            return c;
        }
        c = p.dom.next_in(c, root);
    }
    NONE
}

/// A static NodeList (an array with NodeList's prototype).
pub fn static_list(b: &Rc<Bridge>, r: &mut Realm, nodes: &[NodeId]) -> Value {
    let vals: Vec<Value> = nodes.iter().map(|&n| wrap(b, r, n)).collect();
    let a = r.new_array(&vals);
    let p = r.pinned(&b.protos().node_list);
    r.set_prototype(a, p);
    a
}

// ---- live lists ----------------------------------------------------------------------------

/// The nodes a live list has now.
pub fn list_nodes(p: &Page, k: &ListKind) -> Vec<NodeId> {
    let dom = &p.dom;
    let walk = |root: NodeId, f: &dyn Fn(NodeId) -> bool| {
        let mut v = Vec::new();
        let mut c = dom.next_in(root, root);
        while c != NONE {
            if dom.element(c).is_some() && f(c) {
                v.push(c);
            }
            c = dom.next_in(c, root);
        }
        v
    };
    match k {
        ListKind::Static(v) => v.clone(),
        ListKind::ChildNodes(n) => dom.children(*n).collect(),
        ListKind::Children(n) => dom.children(*n).filter(|&c| dom.element(c).is_some()).collect(),
        ListKind::ByTag(root, t) => {
            let all = t == "*";
            walk(*root, &|c| {
                all || dom.element(c).is_some_and(|e| {
                    let nm = dom.atoms.name(e.name);
                    if e.ns == Ns::Html {
                        nm.eq_ignore_ascii_case(t)
                    } else {
                        nm == t
                    }
                })
            })
        }
        ListKind::ByClass(root, cls) => walk(*root, &|c| {
            let e = dom.element(c).unwrap();
            !cls.is_empty()
                && cls.iter().all(|w| {
                    dom.atoms
                        .get(w)
                        .is_some_and(|a| e.classes.contains(&a))
                })
        }),
        ListKind::ByName(name) => walk(0, &|c| dom.element(c).unwrap().attr("name") == Some(name)),
        ListKind::Forms => walk(0, &|c| dom.is_html(c, tag::FORM)),
        ListKind::Images => walk(0, &|c| dom.is_html(c, tag::IMG)),
        ListKind::Scripts => walk(0, &|c| dom.is_html(c, tag::SCRIPT)),
        ListKind::Links => walk(0, &|c| {
            (dom.is_html(c, tag::A) || dom.is_html(c, tag::AREA))
                && dom.element(c).unwrap().has_attr("href")
        }),
        ListKind::FormElements(form) => {
            let fi = p.forms().forms.iter().position(|f| f == form);
            let mut v: Vec<(u32, NodeId)> = Vec::new();
            for (i, ctl) in p.forms().controls.iter().enumerate() {
                if fi.is_some() && ctl.form == fi.map(|x| x as u32) {
                    v.push((p.forms().seqs[i], p.forms().nodes[i]));
                }
            }
            for h in &p.forms().hidden {
                if fi.is_some() && h.form == fi.map(|x| x as u32) {
                    v.push((h.seq, h.node));
                }
            }
            v.sort();
            v.into_iter().map(|x| x.1).collect()
        }
        ListKind::Options(sel) => walk(*sel, &|c| dom.is_html(c, tag::OPTION)),
    }
}

/// The list object for `kind` (cached: `el.children === el.children`).
pub fn live_list(b: &Rc<Bridge>, r: &mut Realm, kind: ListKind, collection: bool) -> JsResult<Value> {
    let key = format!("{kind:?}");
    if let Some(p) = b.st(r)?.list_objs.get(&key) {
        return Ok(r.pinned(p));
    }
    let id = {
        let mut st = b.st(r)?;
        let id = st.next_list;
        st.next_list += 1;
        st.lists.insert(
            id,
            super::bridge::List {
                kind,
                seen: 0,
                cache: Vec::new(),
                cached: false,
            },
        );
        id
    };
    let proto = if collection {
        r.pinned(&b.protos().html_collection)
    } else {
        r.pinned(&b.protos().node_list)
    };
    let o = r.new_host_object(b.classes().list, id as u64, proto);
    let mut st = b.st(r)?;
    if st.list_objs.len() < 10_000 {
        drop(st);
        let p = r.pin(o);
        b.st(r)?.list_objs.insert(key, p);
    } else {
        st.lists.remove(&id);
        drop(st);
        // Too many distinct lists: a static snapshot instead.
        let nodes = {
            let p = b.page_or(r)?;
            list_nodes(p, &ListKind::Static(Vec::new()))
        };
        return Ok(static_list(b, r, &nodes));
    }
    Ok(o)
}

/// A live list's nodes now (cached by the DOM's generation).
pub fn list_now(b: &Bridge, id: u32) -> Vec<NodeId> {
    let Some(p) = b.page() else {
        return Vec::new();
    };
    p.sync_forms();
    let Ok(mut st) = b.st.try_borrow_mut() else {
        return Vec::new();
    };
    let now_gen = p.dom.generation;
    let Some(l) = st.lists.get_mut(&id) else {
        return Vec::new();
    };
    if !l.cached || l.seen != now_gen {
        l.cache = list_nodes(p, &l.kind);
        l.seen = now_gen;
        l.cached = true;
    }
    l.cache.clone()
}

pub struct ListClass {
    pub b: Rc<Bridge>,
}

const LIST_NAMES: &[&str] = &[
    "length", "item", "namedItem", "forEach", "entries", "keys", "values", "add", "remove",
    "selectedIndex", "value", "constructor", "toString", "hasOwnProperty",
];

impl jsengine::HostClass for ListClass {
    fn name(&self) -> &str {
        "NodeList"
    }
    fn hooks(&self) -> u8 {
        jsengine::hooks::INDEXED | jsengine::hooks::NAMED
    }
    fn get(&self, r: &mut Realm, _o: Value, data: u64, key: jsengine::Key<'_>) -> JsResult<Option<Value>> {
        match key {
            jsengine::Key::Index(i) => {
                let v = list_now(&self.b, data as u32);
                Ok(v.get(i as usize).map(|&n| wrap(&self.b, r, n)))
            }
            jsengine::Key::Str(s) => {
                if LIST_NAMES.contains(&s) {
                    return Ok(None);
                }
                let named = {
                    let st = self.b.st.borrow();
                    st.lists.get(&(data as u32)).is_some_and(|l| {
                        !matches!(l.kind, ListKind::ChildNodes(_) | ListKind::Static(_))
                    })
                };
                if !named {
                    return Ok(None);
                }
                Ok(named_item(&self.b, r, data as u32, s))
            }
            _ => Ok(None),
        }
    }
    fn keys(&self, _r: &mut Realm, _o: Value, data: u64) -> JsResult<Vec<jsengine::OwnedKey>> {
        let n = list_now(&self.b, data as u32).len();
        Ok((0..n as u32).map(jsengine::OwnedKey::Index).collect())
    }
}

fn named_item(b: &Rc<Bridge>, r: &mut Realm, id: u32, name: &str) -> Option<Value> {
    let nodes = list_now(b, id);
    let p = b.page()?;
    let hit = nodes.iter().copied().find(|&n| {
        p.dom
            .element(n)
            .is_some_and(|e| e.attr("id") == Some(name) || e.attr("name") == Some(name))
    })?;
    Some(wrap(b, r, hit))
}

// ---- install -------------------------------------------------------------------------------

type Native = fn(&Rc<Bridge>, &mut Realm, Value, &[Value]) -> JsResult<Value>;

pub fn method(b: &Rc<Bridge>, r: &mut Realm, obj: Value, name: &str, arity: u32, f: Native) -> JsResult<()> {
    let b2 = b.clone();
    r.define_method(obj, name, arity, move |r, t, a| f(&b2, r, t, a))?;
    Ok(())
}

pub fn getter(b: &Rc<Bridge>, r: &mut Realm, obj: Value, name: &str, g: Native, s: Option<Native>) -> JsResult<()> {
    let b2 = b.clone();
    let setter: Option<alloc::boxed::Box<jsengine::NativeFn>> = s.map(|s| {
        let b3 = b.clone();
        let f: alloc::boxed::Box<jsengine::NativeFn> =
            alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| s(&b3, r, t, a));
        f
    });
    r.define_getter(obj, name, move |r, t, a| g(&b2, r, t, a), setter)
}

/// A string attribute reflected as a property.
fn reflect_str(b: &Rc<Bridge>, r: &mut Realm, obj: Value, prop: &'static str, attr: &'static str) -> JsResult<()> {
    let b2 = b.clone();
    let b3 = b.clone();
    let set: alloc::boxed::Box<jsengine::NativeFn> =
        alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
            let n = this_elem(&b3, r, t)?;
            let v = arg_str(r, a, 0)?;
            set_attribute(&b3, r, n, attr, &v)?;
            Ok(Value::UNDEFINED)
        });
    r.define_getter(
        obj,
        prop,
        move |r, t, _| {
            let n = this_elem(&b2, r, t)?;
            let v = b2
                .page_or(r)?
                .dom
                .element(n)
                .and_then(|e| e.attr(attr))
                .map(String::from)
                .unwrap_or_default();
            Ok(r.string(&v))
        },
        Some(set),
    )
}

/// A boolean attribute reflected as a property.
fn reflect_bool(b: &Rc<Bridge>, r: &mut Realm, obj: Value, prop: &'static str, attr: &'static str) -> JsResult<()> {
    let b2 = b.clone();
    let b3 = b.clone();
    let set: alloc::boxed::Box<jsengine::NativeFn> =
        alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
            let n = this_elem(&b3, r, t)?;
            if arg_bool(r, a, 0) {
                set_attribute(&b3, r, n, attr, "")?;
            } else {
                remove_attribute(&b3, r, n, attr)?;
            }
            Ok(Value::UNDEFINED)
        });
    r.define_getter(
        obj,
        prop,
        move |r, t, _| {
            let n = this_elem(&b2, r, t)?;
            let v = b2
                .page_or(r)?
                .dom
                .element(n)
                .is_some_and(|e| e.has_attr(attr));
            Ok(Value::bool(v))
        },
        Some(set),
    )
}

/// A URL attribute: reads resolved against the base URL.
fn reflect_url(b: &Rc<Bridge>, r: &mut Realm, obj: Value, prop: &'static str, attr: &'static str) -> JsResult<()> {
    let b2 = b.clone();
    let b3 = b.clone();
    let set: alloc::boxed::Box<jsengine::NativeFn> =
        alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
            let n = this_elem(&b3, r, t)?;
            let v = arg_str(r, a, 0)?;
            set_attribute(&b3, r, n, attr, &v)?;
            Ok(Value::UNDEFINED)
        });
    r.define_getter(
        obj,
        prop,
        move |r, t, _| {
            let n = this_elem(&b2, r, t)?;
            let p = b2.page_or(r)?;
            let v = match p.dom.element(n).and_then(|e| e.attr(attr)) {
                Some(h) => p.resolve(h),
                None => String::new(),
            };
            Ok(r.string(&v))
        },
        Some(set),
    )
}

/// An integer attribute (with a default when absent or not a number).
fn reflect_int(b: &Rc<Bridge>, r: &mut Realm, obj: Value, prop: &'static str, attr: &'static str, def: i32) -> JsResult<()> {
    let b2 = b.clone();
    let b3 = b.clone();
    let set: alloc::boxed::Box<jsengine::NativeFn> =
        alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
            let n = this_elem(&b3, r, t)?;
            let v = r.to_int32(arg(a, 0))?;
            set_attribute(&b3, r, n, attr, &format!("{v}"))?;
            Ok(Value::UNDEFINED)
        });
    r.define_getter(
        obj,
        prop,
        move |r, t, _| {
            let n = this_elem(&b2, r, t)?;
            let v = b2
                .page_or(r)?
                .dom
                .element(n)
                .and_then(|e| e.attr(attr))
                .and_then(|v| v.trim().parse::<i32>().ok())
                .unwrap_or(def);
            Ok(Value::int(v))
        },
        Some(set),
    )
}

/// setAttribute with what it sets off: `on*` handlers, `async` bookkeeping.
pub fn set_attribute(b: &Rc<Bridge>, r: &mut Realm, n: NodeId, name: &str, value: &str) -> JsResult<()> {
    let p = b.page_or(r)?;
    let html = p.dom.element(n).is_some_and(|e| e.ns == Ns::Html);
    let name = if html {
        name.to_ascii_lowercase()
    } else {
        String::from(name)
    };
    if name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '>' | '/' | '=' | '<'))
    {
        return Err(dom_error(
            r,
            "InvalidCharacterError",
            &format!("'{name}' is not a valid attribute name"),
        ));
    }
    if value.len() > 4 << 20 {
        return Err(r.throw_error(jsengine::ErrorKind::RangeError, "attribute value too long"));
    }
    check_room(p, r)?;
    let p = b.page_or(r)?;
    p.dom.set_attr(n, &name, value);
    if let Some(ty) = handler_attr(&name) {
        let t = handler_target(p, n, ty);
        events_js::set_handler(b, r, t, ty, Some(Handler::Source(String::from(value))));
    }
    // A script element getting a src while in the tree (and not started): prepare it.
    if name == "src" && b.page_or(r)?.dom.is_html(n, tag::SCRIPT) {
        prepare_inserted(b, r, n);
    }
    Ok(())
}

pub fn remove_attribute(b: &Rc<Bridge>, r: &mut Realm, n: NodeId, name: &str) -> JsResult<()> {
    let p = b.page_or(r)?;
    let name = if p.dom.element(n).is_some_and(|e| e.ns == Ns::Html) {
        name.to_ascii_lowercase()
    } else {
        String::from(name)
    };
    p.dom.remove_attr(n, &name);
    if let Some(ty) = handler_attr(&name) {
        let t = handler_target(p, n, ty);
        events_js::set_handler(b, r, t, ty, None);
    }
    Ok(())
}

fn attr_of(b: &Bridge, r: &mut Realm, n: NodeId, name: &str) -> JsResult<Option<String>> {
    let p = b.page_or(r)?;
    let Some(e) = p.dom.element(n) else {
        return Ok(None);
    };
    let name = if e.ns == Ns::Html {
        name.to_ascii_lowercase()
    } else {
        String::from(name)
    };
    Ok(e.attr(&name).map(String::from))
}

/// The text a node holds (textContent): an element's or fragment's descendants' text.
fn text_content(p: &Page, n: NodeId) -> Option<String> {
    match &p.dom.node(n).data {
        NodeData::Text(t) | NodeData::Comment(t) => Some(t.clone()),
        NodeData::Element(_) => Some(p.dom.text_content(n)),
        _ => None,
    }
}

/// innerText, simply: the text content without scripts and styles, block boundaries and
/// `<br>` as line breaks.
fn inner_text(p: &Page, n: NodeId) -> String {
    let mut s = String::new();
    let mut c = p.dom.next_in(n, n);
    while c != NONE {
        match &p.dom.node(c).data {
            NodeData::Text(t) => {
                let mut prev_ws = s.ends_with([' ', '\n']) || s.is_empty();
                for ch in t.chars() {
                    if crate::dom::is_html_space(ch) {
                        if !prev_ws {
                            s.push(' ');
                            prev_ws = true;
                        }
                    } else {
                        s.push(ch);
                        prev_ws = false;
                    }
                }
            }
            NodeData::Element(e) => {
                if e.is(tag::SCRIPT) || e.is(tag::STYLE) || e.is(tag::TEMPLATE) || e.is(tag::HEAD) {
                    c = p.dom.next_skipping_children(c, n);
                    continue;
                }
                if e.is(tag::BR) {
                    while s.ends_with(' ') {
                        s.pop();
                    }
                    s.push('\n');
                } else if is_block(e.name) && !s.is_empty() && !s.ends_with('\n') {
                    while s.ends_with(' ') {
                        s.pop();
                    }
                    s.push('\n');
                }
            }
            _ => {}
        }
        c = p.dom.next_in(c, n);
    }
    String::from(s.trim())
}

fn is_block(t: Atom) -> bool {
    use tag::*;
    matches!(
        t,
        DIV | P | LI | UL | OL | H1 | H2 | H3 | H4 | H5 | H6 | TR | TABLE | SECTION | ARTICLE
            | HEADER | FOOTER | NAV | MAIN | ASIDE | BLOCKQUOTE | PRE | FORM | DL | DT | DD
            | FIGURE | FIGCAPTION | ADDRESS | FIELDSET | DETAILS | SUMMARY | HR
    )
}

fn set_text_content(b: &Rc<Bridge>, r: &mut Realm, n: NodeId, s: &str) -> JsResult<()> {
    let p = b.page_or(r)?;
    if !s.is_empty() {
        check_room(p, r)?;
    }
    let p = b.page_or(r)?;
    match &p.dom.node(n).data {
        NodeData::Text(_) | NodeData::Comment(_) => {
            p.dom.set_data(n, s);
        }
        NodeData::Element(_) => {
            if p.focus != NONE && p.dom.contains(n, p.focus) && p.focus != n {
                p.focus = NONE;
            }
            let old = p.dom.set_text_content(n, s);
            free_unseen(b, &old);
        }
        _ => {}
    }
    Ok(())
}

fn rc_val(b: &Rc<Bridge>, r: &mut Realm, n: NodeId) -> Value {
    wrap(b, r, n)
}

// The natives. Each takes (bridge, realm, this, args).

fn n_node_type(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    Ok(Value::int(node_type(b.page_or(r)?, n)))
}

fn n_node_name(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let p = b.page_or(r)?;
    let s = match &p.dom.node(n).data {
        NodeData::Document => String::from("#document"),
        NodeData::Doctype(d) => d[0].clone(),
        NodeData::Text(_) => String::from("#text"),
        NodeData::Comment(_) => String::from("#comment"),
        NodeData::Element(_) if is_fragment(p, n) => String::from("#document-fragment"),
        NodeData::Element(_) => tag_name(p, n),
    };
    Ok(r.string(&s))
}

fn n_node_value(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    match &b.page_or(r)?.dom.node(n).data {
        NodeData::Text(s) | NodeData::Comment(s) => {
            let s = s.clone();
            Ok(r.string(&s))
        }
        _ => Ok(Value::NULL),
    }
}

fn n_set_node_value(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str_or_empty(r, a, 0)?;
    check_room(b.page_or(r)?, r)?;
    let p = b.page_or(r)?;
    if matches!(p.dom.node(n).data, NodeData::Text(_) | NodeData::Comment(_)) {
        p.dom.set_data(n, &s);
    }
    Ok(Value::UNDEFINED)
}

fn rel(b: &Rc<Bridge>, r: &mut Realm, t: Value, f: fn(&Page, NodeId) -> NodeId) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let m = f(b.page_or(r)?, n);
    Ok(rc_val(b, r, m))
}

fn n_parent(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| p.dom.parent(n))
}
fn n_parent_element(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| {
        let x = p.dom.parent(n);
        if x != NONE && p.dom.element(x).is_some() && !is_fragment(p, x) {
            x
        } else {
            NONE
        }
    })
}
fn n_first_child(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| p.dom.node(n).first_child)
}
fn n_last_child(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| p.dom.node(n).last_child)
}
fn n_prev(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| p.dom.node(n).prev)
}
fn n_next(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| p.dom.node(n).next)
}
fn elem_step(p: &Page, mut n: NodeId, fwd: bool) -> NodeId {
    loop {
        n = if fwd { p.dom.node(n).next } else { p.dom.node(n).prev };
        if n == NONE || p.dom.element(n).is_some() {
            return n;
        }
    }
}
fn n_first_elem(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| {
        let c = p.dom.node(n).first_child;
        if c == NONE || p.dom.element(c).is_some() {
            c
        } else {
            elem_step(p, c, true)
        }
    })
}
fn n_last_elem(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| {
        let c = p.dom.node(n).last_child;
        if c == NONE || p.dom.element(c).is_some() {
            c
        } else {
            elem_step(p, c, false)
        }
    })
}
fn n_prev_elem(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| elem_step(p, n, false))
}
fn n_next_elem(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| elem_step(p, n, true))
}
fn n_child_count(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let p = b.page_or(r)?;
    let k = p.dom.children(n).filter(|&c| p.dom.element(c).is_some()).count();
    Ok(Value::int(k as i32))
}
fn n_child_nodes(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    live_list(b, r, ListKind::ChildNodes(n), false)
}
fn n_children(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    live_list(b, r, ListKind::Children(n), true)
}
fn n_owner_document(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    if n == 0 {
        return Ok(Value::NULL);
    }
    Ok(wrap(b, r, 0))
}
fn n_is_connected(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    Ok(Value::bool(b.page_or(r)?.dom.is_connected(n)))
}
fn n_text_content(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    match text_content(b.page_or(r)?, n) {
        Some(s) => Ok(r.string(&s)),
        None => Ok(Value::NULL),
    }
}
fn n_set_text_content(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str_or_empty(r, a, 0)?;
    set_text_content(b, r, n, &s)?;
    Ok(Value::UNDEFINED)
}
fn n_append_child(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let p = this_node(b, r, t)?;
    let c = arg_node(b, r, a, 0)?;
    insert(b, r, p, c, NONE)?;
    Ok(arg(a, 0))
}
fn n_insert_before(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let p = this_node(b, r, t)?;
    let c = arg_node(b, r, a, 0)?;
    let before = opt_node(b, r, arg(a, 1)).unwrap_or(NONE);
    insert(b, r, p, c, before)?;
    Ok(arg(a, 0))
}
fn n_remove_child(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let p = this_node(b, r, t)?;
    let c = arg_node(b, r, a, 0)?;
    if b.page_or(r)?.dom.parent(c) != p {
        return Err(dom_error(r, "NotFoundError", "The node to be removed is not a child of this node"));
    }
    remove_node(b, r, c)?;
    Ok(arg(a, 0))
}
fn n_replace_child(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let p = this_node(b, r, t)?;
    let new = arg_node(b, r, a, 0)?;
    let old = arg_node(b, r, a, 1)?;
    if b.page_or(r)?.dom.parent(old) != p {
        return Err(dom_error(r, "NotFoundError", "The node to be replaced is not a child of this node"));
    }
    if new == old {
        return Ok(arg(a, 1));
    }
    let next = b.page_or(r)?.dom.node(old).next;
    let next = if next == new { b.page_or(r)?.dom.node(new).next } else { next };
    check_insert(b, r, p, new, NONE)?;
    remove_node(b, r, old)?;
    insert(b, r, p, new, next)?;
    Ok(arg(a, 1))
}
fn n_clone(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let deep = arg_bool(r, a, 0);
    let p = b.page_or(r)?;
    check_room(p, r)?;
    let p = b.page_or(r)?;
    let room = p.limits().max_nodes.saturating_sub(p.dom.live());
    let c = p.dom.clone_node(n, deep, room);
    // Cloned scripts keep their "already started" (HTML says so).
    let started = b.st(r)?.loader.is_started(n);
    if started {
        b.st(r)?.loader.mark_started(c);
    }
    Ok(wrap(b, r, c))
}
fn n_contains(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let Some(o) = opt_node(b, r, arg(a, 0)) else {
        return Ok(Value::FALSE);
    };
    Ok(Value::bool(b.page_or(r)?.dom.contains(n, o)))
}
fn n_has_children(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    Ok(Value::bool(b.page_or(r)?.dom.node(n).first_child != NONE))
}
fn n_root(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    rel(b, r, t, |p, n| p.dom.root_of(n))
}
fn n_same(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    Ok(Value::bool(opt_node(b, r, arg(a, 0)) == Some(n)))
}
fn n_equal(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let Some(o) = opt_node(b, r, arg(a, 0)) else {
        return Ok(Value::FALSE);
    };
    let p = b.page_or(r)?;
    let s1 = crate::serialize::outer_html(&p.dom, n, true, 1 << 20);
    let s2 = crate::serialize::outer_html(&p.dom, o, true, 1 << 20);
    Ok(Value::bool(node_type(p, n) == node_type(p, o) && s1 == s2))
}
fn n_compare(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let o = arg_node(b, r, a, 0)?;
    if n == o {
        return Ok(Value::int(0));
    }
    let p = b.page_or(r)?;
    if p.dom.root_of(n) != p.dom.root_of(o) {
        return Ok(Value::int(1 | 32 | if n < o { 4 } else { 2 }));
    }
    if p.dom.contains(n, o) {
        return Ok(Value::int(16 | 4)); // contained_by | following
    }
    if p.dom.contains(o, n) {
        return Ok(Value::int(8 | 2)); // contains | preceding
    }
    // Tree order: walk from the root.
    let root = p.dom.root_of(n);
    let mut c = root;
    while c != NONE {
        if c == n {
            return Ok(Value::int(4));
        }
        if c == o {
            return Ok(Value::int(2));
        }
        c = p.dom.next_in(c, root);
    }
    Ok(Value::int(0))
}
fn n_normalize(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let p = b.page_or(r)?;
    let mut c = p.dom.next_in(n, n);
    while c != NONE {
        let next_c = p.dom.next_in(c, n);
        if let NodeData::Text(s) = &p.dom.node(c).data {
            let mut s = s.clone();
            let mut m = p.dom.node(c).next;
            while m != NONE && matches!(p.dom.node(m).data, NodeData::Text(_)) {
                if let NodeData::Text(x) = &p.dom.node(m).data {
                    s.push_str(x);
                }
                let after = p.dom.node(m).next;
                p.dom.detach(m);
                m = after;
            }
            if s.is_empty() {
                p.dom.detach(c);
            } else {
                p.dom.set_data(c, &s);
            }
            c = if m == NONE { p.dom.next_skipping_children(c, n) } else { m };
            let _ = next_c;
            continue;
        }
        c = next_c;
    }
    Ok(Value::UNDEFINED)
}

// Element.

fn e_tag_name(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let s = tag_name(b.page_or(r)?, n);
    Ok(r.string(&s))
}
fn e_local_name(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let p = b.page_or(r)?;
    let s = String::from(p.dom.atoms.name(p.dom.element(n).unwrap().name));
    Ok(r.string(&s))
}
fn e_namespace(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let s = match b.page_or(r)?.dom.element(n).unwrap().ns {
        Ns::Html => "http://www.w3.org/1999/xhtml",
        Ns::Svg => "http://www.w3.org/2000/svg",
        Ns::MathMl => "http://www.w3.org/1998/Math/MathML",
    };
    Ok(r.string(s))
}
fn e_get_attribute(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let name = arg_str(r, a, 0)?;
    match attr_of(b, r, n, &name)? {
        Some(v) => Ok(r.string(&v)),
        None => Ok(Value::NULL),
    }
}
fn e_set_attribute(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let name = arg_str(r, a, 0)?;
    let v = arg_str(r, a, 1)?;
    set_attribute(b, r, n, &name, &v)?;
    Ok(Value::UNDEFINED)
}
fn e_remove_attribute(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let name = arg_str(r, a, 0)?;
    remove_attribute(b, r, n, &name)?;
    Ok(Value::UNDEFINED)
}
fn e_has_attribute(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let name = arg_str(r, a, 0)?;
    Ok(Value::bool(attr_of(b, r, n, &name)?.is_some()))
}
fn e_has_attributes(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    Ok(Value::bool(
        !b.page_or(r)?.dom.element(n).unwrap().attrs.is_empty(),
    ))
}
fn e_toggle_attribute(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let name = arg_str(r, a, 0)?;
    let has = attr_of(b, r, n, &name)?.is_some();
    let want = if a.len() > 1 { arg_bool(r, a, 1) } else { !has };
    if want && !has {
        set_attribute(b, r, n, &name, "")?;
    } else if !want && has {
        remove_attribute(b, r, n, &name)?;
    }
    Ok(Value::bool(want))
}
fn e_attribute_names(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let names: Vec<String> = b
        .page_or(r)?
        .dom
        .element(n)
        .unwrap()
        .attrs
        .iter()
        .map(|a| String::from(&*a.name))
        .collect();
    let vals: Vec<Value> = names.iter().map(|s| r.string(s)).collect();
    Ok(r.new_array(&vals))
}
fn e_attributes(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let attrs: Vec<Attr> = b.page_or(r)?.dom.element(n).unwrap().attrs.clone();
    let mut vals = Vec::new();
    for at in &attrs {
        let o = r.new_object();
        let nm = r.string(&at.name);
        let v = r.string(&at.value);
        r.set(o, "name", nm)?;
        r.set(o, "localName", nm)?;
        r.set(o, "nodeName", nm)?;
        r.set(o, "value", v)?;
        r.set(o, "nodeValue", v)?;
        r.set(o, "specified", Value::TRUE)?;
        vals.push(o);
    }
    let arr = r.new_array(&vals);
    for (i, at) in attrs.iter().enumerate() {
        let v = vals[i];
        let _ = r.define(arr, &*at.name, v, Attrs::CONFIGURABLE);
    }
    Ok(arr)
}
fn e_id(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let v = attr_of(b, r, n, "id")?.unwrap_or_default();
    Ok(r.string(&v))
}
fn e_set_id(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let v = arg_str(r, a, 0)?;
    set_attribute(b, r, n, "id", &v)?;
    Ok(Value::UNDEFINED)
}
fn e_class_name(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let v = attr_of(b, r, n, "class")?.unwrap_or_default();
    Ok(r.string(&v))
}
fn e_set_class_name(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let v = arg_str(r, a, 0)?;
    set_attribute(b, r, n, "class", &v)?;
    Ok(Value::UNDEFINED)
}
fn e_inner_html(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = crate::serialize::inner_html(&b.page_or(r)?.dom, n, true, 16 << 20);
    Ok(r.string(&s))
}
fn e_set_inner_html(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str_or_empty(r, a, 0)?;
    set_inner_html(b, r, n, &s)?;
    Ok(Value::UNDEFINED)
}
fn e_outer_html(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let s = crate::serialize::outer_html(&b.page_or(r)?.dom, n, true, 16 << 20);
    Ok(r.string(&s))
}
fn e_set_outer_html(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let s = arg_str_or_empty(r, a, 0)?;
    let parent = b.page_or(r)?.dom.parent(n);
    if parent == NONE {
        return Ok(Value::UNDEFINED);
    }
    if parent == 0 {
        return Err(dom_error(r, "NoModificationAllowedError", "Can't replace the root element"));
    }
    insert_html(b, r, parent, parent, n, &s)?;
    remove_node(b, r, n)?;
    Ok(Value::UNDEFINED)
}
fn e_insert_adjacent_html(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let pos = arg_str(r, a, 0)?.to_ascii_lowercase();
    let html = arg_str(r, a, 1)?;
    let p = b.page_or(r)?;
    let parent = p.dom.parent(n);
    match pos.as_str() {
        "beforebegin" if parent != NONE && parent != 0 => insert_html(b, r, parent, parent, n, &html)?,
        "afterend" if parent != NONE && parent != 0 => {
            let next = p.dom.node(n).next;
            insert_html(b, r, parent, parent, next, &html)?
        }
        "afterbegin" => {
            let first = p.dom.node(n).first_child;
            insert_html(b, r, n, n, first, &html)?
        }
        "beforeend" => insert_html(b, r, n, n, NONE, &html)?,
        "beforebegin" | "afterend" => {
            return Err(dom_error(r, "NoModificationAllowedError", "The element has no parent"));
        }
        _ => return Err(dom_error(r, "SyntaxError", "insertAdjacentHTML: bad position")),
    }
    Ok(Value::UNDEFINED)
}
fn adjacent_target(p: &Page, n: NodeId, pos: &str) -> Option<(NodeId, NodeId)> {
    let parent = p.dom.parent(n);
    match pos {
        "beforebegin" if parent != NONE => Some((parent, n)),
        "afterend" if parent != NONE => Some((parent, p.dom.node(n).next)),
        "afterbegin" => Some((n, p.dom.node(n).first_child)),
        "beforeend" => Some((n, NONE)),
        _ => None,
    }
}
fn e_insert_adjacent_element(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let pos = arg_str(r, a, 0)?.to_ascii_lowercase();
    let el = arg_node(b, r, a, 1)?;
    match adjacent_target(b.page_or(r)?, n, &pos) {
        Some((parent, before)) => {
            insert(b, r, parent, el, before)?;
            Ok(arg(a, 1))
        }
        None => Ok(Value::NULL),
    }
}
fn e_insert_adjacent_text(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let pos = arg_str(r, a, 0)?.to_ascii_lowercase();
    let s = arg_str(r, a, 1)?;
    let tgt = adjacent_target(b.page_or(r)?, n, &pos);
    if let Some((parent, before)) = tgt {
        let p = b.page_or(r)?;
        check_room(p, r)?;
        let tn = b.page_or(r)?.dom.create_text(s);
        insert(b, r, parent, tn, before)?;
    }
    Ok(Value::UNDEFINED)
}
fn e_inner_text(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = inner_text(b.page_or(r)?, n);
    Ok(r.string(&s))
}
fn e_set_inner_text(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str_or_empty(r, a, 0)?;
    if !s.contains('\n') {
        set_text_content(b, r, n, &s)?;
        return Ok(Value::UNDEFINED);
    }
    // Line breaks become <br>s.
    set_text_content(b, r, n, "")?;
    let p = b.page_or(r)?;
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            let br = p.dom.create_element(tag::BR, Ns::Html, Vec::new());
            p.dom.append(n, br);
        }
        if !line.is_empty() {
            let tn = p.dom.create_text(String::from(line));
            p.dom.append(n, tn);
        }
    }
    Ok(Value::UNDEFINED)
}
fn e_query_selector(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str(r, a, 0)?;
    let sel = parse_selector(b, r, &s)?;
    let hit = select(b, b.page_or(r)?, n, &sel, 1);
    Ok(match hit.first() {
        Some(&h) => wrap(b, r, h),
        None => Value::NULL,
    })
}
fn e_query_selector_all(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str(r, a, 0)?;
    let sel = parse_selector(b, r, &s)?;
    let hits = select(b, b.page_or(r)?, n, &sel, 1 << 20);
    Ok(static_list(b, r, &hits))
}
fn e_matches(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let s = arg_str(r, a, 0)?;
    let sel = parse_selector(b, r, &s)?;
    Ok(Value::bool(matches_sel(b, b.page_or(r)?, n, &sel)))
}
fn e_closest(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let s = arg_str(r, a, 0)?;
    let sel = parse_selector(b, r, &s)?;
    let p = b.page_or(r)?;
    let mut c = n;
    while c != NONE {
        if p.dom.element(c).is_some() && matches_sel(b, p, c, &sel) {
            return Ok(wrap(b, r, c));
        }
        c = p.dom.parent(c);
    }
    Ok(Value::NULL)
}
fn e_by_tag(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str(r, a, 0)?;
    live_list(b, r, ListKind::ByTag(n, s), true)
}
fn e_by_class(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str(r, a, 0)?;
    let cls: Vec<String> = s
        .split(crate::dom::is_html_space)
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();
    live_list(b, r, ListKind::ByClass(n, cls), true)
}
fn e_remove(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    remove_node(b, r, n)?;
    Ok(Value::UNDEFINED)
}
fn e_append(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    if a.is_empty() {
        return Ok(Value::UNDEFINED);
    }
    let x = nodes_from_args(b, r, a)?;
    insert(b, r, n, x, NONE)?;
    Ok(Value::UNDEFINED)
}
fn e_prepend(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    if a.is_empty() {
        return Ok(Value::UNDEFINED);
    }
    let x = nodes_from_args(b, r, a)?;
    let first = b.page_or(r)?.dom.node(n).first_child;
    insert(b, r, n, x, first)?;
    Ok(Value::UNDEFINED)
}
fn e_replace_children(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let x = if a.is_empty() { NONE } else { nodes_from_args(b, r, a)? };
    let p = b.page_or(r)?;
    let old: Vec<NodeId> = p.dom.children(n).collect();
    for c in old {
        remove_node(b, r, c)?;
    }
    if x != NONE {
        insert(b, r, n, x, NONE)?;
    }
    Ok(Value::UNDEFINED)
}
fn e_before(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let parent = b.page_or(r)?.dom.parent(n);
    if parent == NONE || a.is_empty() {
        return Ok(Value::UNDEFINED);
    }
    let x = nodes_from_args(b, r, a)?;
    insert(b, r, parent, x, n)?;
    Ok(Value::UNDEFINED)
}
fn e_after(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let parent = b.page_or(r)?.dom.parent(n);
    if parent == NONE || a.is_empty() {
        return Ok(Value::UNDEFINED);
    }
    let x = nodes_from_args(b, r, a)?;
    let next = b.page_or(r)?.dom.node(n).next;
    insert(b, r, parent, x, next)?;
    Ok(Value::UNDEFINED)
}
fn e_replace_with(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let parent = b.page_or(r)?.dom.parent(n);
    if parent == NONE {
        return Ok(Value::UNDEFINED);
    }
    if a.is_empty() {
        remove_node(b, r, n)?;
        return Ok(Value::UNDEFINED);
    }
    let x = nodes_from_args(b, r, a)?;
    if x == n {
        return Ok(Value::UNDEFINED);
    }
    let next = b.page_or(r)?.dom.node(n).next;
    remove_node(b, r, n)?;
    insert(b, r, parent, x, next)?;
    Ok(Value::UNDEFINED)
}
fn e_rect(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let (x, y, w, h) = client_rect(b, n);
    dom_rect(b, r, x, y, w, h)
}
fn e_rects(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let (x, y, w, h) = client_rect(b, n);
    if w == 0.0 && h == 0.0 {
        return Ok(r.new_array(&[]));
    }
    let o = dom_rect(b, r, x, y, w, h)?;
    Ok(r.new_array(&[o]))
}
fn is_root_scroller(p: &Page, n: NodeId) -> bool {
    n == p.dom.document_element() || n == p.dom.body()
}
fn e_offset_width(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    Ok(Value::number(doc_rect(b, n).2.rnd()))
}
fn e_offset_height(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    Ok(Value::number(doc_rect(b, n).3.rnd()))
}
fn e_offset_top(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    Ok(Value::number(doc_rect(b, n).1.rnd()))
}
fn e_offset_left(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    Ok(Value::number(doc_rect(b, n).0.rnd()))
}
fn e_offset_parent(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let p = b.page_or(r)?;
    let body = p.dom.body();
    if n == body || n == p.dom.document_element() || !p.dom.is_connected(n) {
        return Ok(Value::NULL);
    }
    Ok(wrap(b, r, body))
}
fn viewport_css(b: &Bridge) -> (f64, f64) {
    match b.host() {
        Some(h) => {
            let vp = h.viewport();
            let z = vp.zoom.clamp(25, 500) as f64 / 100.0;
            (vp.width as f64 / z, vp.height as f64 / z)
        }
        None => (0.0, 0.0),
    }
}
fn doc_size_css(b: &Bridge) -> (f64, f64) {
    ensure_layout(b);
    match (b.page(), b.host()) {
        (Some(p), Some(h)) => {
            let z = h.viewport().zoom.clamp(25, 500) as f64 / 100.0;
            let dl = p.display_list();
            (dl.width as f64 / z, dl.height as f64 / z)
        }
        _ => (0.0, 0.0),
    }
}
fn e_client_width(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    if n == b.page_or(r)?.dom.document_element() {
        return Ok(Value::number(viewport_css(b).0.rnd()));
    }
    Ok(Value::number(doc_rect(b, n).2.rnd()))
}
fn e_client_height(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    if n == b.page_or(r)?.dom.document_element() {
        return Ok(Value::number(viewport_css(b).1.rnd()));
    }
    Ok(Value::number(doc_rect(b, n).3.rnd()))
}
fn e_scroll_width(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    if is_root_scroller(b.page_or(r)?, n) {
        return Ok(Value::number(doc_size_css(b).0.rnd()));
    }
    Ok(Value::number(doc_rect(b, n).2.rnd()))
}
fn e_scroll_height(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    if is_root_scroller(b.page_or(r)?, n) {
        return Ok(Value::number(doc_size_css(b).1.rnd()));
    }
    Ok(Value::number(doc_rect(b, n).3.rnd()))
}
fn e_scroll_top(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    if is_root_scroller(b.page_or(r)?, n) {
        return Ok(Value::number(super::window::scroll_css(b).1));
    }
    Ok(Value::int(0))
}
fn e_set_scroll_top(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let y = arg(a, 0);
    let y = r.to_number(y)?;
    if is_root_scroller(b.page_or(r)?, n) {
        super::window::scroll_to_css(b, None, Some(y));
    }
    Ok(Value::UNDEFINED)
}
fn e_scroll_left(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let _ = this_elem(b, r, t)?;
    Ok(Value::int(0))
}
fn e_scroll_into_view(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    ensure_layout(b);
    if let Some(rc) = b.page().and_then(|p| p.node_rect(n)) {
        b.request(PageRequest::ScrollTo { x: 0, y: rc.y.max(0) });
    }
    Ok(Value::UNDEFINED)
}
fn e_focus(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let ok = {
        let p = b.page_or(r)?;
        p.sync_forms();
        let ctl = p.control_of_node(n);
        ctl.is_some_and(|id| !p.controls()[id.0 as usize].disabled) && p.dom.is_connected(n)
    };
    if ok {
        events_js::move_focus(b, r, n);
        // Keep it in view.
        ensure_layout(b);
        if let (Some(p), Some(h)) = (b.page(), b.host())
            && let Some(rc) = p.node_rect(n)
        {
            let (_, sy) = h.scroll();
            let vh = h.viewport().height;
            if rc.y < sy || rc.y + rc.h > sy + vh {
                b.request(PageRequest::ScrollTo {
                    x: 0,
                    y: (rc.y - vh / 3).max(0),
                });
            }
        }
    }
    Ok(Value::UNDEFINED)
}
fn e_blur(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    if b.page_or(r)?.focus == n {
        events_js::move_focus(b, r, NONE);
    }
    Ok(Value::UNDEFINED)
}
fn e_click(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    events_js::script_click(b, r, n)?;
    Ok(Value::UNDEFINED)
}
fn e_class_list(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    super::window::cached_object(b, r, "classList", n, b.classes().tokens, &b.protos().token_list)
}
fn e_style(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    super::window::cached_object(b, r, "style", n, b.classes().style, &b.protos().style_decl)
}
fn e_set_style(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let v = arg_str(r, a, 0)?;
    set_attribute(b, r, n, "style", &v)?;
    Ok(Value::UNDEFINED)
}
fn e_dataset(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    super::window::cached_object(b, r, "dataset", n, b.classes().dataset, &b.protos().string_map)
}

// CharacterData.

fn c_data(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    n_node_value(b, r, t, &[])
}
fn c_length(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let len = match &b.page_or(r)?.dom.node(n).data {
        NodeData::Text(s) | NodeData::Comment(s) => s.encode_utf16().count(),
        _ => 0,
    };
    Ok(Value::int(len as i32))
}
fn c_append_data(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_node(b, r, t)?;
    let s = arg_str(r, a, 0)?;
    check_room(b.page_or(r)?, r)?;
    let p = b.page_or(r)?;
    if let NodeData::Text(x) | NodeData::Comment(x) = &p.dom.node(n).data {
        let mut x = x.clone();
        x.push_str(&s);
        p.dom.set_data(n, &x);
    }
    Ok(Value::UNDEFINED)
}

// Document.

fn d_document_element(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let n = b.page_or(r)?.dom.document_element();
    Ok(wrap(b, r, n))
}
fn d_head(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let p = b.page_or(r)?;
    let html = p.dom.document_element();
    let h = if html == NONE {
        NONE
    } else {
        p.dom.children(html).find(|&c| p.dom.is_html(c, tag::HEAD)).unwrap_or(NONE)
    };
    Ok(wrap(b, r, h))
}
fn d_body(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let n = b.page_or(r)?.dom.body();
    Ok(wrap(b, r, n))
}
fn d_title(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let s = b.page_or(r)?.title();
    Ok(r.string(&s))
}
fn d_set_title(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let s = arg_str(r, a, 0)?;
    let p = b.page_or(r)?;
    let mut t = p.dom.find_tag(tag::TITLE);
    if t == NONE {
        let html = p.dom.document_element();
        let head = if html == NONE {
            NONE
        } else {
            p.dom.children(html).find(|&c| p.dom.is_html(c, tag::HEAD)).unwrap_or(NONE)
        };
        if head == NONE {
            return Ok(Value::UNDEFINED);
        }
        t = p.dom.create_element(tag::TITLE, Ns::Html, Vec::new());
        p.dom.append(head, t);
    }
    p.dom.set_text_content(t, &s);
    b.request(PageRequest::Title);
    Ok(Value::UNDEFINED)
}
fn d_url(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let s = String::from(b.page_or(r)?.page_url());
    Ok(r.string(&s))
}
fn d_ready_state(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let s = match b.st(r)?.loader.phase {
        super::loader::Phase::Loading => "loading",
        super::loader::Phase::Interactive | super::loader::Phase::ContentLoaded => "interactive",
        super::loader::Phase::Complete => "complete",
    };
    Ok(r.string(s))
}
/// `document.cookie` of a document with an opaque origin (a sandbox without
/// `allow-same-origin`, a `data:` document) is a SecurityError.
fn opaque_cookie_check(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    if b.xf.sec.borrow().opaque {
        return Err(dom_error(
            r,
            "SecurityError",
            "The document is sandboxed and lacks the 'allow-same-origin' flag.",
        ));
    }
    Ok(())
}

fn d_cookie(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    opaque_cookie_check(b, r)?;
    let s = b.host().map(|h| h.cookies()).unwrap_or_default();
    Ok(r.string(&s))
}
fn d_set_cookie(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    opaque_cookie_check(b, r)?;
    let s = arg_str(r, a, 0)?;
    if let Some(h) = b.host() {
        h.set_cookie(&s);
    }
    Ok(Value::UNDEFINED)
}
fn d_active_element(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let p = b.page_or(r)?;
    let n = if p.focus != NONE && p.dom.is_connected(p.focus) {
        p.focus
    } else {
        p.dom.body()
    };
    Ok(wrap(b, r, n))
}
fn d_current_script(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let n = b.st(r)?.current_script;
    Ok(wrap(b, r, n))
}
fn d_get_element_by_id(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let root = this_node(b, r, t)?;
    let id = arg_str(r, a, 0)?;
    let n = get_element_by_id(b.page_or(r)?, root, &id);
    Ok(wrap(b, r, n))
}
fn d_by_name(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let s = arg_str(r, a, 0)?;
    live_list(b, r, ListKind::ByName(s), false)
}
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() < 256
        && s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == ':')
        && s
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}
fn d_create_element(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let name = arg_str(r, a, 0)?;
    if !valid_name(&name) {
        return Err(dom_error(
            r,
            "InvalidCharacterError",
            &format!("'{name}' is not a valid element name"),
        ));
    }
    let p = b.page_or(r)?;
    check_room(p, r)?;
    let p = b.page_or(r)?;
    let at = p.dom.atoms.intern(&name.to_ascii_lowercase());
    let n = p.dom.create_element(at, Ns::Html, Vec::new());
    Ok(wrap(b, r, n))
}
fn d_create_element_ns(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let ns = if arg(a, 0).is_nullish() { String::new() } else { arg_str(r, a, 0)? };
    let name = arg_str(r, a, 1)?;
    if !valid_name(&name) {
        return Err(dom_error(r, "InvalidCharacterError", "not a valid element name"));
    }
    let local = String::from(name.rsplit(':').next().unwrap_or(&name));
    let p = b.page_or(r)?;
    check_room(p, r)?;
    let p = b.page_or(r)?;
    let (ns, local) = match ns.as_str() {
        "http://www.w3.org/2000/svg" => (Ns::Svg, local),
        "http://www.w3.org/1998/Math/MathML" => (Ns::MathMl, local),
        _ => (Ns::Html, local.to_ascii_lowercase()),
    };
    let at = p.dom.atoms.intern(&local);
    let n = p.dom.create_element(at, ns, Vec::new());
    Ok(wrap(b, r, n))
}
fn d_create_text(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let s = arg_str(r, a, 0)?;
    let p = b.page_or(r)?;
    check_room(p, r)?;
    let n = b.page_or(r)?.dom.create_text(s);
    Ok(wrap(b, r, n))
}
fn d_create_comment(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let s = arg_str(r, a, 0)?;
    let p = b.page_or(r)?;
    check_room(p, r)?;
    let n = b.page_or(r)?.dom.create_comment(s);
    Ok(wrap(b, r, n))
}
fn d_create_fragment(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let p = b.page_or(r)?;
    check_room(p, r)?;
    let n = new_fragment(b.page_or(r)?);
    Ok(wrap(b, r, n))
}
fn d_create_event(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let s = arg_str(r, a, 0)?;
    events_js::create_event(b, r, &s)
}
fn d_write(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let mut s = String::new();
    for &v in a {
        s.push_str(&r.to_string(v)?);
    }
    document_write(b, r, &s)?;
    Ok(Value::UNDEFINED)
}
fn d_writeln(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let mut s = String::new();
    for &v in a {
        s.push_str(&r.to_string(v)?);
    }
    s.push('\n');
    document_write(b, r, &s)?;
    Ok(Value::UNDEFINED)
}
fn d_element_from_point(b: &Rc<Bridge>, r: &mut Realm, _: Value, a: &[Value]) -> JsResult<Value> {
    let x = arg(a, 0);
    let x = r.to_number(x)?;
    let y = arg(a, 1);
    let y = r.to_number(y)?;
    ensure_layout(b);
    let (Some(p), Some(h)) = (b.page(), b.host()) else {
        return Ok(Value::NULL);
    };
    let z = h.viewport().zoom.clamp(25, 500) as f64 / 100.0;
    let mut n = p.node_at((x * z) as i32, (y * z) as i32, h.scroll());
    while n != NONE && p.dom.element(n).is_none() {
        n = p.dom.parent(n);
    }
    Ok(wrap(b, r, n))
}
fn d_list(b: &Rc<Bridge>, r: &mut Realm, k: ListKind) -> JsResult<Value> {
    live_list(b, r, k, true)
}
fn d_forms(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    d_list(b, r, ListKind::Forms)
}
fn d_images(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    d_list(b, r, ListKind::Images)
}
fn d_links(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    d_list(b, r, ListKind::Links)
}
fn d_scripts(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    d_list(b, r, ListKind::Scripts)
}
fn d_doctype(b: &Rc<Bridge>, r: &mut Realm, _: Value, _: &[Value]) -> JsResult<Value> {
    let p = b.page_or(r)?;
    let n = p
        .dom
        .children(0)
        .find(|&c| matches!(p.dom.node(c).data, NodeData::Doctype(_)))
        .unwrap_or(NONE);
    Ok(wrap(b, r, n))
}
fn d_template_content(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let existing = b.st(r)?.template_content.get(&n).copied();
    if let Some(f) = existing {
        return Ok(wrap(b, r, f));
    }
    let p = b.page_or(r)?;
    let f = new_fragment(p);
    // The template's children become its content (the parser keeps them as children).
    let kids: Vec<NodeId> = p.dom.children(n).collect();
    for k in kids {
        p.dom.append(f, k);
    }
    b.st(r)?.template_content.insert(n, f);
    Ok(wrap(b, r, f))
}

pub(crate) fn handler_get(b: &Rc<Bridge>, r: &mut Realm, t: Value, ty: &str) -> JsResult<Value> {
    let target = if t.is_undefined() {
        super::events::WINDOW
    } else {
        events_js::target_of(b, r, t).ok_or_else(|| type_error(r, "Illegal invocation"))?
    };
    let target = match b.page() {
        Some(p) if target != super::events::WINDOW => handler_target(p, target, ty),
        _ => target,
    };
    // An attribute not compiled yet: compile it now so it reads as a function.
    let src = match b.st(r)?.handlers.get(&(target, String::from(ty))) {
        Some(Handler::Source(s)) => Some(s.clone()),
        _ => None,
    };
    if let Some(s) = src {
        let file = String::from(b.page_or(r)?.page_url());
        if let Ok(f) = r.compile_function(&["event"], &s, &file) {
            let p = r.pin(f);
            if let Some(Handler::Func(old)) = b
                .st(r)?
                .handlers
                .insert((target, String::from(ty)), Handler::Func(p))
            {
                r.unpin(old);
            }
        }
    }
    Ok(events_js::get_handler(b, r, target, ty))
}

pub(crate) fn handler_set(b: &Rc<Bridge>, r: &mut Realm, t: Value, ty: &str, v: Value) -> JsResult<Value> {
    let target = if t.is_undefined() {
        super::events::WINDOW
    } else {
        events_js::target_of(b, r, t).ok_or_else(|| type_error(r, "Illegal invocation"))?
    };
    let target = match b.page() {
        Some(p) if target != super::events::WINDOW => handler_target(p, target, ty),
        _ => target,
    };
    if r.is_callable(v) {
        let p = r.pin(v);
        // Keep its place among the listeners if it had one.
        let had = b.st(r)?.handlers.contains_key(&(target, String::from(ty)));
        if had {
            if let Some(Handler::Func(old)) = b
                .st(r)?
                .handlers
                .insert((target, String::from(ty)), Handler::Func(p))
            {
                r.unpin(old);
            }
        } else {
            events_js::set_handler(b, r, target, ty, Some(Handler::Func(p)));
        }
    } else {
        events_js::set_handler(b, r, target, ty, None);
    }
    Ok(Value::UNDEFINED)
}

/// The `on*` properties on `obj`.
pub fn install_handlers(b: &Rc<Bridge>, r: &mut Realm, obj: Value) -> JsResult<()> {
    for &ty in HANDLER_TYPES {
        let b2 = b.clone();
        let b3 = b.clone();
        let set: alloc::boxed::Box<jsengine::NativeFn> =
            alloc::boxed::Box::new(move |r: &mut Realm, t: Value, a: &[Value]| {
                handler_set(&b3, r, t, ty, arg(a, 0))
            });
        r.define_getter(
            obj,
            &format!("on{ty}"),
            move |r, t, _| handler_get(&b2, r, t, ty),
            Some(set),
        )?;
    }
    Ok(())
}

/// The methods shared by Document, Element and DocumentFragment (ParentNode).
fn parent_node(b: &Rc<Bridge>, r: &mut Realm, o: Value) -> JsResult<()> {
    getter(b, r, o, "children", n_children, None)?;
    getter(b, r, o, "firstElementChild", n_first_elem, None)?;
    getter(b, r, o, "lastElementChild", n_last_elem, None)?;
    getter(b, r, o, "childElementCount", n_child_count, None)?;
    method(b, r, o, "querySelector", 1, e_query_selector)?;
    method(b, r, o, "querySelectorAll", 1, e_query_selector_all)?;
    method(b, r, o, "append", 0, e_append)?;
    method(b, r, o, "prepend", 0, e_prepend)?;
    method(b, r, o, "replaceChildren", 0, e_replace_children)?;
    method(b, r, o, "getElementsByTagName", 1, e_by_tag)?;
    method(b, r, o, "getElementsByClassName", 1, e_by_class)?;
    Ok(())
}

/// Node, Element, HTMLElement, CharacterData, Text, Comment, Document, DocumentFragment:
/// their prototypes' members and the constructor objects.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<()> {
    let pr = b.protos();
    let g = r.global();
    let node = r.pinned(&pr.node);
    getter(b, r, node, "nodeType", n_node_type, None)?;
    getter(b, r, node, "nodeName", n_node_name, None)?;
    getter(b, r, node, "nodeValue", n_node_value, Some(n_set_node_value))?;
    getter(b, r, node, "parentNode", n_parent, None)?;
    getter(b, r, node, "parentElement", n_parent_element, None)?;
    getter(b, r, node, "childNodes", n_child_nodes, None)?;
    getter(b, r, node, "firstChild", n_first_child, None)?;
    getter(b, r, node, "lastChild", n_last_child, None)?;
    getter(b, r, node, "previousSibling", n_prev, None)?;
    getter(b, r, node, "nextSibling", n_next, None)?;
    getter(b, r, node, "ownerDocument", n_owner_document, None)?;
    getter(b, r, node, "isConnected", n_is_connected, None)?;
    getter(b, r, node, "textContent", n_text_content, Some(n_set_text_content))?;
    method(b, r, node, "appendChild", 1, n_append_child)?;
    method(b, r, node, "insertBefore", 2, n_insert_before)?;
    method(b, r, node, "removeChild", 1, n_remove_child)?;
    method(b, r, node, "replaceChild", 2, n_replace_child)?;
    method(b, r, node, "cloneNode", 0, n_clone)?;
    method(b, r, node, "contains", 1, n_contains)?;
    method(b, r, node, "hasChildNodes", 0, n_has_children)?;
    method(b, r, node, "getRootNode", 0, n_root)?;
    method(b, r, node, "isSameNode", 1, n_same)?;
    method(b, r, node, "isEqualNode", 1, n_equal)?;
    method(b, r, node, "compareDocumentPosition", 1, n_compare)?;
    method(b, r, node, "normalize", 0, n_normalize)?;
    for (k, v) in [
        ("ELEMENT_NODE", ELEMENT_NODE),
        ("ATTRIBUTE_NODE", 2),
        ("TEXT_NODE", TEXT_NODE),
        ("CDATA_SECTION_NODE", 4),
        ("PROCESSING_INSTRUCTION_NODE", 7),
        ("COMMENT_NODE", COMMENT_NODE),
        ("DOCUMENT_NODE", DOCUMENT_NODE),
        ("DOCUMENT_TYPE_NODE", DOCUMENT_TYPE_NODE),
        ("DOCUMENT_FRAGMENT_NODE", DOCUMENT_FRAGMENT_NODE),
        ("DOCUMENT_POSITION_DISCONNECTED", 1),
        ("DOCUMENT_POSITION_PRECEDING", 2),
        ("DOCUMENT_POSITION_FOLLOWING", 4),
        ("DOCUMENT_POSITION_CONTAINS", 8),
        ("DOCUMENT_POSITION_CONTAINED_BY", 16),
    ] {
        r.define(node, k, Value::int(v), Attrs::ENUMERABLE)?;
    }

    let el = r.pinned(&pr.element);
    getter(b, r, el, "tagName", e_tag_name, None)?;
    getter(b, r, el, "localName", e_local_name, None)?;
    getter(b, r, el, "namespaceURI", e_namespace, None)?;
    getter(b, r, el, "id", e_id, Some(e_set_id))?;
    getter(b, r, el, "className", e_class_name, Some(e_set_class_name))?;
    getter(b, r, el, "classList", e_class_list, None)?;
    getter(b, r, el, "attributes", e_attributes, None)?;
    getter(b, r, el, "innerHTML", e_inner_html, Some(e_set_inner_html))?;
    getter(b, r, el, "outerHTML", e_outer_html, Some(e_set_outer_html))?;
    getter(b, r, el, "previousElementSibling", n_prev_elem, None)?;
    getter(b, r, el, "nextElementSibling", n_next_elem, None)?;
    getter(b, r, el, "clientWidth", e_client_width, None)?;
    getter(b, r, el, "clientHeight", e_client_height, None)?;
    getter(b, r, el, "clientTop", |_, _, _, _| Ok(Value::int(0)), None)?;
    getter(b, r, el, "clientLeft", |_, _, _, _| Ok(Value::int(0)), None)?;
    getter(b, r, el, "scrollWidth", e_scroll_width, None)?;
    getter(b, r, el, "scrollHeight", e_scroll_height, None)?;
    getter(b, r, el, "scrollTop", e_scroll_top, Some(e_set_scroll_top))?;
    getter(b, r, el, "scrollLeft", e_scroll_left, Some(|_, _, _, _| Ok(Value::UNDEFINED)))?;
    parent_node(b, r, el)?;
    method(b, r, el, "getAttribute", 1, e_get_attribute)?;
    method(b, r, el, "setAttribute", 2, e_set_attribute)?;
    method(b, r, el, "removeAttribute", 1, e_remove_attribute)?;
    method(b, r, el, "hasAttribute", 1, e_has_attribute)?;
    method(b, r, el, "hasAttributes", 0, e_has_attributes)?;
    method(b, r, el, "toggleAttribute", 1, e_toggle_attribute)?;
    method(b, r, el, "getAttributeNames", 0, e_attribute_names)?;
    method(b, r, el, "getAttributeNS", 2, |b, r, t, a| e_get_attribute(b, r, t, &a[a.len().min(1)..]))?;
    method(b, r, el, "setAttributeNS", 3, |b, r, t, a| e_set_attribute(b, r, t, &a[a.len().min(1)..]))?;
    method(b, r, el, "removeAttributeNS", 2, |b, r, t, a| e_remove_attribute(b, r, t, &a[a.len().min(1)..]))?;
    method(b, r, el, "matches", 1, e_matches)?;
    method(b, r, el, "webkitMatchesSelector", 1, e_matches)?;
    method(b, r, el, "closest", 1, e_closest)?;
    method(b, r, el, "remove", 0, e_remove)?;
    method(b, r, el, "before", 0, e_before)?;
    method(b, r, el, "after", 0, e_after)?;
    method(b, r, el, "replaceWith", 0, e_replace_with)?;
    method(b, r, el, "insertAdjacentHTML", 2, e_insert_adjacent_html)?;
    method(b, r, el, "insertAdjacentElement", 2, e_insert_adjacent_element)?;
    method(b, r, el, "insertAdjacentText", 2, e_insert_adjacent_text)?;
    method(b, r, el, "getBoundingClientRect", 0, e_rect)?;
    method(b, r, el, "getClientRects", 0, e_rects)?;
    method(b, r, el, "scrollIntoView", 0, e_scroll_into_view)?;
    method(b, r, el, "scrollTo", 0, |_, _, _, _| Ok(Value::UNDEFINED))?;
    method(b, r, el, "scrollBy", 0, |_, _, _, _| Ok(Value::UNDEFINED))?;

    let he = r.pinned(&pr.html_element);
    getter(b, r, he, "innerText", e_inner_text, Some(e_set_inner_text))?;
    getter(b, r, he, "outerText", e_inner_text, None)?;
    getter(b, r, he, "style", e_style, Some(e_set_style))?;
    getter(b, r, he, "dataset", e_dataset, None)?;
    getter(b, r, he, "offsetWidth", e_offset_width, None)?;
    getter(b, r, he, "offsetHeight", e_offset_height, None)?;
    getter(b, r, he, "offsetTop", e_offset_top, None)?;
    getter(b, r, he, "offsetLeft", e_offset_left, None)?;
    getter(b, r, he, "offsetParent", e_offset_parent, None)?;
    reflect_str(b, r, he, "title", "title")?;
    reflect_str(b, r, he, "lang", "lang")?;
    reflect_str(b, r, he, "dir", "dir")?;
    reflect_str(b, r, he, "accessKey", "accesskey")?;
    reflect_bool(b, r, he, "hidden", "hidden")?;
    reflect_bool(b, r, he, "inert", "inert")?;
    reflect_int(b, r, he, "tabIndex", "tabindex", -1)?;
    method(b, r, he, "focus", 0, e_focus)?;
    method(b, r, he, "blur", 0, e_blur)?;
    method(b, r, he, "click", 0, e_click)?;
    install_handlers(b, r, he)?;

    // Per-tag members.
    for (t, p) in &pr.by_tag {
        let o = r.pinned(p);
        match *t {
            tag::A | tag::AREA => {
                reflect_url(b, r, o, "href", "href")?;
                reflect_str(b, r, o, "target", "target")?;
                reflect_str(b, r, o, "rel", "rel")?;
                reflect_str(b, r, o, "download", "download")?;
                getter(b, r, o, "text", n_text_content, Some(n_set_text_content))?;
                super::window::url_parts(b, r, o)?;
            }
            tag::IMG => {
                reflect_url(b, r, o, "src", "src")?;
                reflect_str(b, r, o, "alt", "alt")?;
                reflect_str(b, r, o, "srcset", "srcset")?;
                getter(b, r, o, "width", img_width, Some(img_set_width))?;
                getter(b, r, o, "height", img_height, Some(img_set_height))?;
                getter(b, r, o, "naturalWidth", img_width, None)?;
                getter(b, r, o, "naturalHeight", img_height, None)?;
                getter(b, r, o, "complete", |_, _, _, _| Ok(Value::TRUE), None)?;
            }
            tag::SCRIPT => {
                reflect_url(b, r, o, "src", "src")?;
                reflect_str(b, r, o, "type", "type")?;
                reflect_bool(b, r, o, "defer", "defer")?;
                reflect_bool(b, r, o, "noModule", "nomodule")?;
                reflect_str(b, r, o, "charset", "charset")?;
                reflect_str(b, r, o, "crossOrigin", "crossorigin")?;
                getter(b, r, o, "async", script_async, Some(script_set_async))?;
                getter(b, r, o, "text", n_text_content, Some(n_set_text_content))?;
            }
            tag::LINK => {
                reflect_url(b, r, o, "href", "href")?;
                reflect_str(b, r, o, "rel", "rel")?;
                reflect_str(b, r, o, "media", "media")?;
                reflect_str(b, r, o, "type", "type")?;
                reflect_str(b, r, o, "as", "as")?;
                reflect_str(b, r, o, "crossOrigin", "crossorigin")?;
                reflect_bool(b, r, o, "disabled", "disabled")?;
            }
            tag::TEMPLATE => {
                getter(b, r, o, "content", d_template_content, None)?;
            }
            tag::DETAILS | tag::DIALOG => {
                reflect_bool(b, r, o, "open", "open")?;
            }
            tag::IFRAME => {
                reflect_url(b, r, o, "src", "src")?;
                reflect_str(b, r, o, "name", "name")?;
                reflect_str(b, r, o, "srcdoc", "srcdoc")?;
                reflect_str(b, r, o, "allow", "allow")?;
                reflect_str(b, r, o, "loading", "loading")?;
                reflect_str(b, r, o, "referrerPolicy", "referrerpolicy")?;
                reflect_str(b, r, o, "width", "width")?;
                reflect_str(b, r, o, "height", "height")?;
                getter(b, r, o, "contentWindow", |b, r, t, _| super::xwin::content_window(b, r, t), None)?;
                getter(b, r, o, "contentDocument", |b, r, t, _| super::xwin::content_document(b, r, t), None)?;
            }
            tag::CANVAS => {
                method(b, r, o, "getContext", 1, |_, _, _, _| Ok(Value::NULL))?;
                reflect_int(b, r, o, "width", "width", 300)?;
                reflect_int(b, r, o, "height", "height", 150)?;
            }
            tag::TD | tag::TH => {
                reflect_int(b, r, o, "colSpan", "colspan", 1)?;
                reflect_int(b, r, o, "rowSpan", "rowspan", 1)?;
            }
            tag::OL => {
                reflect_int(b, r, o, "start", "start", 1)?;
                reflect_bool(b, r, o, "reversed", "reversed")?;
            }
            tag::META => {
                reflect_str(b, r, o, "name", "name")?;
                reflect_str(b, r, o, "content", "content")?;
            }
            _ => {}
        }
    }
    super::forms_js::install(b, r)?;

    let cd = r.pinned(&pr.character_data);
    getter(b, r, cd, "data", c_data, Some(n_set_node_value))?;
    getter(b, r, cd, "length", c_length, None)?;
    getter(b, r, cd, "previousElementSibling", n_prev_elem, None)?;
    getter(b, r, cd, "nextElementSibling", n_next_elem, None)?;
    method(b, r, cd, "appendData", 1, c_append_data)?;
    method(b, r, cd, "remove", 0, e_remove)?;
    method(b, r, cd, "before", 0, e_before)?;
    method(b, r, cd, "after", 0, e_after)?;
    method(b, r, cd, "replaceWith", 0, e_replace_with)?;
    let tx = r.pinned(&pr.text);
    getter(b, r, tx, "wholeText", c_data, None)?;

    let fr = r.pinned(&pr.fragment);
    parent_node(b, r, fr)?;
    method(b, r, fr, "getElementById", 1, d_get_element_by_id)?;

    let d = r.pinned(&pr.document);
    parent_node(b, r, d)?;
    getter(b, r, d, "documentElement", d_document_element, None)?;
    getter(b, r, d, "head", d_head, None)?;
    getter(b, r, d, "body", d_body, None)?;
    getter(b, r, d, "title", d_title, Some(d_set_title))?;
    getter(b, r, d, "URL", d_url, None)?;
    getter(b, r, d, "documentURI", d_url, None)?;
    getter(b, r, d, "readyState", d_ready_state, None)?;
    getter(b, r, d, "cookie", d_cookie, Some(d_set_cookie))?;
    getter(b, r, d, "activeElement", d_active_element, None)?;
    getter(b, r, d, "currentScript", d_current_script, None)?;
    getter(b, r, d, "forms", d_forms, None)?;
    getter(b, r, d, "images", d_images, None)?;
    getter(b, r, d, "links", d_links, None)?;
    getter(b, r, d, "scripts", d_scripts, None)?;
    getter(b, r, d, "doctype", d_doctype, None)?;
    getter(b, r, d, "scrollingElement", d_document_element, None)?;
    getter(b, r, d, "defaultView", |_, r, _, _| Ok(r.global()), None)?;
    getter(b, r, d, "location", |_, r, _, _| { let g = r.global(); r.get(g, "location") }, Some(|_, r, _, a| { let g = r.global(); r.set(g, "location", arg(a, 0))?; Ok(Value::UNDEFINED) }))?;
    getter(b, r, d, "referrer", |_, r, _, _| Ok(r.string("")), None)?;
    getter(b, r, d, "domain", |b, r, _, _| {
        let u = String::from(b.page_or(r)?.page_url());
        let h = super::window::split_url(&u).host;
        Ok(r.string(&h))
    }, None)?;
    getter(b, r, d, "characterSet", |_, r, _, _| Ok(r.string("UTF-8")), None)?;
    getter(b, r, d, "charset", |_, r, _, _| Ok(r.string("UTF-8")), None)?;
    getter(b, r, d, "inputEncoding", |_, r, _, _| Ok(r.string("UTF-8")), None)?;
    getter(b, r, d, "contentType", |_, r, _, _| Ok(r.string("text/html")), None)?;
    getter(b, r, d, "compatMode", |b, r, _, _| {
        let q = b.page_or(r)?.dom.quirks == crate::dom::QuirksMode::Quirks;
        Ok(r.string(if q { "BackCompat" } else { "CSS1Compat" }))
    }, None)?;
    getter(b, r, d, "visibilityState", |_, r, _, _| Ok(r.string("visible")), None)?;
    getter(b, r, d, "hidden", |_, _, _, _| Ok(Value::FALSE), None)?;
    method(b, r, d, "getElementById", 1, d_get_element_by_id)?;
    method(b, r, d, "getElementsByName", 1, d_by_name)?;
    method(b, r, d, "createElement", 1, d_create_element)?;
    method(b, r, d, "createElementNS", 2, d_create_element_ns)?;
    method(b, r, d, "createTextNode", 1, d_create_text)?;
    method(b, r, d, "createComment", 1, d_create_comment)?;
    method(b, r, d, "createDocumentFragment", 0, d_create_fragment)?;
    method(b, r, d, "createEvent", 1, d_create_event)?;
    method(b, r, d, "write", 0, d_write)?;
    method(b, r, d, "writeln", 0, d_writeln)?;
    method(b, r, d, "open", 0, |_, r, t, _| Ok(if t.is_undefined() { r.global() } else { t }))?;
    method(b, r, d, "close", 0, |_, _, _, _| Ok(Value::UNDEFINED))?;
    method(b, r, d, "hasFocus", 0, |_, _, _, _| Ok(Value::TRUE))?;
    method(b, r, d, "elementFromPoint", 2, d_element_from_point)?;
    method(b, r, d, "execCommand", 0, |_, _, _, _| Ok(Value::FALSE))?;
    method(b, r, d, "importNode", 1, |b, r, _, a| {
        let n = arg_node(b, r, a, 0)?;
        let v = wrap(b, r, n);
        n_clone(b, r, v, &a[a.len().min(1)..])
    })?;
    method(b, r, d, "adoptNode", 1, |_, _, _, a| Ok(arg(a, 0)))?;
    install_handlers(b, r, d)?;

    // The constructor objects (for instanceof and the constants).
    let ctors: [(&str, &jsengine::Pinned, Option<&str>); 9] = [
        ("Node", &pr.node, Some("EventTarget")),
        ("Element", &pr.element, Some("Node")),
        ("HTMLElement", &pr.html_element, Some("Element")),
        ("CharacterData", &pr.character_data, Some("Node")),
        ("Text", &pr.text, Some("CharacterData")),
        ("Comment", &pr.comment, Some("CharacterData")),
        ("Document", &pr.document, Some("Node")),
        ("HTMLDocument", &pr.document, Some("Node")),
        ("DocumentFragment", &pr.fragment, Some("Node")),
    ];
    for (name, p, parent) in ctors {
        let pv = r.pinned(p);
        let par = match parent {
            Some(pn) => Some(r.get(g, pn)?),
            None => None,
        };
        let ctor = match name {
            "Text" | "Comment" | "DocumentFragment" => {
                let b2 = b.clone();
                let nm = name;
                Some(alloc::boxed::Box::new(move |r: &mut Realm, _t: Value, a: &[Value]| {
                    let d = wrap(&b2, r, 0);
                    match nm {
                        "Text" => d_create_text(&b2, r, d, if a.is_empty() { &[] } else { a }),
                        "Comment" => d_create_comment(&b2, r, d, a),
                        _ => d_create_fragment(&b2, r, d, a),
                    }
                }) as alloc::boxed::Box<jsengine::NativeFn>)
            }
            _ => None,
        };
        // HTMLDocument shares Document's prototype: only define the name.
        if name == "HTMLDocument" {
            let doc = r.get(g, "Document")?;
            r.define(g, name, doc, Attrs::METHOD)?;
            continue;
        }
        let c = r.new_class(name, 0, pv, par, ctor);
        r.define(g, name, c, Attrs::METHOD)?;
    }
    let nc = r.get(g, "Node")?;
    for (k, v) in [
        ("ELEMENT_NODE", ELEMENT_NODE),
        ("TEXT_NODE", TEXT_NODE),
        ("COMMENT_NODE", COMMENT_NODE),
        ("DOCUMENT_NODE", DOCUMENT_NODE),
        ("DOCUMENT_TYPE_NODE", DOCUMENT_TYPE_NODE),
        ("DOCUMENT_FRAGMENT_NODE", DOCUMENT_FRAGMENT_NODE),
    ] {
        r.define(nc, k, Value::int(v), Attrs::ENUMERABLE)?;
    }
    // HTMLxxxElement constructors.
    let he_ctor = r.get(g, "HTMLElement")?;
    for (t, p) in &pr.by_tag {
        let name = super::window::interface_name(*t);
        let exists = r.has(g, name)?;
        if exists {
            continue;
        }
        let pv = r.pinned(p);
        let c = r.new_class(name, 0, pv, Some(he_ctor), None);
        r.define(g, name, c, Attrs::METHOD)?;
    }
    Ok(())
}

fn img_size(b: &Rc<Bridge>, r: &mut Realm, t: Value, h: bool) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let attr = attr_of(b, r, n, if h { "height" } else { "width" })?
        .and_then(|v| v.trim().parse::<i32>().ok());
    if let Some(v) = attr {
        return Ok(Value::int(v));
    }
    let (_, _, w, hh) = doc_rect(b, n);
    Ok(Value::number(if h { hh } else { w }.rnd()))
}
fn img_width(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    img_size(b, r, t, false)
}
fn img_height(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    img_size(b, r, t, true)
}
fn img_set_width(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let v = r.to_int32(arg(a, 0))?;
    set_attribute(b, r, n, "width", &format!("{v}"))?;
    Ok(Value::UNDEFINED)
}
fn img_set_height(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let v = r.to_int32(arg(a, 0))?;
    set_attribute(b, r, n, "height", &format!("{v}"))?;
    Ok(Value::UNDEFINED)
}
fn script_async(b: &Rc<Bridge>, r: &mut Realm, t: Value, _: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    let has = attr_of(b, r, n, "async")?.is_some();
    let ordered = b.st(r)?.ordered.contains(&n);
    // A script made by a page is async unless it was set false ("force async").
    let parser = b.st(r)?.loader.is_started(n) && !ordered;
    Ok(Value::bool(has || (!ordered && !parser)))
}
fn script_set_async(b: &Rc<Bridge>, r: &mut Realm, t: Value, a: &[Value]) -> JsResult<Value> {
    let n = this_elem(b, r, t)?;
    if arg_bool(r, a, 0) {
        b.st(r)?.ordered.retain(|&o| o != n);
        set_attribute(b, r, n, "async", "")?;
    } else {
        remove_attribute(b, r, n, "async")?;
        let mut st = b.st(r)?;
        if !st.ordered.contains(&n) && st.ordered.len() < 10_000 {
            st.ordered.push(n);
        }
    }
    Ok(Value::UNDEFINED)
}
