//! The system journal: one structured log for the kernel and every service.
//!
//! * **Entries**: sequence number, boot id, monotonic time, wall time, pid, uid, unit,
//!   level, message. Kept in a fixed in-memory ring (no allocation to add one, so the
//!   kernel's own lines can go in from anywhere `kprintln!` works).
//! * **Writers**: every `kprintln!` line, as unit `kernel`; user space through
//!   `/dev/journal` (one entry per line written): `UNIT\tLEVEL\tPID\tMESSAGE` from root
//!   (milkd forwards each service's stdout / stderr this way, with the service's pid), or
//!   just `MESSAGE` / `LEVEL\tMESSAGE` from anyone else, filed under unit `user/<name>`
//!   with the writer's own pid.
//! * **Readers**: `/dev/journal` read gives the retained entries as text lines,
//!   `seq\tboot\tmono_ms\twall_s\tpid\tuid\tunit\tlevel\tmessage`, and the file offset is a
//!   cursor into that stream, so reading on at EOF later gets only what is new (`log -f`).
//!   Readable by root and group admin (mode 0662 root:admin; anyone may write).
//! * **Persistence**: the `journald` kernel thread appends new entries (same line format)
//!   to `/Volumes/Milk/var/log/journal/current.log` every 2 s, rotating it to
//!   `journal.1` .. `journal.3` at 512 KiB, so at most 2 MiB of history survives reboots.
//!   The `log` tool reads those files and the live ring.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::fs::node::{Inode, InodeType, Stat};
use crate::fs::{self, Errno};
use crate::sync::SpinLock;

