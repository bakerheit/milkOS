//! A safety net for a frozen board: the SoC's power-manager watchdog, armed once the boot is over and kicked from the
//! timer interrupt of CPU 0, so a kernel that stops (a hung bus, interrupts off for good) resets the board by itself
//! after 15 s. The next boot's log (rpi/logfile.rs) then shows `boot N` with no end line, followed by `boot N+1`:
//! nobody has to pull the power. The rules are `rpikit::watchdog` (host-tested with a fake clock).
//!
//! * **Off the boot path.** It is armed at the very end of `kmain`, after the display, the drivers and the slow
//!   steps, and never during them: a slow boot is not a frozen one.
//! * **Not in the way of a crash dump.** A panic or an exception dump stops it (`disarm_for_crash`, called first thing
//!   by the log's crash path), so the dump stays on the screen for as long as it takes to read.
//! * **Not in the way of a power-off.** `power::restart` / `power::power_off` stop it before they set up their own
//!   short countdown.
//! * `milk.watchdog=on|off|sim` (default on). `on` arms the hardware only where the device tree lists the block
//!   (`watchdog@...`): qemu's raspi4b has the registers but resets at once when RSTC is written instead of counting
//!   down, so there it says so and stays off. `sim` runs the same rules with no register: a CPU that finds the
//!   deadline missed resets the board through the power manager (the qemu test).

use core::ptr::write_volatile;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use rpikit::watchdog::{self as wk, Action, Setting, Watchdog};

use crate::kprintln;
use crate::mmu::phys_to_virt;
use crate::platform::periph;
use crate::sync::SpinLock;

const PM: usize = phys_to_virt(periph(0x10_0000));
const RSTC: usize = 0x1c;
const WDOG: usize = 0x24;

/// Reset after this long without a kick.
const TIMEOUT_MS: u64 = 15_000;
/// The kernel kicks once a second (from the 100 Hz tick).
const KICK_EVERY_MS: u64 = 1000;
/// `milk.watchdog=sim` runs on a shorter deadline, so the test does not wait 15 s.
const SIM_TIMEOUT_MS: u64 = 4_000;

const OFF: u8 = 0;
const HARDWARE: u8 = 1;
const SIM: u8 = 2;

static MODE: AtomicU8 = AtomicU8::new(OFF);
/// Armed right now (the tick's fast check; the crash path clears it without taking the lock).
static ARMED: AtomicBool = AtomicBool::new(false);
/// `sim`: the uptime (ms) after which a CPU that sees it passed resets the board.
static SIM_DEADLINE_MS: AtomicU64 = AtomicU64::new(u64::MAX);
static STATE: SpinLock<Watchdog> = SpinLock::new(Watchdog::new(TIMEOUT_MS, KICK_EVERY_MS));

fn wr(off: usize, v: u32) {
    // SAFETY: the power manager's registers, mapped as Device memory (platform.rs).
    let _c = crate::serror::write_guard(PM + off, v);
    unsafe { write_volatile((PM + off) as *mut u32, v) }
}

fn rd(off: usize) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((PM + off) as *const u32) }
}

fn now_ms() -> u64 {
    crate::timer::now_ns() / 1_000_000
}

/// Read `milk.watchdog` and decide what runs; says what it decided. Called from `rpi::init`.
pub fn configure() {
    let setting = match Setting::parse(crate::fwcfg::option("watchdog")) {
        Ok(s) => s,
        Err(s) => {
            kprintln!("[watchdog] milk.watchdog must be on, off or sim: using on");
            s
        }
    };
    // SAFETY: the bootloader's pointer, or 0.
    let dtb = unsafe { crate::dtb::Dtb::from_addr(crate::mem::boot_dtb_ptr()) };
    let has_block = dtb.is_some_and(|d| d.find_prefix(b"watchdog@", b"compatible").is_some());
    let mode = match setting {
        Setting::Off => {
            kprintln!("[watchdog] off (milk.watchdog=off): a frozen board has to be power cycled by hand");
            OFF
        }
        Setting::Sim => {
            kprintln!("[watchdog] simulated (milk.watchdog=sim): no register is written, a CPU resets the board when the kernel stops kicking");
            SIM
        }
        Setting::On if has_block => HARDWARE,
        Setting::On => {
            kprintln!(
                "[watchdog] not armed: the device tree has no power manager watchdog (qemu's raspi4b resets at once when its RSTC is written; milk.watchdog=sim tests the rules)"
            );
            OFF
        }
    };
    MODE.store(mode, Ordering::Release);
}

