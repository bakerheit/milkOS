//! Scripts in pages: the bindings between the DOM and the JavaScript engine (user/jsengine,
//! through its `api.rs`), and the engine-independent parts they use: which scripts run when
//! ([`loader`]), event listeners and propagation ([`events`]), timers and animation frames
//! ([`timers`]).
//!
//! A [`Runtime`] is one page's scripts: one realm (heap, global object = `window`), with the
//! DOM bound into it ([`nodes`], [`events_js`], [`forms_js`], [`window`]). It is driven by the
//! [`crate::Document`] calls in api.rs. Every call from the app is one "entry": the page and
//! the app's host are made reachable to the natives ([`bridge`]), the script work runs, the
//! microtasks are drained (the checkpoint), and the entry ends. A script that runs past
//! [`ScriptLimits::slow_ms`] makes the app hear about it every 100 ms
//! ([`ScriptHost::slow`]); if the app says stop, or the heap budget runs out, the page's
//! scripts end for good: timers and listeners go, nothing runs again, the DOM stays.

pub mod events;
pub mod loader;
pub mod timers;

mod bridge;
mod events_js;
mod forms_js;
mod net;
mod netbytes;
mod nodes;
mod window;

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{Exception, Options, Realm, Value};

use crate::api::*;
use crate::dom::{NONE, NodeId};
use crate::page::Page;
use bridge::Bridge;
use loader::{Body, Loader, Phase};

pub struct Runtime {
    b: Rc<Bridge>,
    /// Made at the first entry (it needs the host's time zone).
    realm: Option<Realm>,
    limits: ScriptLimits,
}

/// Script work done in one [`Runtime::run`] before it gives the app a turn (timers).
const MAX_TIMERS_PER_RUN: usize = 1000;
const RUN_SLICE_MS: f64 = 50.0;

impl Runtime {
    pub fn new(page: &Page, limits: ScriptLimits) -> Runtime {
        let base = String::from(page.base_url());
        let resolve = move |h: &str| crate::url::resolve(&base, h);
        let loader = Loader::collect(&page.dom, &resolve);
        Runtime {
            b: Rc::new(Bridge::new(limits, loader)),
            realm: None,
            limits,
        }
    }

    pub fn stopped(&self) -> bool {
        self.b.st.borrow().stopped
    }

    pub fn pending(&self) -> Vec<String> {
        let st = self.b.st.borrow();
        if st.stopped {
            return Vec::new();
        }
        st.loader.pending()
    }

    pub fn supply(&mut self, url: &str, body: Option<&[u8]>) {
        let text = body.map(|b| {
            let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
            let b = &b[..b.len().min(self.limits.max_source)];
            match core::str::from_utf8(b) {
                Ok(s) => String::from(s),
                Err(_) => b.iter().map(|&x| crate::charset::windows1252(x)).collect(),
            }
        });
        self.b.st.borrow_mut().loader.supply(url, text.as_deref());
    }

    pub fn next_wake(&self) -> Option<f64> {
        let st = self.b.st.borrow();
        if st.stopped {
            return None;
        }
        st.timers.next_due()
    }

    pub fn wants_frame(&self) -> bool {
        let st = self.b.st.borrow();
        !st.stopped && st.timers.wants_frame()
    }

    pub fn take_requests(&mut self) -> Vec<PageRequest> {
        core::mem::take(&mut self.b.st.borrow_mut().requests)
    }

    pub fn take_net(&mut self) -> Vec<NetOut> {
        core::mem::take(&mut self.b.st.borrow_mut().net.out)
    }

    pub fn net_pending(&self) -> usize {
        self.b.st.borrow().net.pending()
    }

    /// An answer for request `id`: a task of its own.
    pub fn net_event(
        &mut self,
        page: &mut Page,
        host: &mut dyn ScriptHost,
        id: u32,
        ev: NetEvent,
    ) -> bool {
        self.entry(page, host, |b, r| {
            net::deliver(b, r, id, ev);
            true
        })
        .unwrap_or(false)
    }

    pub fn take_console(&mut self) -> Vec<ConsoleLine> {
        let mut st = self.b.st.borrow_mut();
        st.console_bytes = 0;
        core::mem::take(&mut st.console)
    }

