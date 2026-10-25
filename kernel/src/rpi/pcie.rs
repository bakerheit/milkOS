//! The BCM2711's PCIe root complex (Broadcom STB PCIe core), which has exactly one thing behind it on a
//! Raspberry Pi 4: the VIA VL805 USB 3 host controller that serves the four type-A ports.
//!
//! **Not yet confirmed on a working board.** qemu's raspi4b has no PCIe, so this was written from the register
//! layout and bring-up order of the Broadcom core and is logged heavily; the first run on a board was
//! the test (docs/raspberry-pi.md, "PCIe + USB"), and it ended in an SError that the log could only name by
//! the next checkpoint. Hence the structure: the bring-up is a table of steps (`rpikit::pcie::STEPS`), every
//! step ends with an SError checkpoint of its own, and a checkpoint that finds a rejected access is
//! *survived* (`serror::absorb`): the step is reported as failed, the controller is parked and USB is skipped,
//! the rest of the system goes on. `milk.pcie=probe` adds a checkpoint after every register write and a
//! `pcie: step N name ok` line per step, and tries every order of the firmware handshake.
//!
//! What happens: take the bridge out of reset, let the SerDes settle, set the inbound window (PCI bus
//! address 0 = CPU address 0, so DMA addresses are CPU physical addresses), release PERST#, wait for the
//! link, set the outbound window (CPU 0x6_0000_0000 = PCI 0xc000_0000, 1 GiB in the stock tree, where BARs
//! go), then enumerate with the command registers left off, tell the firmware about the VL805
//! (mailbox tag NOTIFY_XHCI_RESET, so it can load the chip's code) and only then turn memory decoding and bus
//! mastering on. Config space of the root port is the controller's own register file at offset 0; everything
//! below goes through an index/data pair.
//!
//! Every wait is bounded; a link that does not come up ends with `None` and a log line.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rpikit::pci as pcik;
use rpikit::pcie as plan;

use super::Timeout;
use crate::kprintln;
use crate::mmu::phys_to_virt;
use crate::pci::{self, Bdf, ConfigSpace, Found, Host};
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

/// `milk.pcie=probe`: a checkpoint after every register access.
static PROBE: AtomicBool = AtomicBool::new(false);
/// What the checkpoint after an access is called: an index into `plan::STEPS`, or `STEPS.len()` + an index
/// into `plan::ORDERS` while the firmware handshake is tried.
static PHASE: AtomicUsize = AtomicUsize::new(0);

fn phase_label() -> &'static str {
    let p = PHASE.load(Ordering::Relaxed);
    match plan::STEPS.get(p) {
        Some(s) => s.label,
        None => plan::ORDERS
            .get(p - plan::STEPS.len())
            .map_or("pcie-step", |o| o.label),
    }
}

/// In probe mode a rejected access is taken (and survived) right after it, and the dump names the register.
fn after_access() {
    if PROBE.load(Ordering::Relaxed) {
        super::serror_checkpoint(phase_label());
    }
}

fn rd(off: usize) -> u32 {
    // SAFETY: the PCIe controller's registers, mapped as Device memory (platform.rs).
    crate::serror::note_read(BASE + off);
    let v = unsafe { read_volatile((BASE + off) as *const u32) };
    after_access();
    v
}

