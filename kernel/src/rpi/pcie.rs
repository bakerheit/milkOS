//! The BCM2711's PCIe root complex (Broadcom STB PCIe core), which has exactly one thing behind it on a
//! Raspberry Pi 4: the VIA VL805 USB 3 host controller that serves the four type-A ports.
//!
//! **Untested on hardware.** qemu's raspi4b has no PCIe, so this was written from the register
//! layout and bring-up order of the Broadcom core (public in the Linux driver's behaviour) and is
//! logged heavily; the first run on a board is its test (docs/raspberry-pi.md, "Stage 2"). What
//! happens: take the bridge out of reset, let the SerDes settle, set the inbound window (PCI bus
//! address 0 = CPU address 0, so DMA addresses are CPU physical addresses), the outbound window
//! (CPU 0x6_0000_0000 = PCI 0xc000_0000, 1 GiB in the stock tree, where BARs go), release PERST#, wait for the link,
//! then hand a config space accessor to `pci::enumerate`. Config space of the root port is the
//! controller's own register file at offset 0; everything below goes through an index/data pair.
//! After the VL805 is found the firmware is told (mailbox tag NOTIFY_XHCI_RESET) so it can load the
//! chip's firmware if it needs to.
//!
//! Every wait is bounded; a link that does not come up ends with `None` and a log line.

use alloc::boxed::Box;
use core::ptr::{read_volatile, write_volatile};

use rpikit::pci as pcik;

use super::Timeout;
use crate::kprintln;
use crate::mmu::phys_to_virt;
use crate::pci::{Bdf, ConfigSpace, Host};
use crate::sync::SpinLock;

/// Where the controller lives (ARM physical, low-peripheral mode; just below the SoC peripherals).
const PCIE_PHYS: usize = 0xfd50_0000;
const BASE: usize = phys_to_virt(PCIE_PHYS);

// Controller registers.
const RC_CFG_VENDOR_SPECIFIC_REG1: usize = 0x0188;
const RC_CFG_PRIV1_ID_VAL3: usize = 0x043c;
const RC_CFG_PRIV1_LINK_CAPABILITY: usize = 0x04dc;
const MISC_MISC_CTRL: usize = 0x4008;
const MISC_CPU_2_PCIE_MEM_WIN0_LO: usize = 0x400c;
const MISC_CPU_2_PCIE_MEM_WIN0_HI: usize = 0x4010;
const MISC_RC_BAR1_CONFIG_LO: usize = 0x402c;
const MISC_RC_BAR2_CONFIG_LO: usize = 0x4034;
const MISC_RC_BAR2_CONFIG_HI: usize = 0x4038;
const MISC_RC_BAR3_CONFIG_LO: usize = 0x403c;
const MISC_PCIE_STATUS: usize = 0x4068;
const MISC_CPU_2_PCIE_MEM_WIN0_BASE_LIMIT: usize = 0x4070;
const MISC_CPU_2_PCIE_MEM_WIN0_BASE_HI: usize = 0x4080;
const MISC_CPU_2_PCIE_MEM_WIN0_LIMIT_HI: usize = 0x4084;
const MISC_HARD_PCIE_HARD_DEBUG: usize = 0x4204;
const INTR2_CPU_CLEAR: usize = 0x4300 + 0x08;
const INTR2_CPU_MASK_SET: usize = 0x4300 + 0x10;
const RGR1_SW_INIT_1: usize = 0x9210;
const EXT_CFG_INDEX: usize = 0x9000;
const EXT_CFG_DATA: usize = 0x8000;

// Fields.
const MISC_CTRL_SCB_ACCESS_EN: u32 = 1 << 12;
const MISC_CTRL_CFG_READ_UR_MODE: u32 = 1 << 13;
const MISC_CTRL_MAX_BURST_SIZE_MASK: u32 = 0x0030_0000; // 0 = 128 bytes
const MISC_CTRL_SCB0_SIZE_SHIFT: u32 = 27;
const MISC_CTRL_SCB0_SIZE_MASK: u32 = 0xf800_0000;
const RGR1_INIT: u32 = 1 << 1; // bridge reset (BCM2711 layout)
const RGR1_PERST: u32 = 1 << 0; // fundamental reset to the link partner
const HARD_DEBUG_SERDES_IDDQ: u32 = 1 << 27;
const STATUS_PHYLINKUP: u32 = 1 << 4;
const STATUS_DL_ACTIVE: u32 = 1 << 5;
const STATUS_PORT_RC: u32 = 1 << 7;
const BAR_CONFIG_SIZE_MASK: u32 = 0x1f;

