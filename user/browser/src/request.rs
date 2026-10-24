//! A page's script requests (fetch, XMLHttpRequest, sendBeacon), the App's side: what the
//! page's scripts asked for ([`webengine::Document::take_net`]) is checked against the tab's
//! limits and run on worker threads by the fetcher (net.rs: scriptnet, which applies the
//! network policy hop by hop), and the answers go back to the page as tasks of its event loop.
//! A request belongs to the page that made it: when the tab shows another page or closes, its
//! requests are cancelled and their answers dropped.
//!
//! Each request is logged on the serial line ("[browser] request GET url: 200 OK, 12 bytes,
//! 5 ms") and in the tab's Network list (the console panel); a failure goes to the page's
//! console with the policy's explanation (the page itself is only told "Failed to fetch").

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use browserkit::netpolicy::Admission;
use milkweb::url::Url;
use webengine::{FrameId, NetEvent, NetFailure, NetFailureKind, NetOut, NetRequest};

use crate::App;
use crate::net::{JobId, Outcome, ScriptSpec};
use crate::tab::{NET_LOG_LINES, NetLogEntry};
use crate::web::Doc;

/// A running script request, from the App's side.
pub struct NetJob {
    pub uid: u64,
    pub epoch: u64,
    pub frame: u32,
    pub id: u32,
    pub method: String,
    pub url: String,
    pub started_ms: u64,
    /// The progress last passed to the page.
    pub sent: (usize, Option<usize>),
}

/// Answers on their way to pages: (tab uid, page generation, request id, the answer).
pub type Inbox = VecDeque<(u64, u64, u32, u32, NetEvent)>;

/// Answers handed to pages per tick (the rest wait for the next).
const DELIVER_PER_TICK: usize = 64;