pub const SLOTS: usize = 4096;
const UNIT_MAX: usize = 32;
const MSG_MAX: usize = 216;
pub const DIR: &str = "/Volumes/Milk/var/log/journal";
const CURRENT: &str = "/Volumes/Milk/var/log/journal/current.log";
const ROTATE_AT: u64 = 512 * 1024;
const KEEP: usize = 3;
const ADMIN_GID: u32 = 80;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Err = 3,
    Warn = 4,
    Info = 6,
    Debug = 7,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::Err => "err",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
    pub fn parse(s: &str) -> Option<Level> {
        Some(match s {
            "err" | "error" | "3" => Level::Err,
            "warn" | "warning" | "4" => Level::Warn,
            "info" | "6" => Level::Info,
            "debug" | "7" => Level::Debug,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
struct Entry {
    seq: u64,
    mono_ns: u64,
    wall_s: u64,
    pid: u32,
    uid: u32,
    level: Level,
    unit: [u8; UNIT_MAX],
    unit_len: u8,
    msg: [u8; MSG_MAX],
    msg_len: u8,
    /// Byte offset of this entry's line in the `/dev/journal` text stream.
    off: u64,
}

const EMPTY: Entry = Entry {
    seq: 0,
    mono_ns: 0,
    wall_s: 0,
    pid: 0,
    uid: 0,
    level: Level::Info,
    unit: [0; UNIT_MAX],
    unit_len: 0,
    msg: [0; MSG_MAX],
    msg_len: 0,
    off: 0,
};

struct Ring {
    slots: [Entry; SLOTS],
    /// Next sequence number (entries 1.. ; slot = seq % SLOTS).
    next: u64,
    /// Length of the text stream so far.
    stream: u64,
}

static RING: SpinLock<Ring> = SpinLock::new(Ring {
    slots: [EMPTY; SLOTS],
    next: 1,
    stream: 0,
});
static BOOT: AtomicU64 = AtomicU64::new(0);
static READY: AtomicBool = AtomicBool::new(false);

fn clip(dst: &mut [u8], s: &[u8]) -> u8 {
    let mut n = 0;
    for &b in s {
        if n == dst.len() {
            break;
        }
        dst[n] = match b {
            b'\t' | b'\n' | b'\r' => b' ',
            c if c < 0x20 => b'?',
            c => c,
        };
        n += 1;
    }
    // Don't cut a UTF-8 sequence in half (the text stream's length depends on it).
    while n > 0 && core::str::from_utf8(&dst[..n]).is_err() {
        n -= 1;
    }
    n as u8
}

/// Decimal digits of `v` (for the text-stream length bookkeeping, no allocation).
fn digits(mut v: u64) -> u64 {
    let mut n = 1;
    while v >= 10 {
        v /= 10;
        n += 1;
    }
    n
}

fn line_len(e: &Entry) -> u64 {
    // seq boot mono_ms wall_s pid uid unit level msg, 8 tabs + newline
    digits(e.seq)
        + 16
        + digits(e.mono_ns / 1_000_000)
        + digits(e.wall_s)
        + digits(e.pid as u64)
        + digits(e.uid as u64)
        + e.unit_len as u64
        + e.level.name().len() as u64
        + e.msg_len as u64
        + 9
}

/// Add one entry. Never allocates or blocks (IRQ-safe); drops it before `init`.
pub fn add(unit: &str, level: Level, pid: u32, uid: u32, msg: &str) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let mut e = EMPTY;
    e.unit_len = clip(&mut e.unit, unit.as_bytes());
    e.msg_len = clip(&mut e.msg, msg.trim_end().as_bytes());
    e.level = level;
    e.pid = pid;
    e.uid = uid;
    e.mono_ns = crate::timer::now_ns();
    e.wall_s = crate::fs::clock::now().sec.max(0) as u64;
    let mut r = RING.lock();
    e.seq = r.next;
    e.off = r.stream;
    r.stream += line_len(&e);
    let slot = (e.seq as usize) % SLOTS;
    r.slots[slot] = e;
    r.next += 1;
}

// ---- kprintln -> journal ------------------------------------------------------------------

/// The `kprintln!` line being assembled (under the print lock, see sink.rs).
struct KLine {
    buf: core::cell::UnsafeCell<[u8; MSG_MAX]>,
    len: AtomicUsize,
}
// SAFETY: only touched under sink's print lock.
unsafe impl Sync for KLine {}
static KLINE: KLine = KLine {
    buf: core::cell::UnsafeCell::new([0; MSG_MAX]),
    len: AtomicUsize::new(0),
};

/// `kprintln!`'s writer: the console (`sink::Sink`) and the journal.
pub struct Tee;

impl Write for Tee {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let _ = crate::sink::Sink.write_str(s);
        kernel_text(s);
        Ok(())
    }
}

/// Part of a `kprintln!` line (under sink's print lock).
pub fn kernel_text(s: &str) {
    if !READY.load(Ordering::Relaxed) {
        return;
    }
    let mut n = KLINE.len.load(Ordering::Relaxed);
    // SAFETY: under the print lock.
    let buf = unsafe { &mut *KLINE.buf.get() };
    for &b in s.as_bytes() {
        if b == b'\n' {
            if n > 0 {
                let line = core::str::from_utf8(&buf[..n]).unwrap_or("?");
                add("kernel", kernel_level(line), 0, 0, line);
            }
            n = 0;
        } else if n < MSG_MAX {
            buf[n] = b;
            n += 1;
        }
    }
    KLINE.len.store(n, Ordering::Relaxed);
}

fn kernel_level(line: &str) -> Level {
    if line.contains("PANIC") || line.contains("EXCEPTION") || line.contains("FAIL") {
        Level::Err
    } else if line.contains("warning") {
        Level::Warn
    } else {
        Level::Info
    }
}

// ---- reading ------------------------------------------------------------------------------

fn format(e: &Entry, out: &mut String) {
    let unit = core::str::from_utf8(&e.unit[..e.unit_len as usize]).unwrap_or("?");
    let msg = core::str::from_utf8(&e.msg[..e.msg_len as usize]).unwrap_or("?");
    let _ = writeln!(
        out,
        "{}\t{:016x}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        e.seq,
        BOOT.load(Ordering::Relaxed),
        e.mono_ns / 1_000_000,
        e.wall_s,
        e.pid,
        e.uid,
        unit,
        e.level.name(),
        msg
    );
}

/// Text of the retained entries from stream offset `off` on, at most about `max` bytes
/// (whole lines; the first may start mid-line if `off` points into one).
fn read_from(off: u64, max: usize) -> Vec<u8> {
    let entries: Vec<Entry> = {
        let r = RING.lock();
        let oldest = r.next.saturating_sub(SLOTS as u64).max(1);
        (oldest..r.next)
            .map(|s| r.slots[(s as usize) % SLOTS])
            .filter(|e| e.off + line_len(e) > off)
            .take(max / 64 + 1)
            .collect()
    };
    let mut s = String::new();
    let mut skip = 0;
    for (k, e) in entries.iter().enumerate() {
        if k == 0 && e.off < off {
            skip = (off - e.off) as usize;
        }
        format(e, &mut s);
        if s.len() >= max + skip {
            break;
        }
    }
    let b = s.into_bytes();
    b.get(skip..).map_or(Vec::new(), |v| v.to_vec())
}

struct JournalDev {
    dev: u64,
}

impl Inode for JournalDev {
    fn ino(&self) -> u64 {
        0x6a72
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(self.dev, 0x6a72, InodeType::CharDev, 0o662);
        st.gid = ADMIN_GID;
        st.rdev = crate::fs::node::makedev(10, 0x6a);
        st
    }
    fn read_at(&self, off: u64, buf: &mut [u8]) -> fs::Result<usize> {
        let text = read_from(off, buf.len());
        let n = text.len().min(buf.len());
        buf[..n].copy_from_slice(&text[..n]);
        Ok(n)
    }
    fn write_at(&self, _off: u64, data: &[u8]) -> fs::Result<usize> {
        let text = core::str::from_utf8(data).map_err(|_| Errno::EINVAL)?;
        let pid = crate::sched::current_pid();
        let c = crate::proc::current_cred();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let f: Vec<&str> = line.splitn(4, '\t').collect();
            if c.is_root() && f.len() == 4 {
                // UNIT \t LEVEL \t PID \t MESSAGE (milkd, for its services)
                let level = Level::parse(f[1]).unwrap_or(Level::Info);
                let p = f[2].parse().unwrap_or(pid);
                add(f[0], level, p, c.euid, f[3]);
            } else {
                let (level, msg) = match line.split_once('\t') {
                    Some((l, m)) if Level::parse(l).is_some() => (Level::parse(l).unwrap(), m),
                    _ => (Level::Info, line),
                };
                let comm = crate::proc::get(pid)
                    .map(|p| String::from(p.inner.lock().name.as_str()))
                    .unwrap_or_default();
                add(&alloc::format!("user/{comm}"), level, pid, c.euid, msg);
            }
        }
        Ok(data.len())
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        Ok(())
    }
}

