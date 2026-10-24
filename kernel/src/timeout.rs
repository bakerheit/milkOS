//! A deadline for polling hardware, in generic-counter ticks. Works before `timer::init` (it reads
//! CNTFRQ_EL0 itself) and with IRQs masked.

use core::arch::asm;

/// Counter frequency usable before `timer::init` has run (the Raspberry Pi's counter runs at 54 MHz;
/// the firmware normally sets CNTFRQ_EL0, qemu says 62.5 MHz or the host's).
fn counter_hz() -> u64 {
    let f: u64;
    // SAFETY: register read.
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack)) };
    if f == 0 { 54_000_000 } else { f }
}

fn counter() -> u64 {
    let v: u64;
    // SAFETY: register read; the isb keeps it from being speculated early.
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
    v
}

#[derive(Clone, Copy)]
pub struct Timeout {
    start: u64,
    ticks: u64,
}

impl Timeout {
    /// Expires `us` microseconds from now.
    pub fn us(us: u64) -> Timeout {
        Timeout {
            start: counter(),
            ticks: (counter_hz() / 1_000_000).max(1) * us.max(1),
        }
    }

    /// Expires `ms` milliseconds from now.
    pub fn ms(ms: u64) -> Timeout {
        Timeout::us(ms.saturating_mul(1000))
    }

    pub fn expired(&self) -> bool {
        counter().wrapping_sub(self.start) > self.ticks
    }
}
