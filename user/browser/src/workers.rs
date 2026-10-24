//! Dedicated workers in the Browser: a thread per worker (`webengine::run_worker`), a mailbox
//! between it and the window's thread, and the App's side of what pages ask of them.
//!
//! * [`ThreadBackend`] is the [`browserkit::workers::Backend`] the rules (limits, routing,
//!   termination) run on: `try_spawn_sized` a thread with a [`Mailbox`], hand it messages, take
//!   what it sent up, flag it to stop, ask whether it ended. A worker waits in its mailbox on a
//!   futex (woken by a message, by termination, or by its next timer's time); the window's
//!   thread looks at the mailboxes every few milliseconds while workers run.
//! * The App part: [`App::worker_requests`] takes what a page's scripts asked of their workers
//!   after any script ran (start, post, terminate), [`App::workers_tick`] takes in what the
//!   workers said (messages for their `Worker` objects, requests for the network path, console
//!   lines), and ends the workers whose page went away, whose frame is gone or whose tab
//!   holds too much memory.
//!
//! A worker's network requests run through the same policy and limits as its page's
//! (request.rs), with the worker's own origin. They are keyed by a pseudo frame: [`wframe`].
//! The workers' console lines print as `[browser] console LEVEL [worker NAME]: ...` and go to
//! the tab's panel with a `[worker NAME]` tag.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use browserkit::netpolicy::{AddressSpace, ReferrerPolicy};
use browserkit::workers::{Backend, Hub, HubEvent, Limits, Owner, describe, prepare_source, weight};
use milkweb::url::Url;
use rt::sys;
use rt::thread::{self, JoinHandle, Mutex};
use webengine::{
    CloseWhy, ConsoleLevel, FrameId, FromWorker, NetEvent, NetOut, Recv, ToWorker, WireMsg, WorkerBoot, WorkerEvent,
    WorkerIo, WorkerOut,
};

use crate::App;

/// Frame ids from here up are workers (the worker's number is added): the key of a worker's
/// requests with the network path, which are keyed by frame.
pub const WORKER_FRAME: u32 = 0x4000_0000;

pub fn wframe(id: u32) -> FrameId {
    FrameId(WORKER_FRAME | (id & (WORKER_FRAME - 1)))
}

pub fn is_worker_frame(f: FrameId) -> bool {
    f.0 & WORKER_FRAME != 0
}

pub fn worker_of(f: FrameId) -> u32 {
    f.0 & (WORKER_FRAME - 1)
}

// ---- the mailbox ---------------------------------------------------------------------------------

/// Messages a worker's inbox holds (and the bytes of them). Answers to its requests are not
/// counted: the worker asked for each.
const INBOX_MESSAGES: usize = 10_000;
const INBOX_BYTES: usize = 32 << 20;
/// What a worker may have waiting to be taken by the window's thread.
const OUTBOX_ITEMS: usize = 50_000;
const OUTBOX_BYTES: usize = 64 << 20;

#[derive(Default)]
struct Queues {
    to: VecDeque<ToWorker>,
    to_msgs: usize,
    to_bytes: usize,
    from: VecDeque<FromWorker>,
    from_bytes: usize,
}

/// The two queues between a worker's thread and the window's, and the flags.
pub struct Mailbox {
    q: Mutex<Queues>,
    /// Bumped on every push into the inbox and on termination: the worker sleeps on it.
    seq: AtomicU32,
    term: AtomicBool,
    /// The worker's heap limit now (the engine's bytes; 0: none given), moved by the window's
    /// thread as the memory plan changes and read by the worker's own.
    limit: AtomicUsize,
    /// What it may grow by above what it holds while one task runs, and its ceiling (0: no
    /// headroom given, the limit stands as it is).
    headroom: AtomicUsize,
    cap: AtomicUsize,
    /// It may also double what it holds at one step (the machine has room to spare).
    burst: AtomicBool,
}

