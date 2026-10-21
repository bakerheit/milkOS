//! Crash testing for milkfs, driven by `scripts/crashtest.sh`. Selected with a qemu
//! fw_cfg string, so a normal boot never runs any of it:
//!
//! ```text
//! -fw_cfg name=opt/milkos/test,string=crash-writer   write forever (the script SIGKILLs qemu)
//! -fw_cfg name=opt/milkos/test,string=powercut=N      same workload, but the disk stops after
//!                                                     N more sectors: the request in flight is
//!                                                     torn, then the machine halts
//! -fw_cfg name=opt/milkos/test,string=powercut-sb=K:T  cut during the K-th superblock write,
//!                                                     after T of its 8 sectors (T = 8: it
//!                                                     landed, but the commit never returned)
//! -fw_cfg name=opt/milkos/test,string=crash-check     verify, print CRASHCHECK PASS/FAIL
//! ```
//!
//! The workload writes self-describing files (`crash/f-K`, via `crash/.tmp-K` + rename),
//! and takes, deletes and rolls back snapshots. The check proves the volume is exactly some
//! committed generation: it mounts (superblock, checksums, namespace check), every file
//! in the live tree and in every snapshot is complete and matches its own header (old or
//! new, never a mix), the generation is the last committed one or the one in flight, and
//! space accounting stays exact after deleting the snapshots (dead lists intact).

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::fs::blockdev::{BlockDevice, BlockRef};
use crate::fs::errno::Result;
use crate::fs::node::InodeType;
use crate::fs::ops;
use crate::sync::SpinLock;
use crate::{fwcfg, kprintln};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Writer,
    PowerCut(u64),
    /// (superblock write number, sectors of it that land)
    PowerCutSb(u64, u64),
    Check,
}

static MODE: SpinLock<Option<Mode>> = SpinLock::new(None);
/// Last generation whose commit completed (for the power-cut report).
pub static LAST_COMMIT: AtomicU64 = AtomicU64::new(0);

/// Read the requested mode from fw_cfg. Call before `automount`.
pub fn configure() {
    let Some(f) = fwcfg::find_file("opt/milkos/test") else {
        return;
    };
    let mut buf = alloc::vec![0u8; f.size as usize];
    if !fwcfg::read_file(f, &mut buf) {
        return;
    }
    let s = String::from_utf8_lossy(&buf);
    let s = s.trim_end_matches('\0').trim();
    let mode = match s {
        "crash-writer" => Some(Mode::Writer),
        "crash-check" => Some(Mode::Check),
        _ => {
            if let Some(n) = s.strip_prefix("powercut=") {
                n.parse().ok().map(Mode::PowerCut)
            } else if let Some(kt) = s.strip_prefix("powercut-sb=") {
                kt.split_once(':')
                    .and_then(|(k, t)| Some(Mode::PowerCutSb(k.parse().ok()?, t.parse().ok()?)))
            } else {
                None
            }
        }
    };
    kprintln!("milkfs crashtest: mode {mode:?} ({s:?})");
    *MODE.lock() = mode;
}

pub fn mode() -> Option<Mode> {
    *MODE.lock()
}

// ---- power cut --------------------------------------------------------------------

/// Passes I/O through until armed; then lets `budget` more sectors reach the disk, tears
/// the request that crosses the budget, and halts the machine.
struct FaultyDev {
    inner: BlockRef,
    armed: AtomicBool,
    budget: AtomicU64,
    written: AtomicU64,
    /// For `PowerCutSb`: superblock writes left before the cut, and sectors of it to keep.
    sb_left: AtomicU64,
    sb_keep: u64,
}

static FAULTY: SpinLock<Option<Arc<FaultyDev>>> = SpinLock::new(None);

