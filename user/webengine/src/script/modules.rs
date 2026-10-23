//! A page's module map and the fetch graph (HTML §8.1.? "module map", "fetch a module script
//! graph"), apart from the engine and from the network: this is a state machine. The caller
//! gives it events (a graph to start, the head of a response, a body, a failure) and takes
//! what it wants done ([`Action`]s: fetch this address) and what has finished
//! ([`Outcome`]s, per graph). Everything about module files that does not need script lives
//! here, so it can be tested with a fake [`Engine`]:
//!
//! * one module map per page, keyed by address (the fragment dropped) and module type, with
//!   in-flight dedup: a module is fetched, compiled and linked once, whoever asks;
//! * the graph walk: when a module has been compiled its requests are resolved (relative to
//!   its own address, through the import map) and fetched in turn; cycles are just entries
//!   already in the map; a graph is ready when everything it reaches is compiled;
//! * the response checks: a successful status and a JavaScript (or, for a JSON module, JSON)
//!   MIME type;
//! * the limits: modules per page, the depth of an import chain, fetches in flight, the size
//!   of one module and of all of them;
//! * the import map and when it may still be set.
//!
//! Errors split the way the standard splits them: a fetch that fails (network, CORS, status,
//! MIME, too big) makes the graph's result null ([`Outcome::Network`]: `error` event for a
//! script element, a rejected `import()`), while a module that does not compile or has an
//! import that does not resolve is an error to throw ([`Outcome::Broken`]: reported through
//! `window.onerror`, nothing in the graph runs).

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::importmap::{self, ImportMap};
use crate::api::CredentialsMode;

/// Modules one page may have in its map (fetched, failed or waiting).
pub const MAX_MODULES: usize = 2_000;
/// The longest chain of imports, from the first module down.
pub const MAX_DEPTH: u32 = 256;
/// Module fetches outstanding at once; the rest wait.
pub const MAX_INFLIGHT: usize = 24;
/// The most bytes of module source a page may fetch in all.
pub const MAX_TOTAL_BYTES: usize = 48 << 20;

/// What a module file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModType {
    Js,
    Json,
}

/// A module map key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    /// An address (no fragment) and the type.
    Url(String, ModType),
    /// An inline module script: never fetched, never the answer to an import.
    Inline(u32),
}

/// One thing a module asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub specifier: String,
    /// Import attributes (`with { type: "json" }`).
    pub attributes: Vec<(String, String)>,
    pub line: u32,
    pub column: u32,
}

/// The engine's types the graph holds on to.
pub trait Types {
    /// A compiled module.
    type Id: Copy + PartialEq + core::fmt::Debug;
    /// A thrown error kept for the page to see (a SyntaxError from compiling).
    type Err;
}

/// The script engine, as the graph needs it.
pub trait Engine<T: Types> {
    /// Compile `source` (for a JSON module: its text) under the name `file`.
    fn compile(
        &mut self,
        ty: ModType,
        source: &str,
        file: &str,
        first_line: u32,
    ) -> Result<T::Id, T::Err>;
    /// The requests of a compiled module, once each, in order (none for a JSON module).
    fn requests(&mut self, id: T::Id) -> Vec<Request>;
    /// Request number `index` of `id` is answered by `dep`.
    fn set_dependency(&mut self, id: T::Id, index: usize, dep: T::Id) -> Result<(), T::Err>;
}

/// What the caller is to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Fetch `url` (mode cors, `credentials`); answer with [`Modules::on_head`] and
    /// [`Modules::on_body`], or [`Modules::on_failed`].
    Fetch {
        key: Key,
        url: String,
        credentials: CredentialsMode,
    },
}

/// A line for the page's console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub error: bool,
    pub text: String,
}

/// Why a module is of no use.
pub enum Broken<T: Types> {
    /// Its fetch failed, or the response wasn't a module.
    Network(String),
    /// It did not compile: the error to throw.
    Compile(T::Err),
    /// An import of it did not resolve (a TypeError's message) or is not one this browser
    /// does (an attribute it doesn't know, a type it can't make).
    Resolve { message: String, line: u32, column: u32 },
}

enum State<T: Types> {
    /// Waiting for a free fetch slot.
    Queued,
    Fetching,
    Parsed { id: T::Id, deps: Vec<Key> },
    Broken(Broken<T>),
}

struct Entry<T: Types> {
    state: State<T>,
    credentials: CredentialsMode,
    depth: u32,
    /// The address its relative imports resolve against (after redirects).
    base: String,
    /// What it is called in errors.
    file: String,
    /// The engine has been told its dependencies.
    bound: bool,
}

/// How a graph ended.
pub enum Outcome<T: Types> {
    /// Everything is compiled (and, for a graph that isn't a preload, its dependencies are
    /// set): link and evaluate the root.
    Ready(T::Id),
    /// A module could not be fetched: its key and why (the console has said already).
    Network(Key, String),
    /// A module is broken in the way that is thrown: its key (see [`Modules::broken`]).
    Broken(Key),
    /// The engine refused to take the dependencies.
    Engine(T::Err),
}

struct Graph {
    id: u32,
    root: Key,
    /// Set the engine's dependencies when ready (false: a preload).
    bind: bool,
}

pub struct Limits {
    pub max_modules: usize,
    pub max_depth: u32,
    pub max_inflight: usize,
    pub max_source: usize,
    pub max_total: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_modules: MAX_MODULES,
            max_depth: MAX_DEPTH,
            max_inflight: MAX_INFLIGHT,
            max_source: 8 << 20,
            max_total: MAX_TOTAL_BYTES,
        }
    }
}