impl Mailbox {
    fn new() -> Mailbox {
        Mailbox {
            q: Mutex::new(Queues::default()),
            seq: AtomicU32::new(0),
            term: AtomicBool::new(false),
            limit: AtomicUsize::new(0),
            headroom: AtomicUsize::new(0),
            cap: AtomicUsize::new(usize::MAX),
            burst: AtomicBool::new(false),
        }
    }

    fn wake(&self) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        let _ = sys::futex_wake(self.seq.as_ptr() as usize, i32::MAX as usize);
    }
}

fn from_weight(f: &FromWorker) -> usize {
    match f {
        FromWorker::Message(m) => weight(m),
        FromWorker::Port { msg: Some(m), .. } => weight(m),
        FromWorker::Console(l) => l.text.len(),
        FromWorker::Error { message, filename, .. } => message.len() + filename.len(),
        FromWorker::Net(NetOut::Start(r)) => r.body.len() + r.url.len(),
        _ => 16,
    }
}

/// The worker thread's end.
struct ThreadIo {
    mb: Arc<Mailbox>,
}

impl WorkerIo for ThreadIo {
    fn now(&self) -> f64 {
        sys::clock_ns(sys::CLOCK_MONOTONIC) as f64 / 1e6
    }
    fn time(&self) -> f64 {
        sys::clock_ns(sys::CLOCK_REALTIME) as f64 / 1e6
    }
    fn random(&mut self, out: &mut [u8]) -> bool {
        let mut at = 0;
        while at < out.len() {
            match sys::getrandom(&mut out[at..]) {
                Ok(n) if n > 0 => at += n,
                _ => return false,
            }
        }
        true
    }
    fn recv(&mut self, timeout_ms: Option<f64>) -> Recv {
        let start = self.now();
        loop {
            if self.mb.term.load(Ordering::Acquire) {
                return Recv::Terminated;
            }
            let seen;
            {
                let mut q = self.mb.q.lock();
                if let Some(m) = q.to.pop_front() {
                    match &m {
                        ToWorker::Message(w) | ToWorker::Port { msg: Some(w), .. } => {
                            q.to_msgs = q.to_msgs.saturating_sub(1);
                            q.to_bytes = q.to_bytes.saturating_sub(weight(w));
                        }
                        _ => {}
                    }
                    return Recv::Msg(m);
                }
                seen = self.mb.seq.load(Ordering::Acquire);
            }
            let wait_ns = match timeout_ms {
                None => None,
                Some(t) => {
                    let left = t - (self.now() - start);
                    if left <= 0.0 {
                        return Recv::Timeout;
                    }
                    Some((left * 1e6) as u64 + 1)
                }
            };
            let _ = sys::futex_wait(self.mb.seq.as_ptr() as usize, seen, wait_ns);
        }
    }
    fn send(&mut self, m: FromWorker) -> bool {
        let w = from_weight(&m);
        let mut q = self.mb.q.lock();
        if q.from.len() >= OUTBOX_ITEMS || q.from_bytes + w > OUTBOX_BYTES {
            // (The end of a worker's life always gets through.)
            if !matches!(m, FromWorker::Closed(_)) {
                return false;
            }
        }
        q.from_bytes += w;
        q.from.push_back(m);
        true
    }
    fn terminated(&self) -> bool {
        self.mb.term.load(Ordering::Acquire)
    }
    fn heap_allowance(&mut self, used: usize) -> webengine::Allowance {
        let limit = self.mb.limit.load(Ordering::Acquire);
        if limit == 0 {
            return webengine::Allowance::Keep;
        }
        // The window's thread looks at this worker every 100 ms or so, from what the worker
        // last reported; a task that allocates fast in between must not run into a limit made
        // from old numbers: it may grow by its headroom from where it is now.
        let room = self.mb.headroom.load(Ordering::Acquire);
        let cap = self.mb.cap.load(Ordering::Acquire);
        let burst = if self.mb.burst.load(Ordering::Acquire) { used } else { 0 };
        let now = used.saturating_add(room.max(burst)).min(cap);
        webengine::Allowance::Limit(limit.max(now))
    }
}

