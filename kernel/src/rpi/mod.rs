//! Raspberry Pi 4 (BCM2711) drivers, written from the public register documentation and the
//! behaviour of the firmware interfaces; nothing copied. Built only for the Pi image (feature
//! `raspi`, docs/raspberry-pi.md).
//!
//! * [`mailbox`]: the VideoCore property mailbox (framebuffer, clocks, power, board info).
//! * [`led`]: the ACT LED as a sign of life (on while running, flashing after a crash).
//! * [`fb`]: the display, a framebuffer the firmware allocates through the mailbox.
//! * [`sdhci`]: the SD host controller (EMMC2 on a real board; qemu's raspi4b has the EMMC1
//!   one), as a block device.
//! * [`genet`]: the Gigabit Ethernet MAC. Not modelled by qemu, so untested until hardware.
//!
//! Every wait on hardware here is bounded ([`Timeout`]): a device that never answers makes a
//! driver give up, never the kernel hang.

pub mod fb;
pub mod genet;
pub mod led;
pub mod mailbox;
pub mod pcie;
pub mod rng;
pub mod sdhci;

use core::arch::asm;

/// Is `what` named in the boot option `milk.disable=a,b,c`? A way to start a board without a driver that hangs
/// it (edit cmdline.txt, no rebuild): `pcie`, `usb`, `genet`, `sdhci`, `rng`.
pub fn disabled(what: &str) -> bool {
    let mut buf = [0u8; 64];
    let Some(f) = crate::fwcfg::find_file("opt/milkos/disable")
        .filter(|f| (f.size as usize) <= buf.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut buf[..f.size as usize]))
    else {
        return false;
    };
    let hit = core::str::from_utf8(&buf[..f.size as usize])
        .unwrap_or("")
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|w| w == what);
    if hit {
        crate::kprintln!("rpi: {what} disabled by the command line (milk.disable)");
    }
    hit
}

/// Log what the firmware says about the board (firmware build, board revision, MAC address).
/// Failures are logged, never fatal: the board works without the answers.
pub fn init() {
    let rev = mailbox::board_revision();
    let fw = mailbox::firmware_revision();
    let mac = mailbox::board_mac();
    match (rev, fw) {
        (Ok(r), Ok(f)) => crate::kprintln!("rpi: board revision {r:#x}, firmware {f:#x}"),
        (Err(e), _) | (_, Err(e)) => {
            crate::kprintln!("rpi: the firmware did not say what board this is ({e:?})")
        }
    }
    match mac {
        Ok(m) => crate::kprintln!(
            "rpi: ethernet address {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0],
            m[1],
            m[2],
            m[3],
            m[4],
            m[5]
        ),
        Err(e) => crate::kprintln!("rpi: no ethernet address from the firmware ({e:?})"),
    }
    if !disabled("rng") {
        rng::init();
    }
    if !disabled("sdhci") {
        sdhci::init();
    }
    if !disabled("genet") {
        genet::init();
    }
}

pub use crate::timeout::Timeout;

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