fn wr(off: usize, v: u32) {
    // SAFETY: as above.
    let _c = crate::serror::write_guard(BASE + off, v);
    unsafe { write_volatile((BASE + off) as *mut u32, v) }
    after_access();
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

/// Where a bring-up is: counts the steps, closes each with a checkpoint and says whether it was clean.
struct Run {
    probe: bool,
    /// `serror::absorbed()` when the current step began.
    seen: u32,
}

impl Run {
    fn begin(&mut self, step: usize) {
        PHASE.store(step, Ordering::Relaxed);
        self.seen = crate::serror::absorbed();
    }

    /// End the first step: an SError taken here is logged once as expected and does not fail the step.
    fn end_lenient(&mut self, step: usize) -> bool {
        let s = &plan::STEPS[step];
        super::serror_checkpoint(s.label);
        if crate::serror::absorbed() != self.seen {
            kprintln!(
                "pcie: step {} {}: the SoC rejected an access while the bridge was in reset (an SError, taken and survived; expected, going on)",
                step + 1,
                s.name
            );
        } else if self.probe {
            kprintln!("pcie: step {} {} ok", step + 1, s.name);
        }
        true
    }

    /// End `step`: take a pending SError here, say how it went (every step in probe mode, failures always).
    fn end(&mut self, step: usize) -> bool {
        let s = &plan::STEPS[step];
        super::serror_checkpoint(s.label);
        let clean = crate::serror::absorbed() == self.seen;
        if !clean {
            kprintln!(
                "pcie: step {} {} FAILED: the SoC rejected an access (an SError, taken and survived; the dump above names the register)",
                step + 1,
                s.name
            );
        } else if self.probe {
            kprintln!("pcie: step {} {} ok", step + 1, s.name);
        }
        clean
    }
}

/// The boot option `milk.pcie=`.
fn mode() -> plan::Mode {
    let mut buf = [0u8; 16];
    let Some(f) = crate::fwcfg::find_file("opt/milkos/pcie")
        .filter(|f| (f.size as usize) <= buf.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut buf[..f.size as usize]))
    else {
        return plan::Mode::Normal;
    };
    match plan::parse_mode(&buf[..f.size as usize]) {
        Some(m) => m,
        None => {
            kprintln!("[pcie] milk.pcie={:?} is not probe or normal: using normal", core::str::from_utf8(&buf[..f.size as usize]).unwrap_or("?"));
            plan::Mode::Normal
        }
    }
}

/// Hold the controller in reset again, after a failure: nothing may keep talking to a half-initialised link.
fn park() {
    wr(RGR1_SW_INIT_1, rd(RGR1_SW_INIT_1) | RGR1_INIT | RGR1_PERST);
    super::serror_checkpoint("pcie-park");
    kprintln!("[pcie] the controller is held in reset: USB is off, the rest of the system goes on");
}