fn power_off(written: u64, torn: u64) -> ! {
    kprintln!(
        "POWERCUT: disk stopped after {written} sectors ({torn} of the last request written), last committed generation {}",
        LAST_COMMIT.load(Ordering::Relaxed)
    );
    loop {
        crate::sync::irq_disable();
        // SAFETY: halt; nothing runs again.
        unsafe { core::arch::asm!("wfi") };
    }
}

impl BlockDevice for FaultyDev {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn block_size(&self) -> usize {
        self.inner.block_size()
    }
    fn num_blocks(&self) -> u64 {
        self.inner.num_blocks()
    }
    fn read_only(&self) -> bool {
        self.inner.read_only()
    }
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_blocks(lba, buf)
    }
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()> {
        if !self.armed.load(Ordering::Relaxed) {
            return self.inner.write_blocks(lba, buf);
        }
        let bs = self.block_size();
        let n = (buf.len() / bs) as u64;
        let sb_sectors = (2 * super::format::BLOCK / bs) as u64;
        if lba < sb_sectors
            && self.sb_left.load(Ordering::Relaxed) > 0
            && self.sb_left.fetch_sub(1, Ordering::Relaxed) == 1
        {
            let keep = self.sb_keep.min(n);
            if keep > 0 {
                let _ = self.inner.write_blocks(lba, &buf[..keep as usize * bs]);
            }
            let _ = self.inner.flush();
            power_off(self.written.load(Ordering::Relaxed) + keep, keep);
        }
        let left = self.budget.load(Ordering::Relaxed);
        if n <= left {
            self.budget.store(left - n, Ordering::Relaxed);
            self.written.fetch_add(n, Ordering::Relaxed);
            return self.inner.write_blocks(lba, buf);
        }
        // Tear: only the first `left` sectors make it.
        if left > 0 {
            let _ = self.inner.write_blocks(lba, &buf[..left as usize * bs]);
        }
        let _ = self.inner.flush();
        power_off(self.written.load(Ordering::Relaxed) + left, left)
    }
    fn flush(&self) -> Result<()> {
        self.inner.flush()
    }
}

/// The device milkfs should mount: wrapped for a power-cut run.
pub fn wrap(dev: BlockRef) -> BlockRef {
    let (budget, sb_left, sb_keep) = match mode() {
        Some(Mode::PowerCut(n)) => (n, 0, 0),
        Some(Mode::PowerCutSb(k, t)) => (u64::MAX, k.max(1), t),
        _ => return dev,
    };
    {
        {
            let f = Arc::new(FaultyDev {
                inner: dev,
                armed: AtomicBool::new(false),
                budget: AtomicU64::new(budget),
                written: AtomicU64::new(0),
                sb_left: AtomicU64::new(sb_left),
                sb_keep,
            });
            *FAULTY.lock() = Some(f.clone());
            f
        }
    }
}

// ---- workload ---------------------------------------------------------------------

const DIR: &str = "/Volumes/Milk/crash";
const SLOTS: u64 = 6;

/// File contents for iteration `i`: a header line, then a body derived from `i`.
fn content(i: u64) -> Vec<u8> {
    let len = 600 + (i.wrapping_mul(7919) % 120_000) as usize;
    let mut v = format!("milkcrash i={i} len={len}\n").into_bytes();
    let mut j = v.len();
    while j < len {
        v.push((i.wrapping_mul(31) as usize ^ j.wrapping_mul(7) ^ (j >> 8)) as u8);
        j += 1;
    }
    v.truncate(len);
    v
}

/// Does `data` match its own header? Empty is reported as `None`.
fn verify(data: &[u8]) -> Option<bool> {
    if data.is_empty() {
        return None;
    }
    let nl = data.iter().position(|&b| b == b'\n')?;
    let head = core::str::from_utf8(&data[..nl]).ok()?;
    let mut it = head.split(' ');
    if it.next() != Some("milkcrash") {
        return Some(false);
    }
    let i: u64 = it.next()?.strip_prefix("i=")?.parse().ok()?;
    Some(content(i) == data)
}

