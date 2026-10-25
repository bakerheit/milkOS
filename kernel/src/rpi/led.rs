//! The green ACT LED (GPIO 42 on the Pi 4 B, active high): a sign of life that needs nothing
//! but the GPIO block, for a board with no serial cable and a screen that may not light up.
//!
//! * [`on`], first thing in `kmain`: the LED is on from the first instruction of the kernel.
//! * [`start_codes`] and [`stage`]: boot progress codes (the start marker, then N pulses per
//!   milestone; the pattern language is `rpikit::ledcode`, how to read it is docs/raspberry-pi.md).
//!   Each burst is played by busy-waiting on the generic counter, so it needs no allocator,
//!   timer interrupt or scheduler, and adds about 9 s to a whole boot (`milk.ledcodes=0` turns
//!   the codes off; the `led: stage N name` log lines stay).
//! * [`finish`]: the boot is done, solid on. If the boot log is going into a file on the card
//!   (`logfile`) and everything logged has been saved with nothing new for two seconds, the LED shows the
//!   **log saved** signal instead of solid on: two short blinks, then about a second of dark, over and over;
//!   new log text brings the solid light back until it is saved again.
//! * [`blink_forever`], from a crash: the crash cycle (`rpikit::ledcrash`): 2 s of fast flashing
//!   (about 5 Hz), so "on" means "running" and "flashing" means "crashed" even when nothing reached
//!   the screen, then the boot stage reached, the kind of crash, the faulting PC and address as
//!   counted pulses, over and over. Whoever crashes calls [`note_exception`] / [`note_panic`] first
//!   (they also log the same numbers in plain hex), then runs the screen paths, then this.
//!
//! GPFSEL4 (+0x10) holds the function of pins 40..49 in 3-bit fields (pin 42 is bits 8:6, 001 =
//! output); GPSET1 (+0x20) / GPCLR1 (+0x2c) set / clear pins 32..53 (pin 42 is bit 10).

use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use rpikit::ledcode::{self, Burst, Stage};
use rpikit::ledcrash::{self, CrashCycle, CrashRecord};

use super::Timeout;
use crate::mmu::phys_to_virt;
use crate::platform::periph;

const GPIO: usize = phys_to_virt(periph(0x20_0000));
const GPFSEL4: usize = 0x10;
const GPSET1: usize = 0x20;
const GPCLR1: usize = 0x2c;
const PIN_BIT: u32 = 1 << (42 - 32);

fn reg_write(off: usize, v: u32) {
    // SAFETY: GPIO registers, mapped as Device memory (platform.rs).
    unsafe { write_volatile((GPIO + off) as *mut u32, v) }
}

fn make_output() {
    // SAFETY: as above; read-modify-write of the function select word for pins 40..49.
    unsafe {
        let p = (GPIO + GPFSEL4) as *mut u32;
        let v = read_volatile(p);
        write_volatile(p, v & !(0b111 << 6) | (0b001 << 6));
    }
}

pub fn on() {
    make_output();
    reg_write(GPSET1, PIN_BIT);
}

/// Are the codes being shown (`milk.ledcodes`, default on)? Set by [`start_codes`].
static CODES: AtomicBool = AtomicBool::new(false);
/// A crash took the LED over: no code is played any more.
static CRASHED: AtomicBool = AtomicBool::new(false);

fn wait_ms(ms: u32) {
    let t = Timeout::ms(u64::from(ms));
    while !t.expired() {}
}

fn play(burst: Burst) {
    for iv in burst {
        if CRASHED.load(Ordering::Relaxed) {
            return;
        }
        reg_write(if iv.on { GPSET1 } else { GPCLR1 }, PIN_BIT);
        wait_ms(iv.ms);
    }
}

/// Read `milk.ledcodes`, show the start marker and the first milestone (memory up). Call once,
/// right after `mem::init` (the device tree is reachable from there on).
pub fn start_codes() {
    let mut buf = [0u8; 8];
    let value = crate::fwcfg::find_file("opt/milkos/ledcodes")
        .filter(|f| (f.size as usize) <= buf.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut buf[..f.size as usize]))
        .map(|f| &buf[..f.size as usize]);
    let on = ledcode::enabled(value);
    CODES.store(on, Ordering::Relaxed);
    if on {
        play(Burst::marker());
    }
    stage(Stage::Memory, true);
}

/// A milestone was reached (`ok`), or its bounded wait gave up / the device is not there (`!ok`).
/// Logs `led: stage N name`, and plays the burst unless the codes are off.
pub fn stage(stage: Stage, ok: bool) {
    STAGE.store(stage.code(), Ordering::Relaxed);
    crate::kprintln!(
        "led: stage {} {}{}",
        stage.code(),
        stage.name(),
        if ok { "" } else { " (failed)" }
    );
    // A milestone is a good moment to save the boot log (a no-op until the card is up and has the log file).
    super::logfile::milestone();
    if CODES.load(Ordering::Relaxed) {
        play(Burst::stage(stage, ok));
    }
}

/// The boot is done (the desktop is up, or there is no desktop to wait for): solid on.
pub fn finish() {
    STAGE.store(ledcrash::MAX_STAGE, Ordering::Relaxed);
    crate::kprintln!("led: done, solid on");
    super::logfile::milestone();
    FINISHED.store(true, Ordering::Release);
    on();
}