/// The page's import map and whether another may still be added.
pub struct Maps {
    pub map: ImportMap,
    /// An import map has been taken.
    pub set: bool,
    /// False once module loading has started.
    pub allowed: bool,
}

pub struct Modules<T: Types> {
    map: BTreeMap<Key, Entry<T>>,
    graphs: Vec<Graph>,
    next_graph: u32,
    next_inline: u32,
    queue: VecDeque<Key>,
    inflight: usize,
    pub maps: Maps,
    actions: Vec<Action>,
    done: Vec<(u32, Outcome<T>)>,
    notes: Vec<Note>,
    total_bytes: usize,
    pub limits: Limits,
}

impl<T: Types> Default for Modules<T> {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

/// A fragment is not part of a module's identity here (the standard keeps it in the key; two
/// fragments of one file are one module for us).
fn strip_fragment(url: &str) -> &str {
    url.split('#').next().unwrap_or(url)
}

fn mime_essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// A JSON MIME type (MIME Sniffing §4.6): `application/json`, `text/json`, or `*/*+json`.
pub fn is_json_mime(essence: &str) -> bool {
    matches!(essence, "application/json" | "text/json")
        || (essence.contains('/') && essence.ends_with("+json"))
}

/// The checks on a module response's head: success, and the right kind of type.
pub fn check_head(
    ty: ModType,
    status: u16,
    status_text: &str,
    content_type: Option<&str>,
) -> Result<(), String> {
    if !(200..300).contains(&status) {
        return Err(if status_text.is_empty() {
            format!("the server responded with a status of {status}")
        } else {
            format!("the server responded with a status of {status} ({status_text})")
        });
    }
    let essence = mime_essence(content_type.unwrap_or(""));
    let ok = match ty {
        ModType::Js => super::loader::is_javascript_mime(&essence),
        ModType::Json => is_json_mime(&essence),
    };
    if ok {
        return Ok(());
    }
    let want = if ty == ModType::Js { "JavaScript" } else { "JSON" };
    Err(if essence.is_empty() {
        format!(
            "expected a {want} module script but the server sent no MIME type; strict MIME type checking is enforced for module scripts per the HTML standard"
        )
    } else {
        format!(
            "expected a {want} module script but the server responded with a MIME type of \"{essence}\"; strict MIME type checking is enforced for module scripts per the HTML standard"
        )
    })
}

/// The check for a module request whose kind (JavaScript or JSON) is not known to the one
/// making it: a success status and either kind of type. (The engine checks the exact kind.)
pub fn check_module_head(status: u16, status_text: &str, content_type: Option<&str>) -> Result<(), String> {
    match check_head(ModType::Js, status, status_text, content_type) {
        Ok(()) => Ok(()),
        Err(e) if (200..300).contains(&status) => {
            let essence = mime_essence(content_type.unwrap_or(""));
            if is_json_mime(&essence) { Ok(()) } else { Err(e) }
        }
        Err(e) => Err(e),
    }
}

/// The module type an import's attributes ask for, or why it can't be had.
pub fn type_of_attributes(attrs: &[(String, String)]) -> Result<ModType, String> {
    let mut ty = ModType::Js;
    for (k, v) in attrs {
        if k != "type" {
            return Err(format!("Import attribute \"{k}\" is not supported"));
        }
        match v.as_str() {
            "json" => ty = ModType::Json,
            other => return Err(format!("Module type \"{other}\" is not supported")),
        }
    }
    Ok(ty)
}

enum Poll {
    Pending,
    Ready,
    Network(Key),
    Broken(Key),
}

/// What [`Modules::on_head`] says to do with the response.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Continue,
    /// Stop the request: it is no module (the console has been told).
    Abort,
}

impl<T: Types> Modules<T> {
    pub fn new(limits: Limits) -> Modules<T> {
        Modules {
            map: BTreeMap::new(),
            graphs: Vec::new(),
            next_graph: 1,
            next_inline: 1,
            queue: VecDeque::new(),
            inflight: 0,
            maps: Maps {
                map: ImportMap::default(),
                set: false,
                allowed: true,
            },
            actions: Vec::new(),
            done: Vec::new(),
            notes: Vec::new(),
            total_bytes: 0,
            limits,
        }
    }

    fn note(&mut self, error: bool, text: String) {
        if self.notes.len() < 200 {
            self.notes.push(Note { error, text });
        }
    }

    pub fn take_notes(&mut self) -> Vec<Note> {
        core::mem::take(&mut self.notes)
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        core::mem::take(&mut self.actions)
    }

    pub fn take_done(&mut self) -> Vec<(u32, Outcome<T>)> {
        core::mem::take(&mut self.done)
    }

    /// Graphs that have not finished.
    pub fn active(&self) -> usize {
        self.graphs.len()
    }

    /// Modules in the map.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Fetches running or waiting.
    pub fn fetching(&self) -> usize {
        self.inflight + self.queue.len()
    }

    // ---- the import map ------------------------------------------------------------------------

    /// Module loading starts: no import map can be added from here on.
    pub fn disallow_maps(&mut self) {
        self.maps.allowed = false;
    }

