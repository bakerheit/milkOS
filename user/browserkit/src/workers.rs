//! The Browser's rules for dedicated workers, without threads or I/O: how many there may be,
//! who owns which, what happens to a message that has nowhere to go, when a worker is over,
//! what a navigation or a closed tab does to its workers, and how what a worker says is
//! routed (to its page, to the network path, to the console).
//!
//! The engine (`webengine::run_worker`) runs the worker itself; the app starts it on a thread
//! and gives this [`Hub`] a [`Backend`] for the thread: a way to start one, to hand it a
//! message, to take what it has sent up, to ask it to stop and to ask whether it has. The
//! tests drive the hub with a backend that is a script, no thread at all.
//!
//! ```text
//!   page: WorkerOut::Spawn ──▶ Hub::spawn ──▶ Backend::spawn (a thread, run_worker)
//!   page: WorkerOut::Post  ──▶ Hub::post  ──▶ Backend::send
//!   every tick:  Hub::poll ──▶ Backend::recv ──▶ HubEvent::{ToPage, Net, Console, Ended}
//!   a page goes away:  Hub::terminate_tab / terminate_old ──▶ Backend::terminate, a zombie list
//!                       until Backend::finished (a thread is never waited for)
//! ```
//!
//! A worker that is terminated is forgotten at once: nothing it still sends reaches the page,
//! and its thread is only joined after it has stopped by itself (a script that does not
//! return is stopped from inside the engine; one that does not even do that stays on the
//! zombie list, counted against the thread cap, and is reported once).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use webengine::{
    CloseWhy, ConsoleLine, FromWorker, NetEvent, NetOut, ToWorker, WireMsg, WorkerBoot, WorkerEvent,
    WorkerLimits, WorkerSpawn, Xfer,
};

/// Who a worker belongs to: a tab (for good), the page it showed when the worker was made, and
/// the frame of that page whose script made it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Owner {
    pub tab: u64,
    pub epoch: u64,
    pub frame: u32,
}

/// The numbers the hub keeps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Workers one tab may have running at once.
    pub per_tab: usize,
    /// Threads in all (running workers and stopped ones not yet gone).
    pub threads: usize,
    /// What a worker is allowed (heap, task time, stack).
    pub worker: WorkerLimits,
    /// All the workers of a tab together may hold this much script heap (bytes); past it the
    /// greediest is stopped. The Browser leaves this to the memory plan (which gives every
    /// worker its own limit, from what the machine has free) and sets it very high.
    pub tab_heap: usize,
    /// How many messages and bytes a worker's inbox may hold (the backend enforces them
    /// where the messages wait; the hub only counts what it was refused).
    pub inbox_messages: usize,
    pub inbox_bytes: usize,
    /// How long a stopped worker's thread gets to end (ms) before it is reported.
    pub zombie_ms: u64,
    /// Items taken from one worker per [`Hub::poll`].
    pub per_poll: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            per_tab: 16,
            threads: 48,
            worker: WorkerLimits::default(),
            tab_heap: 160 << 20,
            inbox_messages: 10_000,
            inbox_bytes: 32 << 20,
            zombie_ms: 15_000,
            per_poll: 256,
        }
    }
}

/// The least heap a worker is started with, however many its tab has.
pub const MIN_WORKER_HEAP: usize = 8 << 20;

/// What the hub needs of a worker's thread.
pub trait Backend {
    type Handle;
    /// Start a thread for the worker; None if it cannot be made.
    fn spawn(&mut self, boot: WorkerBoot) -> Option<Self::Handle>;
    /// Hand the worker a message. False: its inbox is full (the message is dropped).
    fn send(&mut self, h: &mut Self::Handle, m: ToWorker) -> bool;
    /// What the worker has sent up since the last call, at most `max` items.
    fn recv(&mut self, h: &mut Self::Handle, max: usize) -> Vec<FromWorker>;
    /// Ask the worker to stop as soon as it can.
    fn terminate(&mut self, h: &mut Self::Handle);
    /// The worker's heap limit is now this (the engine's bytes); it applies it the next time
    /// its script runs. A backend without budgets that move ignores it.
    fn set_heap_limit(&mut self, h: &mut Self::Handle, bytes: usize) {
        let _ = (h, bytes);
    }
    /// What the worker may grow into while a task of its runs (the app cannot be asked until it
    /// ends): `headroom` bytes above what it holds, never past `cap`; with `burst` also as much
    /// again as it holds (a growing list copies itself into one twice the size at one step).
    fn set_heap_room(&mut self, h: &mut Self::Handle, headroom: usize, cap: usize, burst: bool) {
        let _ = (h, headroom, cap, burst);
    }
    /// The thread has ended.
    fn finished(&mut self, h: &mut Self::Handle) -> bool;
    /// Join (and drop) a thread that has ended.
    fn reap(&mut self, h: Self::Handle);
    /// A monotonic clock, ms.
    fn now_ms(&self) -> u64;
}

