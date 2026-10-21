//! System statistics for monitors (`top`, `uptime`), kept apart from the subsystems they
//! measure:
//!
//! - `/proc/loadavg`: Linux format, `1min 5min 15min running/total last_pid`. A kernel
//!   thread samples the number of runnable threads (Ready or Running, idle threads and
//!   itself excluded) every 5 s and folds it into three exponential moving averages, in
//!   Linux's fixed point (FSHIFT 11; e^-5/60, e^-5/300, e^-5/900).
//! - `/proc/diskstats`: Linux's first 14 columns for every block device whose driver
//!   counts I/O (`BlockDevice::io_stats`; virtio-blk does): major minor name reads 0
//!   sectors_read 0 writes 0 sectors_written 0 0 0 0. Times are not tracked (0).

use alloc::string::String;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::{fs, sched};

const FSHIFT: u64 = 11;
const FIXED_1: u64 = 1 << FSHIFT;
const EXP: [u64; 3] = [1884, 2014, 2037];
const SAMPLE_MS: u64 = 5000;

static AVG: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

fn runnable() -> (u64, u64) {
    let t = sched::threads();
    let me = sched::current_tid();
    let n = t
        .iter()
        .filter(|x| {
            x.tid != me
                && x.prio != sched::Priority::Idle
                && matches!(x.state, sched::State::Ready | sched::State::Running)
        })
        .count() as u64;
    let total = t
        .iter()
        .filter(|x| x.prio != sched::Priority::Idle && x.state != sched::State::Dead)
        .count() as u64;
    (n, total)
}

fn sampler() {
    loop {
        sched::sleep_ms(SAMPLE_MS);
        let (n, _) = runnable();
        for (a, e) in AVG.iter().zip(EXP) {
            let old = a.load(Ordering::Relaxed);
            let new = (old * e + n * FIXED_1 * (FIXED_1 - e)) >> FSHIFT;
            a.store(new, Ordering::Relaxed);
        }
    }
}

fn fixed(v: u64) -> String {
    let v = v + FIXED_1 / 200; // round to 2 decimals
    alloc::format!(
        "{}.{:02}",
        v >> FSHIFT,
        ((v & (FIXED_1 - 1)) * 100) >> FSHIFT
    )
}

fn loadavg() -> String {
    let (n, total) = runnable();
    let a: [u64; 3] = core::array::from_fn(|i| AVG[i].load(Ordering::Relaxed));
    alloc::format!(
        "{} {} {} {}/{} {}\n",
        fixed(a[0]),
        fixed(a[1]),
        fixed(a[2]),
        n + 1, // + the reader itself, as Linux counts it
        total,
        crate::proc::forks()
    )
}

fn diskstats() -> String {
    let mut s = String::new();
    for (minor, d) in fs::blockdev::devices().iter().enumerate() {
        if let Some(io) = d.io_stats() {
            let _ = writeln!(
                s,
                "{:>4} {:>7} {} {} 0 {} 0 {} 0 {} 0 0 0 0",
                254,
                minor * 16,
                d.name(),
                io.reads,
                io.sectors_read,
                io.writes,
                io.sectors_written
            );
        }
    }
    s
}

/// Add /proc/loadavg and /proc/diskstats and start the sampler. After sched and fs init.
pub fn init() {
    if let Some(root) = fs::procfs::root() {
        root.add_file("loadavg", loadavg);
        root.add_file("diskstats", diskstats);
    }
    drop(sched::spawn("loadavg", sampler));
}