// ---- the backend -----------------------------------------------------------------------------------

pub struct Thread {
    mb: Arc<Mailbox>,
    join: JoinHandle,
}

/// The thread backend. `next_room` is the headroom and ceiling the next worker starts with:
/// set before `spawn`, so a task that allocates at once has them from its first step.
pub struct ThreadBackend {
    pub next_room: (usize, usize, bool),
}

impl Backend for ThreadBackend {
    type Handle = Thread;

    fn spawn(&mut self, boot: WorkerBoot) -> Option<Thread> {
        let mb = Arc::new(Mailbox::new());
        mb.limit.store(boot.limits.heap, Ordering::Release);
        mb.headroom.store(self.next_room.0, Ordering::Release);
        mb.cap.store(self.next_room.1, Ordering::Release);
        mb.burst.store(self.next_room.2, Ordering::Release);
        let mb2 = mb.clone();
        let stack = boot.limits.stack.max(512 << 10);
        let mut boot = Some(boot);
        let join = thread::try_spawn_sized(stack, move || {
            if let Some(b) = boot.take() {
                let mut io = ThreadIo { mb: mb2.clone() };
                webengine::run_worker(b, &mut io);
            }
        })
        .ok()?;
        Some(Thread { mb, join })
    }

    fn send(&mut self, h: &mut Thread, m: ToWorker) -> bool {
        let counted = match &m {
            ToWorker::Message(w) | ToWorker::Port { msg: Some(w), .. } => Some(weight(w)),
            _ => None,
        };
        {
            let mut q = h.mb.q.lock();
            if let Some(w) = counted {
                if q.to_msgs >= INBOX_MESSAGES || q.to_bytes + w > INBOX_BYTES {
                    return false;
                }
                q.to_msgs += 1;
                q.to_bytes += w;
            }
            q.to.push_back(m);
        }
        h.mb.wake();
        true
    }

    fn recv(&mut self, h: &mut Thread, max: usize) -> Vec<FromWorker> {
        let mut q = h.mb.q.lock();
        let n = max.min(q.from.len());
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(f) = q.from.pop_front() {
                q.from_bytes = q.from_bytes.saturating_sub(from_weight(&f));
                out.push(f);
            }
        }
        out
    }

    fn terminate(&mut self, h: &mut Thread) {
        h.mb.term.store(true, Ordering::Release);
        h.mb.wake();
    }

    fn set_heap_limit(&mut self, h: &mut Thread, bytes: usize) {
        h.mb.limit.store(bytes, Ordering::Release);
    }

    fn set_heap_room(&mut self, h: &mut Thread, headroom: usize, cap: usize, burst: bool) {
        h.mb.burst.store(burst, Ordering::Release);
        h.mb.headroom.store(headroom, Ordering::Release);
        h.mb.cap.store(cap, Ordering::Release);
    }

    fn finished(&mut self, h: &mut Thread) -> bool {
        h.join.is_finished()
    }

    fn reap(&mut self, h: Thread) {
        h.join.join();
    }

    fn now_ms(&self) -> u64 {
        crate::now_ms()
    }
}

pub type WorkerHub = Hub<ThreadBackend>;

pub fn new_hub(user_agent: &str, tz_offset: i32) -> WorkerHub {
    Hub::new(
        ThreadBackend { next_room: (0, usize::MAX, false) },
        // (The memory plan, mem.rs, gives each worker its heap limit; the hub's fixed share of
        // a tab's heap is out of the way.)
        Limits { per_poll: 1024, tab_heap: usize::MAX / 4, ..Limits::default() },
        user_agent,
        tz_offset,
    )
}

// ---- the App's side -------------------------------------------------------------------------------

