//! Background scrub: read every block reachable from a volume's committed state (live
//! tree, every snapshot and clone, shared subtrees and shared data once, dead-list chains,
//! the space map) straight from the disk, bypassing the caches, and check its checksum.
//! The scrub itself is the milkfs crate's (`milkfs::scrub`, re-exported here); this is the
//! thread, the rate limit, the status file and the audit log around it.
//!
//! A low-priority kernel thread (`milkfs-scrub`) runs it on request (`/sys/fs/milkfs/scrub`,
//! `snap scrub`) and on a schedule (default: every 7 days, at the earliest 5 minutes after
//! boot; the time of the last full scrub is kept in the superblock). It is rate limited
//! (default 32 MB/s, a token bucket), reads at most 128 KiB per request, never holds the
//! volume lock while reading (only to start, to end and to name a damaged file), and
//! yields to every normal-priority thread, so the desktop stays smooth.
//!
//! The state it checks is the one committed when it starts. Blocks freed while it runs
//! are held back from reuse (`Store::hold_frees`), so nothing it is about to read can be
//! overwritten; they count as free in the space map and are released at the end.
//!
//! A checksum error is reported with what the block is (data of a file, with its path and
//! the snapshot or clone it is in; a tree node; a dead-list or space-map block), printed,
//! kept in the status, and audit-logged (`milkfs.scrub.error`). Nothing is repaired: a
//! damaged file can be restored from a snapshot that has a good copy
//! (`versions restore`), a damaged node needs `check` (it can't be repaired either).

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub use ::milkfs::scrub::*;

use super::inode::Volume;
use crate::fs::errno::{Errno, Result};
use crate::kprintln;
use crate::sync::SpinLock;

/// Default rate limit (MB/s; 0 = unlimited).
pub const DEFAULT_RATE_MB: u32 = 32;
/// Default schedule: every 7 days.
pub const DEFAULT_EVERY_S: u64 = 7 * 86_400;
/// Scheduled scrubs wait this long after boot.
const FIRST_DELAY_NS: u64 = 300_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Running,
    Done,
    Stopped,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub phase: Phase,
    /// "requested" or "scheduled".
    pub why: &'static str,
    /// Volume being (or last) scrubbed.
    pub volume: String,
    pub started_ns: u64,
    pub elapsed_ns: u64,
    /// Blocks in use when it started (progress denominator) and verified so far.
    pub total: u64,
    pub done: u64,
    pub bytes: u64,
    pub errors: Vec<ScrubError>,
    /// Summary of the last finished scrub.
    pub last: String,
    /// Scrubs started this boot (one per volume).
    pub runs: u64,
}

static STATUS: SpinLock<Status> = SpinLock::new(Status {
    phase: Phase::Idle,
    why: "",
    volume: String::new(),
    started_ns: 0,
    elapsed_ns: 0,
    total: 0,
    done: 0,
    bytes: 0,
    errors: Vec::new(),
    last: String::new(),
    runs: 0,
});
static REQUEST: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
/// A run over all volumes is in progress.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static RATE_MB: AtomicU32 = AtomicU32::new(DEFAULT_RATE_MB);

pub fn status() -> Status {
    STATUS.lock().clone()
}

/// Is a scrub run (over all volumes) under way?
pub fn running() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Is a scrub reading the volume mounted at `path` right now?
pub fn running_on(path: &str) -> bool {
    let s = STATUS.lock();
    s.phase == Phase::Running && s.volume == path
}

/// Start the thread (after the scheduler runs).
pub fn start_thread() {
    drop(crate::sched::spawn_prio(
        "milkfs-scrub",
        crate::sched::Priority::Low,
        thread,
    ));
}

fn now_unix() -> u64 {
    crate::fs::clock::now().sec.max(0) as u64
}

fn thread() {
    let mut last_check = 0u64;
    loop {
        crate::sched::sleep_ms(200);
        let manual = REQUEST.swap(false, Ordering::AcqRel);
        let now = crate::timer::now_ns();
        let due = !manual && now >= FIRST_DELAY_NS && now - last_check >= 60_000_000_000 && {
            last_check = now;
            schedule_due()
        };
        if manual || due {
            run_all(if manual { "requested" } else { "scheduled" });
        }
    }
}

fn schedule_due() -> bool {
    let Some(v) = super::user_volume() else {
        return false;
    };
    let (last, every) = v.state.lock().scrub;
    match every {
        u32::MAX => false,
        0 => now_unix() >= last + DEFAULT_EVERY_S,
        s => now_unix() >= last + s as u64,
    }
}

