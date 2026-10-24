//! The embedding interface of the JavaScript engine. Embedders (the DOM bindings, the Browser,
//! tests, the guest benchmark) build against this file only; everything else in the crate is
//! internal. The shape is kept stable: anything added later is added beside what is here, and
//! a change to an existing item is noted in the CHANGES list below with what to do instead.
//!
//! # The model
//!
//! ```text
//!   Realm::new(Options) ──▶ Realm  (one heap, one global object, its own limits)
//!        │
//!        ├─ define globals: new_object / new_function / new_class / register_class +
//!        │                  new_host_object, set / define / define_accessor on global()
//!        ├─ eval(source, file) ──▶ JsResult<Value>          (a classic script)
//!        ├─ call(f, this, args) / construct(f, args)         (timers, event listeners)
//!        ├─ run_jobs()                                       (the microtask checkpoint)
//!        └─ report(&Exception) ──▶ ErrorReport               (message, file:line:col, stack)
//! ```
//!
//! - **One realm per page.** A [`Realm`] owns its heap and global object; nothing is shared
//!   between realms, and a [`Value`] from one realm means nothing in another.
//! - **Native functions** are Rust closures `Fn(&mut Realm, this, args) -> JsResult<Value>`
//!   ([`NativeFn`]). They get the same `&mut Realm` back, so they can create values, read
//!   properties and call back into script. A closure must not capture a [`Value`] that refers
//!   to a heap thing (the collector can't see inside closures): capture a [`Pinned`] instead,
//!   or keep state in a host object's `data` or in the embedder's own structures (see
//!   [`Realm::set_host`] for a realm-owned slot that natives can reach).
//! - **Host objects** ([`Realm::register_class`], [`Realm::new_host_object`]) are ordinary
//!   objects (they have a prototype and can hold properties) that also carry a `u64` of the
//!   embedder's data (a node id, say) and a class whose optional hooks can make indexed and
//!   named properties live (`nodeList[0]`, `element.dataset.foo`, `localStorage.key`).
//!   WebIDL-style attributes are better done as accessors on the prototype
//!   ([`Realm::define_accessor`]); hooks are for the open-ended keys.
//! - **Values and the collector.** The collector is precise and runs only while script runs
//!   (at the VM's safe points), never in the middle of a native call. A [`Value`] the embedder
//!   holds stays valid while it is reachable from script, while it is pinned ([`Realm::pin`]),
//!   or while it is a temporary: every heap value the API hands out (results, the `this` and
//!   `args` of a native call) is a temporary until the current native call returns, or, at
//!   the top level (outside any native call), until [`Realm::release_temps`]. Call
//!   `release_temps` once per turn of the event loop. A stale value reads as undefined and
//!   throws a TypeError when used as an object; it never reaches another object.
//! - **Errors.** Every fallible call returns [`JsResult`]. An [`Exception`] is either a thrown
//!   value (catchable by script) or a termination (out of the step budget, interrupted, out of
//!   memory), which script can't catch and which unwinds every frame. Natives make exceptions
//!   with [`Realm::throw_error`] / [`Realm::throw`] and pass them on with `?`.
//! - **Limits** ([`Limits`]): a heap budget (bytes), a step budget per entry from the host
//!   (eval, call, each job), a call-depth limit (a catchable RangeError like browsers), and an
//!   interrupt callback polled every [`INTERRUPT_STEPS`] steps ([`Realm::set_interrupt`]) so
//!   the embedder can stop a runaway script by wall-clock time. Nothing panics on hostile
//!   input; parsing, compiling and running are bounded.
//! - **Jobs.** Promise reactions and [`Realm::enqueue_job`] jobs wait in the realm's
//!   microtask queue until [`Realm::run_jobs`] (the embedder calls it after each script, each
//!   event dispatch and each timer). Rejections nobody handled are kept for
//!   [`Realm::take_unhandled_rejections`].
//! - **Time and randomness** come from the embedder: [`Realm::set_clock`] (ms since the
//!   epoch, for `Date`) and [`Options::tz_offset_minutes`] (the fixed local time zone);
//!   `Math.random` is seeded from [`Options::random_seed`].
//! - **Threads.** A realm is single-threaded (`!Send`). Parsing and compiling recurse with the
//!   source's nesting (bounded); give the thread at least 1 MB of stack (the Browser's main
//!   thread has 8 MB).
//! - **Not supported (yet):** `with`, direct `eval` seeing local variables (every `eval`
//!   is global), BigInt (literals parse and throw a TypeError when evaluated; there is no
//!   BigInt global and no BigInt64Array / BigUint64Array), resizable
//!   ArrayBuffers, Intl (the `toLocale*String` methods format en-US style; the Browser adds
//!   a script-side Intl), String.prototype.normalize (returns the string as is), real
//!   weak references (WeakRef and FinalizationRegistry exist but hold their targets strongly
//!   and never call back), SharedArrayBuffer and Atomics, symbols as WeakMap keys, the ES2024+ additions (iterator
//!   helpers, Set methods, `Array.fromAsync`, RegExp `v` flag, duplicate named groups and
//!   modifiers). Date has a fixed local offset (no daylight-saving rules). ES modules,
//!   private class members (`#x`) and RegExp match indices (`d` flag) are supported.
//!
//! # CHANGES
//! - Errors say where. [`ErrorReport`] has two new fields, `snippet` (the source line of the
//!   top frame, at most 60 characters either side of the column, `...` where it is cut,
//!   indentation removed) and `snippet_col` (the position in it): build one with
//!   `..Default::default()`. Its `Display` ends with that line and a caret under the column
//!   (`    672 | text` and `        |   ^`). Positions are per link of a chain: a property read
//!   or a method call is at the property's name, an index at its `[`, a call of something that
//!   is not a name at its `(` (they were all at the start of the chain; a stack frame, the
//!   `line`/`column` of a report and the position `window.onerror` is told move to the failing
//!   link). `x is not a function` words the callee as written, `a.b(...).c`, and a stack frame
//!   names an unnamed function by where it was stored (`a.b = function () {}` is `a.b`; the
//!   function's `name` property stays `""`).
//! - Heap budgets that move. New [`Realm::set_heap_limit`] (change [`Limits::max_heap_bytes`] of a
//!   running realm; [`Realm::set_limits`] uses it, and a realm over the new budget collects
//!   at its next safe point and runs out of memory only if it still holds more) and
//!   [`Realm::set_memory_hook`] (asked for the budget every [`INTERRUPT_STEPS`] steps with
//!   the heap in use; it answers with a [`HeapAnswer`]). Nothing that worked changes.
//! - Big scripts. A script is parsed, resolved and compiled one top-level statement at a
//!   time and each statement's tree is dropped as soon as it has bytecode; a function of 64
//!   bytes or more is parsed once (for early errors and the names it uses) and left as a
//!   stub that is compiled on its first call (`Code::lazy`; errors in a function body still
//!   come when the script loads). Number and string array literals of 16 or more items are
//!   constants. The parse/compile limit is now an allowance for the biggest piece alive at
//!   once ([`Limits::max_compile_bytes`], default 96 MiB, a new field: build `Limits` with
//!   `..Default::default()`), not a cumulative count of the script, and the source text and
//!   the bytecode a realm keeps (stubs included) are charged to [`Limits::max_heap_bytes`],
//!   so a script whose code does not fit the heap is refused early instead of exhausting
//!   memory. The error for a part bigger than the allowance is still a SyntaxError "the code
//!   is too large to fit in memory"; a function too big to compile when it is first called
//!   throws a RangeError. `set_lazy_min_bytes` (and the env var `JSENGINE_LAZY_ALL=1`) make
//!   every function lazy, for tests.
//! - Added `Proxy` (all thirteen traps with the spec's invariant checks, `Proxy.revocable`,
//!   proxies of arrays and functions, `IsArray` through proxies), `WeakRef` and
//!   `FinalizationRegistry` (strong references; nothing is ever collected or called back),
//!   BigInt literals (they parse; evaluating one throws a TypeError), and
//!   [`Realm::set_clone_marker`] (a symbol under which the host keeps state on its objects;
//!   `structuredClone` refuses such objects). `for-in` and several builtins were made
//!   proxy-aware.
//! - Added ES modules (stage 2b). A module is compiled from source into a [`ModuleId`]
//!   ([`Realm::compile_module`]; [`Realm::compile_synthetic_module`] for modules the host
//!   builds, a JSON module say). The engine never does I/O: the host reads the requested
//!   specifiers ([`Realm::module_requests`], each a [`ModuleRequest`] with its import
//!   attributes), fetches and compiles them in whatever way and order it likes, and hands
//!   each dependency back ([`Realm::set_module_dependency`]); then [`Realm::link_module`]
//!   links the whole graph (missing or ambiguous exports are SyntaxErrors with the importing
//!   file, line and column) and [`Realm::evaluate_module`] runs it and returns a Promise
//!   (top-level await, cycles and async ordering follow the spec's algorithms).
//!   [`Realm::module_namespace`] gives the module namespace object, [`Realm::module_status`]
//!   and [`Realm::module_error`] the state. `import.meta` asks
//!   [`Realm::set_import_meta_hook`] to fill in the object (once per module) and `import()`,
//!   from modules and from classic scripts, asks [`Realm::set_dynamic_import_hook`] (a
//!   [`DynamicImport`]); the host answers later with [`Realm::finish_dynamic_import`] and
//!   the engine links, evaluates and settles the Promise. Modules count against the heap
//!   budget and [`Realm::set_max_modules`]; [`Realm::drop_module`] and
//!   [`Realm::drop_module_graph`] release them. `import`/`export` in a classic script are
//!   still a SyntaxError (and `import()` is allowed there). A script's `import()` with no
//!   hook set rejects with a TypeError. Linking, evaluation, `import()` and the module
//!   namespace objects are real; the engine runs a module's code with the realm's step
//!   budget and interrupt like any other script. Module code is strict, has no HTML-like
//!   comments, and `await` is reserved in it. Also new: private class members (`#x`,
//!   `#m()`, `static #s`, `#x in obj`), RegExp match indices (the `d` flag), and parameters
//!   with defaults are in their TDZ until initialized (`function f(a = b, b) {}` throws).
//! - Added [`Realm::set_rejection_hook`] ([`PromiseRejection`]: unhandled and late-handled
//!   promise rejections, the browser's `unhandledrejection` / `rejectionhandled`) and
//!   [`Realm::set_error_hook`] (exceptions thrown out of queued jobs, for `window.onerror`;
//!   the jobs go on). Both are optional; without them `run_jobs` and
//!   [`Realm::take_unhandled_rejections`] work as before.
//! - Added ArrayBuffer, the typed arrays (Int8, Uint8, Uint8Clamped, Int16, Uint16, Int32,
//!   Uint32, Float32, Float64), DataView, TextEncoder / TextDecoder (utf-8, utf-16le,
//!   latin1), `atob` / `btoa` and `structuredClone` to the engine, and the bytes plumbing
//!   for the embedder: [`Realm::new_array_buffer`], [`Realm::new_array_buffer_from_vec`],
//!   [`Realm::new_uint8_array`], [`Realm::new_uint8_array_from_vec`] (host bytes into script),
//!   [`Realm::buffer_bytes`], [`Realm::buffer_bytes_mut`], [`Realm::copy_bytes`],
//!   [`Realm::take_buffer_bytes`] (script bytes out to the host, borrowed, copied or moved),
//!   [`Realm::is_array_buffer`], [`Realm::is_typed_array`], [`Realm::is_array_buffer_view`]
//!   and [`Realm::set_max_buffer_bytes`] (the cap on one ArrayBuffer; buffer bytes also count
//!   against [`Limits::max_heap_bytes`]). New [`Intrinsic`] variants: `ArrayBuffer`,
//!   `ArrayBufferPrototype`, `Uint8Array`, `Uint8ArrayPrototype`, `TypedArrayPrototype`.
//! - The engine behind the API is real now (it was a stub that threw on `eval`).
//! - Added [`Realm::set_stack_limit`] (the native stack an entry may use; default 768 KB).
//! - Added [`WeakValue`], [`Realm::weak`] and [`Realm::upgrade`] (wrapper caches that let
//!   wrappers be collected), [`Realm::make_global_host`] (a host-object `window`),
//!   [`Realm::current_location`] and [`Realm::stack_trace`] (for a console).
//! - Documented that [`Limits::max_steps`] also counts work builtins do (string copies,
//!   array elements visited), so it bounds CPU time, not only loop iterations.
//! - First version.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

