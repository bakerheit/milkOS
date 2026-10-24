//! Raspberry Pi 4 (BCM2711) drivers, written from the public register documentation and the
//! behaviour of the firmware interfaces; nothing copied. Built only for the Pi image (feature
//! `raspi`, docs/raspberry-pi.md).
//!
//! * [`mailbox`]: the VideoCore property mailbox (framebuffer, clocks, power, board info).
//! * [`fb`]: the display, a framebuffer the firmware allocates through the mailbox.
//! * [`sdhci`]: the SD host controller (EMMC2 on a real board; qemu's raspi4b has the EMMC1
//!   one), as a block device.
//! * [`genet`]: the Gigabit Ethernet MAC. Not modelled by qemu, so untested until hardware.
//!
//! Every wait on hardware here is bounded ([`Timeout`]): a device that never answers makes a
//! driver give up, never the kernel hang.

pub mod fb;
pub mod mailbox;

use core::arch::asm;

/// Log what the firmware says about the board (firmware build, board revision, MAC address).
/// Failures are logged, never fatal: the board works without the answers.
pub fn init() {
    let rev = mailbox::board_revision();
    let fw = mailbox::firmware_revision();
    let mac = mailbox::board_mac();
    match (rev, fw) {
        (Ok(r), Ok(f)) => crate::kprintln!("rpi: board revision {r:#x}, firmware {f:#x}"),
        (Err(e), _) | (_, Err(e)) => crate::kprintln!("rpi: the firmware did not say what board this is ({e:?})"),
    }
    match mac {
        Ok(m) => crate::kprintln!(
            "rpi: ethernet address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        ),
        Err(e) => crate::kprintln!("rpi: no ethernet address from the firmware ({e:?})"),
    }
}

/// A deadline for a hardware poll, in the generic counter's ticks.
#[derive(Clone, Copy)]
pub struct Timeout {
    start: u64,
    ticks: u64,
}

/// Counter frequency usable before `timer::init` has run (the Pi's counter runs at 54 MHz;
/// the firmware normally sets CNTFRQ_EL0, qemu says 62.5 MHz).
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

impl Timeout {
    /// Expires `us` microseconds from now.
    pub fn us(us: u64) -> Timeout {
        Timeout {
            start: counter(),
            ticks: counter_hz() / 1_000_000 * us.max(1),
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

/// Clean and invalidate the data cache over `[va, va+len)`, to the point of coherency: before a
/// device reads memory the CPU wrote, and before the CPU reads what a device wrote.
pub fn flush_cache(va: usize, len: usize) {
    let ctr: u64;
    // SAFETY: register read.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    let line = 4usize << ((ctr >> 16) & 0xf);
    let mut a = va & !(line - 1);
    while a < va + len {
        // SAFETY: cache maintenance on mapped memory.
        unsafe { asm!("dc civac, {}", in(reg) a, options(nostack)) };
        a += line;
    }
    // SAFETY: barrier.
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}
