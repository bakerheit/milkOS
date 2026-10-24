//! Tamper-evident audit log of security-relevant events. See `docs/security.md`.
//!
//! Every record is one text line:
//!
//! ```text
//! <seq> <time_ns> <pid> <exe> <event> <detail> <prev_hash> <hash>
//! ```
//!
//! `exe` and `detail` are %-escaped (no spaces, newlines or `%` inside a field), hashes are
//! lowercase hex SHA-256, and `hash = SHA-256(everything before " <hash>")`. Each record's
//! `prev_hash` is the previous record's `hash` (all zeros for the very first record), so
//! changing, inserting or deleting a record breaks the chain from that point on.
//!
//! Storage: a RAM ring of the newest [`RING`] records (`/proc/audit`), and, once the user's
//! milkfs volume is mounted, `/Volumes/Milk/var/log/audit.log`, appended to by the "audit"
//! kernel thread only (user processes get EPERM when they try to open it for writing,
//! truncate, rename or unlink it: [`sealed`], checked by `fs::ops`). Records made before
//! the volume is attached (early boot) are re-chained onto the file's last record when it
//! is; the file's own chain is verified at that point and a break is itself logged
//! (`audit.tamper`).
//!
//! [`record`] is cheap (one SHA-256 of ~150 bytes under a spinlock) and never blocks; it
//! must be called in thread context (it allocates), never from an IRQ handler, and not
//! while holding the calling process's `inner` lock (it reads the caller's exe).

pub mod sha256;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::fs::OpenFile;
use crate::fs::node::InodeRef;
use crate::kprintln;
use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;
use sha256::{Digest, hex};

/// Records kept in RAM.
pub const RING: usize = 2048;
pub const LOG_DIR: &str = "/Volumes/Milk/var/log";
pub const LOG_PATH: &str = "/Volumes/Milk/var/log/audit.log";
const VOLUME: &str = "/Volumes/Milk";
/// Read at most this much of an existing log when attaching.
const MAX_LOG: usize = 16 << 20;

#[derive(Clone)]
struct Rec {
    seq: u64,
    time_ns: u64,
    pid: u32,
    exe: String,
    event: String,
    detail: String,
    prev: Digest,
    hash: Digest,
    /// Already written to the log file.
    persisted: bool,
}

impl Rec {
    fn body(&self) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "{} {} {} {} {} {} {}",
            self.seq,
            self.time_ns,
            self.pid,
            escape(&self.exe),
            escape(&self.event),
            escape(&self.detail),
            hex(&self.prev)
        );
        s
    }
    fn seal(&mut self) {
        self.hash = sha256::sha256(self.body().as_bytes());
    }
    fn line(&self) -> String {
        let mut s = self.body();
        s.push(' ');
        s.push_str(&hex(&self.hash));
        s
    }
}

struct Log {
    ring: VecDeque<Rec>,
    next_seq: u64,
    last: Digest,
    /// The log file, once attached.
    file: Option<Arc<OpenFile>>,
    /// (dev, ino) of the log file: user processes may not modify it.
    sealed: Option<(u64, u64)>,
    dropped_unpersisted: u64,
    total: u64,
}

static LOG: SpinLock<Log> = SpinLock::new(Log {
    ring: VecDeque::new(),
    next_seq: 1,
    last: [0; 32],
    file: None,
    sealed: None,
    dropped_unpersisted: 0,
    total: 0,
});
static WQ: WaitQueue = WaitQueue::new();
static KICK: AtomicBool = AtomicBool::new(false);
static REATTACH: AtomicBool = AtomicBool::new(false);
static THREAD: AtomicBool = AtomicBool::new(false);
static PERSISTED: AtomicU64 = AtomicU64::new(0);

/// %-escape space, `%`, control characters and non-ASCII, so a field is one token.
pub fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for b in s.bytes() {
        if b <= b' ' || b == b'%' || b >= 0x7f {
            let _ = write!(o, "%{b:02X}");
        } else {
            o.push(b as char);
        }
    }
    if o.is_empty() {
        o.push('-');
    }
    o
}

fn now_ns() -> u64 {
    let t = crate::fs::clock::now();
    t.sec as u64 * 1_000_000_000 + t.nsec as u64
}

