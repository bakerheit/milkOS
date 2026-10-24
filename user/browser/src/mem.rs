//! The Browser's memory: what the machine has (`/proc/meminfo`, `/proc/mempressure`), what each
//! page, frame and worker may use of it (`browserkit::memory`, the rules), and what is done when
//! memory runs short (below, `App::mem_tick`): caches dropped, garbage collected harder,
//! background tabs discarded, the heaviest page offered a Stop.
//!
//! [`MemMonitor`] reads the two files at most four times a second and keeps the plan: a heap
//! limit for every realm it knows of. Scripts ask it (through `JsHost::heap_allowance`) before
//! they run and every 65 thousand steps while they do, so a page that is allocating in a loop
//! sees its allowance shrink as the machine fills up and runs out of memory alone, long before
//! the machine does.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

pub use browserkit::memory::Kind;
use browserkit::memory::{
    self, Budget, Caps, Ladder, Level, Machine, Realm, Step, TabInfo, human, pick_discard,
    pick_heaviest, to_real,
};
use rt::sys;
use webengine::{Allowance, ConsoleLevel, FrameId, ScriptLimits};

use crate::tab::PageKind;
use crate::workers::wframe;
use crate::{App, now_ms};

/// Looked at no more often than this (ms).
const READ_EVERY_MS: u64 = 250;
/// The plan is not worked out again for a call within this long of the last (ms).
const PLAN_EVERY_MS: u64 = 40;

/// The key of a page's or frame's realm / a worker's, in the plan.
pub fn key_of(tab: u64, frame: FrameId) -> u64 {
    (tab << 32) | frame.0 as u64
}

pub fn worker_key(tab: u64, id: u32) -> u64 {
    key_of(tab, wframe(id))
}

/// A realm the monitor knows of.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    pub kind: Kind,
    pub tab: u64,
    pub used: usize,
    /// The limit the engine was last given.
    pub limit: usize,
}

pub struct MemMonitor {
    pub machine: Machine,
    pub budget: Budget,
    pub ladder: Ladder,
    realms: BTreeMap<u64, Known>,
    /// The tab in front (its uid).
    pub front: u64,
    read_at: u64,
    planned_at: u64,
    /// No machine numbers could be read (a host without /proc/meminfo): the old fixed limits.
    pub blind: bool,
    /// Say what happens on the serial line (`--log-input`).
    pub log: bool,
    pub log_realms: bool,
    logged: (u64, usize),
    /// Counters for the log and the tests.
    pub reads: u64,
    pub plans: u64,
}

fn read_text(path: &str) -> String {
    match sys::read_file(path) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => String::new(),
    }
}

/// The machine as `/proc` says (None: no /proc/meminfo).
pub fn read_machine() -> Option<Machine> {
    Machine::parse(&read_text("/proc/meminfo"), &read_text("/proc/mempressure"))
}

impl MemMonitor {
    pub fn new(log: bool) -> MemMonitor {
        let now = now_ms();
        let mut m = MemMonitor {
            machine: Machine::default(),
            budget: Budget::default(),
            ladder: Ladder::new(),
            realms: BTreeMap::new(),
            front: 0,
            read_at: 0,
            planned_at: 0,
            blind: false,
            log,
            log_realms: true,
            logged: (0, 0),
            reads: 0,
            plans: 0,
        };
        m.read(now);
        m
    }

    fn read(&mut self, now: u64) {
        self.read_at = now;
        self.reads += 1;
        match read_machine() {
            Some(machine) => {
                self.machine = machine;
                self.blind = false;
            }
            None => {
                // Without numbers, the memory the Browser had before there were budgets.
                self.machine = Machine::new(512 << 20, 400 << 20);
                self.blind = true;
            }
        }
        self.replan(now);
    }

