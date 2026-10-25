//! The BCM2711's hardware random number generator (the iProc "RNG200" core: a NIST-style
//! entropy source with a FIFO), at 0xfe10_4000 (`rng@7e104000` in the device tree). It seeds the kernel's CSPRNG the way virtio-rng does
//! on qemu (`crypto/rng.rs` asks [`read`] when there is no virtio-rng). **Untested on hardware**
//! (qemu's raspi4b does not model it): bring-up follows the restart sequence of Broadcom's driver;
//! every wait is bounded and a source that does not answer is simply not used.
//!
//! Registers: CTRL (0x00, bit 0 enables the generator), SOFT_RESET (0x04) and RBG_SOFT_RESET
//! (0x08), INT_STATUS (0x18, write 1 to clear), FIFO_DATA (0x20), FIFO_COUNT (0x24, low 8 bits =
//! words available).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering};

use super::Timeout;
use crate::kprintln;
use crate::mmu::phys_to_virt;

const BASE: usize = phys_to_virt(crate::platform::periph(0x10_4000));
const CTRL: usize = 0x00;
const SOFT_RESET: usize = 0x04;
const RBG_SOFT_RESET: usize = 0x08;
const INT_STATUS: usize = 0x18;
const FIFO_DATA: usize = 0x20;
const FIFO_COUNT: usize = 0x24;
const CTRL_RBGEN_MASK: u32 = 0x1fff;
const CTRL_RBGEN_ENABLE: u32 = 1;
/// Interrupt status bits that mean the generator failed its health tests (master fail lockout, NIST fail).
const INT_FAILED: u32 = 1 << 31 | 1 << 5;

static PRESENT: AtomicBool = AtomicBool::new(false);

fn rd(off: usize) -> u32 {
    // SAFETY: the generator's registers, mapped as Device memory (platform.rs).
    unsafe { read_volatile((BASE + off) as *const u32) }
}

fn wr(off: usize, v: u32) {
    // SAFETY: as above.
    unsafe { write_volatile((BASE + off) as *mut u32, v) }
}

/// Stop, clear, reset both blocks, start (Broadcom's restart order).
fn restart() {
    wr(CTRL, rd(CTRL) & !CTRL_RBGEN_MASK);
    wr(INT_STATUS, 0xffff_ffff);
    wr(RBG_SOFT_RESET, rd(RBG_SOFT_RESET) | 1);
    wr(SOFT_RESET, rd(SOFT_RESET) | 1);
    wr(SOFT_RESET, rd(SOFT_RESET) & !1);
    wr(RBG_SOFT_RESET, rd(RBG_SOFT_RESET) & !1);
    wr(CTRL, (rd(CTRL) & !CTRL_RBGEN_MASK) | CTRL_RBGEN_ENABLE);
}

/// Start the generator if the device tree has it (`rng@7e104000`, `brcm,bcm2711-rng200`).
pub fn init() {
    // SAFETY: the bootloader's pointer, or 0.
    let Some(dtb) = (unsafe { crate::dtb::Dtb::from_addr(crate::mem::boot_dtb_ptr()) }) else {
        return;
    };
    let has = dtb
        .find_prefix(b"rng@", b"compatible")
        .is_some_and(|c| crate::dtb::compat_has(c, "rng200"));
    if !has || dtb.node_disabled(b"rng@") {
        kprintln!("[rng] no enabled rng200 node in the device tree");
        return;
    }
    restart();
    PRESENT.store(true, Ordering::Release);
    kprintln!(
        "[rng] BCM2711 hardware generator started (ctrl {:#x} int status {:#x} fifo count {})",
        rd(CTRL),
        rd(INT_STATUS),
        rd(FIFO_COUNT)
    );
}

/// Up to `out.len()` random bytes from the generator, waiting at most `timeout_ns` in total; the
/// number delivered (0 if there is no generator or it is failing).
pub fn read(out: &mut [u8], timeout_ns: u64) -> usize {
    if !PRESENT.load(Ordering::Acquire) {
        return 0;
    }
    let t = Timeout::us(timeout_ns / 1000);
    let mut n = 0;
    while n < out.len() {
        if rd(INT_STATUS) & INT_FAILED != 0 {
            restart();
            return n;
        }
        if rd(FIFO_COUNT) & 0xff == 0 {
            if t.expired() {
                break;
            }
            continue;
        }
        let w = rd(FIFO_DATA).to_le_bytes();
        let k = (out.len() - n).min(4);
        out[n..n + k].copy_from_slice(&w[..k]);
        n += k;
    }
    n
}