    /// An inline `<script type=importmap>` with this text, in a document at `base`.
    pub fn add_import_map(&mut self, text: &str, base: &str) {
        if !self.maps.allowed {
            self.note(
                false,
                String::from("An import map was added after module loading had started, so it was ignored."),
            );
            return;
        }
        if self.maps.set {
            self.note(
                false,
                String::from("A second import map was ignored (only the first one of a page counts)."),
            );
            return;
        }
        let mut warnings = Vec::new();
        let r = importmap::parse(text, base, &mut warnings);
        for w in warnings {
            self.note(false, w);
        }
        match r {
            Ok(m) => {
                self.maps.map = m;
                self.maps.set = true;
            }
            Err(e) => self.note(true, e),
        }
    }

    // ---- starting graphs -----------------------------------------------------------------------

    /// Fetch the module at `url` and everything it imports. `Err`: it can't even be tried
    /// (too many modules, an import chain too deep).
    pub fn fetch_graph(
        &mut self,
        eng: &mut dyn Engine<T>,
        url: &str,
        ty: ModType,
        credentials: CredentialsMode,
        bind: bool,
    ) -> Result<u32, String> {
        self.maps.allowed = false;
        let key = self.ensure(url, ty, credentials, 0)?;
        let id = self.next_graph;
        self.next_graph += 1;
        self.graphs.push(Graph { id, root: key, bind });
        self.pump();
        self.poll(eng);
        Ok(id)
    }

    /// An inline module script: its source is compiled now, its imports fetched.
    pub fn inline_graph(
        &mut self,
        eng: &mut dyn Engine<T>,
        source: &str,
        file: &str,
        base: &str,
        first_line: u32,
        credentials: CredentialsMode,
    ) -> u32 {
        self.maps.allowed = false;
        let n = self.next_inline;
        self.next_inline += 1;
        let key = Key::Inline(n);
        let entry = Entry {
            state: State::Fetching,
            credentials,
            depth: 0,
            base: String::from(base),
            file: String::from(file),
            bound: false,
        };
        self.map.insert(key.clone(), entry);
        self.compile_into(eng, &key, ModType::Js, source, first_line);
        let id = self.next_graph;
        self.next_graph += 1;
        self.graphs.push(Graph {
            id,
            root: key,
            bind: true,
        });
        self.pump();
        self.poll(eng);
        id
    }

    // ---- the map -------------------------------------------------------------------------------

    /// The entry for `url`, made (and its fetch queued or started) if it is new.
    fn ensure(
        &mut self,
        url: &str,
        ty: ModType,
        credentials: CredentialsMode,
        depth: u32,
    ) -> Result<Key, String> {
        let url = strip_fragment(url);
        let key = Key::Url(String::from(url), ty);
        if self.map.contains_key(&key) {
            return Ok(key);
        }
        if self.map.len() >= self.limits.max_modules {
            return Err(format!(
                "This page imports too many modules (the limit is {})",
                self.limits.max_modules
            ));
        }
        if depth > self.limits.max_depth {
            return Err(format!(
                "The imports nest too deeply ({} levels is the limit)",
                self.limits.max_depth
            ));
        }
        let mut e = Entry {
            state: State::Queued,
            credentials,
            depth,
            base: String::from(url),
            file: String::from(url),
            bound: false,
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            let why = String::from("only http and https addresses can be fetched");
            self.note(true, format!("Failed to load module script {url}: {why}"));
            e.state = State::Broken(Broken::Network(why));
            self.map.insert(key.clone(), e);
            return Ok(key);
        }
        self.map.insert(key.clone(), e);
        self.queue.push_back(key.clone());
        Ok(key)
    }

    /// Start queued fetches while there is room.
    fn pump(&mut self) {
        while self.inflight < self.limits.max_inflight {
            let Some(key) = self.queue.pop_front() else { break };
            let Some(e) = self.map.get_mut(&key) else { continue };
            if !matches!(e.state, State::Queued) {
                continue;
            }
            e.state = State::Fetching;
            self.inflight += 1;
            if let Key::Url(url, _) = &key {
                self.actions.push(Action::Fetch {
                    key: key.clone(),
                    url: url.clone(),
                    credentials: e.credentials,
                });
            }
        }
    }

    /// Compile an entry's source and read its requests.
    fn compile_into(&mut self, eng: &mut dyn Engine<T>, key: &Key, ty: ModType, source: &str, first_line: u32) {
        let (file, base, creds, depth) = {
            let e = &self.map[key];
            (e.file.clone(), e.base.clone(), e.credentials, e.depth)
        };
        let state = match eng.compile(ty, source, &file, first_line) {
            Err(err) => State::Broken(Broken::Compile(err)),
            Ok(id) => {
                let reqs = eng.requests(id);
                let mut deps: Vec<Key> = Vec::new();
                let mut bad: Option<Broken<T>> = None;
                for r in &reqs {
                    let dep = type_of_attributes(&r.attributes).and_then(|ty| {
                        self.maps.allowed = false;
                        let url = importmap::resolve(&self.maps.map, &r.specifier, &base).map_err(|e| e.0)?;
                        self.ensure(&url, ty, creds, depth + 1)
                    });
                    match dep {
                        Ok(k) => deps.push(k),
                        Err(message) => {
                            bad = Some(Broken::Resolve {
                                message,
                                line: r.line,
                                column: r.column,
                            });
                            break;
                        }
                    }
                }
                match bad {
                    Some(b) => State::Broken(b),
                    None => State::Parsed { id, deps },
                }
            }
        };
        if let Some(e) = self.map.get_mut(key) {
            e.state = state;
        }
    }

    // ---- events from the network ---------------------------------------------------------------

