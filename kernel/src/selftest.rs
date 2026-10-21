//! Boot self-test for exceptions, interrupts and the timer. Prints to the console; nothing
//! here is load-bearing for the kernel proper.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::exceptions::{BRK_SELFTEST, TrapFrame};
use crate::{gic, kprintln, sync, timer};

static DEADLINE_FIRED: AtomicBool = AtomicBool::new(false);
static DEADLINE_AT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static DEADLINE_LATE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn wfi() {
    unsafe { asm!("wfi", options(nomem, nostack)) };
}

fn deadline_cb(_frame: &mut TrapFrame) {
    DEADLINE_LATE.store(
        timer::now_ns().saturating_sub(DEADLINE_AT.load(Ordering::Relaxed)),
        Ordering::Relaxed,
    );
    DEADLINE_FIRED.store(true, Ordering::Release);
}

/// Run all boot checks. IRQs must be unmasked.
pub fn run() {
    kprintln!("[selftest] begin");

    // 1. The tick advances (~100 per second).
    let t0 = timer::now_ns();
    let k0 = timer::ticks();
    while timer::now_ns() - t0 < 1_000_000_000 {
        wfi();
    }
    let k1 = timer::ticks();
    kprintln!(
        "[selftest] ticks: {} -> {} over {} ms (expect ~{})",
        k0,
        k1,
        (timer::now_ns() - t0) / 1_000_000,
        timer::TICK_HZ
    );

    // 2. One-shot deadline fires, between ticks.
    let at = timer::now_ns() + 25_000_000;
    DEADLINE_AT.store(at, Ordering::Relaxed);
    timer::set_deadline(at, deadline_cb);
    while !DEADLINE_FIRED.load(Ordering::Acquire) {
        wfi();
    }
    kprintln!(
        "[selftest] one-shot deadline fired {} us after its target (25 ms out)",
        DEADLINE_LATE.load(Ordering::Relaxed) / 1000
    );

    // 3. delay_us is roughly right.
    let a = timer::now_ns();
    timer::delay_us(2000);
    kprintln!(
        "[selftest] delay_us(2000) took {} us",
        (timer::now_ns() - a) / 1000
    );

    // 4. IRQ mask nesting.
    {
        let _g = sync::IrqGuard::new();
        let _g2 = sync::IrqGuard::new();
        assert!(!sync::irqs_enabled());
    }
    assert!(sync::irqs_enabled());

    // 5. A synchronous exception from the kernel is caught and returns: SVC...
    unsafe { asm!("svc #0x42", inout("x0") 7u64 => _, in("x8") 93u64) };
    kprintln!("[selftest] back after SVC");
    // ...and BRK.
    unsafe { asm!("brk #{imm}", imm = const BRK_SELFTEST) };
    kprintln!("[selftest] back after BRK");

    kprintln!(
        "[selftest] spurious IRQs: {}, uart overruns: {}",
        gic::spurious_count(),
        crate::uart::overruns()
    );

    #[cfg(feature = "crash-test")]
    crash_test();

    kprintln!("[selftest] ok. type something, it will be echoed.");
}

/// Prove the crash dump works with a deliberately bad access. Enabled with
/// `cargo build --features crash-test`; never returns.
#[cfg(feature = "crash-test")]
fn crash_test() {
    kprintln!("[selftest] crash-test: reading from a bad pointer...");
    let p = 0xdead_0000_0000u64 as *const u64;
    let v = unsafe { core::ptr::read_volatile(p) };
    kprintln!("[selftest] crash-test: survived?! read {v:#x}");
}
