//! GICv3 driver for the Pixel 2 (feature `msm8998`, and its qemu test build with `-M virt,gic-version=3`):
//! the distributor with affinity routing, one redistributor per CPU, the CPU interface through its system
//! registers. [`crate::gic`] calls it where the GICv2 code calls its memory-mapped CPU interface; the
//! handler table, the counters and the numbering (INTID 0 to 15 SGI, 16 to 31 PPI, 32 up SPI) are shared.
//!
//! The sequence is Linux's `irq-gic-v3` for a non-secure kernel under a secure monitor, because that is what
//! this GIC is known to accept (the Snapdragon's firmware owns the secure half: the bootloader-patched tree
//! and the registers' non-secure view are all there is):
//!
//! 1. `ICC_SRE_EL1.SRE = 1` (read back: the monitor must allow it);
//! 2. distributor: off, SPIs configured (level, default priority, disabled), on with `ARE_NS | EnableGrp1`
//!    (read back: affinity routing must have turned on), every SPI routed to the boot CPU;
//! 3. this CPU's redistributor found by its affinity in `GICR_TYPER`, woken (`ProcessorSleep` cleared,
//!    `ChildrenAsleep` waited out, bounded), its SGIs and PPIs disabled with the default priority;
//! 4. CPU interface: priority mask, end-of-interrupt does both drop and deactivate, Group 1 on.
//!
//! Every wait is bounded and every step that can fail says which one did, so the first run on the phone
//! names the register that disagreed (the caller shows it on the screen and goes on without interrupts).
//! Nothing here writes a register the sequence above does not need.

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use rpikit::msm::{
    GIC_DEFAULT_PRIORITY, GIC_PMR, Redist, decode_gicd_typer, find_redist, gic_arch_rev, irouter,
    mpidr_affinity, redistributors, sgi1r,
};

use crate::dtb::Dtb;
use crate::mmu::phys_to_virt;
use crate::percpu::{self, MAX_CPUS};
use crate::timer;

// Distributor.
const GICD_CTLR: usize = 0x0000;
const GICD_TYPER: usize = 0x0004;
const GICD_IGROUPR: usize = 0x0080;
const GICD_ISENABLER: usize = 0x0100;
const GICD_ICENABLER: usize = 0x0180;
const GICD_ISPENDR: usize = 0x0200;
const GICD_ICPENDR: usize = 0x0280;
const GICD_ICACTIVER: usize = 0x0380;
const GICD_IPRIORITYR: usize = 0x0400;
const GICD_ICFGR: usize = 0x0c00;
const GICD_IROUTER: usize = 0x6000;
const GICD_PIDR2: usize = 0xffe8;
const CTLR_RWP: u32 = 1 << 31;
const CTLR_ARE_NS: u32 = 1 << 4;
const CTLR_ENABLE_G1A: u32 = 1 << 1;
const CTLR_ENABLE_G1: u32 = 1 << 0;

// Redistributor: the control frame, then the SGI/PPI frame 64 KiB above it (same register offsets as the
// distributor's first 32 interrupts).
const GICR_CTLR: usize = 0x0000;
const GICR_TYPER: usize = 0x0008;
const GICR_WAKER: usize = 0x0014;
const SGI_FRAME: usize = 0x1_0000;
const WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;
const GICR_CTLR_RWP: u32 = 1 << 3;

/// Virtual address of the distributor and the redistributor region, set by [`init`].
static GICD: AtomicUsize = AtomicUsize::new(0);
static GICR: AtomicUsize = AtomicUsize::new(0);
static GICR_SIZE: AtomicUsize = AtomicUsize::new(0);
/// Each CPU's redistributor control frame (virtual), 0 until that CPU is set up.
static RD: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
/// Interrupt lines the distributor implements.
static LINES: AtomicU32 = AtomicU32::new(0);

