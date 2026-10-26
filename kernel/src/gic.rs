//! GICv2 driver for qemu's `virt` machine (distributor 0x0800_0000, CPU interface
//! 0x0801_0000) and the Raspberry Pi 4's GIC-400 (0xff84_1000 / 0xff84_2000; platform.rs has
//! both; the register layout is the same).
//!
//! Confirmed with `-M virt -accel hvf -cpu host` on qemu 10.2: the default GIC is v2
//! (DTB node `intc@8000000`, compatible `arm,cortex-a15-gic`), so `scripts/run.sh` needs no
//! `gic-version` option. [`init`] checks the GICD_PIDR2 architecture revision and panics
//! with a clear message if a different GIC shows up.
//!
//! IRQ numbers here are GIC interrupt IDs (INTIDs): SGI 0-15, PPI 16-31, SPI 32+. Use
//! [`spi`] / [`ppi`] to convert from the "SPI n" / "PPI n" numbers in device trees. Handlers
//! are attached by INTID with [`register`]; the IRQ vector calls [`handle_irq`].
//!
//! The Pixel 2 build (feature `msm8998`) has a GICv3: everything that touches a register goes to
//! `gic3.rs` there (distributor, redistributors, the CPU interface's system registers), while the handler
//! table, the counters and the numbering stay here.

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

use crate::exceptions::TrapFrame;
use crate::kprintln;
use crate::percpu::{self, MAX_CPUS};
use crate::sync::{FnSlot, IrqGuard};

const GICD_BASE: usize = crate::mmu::phys_to_virt(crate::platform::CURRENT.gicd);
const GICC_BASE: usize = crate::mmu::phys_to_virt(crate::platform::CURRENT.gicc);

// Distributor registers.
const GICD_CTLR: usize = 0x000;
const GICD_TYPER: usize = 0x004;
const GICD_ISENABLER: usize = 0x100;
const GICD_ICENABLER: usize = 0x180;
const GICD_ISPENDR: usize = 0x200;
const GICD_ICPENDR: usize = 0x280;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;
const GICD_ICFGR: usize = 0xc00;
const GICD_SGIR: usize = 0xf00;
const GICD_PIDR2: usize = 0xfe8;

// CPU interface registers.
const GICC_CTLR: usize = 0x00;
const GICC_PMR: usize = 0x04;
const GICC_BPR: usize = 0x08;
const GICC_IAR: usize = 0x0c;
const GICC_EOIR: usize = 0x10;

/// Highest INTID we keep a handler slot for (qemu virt exposes 256 by default, up to 288).
pub const MAX_IRQS: usize = 288;
/// Priority every interrupt starts at. Lower value = more urgent.
pub const DEFAULT_PRIORITY: u8 = 0xa0;

/// An INTID for "SPI n" (shared peripheral interrupt, as written in device trees).
pub const fn spi(n: u32) -> u32 {
    n + 32
}

/// An INTID for "PPI n" (private peripheral interrupt, as written in device trees).
pub const fn ppi(n: u32) -> u32 {
    n + 16
}

/// Interrupt handler. Runs in IRQ context with IRQs masked; for level-triggered sources it
/// must clear the cause before returning. `frame` is the interrupted context (a scheduler
/// may modify it or switch to another one).
pub type Handler = fn(irq: u32, frame: &mut TrapFrame);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// INTID out of range for this GIC.
    BadIrq,
    /// A handler is already registered for this INTID.
    Busy,
}

static HANDLERS: [FnSlot; MAX_IRQS] = [const { FnSlot::new() }; MAX_IRQS];
/// Each CPU's GIC CPU-interface mask (what ITARGETSR0 reads on that CPU).
static CPU_IF_MASK: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
/// SPIs are spread over the online CPUs (after [`spread_spis`]); else all go to CPU 0.
static SPREAD: AtomicBool = AtomicBool::new(false);
/// Round-robin cursor for the next SPI's CPU.
static NEXT_CPU: AtomicU32 = AtomicU32::new(0);
// Interrupt counters, per CPU (atomic adds: several CPUs take interrupts).
static SPURIOUS: AtomicU64 = AtomicU64::new(0);
static NUM_IRQS: AtomicU32 = AtomicU32::new(0);
/// Times each INTID has been taken (for /proc/interrupts). Written only by `handle_irq`.
static COUNTS: [[AtomicU64; MAX_IRQS]; MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; MAX_IRQS] }; MAX_CPUS];