/// The outbound window when the device tree does not say (it does: `ranges` of `pcie@7d500000`): BARs of
/// devices behind the root port are placed in `[PCI_WINDOW, PCI_WINDOW + WINDOW_SIZE)`, which the CPU
/// sees at `CPU_WINDOW`. These are the values of the stock tree.
const PCI_WINDOW: u64 = 0xc000_0000;
const CPU_WINDOW: u64 = 0x6_0000_0000;
const WINDOW_SIZE: u64 = 0x4000_0000;
/// The inbound window: PCI bus address 0.. maps to CPU address 0.. for this much RAM (a power of two;
/// the DMA limit in platform.rs keeps allocations inside what is reachable).
const INBOUND_SIZE: u64 = 1 << 32;

fn rd(off: usize) -> u32 {
    // SAFETY: the PCIe controller's registers, mapped as Device memory (platform.rs).
    crate::serror::note_read(BASE + off);
    unsafe { read_volatile((BASE + off) as *const u32) }
}

fn wr(off: usize, v: u32) {
    // SAFETY: as above.
    let _c = crate::serror::write_guard(BASE + off, v);
    unsafe { write_volatile((BASE + off) as *mut u32, v) }
}

/// The controller's registers, for a log when something does not come up.
fn dump_registers() {
    kprintln!(
        "[pcie] registers: status {:#x} misc_ctrl {:#x} hard_debug {:#x} reset {:#x} link_cap {:#x} bar2 {:#x} win0 {:#x}/{:#x}",
        rd(MISC_PCIE_STATUS),
        rd(MISC_MISC_CTRL),
        rd(MISC_HARD_PCIE_HARD_DEBUG),
        rd(RGR1_SW_INIT_1),
        rd(RC_CFG_PRIV1_LINK_CAPABILITY),
        rd(MISC_RC_BAR2_CONFIG_LO),
        rd(MISC_CPU_2_PCIE_MEM_WIN0_HI),
        rd(MISC_CPU_2_PCIE_MEM_WIN0_LO)
    );
}

fn set_bits(off: usize, mask: u32, val: u32) {
    wr(off, rd(off) & !mask | val & mask);
}

/// Inbound window size field: log2(size) - 15 for 64 KiB..32 GiB (the controller's encoding).
fn encode_ibar_size(size: u64) -> u32 {
    let log2 = 63 - size.leading_zeros();
    match log2 {
        12..=15 => log2 - 12 + 0x1c,
        16..=35 => log2 - 15,
        _ => 0,
    }
}

/// Config space of the root port (bus 0) and of what is behind it (bus 1).
struct Config {
    /// Serialises the index/data pair.
    lock: SpinLock<()>,
}

impl Config {
    fn valid(bdf: Bdf) -> bool {
        match bdf.bus {
            0 => bdf.dev == 0 && bdf.func == 0,
            1 => bdf.dev == 0,
            _ => false,
        }
    }
}

impl ConfigSpace for Config {
    fn read(&self, bdf: Bdf, off: u16) -> u32 {
        if !Self::valid(bdf) {
            return !0;
        }
        if bdf.bus == 0 {
            return rd(off as usize & 0xffc);
        }
        let _g = self.lock.lock();
        wr(
            EXT_CFG_INDEX,
            (bdf.bus as u32) << 20 | (bdf.dev as u32) << 15 | (bdf.func as u32) << 12,
        );
        rd(EXT_CFG_DATA + (off as usize & 0xffc))
    }

