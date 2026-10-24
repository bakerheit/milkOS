//! Memory for the Browser's scripts and pages: how much each page, frame and worker may use,
//! worked out from what the machine has free, and what to do when the machine runs short.
//! No I/O: the app reads `/proc/meminfo` and `/proc/mempressure` (see [`Machine::parse`]), asks
//! [`plan`] what every realm may have, and runs the steps [`Ladder`] asks for.
//!
//! # Budgets
//!
//! A real browser does not have a fixed size for a page; it lets a page grow while the machine
//! has room and takes room back when it does not. The same here:
//!
//! ```text
//!   available (MemAvailable)
//!     - floor            what always stays free for the desktop and the other apps: the
//!                        kernel's low mark (1/8 of RAM) and a reserve for what grows
//!                        meanwhile (a sixteenth of RAM, at least 32 MiB)
//!   = room               what the Browser's scripts may still grow into, in all
//! ```
//!
//! The room is shared out by weight among all the realms of all tabs: a visible page gets 8, a
//! visible frame or worker 4, anything in a background tab 1, and the rest of the system 4 more
//! (it may grow too). A realm's limit is what it holds now plus its share of the room, never
//! less than a few MiB and never more than a hard ceiling sized from RAM (the part of memory one
//! realm may ever have, so that nothing can take the machine down however much is free). A
//! realm that holds more than its limit when the room shrinks is not stopped at once: the limit
//! is never below what it holds plus an eighth; it only cannot grow, and runs out of memory if
//! it tries. Everything is recomputed from the numbers of the moment, so limits go up and down
//! with the machine, with the number of tabs and with what they hold.
//!
//! The script engine counts its heap in its own estimate of bytes, which is less than what the
//! heap really takes from the system ([`OVERHEAD_PCT`]); budgets are in the engine's bytes, the
//! machine's numbers in real ones, and [`plan`] converts.
//!
//! # Pressure
//!
//! [`Machine::level`] says how short memory is: `Normal`, `Tight` (the room is nearly gone: the
//! Browser itself got to its floor), `Low` and `Critical` (the kernel's own marks). A [`Ladder`]
//! turns that into steps, cheapest first: drop caches, collect garbage, discard a background
//! tab, ask to stop the heaviest page. See [`Step`].

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub const KIB: usize = 1 << 10;
pub const MIB: usize = 1 << 20;

/// What the script heap really takes, in percent of what the engine counts (its estimate of
/// the bytes of objects, strings and code, garbage not yet collected included). Measured: the
/// engine asks the allocator for 70% (arrays of small arrays) to 98% (typed arrays) of its own
/// estimate, and the allocator rounds small blocks up by an eighth or so; 110% leaves a
/// little slack on top of that (docs/browser.md "Memory").
pub const OVERHEAD_PCT: usize = 110;

/// Real bytes to the engine's bytes.
pub fn to_engine(real: usize) -> usize {
    (real / OVERHEAD_PCT) * 100 + (real % OVERHEAD_PCT) * 100 / OVERHEAD_PCT
}

/// The engine's bytes to real ones.
pub fn to_real(engine: usize) -> usize {
    (engine / 100)
        .saturating_mul(OVERHEAD_PCT)
        .saturating_add((engine % 100) * OVERHEAD_PCT / 100)
}

// ---- what the machine says --------------------------------------------------------------------

/// The machine's memory, in bytes, as the kernel reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Machine {
    pub total: usize,
    /// What a program can still get.
    pub available: usize,
    /// The kernel's marks: under `low` memory is low, under `critical` critical.
    pub low: usize,
    pub critical: usize,
    /// The kernel's counters (since boot): times free memory fell below the low and critical
    /// marks, and allocations that found no memory.
    pub low_events: u64,
    pub critical_events: u64,
    pub failures: u64,
}

fn kb_of(text: &str, key: &str) -> Option<usize> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.strip_prefix(':')?;
        v.split_whitespace().next()?.parse::<usize>().ok()
    })
}

fn count_of(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.strip_prefix(':')?;
        v.split_whitespace().next()?.parse::<u64>().ok()
    })
}

