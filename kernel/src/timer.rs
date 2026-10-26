//! ARM generic timer: monotonic clock, 100 Hz tick, busy delays and a one-shot deadline.
//!
//! Uses the EL1 *virtual* timer (CNTV_*), PPI 11 = INTID 27 on qemu virt (the Pixel 2 reads its INTID from the
//! tree and can use the physical timer instead: `select_msm`). The task brief
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
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use core::time::Duration;

use crate::exceptions::TrapFrame;
use crate::percpu::{self, MAX_CPUS};
use crate::sync::{FnSlot, IrqGuard};
use crate::{gic, kprintln};

/// Scheduler tick rate.
pub const TICK_HZ: u64 = 100;
/// EL1 virtual timer, PPI 11 in the device tree.
const DEFAULT_IRQ: u32 = gic::ppi(11);
/// The INTID of the timer in use (the Pixel 2's comes from its tree and is checked against the GIC).
static IRQ: AtomicU32 = AtomicU32::new(DEFAULT_IRQ);
/// Drive the non-secure physical timer (CNTP_*) instead of the virtual one. The counter stays the virtual one:
/// the comparator value is converted (the hypervisor may have given CNTVOFF a value).
static USE_PHYS: AtomicBool = AtomicBool::new(false);

fn irq() -> u32 {
    IRQ.load(Ordering::Relaxed)
}

const NO_DEADLINE: u64 = u64::MAX;

// FREQ, BOOT_COUNT and TICK_PERIOD are written once at init. Every CPU has its own
// comparator, so the next tick and the one-shot deadline are per CPU, touched only by that
// CPU (from its timer IRQ or with its IRQs masked).
static FREQ: AtomicU64 = AtomicU64::new(0);
static BOOT_COUNT: AtomicU64 = AtomicU64::new(0);
static TICK_PERIOD: AtomicU64 = AtomicU64::new(0);
static NEXT_TICK: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Ticks of the boot CPU (the 100 Hz clock everyone reads).
static TICKS: AtomicU64 = AtomicU64::new(0);
/// Absolute counter value of each CPU's pending one-shot deadline, or NO_DEADLINE.
static DEADLINE: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(NO_DEADLINE) }; MAX_CPUS];

fn next_tick() -> &'static AtomicU64 {
    &NEXT_TICK[percpu::id()]
}
fn deadline() -> &'static AtomicU64 {
    &DEADLINE[percpu::id()]
}

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
    if USE_PHYS.load(Ordering::Relaxed) {
        // CNTP_CVAL is compared with CNTPCT, which is CNTVCT + CNTVOFF: move the target by the difference.
        let p: u64;
        unsafe { asm!("isb", "mrs {}, cntpct_el0", out(reg) p, options(nomem, nostack)) };
        let v = p.wrapping_add(v.wrapping_sub(counter()));
        unsafe { asm!("msr cntp_cval_el0, {}", "isb", in(reg) v, options(nomem, nostack)) };
        return;
    }
    unsafe { asm!("msr cntv_cval_el0, {}", "isb", in(reg) v, options(nomem, nostack)) };
}

fn set_ctl(v: u64) {
    if USE_PHYS.load(Ordering::Relaxed) {
        unsafe { asm!("msr cntp_ctl_el0, {}", "isb", in(reg) v, options(nomem, nostack)) };
        return;
    }
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
    deadline().store(at, Ordering::Release);
    reprogram();
}

/// Like [`set_deadline`] but relative to now.
pub fn set_timeout_ns(after_ns: u64, cb: DeadlineCallback) {
    set_deadline(now_ns().saturating_add(after_ns), cb);
}

/// Disarm the one-shot deadline, if any.
pub fn cancel_deadline() {
    let _g = IrqGuard::new();
    deadline().store(NO_DEADLINE, Ordering::Release);
    reprogram();
}

/// Absolute `now_ns()`-style time of the pending deadline, if one is armed.
pub fn pending_deadline_ns() -> Option<u64> {
    let d = deadline().load(Ordering::Acquire);
    (d != NO_DEADLINE).then(|| ticks_to_ns(d.wrapping_sub(BOOT_COUNT.load(Ordering::Relaxed))))
}

