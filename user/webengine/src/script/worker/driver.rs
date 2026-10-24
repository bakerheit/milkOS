//! The thread of one worker: [`run_worker`] is its whole body.
//!
//! ```text
//!   boot ─▶ Document::new_worker ─▶ loop {
//!               fetch what the loader wants (the main script)          ──▶ FromWorker::Net
//!               take in what arrived (messages, answers, port traffic) ◀── WorkerIo::recv
//!               Document::run: scripts, timers, port messages, microtasks
//!               send up: posts, requests, console lines, errors, ports ──▶ WorkerIo::send
//!               ended? (close(), stopped by a limit, terminated, failed)
//!               wait for the next timer or message                      ◀── WorkerIo::recv(timeout)
//!           } ─▶ FromWorker::Closed(why)
//! ```
//!
//! The thread blocks only in [`WorkerIo::recv`] (idle, or inside `importScripts`, which waits
//! for its script). A script that runs without returning is stopped from inside the engine:
//! the interrupt callback polls [`ScriptHost::slow`] every 100 ms of running, and the host
//! says stop when the page terminated the worker or the task has run for
//! [`WorkerLimits::task_ms`].

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;

/// Requests the driver makes itself (the main script) are numbered from here, apart from the
/// script's own (which the engine numbers from 1).
const DRIVER_IDS: u32 = 0x8000_0000;
/// Messages that may wait while an `importScripts` blocks.
const MAX_STASH: usize = 8192;
/// How long an `importScripts` waits for its script (ms).
const FETCH_MS: f64 = 60_000.0;
/// The heap is reported to the app at most this often (ms), when it changed by a lot.
const HEAP_EVERY_MS: f64 = 50.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Why {
    Terminated,
    TooLong,
}

struct WorkerHost<'a> {
    io: &'a mut dyn WorkerIo,
    ua: String,
    tz: i32,
    task_ms: f64,
    stash: VecDeque<ToWorker>,
    /// Why the interrupt said stop.
    why: Option<Why>,
    /// Time this task spent waiting for the network (not counted against its budget).
    blocked: f64,
}

impl ScriptHost for WorkerHost<'_> {
    fn measurer(&self) -> &dyn TextMeasurer {
        &ApproxMeasurer
    }
    fn viewport(&self) -> Viewport {
        Viewport::new(1, 1)
    }
    fn scroll(&self) -> (i32, i32) {
        (0, 0)
    }
    fn now(&self) -> f64 {
        self.io.now()
    }
    fn time(&self) -> f64 {
        self.io.time()
    }
    fn tz_offset(&self) -> i32 {
        self.tz
    }
    fn user_agent(&self) -> String {
        self.ua.clone()
    }
    fn random(&mut self, out: &mut [u8]) -> bool {
        self.io.random(out)
    }
    fn slow(&mut self, running_ms: f64) -> bool {
        if self.io.terminated() {
            self.why = Some(Why::Terminated);
            return false;
        }
        if running_ms - self.blocked > self.task_ms {
            self.why = Some(Why::TooLong);
            return false;
        }
        true
    }
    fn fetch_blocking(&mut self, req: NetRequest) -> Option<Result<(NetHead, Vec<u8>), String>> {
        let id = req.id;
        let url = req.url.clone();
        if !self.io.send(FromWorker::Net(NetOut::Start(req))) {
            return Some(Err(String::from("the page is not taking requests")));
        }
        let t0 = self.io.now();
        let mut head: Option<NetHead> = None;
        let out = loop {
            let waited = self.io.now() - t0;
            if waited > FETCH_MS {
                self.io.send(FromWorker::Net(NetOut::Cancel(id)));
                break Err(format!("{url} took longer than {} s", (FETCH_MS / 1000.0) as u32));
            }
            match self.io.recv(Some(200.0)) {
                Recv::Terminated => {
                    self.why = Some(Why::Terminated);
                    break Err(String::from("the worker was terminated"));
                }
                Recv::Timeout => continue,
                Recv::Msg(ToWorker::Net { id: i, ev }) if i == id => match ev {
                    NetEvent::Head(h) => head = Some(h),
                    NetEvent::Progress { .. } => {}
                    NetEvent::Done(body) => {
                        let h = head.take().unwrap_or(NetHead {
                            status: 200,
                            status_text: String::from("OK"),
                            headers: Vec::new(),
                            url: url.clone(),
                            redirected: false,
                            kind: ResponseKind::Basic,
                        });
                        break Ok((h, body));
                    }
                    NetEvent::Failed(f) => break Err(f.message),
                },
                Recv::Msg(other) => {
                    if self.stash.len() < MAX_STASH {
                        self.stash.push_back(other);
                    }
                }
            }
        };
        self.blocked += self.io.now() - t0;
        Some(out)
    }
}