impl Machine {
    /// `/proc/meminfo` and `/proc/mempressure` (either may be empty: the marks are then 1/8 and
    /// 1/16 of RAM, and without MemAvailable MemFree is what is available). None when there is
    /// no MemTotal.
    pub fn parse(meminfo: &str, pressure: &str) -> Option<Machine> {
        let total = kb_of(meminfo, "MemTotal")?.checked_mul(KIB)?;
        if total == 0 {
            return None;
        }
        let available = kb_of(meminfo, "MemAvailable")
            .or_else(|| kb_of(meminfo, "MemFree"))?
            .saturating_mul(KIB)
            .min(total);
        let low = kb_of(pressure, "LowMark").map_or(total / 8, |k| k.saturating_mul(KIB));
        let critical = kb_of(pressure, "CriticalMark").map_or(total / 16, |k| k.saturating_mul(KIB));
        Some(Machine {
            total,
            available,
            low: low.min(total),
            critical: critical.min(low).min(total),
            low_events: count_of(pressure, "LowEvents").unwrap_or(0),
            critical_events: count_of(pressure, "CriticalEvents").unwrap_or(0),
            failures: count_of(pressure, "AllocFailures").unwrap_or(0),
        })
    }

    /// A machine with `total` bytes of which `available` can be had, the kernel's usual marks.
    pub fn new(total: usize, available: usize) -> Machine {
        Machine {
            total,
            available: available.min(total),
            low: total / 8,
            critical: total / 16,
            ..Machine::default()
        }
    }

    /// What is kept for others to grow into while the Browser's scripts do: a sixteenth of RAM,
    /// at least 32 MiB.
    pub fn reserve(&self) -> usize {
        (self.total / 16).max(32 * MIB)
    }

    /// Free memory below which the Browser's scripts stop growing: the kernel's low mark and
    /// the reserve.
    pub fn floor(&self) -> usize {
        self.low.saturating_add(self.reserve())
    }

    /// What the Browser's scripts (and the buffers pages and requests hold) may still grow
    /// into, in real bytes.
    pub fn room(&self) -> usize {
        self.available.saturating_sub(self.floor())
    }

    /// How short memory is.
    pub fn level(&self) -> Level {
        if self.total == 0 {
            return Level::Normal;
        }
        if self.available < self.critical {
            Level::Critical
        } else if self.available < self.low {
            Level::Low
        } else if self.room() < (self.total / 64).max(8 * MIB) {
            Level::Tight
        } else {
            Level::Normal
        }
    }
}

/// How short memory is, mildest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal,
    /// The room is nearly gone: scripts cannot grow any more.
    Tight,
    /// Under the kernel's low mark.
    Low,
    /// Under the kernel's critical mark.
    Critical,
}

// ---- the plan -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Page,
    Frame,
    Worker,
}

/// One script realm: a page, a frame or a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Realm {
    /// Names it for the caller (the plan hands limits back by key).
    pub key: u64,
    pub kind: Kind,
    /// It is in the tab in front.
    pub foreground: bool,
    /// The heap it holds now (the engine's bytes).
    pub used: usize,
}

/// The least a realm is ever given to work with (engine bytes), room or no room: enough for an
/// ordinary page's scripts.
pub fn min_limit(kind: Kind) -> usize {
    match kind {
        Kind::Page => 16 * MIB,
        Kind::Frame | Kind::Worker => 8 * MIB,
    }
}

/// The most one realm may ever have (engine bytes), however much is free: sized from RAM to
/// protect the system, not to tune a page.
pub fn ceiling(m: &Machine, kind: Kind) -> usize {
    let real = match kind {
        Kind::Page => m.total / 8 * 5,
        Kind::Frame | Kind::Worker => m.total / 2,
    };
    to_engine(real).max(min_limit(kind))
}

fn weight(kind: Kind, foreground: bool) -> usize {
    match (kind, foreground) {
        (Kind::Page, true) => 8,
        (Kind::Frame | Kind::Worker, true) => 4,
        (_, false) => 1,
    }
}

/// The rest of the system, which may grow too, counts as this much weight.
const SYSTEM_WEIGHT: usize = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub machine: Machine,
    pub level: Level,
    /// The room, real bytes and the engine's.
    pub room: usize,
    pub room_engine: usize,
    /// Sum of the weights of the realms and of the rest of the system.
    pub weights: usize,
    limits: BTreeMap<u64, usize>,
}

impl Default for Level {
    fn default() -> Level {
        Level::Normal
    }
}

impl Budget {
    /// The heap limit (engine bytes) of the realm `key` in the plan.
    pub fn limit_of(&self, key: u64) -> Option<usize> {
        self.limits.get(&key).copied()
    }

    pub fn len(&self) -> usize {
        self.limits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.limits.is_empty()
    }