/// What the App remembers of a worker besides what the hub keeps.
pub struct WMeta {
    /// The script's address as text (data: addresses are long; the label uses the name).
    pub url: String,
    /// What names it on the console.
    pub label: String,
    /// The address its requests speak for: the script's own, or the creator's page (for a
    /// `blob:` or `data:` script).
    pub page_url: Option<Url>,
    /// An origin of its own (a `data:` script).
    pub opaque: bool,
    pub space: Option<AddressSpace>,
    pub ref_policy: ReferrerPolicy,
}

fn level_tag(l: ConsoleLevel) -> &'static str {
    match l {
        ConsoleLevel::Log => "log",
        ConsoleLevel::Info => "info",
        ConsoleLevel::Warn => "warn",
        ConsoleLevel::Error => "error",
        ConsoleLevel::Debug => "debug",
    }
}

impl App {
    /// A worker's line on the console: `[worker NAME]` on the panel, `[worker NAME]` on the serial
    /// line.
    pub fn worker_console(&mut self, ti: usize, id: u32, level: ConsoleLevel, text: &str) {
        let label = self.worker_meta.get(&id).map(|m| m.label.clone()).unwrap_or_default();
        crate::frames::print_console(&format!("{} [worker {}]", level_tag(level), label), text);
        let text = browserkit::clean_line(text);
        if ti >= self.tabs.len() {
            return;
        }
        let t = &mut self.tabs[ti];
        if t.console.len() >= crate::tab::CONSOLE_LINES {
            t.console.pop_front();
        }
        t.console.push_back((level, format!("[worker {label}] {text}")));
    }

    /// What the scripts of frame `f` of tab `ti` asked about their workers.
    pub fn worker_requests(&mut self, ti: usize, f: FrameId) {
        if ti >= self.tabs.len() {
            return;
        }
        let outs = self.tabs[ti].page.doc.take_workers(f);
        if outs.is_empty() {
            return;
        }
        // A new page in this tab: the old page's workers are over before the new page's start
        // (they would count against the tab's limit).
        let (uid, epoch) = (self.tabs[ti].uid, self.tabs[ti].epoch);
        for id in self.workers.ids_of_tab(uid) {
            if self.workers.owner_of(id).is_some_and(|o| o.epoch != epoch) {
                self.worker_end(id);
            }
        }
        let owner = {
            let t = &self.tabs[ti];
            Owner { tab: t.uid, epoch: t.epoch, frame: f.0 }
        };
        for o in outs {
            match o {
                WorkerOut::Spawn(mut sp) => {
                    let id = sp.id;
                    let name = sp.name.clone();
                    let creator = self.net_page_url(ti, f);
                    let (space, ref_policy) = self.worker_space(ti, f);
                    let url = sp.url.clone();
                    let opaque = sp.origin == "null";
                    // The address its requests speak for: its own for an http(s) script,
                    // else the page that made it.
                    let page_url = match Url::parse(&sp.url) {
                        Ok(u) if !opaque => Some(u),
                        _ => creator,
                    };
                    let short = url.rsplit('/').next().unwrap_or("").split('?').next().unwrap_or("");
                    let short = if url.starts_with("data:") { "data" } else if url.starts_with("blob:") { "blob" } else { short };
                    let label = if name.is_empty() { String::from(short) } else { name.clone() };
                    // What the machine has free decides how much heap it may have.
                    let fg = ti == self.cur;
                    self.workers.limits.worker.heap = self.mem.limit_for_new(browserkit::memory::Kind::Worker, fg);
                    self.workers.backend.next_room = (
                        self.mem.budget.headroom(browserkit::memory::Kind::Worker, fg),
                        self.mem.budget.ceiling_of(browserkit::memory::Kind::Worker),
                        self.mem.level() == browserkit::memory::Level::Normal,
                    );
                    let spawned = match prepare_source(&mut sp) {
                        Ok(()) => self.workers.spawn(owner, sp).map_err(|e| format!("{e}")),
                        Err(why) => Err(why),
                    };
                    match spawned {
                        Ok(()) => {
                            let head = self.mem.budget.headroom(browserkit::memory::Kind::Worker, fg);
                            let cap = self.mem.budget.ceiling_of(browserkit::memory::Kind::Worker);
                            self.workers.set_heap_room(id, head, cap, self.mem.level() == browserkit::memory::Level::Normal);
                            if self.log {
                                rt::println!("[browser] worker {} started: {}", id, describe(&name, &url));
                            }
                            self.worker_meta.insert(id, WMeta { url, label, page_url, opaque, space, ref_policy });
                        }
                        Err(why) => {
                            self.worker_meta.insert(id, WMeta { url: url.clone(), label, page_url, opaque, space, ref_policy });
                            self.worker_console(
                                ti,
                                id,
                                ConsoleLevel::Error,
                                &format!("Failed to construct 'Worker': {why}."),
                            );
                            self.worker_meta.remove(&id);
                            let ev = WorkerEvent::Error { message: String::new(), filename: url, line: 0, column: 0 };
                            self.worker_to_page(owner, id, ev);
                            self.worker_to_page(owner, id, WorkerEvent::Closed);
                        }
                    }
                }
                WorkerOut::Post { worker, msg } => {
                    if let Some(ev) = self.workers.post(worker, msg) {
                        self.worker_hub_event(ev);
                    }
                }
                WorkerOut::Port { worker, port, msg } => {
                    if let Some(ev) = self.workers.post_port(worker, port, msg) {
                        self.worker_hub_event(ev);
                    }
                }
                WorkerOut::Terminate(id) => self.worker_end(id),
            }
        }
    }

