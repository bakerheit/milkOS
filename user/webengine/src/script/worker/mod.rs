//! Dedicated Web Workers.
//!
//! A worker is a script running in a realm of its own on a thread of its own, talking to the
//! page that made it by messages. The engine does no I/O and starts no thread; it is the two
//! ends of a protocol, and the app (the Browser) carries it:
//!
//! ```text
//!   page realm (the main thread)                          worker realm (its own thread)
//!   ──────────────────────────────                        ─────────────────────────────
//!   new Worker(url)  ──Document::take_workers──▶ app ──▶ run_worker(boot, &mut dyn WorkerIo)
//!   worker.postMessage(m) ─▶ WorkerOut::Post ─────────── ToWorker::Message ─▶ onmessage
//!   worker.onmessage ◀── Document::worker_event ◀─────── FromWorker::Message ◀ self.postMessage
//!   worker.terminate() ─▶ WorkerOut::Terminate ─── WorkerIo::terminated / Recv::Terminated
//! ```
//!
//! * **[`page`]** is the page's half: the `Worker` class (`js/workers.js`), what it asks of the
//!   app ([`crate::WorkerOut`]) and how the app answers ([`crate::WorkerEvent`]); every answer is
//!   one task of the page's event loop, like a network answer.
//! * **[`scope`]** is the worker's global (`DedicatedWorkerGlobalScope`): a realm built from
//!   the same bindings as a page's, without the DOM: events, timers, `console`, `fetch` and
//!   `XMLHttpRequest` (through the app, with the worker's own origin), `importScripts`,
//!   `postMessage`, `close`, `location`, `navigator`, URL, Blob, crypto, Intl and the rest of
//!   the platform that is not about a document. It is a [`crate::Document`] with an empty page
//!   whose script runtime is this global ([`crate::Document::new_worker`]), so the same
//!   `run`, `net_event`, `take_net` and `take_console` drive it.
//! * **[`driver`]** is the thread's whole body ([`run_worker`]): it fetches the script (as
//!   [`crate::NetKind::Script`] requests the app runs, or takes it from the blob the page
//!   made), runs the event loop (messages, network answers, timers, microtasks), watches the
//!   budgets (heap, a wall-clock limit per task, the page's termination) and reports.
//! * **[`xfer`]** is the message format both ends use: the structured clone with a transfer
//!   list of message ports and `ArrayBuffer`s.
//!
//! Message ports that are given to a worker (or by it) cross as *links* of the two port tables
//! (`ports.rs`): the end that stayed behind keeps its peer and forwards what the peer posts,
//! the end that crossed is a port of the other table whose posts travel back.
//!
//! Limits are the app's to set (how many workers, how much memory in all) and the driver's to
//! keep (a task's time, the heap, the size of one message); the bounds are in [`crate::WorkerLimits`].

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

use jsengine::Pinned;

use crate::api::*;

pub mod driver;
pub mod page;
pub mod scope;
pub mod xfer;

pub use driver::run_worker;

/// What a worker's realm keeps for its thread to send up.
pub enum Post {
    Message(WireMsg),
    Error { message: alloc::string::String, filename: alloc::string::String, line: u32, column: u32 },
}

/// A worker's own state, in its bridge.
pub struct WorkerState {
    pub boot: WorkerBoot,
    /// What the thread has to send up (in order).
    pub posts: Vec<Post>,
    /// `self.close()` was called: nothing more runs after the current task.
    pub closing: bool,
    /// The main script could not be loaded (the worker then ends).
    pub main_failed: bool,
    /// Bytes of the messages in `posts`.
    pub post_bytes: usize,
}

impl WorkerState {
    pub fn new(boot: WorkerBoot) -> WorkerState {
        WorkerState { boot, posts: Vec::new(), closing: false, main_failed: false, post_bytes: 0 }
    }
}

/// The workers a realm made (a page's, or a worker's own would-be nested ones: none).
#[derive(Default)]
pub struct PageWorkers {
    pub next: u32,
    /// The `Worker` objects that are alive, pinned (a worker with listeners lives on after the
    /// page lets go of the object).
    pub objs: BTreeMap<u32, Pinned>,
    /// What the app has not taken yet.
    pub out: Vec<WorkerOut>,
    /// Events the engine itself has for a worker (its script was refused): delivered as tasks.
    pub local: VecDeque<(u32, WorkerEvent)>,
    /// Workers started and not yet over.
    pub live: u32,
}

/// Workers one realm may have at a time (the app has a limit for the whole tab too).
pub const MAX_PER_REALM: u32 = 16;