    pub fn replan(&mut self, now: u64) {
        self.planned_at = now;
        self.plans += 1;
        let list: Vec<Realm> = self
            .realms
            .iter()
            .map(|(&key, k)| Realm {
                key,
                kind: k.kind,
                foreground: k.tab == self.front,
                used: k.used,
            })
            .collect();
        self.budget = memory::plan(&self.machine, &list);
        if self.log {
            let used: usize = self.realms.values().map(|k| k.used).sum();
            // (Said when the numbers moved by a lot, not every time they are looked at.)
            let avail_mb = self.machine.available >> 20;
            if self.logged.0.abs_diff(avail_mb as u64) >= 16
                || self.logged.1.abs_diff(used) >= 8 << 20
            {
                self.logged = (avail_mb as u64, used);
                rt::println!(
                    "[browser] memory: {} MB free of {} MB, room {} MB, {} realms hold {} MB (browser heap {} MB), level {:?}{}",
                    avail_mb,
                    self.machine.total >> 20,
                    self.budget.room >> 20,
                    self.realms.len(),
                    used >> 20,
                    rt::heap::stats().used >> 20,
                    self.budget.level,
                    if self.log_realms {
                        let mut t = String::new();
                        for (k, r) in &self.realms {
                            t.push_str(&alloc::format!(
                                " [{:x}:{:?} {}/{} MB]",
                                k,
                                r.kind,
                                r.used >> 20,
                                r.limit >> 20
                            ));
                        }
                        t
                    } else {
                        String::new()
                    }
                );
            }
        }
    }

    /// Look at the machine again if it is time (and work out the plan).
    pub fn refresh(&mut self, now: u64) -> bool {
        if now.saturating_sub(self.read_at) >= READ_EVERY_MS {
            self.read(now);
            true
        } else {
            false
        }
    }

    /// The tab in front changed.
    pub fn set_front(&mut self, tab: u64) {
        if self.front != tab {
            self.front = tab;
            self.planned_at = 0;
        }
    }

    /// Note a realm and what it holds.
    pub fn note(&mut self, key: u64, kind: Kind, tab: u64, used: usize, limit: usize) {
        self.realms.insert(
            key,
            Known {
                kind,
                tab,
                used,
                limit,
            },
        );
    }

    pub fn forget(&mut self, key: u64) {
        self.realms.remove(&key);
    }

    pub fn forget_tab(&mut self, tab: u64) {
        self.realms.retain(|_, k| k.tab != tab);
        self.planned_at = 0;
    }

    /// Everything the monitor knows of, with the tab each is in.
    pub fn realms(&self) -> impl Iterator<Item = (u64, &Known)> {
        self.realms.iter().map(|(k, v)| (*k, v))
    }

    pub fn known(&self, key: u64) -> Option<&Known> {
        self.realms.get(&key)
    }

    pub fn len(&self) -> usize {
        self.realms.len()
    }

    /// The realms as the plan wants them.
    pub fn list(&self) -> Vec<Realm> {
        self.realms
            .iter()
            .map(|(&key, k)| Realm {
                key,
                kind: k.kind,
                foreground: k.tab == self.front,
                used: k.used,
            })
            .collect()
    }

    /// What a new realm of this kind gets (the engine's bytes).
    pub fn limit_for_new(&mut self, kind: Kind, foreground: bool) -> usize {
        if self.blind {
            return match kind {
                Kind::Page => 64 << 20,
                Kind::Frame => 16 << 20,
                Kind::Worker => 32 << 20,
            };
        }
        self.refresh(now_ms());
        self.budget.for_new(kind, foreground)
    }

    /// The heap budget of realm `key` now, which holds `used`.
    pub fn allowance(
        &mut self,
        now: u64,
        key: u64,
        kind: Kind,
        tab: u64,
        fg: bool,
        used: usize,
    ) -> Allowance {
        if self.blind {
            return Allowance::Keep;
        }
        let old = self.realms.get(&key).map_or(0, |k| k.limit);
        match self.realms.get_mut(&key) {
            Some(k) => k.used = used,
            None => {
                self.realms.insert(
                    key,
                    Known {
                        kind,
                        tab,
                        used,
                        limit: old,
                    },
                );
                self.planned_at = 0;
            }
        }
        if !self.refresh(now) && now.saturating_sub(self.planned_at) >= PLAN_EVERY_MS {
            self.replan(now);
        }
        // (By the plan's room and weights, but with what the realm holds this very moment.)
        let counted = self.budget.limit_of(key).is_some();
        let limit = if counted {
            self.budget.limit_with(kind, fg, used)
        } else {
            self.budget
                .limit_with(kind, fg, used)
                .min(self.budget.for_new(kind, fg).max(used + used / 8))
        };
        if let Some(k) = self.realms.get_mut(&key) {
            k.limit = limit;
        }
        Allowance::Limit(limit)
    }

