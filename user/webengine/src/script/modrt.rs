//! Modules in a page: the glue between the module map ([`super::modules`], which knows
//! nothing of script or network), the engine's module API, the page's network path
//! (`NetOut`/`NetEvent`, so a module is fetched with mode cors and the policy's rules apply)
//! and the loader (which says when a `<script type=module>` runs).
//!
//! ```text
//!   loader.work ──start_work──▶ Modules (map, import map, graphs) ──Action::Fetch──▶ NetOut::Start
//!                                  ▲                                                     │
//!                                  └──── deliver(NetEvent: head, body, failure) ◀────────┘
//!   graph ready ──pump──▶ a script: the loader's body becomes ready, run_script runs it
//!                         an import(): finish_dynamic_import, the engine links and evaluates
//! ```
//!
//! The engine does no I/O; every module fetch is a request the app runs. A graph is settled
//! (in [`pump`]) at the end of a task, where script may run; starting work only compiles and
//! asks for fetches, so it is safe inside a native call (a script inserting a script element).

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{
    Attrs, DynamicImport, ErrorKind, ErrorReport, Exception, ModuleId, ModuleOptions, Pinned,
    Realm, Value,
};

use super::bridge::{Bridge, arg, arg_str, type_error};
use super::loader::{Body, ModWork};
use super::modules::{
    Action, Broken, Engine, Key, ModType, Modules, Outcome, Request, Types, Verdict,
    type_of_attributes,
};
use crate::api::*;

/// `import()` calls (and script graphs) that may wait at once.
const MAX_PENDING_IMPORTS: usize = 4_000;

/// An error the page will see, kept until it is reported or thrown into a promise.
pub enum ErrVal {
    Thrown(Pinned),
    /// The engine stopped the script (budget, memory): uncatchable.
    Term(Exception),
}

/// The engine's types, for the module map.
pub struct Real;

impl Types for Real {
    type Id = ModuleId;
    type Err = ErrVal;
}

/// Why a script element's graph ended, kept until the loader runs the script.
pub enum ScriptResult {
    /// Link and evaluate this root.
    Run(ModuleId),
    /// A module in the graph is broken: report it, run nothing.
    Broken(Key),
    /// A fetch failed (the console has said why): an `error` event.
    Null,
    /// The engine would not take the dependencies.
    Engine(ErrVal),
}

enum Waiter {
    Script(usize),
    Dynamic(jsengine::DynamicImportId),
    Preload,
}

/// A module request on the wire.
pub struct Fetch {
    pub key: Key,
    /// The address the response came from, once its head is in.
    pub url: String,
}

#[derive(Default)]
pub struct ModRt {
    pub mods: Modules<Real>,
    waiters: BTreeMap<u32, Waiter>,
    pub results: BTreeMap<usize, ScriptResult>,
    /// `JSON.parse` as it was before any page script ran (for JSON modules).
    json_parse: Option<Pinned>,
    /// A script's graph finished: the loader has something to run.
    pub wake: bool,
    pumping: bool,
}

/// The engine, for the module map's calls.
struct RealmEngine<'a> {
    r: &'a mut Realm,
    json_parse: Option<Value>,
}

fn err_val(r: &mut Realm, e: Exception) -> ErrVal {
    match e.value() {
        Some(v) => ErrVal::Thrown(r.pin(v)),
        None => ErrVal::Term(e),
    }
}

