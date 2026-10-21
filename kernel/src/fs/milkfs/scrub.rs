//! Background scrub: read every block reachable from a volume's committed state (live
//! tree, every snapshot and clone, shared subtrees and shared data once, dead-list chains,
//! the space map) straight from the disk, bypassing the caches, and check its checksum.
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

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::format::*;
use super::inode::Volume;
use super::items::{LIVE, clone_view};
use crate::fs::blockdev::BlockRef;
use crate::fs::errno::{Errno, Result};
use crate::kprintln;
use crate::sync::SpinLock;

/// Default rate limit (MB/s; 0 = unlimited).
pub const DEFAULT_RATE_MB: u32 = 32;
/// Default schedule: every 7 days.
pub const DEFAULT_EVERY_S: u64 = 7 * 86_400;
/// Scheduled scrubs wait this long after boot.
const FIRST_DELAY_NS: u64 = 300_000_000_000;
/// Blocks per read request (128 KiB).
const CHUNK: u64 = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Running,
    Done,
    Stopped,
}

#[derive(Clone, Debug)]
pub struct ScrubError {
    pub block: u64,
    /// e.g. "data of /Volumes/Milk/docs/b.bin at offset 0 (live)".
    pub what: String,
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

/// Where a block was found (for naming it in a report).
#[derive(Clone)]
enum Owner {
    Node(String),
    /// View it was found in (names the file), ino, file offset. Every tree holding the
    /// block is looked up when the error is named.
    Data(u64, u64, u64),
    Meta(&'static str),
}

struct Scan {
    dev: BlockRef,
    /// Encrypted volume: blocks are opened (decrypted + tag checked) with this.
    key: Option<Arc<super::crypt::VolumeKey>>,
    per: u64,
    nodes: BTreeSet<u64>,
    data: BTreeSet<u64>,
    bad: Vec<(u64, Owner)>,
    done: u64,
    status: bool,
    lim: Limiter,
    buf: Vec<u8>,
}

impl Scan {
    /// Read `n` consecutive blocks from `block` raw into `self.buf`.
    fn read(&mut self, block: u64, n: u64) -> Result<()> {
        if STOP.load(Ordering::Relaxed) {
            return Err(Errno::EINTR);
        }
        let len = (n as usize) * BLOCK;
        self.buf.resize(len, 0);
        let r = self.dev.read_blocks(block * self.per, &mut self.buf[..len]);
        self.lim.account(len);
        self.done += n;
        if self.status {
            let mut s = STATUS.lock();
            s.done = self.done;
            s.bytes = self.lim.bytes;
        }
        r
    }

    /// Is the `i`-th block of the last read (disk block `block`, born `birth`) intact?
    /// (Encrypted: decrypts it in place, so a node can be decoded afterwards.)
    fn check(&mut self, i: u64, block: u64, birth: u64, csum: u64) -> bool {
        let b = &mut self.buf[(i as usize) * BLOCK..(i as usize + 1) * BLOCK];
        match &self.key {
            Some(k) => k.open(block, birth, csum, b),
            None => xxh64(b) == csum,
        }
    }