    pub fn stats(&self) -> ScriptStats {
        let mut s = self.b.st.borrow().stats;
        if let Some(r) = &self.realm {
            s.heap_bytes = r.heap_stats().used_bytes;
        }
        s.stopped = self.b.st.borrow().stopped;
        s
    }

    /// End the page's scripts for good.
    pub fn stop(&mut self, why: &str) {
        let mut st = self.b.st.borrow_mut();
        if !st.stopped {
            st.stopped = true;
            st.stop_reason = String::from(why);
        }
        drop(st);
        self.teardown();
    }

    fn teardown(&mut self) {
        let mut st = self.b.st.borrow_mut();
        st.timers.clear_all();
        st.listeners.clear();
        st.handlers.clear();
        let cbs = core::mem::take(&mut st.cbs);
        let net_pins = st.net.clear();
        let reason = st.stop_reason.clone();
        let already = st.stats.stopped;
        st.stats.stopped = true;
        drop(st);
        if !already {
            self.b.console(
                ConsoleLevel::Warn,
                format!("The page's scripts were stopped ({reason})."),
            );
        }
        if let Some(r) = self.realm.as_mut() {
            r.clear_jobs();
            r.set_interrupt(None);
            for p in net_pins {
                r.unpin(p);
            }
            for (_, (f, a)) in cbs {
                r.unpin(f);
                if let Some(a) = a {
                    r.unpin(a);
                }
            }
            r.release_temps();
        }
    }

    /// One entry from the app: `f` runs with the page and host reachable, then the
    /// microtask checkpoint. None if the scripts are stopped (or the realm can't be made).
    fn entry<R>(
        &mut self,
        page: &mut Page,
        host: &mut dyn ScriptHost,
        f: impl FnOnce(&Rc<Bridge>, &mut Realm) -> R,
    ) -> Option<R> {
        if self.b.st.borrow().stopped {
            return None;
        }
        if self.realm.is_none() {
            let mut r = Realm::new(Options {
                limits: jsengine::Limits {
                    max_heap_bytes: self.limits.memory,
                    max_steps: 0,
                    max_call_depth: 400,
                    max_source_bytes: self.limits.max_source,
                },
                tz_offset_minutes: host.tz_offset(),
                random_seed: host.time().to_bits() ^ 0x6d69_6c6b,
            });
            let _g = self.b.enter(page, host);
            window::install(&self.b, &mut r);
            drop(_g);
            self.realm = Some(r);
        }
        let realm = self.realm.as_mut().unwrap();
        let b = self.b.clone();
        let out = {
            let _g = b.enter(page, host);
            let now = b.host().map_or(0.0, |h| h.now());
            {
                let mut st = b.st.borrow_mut();
                if st.depth == 0 {
                    st.entry_start = now;
                    st.slow_asked = now;
                }
                st.depth += 1;
            }
            let out = f(&b, realm);
            checkpoint(&b, realm);
            let mut st = b.st.borrow_mut();
            st.depth -= 1;
            let top = st.depth == 0;
            drop(st);
            if top {
                realm.release_temps();
                // Byte buffers the network API made weigh nothing in the script heap: collect
                // now and then so the dead ones are given back.
                let want = b.bytes.get().is_some_and(|x| x.store.borrow_mut().want_gc());
                if want {
                    realm.gc();
                }
            }
            out
        };
        if self.b.st.borrow().stopped {
            self.teardown();
        }
        Some(out)
    }