#[inline]
fn gicd_read(off: usize) -> u32 {
    unsafe { read_volatile((GICD_BASE + off) as *const u32) }
}
#[inline]
fn gicd_write(off: usize, v: u32) {
    unsafe { write_volatile((GICD_BASE + off) as *mut u32, v) }
}
#[inline]
fn gicd_write8(off: usize, v: u8) {
    unsafe { write_volatile((GICD_BASE + off) as *mut u8, v) }
}
#[inline]
fn gicd_read8(off: usize) -> u8 {
    unsafe { read_volatile((GICD_BASE + off) as *const u8) }
}
#[inline]
fn gicc_read(off: usize) -> u32 {
    unsafe { read_volatile((GICC_BASE + off) as *const u32) }
}
#[inline]
fn gicc_write(off: usize, v: u32) {
    unsafe { write_volatile((GICC_BASE + off) as *mut u32, v) }
}

/// Number of interrupt IDs this GIC implements.
pub fn num_irqs() -> u32 {
    NUM_IRQS.load(Ordering::Relaxed)
}

fn check(irq: u32) -> Result<(), Error> {
    if (irq as usize) < MAX_IRQS && irq < num_irqs() {
        Ok(())
    } else {
        Err(Error::BadIrq)
    }
}

/// Bring up the distributor and this core's CPU interface. All interrupts end up disabled,
/// at [`DEFAULT_PRIORITY`], targeted at the calling CPU, until a driver enables them.
pub fn init() {
    #[cfg(feature = "msm8998")]
    let _ = init_v3();
    #[cfg(not(feature = "msm8998"))]
    init_v2();
}

/// The Pixel 2: the GICv3 (kernel/src/gic3.rs). `Err` names the register that disagreed; the caller shows it
/// and carries on without interrupts.
#[cfg(feature = "msm8998")]
pub fn init_v3() -> Result<(), &'static str> {
    match crate::gic3::init() {
        Ok(lines) => {
            NUM_IRQS.store(lines.min(MAX_IRQS as u32), Ordering::Relaxed);
            CPU_IF_MASK[0].store(1, Ordering::Relaxed);
            Ok(())
        }
        Err(why) => {
            kprintln!("[gic] GICv3 setup failed: {why}");
            Err(why)
        }
    }
}

#[cfg(not(feature = "msm8998"))]
fn init_v2() {
    let arch = (gicd_read(GICD_PIDR2) >> 4) & 0xf;
    if arch != 2 {
        panic!(
            "unsupported GIC architecture revision {arch} (expected GICv2; \
             run qemu with -M virt,gic-version=2)"
        );
    }
    let lines = ((gicd_read(GICD_TYPER) & 0x1f) + 1) * 32;
    NUM_IRQS.store(lines.min(MAX_IRQS as u32), Ordering::Relaxed);

    gicd_write(GICD_CTLR, 0);

    // Banked SGI/PPI state (0..31) for this CPU, then the shared SPIs.
    // ITARGETSR0..7 are read-only and report this CPU's mask. A single-CPU GIC (qemu with
    // -smp 1) makes the whole register file RAZ/WI, so fall back to CPU 0.
    let cpu_mask = match (gicd_read(GICD_ITARGETSR) & 0xff) as u8 {
        0 => 1,
        m => m,
    };
    CPU_IF_MASK[0].store(cpu_mask, Ordering::Relaxed);
    for n in 0..lines.div_ceil(32) as usize {
        gicd_write(GICD_ICENABLER + n * 4, !0);
        gicd_write(GICD_ICPENDR + n * 4, !0);
    }
    for n in 0..lines as usize / 4 {
        gicd_write(
            GICD_IPRIORITYR + n * 4,
            u32::from_ne_bytes([DEFAULT_PRIORITY; 4]),
        );
    }
    for n in 8..lines as usize / 4 {
        gicd_write(GICD_ITARGETSR + n * 4, u32::from_ne_bytes([cpu_mask; 4]));
    }

    gicd_write(GICD_CTLR, 1);

    gicc_write(GICC_PMR, 0xff); // let every priority through
    gicc_write(GICC_BPR, 0);
    gicc_write(GICC_CTLR, 1);
    kprintln!(
        "[gic] v2 up: {} interrupt IDs, boot cpu mask {:#x}",
        lines,
        cpu_mask
    );
}