// ---- persistence --------------------------------------------------------------------------

/// Boot: start taking entries, publish /dev/journal, start the persister.
pub fn init() {
    let id = crate::crypto::rng::next_u64() ^ crate::timer::now_ns();
    BOOT.store(id, Ordering::Relaxed);
    READY.store(true, Ordering::Release);
    add("kernel", Level::Info, 0, 0, "journal started");
    let dev = crate::fs::devfs::ids().map_or(0, |ids| ids.dev);
    if let Err(e) =
        crate::fs::devfs::add_device("journal", alloc::sync::Arc::new(JournalDev { dev }))
    {
        crate::kprintln!("[journal] /dev/journal: {e:?}");
    }
    drop(crate::sched::spawn_prio(
        "journald",
        crate::sched::Priority::Low,
        persister,
    ));
}

/// Entries after `seq` as text (for the persister).
fn since(seq: u64) -> (u64, String) {
    let entries: Vec<Entry> = {
        let r = RING.lock();
        let oldest = r.next.saturating_sub(SLOTS as u64).max(1);
        (oldest.max(seq + 1)..r.next)
            .map(|s| r.slots[(s as usize) % SLOTS])
            .collect()
    };
    let mut s = String::new();
    let last = entries.last().map_or(seq, |e| e.seq);
    for e in &entries {
        format(e, &mut s);
    }
    (last, s)
}

static FLUSHED: AtomicU64 = AtomicU64::new(0);

fn persister() {
    loop {
        crate::sched::sleep_ms(2000);
        let _ = flush();
    }
}

/// Append what's new to the on-disk journal (if the milkfs disk is there); rotate.
pub fn flush() -> fs::Result<()> {
    if fs::stat("/Volumes/Milk").is_err() {
        return Ok(());
    }
    let (last, text) = since(FLUSHED.load(Ordering::Relaxed));
    if text.is_empty() {
        return Ok(());
    }
    if fs::stat(DIR).is_err() {
        fs::mkdir_all(DIR, 0o750)?;
        let _ = fs::setattr_at(
            None,
            DIR,
            true,
            &fs::node::SetAttr {
                gid: Some(ADMIN_GID),
                ..Default::default()
            },
        );
    }
    let f = fs::open(
        CURRENT,
        fs::abi::O_WRONLY | fs::abi::O_CREAT | fs::abi::O_APPEND,
        0o640,
    )?;
    let size = f.stat().size;
    if size == 0 {
        let _ = fs::setattr_at(
            None,
            CURRENT,
            true,
            &fs::node::SetAttr {
                gid: Some(ADMIN_GID),
                ..Default::default()
            },
        );
    }
    f.write(text.as_bytes())?;
    drop(f);
    FLUSHED.store(last, Ordering::Relaxed);
    if size + text.len() as u64 > ROTATE_AT {
        rotate();
    }
    Ok(())
}

fn rotate() {
    let name = |k: usize| alloc::format!("{DIR}/journal.{k}");
    let _ = fs::unlink(&name(KEEP));
    for k in (1..KEEP).rev() {
        let _ = fs::rename(&name(k), &name(k + 1));
    }
    let _ = fs::rename(CURRENT, &name(1));
}