fn snap_names() -> Vec<String> {
    super::snapshots()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.name)
        .filter(|n| n.starts_with("crash-"))
        .collect()
}

fn writer() -> ! {
    let _ = ops::mkdir_all(DIR, 0o755);
    let mut i = super::volume()
        .map(|v| v.state.lock().sb.generation)
        .unwrap_or(1)
        * 1000;
    if let Some(f) = FAULTY.lock().as_ref() {
        f.armed.store(true, Ordering::Relaxed);
    }
    kprintln!("crash-writer: started at iteration {i}");
    loop {
        let k = i % SLOTS;
        let tmp = format!("{DIR}/.tmp-{k}");
        let fin = format!("{DIR}/f-{k}");
        if let Err(e) = ops::write_file(&tmp, &content(i)).and_then(|_| ops::rename(&tmp, &fin)) {
            kprintln!("crash-writer: iteration {i}: {e}");
        }
        if i % 25 == 0 {
            let _ = super::snapshot(&format!("crash-{i}"));
            let names = snap_names();
            if names.len() > 3 {
                let _ = super::delete_snapshot(&names[0]);
            }
        }
        if i % 70 == 0
            && let Some(newest) = snap_names().last()
        {
            let _ = super::rollback(newest);
        }
        if i % 10 == 0 {
            kprintln!("crash-writer: iteration {i}");
        }
        i += 1;
    }
}

fn check_dir(path: &str, ok: &mut u32, bad: &mut u32, empty_tmp: &mut u32) {
    for e in ops::readdir(path).unwrap_or_default() {
        if e.kind != InodeType::File {
            continue;
        }
        let p = format!("{path}/{}", e.name);
        match ops::read_file(&p).map(|d| verify(&d)) {
            Ok(Some(true)) => *ok += 1,
            Ok(None) if e.name.starts_with(".tmp-") => *empty_tmp += 1,
            r => {
                *bad += 1;
                kprintln!("CRASHCHECK: {p} is bad ({r:?})");
            }
        }
    }
}

fn check() {
    let Some(v) = super::volume() else {
        kprintln!("CRASHCHECK FAIL: no milkfs volume mounted");
        return;
    };
    let generation = v.state.lock().sb.generation;
    let (mut ok, mut bad, mut empty) = (0, 0, 0);
    check_dir(DIR, &mut ok, &mut bad, &mut empty);
    let live = ok;
    for s in super::snapshots().unwrap_or_default() {
        check_dir(
            &format!("/Volumes/Milk/.snapshots/{}/crash", s.name),
            &mut ok,
            &mut bad,
            &mut empty,
        );
    }
    let nsnaps = super::snapshots().map(|l| l.len()).unwrap_or(0);
    // Dead lists must still be right: delete every snapshot, then the allocator must
    // match what is reachable exactly.
    for n in snap_names() {
        if let Err(e) = super::delete_snapshot(&n) {
            kprintln!("CRASHCHECK: delete snapshot {n}: {e}");
            bad += 1;
        }
    }
    let space = v.state.lock().verify_space();
    let space_ok = matches!(space, Ok(c) if c.leaked == 0 && c.free_but_used == 0);
    let pass = bad == 0 && space_ok;
    kprintln!(
        "CRASHCHECK {}: mounted generation {generation}, {live} live files ok, {} snapshot files ok ({nsnaps} snapshots), {empty} empty tmp, {bad} bad, space {:?}",
        if pass { "PASS" } else { "FAIL" },
        ok - live,
        space
    );
}

/// Run the requested mode instead of the normal milkfs self-test. Returns only for
/// `crash-check` (and when no mode is set).
pub fn run() -> bool {
    match mode() {
        None => false,
        Some(Mode::Check) => {
            check();
            true
        }
        Some(Mode::Writer | Mode::PowerCut(_) | Mode::PowerCutSb(..)) => writer(),
    }
}