    /// What a realm that does not exist yet would get now (engine bytes).
    pub fn for_new(&self, kind: Kind, foreground: bool) -> usize {
        self.limit(kind, foreground, 0, weight(kind, foreground))
    }

    fn limit(&self, kind: Kind, foreground: bool, used: usize, extra_weight: usize) -> usize {
        let w = weight(kind, foreground);
        let head = self.room_engine / (self.weights + extra_weight).max(1) * w;
        let raw = used.saturating_add(head);
        // (A realm is never squeezed under what it holds, and a little over.)
        let kept = used.saturating_add(used / 8).saturating_add(256 * KIB);
        let limit = raw.max(kept).max(min_limit(kind));
        limit.min(ceiling(&self.machine, kind))
    }

    /// The most a single thing the Browser holds as a whole in memory may be (a script's text,
    /// a response read to the end, a page's bytes), in real bytes. See [`Caps`].
    pub fn caps(&self) -> Caps {
        Caps::from_room(self.room)
    }
}

/// The plan: a heap limit for each of `realms` (and what a new one would get), from the
/// machine's numbers. Pure; call it again when anything changes.
pub fn plan(m: &Machine, realms: &[Realm]) -> Budget {
    let room = m.room();
    let weights = realms.iter().map(|r| weight(r.kind, r.foreground)).sum::<usize>() + SYSTEM_WEIGHT;
    let mut b = Budget {
        machine: *m,
        level: m.level(),
        room,
        room_engine: to_engine(room),
        weights,
        limits: BTreeMap::new(),
    };
    for r in realms {
        let l = b.limit(r.kind, r.foreground, r.used, 0);
        b.limits.insert(r.key, l);
    }
    b
}

// ---- the other limits ------------------------------------------------------------------------------

/// How big one thing may be that the Browser holds whole in memory (real bytes), from the room.
/// These replace the fixed caps on a script, a stylesheet, a picture, a download held in
/// memory and a response read to the end; what streams (a page as it arrives, a response body
/// read piece by piece, a download) is not held and has no cap here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// A page's own text: what is taken in, and parsed, before the rest is offered with "Load
    /// more".
    pub page: usize,
    /// One external script's text.
    pub script: usize,
    /// One stylesheet.
    pub style: usize,
    /// One picture's file.
    pub image: usize,
    /// A script's response read to the end (`.text()`, `.json()`, `.arrayBuffer()`, XHR).
    pub response: usize,
    /// DOM nodes of one page.
    pub nodes: usize,
    /// Decoded pictures kept for all tabs, in pixels.
    pub image_pixels: usize,
    /// Script text kept for tabs to share, in bytes.
    pub script_cache: usize,
}

impl Caps {
    pub fn from_room(room: usize) -> Caps {
        let part = |div: usize, lo: usize, hi: usize| (room / div).clamp(lo, hi);
        Caps {
            page: part(8, 16 * MIB, 256 * MIB),
            script: part(16, 4 * MIB, 128 * MIB),
            style: part(32, 2 * MIB, 64 * MIB),
            image: part(16, 8 * MIB, 128 * MIB),
            response: part(8, 8 * MIB, 512 * MIB),
            nodes: part(2 * KIB, 200_000, 8_000_000),
            image_pixels: part(8, 8 * MIB, 128 * MIB) / 4,
            script_cache: part(16, 16 * MIB, 256 * MIB),
        }
    }
}

impl Default for Caps {
    /// What the Browser had before budgets, for when the machine cannot be read.
    fn default() -> Caps {
        Caps {
            page: 16 * MIB,
            script: 4 * MIB,
            style: 2 * MIB,
            image: 8 * MIB,
            response: 8 * MIB,
            nodes: 1_000_000,
            image_pixels: 16 << 20,
            script_cache: 16 * MIB,
        }
    }
}

// ---- reacting to pressure -------------------------------------------------------------------------------

/// What to do about low memory, cheapest first. The app does one at a time and says whether it
/// freed anything (the [`Ladder`] goes on at once if not).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Drop what can be fetched or made again: decoded pictures nobody shows, fetched script
    /// and style text, parsed-ahead pages.
    DropCaches,
    /// Collect garbage in every realm, harder than usual.
    Collect,
    /// Discard the biggest background tab (it reloads when it is opened again).
    DiscardTab,
    /// Ask the user to stop the heaviest page's scripts.
    AskStop,
}