/// A message or port traffic from the page, in the order it came.
fn apply(doc: &mut Document, host: &mut WorkerHost<'_>, m: ToWorker) {
    match m {
        ToWorker::Message(w) => {
            if !doc.worker_closing() {
                doc.worker_message(host, w);
            }
        }
        ToWorker::Port { port, msg } => doc.worker_port(port, msg),
        ToWorker::Net { .. } => {}
    }
}

/// The request for a classic worker's main script.
fn script_request(id: u32, url: &str) -> NetRequest {
    NetRequest {
        id,
        kind: NetKind::Script,
        url: String::from(url),
        method: String::from("GET"),
        headers: Vec::new(),
        body: Vec::new(),
        mode: RequestMode::SameOrigin,
        credentials: CredentialsMode::SameOrigin,
        redirect: RedirectMode::Follow,
        referrer: String::from("about:client"),
        referrer_policy: String::new(),
        cache: String::from("default"),
        timeout_ms: 0,
        force_preflight: false,
    }
}

/// Send up everything the worker has: requests, console lines, posts and errors, port traffic.
fn flush(doc: &mut Document, io: &mut dyn WorkerIo) {
    for o in doc.take_net() {
        io.send(FromWorker::Net(o));
    }
    for l in doc.take_console() {
        io.send(FromWorker::Console(l));
    }
    for f in doc.take_worker_out() {
        io.send(f);
    }
}