/// Bring up this (secondary) CPU's side of the GIC: its banked SGI/PPI state (enables,
/// priorities) and its CPU interface. SGIs and PPIs that have a handler are enabled here
/// too, since `enable` on the boot CPU only reached the boot CPU's banked copy.
pub fn init_cpu() {
    #[cfg(feature = "msm8998")]
    {
        CPU_IF_MASK[percpu::id()].store(1 << percpu::id(), Ordering::Relaxed);
        crate::gic3::init_cpu();
        // This CPU's SGIs and PPIs that have a handler (enable on the boot CPU reached only its own copy).
        for (irq, h) in HANDLERS.iter().take(32).enumerate() {
            if h.get::<Handler>().is_some() {
                crate::gic3::enable(irq as u32);
            }
        }
        return;
    }
    #[cfg(not(feature = "msm8998"))]
    init_cpu_v2();
}

#[cfg(not(feature = "msm8998"))]
fn init_cpu_v2() {
    let m = (gicd_read(GICD_ITARGETSR) & 0xff) as u8;
    CPU_IF_MASK[percpu::id()].store(
        if m == 0 { 1 << percpu::id() } else { m },
        Ordering::Relaxed,
    );
    gicd_write(GICD_ICENABLER, !0);
    gicd_write(GICD_ICPENDR, !0);
    for n in 0..8 {
        gicd_write(
            GICD_IPRIORITYR + n * 4,
            u32::from_ne_bytes([DEFAULT_PRIORITY; 4]),
        );
    }
    let mut en = 0u32;
    for (irq, h) in HANDLERS.iter().take(32).enumerate() {
        if h.get::<Handler>().is_some() {
            en |= 1 << irq;
        }
    }
    gicd_write(GICD_ISENABLER, en);
    gicc_write(GICC_PMR, 0xff);
    gicc_write(GICC_BPR, 0);
    gicc_write(GICC_CTLR, 1);
}

/// Attach `handler` to `irq`. Does not enable the interrupt; call [`enable`].
pub fn register(irq: u32, handler: Handler) -> Result<(), Error> {
    check(irq)?;
    let _g = IrqGuard::new();
    let slot = &HANDLERS[irq as usize];
    if slot.get::<Handler>().is_some() {
        return Err(Error::Busy);
    }
    slot.set(handler);
    if irq >= 32 && SPREAD.load(Ordering::Relaxed) {
        let _ = set_target(irq, next_target());
    }
    Ok(())
}

/// GIC CPU-interface mask of the next online CPU in round-robin order.
fn next_target() -> u8 {
    let n = percpu::count().max(1);
    let k = NEXT_CPU.fetch_add(1, Ordering::Relaxed) as usize % n;
    let cpu = percpu::online().nth(k).unwrap_or(0);
    match CPU_IF_MASK[cpu].load(Ordering::Relaxed) {
        0 => 1,
        m => m,
    }
}

