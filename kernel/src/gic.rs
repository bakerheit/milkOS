//! GICv2 driver for qemu's `virt` machine (distributor 0x0800_0000, CPU interface
//! 0x0801_0000).
//!
//! Confirmed with `-M virt -accel hvf -cpu host` on qemu 10.2: the default GIC is v2
//! (DTB node `intc@8000000`, compatible `arm,cortex-a15-gic`), so `scripts/run.sh` needs no
//! `gic-version` option. [`init`] checks the GICD_PIDR2 architecture revision and panics
//! with a clear message if a different GIC shows up.
//!
//! IRQ numbers here are GIC interrupt IDs (INTIDs): SGI 0-15, PPI 16-31, SPI 32+. Use
//! [`spi`] / [`ppi`] to convert from the "SPI n" / "PPI n" numbers in device trees. Handlers
//! are attached by INTID with [`register`]; the IRQ vector calls [`handle_irq`].

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::exceptions::TrapFrame;
use crate::kprintln;
use crate::sync::{FnSlot, IrqGuard};

const GICD_BASE: usize = 0x0800_0000;
const GICC_BASE: usize = 0x0801_0000;

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
// Single-writer counters (the IRQ path): plain load/store, see sync.rs on atomics.
static SPURIOUS: AtomicU64 = AtomicU64::new(0);
static NUM_IRQS: AtomicU32 = AtomicU32::new(0);
/// Times each INTID has been taken (for /proc/interrupts). Written only by `handle_irq`.
static COUNTS: [AtomicU64; MAX_IRQS] = [const { AtomicU64::new(0) }; MAX_IRQS];

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

/// Attach `handler` to `irq`. Does not enable the interrupt; call [`enable`].
pub fn register(irq: u32, handler: Handler) -> Result<(), Error> {
    check(irq)?;
    let _g = IrqGuard::new();
    let slot = &HANDLERS[irq as usize];
    if slot.get::<Handler>().is_some() {
        return Err(Error::Busy);
    }
    slot.set(handler);
    Ok(())
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
    gicd_write(GICD_ISENABLER + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

/// Mask `irq` at the distributor.
pub fn disable(irq: u32) -> Result<(), Error> {
    check(irq)?;
    gicd_write(GICD_ICENABLER + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

/// Set the priority of `irq` (0 = most urgent). Only the top bits are implemented.
pub fn set_priority(irq: u32, prio: u8) -> Result<(), Error> {
    check(irq)?;
    gicd_write8(GICD_IPRIORITYR + irq as usize, prio);
    Ok(())
}

pub fn priority(irq: u32) -> Result<u8, Error> {
    check(irq)?;
    Ok(gicd_read8(GICD_IPRIORITYR + irq as usize))
}

/// Route an SPI to the CPUs in `cpu_mask` (bit n = CPU interface n). SGIs and PPIs are
/// per-CPU and fixed, so this is a no-op for INTIDs below 32.
pub fn set_target(irq: u32, cpu_mask: u8) -> Result<(), Error> {
    check(irq)?;
    if irq >= 32 {
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

/// Force `irq` pending (software-raise a PPI/SPI; handy for tests).
pub fn set_pending(irq: u32) -> Result<(), Error> {
    check(irq)?;
    gicd_write(GICD_ISPENDR + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

pub fn clear_pending(irq: u32) -> Result<(), Error> {
    check(irq)?;
    gicd_write(GICD_ICPENDR + (irq as usize / 32) * 4, 1 << (irq % 32));
    Ok(())
}

/// Send software-generated interrupt `sgi` (0..15) to the CPUs in `cpu_mask`. For SMP later.
pub fn send_sgi(sgi: u32, cpu_mask: u8) {
    gicd_write(GICD_SGIR, ((cpu_mask as u32) << 16) | (sgi & 0xf));
}

/// Number of spurious interrupts (IAR returned 1020..1023) seen so far.
pub fn spurious_count() -> u64 {
    SPURIOUS.load(Ordering::Relaxed)
}

/// How many times `irq` has been taken since boot (0 for out-of-range INTIDs).
pub fn irq_count(irq: u32) -> u64 {
    COUNTS
        .get(irq as usize)
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
    let iar = gicc_read(GICC_IAR);
    let irq = iar & 0x3ff;
    if irq >= 1020 {
        SPURIOUS.store(SPURIOUS.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        return; // spurious: no EOI
    }
    if let Some(c) = COUNTS.get(irq as usize) {
        c.store(c.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
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
    gicc_write(GICC_EOIR, iar);
}