    fn worker_space(&self, ti: usize, f: FrameId) -> (Option<AddressSpace>, ReferrerPolicy) {
        let t = &self.tabs[ti];
        if f.is_top() {
            (t.page.space, t.page.ref_policy)
        } else if let Some(m) = t.frames.meta.get(&f) {
            (m.space, m.ref_policy)
        } else {
            (None, ReferrerPolicy::DEFAULT)
        }
    }

    /// Stop worker `id` (the page said so, or its page is gone): its thread is told, its
    /// requests are cancelled, it is forgotten.
    pub fn worker_end(&mut self, id: u32) {
        let owner = self.workers.owner_of(id);
        self.workers.terminate(id);
        self.worker_forget(id, owner);
    }

    /// Cancel the requests of a worker that is over and drop what the App knows of it.
    fn worker_forget(&mut self, id: u32, owner: Option<Owner>) {
        self.worker_meta.remove(&id);
        let wf = wframe(id);
        let tabs: Vec<usize> = match owner {
            Some(o) => self.tab_of(o.tab, o.epoch).into_iter().collect(),
            None => (0..self.tabs.len()).collect(),
        };
        for ti in tabs {
            let keys: Vec<u32> = self.tabs[ti]
                .net_run
                .keys()
                .chain(self.tabs[ti].net_wait.keys())
                .filter(|(f, _)| *f == wf.0)
                .map(|(_, r)| *r)
                .collect();
            for r in keys {
                self.net_cancel(ti, wf, r);
            }
        }
        self.net_inbox.retain(|e| e.2 != wf.0);
    }

    /// Deliver `ev` to worker `id`'s `Worker` object, in the frame that made it.
    fn worker_to_page(&mut self, owner: Owner, id: u32, ev: WorkerEvent) {
        let Some(ti) = self.tab_of(owner.tab, owner.epoch) else { return };
        let f = FrameId(owner.frame);
        if !self.tabs[ti].page.doc.scripting_in(f) {
            return;
        }
        self.with_doc_in(ti, f, |d, h| d.worker_event(f, h, id, ev));
        if ti == self.cur {
            self.dirty = true;
        }
    }