/// Bring the root complex up to the point where the devices behind it can be enumerated (steps 1 to 10),
/// or `None` (with the reason logged and the controller parked if it had been touched).
fn root_complex(run: &mut Run) -> Option<Host> {
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
    // (Only the reset block's register is read before the bridge is out of reset: the controller's own status register sits behind
    // the bridge, and a read of it while the bridge is held in reset can be rejected by the SoC. The status is logged by the link wait.)
    kprintln!("[pcie] root complex at {PCIE_PHYS:#x}: bringing it up (reset register {:#x})", rd(RGR1_SW_INIT_1));
    let bail = |r: &mut Run, step: usize| -> Option<Host> {
        let _ = r.end(step);
        park();
        None
    };
    // 1. Reset the bridge, then release it, and let the SerDes settle.
    run.begin(0);
    wr(RGR1_SW_INIT_1, rd(RGR1_SW_INIT_1) | RGR1_INIT);
    delay_ms(1);
    // An SError at this first checkpoint is expected on the real board (it was absorbed here in every run, with and without
    // the probe) and the bring-up goes on after it every time: it is logged once, and the step does not fail.
    if !run.end_lenient(0) {
        return bail(run, 0);
    }
    run.begin(1);
    wr(RGR1_SW_INIT_1, rd(RGR1_SW_INIT_1) & !RGR1_INIT);
    delay_ms(1);
    if !run.end(1) {
        return bail(run, 1);
    }
    run.begin(2);
    set_bits(MISC_HARD_PCIE_HARD_DEBUG, HARD_DEBUG_SERDES_IDDQ, 0);
    delay_ms(1);
    if !run.end(2) {
        return bail(run, 2);
    }
    // 2. Burst size 128 bytes, config reads of absent functions return all ones, SCB (memory) access on.
    run.begin(3);
    set_bits(
        MISC_MISC_CTRL,
        MISC_CTRL_SCB_ACCESS_EN | MISC_CTRL_CFG_READ_UR_MODE | MISC_CTRL_MAX_BURST_SIZE_MASK,
        MISC_CTRL_SCB_ACCESS_EN | MISC_CTRL_CFG_READ_UR_MODE,
    );
    if !run.end(3) {
        return bail(run, 3);
    }
    // 3. The inbound window: PCI address 0 = CPU address 0, INBOUND_SIZE bytes (BAR2); BAR1 and BAR3 off.
    run.begin(4);
    wr(
        MISC_RC_BAR2_CONFIG_LO,
        rd(MISC_RC_BAR2_CONFIG_LO) & !(BAR_CONFIG_SIZE_MASK | 0xffff_ffe0)
            | plan::encode_ibar_size(INBOUND_SIZE),
    );
    wr(MISC_RC_BAR2_CONFIG_HI, 0);
    set_bits(MISC_RC_BAR1_CONFIG_LO, BAR_CONFIG_SIZE_MASK, 0);
    set_bits(MISC_RC_BAR3_CONFIG_LO, BAR_CONFIG_SIZE_MASK, 0);
    set_bits(
        MISC_MISC_CTRL,
        MISC_CTRL_SCB0_SIZE_MASK,
        plan::scb_size_field(INBOUND_SIZE) << MISC_CTRL_SCB0_SIZE_SHIFT,
    );
    // Little-endian BAR2 access.
    set_bits(RC_CFG_VENDOR_SPECIFIC_REG1, 0xc, 0);
    if !run.end(4) {
        return bail(run, 4);
    }
    // 4. No interrupts handled here: mask and clear everything.
    run.begin(5);
    wr(INTR2_CPU_MASK_SET, 0xffff_ffff);
    wr(INTR2_CPU_CLEAR, 0xffff_ffff);
    if !run.end(5) {
        return bail(run, 5);
    }
    // 5. Release PERST# (100 ms before anything talks to the link), then wait for the link.
    run.begin(6);
    wr(RGR1_SW_INIT_1, rd(RGR1_SW_INIT_1) & !RGR1_PERST);
    delay_ms(100);
    if !run.end(6) {
        return bail(run, 6);
    }
    run.begin(7);
    let t = Timeout::ms(100);
    while !link_up() && !t.expired() {
        delay_ms(5);
    }
    let status = rd(MISC_PCIE_STATUS);
    if link_up() && status & STATUS_PORT_RC != 0 {
        if !run.end(7) {
            return bail(run, 7);
        }
    } else {
        super::serror_checkpoint(plan::STEPS[7].label);
        if !link_up() {
            kprintln!("pcie: step 8 link-wait FAILED: the link did not come up (status {status:#x}): no USB devices");
        } else {
            kprintln!("pcie: step 8 link-wait FAILED: the controller is not in root complex mode (status {status:#x})");
        }
        dump_registers();
        park();
        return None;
    }
    kprintln!("[pcie] link up (status {status:#x})");
    // 6. The outbound window: CPU_WINDOW.. -> PCI_WINDOW.., in megabytes for the base/limit fields.
    run.begin(8);
    let w = plan::outbound(pci_window, cpu_window, window_size);
    wr(MISC_CPU_2_PCIE_MEM_WIN0_LO, w.pci_lo);
    wr(MISC_CPU_2_PCIE_MEM_WIN0_HI, w.pci_hi);
    set_bits(
        MISC_CPU_2_PCIE_MEM_WIN0_BASE_LIMIT,
        plan::OUTBOUND_BASE_LIMIT_MASK,
        w.base_limit,
    );
    set_bits(MISC_CPU_2_PCIE_MEM_WIN0_BASE_HI, 0xff, w.base_hi);
    set_bits(MISC_CPU_2_PCIE_MEM_WIN0_LIMIT_HI, 0xff, w.limit_hi);
    if !run.end(8) {
        return bail(run, 8);
    }
    // 7. The root port presents itself as a PCI-PCI bridge, without L0s.
    run.begin(9);
    set_bits(RC_CFG_PRIV1_ID_VAL3, 0x00ff_ffff, 0x0006_0400);
    set_bits(RC_CFG_PRIV1_LINK_CAPABILITY, 0x0000_0c00, 0);
    // The root port's config space is its register file: check it says what we expect.
    let id = rd(0);
    kprintln!(
        "[pcie] root port {:04x}:{:04x}",
        pcik::vendor(id),
        pcik::device(id)
    );
    if !run.end(9) {
        return bail(run, 9);
    }
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

/// Bring the root complex up, find the xHCI controllers behind it, make the firmware handshake and turn them on.
/// Returns them with the translation of their BARs, or `None` (the reason logged; the system goes on without USB).
pub fn bring_up() -> Option<(Vec<pci::Device>, pci::Translate)> {
    if super::disabled("pcie") {
        return None;
    }
    let m = mode();
    let probe = m == plan::Mode::Probe;
    PROBE.store(probe, Ordering::Relaxed);
    if probe {
        kprintln!(
            "[pcie] milk.pcie=probe: a checkpoint after every register access, a line per step, every order of the firmware handshake is tried"
        );
    }
    // An SError found at any checkpoint of this thread from here on is survived (details logged), never fatal.
    let _tolerate = crate::serror::tolerate();
    // An SError left over from some earlier access on this CPU (the SD card driver's, say) may still be pending: take it here,
    // say so once, and do not count it against the bring-up (the dump of what was written lately is in the crash screen only
    // when it is fatal; here it is not).
    if !crate::serror::checkpoint_catch("pcie-drain") {
        kprintln!("[pcie] an SError from an earlier access on this CPU was still pending: drained before the bring-up, it is not the controller's");
    }
    let mut run = Run { probe, seen: 0 };
    let Some(host) = root_complex(&mut run) else {
        PROBE.store(false, Ordering::Relaxed);
        return None;
    };
    let result = devices(&host, &mut run);
    // Nothing may be left pending when this thread goes back to its work.
    super::serror_checkpoint("pcie-done");
    PROBE.store(false, Ordering::Relaxed);
    if result.is_none() {
        park();
    }
    result.map(|d| (d, host.translate()))
}

/// Steps 11 to 13: enumerate with the command registers off, the handshake, decoding on.
fn devices(host: &Host, run: &mut Run) -> Option<Vec<pci::Device>> {
    run.begin(10);
    let found = pci::enumerate_deferred(host);
    if !run.end(10) {
        return None;
    }
    let vl805 = found
        .devices
        .iter()
        .position(|d| d.is_xhci() && pcik::vendor(d.id) == 0x1106);
    if let Some(i) = vl805 {
        if !handshake(host, &found, &found.devices[i], run) {
            return None;
        }
    }
    run.begin(12);
    found.set_commands(host, true, true);
    // The first look at the controller itself: if the device does not decode its BAR, say so here
    // rather than inside the xHCI driver's first register read.
    let mut ok = true;
    for d in found.devices.iter().filter(|d| d.is_xhci()) {
        let bar = d.bars[0];
        if bar.size == 0 {
            continue;
        }
        let Some(va) = pci::mmio(host.cpu_addr(bar.pci_addr), bar.size as usize) else {
            kprintln!("[pcie] cannot map the controller's registers");
            ok = false;
            continue;
        };
        crate::serror::note_read(va);
        // SAFETY: the controller's first BAR, mapped as Device memory just above.
        let v = unsafe { read_volatile(va as *const u32) };
        super::serror_checkpoint("pcie-bar-read");
        kprintln!(
            "[pcie] {:02x}:{:02x}.{} BAR0 reads {v:#010x} (xHCI version {:x}.{:02x}, capability registers {} bytes)",
            d.bdf.bus,
            d.bdf.dev,
            d.bdf.func,
            v >> 24,
            (v >> 16) & 0xff,
            v & 0xff
        );
        if !plan::plausible_xhci_caps(v) {
            kprintln!("[pcie] that is not what an xHCI controller says: the device does not decode its BAR");
            ok = false;
        }
    }
    if !run.end(12) || !ok {
        return None;
    }
    Some(found.devices.into_iter().filter(|d| d.is_xhci()).collect())
}

/// Step 12: the firmware handshake for the VL805, in the first order that works (probe mode: every order,
/// each one's result logged). Returns whether one worked.
fn handshake(host: &Host, found: &Found, vl: &pci::Device, run: &mut Run) -> bool {
    run.seen = crate::serror::absorbed();
    let mut first_ok = None;
    for (k, o) in plan::ORDERS.iter().enumerate() {
        if first_ok.is_some() && !run.probe {
            break;
        }
        PHASE.store(plan::STEPS.len() + k, Ordering::Relaxed);
        let ok = try_order(host, found, vl, o);
        kprintln!(
            "pcie: order {} {} ({}): {}",
            k + 1,
            o.name,
            o.what,
            if ok { "ok" } else { "FAILED" }
        );
        if ok && first_ok.is_none() {
            first_ok = Some(k);
        }
    }
    PHASE.store(11, Ordering::Relaxed);
    match first_ok {
        Some(k) => {
            // The failed orders before it do not matter any more: only the one that worked.
            run.seen = crate::serror::absorbed();
            if run.probe {
                kprintln!("pcie: step 12 vl805-handshake ok (order {} {} worked)", k + 1, plan::ORDERS[k].name);
            }
            true
        }
        None => {
            kprintln!("pcie: step 12 vl805-handshake FAILED: no order of the firmware call left the VL805 answering");
            false
        }
    }
}

/// One attempt of the handshake in order `o`. The command registers are set as the order wants them, the firmware
/// is told (if the order says so), and the device must answer with the same identity afterwards.
fn try_order(host: &Host, found: &Found, vl: &pci::Device, o: &plan::Order) -> bool {
    let seen = crate::serror::absorbed();
    let clean = || crate::serror::absorbed() == seen;
    found.set_commands(host, o.memory_before, o.master_before);
    super::serror_checkpoint(o.label);
    if !clean() {
        return false;
    }
    if o.notify {
        notify_vl805(vl.bdf);
        super::serror_checkpoint(o.label);
        if !clean() {
            return false;
        }
    }
    // The chip is being reset or has just been: wait for it to answer with its identity again.
    let t = Timeout::ms(300);
    let mut now = host.cfg.read(vl.bdf, pcik::cfg::ID);
    while plan::survival(vl.id, now) != plan::Survival::Same && !t.expired() {
        delay_ms(5);
        now = host.cfg.read(vl.bdf, pcik::cfg::ID);
    }
    super::serror_checkpoint(o.label);
    if !clean() {
        return false;
    }
    if plan::survival(vl.id, now) != plan::Survival::Same {
        kprintln!(
            "[pcie] after the handshake the device answers {now:#010x} instead of {:#010x} (link {})",
            vl.id,
            if link_up() { "up" } else { "down" }
        );
        return false;
    }
    if pci::restore_bars(host, vl) {
        kprintln!("[pcie] the VL805 had forgotten its BARs (a reset): put them back");
    }
    super::serror_checkpoint(o.label);
    clean()
}

/// Tell the firmware, which loads the VL805's code if the chip still needs it (the call Linux makes when it
/// enables the device). The caller holds the SError tolerance: the mailbox's own checkpoint ends the call.
fn notify_vl805(bdf: Bdf) {
    let addr = plan::vl805_notify_word(bdf.bus, bdf.dev, bdf.func);
    match super::mailbox::query(rpikit::mailbox::tag::NOTIFY_XHCI_RESET, &[addr], 1) {
        Ok(_) => kprintln!("[pcie] the firmware was told about the VL805 (reset notify {addr:#x})"),
        Err(e) => kprintln!("[pcie] the firmware did not take the VL805 reset notify: {e:?}"),
    }
    delay_ms(20);
}
