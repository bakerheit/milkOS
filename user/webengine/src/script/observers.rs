//! MutationObserver, the host side: which nodes are watched, and the records the DOM made for
//! them. The observers themselves (options, filters, queues, callbacks) are script
//! (`js/observers.js`); this hands them the records at the microtask checkpoint, the way
//! browsers do, and gives back the nodes whose freeing waited for the records.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::bridge::{Bridge, arg, arg_num};
use super::nodes;
use jsengine::{JsResult, Realm, Value};
use crate::dom::MutRec;

/// Most nodes watched at once.
const MAX_WATCHED: usize = 8192;
/// Most rounds of delivery in one checkpoint (callbacks that change what they watch).
const MAX_ROUNDS: usize = 50;

pub fn define_natives(b: &Rc<Bridge>, r: &mut Realm, n: Value) -> JsResult<()> {
    // moWatch(node, deep, delta) -> bool: one more (delta 1) or fewer (-1) observers on the node
    // (false: refused, too many nodes are watched).
    let b2 = b.clone();
    r.define_method(n, "moWatch", 3, move |r, _t, a| {
        let Some(id) = nodes::node_of(&b2, r, arg(a, 0)) else {
            return Ok(Value::FALSE);
        };
        let deep = r.to_boolean(arg(a, 1));
        let delta = arg_num(r, a, 2)? as i32;
        let Some(p) = b2.page() else {
            return Ok(Value::FALSE);
        };
        let w = &mut p.dom.mlog.watch;
        if delta > 0 {
            if !w.contains_key(&id) && w.len() >= MAX_WATCHED {
                return Ok(Value::FALSE);
            }
            let e = w.entry(id).or_insert((0, 0));
            if deep {
                e.1 += 1;
            } else {
                e.0 += 1;
            }
        } else if let Some(e) = w.get_mut(&id) {
            if deep {
                e.1 = e.1.saturating_sub(1);
            } else {
                e.0 = e.0.saturating_sub(1);
            }
            if *e == (0, 0) {
                w.remove(&id);
            }
        }
        if w.is_empty() {
            // Nobody is watching: the records have no readers.
            p.dom.take_mutations();
        }
        Ok(Value::TRUE)
    })?;
    // moTake() -> [[kind, target, added, removed, prev, next, name, old, ancestors], ...]
    // (kind 0: children, 1: attribute, 2: character data)
    let b2 = b.clone();
    r.define_method(n, "moTake", 0, move |r, _t, _a| {
        let recs = match b2.page() {
            Some(p) => p.dom.take_mutations(),
            None => Vec::new(),
        };
        let mut out = Vec::with_capacity(recs.len());
        for rec in recs {
            let item = match rec {
                MutRec::Child { target, added, removed, prev, next, anc } => {
                    let a = ancestors(&b2, r, &anc);
                    let vals = [
                        Value::number(0.0),
                        nodes::wrap(&b2, r, target),
                        nodes::wrap(&b2, r, added),
                        nodes::wrap(&b2, r, removed),
                        nodes::wrap(&b2, r, prev),
                        nodes::wrap(&b2, r, next),
                        Value::NULL,
                        Value::NULL,
                        a,
                    ];
                    r.new_array(&vals)
                }
                MutRec::Attr { target, name, old, anc } => {
                    let a = ancestors(&b2, r, &anc);
                    let nm = r.string(&name);
                    let ov = match old {
                        Some(o) => r.string(&o),
                        None => Value::NULL,
                    };
                    let vals = [
                        Value::number(1.0),
                        nodes::wrap(&b2, r, target),
                        Value::NULL,
                        Value::NULL,
                        Value::NULL,
                        Value::NULL,
                        nm,
                        ov,
                        a,
                    ];
                    r.new_array(&vals)
                }
                MutRec::Data { target, old, anc } => {
                    let a = ancestors(&b2, r, &anc);
                    let ov = r.string(&old);
                    let vals = [
                        Value::number(2.0),
                        nodes::wrap(&b2, r, target),
                        Value::NULL,
                        Value::NULL,
                        Value::NULL,
                        Value::NULL,
                        Value::NULL,
                        ov,
                        a,
                    ];
                    r.new_array(&vals)
                }
            };
            out.push(item);
        }
        Ok(r.new_array(&out))
    })?;
    Ok(())
}

