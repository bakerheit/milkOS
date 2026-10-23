//! "Time Machine" in the kernel: the `timemachine` thread that snapshots the primary
//! volume on its schedule, and `/System/Kernel/fs/milkfs/auto`. The policy and the snapshotting are
//! in the milkfs crate (`milkfs::timemachine`, re-exported here).

use alloc::format;
use alloc::string::String;
use core::fmt::Write;
use core::sync::atomic::{AtomicU64, Ordering};

pub use ::milkfs::timemachine::*;

use super::snap::SnapInfo;
use crate::fs::errno::{Errno, Result};
use crate::kprintln;

/// Unix seconds of the last automatic snapshot attempt (0: none yet).
static LAST_RUN: AtomicU64 = AtomicU64::new(0);

fn now_s() -> u64 {
    crate::fs::clock::now().sec.max(0) as u64
}

/// One automatic snapshot of the primary volume, logged.
pub fn run_once(why: &str) -> Result<Option<SnapInfo>> {
    let v = super::volume().ok_or(Errno::ENODEV)?;
    let now = now_s();
    LAST_RUN.store(now, Ordering::Relaxed);
    let r = v.state.lock().auto_snapshot(now);
    match &r {
        Ok(Some(s)) => kprintln!(
            "timemachine: {why}: snapshot {} (generation {})",
            s.name,
            s.generation
        ),
        Ok(None) => kprintln!("timemachine: {why}: nothing changed, no snapshot"),
        Err(e) => kprintln!("timemachine: {why}: snapshot failed: {e}"),
    }
    r
}

/// The `timemachine` kernel thread. Wakes every second to look at the clock and settings.
fn thread() {
    let _ = run_once("boot");
    loop {
        crate::sched::sleep_ms(1000);
        let Some(v) = super::volume() else { continue };
        // No volume at boot (an encrypted home still locked, a disk formatted later): the
        // interval starts when one shows up, instead of an "every 15m" snapshot right away.
        if LAST_RUN.load(Ordering::Relaxed) == 0 {
            LAST_RUN.store(now_s(), Ordering::Relaxed);
            continue;
        }
        let cfg = v.state.lock().auto_config();
        if cfg.enabled && now_s() >= LAST_RUN.load(Ordering::Relaxed) + cfg.interval_s {
            let _ = run_once(&format!("every {}", interval_text(cfg.interval_s)));
        }
    }
}

pub fn start() {
    let enabled = super::volume().is_some_and(|v| v.state.lock().auto_config().enabled);
    drop(crate::sched::spawn("timemachine", thread));
    kprintln!(
        "timemachine: started ({})",
        if enabled { "on" } else { "off" }
    );
}

/// `/System/Kernel/fs/milkfs/auto`, read side.
pub fn status() -> String {
    let Some(v) = super::volume() else {
        return String::from("no milkfs volume\n");
    };
    let st = v.state.lock();
    let c = st.auto_config();
    let a = autos(&st);
    drop(st);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{} interval={} keep={} daily={}",
        if c.enabled { "on" } else { "off" },
        interval_text(c.interval_s),
        c.keep,
        c.daily
    );
    let last = LAST_RUN.load(Ordering::Relaxed);
    if c.enabled && last != 0 {
        let next = (last + c.interval_s).saturating_sub(now_s());
        let _ = writeln!(s, "next in {}", interval_text(next.max(1)));
    }
    let _ = writeln!(s, "{} automatic snapshot(s)", a.len());
    s
}

/// `/System/Kernel/fs/milkfs/auto`, write side.
pub fn control(cmd: &str) -> Result<()> {
    let v = super::volume().ok_or(Errno::ENODEV)?;
    let mut c = v.state.lock().auto_config();
    let mut now = false;
    for w in cmd.split_whitespace() {
        match w.split_once('=') {
            None if w == "on" => c.enabled = true,
            None if w == "off" => c.enabled = false,
            None if w == "now" => now = true,
            Some(("interval", x)) => c.interval_s = parse_interval(x).ok_or(Errno::EINVAL)?,
            Some(("keep", x)) => c.keep = x.parse().map_err(|_| Errno::EINVAL)?,
            Some(("daily", x)) => c.daily = x.parse().map_err(|_| Errno::EINVAL)?,
            _ => return Err(Errno::EINVAL),
        }
    }
    v.state.lock().set_auto_config(c)?;
    // Restart the interval from now, so a shorter one applies right away.
    LAST_RUN.store(now_s(), Ordering::Relaxed);
    if now {
        run_once("requested")?;
    }
    Ok(())
}
