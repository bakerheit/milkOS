//! The Snapdragon's watchdog. The bootloader may leave the application processor's watchdog
//! running; Linux pets it from the moment its driver loads. Nobody pets it in stage 1, so a phone
//! that resets by itself some seconds after the image started is most likely this, not a crash:
//! the counter on the screen says how long it ran. `msm.wdt=pet` pets it once a second (one
//! register write), so the phone stays up for as long as there is something to photograph.
//! The registers are only read otherwise, and logged.

use crate::dtb::Dtb;
use crate::kprintln;
use crate::mmu::phys_to_virt;
use rpikit::msm::wdt;

/// The watchdog block's physical base, if the tree has one inside the mapped device range.
pub fn locate(d: Option<&Dtb>) -> Option<usize> {
    let mut reg = [(0u64, 0u64); 2];
    if d?.node_reg_compat(b"qcom,msm-watchdog", &mut reg) == 0 {
        return None;
    }
    let (lo, hi) = crate::platform::CURRENT.device.first().copied()?;
    let (pa, size) = reg[0];
    (size >= 0x18 && pa >= lo as u64 && pa + 0x18 <= hi as u64).then_some(pa as usize)
}

fn rd(base: usize, off: usize) -> u32 {
    // SAFETY: Device memory in the first GiB (checked by `locate`); a read.
    unsafe { core::ptr::read_volatile(phys_to_virt(base + off) as *const u32) }
}

/// Log what the registers say (reads only).
pub fn report(base: Option<usize>) {
    let Some(b) = base else {
        kprintln!("wdt: no qcom,msm-watchdog in the tree");
        return;
    };
    let (en, sts, bark, bite) = (
        rd(b, wdt::EN),
        rd(b, wdt::STS),
        rd(b, wdt::BARK_TIME),
        rd(b, wdt::BITE_TIME),
    );
    kprintln!("wdt: {b:#x}: EN {en:#x} STS {sts:#x}");
    kprintln!(
        "wdt: bark {} ms, bite {} ms (if EN is 1 and nobody pets it, the phone resets after the bite)",
        wdt::ms(bark),
        wdt::ms(bite)
    );
}

/// Pet it: restart its countdown.
pub fn pet(base: usize) {
    // SAFETY: as `rd`; the one register written is WDT_RST.
    unsafe { core::ptr::write_volatile(phys_to_virt(base + wdt::RST) as *mut u32, 1) };
}