macro_rules! sysreg_r {
    ($name:literal) => {{
        let v: u64;
        // SAFETY: a CPU interface system register read.
        unsafe { core::arch::asm!(concat!("mrs {}, ", $name), out(reg) v, options(nomem, nostack)) };
        v
    }};
}
macro_rules! sysreg_w {
    ($name:literal, $v:expr) => {{
        let v: u64 = $v;
        // SAFETY: a CPU interface system register write.
        unsafe { core::arch::asm!(concat!("msr ", $name, ", {}"), in(reg) v, options(nomem, nostack)) };
    }};
}

fn read_sre() -> u64 {
    sysreg_r!("S3_0_C12_C12_5")
}
fn write_sre(v: u64) {
    sysreg_w!("S3_0_C12_C12_5", v);
    // SAFETY: barrier.
    unsafe { core::arch::asm!("isb", options(nomem, nostack, preserves_flags)) };
}

fn rd32(a: usize) -> u32 {
    // SAFETY: a register of the GIC, inside the Device mapping (checked by `init`).
    unsafe { read_volatile(a as *const u32) }
}
fn wr32(a: usize, v: u32) {
    // SAFETY: as `rd32`.
    unsafe { write_volatile(a as *mut u32, v) }
}
fn rd64(a: usize) -> u64 {
    // SAFETY: as `rd32`; 8-byte aligned.
    unsafe { read_volatile(a as *const u64) }
}
fn wr64(a: usize, v: u64) {
    // SAFETY: as `rd32`; 8-byte aligned.
    unsafe { write_volatile(a as *mut u64, v) }
}
fn rd8(a: usize) -> u8 {
    // SAFETY: as `rd32`.
    unsafe { read_volatile(a as *const u8) }
}
fn wr8(a: usize, v: u8) {
    // SAFETY: as `rd32`.
    unsafe { write_volatile(a as *mut u8, v) }
}

/// Poll `done` for up to `ms` milliseconds on the counter (a few thousand spins if the counter has no
/// frequency yet). True if it became true.
fn wait(ms: u64, mut done: impl FnMut() -> bool) -> bool {
    let f = timer::frequency();
    if f == 0 {
        for _ in 0..200_000 {
            if done() {
                return true;
            }
            core::hint::spin_loop();
        }
        return done();
    }
    let end = timer::counter() + f / 1000 * ms;
    loop {
        if done() {
            return true;
        }
        if timer::counter() >= end {
            return done();
        }
        core::hint::spin_loop();
    }
}

fn this_mpidr() -> u64 {
    sysreg_r!("mpidr_el1")
}

fn gicd() -> usize {
    GICD.load(Ordering::Relaxed)
}

/// The frame that holds interrupt `irq`'s per-interrupt registers: this CPU's SGI/PPI frame below 32, the
/// distributor above. `None` before the CPU's redistributor is set up.
fn frame(irq: u32) -> Option<usize> {
    if irq < 32 {
        match RD[percpu::id()].load(Ordering::Relaxed) {
            0 => None,
            rd => Some(rd + SGI_FRAME),
        }
    } else {
        Some(gicd())
    }
}

/// What the tree says about the GIC: `(distributor, its size, redistributor region, its size)`.
fn locate(dtb: Option<&Dtb>) -> Result<(usize, usize, usize, usize), &'static str> {
    let d = dtb.ok_or("no device tree")?;
    let mut reg = [(0u64, 0u64); 4];
    if d.node_reg_compat(b"arm,gic-v3", &mut reg) < 2 {
        return Err("the tree has no arm,gic-v3 node with two reg ranges");
    }
    let (lo, hi) = crate::platform::CURRENT.device.first().copied().unwrap_or((0, 0));
    let inside = |(a, s): (u64, u64)| s != 0 && a >= lo as u64 && a.checked_add(s).is_some_and(|e| e <= hi as u64);
    if !inside(reg[0]) || !inside(reg[1]) {
        return Err("the GIC is outside the mapped device range");
    }
    Ok((reg[0].0 as usize, reg[0].1 as usize, reg[1].0 as usize, reg[1].1 as usize))
}