    /// How short memory is.
    pub fn level(&self) -> Level {
        if self.blind {
            Level::Normal
        } else {
            self.budget.level
        }
    }

    pub fn caps(&self) -> Caps {
        if self.blind {
            Caps::default()
        } else {
            self.budget.caps()
        }
    }

    /// Browser-side numbers for the log: real bytes the scripts of all realms hold, about.
    pub fn scripts_real(&self) -> usize {
        to_real(self.realms.values().map(|k| k.used).sum())
    }
}

impl App {
    /// The script limits for a new page or frame in tab `ti` (a worker's come from the hub).
    pub fn script_limits(&mut self, kind: Kind, foreground: bool) -> ScriptLimits {
        let limit = self.mem.limit_for_new(kind, foreground);
        let caps = self.mem.caps();
        ScriptLimits {
            memory: limit,
            // A script's text is bounded by the memory it could be given, not by 8 MB.
            max_source: caps.script.max(8 << 20),
            // What a page may read of a response whole (.text(), .json(), ...).
            buffer: caps.response,
            // (The syntax tree of the biggest piece alive at once, apart from the heap.)
            compile: (limit / 2).clamp(64 << 20, 512 << 20),
            ..ScriptLimits::default()
        }
    }

    /// The engine's limits for a page, from what the machine has free: how much text it takes in
    /// and how many nodes it builds.
    pub fn engine_limits(&self) -> webengine::Limits {
        self.engine_limits_more(0)
    }

    /// The same after "Load more" was asked `more` times: four times the bytes and the nodes
    /// each time, up to what the machine's room allows.
    pub fn engine_limits_more(&self, more: u32) -> webengine::Limits {
        let caps = self.mem.caps();
        let k = 1usize << (2 * more.min(10));
        webengine::Limits {
            max_input: caps
                .page
                .saturating_mul(k)
                .min(caps.page_max)
                .max(caps.page),
            max_nodes: caps
                .nodes
                .saturating_mul(k)
                .min(caps.nodes_max)
                .max(caps.nodes),
            ..webengine::Limits::default()
        }
    }

    /// Whether tab `ti` is the one in front.
    pub fn is_front(&self, ti: usize) -> bool {
        ti == self.cur
    }

    /// Note, for every tab, what each realm holds (so the plan has the whole picture), and
    /// forget the realms that are gone. Cheap; once a second is enough.
    pub fn mem_scan(&mut self) {
        let front = self.tabs.get(self.cur).map_or(0, |t| t.uid);
        self.mem.set_front(front);
        let mut seen: Vec<u64> = Vec::new();
        for ti in 0..self.tabs.len() {
            let t = &self.tabs[ti];
            if t.page.kind != PageKind::Web {
                continue;
            }
            for f in t.page.doc.all_ids() {
                let st = t.page.doc.script_stats_in(f);
                let key = key_of(t.uid, f);
                let kind = if f.is_top() { Kind::Page } else { Kind::Frame };
                if st.stopped || st.heap_limit == 0 {
                    // Stopped scripts hold nothing the plan should count.
                    self.mem.forget(key);
                    continue;
                }
                self.mem
                    .note(key, kind, t.uid, st.heap_bytes, st.heap_limit);
                seen.push(key);
            }
        }
        for (id, tab, used) in self.workers.heaps() {
            let key = worker_key(tab, id);
            let limit = self
                .mem
                .budget
                .limit_of(key)
                .unwrap_or_else(|| self.mem.known(key).map_or(0, |k| k.limit));
            self.mem.note(key, Kind::Worker, tab, used, limit);
            seen.push(key);
        }
        let gone: Vec<u64> = self
            .mem
            .realms()
            .map(|(k, _)| k)
            .filter(|k| !seen.contains(k))
            .collect();
        for k in gone {
            self.mem.forget(k);
        }
    }

