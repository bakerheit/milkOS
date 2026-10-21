//! ARM generic timer: monotonic clock, 100 Hz tick, busy delays and a one-shot deadline.
//!
//! Uses the EL1 *virtual* timer (CNTV_*), PPI 11 = INTID 27 on qemu virt. The task brief
//! asked for the physical timer (CNTP_*, PPI 14 = INTID 30), but under `-accel hvf` on Apple
//! silicon the guest's CNTP_* system registers trap and qemu reflects them back as an
//! "unknown reason" undefined-instruction exception (seen in testing: `msr cntv_cval_el0`
//! faults with EC=0x00). The virtual timer works under both HVF and TCG, and is what Linux
//! uses for an EL1-only guest too. One hardware comparator serves both the periodic tick and the one-shot deadline: after every
//! interrupt it is programmed to whichever of the two is sooner.
//!
//! Time base: the counter value at [`init`] is time zero, so [`uptime`] / [`now_ns`] count
//! from timer init (the raw counter on HVF is the host's, which does not start at 0).

// Public API surface for later subsystems (scheduler, drivers); not all of it is used yet.
#![allow(dead_code)]

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

use crate::exceptions::TrapFrame;
use crate::sync::{FnSlot, IrqGuard};
use crate::{gic, kprintln};

/// Scheduler tick rate.
pub const TICK_HZ: u64 = 100;
/// EL1 virtual timer, PPI 11 in the device tree.
pub const TIMER_IRQ: u32 = gic::ppi(11);

const NO_DEADLINE: u64 = u64::MAX;

// All of this is written either at init or from the timer IRQ / under IrqGuard on a single
// core, so plain load/store is enough (see sync.rs).
static FREQ: AtomicU64 = AtomicU64::new(0);
static BOOT_COUNT: AtomicU64 = AtomicU64::new(0);
static TICK_PERIOD: AtomicU64 = AtomicU64::new(0);
static NEXT_TICK: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
/// Absolute counter value of the pending one-shot deadline, or NO_DEADLINE.
static DEADLINE: AtomicU64 = AtomicU64::new(NO_DEADLINE);

/// Called on every tick (from IRQ context, IRQs masked). The scheduler's preemption point:
/// it may rewrite `frame` or `restore_frame` another one.
pub type TickHook = fn(&mut TrapFrame);
/// Called when the one-shot deadline expires (IRQ context). Disarmed before it runs, so it
/// can re-arm itself.
pub type DeadlineCallback = fn(&mut TrapFrame);

static TICK_HOOK: FnSlot = FnSlot::new();
static DEADLINE_CB: FnSlot = FnSlot::new();

// ---- raw counter access ------------------------------------------------------------

/// Raw virtual counter (CNTVCT_EL0). `isb` first so the read is not speculated early.
#[inline]
pub fn counter() -> u64 {
    let v: u64;
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack)) };
    v
}

/// Counter frequency in Hz (CNTFRQ_EL0; 24 MHz on HVF, 62.5 MHz under TCG).
#[inline]
pub fn frequency() -> u64 {
    FREQ.load(Ordering::Relaxed)
}

fn read_cntfrq() -> u64 {
    let v: u64;
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) v, options(nomem, nostack)) };
    v
}

fn set_cval(v: u64) {
    unsafe { asm!("msr cntv_cval_el0, {}", "isb", in(reg) v, options(nomem, nostack)) };
}

fn set_ctl(v: u64) {
    unsafe { asm!("msr cntv_ctl_el0, {}", "isb", in(reg) v, options(nomem, nostack)) };
}

// ---- conversions (u64 only, no 128-bit division) ------------------------------------

fn ticks_to_ns(t: u64) -> u64 {
    let f = frequency();
    (t / f) * 1_000_000_000 + (t % f) * 1_000_000_000 / f
}

fn ns_to_ticks(ns: u64) -> u64 {
    let f = frequency();
    (ns / 1_000_000_000) * f + (ns % 1_000_000_000) * f / 1_000_000_000
}

// ---- clock -------------------------------------------------------------------------

/// Nanoseconds since [`init`]. Monotonic.
pub fn now_ns() -> u64 {
    ticks_to_ns(counter().wrapping_sub(BOOT_COUNT.load(Ordering::Relaxed)))
}

/// Time since [`init`].
pub fn uptime() -> Duration {
    Duration::from_nanos(now_ns())
}

/// Scheduler ticks since [`init`] (increments at [`TICK_HZ`]).
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Busy-wait for `us` microseconds (works with IRQs masked; does not need the tick).
pub fn delay_us(us: u64) {
    let target = counter().wrapping_add(ns_to_ticks(us.saturating_mul(1000)));
    while (counter().wrapping_sub(target) as i64) < 0 {
        core::hint::spin_loop();
    }
}