/// Bring up the distributor and the boot CPU's redistributor and CPU interface. Returns the number of interrupt
/// lines. All interrupts are disabled afterwards, at the default priority, routed to this CPU.
pub fn init() -> Result<u32, &'static str> {
    // SAFETY: the bootloader's pointer, or 0.
    let dtb = unsafe { Dtb::from_addr(crate::mem::boot_dtb_ptr()) };
    let (gicd_pa, _, gicr_pa, gicr_size) = locate(dtb.as_ref())?;
    let d = phys_to_virt(gicd_pa);
    GICD.store(d, Ordering::Relaxed);
    GICR.store(phys_to_virt(gicr_pa), Ordering::Relaxed);
    GICR_SIZE.store(gicr_size, Ordering::Relaxed);

    let rev = gic_arch_rev(rd32(d + GICD_PIDR2));
    if rev < 3 {
        return Err("the interrupt controller is not a GICv3");
    }
    // 1. The system register interface.
    write_sre(read_sre() | 1);
    if read_sre() & 1 == 0 {
        return Err("ICC_SRE_EL1.SRE will not turn on (the monitor does not allow the system register interface)");
    }
    // 2. The distributor.
    let typer = decode_gicd_typer(rd32(d + GICD_TYPER));
    let lines = typer.lines.min(crate::gic::MAX_IRQS as u32);
    LINES.store(typer.lines, Ordering::Relaxed);
    wr32(d + GICD_CTLR, 0);
    if !wait(100, || rd32(d + GICD_CTLR) & CTLR_RWP == 0) {
        return Err("GICD_CTLR.RWP stays set after the distributor was switched off");
    }
    for n in 2..(lines as usize).div_ceil(16) {
        wr32(d + GICD_ICFGR + n * 4, 0); // level triggered
    }
    for n in 8..(lines as usize).div_ceil(4) {
        wr32(d + GICD_IPRIORITYR + n * 4, u32::from_ne_bytes([GIC_DEFAULT_PRIORITY; 4]));
    }
    for n in 1..(lines as usize).div_ceil(32) {
        // Non-secure Group 1 (matters only when the GIC has a single security state, as qemu's; with two states
        // a non-secure write cannot change what the secure world assigned).
        wr32(d + GICD_IGROUPR + n * 4, !0);
        wr32(d + GICD_ICENABLER + n * 4, !0);
        wr32(d + GICD_ICACTIVER + n * 4, !0);
        wr32(d + GICD_ICPENDR + n * 4, !0);
    }
    if !wait(100, || rd32(d + GICD_CTLR) & CTLR_RWP == 0) {
        return Err("GICD_CTLR.RWP stays set after the SPIs were configured");
    }
    wr32(d + GICD_CTLR, CTLR_ARE_NS | CTLR_ENABLE_G1A | CTLR_ENABLE_G1);
    if !wait(100, || rd32(d + GICD_CTLR) & CTLR_RWP == 0) {
        return Err("GICD_CTLR.RWP stays set after the distributor was switched on");
    }
    let ctlr = rd32(d + GICD_CTLR);
    if ctlr & CTLR_ARE_NS == 0 {
        return Err("GICD_CTLR.ARE_NS will not turn on (affinity routing)");
    }
    // Every SPI to the boot CPU.
    let route = irouter(this_mpidr());
    for irq in 32..lines as usize {
        wr64(d + GICD_IROUTER + irq * 8, route);
    }
    // 3 and 4. This CPU.
    init_cpu_checked()?;
    crate::kprintln!(
        "[gic] v3 up: {} interrupt IDs ({} implemented), GICD_CTLR {:#x}, route {:#x}",
        lines,
        typer.lines,
        ctlr,
        route
    );
    Ok(lines)
}