/// Program the comparator for whichever of next-tick and deadline is sooner.
/// Caller must have IRQs masked (or be the IRQ handler).
fn reprogram() {
    let next = next_tick().load(Ordering::Relaxed);
    let dl = deadline().load(Ordering::Relaxed);
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
    let next = next_tick().load(Ordering::Relaxed);
    if (now.wrapping_sub(next) as i64) >= 0 {
        tick = true;
        if percpu::id() == 0 {
            TICKS.fetch_add(1, Ordering::Relaxed);
        }
        let n = percpu::this().ticks.fetch_add(1, Ordering::Relaxed) + 1;
        #[cfg(feature = "raspi")]
        crate::rpi::watchdog::on_tick();
        // Once a second on every CPU (idle ones included): a pending SError is taken here, in EL1, where the
        // dump can name the last 'tick' as the checkpoint instead of the first eret to EL0 much later.
        if n % TICK_HZ == 0 {
            crate::serror::checkpoint("tick");
        }
        let period = TICK_PERIOD.load(Ordering::Relaxed);
        let mut n = next.wrapping_add(period);
        if (now.wrapping_sub(n) as i64) >= 0 {
            n = now.wrapping_add(period); // fell far behind (e.g. long IRQs-off): skip, don't burst
        }
        next_tick().store(n, Ordering::Relaxed);
    }

    let mut deadline_hit = false;
    let dl = deadline().load(Ordering::Relaxed);
    if dl != NO_DEADLINE && (now.wrapping_sub(dl) as i64) >= 0 {
        deadline_hit = true;
        deadline().store(NO_DEADLINE, Ordering::Relaxed);
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

/// Start this (secondary) CPU's tick: its own comparator, same period. The PPI is enabled
/// for this CPU by `gic::init_cpu`.
pub fn init_cpu() {
    let _g = IrqGuard::new();
    next_tick().store(
        counter() + TICK_PERIOD.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    deadline().store(NO_DEADLINE, Ordering::Relaxed);
    reprogram();
}

/// Start the clock without the interrupt tick (no GIC set up yet, or none to use): `now_ns`,
/// `counter` and `frequency` work, nothing is hooked. The Pixel 2's stage 1 polls the counter.
/// If `CNTFRQ_EL0` is zero the Snapdragon's 19.2 MHz is assumed. Returns the frequency used.
#[cfg(feature = "msm8998")]
pub fn init_polled() -> u64 {
    let mut f = read_cntfrq();
    if f == 0 {
        f = 19_200_000;
    }
    FREQ.store(f, Ordering::Relaxed);
    BOOT_COUNT.store(counter(), Ordering::Relaxed);
    f
}

/// The raw value of `CNTFRQ_EL0` (what the firmware said, 0 if it said nothing).
#[cfg(feature = "msm8998")]
pub fn cntfrq_raw() -> u64 {
    read_cntfrq()
}

/// Set up the clock, hook the timer PPI and start the 100 Hz tick. The GIC must already be
/// initialised; IRQs still have to be unmasked by the caller.
pub fn init() {
    let mut f = read_cntfrq();
    // The Raspberry Pi's stock firmware sets CNTFRQ_EL0 (54 MHz); if some version does not, the counter
    // still runs at the crystal's 54 MHz.
    if f == 0 && crate::platform::RASPI {
        f = 54_000_000;
        kprintln!("[timer] CNTFRQ_EL0 is zero: assuming the Pi's 54 MHz");
    }
    if f == 0 {
        // The Pixel 2's init_polled assumed 19.2 MHz when the firmware said nothing.
        f = FREQ.load(Ordering::Relaxed);
    }
    assert!(f != 0, "CNTFRQ_EL0 is zero");
    FREQ.store(f, Ordering::Relaxed);
    let now = counter();
    // The Pixel 2 started the clock earlier (init_polled): its uptime does not start over.
    if BOOT_COUNT.load(Ordering::Relaxed) == 0 {
        BOOT_COUNT.store(now, Ordering::Relaxed);
    }
    let period = f / TICK_HZ;
    TICK_PERIOD.store(period, Ordering::Relaxed);
    next_tick().store(now + period, Ordering::Relaxed);
    deadline().store(NO_DEADLINE, Ordering::Relaxed);

    gic::register(irq(), on_irq).expect("timer IRQ registration");
    // Level-triggered PPI: ISTATUS stays asserted until the comparator is moved.
    gic::set_edge_triggered(irq(), false).ok();
    gic::enable(irq()).expect("timer IRQ enable");

    let _g = IrqGuard::new();
    reprogram();
    kprintln!(
        "[timer] CNTFRQ {} Hz, tick {} Hz (INTID {})",
        f,
        TICK_HZ,
        irq()
    );
}

/// What [`select_msm`] found out about one timer, for the log.
#[cfg(feature = "msm8998")]
#[derive(Clone, Copy)]
pub struct Probe {
    pub name: &'static str,
    /// The INTID the tree names for it.
    pub intid: u32,
    /// The timer's own condition (`ISTATUS`) came true within the wait.
    pub fired: bool,
    /// The private interrupts that became pending in the GIC meanwhile (bit n = INTID n), none that were already.
    pub pending: u32,
}

/// Which timer the Pixel 2's scheduler tick uses, decided by trying it: arm the comparator 1 ms ahead with the CPU's
/// interrupts masked and see whether the GIC raises the INTID the tree names (or, if it raises exactly one other,
/// which one). `want` is the order to try. The first timer that works is kept (its INTID and the choice of timer
/// registers are set for [`init`]); `Err` has what was seen of each. The GIC must be up (`gic::init`).
#[cfg(feature = "msm8998")]
pub fn select_msm(
    want: rpikit::msm::TimerSel,
    tree: rpikit::msm::TimerIntids,
    out: &mut [Option<Probe>; 2],
) -> Option<(&'static str, u32)> {
    use rpikit::msm::TimerSel;
    let virt = (false, "virtual", tree.virt.unwrap_or(gic::ppi(11)));
    let phys = (true, "physical", tree.phys.unwrap_or(gic::ppi(14)));
    let order: &[(bool, &str, u32)] = match want {
        TimerSel::Auto => &[virt, phys],
        TimerSel::Virt => &[virt],
        TimerSel::Phys => &[phys],
    };
    let f = FREQ.load(Ordering::Relaxed).max(1_000_000);
    let spin = |ms: u64, done: &mut dyn FnMut() -> bool| -> bool {
        let end = counter() + f / 1000 * ms;
        loop {
            if done() {
                return true;
            }
            if counter() >= end {
                return false;
            }
            core::hint::spin_loop();
        }
    };
    for (i, &(is_phys, name, intid)) in order.iter().enumerate() {
        USE_PHYS.store(is_phys, Ordering::Relaxed);
        set_ctl(0);
        let before = crate::gic3::pending_private();
        set_cval(counter() + f / 1000);
        set_ctl(1);
        let status = |is_phys: bool| -> u64 {
            let v: u64;
            // SAFETY: register read.
            unsafe {
                if is_phys {
                    asm!("mrs {}, cntp_ctl_el0", out(reg) v, options(nomem, nostack))
                } else {
                    asm!("mrs {}, cntv_ctl_el0", out(reg) v, options(nomem, nostack))
                }
            }
            v
        };
        // 1 ms ahead is the aim; 100 ms is how long a loaded machine (qemu on a busy host) may take to fire it.
        let fired = spin(100, &mut || status(is_phys) & 4 != 0);
        // The GIC takes a moment to see the line.
        spin(100, &mut || crate::gic3::pending_private() & !before != 0);
        let pending = crate::gic3::pending_private() & !before;
        // An interrupt that is pending proves the condition was met, whatever ISTATUS said when it was looked at.
        let fired = fired || pending != 0;
        set_ctl(0);
        spin(5, &mut || crate::gic3::pending_private() & !before == 0);
        for b in 16..32u32 {
            if pending & (1 << b) != 0 {
                crate::gic3::clear_pending(b);
            }
        }
        if i < out.len() {
            out[i] = Some(Probe { name, intid, fired, pending });
        }
        if fired && pending & (1 << intid) != 0 {
            IRQ.store(intid, Ordering::Relaxed);
            return Some((name, intid));
        }
        // The timer fired and exactly one other private interrupt came with it: that is its line.
        if fired && pending.count_ones() == 1 && pending.trailing_zeros() >= 16 {
            let other = pending.trailing_zeros();
            IRQ.store(other, Ordering::Relaxed);
            return Some((name, other));
        }
    }
    USE_PHYS.store(false, Ordering::Relaxed);
    None
}

/// Is the physical timer the tick source (`select_msm` chose it)?
#[cfg(feature = "msm8998")]
pub fn uses_phys() -> bool {
    USE_PHYS.load(Ordering::Relaxed)
}