/// (pid, exe) of the caller: a user process, or `(0, "[kernel]")` for kernel threads.
pub fn whoami() -> (u32, String) {
    if !sched::started() {
        return (0, String::from("[kernel]"));
    }
    let pid = sched::current_pid();
    if pid == 0 {
        return (0, String::from("[kernel]"));
    }
    match crate::proc::get(pid) {
        Some(p) => {
            let exe = p.inner.lock().exe.clone();
            (
                pid,
                if exe.is_empty() {
                    String::from("?")
                } else {
                    exe
                },
            )
        }
        None => (pid, String::from("?")),
    }
}

/// Append an event on behalf of the calling thread.
pub fn record(event: &str, detail: &str) {
    let (pid, exe) = whoami();
    record_as(pid, &exe, event, detail);
}

/// Append an event with an explicit subject.
pub fn record_as(pid: u32, exe: &str, event: &str, detail: &str) {
    let mut detail = String::from(detail);
    detail.truncate(512);
    {
        let mut l = LOG.lock();
        let mut r = Rec {
            seq: l.next_seq,
            time_ns: now_ns(),
            pid,
            exe: String::from(exe),
            event: String::from(event),
            detail,
            prev: l.last,
            hash: [0; 32],
            persisted: false,
        };
        r.seal();
        l.next_seq += 1;
        l.last = r.hash;
        l.total += 1;
        if l.ring.len() == RING
            && let Some(old) = l.ring.pop_front()
            && !old.persisted
        {
            l.dropped_unpersisted += 1;
        }
        l.ring.push_back(r);
    }
    if THREAD.load(Ordering::Acquire) {
        KICK.store(true, Ordering::Release);
        WQ.wake_all();
    }
}

/// Convenience: `record(event, format!(...))`.
#[macro_export]
macro_rules! audit {
    ($event:expr, $($arg:tt)*) => {
        $crate::audit::record($event, &alloc::format!($($arg)*))
    };
}

/// Is this inode the sealed log file? (`fs::ops` refuses user writes to it.)
pub fn sealed(node: &InodeRef) -> bool {
    match LOG.try_lock() {
        Some(l) => l.sealed == Some((node.dev(), node.ino())),
        None => false,
    }
}

/// Is the caller a user process (as opposed to a kernel thread)?
pub fn user_context() -> bool {
    sched::started() && sched::with_current_user(|u| u.is_some())
}

/// Serialises flushes against detaching (a flush writes with no spinlock held).
static FLUSH: sched::Mutex<()> = sched::Mutex::new(());

/// Stop writing to the log file (waits for a flush in progress). Records keep collecting.
fn detach() {
    let _f = FLUSH.lock();
    let mut l = LOG.lock();
    l.file = None;
    l.sealed = None;
}

/// About to roll the volume back or promote a clone: stop writing to the file now, so no
/// flush lands in the wrong tree. [`volume_rewound`] re-attaches afterwards.
pub fn before_rewind() {
    detach();
}

/// The volume was rolled back or replaced by a clone: re-read the file before appending.
pub fn volume_rewound() {
    detach();
    REATTACH.store(true, Ordering::Release);
    if THREAD.load(Ordering::Acquire) {
        KICK.store(true, Ordering::Release);
        WQ.wake_all();
    }
}

// ---- parsing and verification (shared logic with the `audit` user tool) ----------------

fn unhex(s: &str) -> Option<Digest> {
    if s.len() != 64 {
        return None;
    }
    let mut d = [0u8; 32];
    for (i, b) in d.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(d)
}

/// Result of checking a chain of lines.
#[derive(Clone, Copy, Debug, Default)]
pub struct Verify {
    pub records: u64,
    pub last_seq: u64,
    pub last_hash: Digest,
    /// Sequence number (or 1-based line number if unparsable) of the first bad record.
    pub first_bad: Option<u64>,
}

/// Check `text` (one record per line). `start_prev`: required prev of the first record
/// (None: whatever it says).
pub fn verify(text: &str, start_prev: Option<Digest>) -> Verify {
    let mut v = Verify::default();
    let mut prev = start_prev;
    let mut want_seq: Option<u64> = None;
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let Some((body, h)) = line.rsplit_once(' ') else {
            v.first_bad.get_or_insert(n as u64 + 1);
            break;
        };
        let f: Vec<&str> = body.split(' ').collect();
        let seq = f.first().and_then(|s| s.parse::<u64>().ok());
        let (Some(seq), true, Some(hash), Some(p)) =
            (seq, f.len() == 7, unhex(h), f.get(6).and_then(|x| unhex(x)))
        else {
            v.first_bad.get_or_insert(n as u64 + 1);
            break;
        };
        let ok = sha256::sha256(body.as_bytes()) == hash
            && prev.is_none_or(|q| q == p)
            && want_seq.is_none_or(|w| w == seq);
        if !ok {
            v.first_bad = Some(seq);
            break;
        }
        v.records += 1;
        v.last_seq = seq;
        v.last_hash = hash;
        prev = Some(hash);
        want_seq = Some(seq + 1);
    }
    v
}