    /// Scripts whose turn it is, the readyState steps, DOMContentLoaded and load, and the
    /// timers due.
    pub fn run(&mut self, page: &mut Page, host: &mut dyn ScriptHost) -> bool {
        let start = host.now();
        let pending_resources = !page.pending_resources().is_empty();
        self.entry(page, host, |b, r| {
            let mut did = false;
            // Module scripts found so far: a note each (they don't run yet).
            let mods = core::mem::take(&mut b.st.borrow_mut().loader.modules);
            for m in mods {
                let what = b
                    .page()
                    .and_then(|p| p.dom.element(m).and_then(|e| e.attr("src")).map(|s| p.resolve(s)))
                    .unwrap_or_else(|| String::from("an inline module"));
                b.console(
                    ConsoleLevel::Warn,
                    format!("A module script ({what}) was not run: modules aren't supported yet"),
                );
            }
            // Scripts, phase by phase.
            loop {
                let next = b.st.borrow().loader.next();
                if let Some(i) = next {
                    run_script(b, r, i, true);
                    checkpoint(b, r);
                    did = true;
                    if b.st.borrow().stopped {
                        return did;
                    }
                    continue;
                }
                let adv = b.st.borrow_mut().loader.advance();
                match adv {
                    Some(Phase::Interactive) => {
                        events_js::fire_simple(b, r, 0, "readystatechange", false, false);
                        did = true;
                    }
                    Some(Phase::ContentLoaded) => {
                        b.st.borrow_mut().content_loaded = true;
                        events_js::fire_simple(b, r, 0, "DOMContentLoaded", true, false);
                        did = true;
                    }
                    _ => break,
                }
                checkpoint(b, r);
            }
            // load: once the scripts are done and nothing else is coming.
            let ready = {
                let st = b.st.borrow();
                st.content_loaded && !st.loaded && st.loader.unfinished() == 0
            };
            if ready && !pending_resources {
                {
                    let mut st = b.st.borrow_mut();
                    st.loaded = true;
                    st.loader.phase = Phase::Complete;
                }
                events_js::fire_simple(b, r, 0, "readystatechange", false, false);
                checkpoint(b, r);
                events_js::fire_simple(b, r, events::WINDOW, "load", false, false);
                checkpoint(b, r);
                did = true;
            }
            // Timers due now (not ones they set for "now" again: those wait for the next run).
            let now = b.host().map_or(start, |h| h.now());
            for _ in 0..MAX_TIMERS_PER_RUN {
                let t = b.st.borrow_mut().timers.take_due(now);
                let Some(t) = t else { break };
                did = true;
                run_timer(b, r, t);
                checkpoint(b, r);
                if b.st.borrow().stopped {
                    break;
                }
                if b.host().map_or(0.0, |h| h.now()) - start > RUN_SLICE_MS {
                    break;
                }
            }
            did
        })
        .unwrap_or(false)
    }

    /// requestAnimationFrame callbacks.
    pub fn frame(&mut self, page: &mut Page, host: &mut dyn ScriptHost) -> bool {
        self.entry(page, host, |b, r| {
            let cbs = b.st.borrow_mut().timers.take_frame();
            let now = b.host().map_or(0.0, |h| h.now());
            let any = !cbs.is_empty();
            for (_, h) in cbs {
                if let Some((f, _)) = b.cb(r, h) {
                    b.st.borrow_mut().stats.callbacks += 1;
                    call_report(b, r, f, Value::UNDEFINED, &[Value::number(now)]);
                }
                b.drop_cb(r, h);
                if b.st.borrow().stopped {
                    break;
                }
            }
            any
        })
        .unwrap_or(false)
    }

    pub fn key(
        &mut self,
        page: &mut Page,
        host: &mut dyn ScriptHost,
        key: &KeyInput,
        up: bool,
    ) -> bool {
        let target = key_target(page);
        self.entry(page, host, |b, r| {
            let ty = if up { "keyup" } else { "keydown" };
            let mut ok = events_js::fire_key(b, r, target, ty, key);
            // keypress for keys that make a character (legacy, but pages still use it).
            if !up && ok && key.key.chars().count() == 1 && !key.mods.ctrl && !key.mods.meta {
                ok = events_js::fire_key(b, r, target, "keypress", key);
            }
            ok
        })
        .unwrap_or(true)
    }

    /// Move the focus to `node` (None: nowhere), with the events.
    pub fn focus(&mut self, page: &mut Page, host: &mut dyn ScriptHost, node: Option<NodeId>) {
        let to = node.unwrap_or(NONE);
        if page.focus == to {
            return;
        }
        if self
            .entry(page, host, |b, r| events_js::move_focus(b, r, to))
            .is_none()
        {
            page.set_focus_node(node);
        }
    }

    /// The user changed a control: `input` (and `change` for the kinds that commit at once).
    pub fn user_input(&mut self, page: &mut Page, host: &mut dyn ScriptHost, node: NodeId) {
        let commits = page
            .control_of_node(node)
            .and_then(|id| page.controls().get(id.0 as usize))
            .is_some_and(|c| {
                matches!(
                    c.kind,
                    ControlKind::Select | ControlKind::Checkbox | ControlKind::Radio
                )
            });
        self.entry(page, host, |b, r| {
            events_js::fire_input(b, r, node, "input");
            if commits {
                events_js::fire_simple(b, r, node, "change", true, false);
            }
        });
    }