/// Why a worker could not be started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    TooManyInTab(usize),
    TooManyThreads(usize),
    NoThread,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Refusal::TooManyInTab(n) => write!(f, "too many workers are running in this tab (the limit is {n})"),
            Refusal::TooManyThreads(n) => write!(f, "too many workers are running (the limit is {n} in all)"),
            Refusal::NoThread => write!(f, "the system could not make a thread for it"),
        }
    }
}

/// What [`Hub::poll`] has for the app.
#[derive(Debug, PartialEq, Eq)]
pub enum HubEvent {
    /// Tell the page's `Worker` object (frame `owner.frame` of the tab).
    ToPage { owner: Owner, id: u32, ev: WorkerEvent },
    /// The worker wants a request run (or cancelled); answer with [`Hub::deliver_net`].
    Net { owner: Owner, id: u32, out: NetOut },
    /// A console line, from the worker named `name` (may be empty).
    Console { owner: Owner, id: u32, name: String, line: ConsoleLine },
    /// The worker is over, and why (after its `ToPage` closed event).
    Ended { owner: Owner, id: u32, why: CloseWhy },
}

struct Entry<H> {
    owner: Owner,
    name: String,
    url: String,
    handle: H,
    /// The heap limit it was started with, and the heap it last said it had.
    heap_limit: usize,
    heap: usize,
    /// A message was dropped because the inbox was full (said once).
    warned: bool,
    posted: u64,
}

pub struct Hub<B: Backend> {
    pub backend: B,
    pub limits: Limits,
    workers: BTreeMap<u32, Entry<B::Handle>>,
    /// Threads that were asked to stop and are not known to have: (handle, since, id, said).
    zombies: Vec<(B::Handle, u64, u32, bool)>,
    user_agent: String,
    tz: i32,
}

impl<B: Backend> Hub<B> {
    pub fn new(backend: B, limits: Limits, user_agent: &str, tz_offset: i32) -> Hub<B> {
        Hub { backend, limits, workers: BTreeMap::new(), zombies: Vec::new(), user_agent: String::from(user_agent), tz: tz_offset }
    }