// ---- the persister ------------------------------------------------------------------------

fn volume_mounted() -> bool {
    crate::fs::milkfs::user_volume().is_some()
        && crate::fs::path::mounts().iter().any(|m| m.path == VOLUME)
}

/// Open (or create) the log file, verify it, re-chain the not-yet-persisted RAM records
/// onto its end. Returns a summary for the boot log.
fn attach() -> Option<String> {
    use crate::fs::abi::{O_APPEND, O_CREAT, O_WRONLY};
    crate::fs::ops::mkdir_all(LOG_DIR, 0o755).ok()?;
    let existing = crate::fs::ops::read_file(LOG_PATH).unwrap_or_default();
    let existing = &existing[existing.len().saturating_sub(MAX_LOG)..];
    let text = String::from_utf8_lossy(existing);
    let v = verify(&text, None);
    let f = Arc::new(crate::fs::ops::open(LOG_PATH, O_WRONLY | O_APPEND | O_CREAT, 0o644).ok()?);
    let id = (f.inode().dev(), f.inode().ino());
    let mut summary = String::new();
    let mut rechained = false;
    {
        let mut l = LOG.lock();
        // Where the file says the chain goes on.
        let (seq0, prev0) = if v.records == 0 {
            (1, [0; 32])
        } else {
            (v.last_seq + 1, v.last_hash)
        };
        let continues = l.ring.iter().any(|r| r.seq == seq0 && r.prev == prev0);
        if continues {
            // The file is a prefix of our chain (first attach of a fresh log, or a
            // rollback/promote rewound it): (re)write everything after its end as it is.
            for r in l.ring.iter_mut() {
                r.persisted = r.seq < seq0;
            }
        } else {
            // A different history (boot: early records were chained provisionally; or
            // the ring no longer reaches back far enough): re-chain what the file does not
            // have onto its end. Records only an older tree had are dropped from RAM.
            rechained = true;
            let keep: Vec<Rec> = l.ring.drain(..).filter(|r| !r.persisted).collect();
            let (mut seq, mut prev) = (seq0, prev0);
            for mut r in keep {
                r.seq = seq;
                r.prev = prev;
                r.seal();
                prev = r.hash;
                seq += 1;
                l.ring.push_back(r);
            }
            l.next_seq = seq;
            l.last = prev;
        }
        l.file = Some(f);
        l.sealed = Some(id);
        let _ = write!(
            summary,
            "{} has {} records (last seq {}), chain {}, {}",
            LOG_PATH,
            v.records,
            v.last_seq,
            match v.first_bad {
                None => String::from("ok"),
                Some(s) => alloc::format!("BROKEN at seq {s}"),
            },
            if rechained {
                "new records re-chained onto its end"
            } else {
                "continuing it"
            }
        );
    }
    if let Some(bad) = v.first_bad {
        record_as(
            0,
            "[kernel]",
            "audit.tamper",
            &alloc::format!(
                "file={LOG_PATH} first_bad_seq={bad} verified_records={}",
                v.records
            ),
        );
    }
    Some(summary)
}

/// Write every unpersisted record to the file.
fn flush() {
    let _f = FLUSH.lock();
    let (file, lines) = {
        let mut l = LOG.lock();
        let Some(f) = l.file.clone() else { return };
        let mut out = String::new();
        let mut n = 0;
        for r in l.ring.iter_mut().filter(|r| !r.persisted) {
            out.push_str(&r.line());
            out.push('\n');
            r.persisted = true;
            n += 1;
        }
        if n == 0 {
            return;
        }
        PERSISTED.fetch_add(n, Ordering::Relaxed);
        (f, out)
    };
    if file.write(lines.as_bytes()).is_ok() {
        let _ = crate::fs::milkfs::sync();
    } else {
        kprintln!("[audit] write to {LOG_PATH} failed");
    }
}