    pub fn worker_hub_event(&mut self, ev: HubEvent) {
        match ev {
            HubEvent::ToPage { owner, id, ev } => {
                let closed = matches!(ev, WorkerEvent::Closed);
                self.worker_to_page(owner, id, ev);
                if closed {
                    self.worker_forget(id, Some(owner));
                }
            }
            HubEvent::Net { owner, id, out } => {
                let Some(ti) = self.tab_of(owner.tab, owner.epoch) else {
                    self.worker_end(id);
                    return;
                };
                match out {
                    NetOut::Start(req) => self.net_start(ti, wframe(id), req),
                    NetOut::Cancel(r) => self.net_cancel(ti, wframe(id), r),
                    NetOut::Ack { id: req, consumed } => self.net_ack(ti, wframe(id), req, consumed),
                }
            }
            HubEvent::Console { owner, id, line, .. } => match self.tab_of(owner.tab, owner.epoch) {
                Some(ti) => self.worker_console(ti, id, line.level, &line.text),
                None => rt::println!("[browser] {}", line.text),
            },
            HubEvent::Ended { owner, id, why } => {
                if self.log {
                    let w = match &why {
                        CloseWhy::Closed => String::from("closed itself"),
                        CloseWhy::Terminated => String::from("terminated"),
                        CloseWhy::Failed => String::from("failed to start"),
                        CloseWhy::Stopped(s) => format!("stopped: {s}"),
                    };
                    rt::println!("[browser] worker {id} ended: {w}");
                }
                // A worker that ran out of memory is said so on the bar too (the console has
                // the whole story).
                if let CloseWhy::Stopped(s) = &why
                    && s.starts_with("it ran out of memory")
                    && let Some(ti) = self.tab_of(owner.tab, owner.epoch)
                    && ti == self.cur
                {
                    self.notice = Some(crate::Notice::info(
                        owner.tab,
                        owner.epoch,
                        "A worker of this page ran out of memory and was stopped. The page and your other tabs are not affected.",
                    ));
                    self.dirty = true;
                }
                self.worker_forget(id, Some(owner));
            }
        }
    }

    /// Every tick: what the workers said, and the workers whose page or frame is gone or
    /// whose tab uses too much memory. True if a page may have changed.
    pub fn workers_tick(&mut self) -> bool {
        if !self.workers.active() {
            return false;
        }
        let mut changed = false;
        for ev in self.workers.poll() {
            if matches!(ev, HubEvent::ToPage { .. }) {
                changed = true;
            }
            self.worker_hub_event(ev);
        }
        // Workers whose page went away (another page in the tab, the tab closed) or whose
        // frame did.
        let gone: Vec<u32> = self
            .worker_meta
            .keys()
            .copied()
            .filter(|id| match self.workers.owner_of(*id) {
                None => false,
                Some(o) => match self.tab_of(o.tab, o.epoch) {
                    None => true,
                    Some(ti) => {
                        let f = FrameId(o.frame);
                        !self.tabs[ti].page.doc.scripting_in(f)
                    }
                },
            })
            .collect();
        for id in gone {
            self.worker_end(id);
        }
        // Each worker's heap limit follows the memory plan (the machine's free memory, the
        // tabs open, what everything holds).
        for (id, tab, used) in self.workers.heaps() {
            let fg = self.tabs.get(self.cur).is_some_and(|t| t.uid == tab);
            if self.mem.budget.limit_of(crate::mem::worker_key(tab, id)).is_some() {
                let l = self.mem.budget.limit_with(browserkit::memory::Kind::Worker, fg, used);
                self.workers.set_heap_limit(id, l);
                let head = self.mem.budget.headroom(browserkit::memory::Kind::Worker, fg);
                let cap = self.mem.budget.ceiling_of(browserkit::memory::Kind::Worker);
                self.workers.set_heap_room(id, head, cap, self.mem.level() == browserkit::memory::Level::Normal);
            }
        }
        changed
    }

    /// Workers are running (the main loop looks at them often).
    pub fn workers_running(&self) -> bool {
        self.workers.active()
    }
}

#[allow(dead_code)]
fn _types(_: BTreeMap<u32, u32>, _: WireMsg, _: NetEvent) {}