    fn write(&self, bdf: Bdf, off: u16, v: u32) {
        if !Self::valid(bdf) {
            return;
        }
        if bdf.bus == 0 {
            wr(off as usize & 0xffc, v);
            return;
        }
        let _g = self.lock.lock();
        wr(
            EXT_CFG_INDEX,
            (bdf.bus as u32) << 20 | (bdf.dev as u32) << 15 | (bdf.func as u32) << 12,
        );
        wr(EXT_CFG_DATA + (off as usize & 0xffc), v);
    }
}

fn delay_ms(ms: u64) {
    let t = Timeout::ms(ms);
    while !t.expired() {
        core::hint::spin_loop();
    }
}

fn link_up() -> bool {
    let s = rd(MISC_PCIE_STATUS);
    s & STATUS_PHYLINKUP != 0 && s & STATUS_DL_ACTIVE != 0
}

/// Bring the root complex up and return the host bridge, or `None` (with the reason logged).
pub fn host() -> Option<Host> {
    if super::disabled("pcie") {
        return None;
    }
    // The firmware's device tree has the controller (`pcie@7d500000`); qemu's raspi4b does not model
    // it (an access would be a bus error), and the tests' tree does not name it.
    // SAFETY: the bootloader's pointer, or 0.
    let dtb = unsafe { crate::dtb::Dtb::from_addr(crate::mem::boot_dtb_ptr()) };
    let Some(d) =
        dtb.filter(|d| d.find_prefix(b"pcie@", b"reg").is_some() && !d.node_disabled(b"pcie@"))
    else {
        kprintln!(
            "[pcie] the device tree has no (enabled) PCIe controller: no USB (qemu's raspi4b has none)"
        );
        return None;
    };
    // The outbound window from the tree's `ranges` (2 parent address cells, 2 size cells).
    let mut r = [pcik::Range {
        space: 0,
        prefetch: false,
        pci_addr: 0,
        cpu_addr: 0,
        size: 0,
    }; pcik::MAX_RANGES];
    let n = d
        .find_prefix(b"pcie@", b"ranges")
        .map_or(0, |b| pcik::parse_ranges(b, 2, 2, &mut r));
    let win = pcik::memory_window(&r[..n]).filter(|w| w.pci_addr < 1 << 32 && w.size >= 1 << 20);
    let (pci_window, cpu_window, window_size) =
        win.map_or((PCI_WINDOW, CPU_WINDOW, WINDOW_SIZE), |w| {
            (
                w.pci_addr,
                w.cpu_addr,
                w.size.min((1u64 << 32) - w.pci_addr),
            )
        });
    kprintln!("[pcie] root complex at {PCIE_PHYS:#x}: bringing it up (status before {:#x}, reset register {:#x})", rd(MISC_PCIE_STATUS), rd(RGR1_SW_INIT_1));
    // 1. Reset the bridge, then release it, and let the SerDes settle.
    wr(RGR1_SW_INIT_1, rd(RGR1_SW_INIT_1) | RGR1_INIT);
    delay_ms(1);
    wr(RGR1_SW_INIT_1, rd(RGR1_SW_INIT_1) & !RGR1_INIT);
    delay_ms(1);
    set_bits(MISC_HARD_PCIE_HARD_DEBUG, HARD_DEBUG_SERDES_IDDQ, 0);
    delay_ms(1);
    // 2. Burst size 128 bytes, config reads of absent functions return all ones, SCB (memory) access on.
    set_bits(
        MISC_MISC_CTRL,
        MISC_CTRL_SCB_ACCESS_EN | MISC_CTRL_CFG_READ_UR_MODE | MISC_CTRL_MAX_BURST_SIZE_MASK,
        MISC_CTRL_SCB_ACCESS_EN | MISC_CTRL_CFG_READ_UR_MODE,
    );
    // 3. The inbound window: PCI address 0 = CPU address 0, INBOUND_SIZE bytes (BAR2); BAR1 and BAR3 off.
    wr(
        MISC_RC_BAR2_CONFIG_LO,
        rd(MISC_RC_BAR2_CONFIG_LO) & !(BAR_CONFIG_SIZE_MASK | 0xffff_ffe0)
            | encode_ibar_size(INBOUND_SIZE),
    );
    wr(MISC_RC_BAR2_CONFIG_HI, 0);
    set_bits(MISC_RC_BAR1_CONFIG_LO, BAR_CONFIG_SIZE_MASK, 0);
    set_bits(MISC_RC_BAR3_CONFIG_LO, BAR_CONFIG_SIZE_MASK, 0);
    let scb0 = (63 - INBOUND_SIZE.leading_zeros()) - 15;
    set_bits(
        MISC_MISC_CTRL,
        MISC_CTRL_SCB0_SIZE_MASK,
        scb0 << MISC_CTRL_SCB0_SIZE_SHIFT,
    );
    // Little-endian BAR2 access.
    set_bits(RC_CFG_VENDOR_SPECIFIC_REG1, 0xc, 0);
    // 4. No interrupts handled here: mask and clear everything.
    wr(INTR2_CPU_MASK_SET, 0xffff_ffff);
    wr(INTR2_CPU_CLEAR, 0xffff_ffff);
    // 5. Release PERST# and wait for the link (100 ms after PERST#, then up to 100 ms more).
    wr(RGR1_SW_INIT_1, rd(RGR1_SW_INIT_1) & !RGR1_PERST);
    delay_ms(100);
    let t = Timeout::ms(100);
    while !link_up() && !t.expired() {
        delay_ms(5);
    }
    let status = rd(MISC_PCIE_STATUS);
    if !link_up() {
        kprintln!("[pcie] the link did not come up (status {status:#x}): no USB devices");
        dump_registers();
        return None;
    }
    if status & STATUS_PORT_RC == 0 {
        kprintln!("[pcie] the controller is not in root complex mode (status {status:#x})");
        dump_registers();
        return None;
    }
    kprintln!("[pcie] link up (status {status:#x})");
    // 6. The outbound window: CPU_WINDOW.. -> PCI_WINDOW.., in megabytes for the base/limit fields.
    wr(MISC_CPU_2_PCIE_MEM_WIN0_LO, pci_window as u32);
    wr(MISC_CPU_2_PCIE_MEM_WIN0_HI, (pci_window >> 32) as u32);
    let base_mb = cpu_window >> 20;
    let limit_mb = (cpu_window + window_size - 1) >> 20;
    set_bits(
        MISC_CPU_2_PCIE_MEM_WIN0_BASE_LIMIT,
        0x0000_fff0 | 0xfff0_0000,
        ((base_mb as u32) & 0xfff) << 4 | ((limit_mb as u32) & 0xfff) << 20,
    );
    set_bits(
        MISC_CPU_2_PCIE_MEM_WIN0_BASE_HI,
        0xff,
        (base_mb >> 12) as u32,
    );
    set_bits(
        MISC_CPU_2_PCIE_MEM_WIN0_LIMIT_HI,
        0xff,
        (limit_mb >> 12) as u32,
    );
    // 7. The root port presents itself as a PCI-PCI bridge, without L0s.
    set_bits(RC_CFG_PRIV1_ID_VAL3, 0x00ff_ffff, 0x0006_0400);
    set_bits(RC_CFG_PRIV1_LINK_CAPABILITY, 0x0000_0c00, 0);
    // The root port's config space is its register file: check it says what we expect.
    let id = rd(0);
    kprintln!(
        "[pcie] root port {:04x}:{:04x}",
        pcik::vendor(id),
        pcik::device(id)
    );
    Some(Host {
        cfg: Box::new(Config {
            lock: SpinLock::new(()),
        }),
        window_pci: pci_window,
        window_cpu: cpu_window,
        window_size,
        first_bus: 0,
        last_bus: 1,
    })
}

/// After a VL805 has been enabled: tell the firmware, which loads the chip's firmware if it still
/// needs one (the same call Linux makes when it enables the device).
pub fn notify_vl805(bus: u8, dev: u8, func: u8) {
    let addr = (bus as u32) << 20 | (dev as u32) << 15 | (func as u32) << 12;
    match super::mailbox::query(rpikit::mailbox::tag::NOTIFY_XHCI_RESET, &[addr], 1) {
        Ok(_) => kprintln!("[pcie] the firmware was told about the VL805 (reset notify {addr:#x})"),
        Err(e) => kprintln!("[pcie] the firmware did not take the VL805 reset notify: {e:?}"),
    }
    delay_ms(20);
}