    /// Workers running, in all.
    pub fn len(&self) -> usize {
        self.workers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// Threads: running workers and stopped ones whose threads have not ended.
    pub fn threads(&self) -> usize {
        self.workers.len() + self.zombies.len()
    }

    /// Workers of `tab` running.
    pub fn count_tab(&self, tab: u64) -> usize {
        self.workers.values().filter(|e| e.owner.tab == tab).count()
    }

    pub fn owner_of(&self, id: u32) -> Option<Owner> {
        self.workers.get(&id).map(|e| e.owner)
    }

    /// The ids of the workers of `tab`.
    pub fn ids_of_tab(&self, tab: u64) -> Vec<u32> {
        self.workers.iter().filter(|(_, e)| e.owner.tab == tab).map(|(i, _)| *i).collect()
    }

    /// Every running worker: (id, its tab, the heap it last said it holds).
    pub fn heaps(&self) -> Vec<(u32, u64, usize)> {
        self.workers.iter().map(|(&id, e)| (id, e.owner.tab, e.heap)).collect()
    }

    /// How much worker `id` may grow into during one task, and the most it may ever have.
    pub fn set_heap_room(&mut self, id: u32, headroom: usize, cap: usize, burst: bool) {
        if let Some(e) = self.workers.get_mut(&id) {
            self.backend.set_heap_room(&mut e.handle, headroom, cap, burst);
        }
    }

    /// Give worker `id` a new heap limit (the memory plan moves them).
    pub fn set_heap_limit(&mut self, id: u32, bytes: usize) {
        if let Some(e) = self.workers.get_mut(&id) {
            e.heap_limit = bytes;
            self.backend.set_heap_limit(&mut e.handle, bytes);
        }
    }

    /// Script heap the workers of `tab` hold, bytes (as they last said).
    pub fn heap_of_tab(&self, tab: u64) -> usize {
        self.workers.values().filter(|e| e.owner.tab == tab).map(|e| e.heap).sum()
    }

    /// Start a worker for `owner`. Err: it was not started, and why (the page gets an `error`
    /// event and the console a line).
    pub fn spawn(&mut self, owner: Owner, spawn: WorkerSpawn) -> Result<(), Refusal> {
        if self.count_tab(owner.tab) >= self.limits.per_tab {
            return Err(Refusal::TooManyInTab(self.limits.per_tab));
        }
        if self.threads() >= self.limits.threads {
            return Err(Refusal::TooManyThreads(self.limits.threads));
        }
        let (id, name, url) = (spawn.id, spawn.name.clone(), spawn.url.clone());
        // What the tab's workers may hold in all is shared out as they start: the first ones
        // get the full heap, later ones what is left (but never less than 8 MB), so sixteen
        // workers cannot hold sixteen full heaps.
        let promised: usize = self.workers.values().filter(|e| e.owner.tab == owner.tab).map(|e| e.heap_limit).sum();
        let left = (self.limits.tab_heap / 2).saturating_sub(promised);
        let heap_limit = self.limits.worker.heap.min(left.max(MIN_WORKER_HEAP)).max(1 << 20);
        let boot = WorkerBoot {
            spawn,
            limits: WorkerLimits { heap: heap_limit, ..self.limits.worker },
            user_agent: self.user_agent.clone(),
            tz_offset: self.tz,
        };
        let Some(handle) = self.backend.spawn(boot) else {
            return Err(Refusal::NoThread);
        };
        self.workers.insert(id, Entry { owner, name, url, handle, heap_limit, heap: 0, warned: false, posted: 0 });
        Ok(())
    }

    /// `worker.postMessage`. False: the worker is not running (the message is dropped) or its
    /// inbox is full (and the console says so once).
    pub fn post(&mut self, id: u32, msg: WireMsg) -> Option<HubEvent> {
        let e = self.workers.get_mut(&id)?;
        e.posted += 1;
        if self.backend.send(&mut e.handle, ToWorker::Message(msg)) {
            return None;
        }
        Self::full(e, id)
    }

    /// A message on a port the page gave the worker (or the port closing).
    pub fn post_port(&mut self, id: u32, port: u64, msg: Option<WireMsg>) -> Option<HubEvent> {
        let e = self.workers.get_mut(&id)?;
        if self.backend.send(&mut e.handle, ToWorker::Port { port, msg }) {
            return None;
        }
        Self::full(e, id)
    }

    fn full(e: &mut Entry<B::Handle>, id: u32) -> Option<HubEvent> {
        if e.warned {
            return None;
        }
        e.warned = true;
        Some(HubEvent::Console {
            owner: e.owner,
            id,
            name: e.name.clone(),
            line: ConsoleLine {
                level: webengine::ConsoleLevel::Warn,
                text: String::from("The worker is not keeping up: too many messages are waiting for it, so one was dropped."),
            },
        })
    }

    /// The answer to a request the worker started.
    pub fn deliver_net(&mut self, id: u32, req: u32, ev: NetEvent) {
        if let Some(e) = self.workers.get_mut(&id) {
            // (An answer is not a message: it is never refused for a full inbox, but a worker
            // that has stopped taking anything has its own limits in the backend.)
            let _ = self.backend.send(&mut e.handle, ToWorker::Net { id: req, ev });
        }
    }

    /// Stop worker `id` (the page's `terminate()`, or the page is gone). It is forgotten at
    /// once; what it still says is dropped.
    pub fn terminate(&mut self, id: u32) {
        if let Some(mut e) = self.workers.remove(&id) {
            self.backend.terminate(&mut e.handle);
            let now = self.backend.now_ms();
            self.zombies.push((e.handle, now, id, false));
        }
    }

    /// Every worker of `tab` (the tab closed).
    pub fn terminate_tab(&mut self, tab: u64) {
        for id in self.ids_of_tab(tab) {
            self.terminate(id);
        }
    }

    /// Every worker of `tab` made for a page older than `epoch` (the tab navigated).
    pub fn terminate_old(&mut self, tab: u64, epoch: u64) {
        let ids: Vec<u32> = self
            .workers
            .iter()
            .filter(|(_, e)| e.owner.tab == tab && e.owner.epoch < epoch)
            .map(|(i, _)| *i)
            .collect();
        for id in ids {
            self.terminate(id);
        }
    }

    /// Every worker made by frame `frame` of `tab`'s page (the frame went away).
    pub fn terminate_frame(&mut self, tab: u64, epoch: u64, frame: u32) {
        let ids: Vec<u32> = self
            .workers
            .iter()
            .filter(|(_, e)| e.owner == Owner { tab, epoch, frame })
            .map(|(i, _)| *i)
            .collect();
        for id in ids {
            self.terminate(id);
        }
    }

    /// The tab's workers together hold too much: stop the greediest. The ids stopped (the app
    /// tells the page and the console).
    pub fn enforce_tab_heap(&mut self, tab: u64, extra: usize) -> Option<u32> {
        let total = self.heap_of_tab(tab) + extra;
        if total <= self.limits.tab_heap {
            return None;
        }
        let worst = self
            .workers
            .iter()
            .filter(|(_, e)| e.owner.tab == tab)
            .max_by_key(|(_, e)| e.heap)
            .map(|(i, _)| *i)?;
        Some(worst)
    }

    /// Take in what the workers have sent since the last call; reap the threads that ended.
    pub fn poll(&mut self) -> Vec<HubEvent> {
        let mut out = Vec::new();
        let ids: Vec<u32> = self.workers.keys().copied().collect();
        let per = self.limits.per_poll;
        let mut ended: Vec<(u32, CloseWhy)> = Vec::new();
        for id in ids {
            let Some(e) = self.workers.get_mut(&id) else { continue };
            let items = self.backend.recv(&mut e.handle, per);
            let owner = e.owner;
            for f in items {
                match f {
                    FromWorker::Message(m) => out.push(HubEvent::ToPage { owner, id, ev: WorkerEvent::Message(m) }),
                    FromWorker::Net(o) => out.push(HubEvent::Net { owner, id, out: o }),
                    FromWorker::Port { port, msg } => out.push(HubEvent::ToPage { owner, id, ev: WorkerEvent::Port { port, msg } }),
                    FromWorker::Console(line) => out.push(HubEvent::Console { owner, id, name: e.name.clone(), line }),
                    FromWorker::Error { message, filename, line, column } => out.push(HubEvent::ToPage {
                        owner,
                        id,
                        ev: WorkerEvent::Error { message, filename, line, column },
                    }),
                    FromWorker::Heap(n) => e.heap = n,
                    FromWorker::Closed(why) => {
                        ended.push((id, why));
                        break;
                    }
                }
            }
        }
        for (id, why) in ended {
            let Some(mut e) = self.workers.remove(&id) else { continue };
            let owner = e.owner;
            // Messages it sent before closing are delivered first (they were taken above).
            out.push(HubEvent::ToPage { owner, id, ev: WorkerEvent::Closed });
            match &why {
                CloseWhy::Stopped(w) => out.push(HubEvent::Console {
                    owner,
                    id,
                    name: e.name.clone(),
                    line: ConsoleLine {
                        level: webengine::ConsoleLevel::Error,
                        text: format!("The worker {} was stopped: {w}.", describe(&e.name, &e.url)),
                    },
                }),
                CloseWhy::Failed => out.push(HubEvent::Console {
                    owner,
                    id,
                    name: e.name.clone(),
                    line: ConsoleLine {
                        level: webengine::ConsoleLevel::Error,
                        text: format!("The worker {} could not be started: its script did not load.", describe(&e.name, &e.url)),
                    },
                }),
                CloseWhy::Closed | CloseWhy::Terminated => {}
            }
            out.push(HubEvent::Ended { owner, id, why });
            // It said goodbye: its thread is about to end.
            let now = self.backend.now_ms();
            self.backend.terminate(&mut e.handle);
            self.zombies.push((e.handle, now, id, false));
        }
        // Threads that have ended are joined; ones that have not, for too long, are reported.
        let now = self.backend.now_ms();
        let mut i = 0;
        while i < self.zombies.len() {
            if self.backend.finished(&mut self.zombies[i].0) {
                let (h, ..) = self.zombies.swap_remove(i);
                self.backend.reap(h);
            } else {
                let (_, since, id, said) = &mut self.zombies[i];
                if !*said && now.saturating_sub(*since) > self.limits.zombie_ms {
                    *said = true;
                    out.push(HubEvent::Console {
                        owner: Owner { tab: 0, epoch: 0, frame: 0 },
                        id: *id,
                        name: String::new(),
                        line: ConsoleLine {
                            level: webengine::ConsoleLevel::Warn,
                            text: format!("A worker's thread did not stop {} s after it was told to.", self.limits.zombie_ms / 1000),
                        },
                    });
                }
                i += 1;
            }
        }
        out
    }

    /// Any worker or stopped thread is around (the app keeps polling).
    pub fn active(&self) -> bool {
        !self.workers.is_empty() || !self.zombies.is_empty()
    }

    /// Workers running (not stopped ones), for the app to decide how often to poll.
    pub fn running(&self) -> bool {
        !self.workers.is_empty()
    }
}

/// A `data:` script is decoded here (the network path has no such addresses): its bytes become
/// the worker's source, so the worker fetches nothing. Err: why it cannot be a worker (a
/// broken address, a type other than JavaScript).
pub fn prepare_source(sp: &mut WorkerSpawn) -> Result<(), String> {
    let Some(rest) = sp.url.strip_prefix("data:") else { return Ok(()) };
    let d = crate::netpolicy::origin::decode_data_url(rest, 8 << 20).map_err(|e| format!("{e}"))?;
    let essence = d.mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if !webengine::script::loader::is_javascript_mime(&essence) {
        return Err(format!("the data address has the type {essence}, not JavaScript"));
    }
    sp.source = Some(d.body);
    Ok(())
}

/// A worker as the console names it: `"name" (url)` or just the address.
pub fn describe(name: &str, url: &str) -> String {
    let short: String = url.chars().take(120).collect();
    if name.is_empty() {
        short
    } else {
        format!("\"{name}\" ({short})")
    }
}

/// The bytes a message weighs for an inbox limit (its text and the buffers it moves).
pub fn weight(m: &WireMsg) -> usize {
    m.data.len() + m.xfer.iter().map(|x| if let Xfer::Buffer(b) = x { b.len() } else { 8 }).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::vec;

    /// A thread that is a script: what it will say, what it was told.
    #[derive(Default)]
    struct Fake {
        said: VecDeque<FromWorker>,
        got: Vec<ToWorker>,
        terminated: bool,
        /// The thread ends only after this many `finished` calls once terminated.
        lingers: u32,
        ended: bool,
        cap: usize,
    }

    #[derive(Default)]
    struct Back {
        threads: BTreeMap<u32, Fake>,
        next: u32,
        now: u64,
        refuse_spawn: bool,
        reaped: Vec<u32>,
        boots: Vec<WorkerBoot>,
    }

    impl Backend for Back {
        type Handle = u32;
        fn spawn(&mut self, boot: WorkerBoot) -> Option<u32> {
            if self.refuse_spawn {
                return None;
            }
            self.next += 1;
            self.boots.push(boot);
            self.threads.insert(self.next, Fake { cap: 4, ..Fake::default() });
            Some(self.next)
        }
        fn send(&mut self, h: &mut u32, m: ToWorker) -> bool {
            let t = self.threads.get_mut(h).unwrap();
            if matches!(m, ToWorker::Message(_)) && t.got.iter().filter(|x| matches!(x, ToWorker::Message(_))).count() >= t.cap {
                return false;
            }
            t.got.push(m);
            true
        }
        fn recv(&mut self, h: &mut u32, max: usize) -> Vec<FromWorker> {
            let t = self.threads.get_mut(h).unwrap();
            let n = max.min(t.said.len());
            t.said.drain(..n).collect()
        }
        fn terminate(&mut self, h: &mut u32) {
            self.threads.get_mut(h).unwrap().terminated = true;
        }
        fn finished(&mut self, h: &mut u32) -> bool {
            let t = self.threads.get_mut(h).unwrap();
            if !t.terminated {
                return false;
            }
            if t.lingers > 0 {
                t.lingers -= 1;
                return false;
            }
            t.ended = true;
            true
        }
        fn reap(&mut self, h: u32) {
            self.reaped.push(h);
            self.threads.remove(&h);
        }
        fn now_ms(&self) -> u64 {
            self.now
        }
    }

    fn spawn_of(id: u32, name: &str) -> WorkerSpawn {
        WorkerSpawn {
            id,
            url: format!("http://www.milk.test/{name}.js"),
            kind: webengine::WorkerType::Classic,
            name: String::from(name),
            origin: String::from("http://www.milk.test"),
            source: None,
            secure: false,
        }
    }

    fn hub() -> Hub<Back> {
        Hub::new(Back::default(), Limits::default(), "UA", 60)
    }

    const A: Owner = Owner { tab: 1, epoch: 1, frame: 0 };

    fn msg(s: &str) -> WireMsg {
        WireMsg { data: String::from(s), xfer: vec![] }
    }

    #[test]
    fn a_worker_is_started_with_the_hubs_limits_and_the_apps_identity() {
        let mut h = hub();
        h.spawn(A, spawn_of(7, "w")).unwrap();
        let b = &h.backend.boots[0];
        assert_eq!((b.spawn.id, b.user_agent.as_str(), b.tz_offset), (7, "UA", 60));
        assert_eq!(b.limits, WorkerLimits::default());
        assert_eq!(h.len(), 1);
        // The tab's heap is shared out: the first workers get 32 MB, later ones less.
        for i in 0..15 {
            h.spawn(A, spawn_of(100 + i, "w")).unwrap();
        }
        let heaps: Vec<usize> = h.backend.boots.iter().map(|b| b.limits.heap >> 20).collect();
        assert_eq!(&heaps[..3], [32, 32, 16]);
        assert!(heaps.iter().all(|&m| m >= 8), "{heaps:?}");
        assert!(heaps.iter().sum::<usize>() <= 80 + 8 * 16, "{heaps:?}");
        // Another tab starts afresh.
        let b2 = Owner { tab: 2, epoch: 1, frame: 0 };
        h.spawn(b2, spawn_of(500, "w")).unwrap();
        assert_eq!(h.backend.boots.last().unwrap().limits.heap, 32 << 20);
        assert_eq!(h.owner_of(7), Some(A));
    }

    #[test]
    fn a_tab_may_have_sixteen_and_the_threads_are_capped_in_all() {
        let mut h = hub();
        for i in 0..16 {
            h.spawn(A, spawn_of(i + 1, "w")).unwrap();
        }
        assert_eq!(h.spawn(A, spawn_of(99, "w")), Err(Refusal::TooManyInTab(16)));
        // Another tab has its own sixteen, up to the threads in all.
        let b = Owner { tab: 2, epoch: 1, frame: 0 };
        let c = Owner { tab: 3, epoch: 1, frame: 0 };
        for i in 0..16 {
            h.spawn(b, spawn_of(100 + i, "w")).unwrap();
            h.spawn(c, spawn_of(200 + i, "w")).unwrap();
        }
        assert_eq!(h.threads(), 48);
        assert_eq!(h.spawn(Owner { tab: 4, epoch: 1, frame: 0 }, spawn_of(300, "w")), Err(Refusal::TooManyThreads(48)));
        // Stopped threads count until they have ended.
        h.backend.threads.get_mut(&1).unwrap().lingers = 3;
        h.terminate(1);
        assert_eq!(h.len(), 47);
        assert_eq!(h.threads(), 48);
        assert!(h.spawn(Owner { tab: 4, epoch: 1, frame: 0 }, spawn_of(301, "w")).is_err());
        for _ in 0..4 {
            h.poll();
        }
        assert_eq!(h.threads(), 47);
        assert!(h.spawn(Owner { tab: 4, epoch: 1, frame: 0 }, spawn_of(302, "w")).is_ok());
        // A backend that cannot make a thread says so.
        let mut h = hub();
        h.backend.refuse_spawn = true;
        assert_eq!(h.spawn(A, spawn_of(400, "w")), Err(Refusal::NoThread));
        assert_eq!(h.count_tab(1), 0);
    }

    #[test]
    fn what_a_worker_says_is_routed_by_kind() {
        let mut h = hub();
        h.spawn(A, spawn_of(7, "calc")).unwrap();
        let req = webengine::NetRequest {
            id: 1,
            kind: webengine::NetKind::Fetch,
            url: String::from("http://www.milk.test/x"),
            method: String::from("GET"),
            headers: vec![],
            body: vec![],
            mode: webengine::RequestMode::Cors,
            credentials: webengine::CredentialsMode::SameOrigin,
            redirect: webengine::RedirectMode::Follow,
            referrer: String::new(),
            referrer_policy: String::new(),
            cache: String::new(),
            timeout_ms: 0,
            force_preflight: false,
            stream: false,
        };
        let t = h.backend.threads.get_mut(&1).unwrap();
        t.said.push_back(FromWorker::Message(msg("\"hi\"")));
        t.said.push_back(FromWorker::Net(NetOut::Start(req.clone())));
        t.said.push_back(FromWorker::Console(ConsoleLine { level: webengine::ConsoleLevel::Log, text: String::from("logged") }));
        t.said.push_back(FromWorker::Heap(5 << 20));
        t.said.push_back(FromWorker::Error { message: String::from("Uncaught Error: x"), filename: String::from("f"), line: 3, column: 4 });
        t.said.push_back(FromWorker::Port { port: 9, msg: None });
        let ev = h.poll();
        assert_eq!(ev.len(), 5);
        assert_eq!(ev[0], HubEvent::ToPage { owner: A, id: 7, ev: WorkerEvent::Message(msg("\"hi\"")) });
        assert!(matches!(&ev[1], HubEvent::Net { id: 7, out: NetOut::Start(r), .. } if r.url.ends_with("/x")));
        assert!(matches!(&ev[2], HubEvent::Console { name, line, .. } if name == "calc" && line.text == "logged"));
        assert!(matches!(&ev[3], HubEvent::ToPage { ev: WorkerEvent::Error { line: 3, .. }, .. }));
        assert!(matches!(&ev[4], HubEvent::ToPage { ev: WorkerEvent::Port { port: 9, msg: None }, .. }));
        assert_eq!(h.heap_of_tab(1), 5 << 20);
        assert_eq!(h.heap_of_tab(2), 0);
        // And the other way.
        assert_eq!(h.post(7, msg("1")), None);
        assert_eq!(h.post_port(7, 3, Some(msg("p"))), None);
        h.deliver_net(7, 1, NetEvent::Done(vec![1]));
        let got = &h.backend.threads[&1].got;
        assert_eq!(got.len(), 3);
        assert!(matches!(&got[0], ToWorker::Message(m) if m.data == "1"));
        assert!(matches!(&got[1], ToWorker::Port { port: 3, .. }));
        assert!(matches!(&got[2], ToWorker::Net { id: 1, .. }));
        // A worker that does not exist hears nothing and says nothing.
        assert_eq!(h.post(99, msg("x")), None);
        h.deliver_net(99, 1, NetEvent::Done(vec![]));
    }

    #[test]
    fn a_full_inbox_drops_the_message_and_says_so_once() {
        let mut h = hub();
        h.spawn(A, spawn_of(7, "slow")).unwrap();
        for _ in 0..4 {
            assert_eq!(h.post(7, msg("m")), None);
        }
        let ev = h.post(7, msg("m")).expect("the first drop is reported");
        assert!(matches!(ev, HubEvent::Console { ref line, .. } if line.text.contains("not keeping up")));
        assert_eq!(h.post(7, msg("m")), None, "once");
        assert_eq!(h.backend.threads[&1].got.len(), 4);
    }

    #[test]
    fn closing_is_reported_in_order_and_the_thread_is_reaped_when_it_ends() {
        let mut h = hub();
        h.spawn(A, spawn_of(7, "w")).unwrap();
        h.spawn(A, spawn_of(8, "v")).unwrap();
        let t = h.backend.threads.get_mut(&1).unwrap();
        t.said.push_back(FromWorker::Message(msg("last words")));
        t.said.push_back(FromWorker::Closed(CloseWhy::Stopped(String::from("a task ran for more than 8 s"))));
        t.said.push_back(FromWorker::Message(msg("never heard")));
        t.lingers = 1;
        let ev = h.poll();
        let kinds: Vec<&str> = ev
            .iter()
            .map(|e| match e {
                HubEvent::ToPage { ev: WorkerEvent::Message(_), .. } => "message",
                HubEvent::ToPage { ev: WorkerEvent::Closed, .. } => "closed",
                HubEvent::Console { .. } => "console",
                HubEvent::Ended { .. } => "ended",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["message", "closed", "console", "ended"]);
        assert!(matches!(&ev[2], HubEvent::Console { line, .. } if line.level == webengine::ConsoleLevel::Error && line.text.contains("a task ran for more than 8 s")));
        assert_eq!(h.len(), 1);
        // The thread is told to stop (it is about to anyway) and is reaped once it has.
        assert!(h.backend.threads[&1].terminated);
        h.poll();
        assert_eq!(h.backend.reaped, [1]);
        assert_eq!(h.threads(), 1);
        // A failed start is a console line too.
        h.backend.threads.get_mut(&2).unwrap().said.push_back(FromWorker::Closed(CloseWhy::Failed));
        let ev = h.poll();
        assert!(ev.iter().any(|e| matches!(e, HubEvent::Console { line, .. } if line.text.contains("script did not load"))));
        assert_eq!(h.threads(), 0);
    }

    #[test]
    fn terminating_forgets_the_worker_at_once() {
        let mut h = hub();
        h.spawn(A, spawn_of(7, "w")).unwrap();
        h.backend.threads.get_mut(&1).unwrap().said.push_back(FromWorker::Message(msg("late")));
        h.terminate(7);
        assert!(h.backend.threads[&1].terminated);
        assert!(h.poll().is_empty(), "what it said after is dropped");
        assert_eq!(h.post(7, msg("x")), None);
        assert_eq!(h.len(), 0);
        assert_eq!(h.backend.reaped, [1]);
        // Terminating twice is nothing.
        h.terminate(7);
    }

    #[test]
    fn a_navigation_a_closed_tab_and_a_gone_frame_end_the_right_workers() {
        let mut h = hub();
        let p1 = Owner { tab: 1, epoch: 1, frame: 0 };
        let p1f = Owner { tab: 1, epoch: 1, frame: 4 };
        let p2 = Owner { tab: 1, epoch: 2, frame: 0 };
        let other = Owner { tab: 2, epoch: 1, frame: 0 };
        h.spawn(p1, spawn_of(1, "a")).unwrap();
        h.spawn(p1f, spawn_of(2, "b")).unwrap();
        h.spawn(p2, spawn_of(3, "c")).unwrap();
        h.spawn(other, spawn_of(4, "d")).unwrap();
        h.terminate_frame(1, 1, 4);
        assert_eq!(h.ids_of_tab(1), [1, 3]);
        h.terminate_old(1, 2);
        assert_eq!(h.ids_of_tab(1), [3]);
        h.terminate_tab(1);
        assert_eq!(h.ids_of_tab(1), Vec::<u32>::new());
        assert_eq!(h.ids_of_tab(2), [4]);
        h.poll();
        assert_eq!(h.backend.reaped.len(), 3);
    }

    #[test]
    fn a_thread_that_does_not_stop_is_reported_once_and_counted() {
        let mut h = hub();
        h.spawn(A, spawn_of(7, "stuck")).unwrap();
        h.backend.threads.get_mut(&1).unwrap().lingers = u32::MAX;
        h.terminate(7);
        h.backend.now = 5_000;
        assert!(h.poll().is_empty());
        h.backend.now = 16_000;
        let ev = h.poll();
        assert_eq!(ev.len(), 1);
        assert!(matches!(&ev[0], HubEvent::Console { line, .. } if line.text.contains("did not stop")));
        assert!(h.poll().is_empty(), "once");
        assert_eq!(h.threads(), 1);
        assert!(h.active());
        assert!(!h.running());
    }

    #[test]
    fn the_tabs_workers_share_a_heap_budget() {
        let mut h = hub();
        for i in 0..3 {
            h.spawn(A, spawn_of(i + 1, "w")).unwrap();
        }
        for (t, mb) in [(1u32, 40usize), (2, 90), (3, 20)] {
            h.backend.threads.get_mut(&t).unwrap().said.push_back(FromWorker::Heap(mb << 20));
        }
        h.poll();
        assert_eq!(h.heap_of_tab(1), 150 << 20);
        assert_eq!(h.enforce_tab_heap(1, 0), None);
        // The page and its frames count too: 20 MB more and the greediest worker goes.
        assert_eq!(h.enforce_tab_heap(1, 20 << 20), Some(2));
        assert_eq!(h.enforce_tab_heap(9, 1 << 30), None);
    }

    #[test]
    fn a_data_script_is_decoded_for_the_worker() {
        let mut sp = spawn_of(1, "d");
        sp.url = String::from("data:text/javascript,postMessage(1%2B1)");
        sp.origin = String::from("null");
        assert_eq!(prepare_source(&mut sp), Ok(()));
        assert_eq!(sp.source.as_deref(), Some(&b"postMessage(1+1)"[..]));
        let mut sp = spawn_of(1, "d");
        sp.url = String::from("data:application/javascript;base64,cG9zdE1lc3NhZ2UoMSk=#frag");
        assert_eq!(prepare_source(&mut sp), Ok(()));
        assert_eq!(sp.source.as_deref(), Some(&b"postMessage(1)"[..]));
        // Not JavaScript, no comma, bad base64: refused.
        for bad in ["data:,hello", "data:text/html,<p>", "data:text/javascript", "data:text/javascript;base64,!!!"] {
            let mut sp = spawn_of(1, "d");
            sp.url = String::from(bad);
            assert!(prepare_source(&mut sp).is_err(), "{bad}");
            assert!(sp.source.is_none());
        }
        // Other addresses are left alone.
        let mut sp = spawn_of(1, "w");
        assert_eq!(prepare_source(&mut sp), Ok(()));
        assert!(sp.source.is_none());
    }

    #[test]
    fn random_use_never_breaks_the_caps() {
        let mut x = 0x0123_4567_89ab_cdefu64;
        let mut rnd = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        let mut h = hub();
        let mut next = 1u32;
        let mut live: Vec<u32> = Vec::new();
        for step in 0..3000u64 {
            let owner = Owner { tab: 1 + rnd(4), epoch: 1 + rnd(3), frame: rnd(3) as u32 };
            match rnd(10) {
                0..=2 => {
                    let id = next;
                    next += 1;
                    if h.spawn(owner, spawn_of(id, "r")).is_ok() {
                        live.push(id);
                    }
                }
                3 | 4 => {
                    if !live.is_empty() {
                        let id = live[rnd(live.len() as u64) as usize];
                        let _ = h.post(id, msg("m"));
                    }
                }
                5 => {
                    if !live.is_empty() {
                        let id = live.swap_remove(rnd(live.len() as u64) as usize);
                        h.terminate(id);
                    }
                }
                6 => h.terminate_old(owner.tab, owner.epoch),
                7 => h.terminate_frame(owner.tab, owner.epoch, owner.frame),
                8 => {
                    // A worker says something, or ends.
                    if !live.is_empty() {
                        let id = live[rnd(live.len() as u64) as usize];
                        if let Some(t) = h.workers.get(&id).map(|e| e.handle)
                            && let Some(f) = h.backend.threads.get_mut(&t)
                        {
                            f.said.push_back(match rnd(4) {
                                0 => FromWorker::Heap((rnd(60) as usize) << 20),
                                1 => FromWorker::Closed(CloseWhy::Closed),
                                _ => FromWorker::Message(msg("up")),
                            });
                        }
                    }
                }
                _ => {
                    h.backend.now += 1000;
                    let _ = h.poll();
                    live.retain(|id| h.owner_of(*id).is_some());
                }
            }
            for tab in 1..=4 {
                assert!(h.count_tab(tab) <= h.limits.per_tab, "step {step}");
            }
            assert!(h.threads() <= h.limits.threads, "step {step}: {}", h.threads());
            // Every worker still running has a thread, and every thread is somebody's.
            assert_eq!(h.backend.threads.len(), h.threads(), "step {step}");
        }
        for tab in 1..=4 {
            h.terminate_tab(tab);
        }
        for _ in 0..3 {
            h.poll();
        }
        assert_eq!(h.threads(), 0);
        assert!(h.backend.threads.is_empty());
    }

    #[test]
    fn describing_a_worker_for_the_console() {
        assert_eq!(describe("", "http://a/b.js"), "http://a/b.js");
        assert_eq!(describe("calc", "http://a/b.js"), "\"calc\" (http://a/b.js)");
        assert_eq!(describe("", &"x".repeat(500)).len(), 120);
        assert_eq!(weight(&WireMsg { data: String::from("abc"), xfer: vec![Xfer::Buffer(vec![0; 10]), Xfer::Port(1)] }), 3 + 10 + 8);
    }
}
