//! Wall-clock time for file timestamps.
//!
//! qemu `virt` has a PL031 RTC at 0x0901_0000 whose data register is Unix seconds. We read
//! it once (in [`init`]) and add the monotonic uptime after that. Without an RTC (reads 0)
//! timestamps are seconds since boot. If a real time subsystem shows up, point [`now`] at it.
//!
//! Test-only time warp: with the fw_cfg file `opt/milkos/test-timewarp` present (tests pass
//! `-fw_cfg name=opt/milkos/test-timewarp,string=on`), `/System/Kernel/kernel/timewarp` exists,
//! and writing `+N` to it moves the wall clock N seconds ahead (it only ever goes forward), so a
//! test sees an alarm a minute away ring without waiting for it (scripts/alarm-test.sh). The
//! monotonic clock doesn't move. Without the fw_cfg file there is no such file and no warp.

use alloc::sync::Arc;
use core::ptr::read_volatile;
use core::sync::atomic::{AtomicU64, Ordering};

use super::errno::{Errno, Result};
use super::node::Timespec;
use super::synth::SynthDir;
use crate::{kprintln, timer};

const PL031_DR: usize = crate::mmu::phys_to_virt(0x0901_0000);
/// PrimeCell peripheral ID registers at 0xfe0..0xfec; PL031 reads 0x31, 0x10, x4, 0x00.
const PL031_PERIPH_ID0: usize = PL031_DR + 0xfe0;

/// Unix seconds at `timer::now_ns() == 0`.
static BOOT_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    if crate::platform::PHONE {
        return; // no PL031 on the phone: that address is a register of the SoC
    }
    // SAFETY: device memory, identity-mapped (0..1 GiB is Device-nGnRE).
    let id0 = unsafe { read_volatile(PL031_PERIPH_ID0 as *const u32) } & 0xff;
    if id0 != 0x31 {
        return;
    }
    let rtc = unsafe { read_volatile(PL031_DR as *const u32) } as u64;
    BOOT_EPOCH.store(
        rtc.saturating_sub(timer::now_ns() / 1_000_000_000),
        Ordering::Relaxed,
    );
}

/// Unix seconds when the kernel started (0 if there is no RTC).
pub fn boot_epoch() -> u64 {
    BOOT_EPOCH.load(Ordering::Relaxed)
}

/// Seconds the test time warp has moved the wall clock ahead (0 unless a test asked).
static WARP_S: AtomicU64 = AtomicU64::new(0);

/// `/System/Kernel/kernel/timewarp`, only in a boot with fw_cfg `opt/milkos/test-timewarp`.
pub fn sysfs(dir: &Arc<SynthDir>) {
    if !crate::fwcfg::available() || crate::fwcfg::find_file("opt/milkos/test-timewarp").is_none() {
        return;
    }
    kprintln!("[clock] test time warp on (opt/milkos/test-timewarp): /System/Kernel/kernel/timewarp");
    dir.add_control(
        "timewarp",
        || alloc::format!("{}\n", WARP_S.load(Ordering::Relaxed)),
        warp,
    );
}

/// A write of `+N` (or `N`): N more seconds ahead, at most a day per write.
fn warp(b: &[u8]) -> Result<()> {
    let t = core::str::from_utf8(b).map_err(|_| Errno::EINVAL)?.trim();
    let n: u64 = t
        .strip_prefix('+')
        .unwrap_or(t)
        .parse()
        .map_err(|_| Errno::EINVAL)?;
    if n > 86_400 {
        return Err(Errno::EINVAL);
    }
    let total = WARP_S.fetch_add(n, Ordering::Relaxed) + n;
    kprintln!("[clock] test time warp: +{n} s (the wall clock is {total} s ahead now)");
    Ok(())
}

pub fn now() -> Timespec {
    let ns = timer::now_ns();
    Timespec {
        sec: (boot_epoch() + WARP_S.load(Ordering::Relaxed) + ns / 1_000_000_000) as i64,
        nsec: (ns % 1_000_000_000) as i64,
    }
}