fn ancestors(b: &Rc<Bridge>, r: &mut Realm, anc: &[u32]) -> Value {
    let v: Vec<Value> = anc.iter().map(|&n| nodes::wrap(b, r, n)).collect();
    r.new_array(&v)
}

/// At the microtask checkpoint: the records the DOM made go to the observers. True if any
/// callback ran (more jobs may be waiting).
pub fn deliver(b: &Rc<Bridge>, r: &mut Realm) -> bool {
    let mut did = false;
    for _ in 0..MAX_ROUNDS {
        let pending = b.page().is_some_and(|p| !p.dom.mlog.recs.is_empty());
        if !pending {
            break;
        }
        let Ok(ex) = super::net::exports(b, r) else { break };
        let Ok(f) = r.get(ex, "deliverMutations") else { break };
        super::call_report(b, r, f, Value::UNDEFINED, &[]);
        did = true;
        // The callbacks' own jobs run before the next round.
        super::run_jobs_reporting(b, r);
        if b.st.borrow().stopped {
            break;
        }
    }
    // What was left waiting for the records goes back now (unless a script saw it meanwhile).
    let freed: Vec<u32> = match b.page() {
        Some(p) if p.dom.mlog.recs.is_empty() => p.dom.take_deferred(),
        _ => Vec::new(),
    };
    if !freed.is_empty() {
        nodes::free_unseen(b, &freed);
    }
    did
}

// ---- layout observers (IntersectionObserver, ResizeObserver) ------------------------------------

/// How often the host looks for a scroll or a resize while those observers exist.
const POLL_MS: f64 = 100.0;

/// When the boxes of the observed elements are looked at again: after the DOM changed, or the
/// view scrolled or was resized (seen by a light poll), at the next frame.
#[derive(Default)]
pub struct LayoutWatch {
    pub on: bool,
    pub dirty: bool,
    generation: u64,
    poll_at: f64,
    view: (i32, i32, i32, i32, u16),
}

pub fn define_layout_natives(b: &Rc<Bridge>, r: &mut Realm, n: Value) -> JsResult<()> {
    // layoutWatch(on): the layout observers have (or no longer have) targets.
    let b2 = b.clone();
    r.define_method(n, "layoutWatch", 1, move |r, _t, a| {
        let on = r.to_boolean(arg(a, 0));
        let now = b2.host().map_or(0.0, |h| h.now());
        let mut st = b2.st.borrow_mut();
        st.watch.on = on;
        if on {
            st.watch.dirty = true;
            st.watch.poll_at = now + POLL_MS;
        } else {
            st.watch.dirty = false;
        }
        Ok(Value::UNDEFINED)
    })?;
    Ok(())
}

/// At the end of an entry into script: did the DOM change?
pub fn note_entry_end(b: &Bridge) {
    let Some(p) = b.page() else { return };
    let Ok(mut st) = b.st.try_borrow_mut() else { return };
    if st.watch.on && !st.watch.dirty && p.dom.generation != st.watch.generation {
        st.watch.dirty = true;
    }
}

/// The light poll for scrolling and resizing (called from `run`).
pub fn poll(b: &Bridge) {
    let Some(h) = b.host() else { return };
    let now = h.now();
    let Ok(mut st) = b.st.try_borrow_mut() else { return };
    if !st.watch.on || now < st.watch.poll_at {
        return;
    }
    st.watch.poll_at = now + POLL_MS;
    let (sx, sy) = h.scroll();
    let v = h.viewport();
    let view = (sx, sy, v.width, v.height, v.zoom);
    if view != st.watch.view {
        st.watch.view = view;
        st.watch.dirty = true;
    }
}

/// When the poll is next due (the host's clock).
pub fn next_poll(b: &Bridge) -> Option<f64> {
    let st = b.st.borrow();
    st.watch.on.then_some(st.watch.poll_at)
}

/// A frame is about to be drawn: look at the observed boxes if they may have moved.
pub fn check_layout(b: &Rc<Bridge>, r: &mut Realm) -> bool {
    {
        let mut st = b.st.borrow_mut();
        if !st.watch.on || !st.watch.dirty {
            return false;
        }
        st.watch.dirty = false;
    }
    if let Some(p) = b.page() {
        let g = p.dom.generation;
        b.st.borrow_mut().watch.generation = g;
    }
    let Ok(ex) = super::net::exports(b, r) else { return false };
    let Ok(f) = r.get(ex, "checkLayout") else { return false };
    super::call_report(b, r, f, Value::UNDEFINED, &[]);
    true
}