/// The boot is over: start the countdown. Called once, from `kmain`, after everything slow is done.
pub fn arm() {
    let mode = MODE.load(Ordering::Acquire);
    if mode == OFF {
        return;
    }
    let timeout = if mode == SIM { SIM_TIMEOUT_MS } else { TIMEOUT_MS };
    let now = now_ms();
    let act = {
        let mut w = STATE.lock();
        *w = Watchdog::new(timeout, KICK_EVERY_MS);
        w.arm(now)
    };
    if act != Action::Arm {
        return;
    }
    if mode == HARDWARE {
        // Linux's order: the timeout first, then the reset configuration, which starts the countdown.
        wr(WDOG, wk::wdog_value(timeout));
        wr(RSTC, wk::rstc_full_reset(rd(RSTC)));
    }
    SIM_DEADLINE_MS.store(now + timeout, Ordering::Release);
    ARMED.store(true, Ordering::Release);
    kprintln!(
        "[watchdog] armed ({}): the board resets after {} s without a kick, kicked every {} s from the timer interrupt of CPU 0; milk.watchdog=off turns it off",
        if mode == SIM { "simulated" } else { "power manager" },
        timeout / 1000,
        KICK_EVERY_MS / 1000
    );
}

/// The timer interrupt (every CPU, 100 Hz): CPU 0 kicks; with `sim`, the others look for a missed deadline.
pub fn on_tick() {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    let now = now_ms();
    if crate::percpu::id() == 0 {
        let (act, timeout) = {
            let mut w = STATE.lock();
            (w.tick(now), w.timeout_ms())
        };
        if act == Action::Kick {
            if MODE.load(Ordering::Relaxed) == HARDWARE {
                wr(WDOG, wk::wdog_value(timeout));
            }
            SIM_DEADLINE_MS.store(now + timeout, Ordering::Release);
        }
    } else if MODE.load(Ordering::Relaxed) == SIM && now >= SIM_DEADLINE_MS.load(Ordering::Acquire) {
        // Only one CPU gets to do it.
        if ARMED.swap(false, Ordering::AcqRel) {
            kprintln!("[watchdog] simulated: no kick for {} s: the board is reset now", SIM_TIMEOUT_MS / 1000);
            super::power::restart();
        }
    }
}

/// Stop the countdown: a power-off or restart is about to set up its own.
pub fn disarm() {
    if !ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    let _ = STATE.lock().disarm();
    if MODE.load(Ordering::Relaxed) == HARDWARE {
        wr(RSTC, wk::RSTC_STOP);
    }
}

/// A panic or an exception dump: stop it so the screen stays readable. No lock (the crash may have happened inside one).
pub fn disarm_for_crash() {
    if !ARMED.swap(false, Ordering::AcqRel) {
        return;
    }
    if MODE.load(Ordering::Relaxed) == HARDWARE {
        wr(RSTC, wk::RSTC_STOP);
    }
}

/// For the system report: what it is doing.
pub fn status() -> alloc::string::String {
    use alloc::format;
    match MODE.load(Ordering::Relaxed) {
        OFF => alloc::string::String::from("off"),
        m => {
            let w = STATE.lock();
            format!(
                "{} ({}), {} kicks, longest gap between kicks {} ms of {} ms",
                if ARMED.load(Ordering::Relaxed) { "armed" } else { "not armed (boot not over, or stopped for a crash dump or power off)" },
                if m == SIM { "simulated" } else { "power manager" },
                w.kicks(),
                w.max_gap_ms(),
                w.timeout_ms()
            )
        }
    }
}

/// `milk.crashtest=hang`: the kernel stops on CPU 0 with interrupts off, as a frozen board would (the watchdog test).
pub fn hang_cpu0() {
    kprintln!("crash-test: CPU 0 now stops with interrupts off (milk.crashtest=hang); the watchdog should reset the board");
    crate::sched::spawn_on("hang", crate::sched::Priority::High, Some(0), || {
        crate::sync::irq_disable();
        loop {
            core::hint::spin_loop();
        }
    });
}