    /// Validation, then the `submit` event at `form` (the submitter `id`): false if the
    /// form is invalid or a listener cancelled it.
    pub fn submit_event(
        &mut self,
        page: &mut Page,
        host: &mut dyn ScriptHost,
        form: NodeId,
        id: ControlId,
    ) -> bool {
        let submitter = page.control_node(id).unwrap_or(NONE);
        self.entry(page, host, |b, r| {
            forms_js::validate_form(b, r, form, submitter)
                && events_js::fire_submit(b, r, form, submitter)
        })
        .unwrap_or(true)
    }

    /// Test hook: run `f` as an entry (the realm and bridge at hand).
    #[cfg(test)]
    pub(crate) fn test_entry<R>(
        &mut self,
        page: &mut Page,
        host: &mut dyn ScriptHost,
        f: impl FnOnce(&Rc<Bridge>, &mut Realm) -> R,
    ) -> Option<R> {
        self.entry(page, host, f)
    }

    /// mousedown, mouseup, click at `target`; the checkbox dance around it. False if the
    /// click was cancelled.
    pub fn click(
        &mut self,
        page: &mut Page,
        host: &mut dyn ScriptHost,
        target: NodeId,
        pos: (i32, i32),
        mods: Mods,
    ) -> Option<bool> {
        self.entry(page, host, |b, r| events_js::user_click(b, r, target, pos, mods))
    }
}

/// Where key events go: the focused element, else the body (else the document).
fn key_target(page: &Page) -> NodeId {
    if page.focus != NONE && page.dom.is_connected(page.focus) {
        return page.focus;
    }
    let b = page.dom.body();
    if b != NONE { b } else { 0 }
}

/// The microtask checkpoint: run jobs until none are left (reporting what they throw), then
/// report promise rejections nobody handled.
fn checkpoint(b: &Rc<Bridge>, r: &mut Realm) {
    if b.st.borrow().depth > 1 {
        return;
    }
    for _ in 0..1000 {
        match r.run_jobs() {
            Ok(()) => break,
            Err(e) => {
                report(b, r, &e, "");
                if e.is_uncatchable() {
                    break;
                }
            }
        }
    }
    for v in r.take_unhandled_rejections() {
        let rep = r.report_value(v);
        b.console(ConsoleLevel::Error, format!("Uncaught (in promise) {rep}"));
        if let Ok(mut st) = b.st.try_borrow_mut() {
            st.stats.errors += 1;
        }
    }
}

/// An exception reached the top: to the console (and an uncatchable one ends the scripts).
pub(crate) fn report(b: &Bridge, r: &mut Realm, e: &Exception, what: &str) {
    let rep = r.report(e);
    let text = if what.is_empty() {
        format!("Uncaught {rep}")
    } else {
        format!("Uncaught {rep} (in {what})")
    };
    b.console(ConsoleLevel::Error, text);
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.stats.errors += 1;
        if rep.uncatchable && !st.stopped {
            st.stopped = true;
            st.stop_reason = String::from(match e.termination() {
                Some(jsengine::Termination::OutOfMemory) => "out of memory",
                Some(jsengine::Termination::Interrupted) => "it was too slow",
                _ => "it ran too long",
            });
        }
    }
}

/// Call `f`, reporting what it throws; None if it threw.
pub(crate) fn call_report(
    b: &Bridge,
    r: &mut Realm,
    f: Value,
    this: Value,
    args: &[Value],
) -> Option<Value> {
    if b.st.borrow().stopped {
        return None;
    }
    match r.call(f, this, args) {
        Ok(v) => Some(v),
        Err(e) => {
            report(b, r, &e, "");
            None
        }
    }
}

