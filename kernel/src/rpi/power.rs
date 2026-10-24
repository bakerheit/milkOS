//! Restart and power off, through the SoC's power manager (the "PM" watchdog block at 0xfe10_0000): the
//! Pi has no PSCI. A restart is a full reset by the watchdog; "power off" sets the firmware's halt marker
//! in the reset status register first (partition 63, the same trick Linux uses), so the bootloader stops
//! after the reset and the board draws almost nothing until it is power cycled.
//!
//! Registers: RSTC (0x1c), RSTS (0x20), WDOG (0x24); every write carries the password 0x5a in its top byte.

use core::ptr::{read_volatile, write_volatile};

use crate::mmu::phys_to_virt;
use crate::platform::periph;

const PM: usize = phys_to_virt(periph(0x10_0000));
const RSTC: usize = 0x1c;
const RSTS: usize = 0x20;
const WDOG: usize = 0x24;
const PASSWORD: u32 = 0x5a00_0000;
const RSTC_WRCFG_CLR: u32 = 0xffff_ffcf;
const RSTC_WRCFG_FULL_RESET: u32 = 0x20;
const RSTS_HALT: u32 = 0x555;
const WDOG_TIME_MASK: u32 = 0x000f_ffff;

fn rd(off: usize) -> u32 {
    // SAFETY: the power manager's registers, mapped as Device memory (platform.rs).
    unsafe { read_volatile((PM + off) as *const u32) }
}

fn wr(off: usize, v: u32) {
    // SAFETY: as above.
    unsafe { write_volatile((PM + off) as *mut u32, v) }
}

fn full_reset() -> ! {
    // A watchdog timeout of 10 ticks (about 150 us), then a full reset.
    wr(WDOG, PASSWORD | (10 & WDOG_TIME_MASK));
    wr(
        RSTC,
        (rd(RSTC) & RSTC_WRCFG_CLR) | PASSWORD | RSTC_WRCFG_FULL_RESET,
    );
    loop {
        // SAFETY: wait for the reset to take us.
        unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
    }
}

/// Reset the board. Does not return.
pub fn restart() -> ! {
    full_reset()
}

/// Power off (halt until power cycled). Does not return.
pub fn power_off() -> ! {
    wr(RSTS, (rd(RSTS) & RSTC_WRCFG_CLR) | PASSWORD | RSTS_HALT);
    full_reset()
}