/// Busy-wait for `ms` milliseconds.
pub fn delay_ms(ms: u64) {
    delay_us(ms.saturating_mul(1000));
}

// ---- tick hook and one-shot deadline -------------------------------------------------

/// Install the per-tick hook (replaces any previous one).
pub fn set_tick_hook(h: TickHook) {
    TICK_HOOK.set(h);
}

/// Arm the one-shot deadline: call `cb` from IRQ context once `now_ns()` reaches `at_ns`.
/// Replaces any pending deadline. A time in the past fires as soon as IRQs are unmasked.
pub fn set_deadline(at_ns: u64, cb: DeadlineCallback) {
    let _g = IrqGuard::new();
    let at = BOOT_COUNT
        .load(Ordering::Relaxed)
        .wrapping_add(ns_to_ticks(at_ns));
    DEADLINE_CB.set(cb);
    DEADLINE.store(at, Ordering::Release);
    reprogram();
}

/// Like [`set_deadline`] but relative to now.
pub fn set_timeout_ns(after_ns: u64, cb: DeadlineCallback) {
    set_deadline(now_ns().saturating_add(after_ns), cb);
}

/// Disarm the one-shot deadline, if any.
pub fn cancel_deadline() {
    let _g = IrqGuard::new();
    DEADLINE.store(NO_DEADLINE, Ordering::Release);
    reprogram();
}

/// Absolute `now_ns()`-style time of the pending deadline, if one is armed.
pub fn pending_deadline_ns() -> Option<u64> {
    let d = DEADLINE.load(Ordering::Acquire);
    (d != NO_DEADLINE).then(|| ticks_to_ns(d.wrapping_sub(BOOT_COUNT.load(Ordering::Relaxed))))
}

/// Program the comparator for whichever of next-tick and deadline is sooner.
/// Caller must have IRQs masked (or be the IRQ handler).
fn reprogram() {
    let next = NEXT_TICK.load(Ordering::Relaxed);
    let dl = DEADLINE.load(Ordering::Relaxed);
    let target = if dl != NO_DEADLINE && (dl.wrapping_sub(next) as i64) < 0 {
        dl
    } else {
        next
    };
    set_cval(target);
    set_ctl(1); // ENABLE, unmasked
}

fn on_irq(_irq: u32, frame: &mut TrapFrame) {
    let now = counter();

    let mut tick = false;
    let next = NEXT_TICK.load(Ordering::Relaxed);
    if (now.wrapping_sub(next) as i64) >= 0 {
        tick = true;
        TICKS.store(TICKS.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        let period = TICK_PERIOD.load(Ordering::Relaxed);
        let mut n = next.wrapping_add(period);
        if (now.wrapping_sub(n) as i64) >= 0 {
            n = now.wrapping_add(period); // fell far behind (e.g. long IRQs-off): skip, don't burst
        }
        NEXT_TICK.store(n, Ordering::Relaxed);
    }

    let mut deadline_hit = false;
    let dl = DEADLINE.load(Ordering::Relaxed);
    if dl != NO_DEADLINE && (now.wrapping_sub(dl) as i64) >= 0 {
        deadline_hit = true;
        DEADLINE.store(NO_DEADLINE, Ordering::Relaxed);
    }

    // Reprogram first: this drops the interrupt condition before EOI, and lets the hooks
    // below re-arm or switch contexts freely.
    reprogram();

    if deadline_hit && let Some(cb) = DEADLINE_CB.get::<DeadlineCallback>() {
        cb(frame);
    }
    if tick && let Some(h) = TICK_HOOK.get::<TickHook>() {
        h(frame);
    }
}

/// Set up the clock, hook the timer PPI and start the 100 Hz tick. The GIC must already be
/// initialised; IRQs still have to be unmasked by the caller.
pub fn init() {
    let f = read_cntfrq();
    assert!(f != 0, "CNTFRQ_EL0 is zero");
    FREQ.store(f, Ordering::Relaxed);
    let now = counter();
    BOOT_COUNT.store(now, Ordering::Relaxed);
    let period = f / TICK_HZ;
    TICK_PERIOD.store(period, Ordering::Relaxed);
    NEXT_TICK.store(now + period, Ordering::Relaxed);
    DEADLINE.store(NO_DEADLINE, Ordering::Relaxed);

    gic::register(TIMER_IRQ, on_irq).expect("timer IRQ registration");
    // Level-triggered PPI: ISTATUS stays asserted until the comparator is moved.
    gic::set_edge_triggered(TIMER_IRQ, false).ok();
    gic::enable(TIMER_IRQ).expect("timer IRQ enable");

    let _g = IrqGuard::new();
    reprogram();
    kprintln!(
        "[timer] CNTFRQ {} Hz, tick {} Hz (INTID {})",
        f,
        TICK_HZ,
        TIMER_IRQ
    );
}