pub use crate::realm::Realm;
pub use crate::value::{Kind, Value};

use crate::object::ObjKind;
use crate::value::Val;

/// The result of anything that can throw.
pub type JsResult<T> = Result<T, Exception>;

/// A native function: `(realm, this, args) -> result`. Inside, [`Realm::new_target`] says
/// whether it was called with `new`.
pub type NativeFn = dyn Fn(&mut Realm, Value, &[Value]) -> JsResult<Value>;
pub(crate) type HostFn = NativeFn;

/// How often (in VM steps) the interrupt callback is polled.
pub const INTERRUPT_STEPS: u64 = 1 << 16;

/// Something thrown out of script or a native: a value script could catch, or a termination
/// it can't.
#[derive(Clone, Copy, Debug)]
pub struct Exception(pub(crate) Exc);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Exc {
    Throw(Val),
    Term(Termination),
}

/// What a memory hook says ([`Realm::set_memory_hook`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeapAnswer {
    Keep,
    /// The heap budget is this many bytes from now on.
    Limit(usize),
    /// Stop the script.
    Stop,
}

/// Why a script was stopped (uncatchable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    /// It ran more steps than [`Limits::max_steps`] in one entry.
    StepBudget,
    /// The interrupt callback said stop ([`Realm::set_interrupt`]).
    Interrupted,
    /// The heap grew past [`Limits::max_heap_bytes`].
    OutOfMemory,
}

impl Exception {
    /// The thrown value, unless this is a termination.
    pub fn value(&self) -> Option<Value> {
        match self.0 {
            Exc::Throw(v) => Some(Value(v)),
            Exc::Term(_) => None,
        }
    }
    pub fn termination(&self) -> Option<Termination> {
        match self.0 {
            Exc::Throw(_) => None,
            Exc::Term(t) => Some(t),
        }
    }
    pub fn is_uncatchable(&self) -> bool {
        matches!(self.0, Exc::Term(_))
    }
}

/// The error constructors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Error,
    TypeError,
    RangeError,
    SyntaxError,
    ReferenceError,
    URIError,
    EvalError,
}

impl ErrorKind {
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Error => "Error",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::RangeError => "RangeError",
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::ReferenceError => "ReferenceError",
            ErrorKind::URIError => "URIError",
            ErrorKind::EvalError => "EvalError",
        }
    }
}

/// A readable account of an exception, for a console or an error page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ErrorReport {
    /// "TypeError", "Error", ... or "Uncaught" for a thrown non-error value, or the
    /// termination ("Terminated").
    pub name: String,
    pub message: String,
    /// Where it was thrown (the innermost script frame): the file name given to `eval`,
    /// 1-based line and column (0 when unknown).
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// "    at f (file:line:col)" lines, innermost first, joined with '\n'.
    pub stack: String,
    /// The line of source at `line`:`column`, cut to at most 60 characters on each side of
    /// the position (`...` marks a cut) with its indentation removed, and `snippet_col`, the
    /// character index of the position within it. Empty when the source is not at hand (a
    /// native error with no script running, a termination).
    pub snippet: String,
    pub snippet_col: u32,
    pub uncatchable: bool,
}

impl core::fmt::Display for ErrorReport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.message.is_empty() {
            write!(f, "{}", self.name)?;
        } else {
            write!(f, "{}: {}", self.name, self.message)?;
        }
        if !self.stack.is_empty() {
            write!(f, "\n{}", self.stack)?;
        } else if !self.file.is_empty() {
            write!(f, "\n    at {}:{}:{}", self.file, self.line, self.column)?;
        }
        if !self.snippet.is_empty() {
            // "    672 | the source line" and a caret under the position.
            let w = alloc::format!("{}", self.line).len();
            write!(f, "\n    {:>w$} | {}", self.line, self.snippet, w = w)?;
            write!(f, "\n    {:>w$} | ", "", w = w)?;
            for _ in 0..self.snippet_col {
                f.write_str(" ")?;
            }
            f.write_str("^")?;
        }
        Ok(())
    }
}

/// What the rejection hook ([`Realm::set_rejection_hook`]) is told.
#[derive(Clone, Copy, Debug)]
pub enum PromiseRejection {
    /// A promise was rejected and nothing handled it by the end of the microtask checkpoint
    /// (the browser's `unhandledrejection`). `reason` is the rejection value.
    Unhandled { promise: Value, reason: Value },
    /// A promise already reported as unhandled got a handler after all (the browser's
    /// `rejectionhandled`).
    Handled { promise: Value },
}

/// Called with each [`PromiseRejection`].
pub type RejectionHook = dyn FnMut(&mut Realm, PromiseRejection);

/// Called with an uncaught exception ([`Realm::set_error_hook`]).
pub type ErrorHook = dyn FnMut(&mut Realm, &Exception);

/// Per-realm budgets. A zero means "no limit" for `max_steps`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The heap budget in bytes (objects, strings, the VM stack). Default 64 MB.
    pub max_heap_bytes: usize,
    /// VM steps allowed per entry from the host (one eval, call or job): loop iterations,
    /// calls, and builtins' work (copied string units, visited elements) in proportion.
    /// Default 0 (none: use the interrupt callback for wall-clock limits).
    pub max_steps: u64,
    /// Nested calls (script and native) before a RangeError. Default 1000.
    pub max_call_depth: u32,
    /// The longest source text `eval` and `Function` accept. Default 32 MB.
    pub max_source_bytes: usize,
    /// The most memory (an estimate, in bytes) that the syntax tree of one parse may take
    /// at once. Compiling has its own allowance, apart from the heap budget: the tree only
    /// lives while a script or one function is parsed and compiled, and big functions are
    /// parsed in pieces (each is dropped as soon as it is checked and parsed again when it is
    /// first called), so the biggest thing that is parsed at once is one function body or the
    /// statements outside any function. A script with a part bigger than this fails with a
    /// SyntaxError ("the code is too large to fit in memory"). Default 96 MB (about 1.3 million
    /// tokens: a function of about 1.3 MB of minified code, or as many statements).
    pub max_compile_bytes: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_heap_bytes: 64 << 20,
            max_steps: 0,
            max_call_depth: 1000,
            max_source_bytes: 32 << 20,
            max_compile_bytes: 96 << 20,
        }
    }
}