impl Engine<Real> for RealmEngine<'_> {
    fn compile(
        &mut self,
        ty: ModType,
        source: &str,
        file: &str,
        first_line: u32,
    ) -> Result<ModuleId, ErrVal> {
        match ty {
            ModType::Js => {
                let opts = ModuleOptions {
                    file,
                    first_line,
                    first_column: 1,
                };
                self.r.compile_module(source, &opts).map_err(|e| err_val(self.r, e))
            }
            ModType::Json => {
                let parse = match self.json_parse {
                    Some(p) => p,
                    None => {
                        let e = self.r.throw_error(ErrorKind::Error, "JSON modules are not available");
                        return Err(err_val(self.r, e));
                    }
                };
                let text = self.r.string(source);
                let v = self
                    .r
                    .call(parse, Value::UNDEFINED, &[text])
                    .map_err(|e| err_val(self.r, e))?;
                self.r
                    .compile_synthetic_module(file, &[("default", v)])
                    .map_err(|e| err_val(self.r, e))
            }
        }
    }

    fn requests(&mut self, id: ModuleId) -> Vec<Request> {
        self.r
            .module_requests(id)
            .into_iter()
            .map(|q| Request {
                specifier: q.specifier,
                attributes: q.attributes,
                line: q.line,
                column: q.column,
            })
            .collect()
    }

    fn set_dependency(&mut self, id: ModuleId, index: usize, dep: ModuleId) -> Result<(), ErrVal> {
        self.r
            .set_module_dependency(id, index, dep)
            .map_err(|e| err_val(self.r, e))
    }
}