/// The whole life of a worker, on the calling thread. Returns when the worker is over, after
/// sending [`FromWorker::Closed`]. Never panics on what a script does.
pub fn run_worker(boot: WorkerBoot, io: &mut dyn WorkerIo) {
    let url = boot.spawn.url.clone();
    let mut doc = Document::new_worker(&boot);
    let mut host = WorkerHost {
        io,
        ua: boot.user_agent.clone(),
        tz: boot.tz_offset,
        task_ms: boot.limits.task_ms as f64,
        stash: VecDeque::new(),
        why: None,
        blocked: 0.0,
    };
    let mut requested: BTreeSet<String> = BTreeSet::new();
    let mut script_reqs: BTreeMap<u32, String> = BTreeMap::new();
    let mut heads: BTreeMap<u32, NetHead> = BTreeMap::new();
    let mut next_id = DRIVER_IDS;
    // Messages that came before the script ran wait for it (the worker's port is not open
    // until its script has been evaluated).
    let mut early: VecDeque<ToWorker> = VecDeque::new();
    let mut last_heap = 0usize;
    let mut heap_at = f64::NEG_INFINITY;
    let end: CloseWhy;
    'main: loop {
        // What the loader wants (the main script of a classic worker).
        for u in doc.pending_scripts() {
            if requested.insert(u.clone()) {
                let id = next_id;
                next_id += 1;
                script_reqs.insert(id, u.clone());
                host.io.send(FromWorker::Net(NetOut::Start(script_request(id, &u))));
            }
        }
        // Everything that has arrived.
        let mut took = 0usize;
        loop {
            let m = match host.stash.pop_front() {
                Some(m) => Recv::Msg(m),
                None => host.io.recv(Some(0.0)),
            };
            match m {
                Recv::Terminated => {
                    end = CloseWhy::Terminated;
                    break 'main;
                }
                Recv::Timeout => break,
                Recv::Msg(m) => {
                    took += 1;
                    host.blocked = 0.0;
                    match m {
                        m @ (ToWorker::Message(_) | ToWorker::Port { .. }) => {
                            if !doc.worker_started() || !early.is_empty() {
                                if early.len() < MAX_STASH {
                                    early.push_back(m);
                                }
                            } else {
                                apply(&mut doc, &mut host, m);
                            }
                        }
                        ToWorker::Net { id, ev } => match script_reqs.get(&id).cloned() {
                            Some(u) => match ev {
                                NetEvent::Head(h) => {
                                    heads.insert(id, h);
                                }
                                NetEvent::Progress { .. } => {}
                                NetEvent::Done(body) => {
                                    let ok = heads.remove(&id).is_none_or(|h| (200..300).contains(&h.status));
                                    if ok {
                                        doc.supply_script(&u, Some(&body));
                                    } else {
                                        host.io.send(FromWorker::Console(ConsoleLine {
                                            level: ConsoleLevel::Error,
                                            text: format!("Failed to load worker script {u}: the server did not answer with a success status"),
                                        }));
                                        doc.supply_script(&u, None);
                                    }
                                    script_reqs.remove(&id);
                                }
                                NetEvent::Failed(f) => {
                                    host.io.send(FromWorker::Console(ConsoleLine {
                                        level: ConsoleLevel::Error,
                                        text: f.message,
                                    }));
                                    doc.supply_script(&u, None);
                                    script_reqs.remove(&id);
                                }
                            },
                            None => {
                                doc.net_event(&mut host, id, ev);
                            }
                        },
                    }
                    flush(&mut doc, host.io);
                    if doc.worker_closing() || doc.script_stats().stopped || host.why.is_some() {
                        break;
                    }
                    // (A flood of messages is taken a slice at a time, with the timers between.)
                    if took >= 256 {
                        break;
                    }
                }
            }
        }
        host.blocked = 0.0;
        let ran = doc.run(&mut host);
        while doc.worker_started() && !doc.worker_closing() && host.why.is_none() {
            let Some(m) = early.pop_front() else { break };
            host.blocked = 0.0;
            apply(&mut doc, &mut host, m);
        }
        flush(&mut doc, host.io);
        // Is it over?
        let stats = doc.script_stats();
        if host.why == Some(Why::Terminated) || host.io.terminated() {
            end = CloseWhy::Terminated;
            break;
        }
        if stats.stopped {
            end = CloseWhy::Stopped(match host.why {
                Some(Why::TooLong) => format!(
                    "a task ran for more than {} s without returning",
                    boot.limits.task_ms / 1000
                ),
                _ => {
                    let r = doc.script_stop_reason();
                    if r == "out of memory" {
                        format!("it ran out of memory (the limit is {} MB)", boot.limits.heap >> 20)
                    } else {
                        r
                    }
                }
            });
            break;
        }
        if doc.worker_main_failed() {
            host.io.send(FromWorker::Error {
                message: String::new(),
                filename: url.clone(),
                line: 0,
                column: 0,
            });
            end = CloseWhy::Failed;
            break;
        }
        if doc.worker_closing() {
            end = CloseWhy::Closed;
            break;
        }
        // The heap, for the page's budget.
        let now = host.io.now();
        let heap = stats.heap_bytes;
        if now - heap_at >= HEAP_EVERY_MS && heap.abs_diff(last_heap) >= (512 << 10) {
            last_heap = heap;
            heap_at = now;
            host.io.send(FromWorker::Heap(heap));
        }
        // Wait for the next timer or message.
        if !host.stash.is_empty() || took > 0 {
            continue;
        }
        let mut timeout = doc.next_wake().map(|w| (w - host.io.now()).max(0.0));
        // Work that says it is due and did nothing is not allowed to spin the thread.
        if !ran && timeout == Some(0.0) {
            timeout = Some(1.0);
        }
        match host.io.recv(timeout.map(|t| t.min(60_000.0))) {
            Recv::Terminated => {
                end = CloseWhy::Terminated;
                break;
            }
            Recv::Timeout => {}
            Recv::Msg(m) => host.stash.push_back(m),
        }
    }
    flush(&mut doc, host.io);
    host.io.send(FromWorker::Closed(end));
}