/// How long memory must be fine before the ladder starts over.
const CALM_MS: u64 = 3_000;
/// The time between steps while memory stays short (Critical: none).
const STEP_MS: u64 = 1_000;
/// Caches are dropped again after this long if memory is still short.
const AGAIN_MS: u64 = 10_000;

/// The order in which a short machine is relieved. Call [`Ladder::next`] every time memory was
/// looked at (at most once a second is plenty).
#[derive(Clone, Debug, Default)]
pub struct Ladder {
    stage: u8,
    last_step: u64,
    started: u64,
    calm_since: Option<u64>,
}

impl Ladder {
    pub fn new() -> Ladder {
        Ladder::default()
    }

    /// Has the ladder been climbed at all (memory was short and has not been fine for long).
    pub fn active(&self) -> bool {
        self.stage > 0
    }

    /// The step to take now, if any. `level`: how short memory is; `now`: ms on a monotonic
    /// clock. After a step that freed nothing call again at once ([`Ladder::skip`] is the same
    /// as calling `next` with the clock moved on).
    pub fn next(&mut self, level: Level, now: u64) -> Option<Step> {
        if level == Level::Normal {
            let since = *self.calm_since.get_or_insert(now);
            if now.saturating_sub(since) >= CALM_MS {
                *self = Ladder::default();
            }
            return None;
        }
        self.calm_since = None;
        if self.stage == 0 {
            self.stage = 1;
            self.started = now;
            self.last_step = now;
            return Some(Step::DropCaches);
        }
        let wait = if level == Level::Critical { 0 } else { STEP_MS };
        if now.saturating_sub(self.last_step) < wait {
            return None;
        }
        self.last_step = now;
        match self.stage {
            1 => {
                self.stage = 2;
                Some(Step::Collect)
            }
            2 => {
                // (Stay here while tabs can still be discarded: `skip` moves on.)
                Some(Step::DiscardTab)
            }
            _ => {
                if now.saturating_sub(self.started) >= AGAIN_MS {
                    self.started = now;
                    self.stage = 1;
                    return Some(Step::DropCaches);
                }
                Some(Step::AskStop)
            }
        }
    }

    /// The step just returned freed nothing (no background tab was left, say): move on without
    /// waiting.
    pub fn skip(&mut self, step: Step) {
        if step == Step::DiscardTab && self.stage == 2 {
            self.stage = 3;
        }
        self.last_step = 0;
    }
}

/// A tab as [`pick_discard`] sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabInfo {
    /// Its place in the app's list.
    pub index: usize,
    pub foreground: bool,
    /// It is loading something, or the user has typed into it (a discard would lose that).
    pub busy: bool,
    pub touched: bool,
    /// What it holds: script heap and page, real bytes.
    pub bytes: usize,
    /// Milliseconds since it was last in front.
    pub idle_ms: u64,
    /// Already discarded.
    pub discarded: bool,
}

/// The background tab to discard: the one holding the most, among those that have been idle for
/// a moment. Tabs the user typed into are left alone unless `desperate`. None if there is no
/// candidate worth it (a tab that holds under 512 KiB is not).
pub fn pick_discard(tabs: &[TabInfo], desperate: bool) -> Option<usize> {
    tabs.iter()
        .filter(|t| !t.foreground && !t.discarded && !t.busy && (desperate || !t.touched))
        .filter(|t| t.bytes >= 512 * KIB && t.idle_ms >= 1_000)
        .max_by_key(|t| (t.bytes, t.idle_ms))
        .map(|t| t.index)
}

/// The realm to ask about stopping: the biggest holder, the one in front first among equals.
pub fn pick_heaviest(realms: &[Realm]) -> Option<u64> {
    realms
        .iter()
        .max_by_key(|r| (r.used, r.foreground))
        .filter(|r| r.used >= 8 * MIB)
        .map(|r| r.key)
}

/// Words for a size: "812 KB", "37 MB", "1.2 GB".
pub fn human(bytes: usize) -> alloc::string::String {
    use alloc::format;
    if bytes >= 1 << 30 {
        let t = bytes * 10 >> 30;
        format!("{}.{} GB", t / 10, t % 10)
    } else if bytes >= MIB {
        format!("{} MB", bytes / MIB)
    } else {
        format!("{} KB", bytes / KIB)
    }
}

/// Keys of the realms in `realms` as a list, for the callers that only need to know who is there.
pub fn keys(realms: &[Realm]) -> Vec<u64> {
    realms.iter().map(|r| r.key).collect()
}

#[cfg(test)]
mod tests;