/// Run `f` on the module map with the engine, the state borrowed for the length of the call.
/// (The engine calls made here compile and read; none runs page script or reaches a hook.)
fn with_mods<R>(
    b: &Rc<Bridge>,
    r: &mut Realm,
    f: impl FnOnce(&mut Modules<Real>, &mut RealmEngine<'_>, &mut super::net::NetState) -> R,
) -> Option<R> {
    let mut guard = b.st.try_borrow_mut().ok()?;
    let st = &mut *guard;
    let json_parse = st.modrt.json_parse.as_ref().map(|p| r.pinned(p));
    let mut eng = RealmEngine { r, json_parse };
    let out = f(&mut st.modrt.mods, &mut eng, &mut st.net);
    Some(out)
}

// ---- setting up ------------------------------------------------------------------------------

/// Hooks and the pristine `JSON.parse`; once per realm, before any page script.
pub fn install(b: &Rc<Bridge>, r: &mut Realm) {
    let g = r.global();
    if let Ok(json) = r.get(g, "JSON")
        && json.is_object()
        && let Ok(parse) = r.get(json, "parse")
        && r.is_callable(parse)
    {
        let p = r.pin(parse);
        b.st.borrow_mut().modrt.json_parse = Some(p);
    }
    let b2 = b.clone();
    r.set_import_meta_hook(Some(Box::new(move |r: &mut Realm, id: ModuleId, meta: Value| {
        import_meta(&b2, r, id, meta)
    })));
    let b2 = b.clone();
    r.set_dynamic_import_hook(Some(Box::new(move |r: &mut Realm, di: DynamicImport| {
        dynamic_import(&b2, r, di)
    })));
}

/// `import.meta`: `url` and `resolve(specifier)`.
fn import_meta(b: &Rc<Bridge>, r: &mut Realm, id: ModuleId, meta: Value) -> jsengine::JsResult<()> {
    let base = {
        let st = b.st(r)?;
        st.modrt
            .mods
            .key_of(id)
            .and_then(|k| st.modrt.mods.base_of(k))
            .map(String::from)
            .unwrap_or_default()
    };
    let url = r.string(&base);
    r.define(meta, "url", url, Attrs::DEFAULT)?;
    let b2 = b.clone();
    let from = base.clone();
    let f = r.new_function("resolve", 1, move |r, _t, a| {
        let spec = arg_str(r, a, 0)?;
        let res = b2.st(r)?.modrt.mods.resolve(&spec, &from);
        match res {
            Ok(u) => Ok(r.string(&u)),
            Err(m) => Err(type_error(r, &m)),
        }
    });
    r.define(meta, "resolve", f, Attrs::DEFAULT)?;
    Ok(())
}

/// `import(specifier)`: resolve it, start the graph; [`pump`] answers the engine when it is in.
fn dynamic_import(b: &Rc<Bridge>, r: &mut Realm, di: DynamicImport) -> jsengine::JsResult<()> {
    let page_url = b.page().map(|p| String::from(p.page_url())).unwrap_or_default();
    let doc_base = b.page().map(|p| String::from(p.base_url())).unwrap_or_default();
    let ty = match type_of_attributes(&di.request.attributes) {
        Ok(t) => t,
        Err(m) => return Err(type_error(r, &m)),
    };
    // A page that calls import() in a loop gets rejections, not memory.
    if b.st(r)?.modrt.waiters.len() >= MAX_PENDING_IMPORTS {
        return Err(type_error(r, "Too many import() calls are waiting for their modules"));
    }
    // Relative to the module that asked; for a classic script, its file (an inline script's
    // file is the page itself: relative to the document's base address).
    let (base, creds) = {
        let st = b.st(r)?;
        let m = &st.modrt.mods;
        match di.referrer.and_then(|id| m.key_of(id)) {
            Some(k) => (
                m.base_of(k).map(String::from).unwrap_or(doc_base.clone()),
                m.credentials_of(k),
            ),
            None if di.referrer_file == page_url || di.referrer_file.is_empty() => {
                (doc_base.clone(), CredentialsMode::SameOrigin)
            }
            None => (di.referrer_file.clone(), CredentialsMode::SameOrigin),
        }
    };
    let url = match b.st(r)?.modrt.mods.resolve(&di.request.specifier, &base) {
        Ok(u) => u,
        Err(m) => return Err(type_error(r, &m)),
    };
    let started = with_mods(b, r, |mods, eng, _net| mods.fetch_graph(eng, &url, ty, creds, true));
    match started {
        Some(Ok(g)) => {
            if let Ok(mut st) = b.st.try_borrow_mut() {
                st.modrt.waiters.insert(g, Waiter::Dynamic(di.id));
            }
            Ok(())
        }
        Some(Err(m)) => Err(type_error(r, &m)),
        None => Err(type_error(r, "busy (re-entrant call)")),
    }
}

// ---- starting work ---------------------------------------------------------------------------

/// Begin what the loader has queued (import maps, module graphs, preloads) and ask the app for
/// the fetches. Compiles inline modules; runs no page script.
pub fn start_work(b: &Rc<Bridge>, r: &mut Realm) {
    loop {
        let work = core::mem::take(&mut b.st.borrow_mut().loader.work);
        if work.is_empty() {
            break;
        }
        let page_url = b.page().map(|p| String::from(p.page_url())).unwrap_or_default();
        let doc_base = b.page().map(|p| String::from(p.base_url())).unwrap_or_default();
        for w in work {
            match w {
                ModWork::ImportMap(_, text) => {
                    with_mods(b, r, |mods, _eng, _net| mods.add_import_map(&text, &doc_base));
                }
                ModWork::BadImportMap(node) => {
                    b.console(
                        ConsoleLevel::Warn,
                        String::from("External import maps (with a src) are not supported; use an inline one."),
                    );
                    super::events_js::fire_simple(b, r, node, "error", false, false);
                }
                ModWork::Preload(url, creds) => {
                    let g = with_mods(b, r, |mods, eng, _net| {
                        mods.fetch_graph(eng, &url, ModType::Js, creds, false)
                    });
                    match g {
                        Some(Ok(g)) => {
                            b.st.borrow_mut().modrt.waiters.insert(g, Waiter::Preload);
                        }
                        Some(Err(m)) => b.console(ConsoleLevel::Warn, m),
                        None => {}
                    }
                }
                ModWork::Module(idx, inline, creds) => {
                    let src = b.st.borrow().loader.scripts.get(idx).and_then(|s| s.src.clone());
                    let g = with_mods(b, r, |mods, eng, _net| match (&src, &inline) {
                        (Some(url), _) => mods.fetch_graph(eng, url, ModType::Js, creds, true),
                        (None, Some(text)) => Ok(mods.inline_graph(eng, text, &page_url, &doc_base, 1, creds)),
                        (None, None) => Err(String::from("an empty module")),
                    });
                    match g {
                        Some(Ok(g)) => {
                            b.st.borrow_mut().modrt.waiters.insert(g, Waiter::Script(idx));
                        }
                        Some(Err(m)) => {
                            b.console(ConsoleLevel::Error, m);
                            let mut st = b.st.borrow_mut();
                            st.modrt.results.insert(idx, ScriptResult::Null);
                            st.modrt.wake = true;
                            if let Some(s) = st.loader.scripts.get_mut(idx) {
                                s.body = Body::Ready(String::new());
                            }
                        }
                        None => {}
                    }
                }
            }
        }
    }
    emit(b, r);
}

/// Console notes and fetch requests the module map has for the app.
fn emit(b: &Rc<Bridge>, r: &mut Realm) {
    let (notes, actions) = {
        let Ok(mut st) = b.st.try_borrow_mut() else { return };
        (st.modrt.mods.take_notes(), st.modrt.mods.take_actions())
    };
    for n in notes {
        b.console(if n.error { ConsoleLevel::Error } else { ConsoleLevel::Warn }, n.text);
    }
    let Ok(mut st) = b.st.try_borrow_mut() else { return };
    for a in actions {
        match a {
            Action::Fetch { key, url, credentials } => {
                let id = st.net.alloc_id();
                st.net.mod_fetches.insert(id, Fetch { key, url: String::new() });
                st.net.out.push(NetOut::Start(NetRequest {
                    id,
                    kind: NetKind::Module,
                    url,
                    method: String::from("GET"),
                    headers: Vec::new(),
                    body: Vec::new(),
                    mode: RequestMode::Cors,
                    credentials,
                    redirect: RedirectMode::Follow,
                    referrer: String::from("about:client"),
                    referrer_policy: String::new(),
                    cache: String::from("default"),
                    timeout_ms: 0,
                    force_preflight: false,
                }));
            }
        }
    }
    let _ = r;
}

// ---- answers from the app --------------------------------------------------------------------

fn header<'a>(h: &'a NetHead, name: &str) -> Option<&'a str> {
    h.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Is request `id` a module fetch of this page?