/// How to make a realm.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub limits: Limits,
    /// Local time = UTC + this many minutes (Date's local-time methods).
    pub tz_offset_minutes: i32,
    /// Seed for Math.random (any value; 0 is fine).
    pub random_seed: u64,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            limits: Limits::default(),
            tz_offset_minutes: 0,
            random_seed: 0x5eed_1234_abcd_0001,
        }
    }
}

/// A property key.
#[derive(Clone, Copy, Debug)]
pub enum Key<'a> {
    Str(&'a str),
    Index(u32),
    /// Any value, converted with ToPropertyKey (a symbol stays a symbol). In hooks this
    /// variant only ever carries a symbol.
    Value(Value),
}

impl<'a> From<&'a str> for Key<'a> {
    fn from(s: &'a str) -> Key<'a> {
        Key::Str(s)
    }
}
impl<'a> From<&'a String> for Key<'a> {
    fn from(s: &'a String) -> Key<'a> {
        Key::Str(s.as_str())
    }
}
impl From<u32> for Key<'_> {
    fn from(i: u32) -> Key<'static> {
        Key::Index(i)
    }
}
impl From<Value> for Key<'_> {
    fn from(v: Value) -> Key<'static> {
        Key::Value(v)
    }
}

/// A key a host class reports from [`HostClass::keys`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnedKey {
    Index(u32),
    Str(String),
}

/// Property attributes for [`Realm::define`] and [`Realm::define_accessor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attrs(pub u8);

impl Attrs {
    pub const WRITABLE: Attrs = Attrs(1);
    pub const ENUMERABLE: Attrs = Attrs(2);
    pub const CONFIGURABLE: Attrs = Attrs(4);
    /// Writable, enumerable, configurable: what assignment makes.
    pub const DEFAULT: Attrs = Attrs(7);
    /// Writable and configurable but not enumerable: how builtin methods are defined.
    pub const METHOD: Attrs = Attrs(5);
    /// Nothing: read-only, hidden, permanent.
    pub const FROZEN: Attrs = Attrs(0);
    pub fn has(self, a: Attrs) -> bool {
        self.0 & a.0 == a.0
    }
}

impl core::ops::BitOr for Attrs {
    type Output = Attrs;
    fn bitor(self, o: Attrs) -> Attrs {
        Attrs(self.0 | o.0)
    }
}

/// Which keys a [`HostClass`]'s hooks want to see (others go straight to the ordinary
/// object, at no cost).
pub mod hooks {
    /// Array-index keys ("0", "1", ...: `Key::Index`).
    pub const INDEXED: u8 = 1;
    /// Every other string key (`Key::Str`).
    pub const NAMED: u8 = 2;
    /// Symbol keys (`Key::Value`).
    pub const SYMBOLS: u8 = 4;
    pub const NONE: u8 = 0;
}

/// A class of host objects. Every hook has a default that does nothing, so a class with no
/// live properties is just `struct Node; impl HostClass for Node {}` (plus a prototype with
/// accessors and methods). Hooks run with the object first in line: `get` is asked before
/// the object's own ordinary properties and its prototype chain; returning `Ok(None)` (or
/// `Ok(false)`) falls through to the ordinary behaviour. Hooks are only called for the key
/// kinds [`HostClass::hooks`] asks for.
pub trait HostClass: 'static {
    /// The class name (`Object.prototype.toString` shows `[object Name]`).
    fn name(&self) -> &str {
        "Object"
    }
    /// Which keys go to the hooks: a mix of [`hooks::INDEXED`], [`hooks::NAMED`],
    /// [`hooks::SYMBOLS`]. Default: none.
    fn hooks(&self) -> u8 {
        hooks::NONE
    }
    /// A live own property: `Some(value)` to answer, `None` to fall through.
    fn get(
        &self,
        realm: &mut Realm,
        obj: Value,
        data: u64,
        key: Key<'_>,
    ) -> JsResult<Option<Value>> {
        let _ = (realm, obj, data, key);
        Ok(None)
    }
    /// An assignment (or a data-property definition): `true` if the host took it, `false`
    /// to store it as an ordinary property.
    fn set(
        &self,
        realm: &mut Realm,
        obj: Value,
        data: u64,
        key: Key<'_>,
        value: Value,
    ) -> JsResult<bool> {
        let _ = (realm, obj, data, key, value);
        Ok(false)
    }
    /// `key in obj` for a live property: `Some(true)` if it exists, `None` to ask the ordinary
    /// object (the default answers from `get`).
    fn has(
        &self,
        realm: &mut Realm,
        obj: Value,
        data: u64,
        key: Key<'_>,
    ) -> JsResult<Option<bool>> {
        Ok(self.get(realm, obj, data, key)?.map(|_| true))
    }
    /// `delete obj[key]`: `Some(result)` if the host handled it.
    fn delete(
        &self,
        realm: &mut Realm,
        obj: Value,
        data: u64,
        key: Key<'_>,
    ) -> JsResult<Option<bool>> {
        let _ = (realm, obj, data, key);
        Ok(None)
    }
    /// The live own keys, listed before the ordinary ones (for `Object.keys`, `for-in`, ...).
    fn keys(&self, realm: &mut Realm, obj: Value, data: u64) -> JsResult<Vec<OwnedKey>> {
        let _ = (realm, obj, data);
        Ok(Vec::new())
    }
    /// The object was collected. Called after the collection, outside script; it can't
    /// touch the realm.
    fn finalize(&self, data: u64) {
        let _ = data;
    }
}

/// A registered host class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClassId(pub(crate) u32);

/// A value kept alive until [`Realm::unpin`]. Not `Clone`: pin again for a second handle.
/// Dropping a `Pinned` without unpinning keeps the value for the realm's life.
#[derive(Debug, PartialEq, Eq, Hash)]
pub struct Pinned(pub(crate) u32);

/// A handle that doesn't keep its value alive (for caches of wrappers): see
/// [`Realm::weak`] and [`Realm::upgrade`]. Copy; never dangles (a collected value reads as
/// None, even if its slot was reused).
#[derive(Clone, Copy, Debug)]
pub struct WeakValue(pub(crate) Val);

/// A mark in the temporaries, for [`Realm::release_temps_to`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TempMark(pub(crate) usize);

/// The well-known symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WellKnown {
    Iterator,
    AsyncIterator,
    HasInstance,
    ToPrimitive,
    ToStringTag,
    Species,
    IsConcatSpreadable,
    Unscopables,
    Match,
    MatchAll,
    Replace,
    Search,
    Split,
}

/// Builtin objects an embedder may want (to put on prototypes, or to compare against).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intrinsic {
    ObjectPrototype,
    FunctionPrototype,
    ArrayPrototype,
    ErrorPrototype,
    IteratorPrototype,
    PromisePrototype,
    /// `Array.prototype.values` (also `Array.prototype[Symbol.iterator]`): works on any
    /// array-like, so a NodeList prototype can use it as its `[Symbol.iterator]`.
    ArrayValues,
    /// `Array.prototype.forEach`, likewise generic.
    ArrayForEach,
    Object,
    Function,
    Array,
    Promise,
    Error,
    ArrayBuffer,
    ArrayBufferPrototype,
    Uint8Array,
    Uint8ArrayPrototype,
    /// `%TypedArray%.prototype`.
    TypedArrayPrototype,
}

/// Heap figures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeapStats {
    /// Estimated bytes in use (live at the last collection plus allocated since).
    pub used_bytes: usize,
    /// Estimated bytes live after the last collection.
    pub live_bytes: usize,
    pub objects: usize,
    pub strings: usize,
    pub collections: u64,
    /// The heap budget now ([`Limits::max_heap_bytes`], as [`Realm::set_heap_limit`] left it).
    pub limit_bytes: usize,
}

/// Where a script comes from, for [`Realm::eval_with`].
#[derive(Clone, Copy, Debug)]
pub struct ScriptOptions<'a> {
    /// The file name in error reports and stacks (a URL, or "inline").
    pub file: &'a str,
    /// The line the source starts on (1 for a file; an inline `<script>`'s line in the
    /// page), so errors point at the page's own line numbers.
    pub first_line: u32,
    /// The column of the first line (1-based).
    pub first_column: u32,
}

impl Default for ScriptOptions<'_> {
    fn default() -> Self {
        ScriptOptions {
            file: "script",
            first_line: 1,
            first_column: 1,
        }
    }
}

/// A module record (a compiled module, or a synthetic one the host made). Copy; valid until
/// [`Realm::drop_module`] / [`Realm::drop_module_graph`] (a dropped or foreign id is refused
/// with a TypeError, never confused with another module).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub(crate) u32, pub(crate) u32);

/// How to compile a module ([`Realm::compile_module`]).
#[derive(Clone, Copy, Debug)]
pub struct ModuleOptions<'a> {
    /// The module's name in error reports and stacks, and the base the host resolves its
    /// own specifiers against: normally its URL.
    pub file: &'a str,
    /// The line the source starts on (1 for a file; an inline `<script type="module">`'s
    /// line in the page).
    pub first_line: u32,
    /// The column of the first line (1-based).
    pub first_column: u32,
}

impl Default for ModuleOptions<'_> {
    fn default() -> Self {
        ModuleOptions {
            file: "module",
            first_line: 1,
            first_column: 1,
        }
    }
}

/// One thing a module (or an `import()` call) asks the host for: a specifier and its import
/// attributes (`with { type: "json" }` gives `("type", "json")`), where it was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleRequest {
    pub specifier: String,
    /// The attributes, sorted by key.
    pub attributes: Vec<(String, String)>,
    /// Where the first request for this pair is (the file is the module's own).
    pub line: u32,
    pub column: u32,
}