/// IRQ affinity: route every SPI that has a handler to one online CPU, round robin
/// (ITARGETSR), and keep doing that for SPIs registered later. `false` puts them all
/// back on CPU 0. Call after the secondaries are up.
pub fn spread_spis(spread: bool) {
    SPREAD.store(spread, Ordering::Relaxed);
    NEXT_CPU.store(0, Ordering::Relaxed);
    let cpu0 = CPU_IF_MASK[0].load(Ordering::Relaxed).max(1);
    for irq in 32..num_irqs() {
        if is_registered(irq) {
            let _ = set_target(irq, if spread { next_target() } else { cpu0 });
        }
    }
}

pub fn spis_spread() -> bool {
    SPREAD.load(Ordering::Relaxed)
}

/// Which CPUs (our CPU numbers) `irq` is routed to. SGIs/PPIs: every CPU (banked).
pub fn target_cpus(irq: u32) -> u32 {
    if check(irq).is_err() {
        return 0;
    }
    if irq < 32 {
        return percpu::online().fold(0, |m, c| m | 1 << c);
    }
    #[cfg(feature = "msm8998")]
    return crate::gic3::routed_to(irq);
    #[cfg(not(feature = "msm8998"))]
    let t = gicd_read8(GICD_ITARGETSR + irq as usize);
    #[cfg(not(feature = "msm8998"))]
    percpu::online()
        .filter(|&c| t & CPU_IF_MASK[c].load(Ordering::Relaxed) != 0)
        .fold(0, |m, c| m | 1 << c)
}

/// Detach whatever handler is on `irq` (and disable the interrupt first).
pub fn unregister(irq: u32) -> Result<(), Error> {
    check(irq)?;
    disable(irq)?;
    HANDLERS[irq as usize].clear();
    Ok(())
}