    /// Say something about memory on the page's console and the serial line.
    pub fn mem_console(&mut self, ti: usize, level: ConsoleLevel, text: &str) {
        if ti >= self.tabs.len() {
            return;
        }
        self.console_line(ti, "", level, text);
    }

    /// The scripts of frame `f` of tab `ti` were stopped for lack of memory: say so, plainly,
    /// once. `used`/`limit`: what it held and was allowed (the engine's bytes).
    pub fn announce_memory_stop(&mut self, ti: usize, f: FrameId, used: usize, limit: usize) {
        let what = if f.is_top() {
            "This page"
        } else {
            "A frame of this page"
        };
        let msg = alloc::format!(
            "{what} ran out of memory: it needed more than the {} the Browser could give it (it held {}). Its scripts were stopped and their memory given back. The Browser, the desktop and your other tabs are not affected.",
            human(to_real(limit)),
            human(to_real(used)),
        );
        self.mem_console(ti, ConsoleLevel::Error, &msg);
        if ti == self.cur {
            let (uid, epoch) = (self.tabs[ti].uid, self.tabs[ti].epoch);
            self.notice = Some(crate::Notice::info(
                uid,
                epoch,
                &alloc::format!("{what} ran out of memory and its scripts were stopped."),
            ));
            self.dirty = true;
        }
    }

    /// The tab in front changed: the one left is timestamped (it may be discarded after a while),
    /// and a discarded one that was opened is fetched again.
    pub fn mem_front(&mut self) -> bool {
        let Some(front) = self.tabs.get(self.cur).map(|t| t.uid) else {
            return false;
        };
        let mut changed = false;
        if front != self.mem_front_uid {
            let now = now_ms();
            let left = self.mem_front_uid;
            if let Some(t) = self.tabs.iter_mut().find(|t| t.uid == left) {
                t.last_front = now;
            }
            self.mem_front_uid = front;
            self.mem.set_front(front);
            changed = true;
        }
        if self.tabs[self.cur].discarded.is_some() {
            let cur = self.cur;
            self.restore_tab(cur);
            changed = true;
        }
        changed
    }

    // ---- pressure ---------------------------------------------------------------------------

    /// Look at the machine's memory (at most every half second): plan again, and when it is
    /// short, climb the ladder one step: caches, garbage, background tabs, the heaviest page.
    /// True if something on screen may have changed.
    pub fn mem_tick(&mut self) -> bool {
        let now = now_ms();
        // (Workers run on threads of their own and grow while the window's thread does something
        // else: their limits are kept up to date more often.)
        let every = if self.workers.active() { 100 } else { 500 };
        if now.saturating_sub(self.mem_scan_at) < every {
            return false;
        }
        self.mem_scan_at = now;
        self.mem.refresh(now);
        self.mem_scan();
        self.mem.replan(now);
        let level = self.mem.level();
        let mut changed = false;
        // A step that frees nothing is followed at once by the next.
        for _ in 0..4 {
            let Some(step) = self.mem.ladder.next(level, now) else {
                break;
            };
            let did = self.mem_step(step, level);
            if self.mem.log && (did || step != Step::AskStop) {
                rt::println!(
                    "[browser] memory is {:?}: {:?} {}",
                    level,
                    step,
                    if did {
                        "did something"
                    } else {
                        "had nothing to do"
                    }
                );
            }
            changed |= did;
            if did {
                break;
            }
            self.mem.ladder.skip(step);
        }
        // Free memory gone from under the floor and nothing left to give back: the notice
        // bar goes when the page does.
        if let Some(n) = &self.notice
            && (self.tabs.iter().all(|t| t.uid != n.tab)
                || self
                    .tabs
                    .iter()
                    .any(|t| t.uid == n.tab && n.epoch != t.epoch))
        {
            self.notice = None;
            changed = true;
        }
        changed
    }

    fn mem_step(&mut self, step: Step, level: Level) -> bool {
        match step {
            Step::DropCaches => self.drop_caches() > 0,
            Step::Collect => self.collect_all() > 0,
            Step::DiscardTab => self.discard_one(level == Level::Critical),
            Step::AskStop => self.ask_stop(),
        }
    }