/// Scrub every mounted volume (the primary one first).
fn run_all(why: &'static str) {
    STOP.store(false, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Release);
    let mut vols = super::all_volumes();
    if let Some(p) = super::user_volume() {
        vols.retain(|(v, _)| !Arc::ptr_eq(v, &p));
        vols.insert(0, (p, super::mount_path().unwrap_or_default()));
    }
    for (v, path) in vols {
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        let _ = scrub_volume(&v, &path, why, true);
    }
    ACTIVE.store(false, Ordering::Release);
}

/// `/sys/fs/milkfs/scrub` write: `start`, `stop`, `rate=MB/s` (0 = unlimited),
/// `auto=off|on|7d|12h|...`.
pub fn control(cmd: &str) -> Result<()> {
    for w in cmd.split_whitespace() {
        match w.split_once('=') {
            None if w == "start" => {
                if ACTIVE.load(Ordering::Acquire) {
                    return Err(Errno::EBUSY);
                }
                if super::all_volumes()
                    .iter()
                    .any(|(v, _)| v.state.lock().converting())
                {
                    return super::fail(
                        Errno::EBUSY,
                        "a volume is being converted in place: scrub after it is done",
                    );
                }
                REQUEST.store(true, Ordering::Release);
            }
            None if w == "stop" => STOP.store(true, Ordering::Relaxed),
            Some(("rate", x)) => {
                RATE_MB.store(x.parse().map_err(|_| Errno::EINVAL)?, Ordering::Relaxed)
            }
            Some(("auto", x)) => {
                let every = match x {
                    "off" => u32::MAX,
                    "on" => 0,
                    _ => parse_interval(x).ok_or(Errno::EINVAL)?,
                };
                let v = super::user_volume().ok_or(Errno::ENODEV)?;
                let mut st = v.state.lock();
                st.scrub.1 = every;
                st.meta_dirty = true;
                st.commit()?;
            }
            _ => return Err(Errno::EINVAL),
        }
    }
    Ok(())
}

fn parse_interval(v: &str) -> Option<u32> {
    let (num, mult) = match v.as_bytes().last()? {
        b's' => (&v[..v.len() - 1], 1),
        b'm' => (&v[..v.len() - 1], 60),
        b'h' => (&v[..v.len() - 1], 3600),
        b'd' => (&v[..v.len() - 1], 86_400),
        _ => return None,
    };
    let n: u64 = num.parse().ok()?;
    let s = n.checked_mul(mult)?;
    (s > 0 && s < u32::MAX as u64).then_some(s as u32)
}

fn every_text(every: u32) -> String {
    let s = match every {
        u32::MAX => return String::from("off"),
        0 => DEFAULT_EVERY_S,
        s => s as u64,
    };
    if s % 86_400 == 0 {
        format!("every {}d", s / 86_400)
    } else if s % 3600 == 0 {
        format!("every {}h", s / 3600)
    } else {
        format!("every {s}s")
    }
}

/// `/sys/fs/milkfs/scrub` read.
pub fn status_text() -> String {
    let s = status();
    let mut out = String::new();
    let mb = |bytes: u64, ns: u64| bytes as f64 / 1_048_576.0 / (ns.max(1) as f64 / 1e9);
    match s.phase {
        Phase::Running => {
            let el = crate::timer::now_ns() - s.started_ns;
            let _ = writeln!(
                out,
                "scrub: [run {}] running ({}) on {}: {}/{} blocks ({}%), {:.1} MB/s, {} error(s)",
                s.runs,
                s.why,
                s.volume,
                s.done,
                s.total,
                s.done * 100 / s.total.max(1),
                mb(s.bytes, el),
                s.errors.len()
            );
        }
        _ if ACTIVE.load(Ordering::Acquire) => {
            let _ = writeln!(out, "scrub: [run {}] running (next volume)", s.runs);
        }
        Phase::Idle => {
            let _ = writeln!(out, "scrub: idle (never run this boot)");
        }
        _ => {
            let _ = writeln!(out, "scrub: [run {}] {}", s.runs, s.last);
        }
    }
    let rate = RATE_MB.load(Ordering::Relaxed);
    let (last, every) = super::user_volume()
        .map(|v| v.state.lock().scrub)
        .unwrap_or((0, 0));
    let _ = writeln!(
        out,
        "rate: {}, auto: {}, last full scrub of the primary volume: {}",
        if rate == 0 {
            String::from("unlimited")
        } else {
            format!("{rate} MB/s")
        },
        every_text(every),
        if last == 0 {
            String::from("never")
        } else {
            super::timemachine::stamp(last)
        }
    );
    for e in &s.errors {
        let _ = writeln!(out, "error: block {}: {}", e.block, e.what);
    }
    out
}