    /// The head of the response for `key` came. `url` is the address it was served from.
    pub fn on_head(
        &mut self,
        eng: &mut dyn Engine<T>,
        key: &Key,
        status: u16,
        status_text: &str,
        content_type: Option<&str>,
    ) -> Verdict {
        let Key::Url(_, ty) = key else {
            return Verdict::Continue;
        };
        match check_head(*ty, status, status_text, content_type) {
            Ok(()) => Verdict::Continue,
            Err(why) => {
                self.fail(eng, key, why);
                Verdict::Abort
            }
        }
    }

    /// The fetch of `key` failed (`message` says why).
    pub fn on_failed(&mut self, eng: &mut dyn Engine<T>, key: &Key, message: String) {
        self.fail(eng, key, message);
    }

    fn fail(&mut self, eng: &mut dyn Engine<T>, key: &Key, why: String) {
        let Some(e) = self.map.get_mut(key) else { return };
        if !matches!(e.state, State::Fetching) {
            return;
        }
        let url = e.file.clone();
        e.state = State::Broken(Broken::Network(why.clone()));
        self.inflight = self.inflight.saturating_sub(1);
        self.note(true, format!("Failed to load module script {url}: {why}"));
        self.pump();
        self.poll(eng);
    }

    /// The whole body of `key`'s response came; `url` is the address it was served from (after
    /// redirects; "" for the one asked for).
    pub fn on_body(&mut self, eng: &mut dyn Engine<T>, key: &Key, body: &[u8], url: &str) {
        let Key::Url(_, ty) = key else { return };
        let ty = *ty;
        if !matches!(self.map.get(key).map(|e| &e.state), Some(State::Fetching)) {
            return;
        }
        if body.len() > self.limits.max_source {
            let why = format!("it is too big ({} KB; the limit is {} KB)", body.len() >> 10, self.limits.max_source >> 10);
            self.fail(eng, key, why);
            return;
        }
        if self.total_bytes.saturating_add(body.len()) > self.limits.max_total {
            let why = format!(
                "the page has fetched more module code than it may ({} MB)",
                self.limits.max_total >> 20
            );
            self.fail(eng, key, why);
            return;
        }
        self.total_bytes += body.len();
        let e = self.map.get_mut(key).expect("checked above");
        if !url.is_empty() {
            let u = strip_fragment(url);
            e.base = String::from(u);
            e.file = String::from(u);
        }
        self.inflight = self.inflight.saturating_sub(1);
        let b = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body);
        let text = String::from_utf8_lossy(b);
        self.compile_into(eng, key, ty, &text, 1);
        self.pump();
        self.poll(eng);
    }

    // ---- graphs --------------------------------------------------------------------------------

    fn check(&self, root: &Key) -> Poll {
        let mut seen: BTreeSet<&Key> = BTreeSet::new();
        let mut stack: Vec<&Key> = alloc::vec![root];
        let mut pending = false;
        while let Some(k) = stack.pop() {
            if !seen.insert(k) {
                continue;
            }
            let Some(e) = self.map.get(k) else { continue };
            match &e.state {
                State::Queued | State::Fetching => pending = true,
                State::Broken(Broken::Network(_)) => return Poll::Network(k.clone()),
                State::Broken(_) => return Poll::Broken(k.clone()),
                State::Parsed { deps, .. } => {
                    for d in deps.iter().rev() {
                        stack.push(d);
                    }
                }
            }
        }
        if pending { Poll::Pending } else { Poll::Ready }
    }

    /// Settle the graphs that can be.
    fn poll(&mut self, eng: &mut dyn Engine<T>) {
        let graphs = core::mem::take(&mut self.graphs);
        let mut keep = Vec::new();
        for g in graphs {
            match self.check(&g.root) {
                Poll::Pending => keep.push(g),
                Poll::Network(k) => {
                    let why = match self.map.get(&k).map(|e| &e.state) {
                        Some(State::Broken(Broken::Network(w))) => w.clone(),
                        _ => String::new(),
                    };
                    self.done.push((g.id, Outcome::Network(k, why)));
                }
                Poll::Broken(k) => self.done.push((g.id, Outcome::Broken(k))),
                Poll::Ready => {
                    let out = match self.bind(eng, &g.root, g.bind) {
                        Ok(id) => Outcome::Ready(id),
                        Err(e) => Outcome::Engine(e),
                    };
                    self.done.push((g.id, out));
                }
            }
        }
        self.graphs = keep;
    }

    /// Tell the engine the dependencies of every module reachable from `root` that it has
    /// not been told (when `bind`), and give back the root's id.
    fn bind(&mut self, eng: &mut dyn Engine<T>, root: &Key, bind: bool) -> Result<T::Id, T::Err> {
        let mut seen: BTreeSet<Key> = BTreeSet::new();
        let mut stack: Vec<Key> = alloc::vec![root.clone()];
        while let Some(k) = stack.pop() {
            if !seen.insert(k.clone()) {
                continue;
            }
            let (id, deps, bound) = match self.map.get(&k) {
                Some(Entry {
                    state: State::Parsed { id, deps },
                    bound,
                    ..
                }) => (*id, deps.clone(), *bound),
                _ => continue,
            };
            if bind && !bound {
                for (i, d) in deps.iter().enumerate() {
                    if let Some(Entry {
                        state: State::Parsed { id: did, .. },
                        ..
                    }) = self.map.get(d)
                    {
                        eng.set_dependency(id, i, *did)?;
                    }
                }
                if let Some(e) = self.map.get_mut(&k) {
                    e.bound = true;
                }
            }
            stack.extend(deps);
        }
        match self.map.get(root).map(|e| &e.state) {
            Some(State::Parsed { id, .. }) => Ok(*id),
            _ => unreachable!("a ready graph's root is compiled"),
        }
    }

    // ---- looking ---------------------------------------------------------------------------------

    pub fn broken(&self, key: &Key) -> Option<&Broken<T>> {
        match self.map.get(key).map(|e| &e.state) {
            Some(State::Broken(b)) => Some(b),
            _ => None,
        }
    }

    /// The credentials mode a module was first asked for with (imports inherit it).
    pub fn credentials_of(&self, key: &Key) -> CredentialsMode {
        self.map
            .get(key)
            .map_or(CredentialsMode::SameOrigin, |e| e.credentials)
    }

    /// Where a module's relative imports resolve from, and what it is called.
    pub fn base_of(&self, key: &Key) -> Option<&str> {
        self.map.get(key).map(|e| e.base.as_str())
    }

    pub fn file_of(&self, key: &Key) -> Option<&str> {
        self.map.get(key).map(|e| e.file.as_str())
    }

    /// The key of the compiled module `id`.
    pub fn key_of(&self, id: T::Id) -> Option<&Key> {
        self.map.iter().find_map(|(k, e)| match &e.state {
            State::Parsed { id: i, .. } if *i == id => Some(k),
            _ => None,
        })
    }

    /// The compiled module for `key`, if it is one.
    pub fn id_of(&self, key: &Key) -> Option<T::Id> {
        match self.map.get(key).map(|e| &e.state) {
            Some(State::Parsed { id, .. }) => Some(*id),
            _ => None,
        }
    }

    /// The ids of every compiled module (to give back when the page ends).
    pub fn ids(&self) -> Vec<T::Id> {
        self.map
            .values()
            .filter_map(|e| match &e.state {
                State::Parsed { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Give up every fetch (the page ends): nothing more is asked for.
    pub fn clear_fetches(&mut self) {
        self.queue.clear();
        self.actions.clear();
        self.graphs.clear();
        self.done.clear();
    }

    /// Resolve `specifier` as written in a module (or script) whose address is `base`.
    pub fn resolve(&mut self, specifier: &str, base: &str) -> Result<String, String> {
        self.maps.allowed = false;
        importmap::resolve(&self.maps.map, specifier, base).map_err(|e| e.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    /// A script engine in a few lines: a source is lines like `import "spec"`,
    /// `import "spec" json`; "SYNTAX" anywhere is a compile error.
    #[derive(Default)]
    struct Fake {
        next: u32,
        sources: Vec<(u32, Vec<Request>)>,
        deps: Vec<(u32, usize, u32)>,
        compiled: Vec<String>,
    }

    impl Types for Fake {
        type Id = u32;
        type Err = String;
    }

    impl Engine<Fake> for Fake {
        fn compile(&mut self, ty: ModType, source: &str, file: &str, _line: u32) -> Result<u32, String> {
            self.compiled.push(file.to_string());
            if source.contains("SYNTAX") {
                return Err(format!("SyntaxError in {file}"));
            }
            self.next += 1;
            let mut reqs: Vec<Request> = Vec::new();
            if ty == ModType::Js {
                for (n, l) in source.lines().enumerate() {
                    if let Some(rest) = l.strip_prefix("import \"") {
                        let (spec, tail) = rest.split_once('"').unwrap();
                        let mut attributes = Vec::new();
                        if tail.trim() == "json" {
                            attributes.push((String::from("type"), String::from("json")));
                        } else if tail.trim() == "css" {
                            attributes.push((String::from("type"), String::from("css")));
                        } else if tail.trim() == "foo" {
                            attributes.push((String::from("foo"), String::from("bar")));
                        }
                        let r = Request { specifier: String::from(spec), attributes, line: n as u32 + 1, column: 1 };
                        if !reqs.iter().any(|x| x.specifier == r.specifier && x.attributes == r.attributes) {
                            reqs.push(r);
                        }
                    }
                }
            }
            self.sources.push((self.next, reqs));
            Ok(self.next)
        }
        fn requests(&mut self, id: u32) -> Vec<Request> {
            self.sources.iter().find(|s| s.0 == id).map(|s| s.1.clone()).unwrap_or_default()
        }
        fn set_dependency(&mut self, id: u32, index: usize, dep: u32) -> Result<(), String> {
            self.deps.push((id, index, dep));
            Ok(())
        }
    }

    const CR: CredentialsMode = CredentialsMode::SameOrigin;
    const A: &str = "https://t.test/a.js";

    fn url(u: &str) -> Key {
        Key::Url(String::from(u), ModType::Js)
    }

    fn fetches(m: &mut Modules<Fake>) -> Vec<String> {
        m.take_actions()
            .into_iter()
            .map(|a| match a {
                Action::Fetch { url, .. } => url,
            })
            .collect()
    }

    fn serve(m: &mut Modules<Fake>, e: &mut Fake, u: &str, body: &str) {
        let k = url(u);
        assert_eq!(m.on_head(e, &k, 200, "OK", Some("text/javascript")), Verdict::Continue);
        m.on_body(e, &k, body.as_bytes(), "");
    }

    fn done_ids(m: &mut Modules<Fake>) -> Vec<u32> {
        m.take_done().into_iter().map(|d| d.0).collect()
    }

    #[test]
    fn a_graph_is_fetched_once_and_ready_when_all_is_compiled() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        let g = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        assert_eq!(fetches(&mut m), [A]);
        assert!(m.take_done().is_empty());
        serve(&mut m, &mut e, A, "import \"./b.js\"\nimport \"./c.js\"\nimport \"./b.js\"\n");
        // b and c, once each, in source order.
        assert_eq!(fetches(&mut m), ["https://t.test/b.js", "https://t.test/c.js"]);
        serve(&mut m, &mut e, "https://t.test/c.js", "");
        assert!(m.take_done().is_empty(), "b is still out");
        serve(&mut m, &mut e, "https://t.test/b.js", "import \"./d.js\"\n");
        assert_eq!(fetches(&mut m), ["https://t.test/d.js"]);
        serve(&mut m, &mut e, "https://t.test/d.js", "");
        let done = m.take_done();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, g);
        assert!(matches!(done[0].1, Outcome::Ready(1)));
        // The engine was told all of the dependencies: a's two (b, c), b's one (d).
        e.deps.sort();
        assert_eq!(e.deps.len(), 3, "{:?}", e.deps);
        assert_eq!(m.len(), 4);
        assert_eq!(m.active(), 0);
    }

    #[test]
    fn the_same_module_asked_twice_is_one_fetch_and_one_compile() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        let g1 = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        let g2 = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        assert_eq!(fetches(&mut m), [A], "in-flight dedup");
        serve(&mut m, &mut e, A, "");
        let mut ids = done_ids(&mut m);
        ids.sort();
        assert_eq!(ids, [g1, g2]);
        // Later: ready at once, no fetch.
        let g3 = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        assert!(fetches(&mut m).is_empty());
        assert_eq!(done_ids(&mut m), [g3]);
        assert_eq!(e.compiled.len(), 1);
        // A fragment is the same module; another type is not.
        m.fetch_graph(&mut e, "https://t.test/a.js#x", ModType::Js, CR, true).unwrap();
        assert!(fetches(&mut m).is_empty());
        m.fetch_graph(&mut e, A, ModType::Json, CR, true).unwrap();
        assert_eq!(fetches(&mut m), [A]);
    }

    #[test]
    fn cycles_end() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"./b.js\"\n");
        fetches(&mut m);
        serve(&mut m, &mut e, "https://t.test/b.js", "import \"./a.js\"\nimport \"./b.js\"\n");
        assert!(fetches(&mut m).is_empty(), "both are known already");
        let d = m.take_done();
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0].1, Outcome::Ready(_)));
    }

    #[test]
    fn a_failed_fetch_fails_the_graphs_that_reach_it_and_is_remembered() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        let g = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"./gone.js\"\nimport \"./slow.js\"\n");
        fetches(&mut m);
        let k = url("https://t.test/gone.js");
        assert_eq!(m.on_head(&mut e, &k, 404, "Not Found", Some("text/html")), Verdict::Abort);
        let d = m.take_done();
        assert_eq!(d.len(), 1, "no waiting for slow.js");
        assert_eq!(d[0].0, g);
        match &d[0].1 {
            Outcome::Network(k2, why) => {
                assert_eq!(*k2, k);
                assert!(why.contains("404 (Not Found)"), "{why}");
            }
            _ => panic!("wanted a network failure"),
        }
        let notes = m.take_notes();
        assert!(notes.iter().any(|n| n.error && n.text.contains("gone.js") && n.text.contains("404")), "{notes:?}");
        // Another graph reaching it fails the same way, without a second fetch or a second note.
        let g2 = m.fetch_graph(&mut e, "https://t.test/gone.js", ModType::Js, CR, true).unwrap();
        let d = m.take_done();
        assert_eq!(d[0].0, g2);
        assert!(matches!(d[0].1, Outcome::Network(..)));
        assert!(m.take_notes().is_empty());
    }

    #[test]
    fn response_checks() {
        assert!(check_head(ModType::Js, 200, "OK", Some("text/javascript; charset=utf-8")).is_ok());
        assert!(check_head(ModType::Js, 200, "OK", Some("APPLICATION/JAVASCRIPT")).is_ok());
        assert!(check_head(ModType::Js, 204, "", Some("text/javascript")).is_ok());
        let e = check_head(ModType::Js, 200, "OK", Some("text/html")).unwrap_err();
        assert!(e.contains("MIME type of \"text/html\"") && e.contains("JavaScript"), "{e}");
        let e = check_head(ModType::Js, 200, "OK", None).unwrap_err();
        assert!(e.contains("no MIME type"), "{e}");
        assert!(check_head(ModType::Js, 200, "OK", Some("application/json")).is_err());
        assert!(check_head(ModType::Js, 200, "OK", Some("text/plain")).is_err());
        assert!(check_head(ModType::Js, 301, "Moved", Some("text/javascript")).is_err());
        assert!(check_head(ModType::Js, 500, "", Some("text/javascript")).unwrap_err().ends_with("500"));
        assert!(check_head(ModType::Json, 200, "OK", Some("application/json")).is_ok());
        assert!(check_head(ModType::Json, 200, "OK", Some("text/json")).is_ok());
        assert!(check_head(ModType::Json, 200, "OK", Some("application/ld+json")).is_ok());
        assert!(check_head(ModType::Json, 200, "OK", Some("text/javascript")).is_err());
        assert!(check_head(ModType::Json, 200, "OK", Some("application/x-json")).is_err());
    }

    #[test]
    fn syntax_errors_and_bad_imports_are_broken_not_network() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        let g = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"./bad.js\"\n");
        fetches(&mut m);
        serve(&mut m, &mut e, "https://t.test/bad.js", "SYNTAX");
        let d = m.take_done();
        assert_eq!(d[0].0, g);
        let Outcome::Broken(k) = &d[0].1 else { panic!("wanted broken") };
        assert!(matches!(m.broken(k), Some(Broken::Compile(s)) if s.contains("bad.js")));
        // A bare specifier: a resolve error with the line.
        let g = m.fetch_graph(&mut e, "https://t.test/bare.js", ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, "https://t.test/bare.js", "import \"./ok.js\"\nimport \"lodash\"\n");
        let d = m.take_done();
        assert_eq!(d[0].0, g);
        let Outcome::Broken(k) = &d[0].1 else { panic!("wanted broken") };
        match m.broken(k) {
            Some(Broken::Resolve { message, line, .. }) => {
                assert!(message.contains("\"lodash\""), "{message}");
                assert_eq!(*line, 2);
            }
            _ => panic!("wanted a resolve error"),
        }
        // Unsupported attributes and types.
        for (tail, want) in [("css", "\"css\" is not supported"), ("foo", "\"foo\" is not supported")] {
            let u = format!("https://t.test/{tail}.js");
            m.fetch_graph(&mut e, &u, ModType::Js, CR, true).unwrap();
            fetches(&mut m);
            serve(&mut m, &mut e, &u, &format!("import \"./x.json\" {tail}\n"));
            let d = m.take_done();
            let Outcome::Broken(k) = &d[0].1 else { panic!("wanted broken") };
            assert!(matches!(m.broken(k), Some(Broken::Resolve { message, .. }) if message.contains(want)));
        }
        // JSON by attribute: another module type, fetched under its own key.
        let u = "https://t.test/j.js";
        m.fetch_graph(&mut e, u, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, u, "import \"./d.json\" json\n");
        let acts = m.take_actions();
        assert_eq!(
            acts,
            [Action::Fetch { key: Key::Url(String::from("https://t.test/d.json"), ModType::Json), url: String::from("https://t.test/d.json"), credentials: CR }]
        );
    }

    #[test]
    fn the_import_map_decides_bare_specifiers() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        m.add_import_map(r#"{"imports": {"lodash": "/vendor/lodash.js"}}"#, "https://t.test/index.html");
        assert!(m.take_notes().is_empty());
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"lodash\"\n");
        assert_eq!(fetches(&mut m), ["https://t.test/vendor/lodash.js"]);
        // After loading has started, an import map is refused with a note.
        m.add_import_map(r#"{"imports": {"x": "/x.js"}}"#, "https://t.test/index.html");
        let n = m.take_notes();
        assert!(n.iter().any(|n| n.text.contains("after module loading had started")), "{n:?}");
    }

    #[test]
    fn only_the_first_import_map_counts() {
        let mut m = Modules::<Fake>::default();
        m.add_import_map(r#"{"imports": {"a": "/1.js"}}"#, "https://t.test/");
        m.add_import_map(r#"{"imports": {"a": "/2.js"}}"#, "https://t.test/");
        assert!(m.take_notes().iter().any(|n| n.text.contains("second import map")));
        assert_eq!(m.resolve("a", "https://t.test/m.js").unwrap(), "https://t.test/1.js");
        // A bad one is a console error and leaves the slot free.
        let mut m = Modules::<Fake>::default();
        m.add_import_map("{oops", "https://t.test/");
        assert!(m.take_notes().iter().any(|n| n.error && n.text.contains("Invalid import map")));
        m.add_import_map(r#"{"imports": {"a": "/3.js"}}"#, "https://t.test/");
        assert_eq!(m.resolve("a", "https://t.test/m.js").unwrap(), "https://t.test/3.js");
        // And resolving closes the door.
        m.add_import_map(r#"{"imports": {"b": "/4.js"}}"#, "https://t.test/");
        assert!(m.resolve("b", "https://t.test/m.js").is_err());
    }

    #[test]
    fn inline_modules_are_roots_that_are_never_imported() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        let g = m.inline_graph(&mut e, "import \"./a.js\"\n", "https://t.test/page.html", "https://t.test/dir/", 1, CR);
        assert_eq!(fetches(&mut m), ["https://t.test/dir/a.js"]);
        assert!(m.take_done().is_empty());
        serve(&mut m, &mut e, "https://t.test/dir/a.js", "");
        assert_eq!(done_ids(&mut m), [g]);
        // An inline module that imports "inline:1" gets a fetch failure, not itself.
        let mut m = Modules::<Fake>::default();
        let g = m.inline_graph(&mut e, "import \"inline:1\"\n", "https://t.test/page.html", "https://t.test/", 1, CR);
        let d = m.take_done();
        assert_eq!(d[0].0, g);
        assert!(matches!(d[0].1, Outcome::Network(..)));
        // An inline module that doesn't compile.
        let g = m.inline_graph(&mut e, "SYNTAX", "https://t.test/page.html", "https://t.test/", 1, CR);
        let d = m.take_done();
        assert_eq!(d[0].0, g);
        assert!(matches!(d[0].1, Outcome::Broken(_)));
    }

    #[test]
    fn limits() {
        let mut e = Fake::default();
        // Fetches in flight: the rest wait their turn.
        let mut m = Modules::<Fake>::new(Limits { max_inflight: 2, ..Limits::default() });
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"./1.js\"\nimport \"./2.js\"\nimport \"./3.js\"\nimport \"./4.js\"\n");
        assert_eq!(fetches(&mut m), ["https://t.test/1.js", "https://t.test/2.js"]);
        serve(&mut m, &mut e, "https://t.test/1.js", "");
        assert_eq!(fetches(&mut m), ["https://t.test/3.js"]);
        serve(&mut m, &mut e, "https://t.test/2.js", "");
        serve(&mut m, &mut e, "https://t.test/3.js", "");
        assert_eq!(fetches(&mut m), ["https://t.test/4.js"]);
        assert!(m.take_done().is_empty());
        serve(&mut m, &mut e, "https://t.test/4.js", "");
        assert_eq!(m.take_done().len(), 1);
        assert_eq!(m.fetching(), 0);

        // Modules per page.
        let mut m = Modules::<Fake>::new(Limits { max_modules: 3, ..Limits::default() });
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"./1.js\"\nimport \"./2.js\"\nimport \"./3.js\"\n");
        let d = m.take_done();
        let Outcome::Broken(k) = &d[0].1 else { panic!("wanted broken") };
        assert!(matches!(m.broken(k), Some(Broken::Resolve { message, .. }) if message.contains("too many modules")));
        assert!(m.len() <= 3);

        // Depth: a chain that never ends.
        let mut m = Modules::<Fake>::new(Limits { max_depth: 5, ..Limits::default() });
        m.fetch_graph(&mut e, "https://t.test/n0.js", ModType::Js, CR, true).unwrap();
        let mut n = 0;
        let mut ended = false;
        while !ended {
            let f = fetches(&mut m);
            assert!(f.len() <= 1);
            if let Some(u) = f.first() {
                n += 1;
                serve(&mut m, &mut e, u, &format!("import \"./n{n}.js\"\n"));
            }
            ended = !m.take_done().is_empty();
            assert!(n < 20, "the chain must stop");
        }
        assert!(n <= 7, "stopped after {n}");

        // Size of one module and of all.
        let mut m = Modules::<Fake>::new(Limits { max_source: 10, max_total: 15, ..Limits::default() });
        let g = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        m.on_body(&mut e, &url(A), &[b' '; 11], "");
        let d = m.take_done();
        assert_eq!(d[0].0, g);
        assert!(matches!(&d[0].1, Outcome::Network(_, w) if w.contains("too big")));
        let mut m = Modules::<Fake>::new(Limits { max_source: 100, max_total: 15, ..Limits::default() });
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"./b.js\"\n"); // 16 bytes: over the total
        assert!(matches!(&m.take_done()[0].1, Outcome::Network(_, w) if w.contains("more module code")));
    }

    #[test]
    fn only_http_addresses_are_fetched() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        for u in ["file:///etc/passwd", "ftp://x.test/a.js", "data:text/javascript,1", "node:fs", "blob:https://t.test/x", "javascript:alert(1)"] {
            let g = m.fetch_graph(&mut e, u, ModType::Js, CR, true).unwrap();
            assert!(m.take_actions().is_empty(), "{u}");
            let d = m.take_done();
            assert_eq!(d[0].0, g);
            assert!(matches!(d[0].1, Outcome::Network(..)), "{u}");
        }
    }

    #[test]
    fn a_preload_graph_does_not_bind() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        let g = m.fetch_graph(&mut e, A, ModType::Js, CR, false).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "import \"./b.js\"\n");
        fetches(&mut m);
        serve(&mut m, &mut e, "https://t.test/b.js", "");
        assert_eq!(done_ids(&mut m), [g]);
        assert!(e.deps.is_empty());
        // A later real graph for the same modules binds then.
        let g = m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        assert_eq!(done_ids(&mut m), [g]);
        assert_eq!(e.deps.len(), 1);
    }

    #[test]
    fn bodies_are_utf8_without_a_bom() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        let mut body = alloc::vec![0xEF, 0xBB, 0xBF];
        body.extend_from_slice(b"import \"./caf\xC3\xA9.js\"\n");
        m.on_body(&mut e, &url(A), &body, "");
        // The BOM is gone and the text is read as UTF-8 (percent-encoded in the address).
        assert_eq!(fetches(&mut m), ["https://t.test/caf%C3%A9.js"]);
        m.on_body(&mut e, &url("https://t.test/caf%C3%A9.js"), &[0xFF, 0xFE, b'x'], "");
        assert_eq!(m.take_done().len(), 1, "invalid UTF-8 is replaced, not refused");
    }

    #[test]
    fn redirects_move_the_base() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        m.on_body(&mut e, &url(A), b"import \"./b.js\"\n", "https://cdn.test/v2/a.js#h");
        assert_eq!(fetches(&mut m), ["https://cdn.test/v2/b.js"]);
        assert_eq!(m.base_of(&url(A)), Some("https://cdn.test/v2/a.js"));
        assert_eq!(m.file_of(&url(A)), Some("https://cdn.test/v2/a.js"));
    }

    #[test]
    fn a_late_event_for_a_finished_fetch_is_ignored() {
        let mut e = Fake::default();
        let mut m = Modules::<Fake>::default();
        m.fetch_graph(&mut e, A, ModType::Js, CR, true).unwrap();
        fetches(&mut m);
        serve(&mut m, &mut e, A, "");
        m.take_done();
        m.on_failed(&mut e, &url(A), String::from("late"));
        m.on_body(&mut e, &url(A), b"import \"./x.js\"", "");
        assert!(m.take_actions().is_empty());
        assert!(m.take_done().is_empty());
        // And for a key nobody knows.
        m.on_failed(&mut e, &url("https://nowhere.test/x.js"), String::new());
        assert_eq!(m.on_head(&mut e, &Key::Inline(9), 500, "", None), Verdict::Continue);
    }
}