/// This CPU's redistributor, found by affinity, woken, and the CPU interface switched on.
fn init_cpu_checked() -> Result<(), &'static str> {
    let cpu = percpu::id();
    let mpidr = this_mpidr();
    let gicr = GICR.load(Ordering::Relaxed);
    let size = GICR_SIZE.load(Ordering::Relaxed);
    let mut frames = [Redist::default(); 32];
    let n = redistributors(size, |off| rd64(gicr + off + GICR_TYPER), &mut frames);
    let Some(me) = find_redist(&frames[..n], mpidr_affinity(mpidr)) else {
        return Err("no redistributor has this CPU's affinity");
    };
    let rd = gicr + me.offset;
    RD[cpu].store(rd, Ordering::Relaxed);
    // Wake it.
    wr32(rd + GICR_WAKER, rd32(rd + GICR_WAKER) & !WAKER_PROCESSOR_SLEEP);
    if rd32(rd + GICR_WAKER) & WAKER_PROCESSOR_SLEEP == 0
        && !wait(200, || rd32(rd + GICR_WAKER) & WAKER_CHILDREN_ASLEEP == 0)
    {
        return Err("GICR_WAKER.ChildrenAsleep stays set (the redistributor does not wake)");
    }
    // SGIs and PPIs: all off, nothing pending or active, default priority.
    let s = rd + SGI_FRAME;
    wr32(s + GICD_IGROUPR, !0); // SGIs and PPIs: Group 1 (see the distributor)
    wr32(s + GICD_ICENABLER, !0);
    wr32(s + GICD_ICACTIVER, !0);
    wr32(s + GICD_ICPENDR, !0);
    for n in 0..8 {
        wr32(s + GICD_IPRIORITYR + n * 4, u32::from_ne_bytes([GIC_DEFAULT_PRIORITY; 4]));
    }
    if !wait(100, || rd32(rd + GICR_CTLR) & GICR_CTLR_RWP == 0) {
        return Err("GICR_CTLR.RWP stays set after the SGIs and PPIs were configured");
    }
    // The CPU interface.
    write_sre(read_sre() | 1);
    if read_sre() & 1 == 0 {
        return Err("ICC_SRE_EL1.SRE will not turn on on this CPU");
    }
    sysreg_w!("S3_0_C4_C6_0", GIC_PMR); // ICC_PMR_EL1
    let ctlr = sysreg_r!("S3_0_C12_C12_4"); // ICC_CTLR_EL1
    sysreg_w!("S3_0_C12_C12_4", ctlr & !(1 << 1)); // EOImode 0: EOIR drops the priority and deactivates
    sysreg_w!("S3_0_C12_C12_7", 1); // ICC_IGRPEN1_EL1
    // SAFETY: barrier.
    unsafe { core::arch::asm!("isb", options(nomem, nostack, preserves_flags)) };
    Ok(())
}

/// Bring up a secondary CPU's redistributor and CPU interface (the boot CPU's was done by [`init`]).
pub fn init_cpu() {
    if let Err(why) = init_cpu_checked() {
        // Nowhere to show it from a secondary: a CPU without interrupts stays parked by its caller's timeout.
        crate::kprintln!("[gic] cpu {}: {why}", percpu::id());
    }
}

/// Number of lines the distributor implements (before the handler table's limit).
pub fn lines() -> u32 {
    LINES.load(Ordering::Relaxed)
}

/// The redistributor of CPU `cpu` (virtual address of its control frame), if it is set up.
pub fn redistributor(cpu: usize) -> Option<usize> {
    match RD.get(cpu)?.load(Ordering::Relaxed) {
        0 => None,
        a => Some(a),
    }
}

pub fn enable(irq: u32) {
    if let Some(f) = frame(irq) {
        wr32(f + GICD_ISENABLER + (irq as usize / 32) * 4, 1 << (irq % 32));
    }
}

pub fn disable(irq: u32) {
    if let Some(f) = frame(irq) {
        wr32(f + GICD_ICENABLER + (irq as usize / 32) * 4, 1 << (irq % 32));
    }
}

pub fn is_enabled(irq: u32) -> bool {
    frame(irq).is_some_and(|f| rd32(f + GICD_ISENABLER + (irq as usize / 32) * 4) & (1 << (irq % 32)) != 0)
}

pub fn set_priority(irq: u32, prio: u8) {
    if let Some(f) = frame(irq) {
        wr8(f + GICD_IPRIORITYR + irq as usize, prio);
    }
}

pub fn priority(irq: u32) -> u8 {
    frame(irq).map_or(0, |f| rd8(f + GICD_IPRIORITYR + irq as usize))
}

