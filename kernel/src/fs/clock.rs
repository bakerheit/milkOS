//! Wall-clock time for file timestamps.
//!
//! qemu `virt` has a PL031 RTC at 0x0901_0000 whose data register is Unix seconds. We read
//! it once (in [`init`]) and add the monotonic uptime after that. Without an RTC (reads 0)
//! timestamps are seconds since boot. If a real time subsystem shows up, point [`now`] at it.

use core::ptr::read_volatile;
use core::sync::atomic::{AtomicU64, Ordering};

use super::node::Timespec;
use crate::timer;

const PL031_DR: usize = 0x0901_0000;
/// PrimeCell peripheral ID registers at 0xfe0..0xfec; PL031 reads 0x31, 0x10, x4, 0x00.
const PL031_PERIPH_ID0: usize = PL031_DR + 0xfe0;

/// Unix seconds at `timer::now_ns() == 0`.
static BOOT_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn init() {
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

pub fn now() -> Timespec {
    let ns = timer::now_ns();
    Timespec {
        sec: (boot_epoch() + ns / 1_000_000_000) as i64,
        nsec: (ns % 1_000_000_000) as i64,
    }
}