    fn tree(&mut self, label: &str, view: u64, root: BlockPtr) -> Result<()> {
        let mut stack = alloc::vec![root];
        while let Some(p) = stack.pop() {
            if p.is_none() || !self.nodes.insert(p.block) {
                continue;
            }
            let ok = match self.read(p.block, 1) {
                Ok(()) => self.check(0, p.block, p.birth, p.csum),
                Err(Errno::EINTR) => return Err(Errno::EINTR),
                Err(_) => false,
            };
            if !ok {
                self.bad.push((p.block, Owner::Node(String::from(label))));
                continue; // can't trust what it points to
            }
            let node = decode_node(self.buf[..BLOCK].try_into().unwrap())?;
            match node {
                DiskNode::Internal(_, kids) => stack.extend(kids.iter().map(|(_, c)| *c)),
                DiskNode::Leaf(items) => {
                    for (k, v) in &items {
                        if let Some(e) = Ext::decode(k, v) {
                            self.run(view, k.obj, &e)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn run(&mut self, view: u64, ino: u64, e: &Ext) -> Result<()> {
        let n = e.len();
        let mut i = 0;
        while i < n {
            let c = CHUNK.min(n - i);
            if (i..i + c).all(|j| self.data.contains(&(e.block + j))) {
                i += c;
                continue;
            }
            let read = self.read(e.block + i, c);
            if read == Err(Errno::EINTR) {
                return read;
            }
            for j in 0..c {
                let b = e.block + i + j;
                if !self.data.insert(b) {
                    continue;
                }
                if read.is_err() || !self.check(j, b, e.birth, e.csums[(i + j) as usize]) {
                    let off = (e.start + i + j) * BLOCK as u64;
                    self.bad.push((b, Owner::Data(view, ino, off)));
                }
            }
            i += c;
        }
        Ok(())
    }

    fn meta(&mut self, p: BlockPtr, what: &'static str) -> Result<bool> {
        if p.is_none() || !self.nodes.insert(p.block) {
            return Ok(false);
        }
        let ok = match self.read(p.block, 1) {
            Ok(()) => self.check(0, p.block, p.birth, p.csum),
            Err(Errno::EINTR) => return Err(Errno::EINTR),
            Err(_) => false,
        };
        if !ok {
            self.bad.push((p.block, Owner::Meta(what)));
        }
        Ok(ok)
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
    let key = v.state.lock().store.crypt.clone();
    let (dev, trees, heads, map, total) = {
        let mut st = v.state.lock();
        st.commit()?;
        st.store.hold_frees();
        let mut trees = alloc::vec![(String::from("live"), LIVE, st.sb.root)];
        trees.extend(
            st.snaps
                .iter()
                .map(|s| (format!("snapshot {}", s.name), s.generation, s.root)),
        );
        trees.extend(
            st.clones
                .iter()
                .map(|c| (format!("clone {}", c.name), clone_view(c.id), c.root)),
        );
        let mut heads = alloc::vec![st.deadlist];
        heads.extend(st.snaps.iter().map(|s| s.deadlist));
        let total = st.store.total() - st.store.free_blocks();
        (
            st.store.cache.device().clone(),
            trees,
            heads,
            st.sb.spacemap,
            total,
        )
    };
    if user {
        let mut s = STATUS.lock();
        s.phase = Phase::Running;
        s.runs += 1;
        s.why = why;
        s.volume = String::from(path);
        s.started_ns = t0;
        s.total = total;
        s.done = 0;
        s.bytes = 0;
        s.errors.clear();
    }
    let per = (BLOCK / dev.block_size()) as u64;
    let mut sc = Scan {
        dev,
        key,
        per,
        nodes: BTreeSet::new(),
        data: BTreeSet::new(),
        bad: Vec::new(),
        done: 0,
        status: user,
        lim: Limiter {
            start: t0,
            bytes: 0,
            sleep,
        },
        buf: Vec::new(),
    };
    let walked = (|| -> Result<()> {
        for (label, view, root) in &trees {
            sc.tree(label, *view, *root)?;
        }
        for h in heads {
            let mut p = h;
            while sc.meta(p, "dead-list block")? {
                let b: &[u8; BLOCK] = sc.buf[..BLOCK].try_into().unwrap();
                p = decode_dead(b)
                    .map(|(_, next)| next)
                    .unwrap_or(BlockPtr::NONE);
            }
        }
        if sc.meta(map, "space-map index")? {
            let b: &[u8; BLOCK] = sc.buf[..BLOCK].try_into().unwrap();
            for c in MapIndex::decode(b).map(|i| i.chunks).unwrap_or_default() {
                sc.meta(c, "space-map block")?;
            }
        }
        Ok(())
    })();
    // Done reading: free what was held, and name what was found.
    let mut errors = Vec::new();
    {
        let mut st = v.state.lock();
        st.store.release_held();
        for (block, owner) in &sc.bad {
            let what = match owner {
                Owner::Node(t) => format!("tree node of {t}"),
                Owner::Meta(m) => String::from(*m),
                Owner::Data(view, ino, off) => {
                    let p = st
                        .path_in(*view, *ino)
                        .map(|p| format!("{path}{p}"))
                        .unwrap_or_else(|| format!("inode {ino}"));
                    // Every tree that has this block there (shared subtrees were read once).
                    let idx = off / BLOCK as u64;
                    let holders: Vec<&str> = trees
                        .iter()
                        .filter(|(_, tv, _)| {
                            st.exts(*tv, *ino, idx, idx + 1).is_ok_and(|es| {
                                es.iter().any(|e| e.block + (idx - e.start) == *block)
                            })
                        })
                        .map(|(l, _, _)| l.as_str())
                        .collect();
                    format!("data of {p} at offset {off} ({})", holders.join(", "))
                }
            };
            errors.push(ScrubError {
                block: *block,
                what,
            });
        }
        if walked.is_ok() && super::user_volume().is_some_and(|p| Arc::ptr_eq(&p, v)) {
            st.scrub.0 = now_unix();
            st.meta_dirty = true; // persisted with the next commit
        }
    }
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
        sc.done,
        sc.lim.bytes >> 20,
        el / 1_000_000,
        sc.lim.bytes as f64 / 1_048_576.0 / (el.max(1) as f64 / 1e9),
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