pub fn set_edge_triggered(irq: u32, edge: bool) {
    let Some(f) = frame(irq) else { return };
    let off = GICD_ICFGR + (irq as usize / 16) * 4;
    let shift = (irq % 16) * 2 + 1;
    let mut v = rd32(f + off);
    if edge {
        v |= 1 << shift;
    } else {
        v &= !(1 << shift);
    }
    wr32(f + off, v);
}

pub fn set_pending(irq: u32) {
    if let Some(f) = frame(irq) {
        wr32(f + GICD_ISPENDR + (irq as usize / 32) * 4, 1 << (irq % 32));
    }
}

pub fn clear_pending(irq: u32) {
    if let Some(f) = frame(irq) {
        wr32(f + GICD_ICPENDR + (irq as usize / 32) * 4, 1 << (irq % 32));
    }
}

/// Is `irq` pending in this CPU's redistributor (below 32) or the distributor?
pub fn is_pending(irq: u32) -> bool {
    frame(irq).is_some_and(|f| rd32(f + GICD_ISPENDR + (irq as usize / 32) * 4) & (1 << (irq % 32)) != 0)
}

/// This CPU's pending SGIs and PPIs as a bit mask (`GICR_ISPENDR0`).
pub fn pending_private() -> u32 {
    frame(0).map_or(0, |f| rd32(f + GICD_ISPENDR))
}

/// The highest priority pending interrupt of group 1 for this CPU (`ICC_HPPIR1_EL1`), or 1023.
pub fn highest_pending() -> u32 {
    sysreg_r!("S3_0_C12_C12_2") as u32 & 0xff_ffff
}

/// Send SPI `irq` to CPU `cpu` (its `MPIDR` from `percpu`).
pub fn route(irq: u32, cpu: usize) {
    if irq < 32 || irq >= lines() {
        return;
    }
    let mpidr = percpu::get(cpu).mpidr.load(Ordering::Relaxed);
    wr64(gicd() + GICD_IROUTER + irq as usize * 8, irouter(mpidr));
}

/// Which CPU (our numbering) SPI `irq` goes to: its `GICD_IROUTER` against every online CPU's affinity.
pub fn routed_to(irq: u32) -> u32 {
    if irq < 32 {
        return percpu::online().fold(0, |m, c| m | 1 << c);
    }
    let r = rd64(gicd() + GICD_IROUTER + irq as usize * 8);
    percpu::online()
        .filter(|&c| irouter(percpu::get(c).mpidr.load(Ordering::Relaxed)) == r & 0xff_00ff_ffff)
        .fold(0, |m, c| m | 1 << c)
}

/// Raise SGI `sgi` on each CPU in `cpu_mask` (bit n = our CPU n).
pub fn send_sgi(sgi: u32, cpu_mask: u8) {
    // SAFETY: barrier: the data the other CPU will look at is visible before the interrupt.
    unsafe { core::arch::asm!("dsb ishst", options(nostack, preserves_flags)) };
    for cpu in 0..MAX_CPUS {
        if cpu_mask & (1 << cpu) == 0 {
            continue;
        }
        if let Some(v) = sgi1r(percpu::get(cpu).mpidr.load(Ordering::Relaxed), sgi) {
            sysreg_w!("S3_0_C12_C11_5", v); // ICC_SGI1R_EL1
        }
    }
    // SAFETY: barrier.
    unsafe { core::arch::asm!("isb", options(nomem, nostack, preserves_flags)) };
}

/// Acknowledge the highest priority pending interrupt: the raw `ICC_IAR1_EL1` value (INTID in the low 24 bits;
/// 1020 to 1023 are special: nothing to handle).
pub fn ack() -> u32 {
    let v = sysreg_r!("S3_0_C12_C12_0") as u32;
    // SAFETY: barrier.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
    v
}

/// End of interrupt for what [`ack`] returned.
pub fn eoi(iar: u32) {
    sysreg_w!("S3_0_C12_C12_1", iar as u64); // ICC_EOIR1_EL1
    // SAFETY: barrier.
    unsafe { core::arch::asm!("isb", options(nomem, nostack, preserves_flags)) };
}