fn persister() {
    loop {
        if REATTACH.swap(false, Ordering::AcqRel) {
            // The volume was rewound (already detached): note it, then attach again below,
            // which re-appends whatever the rewound file no longer has.
            record_as(0, "[kernel]", "audit.reattach", "reason=volume_rewound");
        }
        if LOG.lock().file.is_none()
            && volume_mounted()
            && let Some(s) = attach()
        {
            kprintln!("[audit] attached: {s}");
        }
        flush();
        WQ.wait_until_timeout(|| KICK.swap(false, Ordering::AcqRel), 1_000_000_000);
    }
}

/// `/proc/audit`: the RAM ring, one record per line.
fn proc_text() -> String {
    let l = LOG.lock();
    let mut s = String::new();
    for r in &l.ring {
        s.push_str(&r.line());
        s.push('\n');
    }
    s
}

fn proc_status() -> String {
    let l = LOG.lock();
    alloc::format!(
        "records: {} total, {} in RAM (ring {}), {} persisted, {} lost before persisting\n\
         file: {}\nhead: seq {} hash {}\n",
        l.total,
        l.ring.len(),
        RING,
        PERSISTED.load(Ordering::Relaxed),
        l.dropped_unpersisted,
        if l.file.is_some() {
            LOG_PATH
        } else {
            "(not attached: no milkfs volume)"
        },
        l.next_seq - 1,
        hex(&l.last)
    )
}

/// `/dev/audit` (write-only): lets privileged user tools (the `apps` package manager)
/// add records without writing the log. Each line written is `<event> [detail...]`; the
/// kernel records it as event `user.<event>` with the writer's pid and exe, so user records
/// can neither pose as kernel events nor touch existing ones. `event`: 1-32 of
/// `[a-z0-9._-]`. Anyone may write (all processes are uid 0 today); the `user.` prefix and
/// the exe column say who it came from.
struct AuditDev {
    ino: u64,
    dev: u64,
}

fn valid_event(e: &str) -> bool {
    (1..=32).contains(&e.len())
        && e.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
}

impl crate::fs::node::Inode for AuditDev {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> crate::fs::node::InodeType {
        crate::fs::node::InodeType::CharDev
    }
    fn stat(&self) -> crate::fs::node::Stat {
        let mut st = crate::fs::node::Stat::new(
            self.dev,
            self.ino,
            crate::fs::node::InodeType::CharDev,
            0o222,
        );
        st.rdev = crate::fs::node::makedev(10, 0xad);
        st
    }
    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> crate::fs::errno::Result<usize> {
        Err(crate::fs::errno::Errno::EINVAL) // read /proc/audit instead
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> crate::fs::errno::Result<usize> {
        let text = core::str::from_utf8(buf).map_err(|_| crate::fs::errno::Errno::EINVAL)?;
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() > 16 {
            return Err(crate::fs::errno::Errno::EINVAL);
        }
        let split = |l: &str| -> (String, String) {
            let l = l.trim();
            let (e, d) = l.split_once(' ').unwrap_or((l, ""));
            (String::from(e), String::from(d.trim()))
        };
        if lines.iter().any(|l| !valid_event(&split(l).0)) {
            return Err(crate::fs::errno::Errno::EINVAL);
        }
        for l in lines {
            let (event, detail) = split(l);
            record(&alloc::format!("user.{event}"), &detail);
        }
        Ok(buf.len())
    }
    fn truncate(&self, _len: u64) -> crate::fs::errno::Result<()> {
        Ok(())
    }
}

/// Boot: SHA-256 self-test, the first record, /proc/audit, the persister thread.
/// After `sched::init` and `fs::init` (records made before this are kept and chained).
pub fn init() {
    let (ok, n) = sha256::selftest();
    kprintln!("[audit] sha256 self-test: {ok}/{n} FIPS 180-4 vectors ok");
    record_as(
        0,
        "[kernel]",
        "audit.boot",
        // The package version follows <repo>/VERSION (scripts/version-check.sh checks it).
        concat!(
            "kernel=milkOS-",
            env!("CARGO_PKG_VERSION"),
            " build=",
            env!("MILKOS_BUILD")
        ),
    );
    if let Some(root) = crate::fs::procfs::root() {
        root.add_file("audit", proc_text);
        root.add_file("audit_status", proc_status);
    }
    let dev = crate::fs::devfs::ids().map_or(0, |ids| ids.dev);
    let node = Arc::new(AuditDev { ino: 0xad17, dev });
    if let Err(e) = crate::fs::devfs::add_device("audit", node) {
        kprintln!("[audit] /dev/audit: {e:?}");
    }
    THREAD.store(true, Ordering::Release);
    drop(sched::spawn_prio(
        "audit",
        sched::Priority::Normal,
        persister,
    ));
}