pub fn is_module_fetch(b: &Rc<Bridge>, id: u32) -> bool {
    b.st
        .try_borrow()
        .map(|st| st.net.mod_fetches.contains_key(&id))
        .unwrap_or(false)
}

/// One answer for the module fetch `id`. True if it was a task of the page's.
pub fn deliver(b: &Rc<Bridge>, r: &mut Realm, id: u32, ev: NetEvent) -> bool {
    match ev {
        NetEvent::Progress { .. } => return true,
        NetEvent::Head(h) => {
            let key = {
                let Ok(mut st) = b.st.try_borrow_mut() else { return true };
                let Some(f) = st.net.mod_fetches.get_mut(&id) else { return true };
                f.url = h.url.clone();
                f.key.clone()
            };
            let ctype = header(&h, "content-type").map(String::from);
            let verdict = with_mods(b, r, |mods, eng, _net| {
                mods.on_head(eng, &key, h.status, &h.status_text, ctype.as_deref())
            });
            if verdict == Some(Verdict::Abort) {
                let Ok(mut st) = b.st.try_borrow_mut() else { return true };
                st.net.mod_fetches.remove(&id);
                st.net.out.push(NetOut::Cancel(id));
            }
        }
        NetEvent::Done(body) => {
            let f = b.st.try_borrow_mut().ok().and_then(|mut st| st.net.mod_fetches.remove(&id));
            if let Some(f) = f {
                with_mods(b, r, |mods, eng, _net| mods.on_body(eng, &f.key, &body, &f.url));
            }
        }
        NetEvent::Failed(fail) => {
            let f = b.st.try_borrow_mut().ok().and_then(|mut st| st.net.mod_fetches.remove(&id));
            if let Some(f) = f {
                with_mods(b, r, |mods, eng, _net| mods.on_failed(eng, &f.key, fail.message));
            }
        }
    }
    emit(b, r);
    super::checkpoint(b, r);
    true
}

// ---- finishing graphs ------------------------------------------------------------------------

/// A report the page's `onerror` hears, for an error the engine did not throw (a bad
/// import's TypeError).
fn report_text(b: &Rc<Bridge>, r: &mut Realm, name: &str, message: &str, file: &str, line: u32, col: u32) {
    let e = r.new_error(
        match name {
            "SyntaxError" => ErrorKind::SyntaxError,
            _ => ErrorKind::TypeError,
        },
        message,
    );
    let rep = ErrorReport {
        name: String::from(name),
        message: String::from(message),
        file: String::from(file),
        line,
        column: col,
        stack: String::new(),
        uncatchable: false,
    };
    if !super::events_js::fire_error(b, r, &rep, Some(e)) {
        let at = if file.is_empty() { String::new() } else { format!("\n    at {file}:{line}:{col}") };
        b.console(ConsoleLevel::Error, format!("Uncaught {name}: {message}{at}"));
    }
    if let Ok(mut st) = b.st.try_borrow_mut() {
        st.stats.errors += 1;
    }
}