impl ModuleRequest {
    /// The value of attribute `key`, if present.
    pub fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Where a module is in its life (the spec's cyclic module record status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleStatus {
    /// Compiled; dependencies may still be missing.
    Unlinked,
    /// [`Realm::link_module`] is running (only seen from a native called while it runs).
    Linking,
    Linked,
    /// Running (or waiting on a dependency that is).
    Evaluating,
    /// Done synchronously but waiting for async dependencies, or suspended at a top-level
    /// `await`.
    EvaluatingAsync,
    /// Finished, with a result or with an error ([`Realm::module_error`]).
    Evaluated,
}

/// An `import()` call waiting for the host ([`Realm::set_dynamic_import_hook`]).
#[derive(Clone, Debug)]
pub struct DynamicImport {
    /// Pass this to [`Realm::finish_dynamic_import`].
    pub id: DynamicImportId,
    /// The module whose code called `import()`, or None for a classic script.
    pub referrer: Option<ModuleId>,
    /// The file name of the calling script or module (what it was compiled with): the host
    /// resolves a relative specifier against this (or the page, for an inline script).
    pub referrer_file: String,
    pub request: ModuleRequest,
}

/// Names one pending `import()` for [`Realm::finish_dynamic_import`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DynamicImportId(pub(crate) u32);

/// Called the first time a module evaluates `import.meta`, with the new (null-prototype)
/// meta object to fill in (`url`, `resolve`, ...).
pub type ImportMetaHook = dyn FnMut(&mut Realm, ModuleId, Value) -> JsResult<()>;

/// Called when script runs `import()`. The host starts loading and later calls
/// [`Realm::finish_dynamic_import`]. An `Err` rejects the import's Promise with that error.
pub type DynamicImportHook = dyn FnMut(&mut Realm, DynamicImport) -> JsResult<()>;

impl Realm {
    // ---- creating ----

    /// A new realm with the standard builtins on its global object.
    pub fn new(options: Options) -> Realm {
        Realm::create(options)
    }

    /// The global object (`globalThis`; the Browser makes it the `window`).
    pub fn global(&self) -> Value {
        Value(Val::Obj(self.global))
    }

    /// Makes the global object a host object of `class` (for live named properties on
    /// `window`, like elements by id). Its ordinary properties (the builtins, globals) stay.
    pub fn make_global_host(&mut self, class: ClassId, data: u64) {
        let g = self.global;
        self.om(g).kind = crate::object::ObjKind::Host {
            class: class.0,
            data,
        };
    }

    /// Inside a native: the innermost script position (file, line, column) — where
    /// `console.log` was called from, say. None when no script is running.
    pub fn current_location(&mut self) -> Option<(String, u32, u32)> {
        self.stack_frames(1)
            .into_iter()
            .next()
            .map(|(_, f, l, c)| (f, l, c))
    }

    /// Inside a native: the script stack as "    at f (file:line:col)" lines (for
    /// `console.trace`).
    pub fn stack_trace(&mut self) -> String {
        self.current_site().stack
    }

    // ---- running script ----

    /// Runs `source` as a classic script in the global scope and returns its completion
    /// value. Microtasks are not run: call [`Realm::run_jobs`] after.
    pub fn eval(&mut self, source: &str, file: &str) -> JsResult<Value> {
        self.eval_with(
            source,
            &ScriptOptions {
                file,
                ..ScriptOptions::default()
            },
        )
    }

    /// [`Realm::eval`] with a starting line and column (for inline scripts).
    pub fn eval_with(&mut self, source: &str, opts: &ScriptOptions<'_>) -> JsResult<Value> {
        let r = self.entry(|r| r.eval_script(source, opts));
        self.out(r)
    }

    /// Only parses `source`: `Ok` if it is a valid script, else the SyntaxError's report.
    pub fn check_syntax(&mut self, source: &str, file: &str) -> Result<(), ErrorReport> {
        self.syntax_check(source, file)
    }

    /// Makes a function from parameter names and a body, like `new Function(...params, body)`
    /// but with a file name for errors: for `on*` attributes, say
    /// `compile_function(&["event"], "doThing(this)", "page.html")`.
    pub fn compile_function(&mut self, params: &[&str], body: &str, file: &str) -> JsResult<Value> {
        let r = self.entry(|r| r.make_function_from_source(params, body, file));
        self.out(r)
    }

    /// Calls `f` with `this` and `args`.
    pub fn call(&mut self, f: Value, this: Value, args: &[Value]) -> JsResult<Value> {
        let f = self.check_val(f.0);
        let this = self.check_val(this.0);
        let args: Vec<Val> = args.iter().map(|a| self.check_val(a.0)).collect();
        let r = self.entry(|r| r.call_v(f, this, &args));
        self.out(r)
    }

    /// `new f(...args)`.
    pub fn construct(&mut self, f: Value, args: &[Value]) -> JsResult<Value> {
        let f = self.check_val(f.0);
        let args: Vec<Val> = args.iter().map(|a| self.check_val(a.0)).collect();
        let r = self.entry(|r| r.construct_v(f, &args, f));
        self.out(r)
    }

    /// `obj[name](...args)`.
    pub fn call_method(&mut self, obj: Value, name: &str, args: &[Value]) -> JsResult<Value> {
        let o = self.check_val(obj.0);
        let args: Vec<Val> = args.iter().map(|a| self.check_val(a.0)).collect();
        let r = self.entry(|r| {
            let k = r.key_str(name);
            let f = r.get_v(o, k)?;
            r.call_v(f, o, &args)
        });
        self.out(r)
    }

    pub fn is_callable(&self, v: Value) -> bool {
        let v = self.check_val(v.0);
        self.callable(v)
    }

    pub fn is_constructor(&self, v: Value) -> bool {
        let v = self.check_val(v.0);
        self.constructible(v)
    }

    // ---- jobs ----

    /// Runs queued jobs (promise reactions, [`Realm::enqueue_job`]s) until the queue is empty,
    /// including the ones they queue. If a job throws, that exception is returned and the
    /// rest stay queued: report it and call `run_jobs` again.
    pub fn run_jobs(&mut self) -> JsResult<()> {
        let r = self.entry(|r| r.drain_jobs());
        if self.rejection_hook.is_some() {
            self.entry(|r| {
                r.deliver_rejections();
                Ok(())
            })?;
        }
        r
    }

    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Queues `f(...args)` as a microtask (`queueMicrotask`).
    pub fn enqueue_job(&mut self, f: Value, args: &[Value]) {
        let f = self.check_val(f.0);
        let args: Vec<Val> = args.iter().map(|a| self.check_val(a.0)).collect();
        self.push_call_job(f, args);
    }

    /// Drops every queued job (when a page is torn down or its script is stopped).
    pub fn clear_jobs(&mut self) {
        self.jobs.clear();
    }

    /// The reasons of promises rejected with no handler since the last call (as of the end
    /// of the last `run_jobs`), oldest first.
    pub fn take_unhandled_rejections(&mut self) -> Vec<Value> {
        let v = self.unhandled_rejections();
        v.into_iter().map(|x| self.out_val(x)).collect()
    }

    /// Installs a hook that is told about rejected promises nobody handled, the way a page
    /// gets `unhandledrejection` and `rejectionhandled`. It is called at the end of
    /// [`Realm::run_jobs`] (the microtask checkpoint), after the queue drained, once per
    /// promise; a promise that gets a handler later is reported as `Handled` at the end of
    /// a later `run_jobs`. The hook may call back into the realm (to dispatch an event
    /// whose listener runs script); promises it rejects itself are reported in the same
    /// checkpoint. While a hook is installed, [`Realm::take_unhandled_rejections`] finds
    /// nothing (the hook got them). `None` removes it.
    pub fn set_rejection_hook(&mut self, hook: Option<Box<RejectionHook>>) {
        self.rejection_hook = hook;
    }

    /// Installs a hook for exceptions nobody can catch: one thrown out of a queued job (a
    /// `queueMicrotask` callback, a promise reaction). The hook gets the exception (use
    /// [`Realm::report`] for its message and position, [`Exception::value`] for the thrown
    /// value, the way `window.onerror` wants them) and the jobs go on, so one failing
    /// callback no longer stops [`Realm::run_jobs`]. Uncatchable terminations (budget,
    /// interrupt, memory) are never given to the hook: `run_jobs` still returns them.
    /// Errors thrown from `eval`, `call` and `construct` are returned to the embedder as
    /// before. `None` removes the hook.
    pub fn set_error_hook(&mut self, hook: Option<Box<ErrorHook>>) {
        self.error_hook = hook;
    }

    /// A new pending promise and its resolve and reject functions.
    pub fn new_promise(&mut self) -> (Value, Value, Value) {
        let (p, res, rej) = self.promise_with_resolvers();
        (
            self.out_val(Val::Obj(p)),
            self.out_val(res),
            self.out_val(rej),
        )
    }

    // ---- primitives and conversions ----

    /// A string value.
    pub fn string(&mut self, s: &str) -> Value {
        let id = self.heap.new_str_vec(crate::string::utf16(s));
        self.out_val(Val::Str(id))
    }

    /// A string value from UTF-16 code units.
    pub fn string_utf16(&mut self, units: &[u16]) -> Value {
        let id = self.heap.new_str(units);
        self.out_val(Val::Str(id))
    }