/// Run (or fail) script `i` of the loader. `top`: from the event loop (not from inside a
/// native, where the microtask checkpoint must wait).
pub(crate) fn run_script(b: &Rc<Bridge>, r: &mut Realm, i: usize, top: bool) {
    let (node, src, body) = {
        let mut st = b.st.borrow_mut();
        let s = &mut st.loader.scripts[i];
        s.done = true;
        let body = core::mem::replace(&mut s.body, Body::Failed);
        (s.node, s.src.clone(), body)
    };
    match body {
        Body::Ready(text) => {
            let file = src.clone().unwrap_or_else(|| {
                b.page()
                    .map(|p| String::from(p.page_url()))
                    .unwrap_or_default()
            });
            let prev = core::mem::replace(&mut b.st.borrow_mut().current_script, node);
            b.st.borrow_mut().stats.scripts += 1;
            if let Err(e) = r.eval(&text, &file) {
                report(b, r, &e, "");
            }
            b.st.borrow_mut().current_script = prev;
            if src.is_some() {
                if top {
                    checkpoint(b, r);
                }
                events_js::fire_simple(b, r, node, "load", false, false);
            }
        }
        Body::Failed | Body::Waiting => {
            if let Some(u) = &src {
                b.console(ConsoleLevel::Error, format!("Failed to load script {u}"));
            }
            events_js::fire_simple(b, r, node, "error", false, false);
        }
    }
}

fn run_timer(b: &Rc<Bridge>, r: &mut Realm, t: timers::Timer<u32>) {
    let Some((f, args)) = b.cb(r, t.cb) else {
        return;
    };
    if t.interval.is_none() {
        // A timeout runs once: its handle goes now (the callback can't clear it any more).
        // (Keep the values alive for the call: they are temporaries of this entry.)
        b.drop_cb(r, t.cb);
    }
    b.st.borrow_mut().stats.callbacks += 1;
    let prev = core::mem::replace(&mut b.st.borrow_mut().timers.nesting, t.nesting);
    let mut argv: Vec<Value> = Vec::new();
    if let Some(a) = args {
        let n = r
            .get(a, "length")
            .ok()
            .and_then(|v| v.as_number())
            .unwrap_or(0.0)
            .clamp(0.0, 64.0) as u32;
        for k in 0..n {
            argv.push(r.get(a, k).unwrap_or(Value::UNDEFINED));
        }
    }
    if r.is_callable(f) {
        let g = r.global();
        call_report(b, r, f, g, &argv);
    } else if let Some(src) = r.get_string(f) {
        // setTimeout("code", ms).
        if let Err(e) = r.eval(&src, "setTimeout") {
            report(b, r, &e, "");
        }
    }
    b.st.borrow_mut().timers.nesting = prev;
}

/// The app's click at viewport (x, y): find the element, then the events (and with scripts
/// off, just the checkbox toggle).
pub fn click(
    d: &mut Document,
    host: &mut dyn ScriptHost,
    x: i32,
    y: i32,
    scroll: (i32, i32),
    mods: Mods,
) -> bool {
    let page = &mut d.page;
    let mut target = page.node_at(x, y, scroll);
    // Events go to elements, not text.
    while target != NONE && page.dom.element(target).is_none() {
        target = page.dom.parent(target);
    }
    if target == NONE {
        target = page.dom.body();
    }
    if target == NONE {
        return true;
    }
    let pos = (x, y);
    match d.js.as_mut() {
        Some(js) if !js.stopped() => {
            if let Some(ok) = js.click(page, host, target, pos, mods) {
                return ok;
            }
            nodes_toggle(page, target)
        }
        _ => nodes_toggle(page, target),
    }
}

/// Without scripts: a click on a checkbox or radio button (or its label) toggles it.
fn nodes_toggle(page: &mut Page, target: NodeId) -> bool {
    if let Some(n) = events_js::toggle_target(page, target)
        && let Some(id) = page.control_of_node(n)
    {
        let c = &page.controls()[id.0 as usize];
        if !c.disabled {
            let on = match c.kind {
                ControlKind::Checkbox => !c.checked,
                _ => true,
            };
            page.set_checked(id, on);
        }
    }
    true
}

/// Without scripts: the first control of `form` that fails the basic constraints (required,
/// type, length, range), and why. (`novalidate` and `formnovalidate` are honoured.)
pub fn first_invalid(page: &mut Page, form: NodeId, submitter: ControlId) -> Option<(NodeId, String)> {
    let skip = page.dom.element(form).is_some_and(|e| e.has_attr("novalidate"))
        || page
            .control_node(submitter)
            .and_then(|n| page.dom.element(n))
            .is_some_and(|e| e.has_attr("formnovalidate"));
    if skip {
        return None;
    }
    page.sync_forms();
    let list = nodes::list_nodes(page, &bridge::ListKind::FormElements(form));
    for n in list {
        if let Some(m) = forms_js::basic_invalid(page, n) {
            return Some((n, m));
        }
    }
    None
}