/// Report the error of a broken module (the one `key` names).
fn report_broken(b: &Rc<Bridge>, r: &mut Realm, key: &Key) {
    enum What {
        Thrown(Value),
        Term(Exception),
        Resolve(String, String, u32, u32),
        Nothing,
    }
    let what = {
        let Ok(st) = b.st.try_borrow() else { return };
        match st.modrt.mods.broken(key) {
            Some(Broken::Compile(ErrVal::Thrown(p))) => What::Thrown(r.pinned(p)),
            Some(Broken::Compile(ErrVal::Term(e))) => What::Term(*e),
            Some(Broken::Resolve { message, line, column }) => What::Resolve(
                message.clone(),
                st.modrt.mods.file_of(key).map(String::from).unwrap_or_default(),
                *line,
                *column,
            ),
            _ => What::Nothing,
        }
    };
    match what {
        What::Thrown(v) => {
            let e = r.throw(v);
            super::report(b, r, &e, "");
        }
        What::Term(e) => super::report(b, r, &e, ""),
        What::Resolve(m, file, line, col) => report_text(b, r, "TypeError", &m, &file, line, col),
        What::Nothing => {}
    }
}

/// The value a rejected `import()` gets for a broken module.
fn broken_value(b: &Rc<Bridge>, r: &mut Realm, key: &Key) -> Value {
    let found = {
        let Ok(st) = b.st.try_borrow() else {
            return r.new_error(ErrorKind::TypeError, "busy");
        };
        match st.modrt.mods.broken(key) {
            Some(Broken::Compile(ErrVal::Thrown(p))) => Ok(r.pinned(p)),
            Some(Broken::Resolve { message, .. }) => Err(message.clone()),
            _ => Err(String::from("the module can't be used")),
        }
    };
    match found {
        Ok(v) => v,
        Err(m) => r.new_error(ErrorKind::TypeError, &m),
    }
}

/// Settle the graphs that have finished: scripts get their result, `import()` its answer.
/// Call at a task's end, where script may run. True if anything happened (more jobs may be
/// queued).
pub fn pump(b: &Rc<Bridge>, r: &mut Realm) -> bool {
    {
        let Ok(mut st) = b.st.try_borrow_mut() else { return false };
        if st.modrt.pumping {
            return false;
        }
        st.modrt.pumping = true;
    }
    let mut did = false;
    for _ in 0..64 {
        if !b.st.borrow().loader.work.is_empty() {
            start_work(b, r);
            did = true;
        }
        emit(b, r);
        let done = {
            let Ok(mut st) = b.st.try_borrow_mut() else { break };
            st.modrt.mods.take_done()
        };
        if done.is_empty() {
            break;
        }
        did = true;
        for (g, out) in done {
            let waiter = b.st.borrow_mut().modrt.waiters.remove(&g);
            match waiter {
                Some(Waiter::Script(i)) => script_graph_done(b, r, i, out),
                Some(Waiter::Dynamic(id)) => dynamic_graph_done(b, r, id, out),
                Some(Waiter::Preload) | None => {}
            }
            if b.st.borrow().stopped {
                break;
            }
        }
        emit(b, r);
        if b.st.borrow().stopped {
            break;
        }
    }
    b.st.borrow_mut().modrt.pumping = false;
    did
}

fn script_graph_done(b: &Rc<Bridge>, r: &mut Realm, i: usize, out: Outcome<Real>) {
    let res = match out {
        Outcome::Ready(id) => ScriptResult::Run(id),
        Outcome::Network(..) => ScriptResult::Null,
        Outcome::Broken(k) => ScriptResult::Broken(k),
        Outcome::Engine(e) => ScriptResult::Engine(e),
    };
    let _ = r;
    let mut st = b.st.borrow_mut();
    st.modrt.results.insert(i, res);
    st.modrt.wake = true;
    if let Some(s) = st.loader.scripts.get_mut(i) {
        s.body = Body::Ready(String::new());
    }
}

