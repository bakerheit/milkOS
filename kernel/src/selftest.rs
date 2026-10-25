//! Boot self-test for exceptions, interrupts and the timer. Prints to the console; nothing
//! here is load-bearing for the kernel proper.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::exceptions::{BRK_SELFTEST, TrapFrame};
use crate::{gic, kprintln, sync, timer};

/// The boot self-tests that are slow (seconds in a debug kernel), each behind a bit of the boot
/// option `opt/milkos/selftest`: `full` runs them all, or a comma list of `timer` (this file),
/// `crypto` (the shared crate's vectors), `fs`, `sched` and `net`. The default boot runs none of
/// them: scripts/selftest-boot.sh is the test that runs them all (`MILK_SELFTEST=full` in
/// scripts/run.sh); a test that checks one subsystem's lines asks for just that one (net-test:
/// `net`). memtest, the CSPRNG's own vectors and the process layer's self-test run every boot:
/// they are fast and the rest of the system leans on what they check.
pub const TIMER: u32 = 1 << 0;
pub const CRYPTO: u32 = 1 << 1;
pub const FS: u32 = 1 << 2;
pub const SCHED: u32 = 1 << 3;
pub const NET: u32 = 1 << 4;
const ALL: u32 = TIMER | CRYPTO | FS | SCHED | NET;
const NAMES: [(&str, u32); 5] = [
    ("timer", TIMER),
    ("crypto", CRYPTO),
    ("fs", FS),
    ("sched", SCHED),
    ("net", NET),
];

static WANT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Read `opt/milkos/selftest` (early in kmain: fw_cfg works from the start).
pub fn read_option() {
    let mut text = [0u8; 64];
    let n = crate::fwcfg::find_file("opt/milkos/selftest")
        .filter(|f| (f.size as usize) <= text.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut text[..f.size as usize]))
        .map_or(0, |f| f.size as usize);
    let s = core::str::from_utf8(&text[..n]).unwrap_or("").trim();
    let mut want = 0;
    for w in s.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        match NAMES.iter().find(|(name, _)| *name == w) {
            Some((_, bit)) => want |= bit,
            None if w == "full" => want = ALL,
            None => kprintln!("[selftest] opt/milkos/selftest: unknown self-test {w:?} (ignored)"),
        }
    }
    WANT.store(want, Ordering::Relaxed);
    if want == ALL {
        kprintln!("[selftest] full boot: every boot self-test runs (opt/milkos/selftest=full)");
    } else if want == 0 {
        kprintln!(
            "[selftest] quick boot: the slow self-tests are off (opt/milkos/selftest=full runs them)"
        );
    } else {
        let mut on = alloc::string::String::new();
        for (name, bit) in NAMES {
            if want & bit != 0 {
                on.push_str(if on.is_empty() { "" } else { "," });
                on.push_str(name);
            }
        }
        kprintln!("[selftest] boot self-tests on: {on} (opt/milkos/selftest)");
    }
}

/// Does this boot run the self-test(s) `bit`?
pub fn wants(bit: u32) -> bool {
    WANT.load(Ordering::Relaxed) & bit != 0
}

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

    // 6. The baked system fonts decode (every family, size, weight and Latin-1 glyph).
    let t = timer::now_ns();
    let fonts_ok = crate::sysfont::selftest();
    kprintln!(
        "[selftest] system fonts: {} families {} in {} ms",
        crate::sysfont::FAMILIES.len(),
        if fonts_ok { "ok" } else { "BROKEN" },
        (timer::now_ns() - t) / 1_000_000
    );
    assert!(fonts_ok, "baked system fonts are broken");

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

/// Boot option `opt/milkos/crashtest` (command line `milk.crashtest=early` or `=late` on the Pi): panic
/// on purpose, to prove what a crash shows on the screen (scripts/pi-qemu-test.sh). `early` panics
/// as soon as the display and the board drivers are up (the boot log is still on screen); `late`
/// once the desktop has been up for a few seconds. Called from `kmain` at both points.
pub fn crash_if_asked(stage: &str) {
    let mut text = [0u8; 16];
    let Some(f) = crate::fwcfg::find_file("opt/milkos/crashtest")
        .filter(|f| (f.size as usize) <= text.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut text[..f.size as usize]))
    else {
        return;
    };
    let want = &text[..f.size as usize];
    // `late-off` / `late-reset`: the same wait, then the desktop's Shut Down / Restart, to test the power paths.
    if stage == "late" && (want == b"late-off" || want == b"late-reset") {
        crate::sched::sleep_ms(5000);
        if want == b"late-off" {
            crate::wm::power::shut_down();
        } else {
            crate::wm::power::restart();
        }
        return;
    }
    // `serror` / `dabt` (at the early point): the exception dump instead of a panic, for the Pi's qemu test.
    // `serror` runs a fake SError through the real crash path (qemu cannot raise one); `dabt` is a real data
    // abort, a store to an address nothing maps, right after a checkpoint.
    if stage == "early" && want == b"serror" {
        crate::serror::checkpoint("crashtest-before");
        crate::serror::simulate();
    }
    if stage == "early" && want == b"dabt" {
        crate::serror::checkpoint("crashtest-dabt");
        // SAFETY: deliberately not: nothing maps this address, the store takes a translation fault at EL1.
        unsafe { core::ptr::write_volatile(0xffff_f000_dead_0010 as *mut u32, 0x600d_f00d) };
    }
    if want != stage.as_bytes() {
        return;
    }
    if stage == "late" {
        crate::sched::sleep_ms(8000);
    }
    panic!("crash-test: a deliberate panic, {stage} (opt/milkos/crashtest={stage})");
}