    /// The contents of a string value (lone surrogates become U+FFFD); None if `v` isn't a
    /// string. No conversion: see [`Realm::to_string`].
    pub fn get_string(&mut self, v: Value) -> Option<String> {
        match self.check_val(v.0) {
            Val::Str(s) => Some(crate::string::to_rust(self.heap.units(s))),
            _ => None,
        }
    }

    /// A string value's UTF-16 code units.
    pub fn get_string_utf16(&mut self, v: Value) -> Option<Vec<u16>> {
        match self.check_val(v.0) {
            Val::Str(s) => Some(self.heap.units(s).to_vec()),
            _ => None,
        }
    }

    /// ToString (may call `toString`/`valueOf`; a symbol throws).
    pub fn to_string(&mut self, v: Value) -> JsResult<String> {
        let v = self.check_val(v.0);

        self.entry(|r| {
            let s = r.to_str(v)?;
            Ok(crate::string::to_rust(r.heap.units(s)))
        })
    }

    /// ToNumber.
    pub fn to_number(&mut self, v: Value) -> JsResult<f64> {
        let v = self.check_val(v.0);
        self.entry(|r| r.to_num(v))
    }

    /// ToInt32 (`x | 0`).
    pub fn to_int32(&mut self, v: Value) -> JsResult<i32> {
        let v = self.check_val(v.0);
        self.entry(|r| r.to_i32(v))
    }

    /// ToBoolean (never throws or runs script).
    pub fn to_boolean(&self, v: Value) -> bool {
        let v = self.check_val(v.0);
        self.truthy(v)
    }

    /// `typeof v`.
    pub fn type_of(&self, v: Value) -> &'static str {
        let v = self.check_val(v.0);
        self.typeof_str(v)
    }

    /// `a === b`.
    pub fn strict_equals(&mut self, a: Value, b: Value) -> bool {
        let a = self.check_val(a.0);
        let b = self.check_val(b.0);
        self.strict_eq(a, b)
    }

    /// `Object.is(a, b)`.
    pub fn same_value(&mut self, a: Value, b: Value) -> bool {
        let a = self.check_val(a.0);
        let b = self.check_val(b.0);
        self.same_val(a, b)
    }

    /// A console-style rendering of a value (`[1, 2, {a: "x"}]`, `ƒ name()`, `Error: msg`):
    /// never runs script (getters show as `[Getter]`), depth- and length-limited.
    pub fn inspect(&mut self, v: Value) -> String {
        let v = self.check_val(v.0);
        self.inspect_val(v)
    }

    // ---- objects ----

    /// `{}`.
    pub fn new_object(&mut self) -> Value {
        let o = self.new_plain();
        self.out_val(Val::Obj(o))
    }

    /// `Object.create(proto)` (proto may be null).
    pub fn new_object_with_proto(&mut self, proto: Value) -> Value {
        let p = self.check_val(proto.0).as_obj();
        let o = self.alloc(p, crate::object::ObjKind::Ordinary);
        self.out_val(Val::Obj(o))
    }

    /// An array of `items`.
    pub fn new_array(&mut self, items: &[Value]) -> Value {
        let v: Vec<Val> = items.iter().map(|a| self.check_val(a.0)).collect();
        let o = self.new_array_from(v);
        self.out_val(Val::Obj(o))
    }

    /// `arr.push(v)` for an array (or anything with a length); returns the new length.
    pub fn array_push(&mut self, arr: Value, v: Value) -> JsResult<u32> {
        let a = self.check_val(arr.0);
        let v = self.check_val(v.0);
        self.entry(|r| {
            let lk = r.key(crate::names::N::length);
            let lv = r.get_v(a, lk)?;
            let len = r.to_length(lv)?;
            let k = r.to_key(Val::num(len))?;
            r.set_v(a, k, v, true)?;
            r.set_v(a, lk, Val::num(len + 1.0), true)?;
            Ok((len + 1.0) as u32)
        })
    }

    /// `Array.isArray(v)`.
    pub fn is_array(&self, v: Value) -> bool {
        match self.check_val(v.0) {
            Val::Obj(o) => matches!(self.o(o).kind, crate::object::ObjKind::Array(_)),
            _ => false,
        }
    }

    /// Whether `v` is an Error object (made by an Error constructor or a builtin).
    pub fn is_error(&self, v: Value) -> bool {
        match self.check_val(v.0) {
            Val::Obj(o) => matches!(self.o(o).kind, crate::object::ObjKind::Error(_)),
            _ => false,
        }
    }