impl App {
    pub(crate) fn tab_of(&self, uid: u64, epoch: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid && t.epoch == epoch)
    }

    /// Take in what the scripts of frame `f` of tab `ti` asked of the network.
    pub fn net_requests(&mut self, ti: usize, f: FrameId) {
        let outs = self.tabs[ti].page.doc.take_net(f);
        for o in outs {
            match o {
                NetOut::Start(req) => self.net_start(ti, f, req),
                NetOut::Cancel(id) => self.net_cancel(ti, f, id),
                NetOut::Ack { id, consumed } => self.net_ack(ti, f, id, consumed),
            }
        }
    }

    /// The page has used a streamed body up to `consumed` bytes: the job may send more.
    pub(crate) fn net_ack(&mut self, ti: usize, f: FrameId, id: u32, consumed: u64) {
        let Some(job) = self.tabs[ti].net_run.get(&(f.0, id)).copied() else { return };
        if let Some(sh) = self.fetch.shared(job) {
            let mut s = sh.lock();
            s.acked = s.acked.max(consumed);
        }
    }

    fn net_say(&self, line: &str) {
        rt::println!("[browser] {line}");
    }

    /// An answer for the page of tab `ti`, delivered at the next tick.
    fn net_queue(&mut self, ti: usize, f: FrameId, id: u32, ev: NetEvent) {
        let t = &self.tabs[ti];
        self.net_inbox.push_back((t.uid, t.epoch, f.0, id, ev));
    }

    fn net_fail_now(&mut self, ti: usize, f: FrameId, req: &NetRequest, kind: NetFailureKind, message: String) {
        self.net_say(&format!("request {} {}: {}", req.method, req.url, message));
        let t = &mut self.tabs[ti];
        if t.net_log.len() >= NET_LOG_LINES {
            t.net_log.pop_front();
        }
        t.net_log.push_back(NetLogEntry {
            method: req.method.clone(),
            url: req.url.clone(),
            status: 0,
            state: if kind == NetFailureKind::Blocked { "blocked" } else { "failed" },
            ms: 0,
            bytes: 0,
        });
        self.net_queue(ti, f, req.id, NetEvent::Failed(NetFailure { kind, message }));
    }

    /// The address a frame's scripts speak for (None: none).
    pub(crate) fn net_page_url(&self, ti: usize, f: FrameId) -> Option<Url> {
        let t = &self.tabs[ti];
        if crate::workers::is_worker_frame(f) {
            return self.worker_meta.get(&crate::workers::worker_of(f)).and_then(|m| m.page_url.clone());
        }
        if f.is_top() {
            t.page.url.clone()
        } else {
            t.frames.meta.get(&f).and_then(|m| m.ancestor_url.clone())
        }
    }

    pub(crate) fn net_start(&mut self, ti: usize, f: FrameId, req: NetRequest) {
        if self.net_page_url(ti, f).is_none() {
            self.net_fail_now(
                ti,
                f,
                &req,
                NetFailureKind::Blocked,
                format!("{} {}: this page can't make requests", req.method, req.url),
            );
            return;
        }
        let Ok(u) = Url::parse(&req.url) else {
            self.net_fail_now(
                ti,
                f,
                &req,
                NetFailureKind::Network,
                format!("{} {}: not an address milkOS can fetch", req.method, req.url),
            );
            return;
        };
        let host = format!("{}:{}", u.host.to_ascii_lowercase(), u.port);
        let key = ((f.0 as u64) << 32) | req.id as u64;
        match self.tabs[ti].limiter.submit(key, &host) {
            Err(e) => self.net_fail_now(ti, f, &req, NetFailureKind::Blocked, format!("{e}")),
            Ok(Admission::Start) => self.net_launch(ti, f, req),
            Ok(Admission::Queued) => {
                self.tabs[ti].net_wait.insert((f.0, req.id), req);
            }
        }
    }

    /// The limiter said go: hand the request to the fetcher.
    fn net_launch(&mut self, ti: usize, f: FrameId, req: NetRequest) {
        let Some(url) = self.net_page_url(ti, f) else { return };
        let t = &mut self.tabs[ti];
        let mut page = browserkit::netpolicy::Page::new(url);
        if crate::workers::is_worker_frame(f) {
            let wid = crate::workers::worker_of(f);
            if let Some(m) = self.worker_meta.get(&wid) {
                if let Some(space) = m.space {
                    page.address_space = space;
                }
                page.referrer_policy = m.ref_policy;
                // A script from a data: address speaks for an origin of its own.
                if m.opaque {
                    page.origin = browserkit::netpolicy::Origin::Opaque(0x8000_0000_0000_0000 | wid as u64);
                }
            }
        } else if f.is_top() {
            if let Some(space) = t.page.space {
                page.address_space = space;
            }
            page.referrer_policy = t.page.ref_policy;
        } else if let Some(m) = t.frames.meta.get(&f) {
            if let Some(space) = m.space {
                page.address_space = space;
            }
            page.referrer_policy = m.ref_policy;
            // A sandbox without allow-same-origin speaks for an origin of its own.
            if m.sandbox.opaque_origin() {
                page.origin = browserkit::netpolicy::Origin::Opaque(m.doc_id);
            }
        }
        // What a response may be when it comes whole (an XHR's, a fetch the page cannot take in
        // pieces) follows the machine's memory; what streams has no limit here.
        let mut cfg = self.net_cfg.clone();
        cfg.limits.max_response_body = self.mem.caps().response;
        let spec = ScriptSpec {
            req: req.clone(),
            page,
            cfg,
            cache: self.preflight.clone(),
        };
        let job = self.fetch.start_script(spec);
        let t = &mut self.tabs[ti];
        t.net_run.insert((f.0, req.id), job);
        self.net_jobs.insert(
            job,
            NetJob {
                uid: t.uid,
                epoch: t.epoch,
                frame: f.0,
                id: req.id,
                method: req.method.clone(),
                url: req.url.clone(),
                started_ms: crate::now_ms(),
                sent: (0, None),
            },
        );
        if self.log {
            self.net_say(&format!("request {} {} started", req.method, req.url));
        }
    }

    pub(crate) fn net_cancel(&mut self, ti: usize, f: FrameId, id: u32) {
        let t = &mut self.tabs[ti];
        let promoted = {
            t.net_wait.remove(&(f.0, id));
            let had = t.net_run.remove(&(f.0, id));
            if let Some(job) = had {
                self.fetch.cancel(job);
                self.net_jobs.remove(&job);
            }
            self.tabs[ti].limiter.finish(((f.0 as u64) << 32) | id as u64)
        };
        self.net_promote(ti, promoted);
    }

    fn net_promote(&mut self, ti: usize, promoted: Vec<u64>) {
        for pid in promoted {
            let key = ((pid >> 32) as u32, pid as u32);
            if let Some(r) = self.tabs[ti].net_wait.remove(&key) {
                self.net_launch(ti, FrameId(key.0), r);
            }
        }
    }

    /// Tabs that show another page (or are gone) leave requests behind: cancel them.
    fn net_sweep(&mut self) {
        let stale: Vec<JobId> = self
            .net_jobs
            .iter()
            .filter(|(_, j)| self.tab_of(j.uid, j.epoch).is_none())
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.fetch.cancel(id);
            self.net_jobs.remove(&id);
        }
    }

    /// Every tick: the heads and progress of running requests, the finished ones, and the
    /// answers waiting for their pages. True if a page may have changed.
    pub fn net_tick(&mut self) -> bool {
        if self.net_jobs.is_empty() && self.net_inbox.is_empty() {
            return false;
        }
        self.net_sweep();
        // Heads and progress.
        let ids: Vec<JobId> = self.net_jobs.keys().copied().collect();
        for job in ids {
            let Some(sh) = self.fetch.shared(job) else { continue };
            let (head, got, total, chunks) = {
                let mut s = sh.lock();
                (s.head.take(), s.got, s.total, core::mem::take(&mut s.stream.chunks))
            };
            let Some(nj) = self.net_jobs.get_mut(&job) else { continue };
            let (uid, epoch, frame, id) = (nj.uid, nj.epoch, nj.frame, nj.id);
            let progress = (got, total);
            let send_progress = head.is_none() && progress != nj.sent && got > 0;
            if head.is_some() || send_progress {
                nj.sent = progress;
            }
            if let Some(h) = head {
                self.net_inbox.push_back((uid, epoch, frame, id, NetEvent::Head(h)));
            }
            // The body's pieces, in order, between the head and the end.
            for c in chunks {
                self.net_inbox.push_back((uid, epoch, frame, id, NetEvent::Chunk(c)));
            }
            if send_progress {
                self.net_inbox.push_back((
                    uid,
                    epoch,
                    frame,
                    id,
                    NetEvent::Progress {
                        loaded: got as u64,
                        total: total.map(|t| t as u64),
                    },
                ));
            }
        }
        self.net_deliver()
    }

    /// Hand waiting answers to their pages.
    fn net_deliver(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..DELIVER_PER_TICK {
            let Some((uid, epoch, frame, id, ev)) = self.net_inbox.pop_front() else { break };
            let Some(ti) = self.tab_of(uid, epoch) else { continue };
            let f = FrameId(frame);
            if crate::workers::is_worker_frame(f) {
                // A worker's answer goes to its thread.
                self.workers.deliver_net(crate::workers::worker_of(f), id, ev);
                continue;
            }
            if !self.tabs[ti].page.doc.scripting_in(f) {
                continue;
            }
            self.with_doc_in(ti, f, |d: &mut Doc, h| d.net_event(f, h, id, ev));
            if ti == self.cur {
                changed = true;
            }
        }
        changed
    }

    /// A script request's job ended: queue its last answers, log it, start the next waiting.
    pub fn net_finished(
        &mut self,
        job: JobId,
        out: Outcome,
        head: Option<webengine::NetHead>,
    ) {
        let Some(nj) = self.net_jobs.remove(&job) else { return };
        // The last pieces of a streamed body that came since the last look.
        let (rest, _) = self.fetch.take_leftover(job);
        let Some(ti) = self.tab_of(nj.uid, nj.epoch) else { return };
        let ms = crate::now_ms().saturating_sub(nj.started_ms);
        let t = &mut self.tabs[ti];
        t.net_run.remove(&(nj.frame, nj.id));
        let mut entry = NetLogEntry {
            method: nj.method.clone(),
            url: nj.url.clone(),
            status: 0,
            state: "failed",
            ms,
            bytes: 0,
        };
        let mut events: Vec<NetEvent> = Vec::new();
        let mut line = String::new();
        match out {
            Outcome::Net(r) => match *r {
                Ok(done) => {
                    // The head normally went out while the body came; if not, now.
                    if head.is_some() || nj.sent == (0, None) {
                        events.push(NetEvent::Head(head.unwrap_or_else(|| done.head.clone())));
                    }
                    entry.status = done.head.status;
                    entry.state = "ok";
                    let size = done.streamed.unwrap_or(done.body.len());
                    entry.bytes = size;
                    line = format!(
                        "request {} {}: {} {}, {} bytes{}, {} ms",
                        nj.method,
                        done.head.url_or(&nj.url),
                        done.head.status,
                        done.head.status_text,
                        size,
                        if done.streamed.is_some() { " (streamed)" } else { "" },
                        ms
                    );
                    if !rest.is_empty() {
                        events.push(NetEvent::Chunk(rest.clone()));
                    }
                    events.push(NetEvent::Done(done.body));
                }
                Err(f) => {
                    if f.cancelled {
                        entry.state = "cancelled";
                    } else {
                        entry.state = match f.failure.kind {
                            NetFailureKind::Blocked => "blocked",
                            NetFailureKind::Timeout => "timeout",
                            _ => "failed",
                        };
                        line = format!("request {} {}: {}", nj.method, nj.url, f.failure.message);
                        events.push(NetEvent::Failed(f.failure));
                    }
                }
            },
            // The job was cancelled before it ran, or something else ended it.
            Outcome::Failed(_) => entry.state = "cancelled",
            _ => {}
        }
        if t.net_log.len() >= NET_LOG_LINES {
            t.net_log.pop_front();
        }
        t.net_log.push_back(entry);
        for ev in events {
            self.net_inbox.push_back((nj.uid, nj.epoch, nj.frame, nj.id, ev));
        }
        if !line.is_empty() {
            self.net_say(&line);
        }
        let promoted = self.tabs[ti].limiter.finish(((nj.frame as u64) << 32) | nj.id as u64);
        self.net_promote(ti, promoted);
    }

    /// Script requests are outstanding in some tab (the main loop keeps polling).
    pub fn net_active(&self) -> bool {
        !self.net_jobs.is_empty() || !self.net_inbox.is_empty()
    }
}

/// The address a response came from, for the log.
trait HeadUrl {
    fn url_or<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl HeadUrl for webengine::NetHead {
    fn url_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.url.is_empty() { fallback } else { &self.url }
    }
}

/// Lines of the Network list for the panel: newest last.
pub fn net_lines(log: &VecDeque<NetLogEntry>) -> Vec<String> {
    log.iter()
        .map(|e| {
            let status = if e.status > 0 {
                format!("{}", e.status)
            } else {
                String::from(e.state)
            };
            let size = if e.bytes > 0 { format!(" {} B", e.bytes) } else { String::new() };
            let ms = if e.state.is_empty() { String::new() } else { format!(" {} ms", e.ms) };
            format!("{} {} {}{}{}", e.method, e.url, status, size, ms)
        })
        .collect()
}

#[allow(dead_code)]
type _Unused = BTreeMap<u32, u32>;