/// The boot is done and the LED is solid on (set by [`finish`]).
static FINISHED: AtomicBool = AtomicBool::new(false);

/// May the log writer show its "log saved" signal? Only once the boot is over, with no crash, and not
/// when the codes are off (`milk.ledcodes=0`).
pub fn saved_signal_allowed() -> bool {
    FINISHED.load(Ordering::Acquire)
        && CODES.load(Ordering::Relaxed)
        && !CRASHED.load(Ordering::Relaxed)
}

/// Drive the LED for the "log saved" signal: two short blinks and a pause, repeating (solid on = still
/// writing or no log file). Ignored before the boot is over and after a crash.
pub fn saved_signal(on: bool) {
    if !FINISHED.load(Ordering::Acquire) || CRASHED.load(Ordering::Relaxed) {
        return;
    }
    reg_write(if on { GPSET1 } else { GPCLR1 }, PIN_BIT);
}

// ---- the crash cycle ----------------------------------------------------------------------

/// The last boot stage reached (0: none yet, 8: the desktop is up).
static STAGE: AtomicU8 = AtomicU8::new(0);
/// What the crash said, set by [`note_exception`] / [`note_panic`]; plain atomics, no locks.
static NOTE_EC: AtomicU8 = AtomicU8::new(ledcrash::EC_OTHER);
static NOTE_PC: AtomicU64 = AtomicU64::new(0);
static NOTE_FAR: AtomicU64 = AtomicU64::new(0);
/// Set by the first CPU to start the cycle; a second crashing CPU parks instead of fighting over the pin.
static CYCLE_OWNER: AtomicBool = AtomicBool::new(false);

fn note(rec: CrashRecord) {
    NOTE_EC.store(rec.ec, Ordering::Relaxed);
    NOTE_PC.store(rec.pc, Ordering::Relaxed);
    NOTE_FAR.store(u64::from(rec.far), Ordering::Relaxed);
}

fn record() -> CrashRecord {
    CrashRecord::new(
        STAGE.load(Ordering::Relaxed),
        NOTE_EC.load(Ordering::Relaxed),
        NOTE_PC.load(Ordering::Relaxed),
        NOTE_FAR.load(Ordering::Relaxed),
    )
}

fn kind_name(ec: u8) -> &'static str {
    match ec {
        ledcrash::EC_PANIC => "panic",
        ledcrash::EC_STACK_OVERFLOW => "kernel stack overflow",
        ledcrash::EC_OTHER => "other",
        ec => crate::exceptions::ec_name(u64::from(ec)),
    }
}

/// The same numbers as the cycle, in the log (and so on the crash screens): octal as counted, hex as the
/// decoder prints. Allocation-free.
fn log_code(rec: CrashRecord, what: core::fmt::Arguments) {
    crate::kprintln!(
        "led: crash code {rec}  [{what}]\nled: stage {} ec {:#04x} ({}) pc {:#018x} far {:#08x}",
        rec.stage,
        rec.ec,
        kind_name(rec.ec),
        rec.kernel_pc(),
        rec.far
    );
}

/// An exception is about to stop the kernel: remember it for the cycle and log the code. `far` is only
/// kept for aborts and alignment faults (the register is stale otherwise).
pub fn note_exception(esr: u64, elr: u64, far: u64) {
    let ec = (esr >> 26) & 0x3f;
    let has_far = matches!(ec, 0x20 | 0x21 | 0x22 | 0x24 | 0x25 | 0x26 | 0x34 | 0x35);
    let rec = CrashRecord::exception(
        STAGE.load(Ordering::Relaxed),
        esr,
        elr,
        if has_far { far } else { 0 },
    );
    note(rec);
    log_code(rec, format_args!("exception"));
    super::logfile::crash_flush();
}

/// The kernel stack overflowed.
pub fn note_stack_overflow(elr: u64, far: u64) {
    let rec = CrashRecord::new(STAGE.load(Ordering::Relaxed), ledcrash::EC_STACK_OVERFLOW, elr, far);
    note(rec);
    log_code(rec, format_args!("stack overflow"));
    super::logfile::crash_flush();
}

/// A Rust panic: the cycle carries the source line and a hash of the file name.
pub fn note_panic(info: &core::panic::PanicInfo) {
    let (file, line) = match info.location() {
        Some(l) => (l.file(), l.line()),
        None => ("", 0),
    };
    let rec = CrashRecord::panic(STAGE.load(Ordering::Relaxed), file, line);
    note(rec);
    log_code(rec, format_args!("panic at {file}:{line}"));
    super::logfile::crash_flush();
}

/// Show the crash cycle until the end of time. Never returns. Needs no allocator, no timer interrupt
/// and no locks: the pin and the generic counter only.
pub fn blink_forever() -> ! {
    CRASHED.store(true, Ordering::Relaxed);
    if CYCLE_OWNER.swap(true, Ordering::AcqRel) {
        loop {
            // SAFETY: waits for an event; another CPU is showing the cycle.
            unsafe { core::arch::asm!("wfe", options(nomem, nostack)) };
        }
    }
    // The log file gets the last of the log (the crash screens may have added to it) before the cycle starts.
    super::logfile::crash_flush();
    make_output();
    loop {
        // The record is read again every round, so a late note still shows.
        for iv in CrashCycle::new(record()) {
            reg_write(if iv.on { GPSET1 } else { GPCLR1 }, PIN_BIT);
            wait_ms(iv.ms);
        }
    }
}