    /// `obj[key]` (primitives work too: `"abc".length`).
    pub fn get<'k>(&mut self, obj: Value, key: impl Into<Key<'k>>) -> JsResult<Value> {
        let o = self.check_val(obj.0);
        let key = key.into();
        let r = self.entry(|r| {
            let k = r.api_key(key)?;
            r.get_v(o, k)
        });
        self.out(r)
    }

    /// `obj[key] = value`, with strict-mode semantics: a rejected assignment (read-only
    /// property, non-extensible object) throws a TypeError.
    pub fn set<'k>(&mut self, obj: Value, key: impl Into<Key<'k>>, value: Value) -> JsResult<()> {
        let o = self.check_val(obj.0);
        let v = self.check_val(value.0);
        let key = key.into();
        self.entry(|r| {
            let k = r.api_key(key)?;
            r.set_v(o, k, v, true)
        })
    }

    /// `key in obj` (throws if `obj` isn't an object).
    pub fn has<'k>(&mut self, obj: Value, key: impl Into<Key<'k>>) -> JsResult<bool> {
        let o = self.check_val(obj.0);
        let key = key.into();
        self.entry(|r| {
            let o = r.require_obj(o, "'in' on a non-object")?;
            let k = r.api_key(key)?;
            r.has_property(o, k)
        })
    }

    /// Whether `obj` has `key` as an own property.
    pub fn has_own<'k>(&mut self, obj: Value, key: impl Into<Key<'k>>) -> JsResult<bool> {
        let o = self.check_val(obj.0);
        let key = key.into();
        self.entry(|r| {
            let o = r.require_obj(o, "hasOwn on a non-object")?;
            let k = r.api_key(key)?;
            r.has_own_p(o, k)
        })
    }

    /// `delete obj[key]`; false if the property is non-configurable.
    pub fn delete<'k>(&mut self, obj: Value, key: impl Into<Key<'k>>) -> JsResult<bool> {
        let o = self.check_val(obj.0);
        let key = key.into();
        self.entry(|r| {
            let o = r.require_obj(o, "delete on a non-object")?;
            let k = r.api_key(key)?;
            r.delete_p(o, k)
        })
    }

    /// Defines (or redefines) a data property with the given attributes
    /// (`Object.defineProperty` semantics; throws if that would be rejected).
    pub fn define<'k>(
        &mut self,
        obj: Value,
        key: impl Into<Key<'k>>,
        value: Value,
        attrs: Attrs,
    ) -> JsResult<()> {
        let o = self.check_val(obj.0);
        let v = self.check_val(value.0);
        let key = key.into();
        self.entry(|r| {
            let o = r.require_obj(o, "define on a non-object")?;
            let k = r.api_key(key)?;
            let d = crate::object::Desc::data(v, attrs.0 & 7);
            if r.define_own(o, k, d)? {
                Ok(())
            } else {
                let ks = r.key_display(k);
                Err(r.type_error(&alloc::format!("Cannot redefine property: {ks}")))
            }
        })
    }

    /// Defines an accessor property; `getter` / `setter` are functions or undefined. Attrs:
    /// only ENUMERABLE and CONFIGURABLE matter.
    pub fn define_accessor<'k>(
        &mut self,
        obj: Value,
        key: impl Into<Key<'k>>,
        getter: Value,
        setter: Value,
        attrs: Attrs,
    ) -> JsResult<()> {
        let o = self.check_val(obj.0);
        let g = self.check_val(getter.0);
        let s = self.check_val(setter.0);
        let key = key.into();
        self.entry(|r| {
            let o = r.require_obj(o, "define on a non-object")?;
            let k = r.api_key(key)?;
            let d = crate::object::Desc {
                get: Some(g),
                set: Some(s),
                enumerable: Some(attrs.0 & 2 != 0),
                configurable: Some(attrs.0 & 4 != 0),
                ..Default::default()
            };
            if r.define_own(o, k, d)? {
                Ok(())
            } else {
                let ks = r.key_display(k);
                Err(r.type_error(&alloc::format!("Cannot redefine property: {ks}")))
            }
        })
    }

    /// Defines a method: a native function under `name`, non-enumerable (like builtins).
    /// Returns the function.
    pub fn define_method<F>(&mut self, obj: Value, name: &str, arity: u32, f: F) -> JsResult<Value>
    where
        F: Fn(&mut Realm, Value, &[Value]) -> JsResult<Value> + 'static,
    {
        let fv = self.new_function(name, arity, f);
        self.define(obj, name, fv, Attrs::METHOD)?;
        Ok(fv)
    }

    /// Defines an accessor made of native functions (`get name` / `set name`),
    /// configurable and, like WebIDL attributes, enumerable.
    pub fn define_getter<G>(
        &mut self,
        obj: Value,
        name: &str,
        getter: G,
        setter: Option<Box<NativeFn>>,
    ) -> JsResult<()>
    where
        G: Fn(&mut Realm, Value, &[Value]) -> JsResult<Value> + 'static,
    {
        let gname = alloc::format!("get {name}");
        let g = self.new_function(&gname, 0, getter);
        let s = match setter {
            Some(sf) => {
                let sname = alloc::format!("set {name}");
                let rc: Rc<NativeFn> = Rc::from(sf);
                self.new_function_rc(&sname, 1, rc)
            }
            None => Value::UNDEFINED,
        };
        self.define_accessor(obj, name, g, s, Attrs::ENUMERABLE | Attrs::CONFIGURABLE)
    }

    /// The own enumerable string keys, like `Object.keys(obj)`.
    pub fn own_keys(&mut self, obj: Value) -> JsResult<Vec<Value>> {
        let o = self.check_val(obj.0);
        let r = self.entry(|r| {
            let o = r.require_obj(o, "keys of a non-object")?;
            let ks = r.own_keys_p(o, true, false, true)?;
            Ok(ks
                .into_iter()
                .map(|k| r.key_to_val(k))
                .collect::<Vec<Val>>())
        });
        match r {
            Ok(v) => Ok(v.into_iter().map(|x| self.out_val(x)).collect()),
            Err(e) => Err(e),
        }
    }

    /// The own enumerable string keys as Rust strings.
    pub fn own_key_strings(&mut self, obj: Value) -> JsResult<Vec<String>> {
        let o = self.check_val(obj.0);
        self.entry(|r| {
            let o = r.require_obj(o, "keys of a non-object")?;
            let ks = r.own_keys_p(o, true, false, true)?;
            Ok(ks.into_iter().map(|k| r.key_display(k)).collect())
        })
    }

    /// `Object.getPrototypeOf` (null for none; primitives give their wrapper's prototype).
    pub fn get_prototype(&mut self, obj: Value) -> Value {
        match self.check_val(obj.0) {
            Val::Obj(o) => match self.o(o).proto {
                Some(p) => self.out_val(Val::Obj(p)),
                None => Value::NULL,
            },
            Val::Undef | Val::Null | Val::Empty => Value::NULL,
            v => {
                let p = self.proto_of_primitive(v);
                self.out_val(Val::Obj(p))
            }
        }
    }

    /// `Object.setPrototypeOf` (proto may be null); false if refused (cycle,
    /// non-extensible).
    pub fn set_prototype(&mut self, obj: Value, proto: Value) -> bool {
        let (Val::Obj(o), p) = (self.check_val(obj.0), self.check_val(proto.0)) else {
            return false;
        };
        let p = match p {
            Val::Obj(p) => Some(p),
            Val::Null => None,
            _ => return false,
        };
        self.set_proto(o, p)
    }

    /// `v instanceof ctor`.
    pub fn instance_of(&mut self, v: Value, ctor: Value) -> JsResult<bool> {
        let v = self.check_val(v.0);
        let c = self.check_val(ctor.0);
        self.entry(|r| r.instance_of_v(v, c))
    }

    /// `Object.freeze(obj)`.
    pub fn freeze(&mut self, obj: Value) -> JsResult<()> {
        let o = self.check_val(obj.0);
        self.entry(|r| {
            if let Val::Obj(o) = o {
                r.set_integrity(o, true)?;
            }
            Ok(())
        })
    }

    // ---- functions ----

    /// A native function object.
    pub fn new_function<F>(&mut self, name: &str, arity: u32, f: F) -> Value
    where
        F: Fn(&mut Realm, Value, &[Value]) -> JsResult<Value> + 'static,
    {
        let rc: Rc<NativeFn> = Rc::new(f);
        self.new_function_rc(name, arity, rc)
    }

    /// [`Realm::new_function`] from a shared closure.
    pub fn new_function_rc(&mut self, name: &str, arity: u32, f: Rc<NativeFn>) -> Value {
        let id = self.make_host_fn(name, arity, f, false);
        self.out_val(Val::Obj(id))
    }

    /// A class (constructor function) for host objects: `F.prototype` is `proto` (and
    /// `proto.constructor` is F), and F's own prototype is `parent` (the parent class, for
    /// `extends`-like chains: pass its constructor) or Function.prototype.
    ///
    /// With `ctor`, `new F(...)` calls it with `this` = a fresh ordinary object whose prototype
    /// is `new.target.prototype`; whatever object it returns is the result (return a
    /// [`Realm::new_host_object`] to make a host object), and a non-object return means
    /// `this`. Calling F without `new` throws a TypeError. Without `ctor`, `new F` throws
    /// "Illegal constructor" (like `new HTMLElement()`), but `instanceof F` still works.
    pub fn new_class(
        &mut self,
        name: &str,
        arity: u32,
        proto: Value,
        parent: Option<Value>,
        ctor: Option<Box<NativeFn>>,
    ) -> Value {
        let proto = self.check_val(proto.0).as_obj();
        let parent = parent.and_then(|p| self.check_val(p.0).as_obj());
        let id = self.make_host_class(name, arity, proto, parent, ctor.map(Rc::from));
        self.out_val(Val::Obj(id))
    }

    /// Inside a native: `new.target` if it was called with `new`, else None.
    pub fn new_target(&self) -> Option<Value> {
        self.native_frames
            .last()
            .and_then(|f| f.new_target.as_obj())
            .map(|o| Value(Val::Obj(o)))
    }

    /// Inside a native: the function object being called.
    pub fn callee(&self) -> Option<Value> {
        self.native_frames.last().map(|f| Value(Val::Obj(f.callee)))
    }

    // ---- host objects ----

    /// Registers a host class; its hooks see the objects made with
    /// [`Realm::new_host_object`].
    pub fn register_class<C: HostClass>(&mut self, class: C) -> ClassId {
        let hooks = class.hooks();
        self.classes.push(crate::realm::ClassEntry {
            class: Rc::new(class),
            hooks,
        });
        ClassId(self.classes.len() as u32 - 1)
    }

    /// A host object of `class` with `data` and the given prototype (null for none).
    pub fn new_host_object(&mut self, class: ClassId, data: u64, proto: Value) -> Value {
        let p = self.check_val(proto.0).as_obj();
        let o = self.alloc(
            p,
            crate::object::ObjKind::Host {
                class: class.0,
                data,
            },
        );
        self.out_val(Val::Obj(o))
    }

    /// The data of a host object of `class` (None for anything else: use this to check
    /// `this` in a native).
    pub fn host_data(&self, v: Value, class: ClassId) -> Option<u64> {
        match self.check_val(v.0) {
            Val::Obj(o) => match self.o(o).kind {
                crate::object::ObjKind::Host { class: c, data } if c == class.0 => Some(data),
                _ => None,
            },
            _ => None,
        }
    }

    /// The class and data of any host object.
    pub fn host_info(&self, v: Value) -> Option<(ClassId, u64)> {
        match self.check_val(v.0) {
            Val::Obj(o) => match self.o(o).kind {
                crate::object::ObjKind::Host { class, data } => Some((ClassId(class), data)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Replaces a host object's data; false if `v` isn't a host object.
    pub fn set_host_data(&mut self, v: Value, data: u64) -> bool {
        match self.check_val(v.0) {
            Val::Obj(o) => match &mut self.om(o).kind {
                crate::object::ObjKind::Host { data: d, .. } => {
                    *d = data;
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Puts any Rust value in the realm for natives to reach with [`Realm::host`] (one slot;
    /// setting it again replaces it). Returns the previous one.
    pub fn set_host(&mut self, host: Box<dyn Any>) -> Option<Box<dyn Any>> {
        self.host.replace(host)
    }

    /// The value from [`Realm::set_host`], if it is a `T`.
    pub fn host<T: 'static>(&mut self) -> Option<&mut T> {
        self.host.as_mut().and_then(|h| h.downcast_mut::<T>())
    }

    /// Takes the value from [`Realm::set_host`] out of the realm.
    pub fn take_host(&mut self) -> Option<Box<dyn Any>> {
        self.host.take()
    }

    // ---- errors ----

    /// An exception throwing `v`.
    pub fn throw(&mut self, v: Value) -> Exception {
        let v = self.check_val(v.0);
        self.temps.push(v);
        Exception(Exc::Throw(v))
    }

    /// An exception throwing a new error of `kind` with `message`.
    pub fn throw_error(&mut self, kind: ErrorKind, message: &str) -> Exception {
        let e = self.make_error(kind, message);
        self.temps.push(Val::Obj(e));
        Exception(Exc::Throw(Val::Obj(e)))
    }

    /// Shorthand for `throw_error(ErrorKind::TypeError, message)`.
    pub fn throw_type_error(&mut self, message: &str) -> Exception {
        self.throw_error(ErrorKind::TypeError, message)
    }

    /// A new error object (not thrown).
    pub fn new_error(&mut self, kind: ErrorKind, message: &str) -> Value {
        let e = self.make_error(kind, message);
        self.out_val(Val::Obj(e))
    }

    /// A readable report of an exception.
    pub fn report(&mut self, e: &Exception) -> ErrorReport {
        match e.0 {
            Exc::Throw(v) => {
                let v = self.check_val(v);
                self.error_report(v)
            }
            Exc::Term(t) => ErrorReport {
                name: String::from("Terminated"),
                message: String::from(match t {
                    Termination::StepBudget => "the script ran too long (step budget)",
                    Termination::Interrupted => "the script was stopped",
                    Termination::OutOfMemory => "the script ran out of memory",
                }),
                uncatchable: true,
                ..ErrorReport::default()
            },
        }
    }

    /// A report for any value, as if it were thrown (for unhandled rejections).
    pub fn report_value(&mut self, v: Value) -> ErrorReport {
        let v = self.check_val(v.0);
        self.error_report(v)
    }

    // ---- handles and the collector ----

    /// Keeps `v` alive (and valid) until [`Realm::unpin`].
    pub fn pin(&mut self, v: Value) -> Pinned {
        let v = self.check_val(v.0);
        Pinned(self.pin_val(v))
    }

    /// The value of a pin (a temporary, like every value the API returns).
    pub fn pinned(&mut self, p: &Pinned) -> Value {
        let v = self
            .pins
            .get(p.0 as usize)
            .copied()
            .flatten()
            .unwrap_or(Val::Undef);
        self.out_val(v)
    }

    pub fn unpin(&mut self, p: Pinned) {
        if let Some(slot) = self.pins.get_mut(p.0 as usize)
            && slot.take().is_some()
        {
            self.free_pins.push(p.0);
        }
    }

    /// A weak handle to `v`: it doesn't keep `v` alive.
    pub fn weak(&mut self, v: Value) -> WeakValue {
        WeakValue(self.check_val(v.0))
    }

    /// The value of a weak handle, if it is still alive (then it is a temporary again).
    pub fn upgrade(&mut self, w: &WeakValue) -> Option<Value> {
        match w.0 {
            Val::Obj(o) if !self.heap.obj_valid(o) => None,
            Val::Str(s) if !self.heap.str_valid(s) => None,
            v => Some(self.out_val(v)),
        }
    }

    /// Forgets every temporary of the current scope (at the top level: all of them). Values
    /// not reachable otherwise may then be collected the next time script runs.
    pub fn release_temps(&mut self) {
        let base = self.native_frames.last().map_or(0, |f| f.temps_base);
        self.temps.truncate(base);
    }

    /// The current temporaries' mark, to release only what comes after it.
    pub fn temp_mark(&self) -> TempMark {
        TempMark(self.temps.len())
    }

    pub fn release_temps_to(&mut self, mark: TempMark) {
        let base = self.native_frames.last().map_or(0, |f| f.temps_base);
        self.temps.truncate(mark.0.max(base));
    }

    /// Collects garbage now (only outside native calls; inside one it does nothing).
    pub fn gc(&mut self) {
        if self.native_frames.is_empty() && self.frames_empty() {
            self.collect_garbage();
        }
    }

    pub fn heap_stats(&self) -> HeapStats {
        HeapStats {
            used_bytes: self.heap.used(),
            live_bytes: self.heap.live_bytes,
            objects: self.heap.object_count(),
            strings: self.heap.string_count(),
            collections: self.heap.gc_count,
            limit_bytes: self.heap.max_bytes,
        }
    }

    // ---- limits, time, interrupts ----

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
        self.set_heap_limit(limits.max_heap_bytes);
    }

    /// Changes the heap budget ([`Limits::max_heap_bytes`]) of a running realm. A realm that
    /// holds more than the new budget collects garbage at its next safe point and runs out of
    /// memory if it still holds more; one that holds less can grow to it.
    pub fn set_heap_limit(&mut self, bytes: usize) {
        self.limits.max_heap_bytes = bytes;
        self.heap.max_bytes = bytes;
        if self.heap.used() > bytes {
            self.heap.gc_wanted = true;
        }
    }

    /// A callback asked for the heap budget every [`INTERRUPT_STEPS`] VM steps while script
    /// runs, with the heap in use (an estimate, in bytes): [`HeapAnswer::Limit`] changes the
    /// budget ([`Realm::set_heap_limit`]), [`HeapAnswer::Stop`] ends the script (uncatchable
    /// [`Termination::Interrupted`]), [`HeapAnswer::Keep`] leaves things as they are. The
    /// Browser uses it to give a page more room while the machine has it and to take it back
    /// when the machine runs short.
    pub fn set_memory_hook(&mut self, f: Option<Box<dyn FnMut(usize) -> HeapAnswer>>) {
        self.memory_hook = f;
    }

    /// A callback polled every [`INTERRUPT_STEPS`] VM steps: return false to stop the script
    /// (uncatchable [`Termination::Interrupted`]). The Browser checks a wall clock here.
    pub fn set_interrupt(&mut self, f: Option<Box<dyn FnMut() -> bool>>) {
        self.interrupt = f;
    }

    /// How much native (Rust) stack one entry from the host may use before script gets a
    /// RangeError / SyntaxError for nesting too deep (default 768 KB; give the thread at
    /// least this plus 256 KB).
    pub fn set_stack_limit(&mut self, bytes: usize) {
        self.stack.limit = bytes.max(64 << 10);
    }

    /// Mark a symbol as the key under which the host keeps its own state on script-visible
    /// objects: `structuredClone` refuses an object that has an own property with it (a
    /// `DataCloneError`, as for any other host object) instead of copying the empty shell.
    pub fn set_clone_marker(&mut self, sym: Value) {
        if let Val::Sym(s) = sym.0 {
            self.clone_marker = Some(s);
        }
    }

    /// Steps run in the current (or last) entry from the host.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// The clock `Date` reads: milliseconds since 1970-01-01 UTC. Default: always 0.
    pub fn set_clock(&mut self, f: Box<dyn Fn() -> f64>) {
        self.clock = Some(f);
    }

    /// Local time = UTC + `minutes`.
    pub fn set_tz_offset(&mut self, minutes: i32) {
        self.tz_offset_min = minutes;
    }

    // ---- misc ----

    /// `JSON.parse(text)`.
    pub fn json_parse(&mut self, text: &str) -> JsResult<Value> {
        let r = self.entry(|r| {
            let u = crate::string::utf16(text);
            r.json_parse_units(&u)
        });
        self.out(r)
    }

    /// `JSON.stringify(v)`; None where JSON.stringify returns undefined.
    pub fn json_stringify(&mut self, v: Value) -> JsResult<Option<String>> {
        let v = self.check_val(v.0);
        self.entry(|r| {
            let s = r.json_stringify_val(v, Val::Undef, Val::Undef)?;
            Ok(s.map(|s| crate::string::to_rust(r.heap.units(s))))
        })
    }

    // ---- bytes: ArrayBuffer and typed arrays ----

    /// The largest ArrayBuffer script or the host may make, in bytes (default 32 MiB). A
    /// bigger `new ArrayBuffer(n)` is a RangeError. All buffer bytes also count against the
    /// heap budget ([`Limits::max_heap_bytes`]), so many buffers run out of memory like any
    /// other data.
    pub fn set_max_buffer_bytes(&mut self, bytes: usize) {
        self.max_buffer = bytes;
    }

    pub fn max_buffer_bytes(&self) -> usize {
        self.max_buffer
    }

    /// A new ArrayBuffer holding a copy of `bytes`. A RangeError (as an [`Exception`]) when it
    /// is over [`Realm::max_buffer_bytes`] or can't fit the heap budget.
    pub fn new_array_buffer(&mut self, bytes: &[u8]) -> JsResult<Value> {
        let r = self.entry(|r| {
            r.check_buffer_size(bytes.len())?;
            Ok(Val::Obj(r.buffer_from_vec(bytes.to_vec())))
        });
        self.out(r)
    }

    /// Like [`Realm::new_array_buffer`] but takes the bytes without copying them.
    pub fn new_array_buffer_from_vec(&mut self, bytes: Vec<u8>) -> JsResult<Value> {
        let r = self.entry(|r| {
            r.check_buffer_size(bytes.len())?;
            Ok(Val::Obj(r.buffer_from_vec(bytes)))
        });
        self.out(r)
    }

    /// A new `Uint8Array` (on a new ArrayBuffer) holding a copy of `bytes`.
    pub fn new_uint8_array(&mut self, bytes: &[u8]) -> JsResult<Value> {
        self.new_uint8_array_from_vec(bytes.to_vec())
    }

    /// A new `Uint8Array` that takes `bytes` without copying them.
    pub fn new_uint8_array_from_vec(&mut self, bytes: Vec<u8>) -> JsResult<Value> {
        let r = self.entry(|r| {
            r.check_buffer_size(bytes.len())?;
            let n = bytes.len();
            let buf = r.buffer_from_vec(bytes);
            Ok(Val::Obj(r.typed_new(
                crate::typed::TaKind::Uint8,
                buf,
                0,
                n,
                None,
            )))
        });
        self.out(r)
    }

    /// Whether `v` is an ArrayBuffer.
    pub fn is_array_buffer(&self, v: Value) -> bool {
        matches!(self.check_val(v.0), Val::Obj(o) if matches!(self.o(o).kind, ObjKind::ArrayBuffer(_)))
    }

    /// Whether `v` is a typed array (any of the nine kinds).
    pub fn is_typed_array(&self, v: Value) -> bool {
        matches!(self.check_val(v.0), Val::Obj(o) if matches!(self.o(o).kind, ObjKind::Typed(_)))
    }

    /// Whether `v` is a typed array or a DataView.
    pub fn is_array_buffer_view(&self, v: Value) -> bool {
        matches!(self.check_val(v.0), Val::Obj(o) if matches!(self.o(o).kind, ObjKind::Typed(_) | ObjKind::DataView(_)))
    }

    /// Borrows the bytes of an ArrayBuffer (all of them) or of a typed array / DataView (the
    /// bytes it views). None for anything else, and for a detached buffer. The slice is
    /// valid until the next call that takes `&mut Realm`.
    pub fn buffer_bytes(&self, v: Value) -> Option<&[u8]> {
        let Val::Obj(o) = self.check_val(v.0) else {
            return None;
        };
        match &self.o(o).kind {
            ObjKind::ArrayBuffer(b) if !b.detached => Some(&b.bytes),
            ObjKind::Typed(t) if !self.buf_detached(t.buf) => Some(self.typed_bytes(o)),
            ObjKind::DataView(d) => {
                let b = self.buf_data(d.buf)?;
                if b.detached {
                    return None;
                }
                b.bytes.get(d.offset..d.offset + d.len)
            }
            _ => None,
        }
    }

    /// Like [`Realm::buffer_bytes`], writable (the script sees the changes).
    pub fn buffer_bytes_mut(&mut self, v: Value) -> Option<&mut [u8]> {
        let Val::Obj(o) = self.check_val(v.0) else {
            return None;
        };
        let (buf, off, len) = match &self.o(o).kind {
            ObjKind::ArrayBuffer(b) if !b.detached => (o, 0, b.bytes.len()),
            ObjKind::Typed(t) => (t.buf, t.offset, t.len * t.kind.size()),
            ObjKind::DataView(d) => (d.buf, d.offset, d.len),
            _ => return None,
        };
        match &mut self.heap.om(buf).kind {
            ObjKind::ArrayBuffer(b) if !b.detached => b.bytes.get_mut(off..off + len),
            _ => None,
        }
    }

    /// A copy of the bytes [`Realm::buffer_bytes`] would borrow.
    pub fn copy_bytes(&self, v: Value) -> Option<Vec<u8>> {
        self.buffer_bytes(v).map(|b| b.to_vec())
    }

    /// Moves the bytes out of an ArrayBuffer without copying (a typed array's or DataView's
    /// buffer, whole): the buffer is detached afterwards (length 0, and every view of it
    /// stops working), like `ArrayBuffer.prototype.transfer`. None if `v` is not a buffer
    /// or view, or is already detached.
    pub fn take_buffer_bytes(&mut self, v: Value) -> Option<Vec<u8>> {
        let Val::Obj(o) = self.check_val(v.0) else {
            return None;
        };
        let buf = match &self.o(o).kind {
            ObjKind::ArrayBuffer(b) if !b.detached => o,
            ObjKind::Typed(t) => t.buf,
            ObjKind::DataView(d) => d.buf,
            _ => return None,
        };
        if self.buf_detached(buf) {
            return None;
        }
        Some(self.detach_buffer(buf))
    }

    pub fn well_known_symbol(&self, w: WellKnown) -> Value {
        Value(Val::Sym(self.well_known(w)))
    }

    pub fn intrinsic(&self, i: Intrinsic) -> Value {
        Value(Val::Obj(self.intrinsic_obj(i)))
    }
    // ---- ES modules ----

    /// Compiles `source` as a module: parses it (strict, with `import`/`export`, top-level
    /// `await`, `import.meta`), analyses its scopes and compiles its code, but runs nothing
    /// and fetches nothing. A SyntaxError (an early error, a duplicate or undefined export,
    /// ...) is returned as an `Err` with file, line and column. Read what it wants with
    /// [`Realm::module_requests`].
    pub fn compile_module(&mut self, source: &str, opts: &ModuleOptions<'_>) -> JsResult<ModuleId> {
        self.entry(|r| r.module_compile(source, opts))
    }

    /// A module the host builds from values: its exports are `exports` (name and value, in
    /// any order; `"default"` for the default export). It has no dependencies and no code,
    /// and is evaluated already (a JSON module is `[("default", parsed)]`). The values stay
    /// alive as long as the module does.
    pub fn compile_synthetic_module(
        &mut self,
        file: &str,
        exports: &[(&str, Value)],
    ) -> JsResult<ModuleId> {
        let ex: Vec<(String, Val)> = exports
            .iter()
            .map(|(n, v)| (String::from(*n), self.check_val(v.0)))
            .collect();
        self.entry(|r| r.module_synthetic(file, ex))
    }

    /// The module requests of `m`, once each, in source order (`import` and `export ... from`
    /// together). The index of a request in this list is what
    /// [`Realm::set_module_dependency`] takes.
    pub fn module_requests(&self, m: ModuleId) -> Vec<ModuleRequest> {
        self.module_requests_of(m)
    }

    /// Says which module satisfies request number `index` of `m`. Every request has to be
    /// answered (for `m` and, in turn, for every module reached) before [`Realm::link_module`].
    /// Answering twice replaces the earlier answer while `m` is unlinked.
    pub fn set_module_dependency(
        &mut self,
        m: ModuleId,
        index: usize,
        dep: ModuleId,
    ) -> JsResult<()> {
        self.entry(|r| r.module_set_dependency(m, index, dep))
    }

    /// The module answering request `index` of `m`, if the host has set one.
    pub fn module_dependency(&self, m: ModuleId, index: usize) -> Option<ModuleId> {
        self.module_dependency_of(m, index)
    }

    /// Links `m` and everything it reaches: resolves every import against the exports it
    /// asks for and builds the module environments (functions are usable from here on,
    /// `let` and `class` bindings are still uninitialized). A missing request answer, a
    /// missing export or an ambiguous `export *` is a SyntaxError that names the importing
    /// file, line and column; nothing stays half-linked after an error. Linking a linked
    /// module does nothing.
    pub fn link_module(&mut self, m: ModuleId) -> JsResult<()> {
        self.entry(|r| r.module_link(m))
    }

    /// Evaluates `m` and its dependencies (each once, dependencies first, cycles and
    /// top-level `await` by the spec's algorithm) and returns a Promise that fulfills with
    /// undefined when everything has run, or rejects with the first error. Links first if
    /// needed. Call [`Realm::run_jobs`] to let asynchronous modules run; a module without
    /// `await` anywhere in its graph has finished when this returns. Evaluating a module
    /// again gives the same outcome without running it again. The Promise counts as handled:
    /// a failed module never causes an unhandled-rejection report, so the host reports it
    /// itself (the browser's `window.onerror`) from [`Realm::module_error`] once the jobs
    /// have run, or by calling `then` on the Promise.
    pub fn evaluate_module(&mut self, m: ModuleId) -> JsResult<Value> {
        let r = self.entry(|r| r.module_evaluate(m));
        self.out(r)
    }

    /// The module namespace object of `m` (`import * as ns`): sorted keys, read-only, live.
    /// The module has to be linked.
    pub fn module_namespace(&mut self, m: ModuleId) -> JsResult<Value> {
        let r = self.entry(|r| r.module_ns(m));
        self.out(r)
    }

    pub fn module_status(&self, m: ModuleId) -> Option<ModuleStatus> {
        self.module_status_of(m)
    }

    /// The error a finished module failed with (its evaluation threw, or one of its
    /// dependencies' did).
    pub fn module_error(&mut self, m: ModuleId) -> Option<Value> {
        let v = self.module_error_of(m)?;
        Some(self.out_val(v))
    }

    /// The file name `m` was compiled with.
    pub fn module_file(&self, m: ModuleId) -> Option<String> {
        self.module_file_of(m)
    }

    /// The names `m` exports, sorted (the keys of its namespace). The module has to be
    /// linked.
    pub fn module_export_names(&mut self, m: ModuleId) -> JsResult<Vec<String>> {
        self.entry(|r| r.module_export_names_of(m))
    }

    /// Installs the hook that fills in `import.meta` (once per module; without one the
    /// object is empty).
    pub fn set_import_meta_hook(&mut self, hook: Option<Box<ImportMetaHook>>) {
        self.import_meta_hook = hook;
    }

    /// Installs the hook that gets `import()` calls. Without one, `import()` returns a
    /// Promise rejected with a TypeError.
    pub fn set_dynamic_import_hook(&mut self, hook: Option<Box<DynamicImportHook>>) {
        self.dynamic_import_hook = hook;
    }

    /// Completes an `import()` the hook was told about: `Ok(module)` is the module that
    /// satisfies it (its whole graph set up as for [`Realm::link_module`]); the engine links
    /// and evaluates it and, when that has finished, fulfills the script's Promise with the
    /// namespace (or rejects it with the error). `Err(value)` rejects the Promise with
    /// `value` (a failed fetch, say). Unknown or already-finished ids are ignored. The work
    /// happens in a job: call [`Realm::run_jobs`] after (the answer is never acted on in
    /// the middle of running script, even if the hook answers at once).
    pub fn finish_dynamic_import(
        &mut self,
        id: DynamicImportId,
        result: Result<ModuleId, Value>,
    ) -> JsResult<()> {
        let result = result.map_err(|v| self.check_val(v.0));
        self.entry(|r| r.dynamic_import_finish(id, result))
    }

    /// How many `import()` calls are waiting for [`Realm::finish_dynamic_import`].
    pub fn pending_dynamic_imports(&self) -> usize {
        self.dynamic_import_count()
    }

    /// Forgets every pending `import()` (when the page goes away): their Promises stay
    /// pending.
    pub fn cancel_dynamic_imports(&mut self) {
        self.dynamic_import_cancel();
    }

    /// The most modules a realm may hold (default 10 000); past it
    /// [`Realm::compile_module`] throws a RangeError.
    pub fn set_max_modules(&mut self, n: usize) {
        self.modules.max = n;
    }

    pub fn module_count(&self) -> usize {
        self.modules.count()
    }

    /// Releases `m` (its code, environment and namespace become garbage). Modules that
    /// still depend on it stop being linkable; the host drops those too.
    pub fn drop_module(&mut self, m: ModuleId) {
        self.module_drop(m);
    }

    /// Releases `m` and every module reachable from it through the dependencies the host
    /// set. Use it for a graph that is not shared with other roots.
    pub fn drop_module_graph(&mut self, m: ModuleId) {
        self.module_drop_graph(m);
    }
}