    /// Drop what can be fetched or made again: pictures no tab shows, stylesheet and script
    /// text the pages already took, parsed-ahead things. Returns about how many bytes.
    pub fn drop_caches(&mut self) -> usize {
        let mut freed = 0usize;
        let mut shown: alloc::collections::BTreeSet<&str> = alloc::collections::BTreeSet::new();
        for t in &self.tabs {
            for u in &t.page.shown {
                shown.insert(u.as_str());
            }
            for m in t.frames.meta.values() {
                for u in &m.shown {
                    shown.insert(u.as_str());
                }
            }
        }
        let drop_images: Vec<alloc::string::String> = self
            .images
            .iter()
            .filter(|(u, st)| {
                !shown.contains(u.as_str()) && !matches!(st, crate::ImgState::Loading(_))
            })
            .map(|(u, _)| u.clone())
            .collect();
        for u in drop_images {
            if let Some(st) = self.images.remove(&u) {
                let px = st.pixels();
                self.image_pixels = self.image_pixels.saturating_sub(px);
                freed += px * 4;
            }
        }
        for cache in [&mut self.sheets, &mut self.scripts] {
            let keys: Vec<alloc::string::String> = cache
                .iter()
                .filter(|(_, st)| matches!(st, crate::ImgState::Sheet(_)))
                .map(|(u, _)| u.clone())
                .collect();
            for u in keys {
                if let Some(crate::ImgState::Sheet(b)) = cache.remove(&u) {
                    freed += b.len();
                }
            }
        }
        self.script_bytes = 0;
        // (What the Browser freed is given back to the system at once: the allocator hands
        // whole free pages back as it goes.)
        freed
    }

    /// Collect garbage in every page and frame. Returns the bytes freed by the engine's count.
    pub fn collect_all(&mut self) -> usize {
        let mut freed = 0usize;
        for t in &mut self.tabs {
            freed += t.page.doc.collect_garbage();
        }
        freed
    }