/// Token bucket over bytes read.
struct Limiter {
    start: u64,
    bytes: u64,
    sleep: bool,
}

impl Limiter {
    fn account(&mut self, n: usize) {
        self.bytes += n as u64;
        let rate = RATE_MB.load(Ordering::Relaxed) as u64 * 1_048_576;
        if rate == 0 || !self.sleep {
            return;
        }
        let due = self.start + self.bytes * 1_000_000_000 / rate;
        let now = crate::timer::now_ns();
        if due > now {
            crate::sched::sleep_ns(due - now);
        }
    }
}

/// The kernel's side of a scrub: the stop flag, the rate limit, the status file, and
/// whether it counts as the primary volume's last full scrub.
struct Runner<'a> {
    v: &'a Arc<Volume>,
    path: &'a str,
    why: &'static str,
    status: bool,
    lim: Limiter,
}

impl ::milkfs::scrub::ScrubRunner for Runner<'_> {
    fn started(&mut self, total: u64) {
        if self.status {
            let mut s = STATUS.lock();
            s.phase = Phase::Running;
            s.runs += 1;
            s.why = self.why;
            s.volume = String::from(self.path);
            s.started_ns = self.lim.start;
            s.total = total;
            s.done = 0;
            s.bytes = 0;
            s.errors.clear();
        }
    }
    fn stop(&self) -> bool {
        STOP.load(Ordering::Relaxed)
    }
    fn read(&mut self, n: usize, done: u64) {
        self.lim.account(n);
        if self.status {
            let mut s = STATUS.lock();
            s.done = done;
            s.bytes = self.lim.bytes;
        }
    }
    fn records_last_scrub(&self) -> bool {
        super::user_volume().is_some_and(|p| Arc::ptr_eq(&p, self.v))
    }
}

/// Scrub one volume (`path`: where it is mounted, for naming files). `user`: a real
/// volume (rate limit, `/sys/fs/milkfs/scrub` status, audit log); false for the
/// self-test (before the scheduler runs).
pub fn scrub_volume(
    v: &Arc<Volume>,
    path: &str,
    why: &'static str,
    user: bool,
) -> Result<Vec<ScrubError>> {
    let (audit, sleep) = (user, user);
    if v.state.lock().converting() {
        kprintln!("milkfs scrub: {path}: skipped (being converted in place)");
        return Ok(Vec::new());
    }
    let t0 = crate::timer::now_ns();
    let mut run = Runner {
        v,
        path,
        why,
        status: user,
        lim: Limiter {
            start: t0,
            bytes: 0,
            sleep,
        },
    };
    let Scrubbed {
        walked,
        errors,
        done,
        bytes,
    } = ::milkfs::scrub::scrub(&**v, path, &mut run)?;
    let el = crate::timer::now_ns() - t0;
    for e in &errors {
        kprintln!(
            "milkfs scrub: {path}: CHECKSUM ERROR in block {}: {}",
            e.block,
            e.what
        );
        if audit {
            crate::audit::record(
                "milkfs.scrub.error",
                &format!("volume={path} block={} what={}", e.block, e.what),
            );
        }
    }
    let summary = format!(
        "{} on {path} ({why}): {} blocks ({} MiB) in {} ms, {:.1} MB/s, {} error(s){}",
        match walked {
            Ok(()) => "done",
            Err(Errno::EINTR) => "stopped",
            Err(_) => "failed",
        },
        done,
        bytes >> 20,
        el / 1_000_000,
        bytes as f64 / 1_048_576.0 / (el.max(1) as f64 / 1e9),
        errors.len(),
        match &walked {
            Err(e) if *e != Errno::EINTR => format!(" ({e})"),
            _ => String::new(),
        }
    );
    kprintln!("milkfs scrub: {summary}");
    if user {
        let mut s = STATUS.lock();
        s.phase = if walked.is_ok() {
            Phase::Done
        } else {
            Phase::Stopped
        };
        s.elapsed_ns = el;
        s.errors = errors.clone();
        s.last = summary;
    }
    walked.map(|_| errors)
}