/// Unmask `irq` at the distributor.
pub fn enable(irq: u32) -> Result<(), Error> {
    check(irq)?;
    #[cfg(feature = "msm8998")]
    crate::gic3::enable(irq);
    #[cfg(not(feature = "msm8998"))]
    gicd_write(GICD_ISENABLER + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

/// Mask `irq` at the distributor.
pub fn disable(irq: u32) -> Result<(), Error> {
    check(irq)?;
    #[cfg(feature = "msm8998")]
    crate::gic3::disable(irq);
    #[cfg(not(feature = "msm8998"))]
    gicd_write(GICD_ICENABLER + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

/// Set the priority of `irq` (0 = most urgent). Only the top bits are implemented.
pub fn set_priority(irq: u32, prio: u8) -> Result<(), Error> {
    check(irq)?;
    #[cfg(feature = "msm8998")]
    crate::gic3::set_priority(irq, prio);
    #[cfg(not(feature = "msm8998"))]
    gicd_write8(GICD_IPRIORITYR + irq as usize, prio);
    Ok(())
}

pub fn priority(irq: u32) -> Result<u8, Error> {
    check(irq)?;
    #[cfg(feature = "msm8998")]
    return Ok(crate::gic3::priority(irq));
    #[cfg(not(feature = "msm8998"))]
    Ok(gicd_read8(GICD_IPRIORITYR + irq as usize))
}

/// Route an SPI to the CPUs in `cpu_mask` (bit n = CPU interface n). SGIs and PPIs are
/// per-CPU and fixed, so this is a no-op for INTIDs below 32.
pub fn set_target(irq: u32, cpu_mask: u8) -> Result<(), Error> {
    check(irq)?;
    if irq >= 32 {
        #[cfg(feature = "msm8998")]
        if cpu_mask != 0 {
            // GICv3: the mask is a set of our CPU numbers; the router takes the lowest.
            crate::gic3::route(irq, cpu_mask.trailing_zeros() as usize);
        }
        #[cfg(not(feature = "msm8998"))]
        gicd_write8(GICD_ITARGETSR + irq as usize, cpu_mask);
    }
    Ok(())
}

/// Choose edge (true) or level (false) triggering. Fixed for SGIs; PPIs/SPIs configurable.
pub fn set_edge_triggered(irq: u32, edge: bool) -> Result<(), Error> {
    check(irq)?;
    if irq < 16 {
        return Ok(());
    }
    #[cfg(feature = "msm8998")]
    {
        crate::gic3::set_edge_triggered(irq, edge);
        return Ok(());
    }
    #[cfg(not(feature = "msm8998"))]
    {
        let off = GICD_ICFGR + (irq as usize / 16) * 4;
        let shift = (irq % 16) * 2 + 1;
        let mut v = gicd_read(off);
        if edge {
            v |= 1 << shift;
        } else {
            v &= !(1 << shift);
        }
        gicd_write(off, v);
        Ok(())
    }
}

/// Force `irq` pending (software-raise a PPI/SPI; handy for tests).
pub fn set_pending(irq: u32) -> Result<(), Error> {
    check(irq)?;
    #[cfg(feature = "msm8998")]
    crate::gic3::set_pending(irq);
    #[cfg(not(feature = "msm8998"))]
    gicd_write(GICD_ISPENDR + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

pub fn clear_pending(irq: u32) -> Result<(), Error> {
    check(irq)?;
    #[cfg(feature = "msm8998")]
    crate::gic3::clear_pending(irq);
    #[cfg(not(feature = "msm8998"))]
    gicd_write(GICD_ICPENDR + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

/// Send software-generated interrupt `sgi` (0..15) to the CPUs in `cpu_mask`. For SMP later.
pub fn send_sgi(sgi: u32, cpu_mask: u8) {
    #[cfg(feature = "msm8998")]
    crate::gic3::send_sgi(sgi & 0xf, cpu_mask);
    #[cfg(not(feature = "msm8998"))]
    gicd_write(GICD_SGIR, ((cpu_mask as u32) << 16) | (sgi & 0xf));
}

/// Number of spurious interrupts (IAR returned 1020..1023) seen so far.
pub fn spurious_count() -> u64 {
    SPURIOUS.load(Ordering::Relaxed)
}

/// How many times `irq` has been taken since boot (0 for out-of-range INTIDs).
pub fn irq_count(irq: u32) -> u64 {
    (0..MAX_CPUS).map(|c| irq_count_cpu(irq, c)).sum()
}

/// How often `irq` was taken on `cpu`.
pub fn irq_count_cpu(irq: u32, cpu: usize) -> u64 {
    COUNTS
        .get(cpu)
        .and_then(|c| c.get(irq as usize))
        .map_or(0, |c| c.load(Ordering::Relaxed))
}

/// Is a handler registered for `irq`?
pub fn is_registered(irq: u32) -> bool {
    HANDLERS
        .get(irq as usize)
        .is_some_and(|s| s.get::<Handler>().is_some())
}

/// Called from the IRQ vector: acknowledge, run the registered handler, end the interrupt.
pub fn handle_irq(frame: &mut TrapFrame) {
    #[cfg(feature = "msm8998")]
    let (iar, irq) = {
        let iar = crate::gic3::ack();
        (iar, iar & 0xff_ffff)
    };
    #[cfg(not(feature = "msm8998"))]
    let (iar, irq) = {
        let iar = gicc_read(GICC_IAR);
        (iar, iar & 0x3ff)
    };
    if irq >= 1020 {
        SPURIOUS.fetch_add(1, Ordering::Relaxed);
        return; // spurious: no EOI
    }
    if let Some(c) = COUNTS[percpu::id()].get(irq as usize) {
        c.fetch_add(1, Ordering::Relaxed);
    }
    match HANDLERS.get(irq as usize).and_then(|s| s.get::<Handler>()) {
        Some(h) => h(irq, frame),
        None => {
            // Nobody owns it. Shut it up so it cannot storm, and say so.
            let _ = disable(irq);
            kprintln!("[gic] unhandled IRQ {irq}, disabled");
        }
    }
    // Writing the full IAR value (including the SGI source CPU bits) is required.
    #[cfg(feature = "msm8998")]
    crate::gic3::eoi(iar);
    #[cfg(not(feature = "msm8998"))]
    gicc_write(GICC_EOIR, iar);
}