    /// Discard the background tab that holds the most. The tab keeps its address, its scroll and
    /// its place in the history; the page is gone, and comes back from the network when the
    /// tab is opened. True if one was discarded.
    pub fn discard_one(&mut self, desperate: bool) -> bool {
        let now = now_ms();
        let infos: Vec<TabInfo> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let scripts: usize = t
                    .page
                    .doc
                    .all_ids()
                    .into_iter()
                    .map(|f| to_real(t.page.doc.script_stats_in(f).heap_bytes))
                    .sum();
                let workers: usize = self
                    .workers
                    .heaps()
                    .into_iter()
                    .filter(|(_, tab, _)| *tab == t.uid)
                    .map(|(_, _, h)| to_real(h))
                    .sum();
                TabInfo {
                    index: i,
                    foreground: i == self.cur,
                    busy: t.load.is_some() || t.net_busy() || t.frames.busy(),
                    touched: t.touched,
                    bytes: scripts + workers + t.page.doc.approx_bytes(),
                    idle_ms: if t.last_front == 0 {
                        u64::MAX
                    } else {
                        now.saturating_sub(t.last_front)
                    },
                    discarded: t.discarded.is_some() || t.page.kind != PageKind::Web,
                }
            })
            .collect();
        let Some(i) = pick_discard(&infos, desperate) else {
            return false;
        };
        self.discard_tab(i);
        true
    }

    pub fn discard_tab(&mut self, i: usize) {
        if i >= self.tabs.len() || i == self.cur {
            return;
        }
        let blank = self.internal_page(browserkit::address::Internal::NewTab);
        let t = &mut self.tabs[i];
        let d = crate::tab::Discarded {
            address: t
                .history
                .current()
                .map_or_else(|| t.page.address.clone(), |e| e.url.clone()),
            scroll: t.scroll,
            title: t.page.title.clone(),
            zoom: t.zoom,
        };
        let uid = t.uid;
        let before = t.page.doc.approx_bytes();
        if let Some(l) = t.load.take() {
            self.fetch.cancel(l.job);
        }
        let t = &mut self.tabs[i];
        if let Some(e) = t.history.current_mut() {
            e.scroll = d.scroll;
        }
        let title = d.title.clone();
        t.show(blank, 0);
        t.page.title = title;
        t.page.address = d.address.clone();
        t.discarded = Some(d);
        self.mem.forget_tab(uid);
        if self.mem.log {
            rt::println!(
                "[browser] discarded tab {} ({} KB of page)",
                i + 1,
                before >> 10
            );
        }
        self.dirty = true;
    }

    /// A discarded tab is opened: fetch its page again, where it was scrolled.
    pub fn restore_tab(&mut self, i: usize) {
        let Some(d) = self.tabs.get_mut(i).and_then(|t| t.discarded.take()) else {
            return;
        };
        let t = &mut self.tabs[i];
        t.zoom = d.zoom;
        if let Some(e) = t.history.current_mut() {
            e.scroll = d.scroll;
        }
        let target = browserkit::address::interpret(&d.address);
        self.go(i, target, false, None, None);
        if self.mem.log {
            rt::println!(
                "[browser] restoring discarded tab {} from {}",
                i + 1,
                d.address
            );
        }
    }

    /// Ask whether to stop the heaviest page (or frame). False if no realm holds enough to be
    /// worth asking about, or the bar is already up.
    pub fn ask_stop(&mut self) -> bool {
        if matches!(&self.notice, Some(n) if matches!(n.kind, crate::NoticeKind::AskStop { .. })) {
            return false;
        }
        let list: Vec<Realm> = self
            .mem
            .list()
            .into_iter()
            .filter(|r| {
                self.mem_waits
                    .get(&r.key)
                    .is_none_or(|until| *until <= now_ms())
            })
            .collect();
        let Some(key) = pick_heaviest(&list) else {
            return false;
        };
        let (uid, frame) = ((key >> 32), FrameId(key as u32));
        let Some(ti) = self.tabs.iter().position(|t| t.uid == uid) else {
            return false;
        };
        let used = self.mem.known(key).map_or(0, |k| k.used);
        let epoch = self.tabs[ti].epoch;
        let what = if frame.is_top() {
            "This page"
        } else if crate::workers::is_worker_frame(frame) {
            "A worker of this page"
        } else {
            "A frame on this page"
        };
        let text = alloc::format!(
            "{what} uses a lot of memory ({}). Stop it?",
            human(to_real(used))
        );
        self.notice = Some(crate::Notice {
            tab: uid,
            epoch,
            text,
            kind: crate::NoticeKind::AskStop { frame },
        });
        if self.mem.log {
            rt::println!(
                "[browser] memory: asking to stop tab {} frame {:?} ({} KB)",
                ti + 1,
                frame,
                to_real(used) >> 10
            );
        }
        self.dirty = true;
        true
    }

    /// The answer to the bar: Stop ends the scripts of the document it asked about, Wait leaves
    /// it alone for a while, OK closes a plain message.
    pub fn notice_answer(&mut self, stop: bool) {
        let Some(n) = self.notice.take() else { return };
        self.dirty = true;
        let Some(ti) = self
            .tabs
            .iter()
            .position(|t| t.uid == n.tab && t.epoch == n.epoch)
        else {
            return;
        };
        if n.kind == crate::NoticeKind::LoadMore {
            if stop {
                self.load_more(ti);
            }
            return;
        }
        if let crate::NoticeKind::AskStop { frame } = n.kind {
            let key = key_of(n.tab, frame);
            if stop && crate::workers::is_worker_frame(frame) {
                self.worker_end(crate::workers::worker_of(frame));
                self.mem.forget(key);
                self.say("Stopped the worker");
                self.mem_console(ti, ConsoleLevel::Warn, "You stopped a worker of this page because it used a lot of memory. The page, the Browser and your other tabs are not affected.");
            } else if stop {
                self.tabs[ti]
                    .page
                    .doc
                    .stop_scripts_because(frame, "you stopped it: it used a lot of memory");
                self.mem.forget(key);
                self.after_scripts(ti);
                self.say("Stopped the page's scripts");
                self.mem_console(ti, ConsoleLevel::Warn, "You stopped this page's scripts because it used a lot of memory. The Browser and your other tabs are not affected.");
            } else {
                self.mem_waits.insert(key, now_ms() + 60_000);
            }
        }
    }
}
