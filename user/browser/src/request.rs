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
use webengine::{NetEvent, NetFailure, NetFailureKind, NetOut, NetRequest};

use crate::App;
use crate::net::{JobId, Outcome, ScriptSpec};
use crate::tab::{NET_LOG_LINES, NetLogEntry};
use crate::web::Doc;

/// A running script request, from the App's side.
pub struct NetJob {
    pub uid: u64,
    pub epoch: u64,
    pub id: u32,
    pub method: String,
    pub url: String,
    pub started_ms: u64,
    /// The progress last passed to the page.
    pub sent: (usize, Option<usize>),
}

/// Answers on their way to pages: (tab uid, page generation, request id, the answer).
pub type Inbox = VecDeque<(u64, u64, u32, NetEvent)>;

/// Answers handed to pages per tick (the rest wait for the next).
const DELIVER_PER_TICK: usize = 64;

impl App {
    fn tab_of(&self, uid: u64, epoch: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid && t.epoch == epoch)
    }

    /// Take in what tab `ti`'s scripts asked of the network.
    pub fn net_requests(&mut self, ti: usize) {
        let outs = self.tabs[ti].page.doc.take_net();
        for o in outs {
            match o {
                NetOut::Start(req) => self.net_start(ti, req),
                NetOut::Cancel(id) => self.net_cancel(ti, id),
            }
        }
    }

    fn net_say(&self, line: &str) {
        rt::println!("[browser] {line}");
    }

    /// An answer for the page of tab `ti`, delivered at the next tick.
    fn net_queue(&mut self, ti: usize, id: u32, ev: NetEvent) {
        let t = &self.tabs[ti];
        self.net_inbox.push_back((t.uid, t.epoch, id, ev));
    }

    fn net_fail_now(&mut self, ti: usize, req: &NetRequest, kind: NetFailureKind, message: String) {
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
        self.net_queue(ti, req.id, NetEvent::Failed(NetFailure { kind, message }));
    }

    fn net_start(&mut self, ti: usize, req: NetRequest) {
        if self.tabs[ti].page.url.is_none() {
            self.net_fail_now(
                ti,
                &req,
                NetFailureKind::Blocked,
                format!("{} {}: this page can't make requests", req.method, req.url),
            );
            return;
        }
        let Ok(u) = Url::parse(&req.url) else {
            self.net_fail_now(
                ti,
                &req,
                NetFailureKind::Network,
                format!("{} {}: not an address milkOS can fetch", req.method, req.url),
            );
            return;
        };
        let host = format!("{}:{}", u.host.to_ascii_lowercase(), u.port);
        match self.tabs[ti].limiter.submit(req.id as u64, &host) {
            Err(e) => self.net_fail_now(ti, &req, NetFailureKind::Blocked, format!("{e}")),
            Ok(Admission::Start) => self.net_launch(ti, req),
            Ok(Admission::Queued) => {
                self.tabs[ti].net_wait.insert(req.id, req);
            }
        }
    }

    /// The limiter said go: hand the request to the fetcher.
    fn net_launch(&mut self, ti: usize, req: NetRequest) {
        let t = &mut self.tabs[ti];
        let Some(url) = t.page.url.clone() else { return };
        let mut page = browserkit::netpolicy::Page::new(url);
        if let Some(space) = t.page.space {
            page.address_space = space;
        }
        let spec = ScriptSpec {
            req: req.clone(),
            page,
            cfg: self.net_cfg.clone(),
            cache: self.preflight.clone(),
        };
        let job = self.fetch.start_script(spec);
        let t = &mut self.tabs[ti];
        t.net_run.insert(req.id, job);
        self.net_jobs.insert(
            job,
            NetJob {
                uid: t.uid,
                epoch: t.epoch,
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

    fn net_cancel(&mut self, ti: usize, id: u32) {
        let t = &mut self.tabs[ti];
        let promoted = {
            t.net_wait.remove(&id);
            let had = t.net_run.remove(&id);
            if let Some(job) = had {
                self.fetch.cancel(job);
                self.net_jobs.remove(&job);
            }
            self.tabs[ti].limiter.finish(id as u64)
        };
        self.net_promote(ti, promoted);
    }

    fn net_promote(&mut self, ti: usize, promoted: Vec<u64>) {
        for pid in promoted {
            if let Some(r) = self.tabs[ti].net_wait.remove(&(pid as u32)) {
                self.net_launch(ti, r);
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
            let (head, got, total) = {
                let mut s = sh.lock();
                (s.head.take(), s.got, s.total)
            };
            let Some(nj) = self.net_jobs.get_mut(&job) else { continue };
            let (uid, epoch, id) = (nj.uid, nj.epoch, nj.id);
            let progress = (got, total);
            let send_progress = head.is_none() && progress != nj.sent && got > 0;
            if head.is_some() || send_progress {
                nj.sent = progress;
            }
            if let Some(h) = head {
                self.net_inbox.push_back((uid, epoch, id, NetEvent::Head(h)));
            }
            if send_progress {
                self.net_inbox.push_back((
                    uid,
                    epoch,
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
            let Some((uid, epoch, id, ev)) = self.net_inbox.pop_front() else { break };
            let Some(ti) = self.tab_of(uid, epoch) else { continue };
            if !self.tabs[ti].page.doc.scripting() {
                continue;
            }
            self.with_doc(ti, |d: &mut Doc, h| d.net_event(h, id, ev));
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
        let Some(ti) = self.tab_of(nj.uid, nj.epoch) else { return };
        let ms = crate::now_ms().saturating_sub(nj.started_ms);
        let t = &mut self.tabs[ti];
        t.net_run.remove(&nj.id);
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
                    entry.bytes = done.body.len();
                    line = format!(
                        "request {} {}: {} {}, {} bytes, {} ms",
                        nj.method,
                        done.head.url_or(&nj.url),
                        done.head.status,
                        done.head.status_text,
                        done.body.len(),
                        ms
                    );
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
            self.net_inbox.push_back((nj.uid, nj.epoch, nj.id, ev));
        }
        if !line.is_empty() {
            self.net_say(&line);
        }
        let promoted = self.tabs[ti].limiter.finish(nj.id as u64);
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