fn dynamic_graph_done(b: &Rc<Bridge>, r: &mut Realm, id: jsengine::DynamicImportId, out: Outcome<Real>) {
    let result: Result<ModuleId, Value> = match out {
        Outcome::Ready(m) => Ok(m),
        Outcome::Network(k, _) => {
            let url = b
                .st
                .borrow()
                .modrt
                .mods
                .file_of(&k)
                .map(String::from)
                .unwrap_or_default();
            Err(r.new_error(
                ErrorKind::TypeError,
                &format!("Failed to fetch dynamically imported module: {url}"),
            ))
        }
        Outcome::Broken(k) => Err(broken_value(b, r, &k)),
        Outcome::Engine(ErrVal::Thrown(p)) => Err(r.pinned(&p)),
        Outcome::Engine(ErrVal::Term(e)) => {
            super::report(b, r, &e, "");
            return;
        }
    };
    if let Err(e) = r.finish_dynamic_import(id, result) {
        super::report(b, r, &e, "");
    }
}

// ---- running a module script -----------------------------------------------------------------

/// Run (or fail) module script `i` of the loader; its graph is in. `top`: from the event loop.
pub fn run_script(b: &Rc<Bridge>, r: &mut Realm, i: usize, top: bool) {
    let (node, has_src) = {
        let st = b.st.borrow();
        let s = &st.loader.scripts[i];
        (s.node, s.src.is_some())
    };
    let res = b.st.borrow_mut().modrt.results.remove(&i);
    match res {
        Some(ScriptResult::Run(id)) => {
            let prev = core::mem::replace(&mut b.st.borrow_mut().current_script, crate::dom::NONE);
            b.st.borrow_mut().stats.scripts += 1;
            match r.evaluate_module(id) {
                Ok(promise) => report_rejection(b, r, promise),
                Err(e) => super::report(b, r, &e, ""),
            }
            b.st.borrow_mut().current_script = prev;
            if has_src {
                if top {
                    super::checkpoint(b, r);
                }
                super::events_js::fire_simple(b, r, node, "load", false, false);
            }
        }
        Some(ScriptResult::Broken(k)) => report_broken(b, r, &k),
        Some(ScriptResult::Engine(ErrVal::Thrown(p))) => {
            let v = r.pinned(&p);
            let e = r.throw(v);
            super::report(b, r, &e, "");
        }
        Some(ScriptResult::Engine(ErrVal::Term(e))) => super::report(b, r, &e, ""),
        Some(ScriptResult::Null) | None => {
            // (A fetch failed: the console has said why.)
            super::events_js::fire_simple(b, r, node, "error", false, false);
        }
    }
}

/// Report what a module's evaluation promise rejects with, as an uncaught error.
fn report_rejection(b: &Rc<Bridge>, r: &mut Realm, promise: Value) {
    let b2 = b.clone();
    let on_rejected = r.new_function("", 1, move |r, _t, a| {
        let e = r.throw(arg(a, 0));
        super::report(&b2, r, &e, "");
        Ok(Value::UNDEFINED)
    });
    if let Ok(then) = r.get(promise, "then") {
        let _ = r.call(then, promise, &[Value::UNDEFINED, on_rejected]);
    }
}

// ---- ending ------------------------------------------------------------------------------------

/// The page's scripts end: nothing more is fetched or imported, and the modules are given back.
pub fn teardown(b: &Rc<Bridge>, r: &mut Realm) {
    r.cancel_dynamic_imports();
    let (ids, pins) = {
        let mut st = b.st.borrow_mut();
        let ids = st.modrt.mods.ids();
        st.modrt.mods.clear_fetches();
        st.modrt.waiters.clear();
        st.modrt.results.clear();
        st.modrt.wake = false;
        let mut pins = Vec::new();
        if let Some(p) = st.modrt.json_parse.take() {
            pins.push(p);
        }
        (ids, pins)
    };
    for id in ids {
        r.drop_module(id);
    }
    for p in pins {
        r.unpin(p);
    }
}
