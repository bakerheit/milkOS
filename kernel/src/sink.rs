//! Where `kprintln!` output goes: the UART, the kernel log ring ([`crate::klog`], shown by the
//! desktop's Terminal), and the on-screen console if one is installed (only when the desktop
//! could not start, see `display::console_fallback`).
//!
//! The screen console sits behind a try-lock. If something (an interrupt handler, a panic) prints
//! while the console is already mid-write, the text still goes to the UART but is dropped from the
//! screen rather than deadlocking or corrupting the console state.

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::fbcon::Console;

struct ScreenConsole {
    busy: AtomicBool,
    inner: UnsafeCell<Option<Console>>,
}

/// The second screen, on a Raspberry Pi with the 3.5-inch SPI display: the same text, on its own
/// console (rpi/lcd.rs sends what it draws to the panel).
#[cfg(feature = "raspi")]
static LCD: ScreenConsole = ScreenConsole {
    busy: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

// SAFETY: access to `inner` is guarded by `busy` (see `try_lock`).
unsafe impl Sync for ScreenConsole {}

static SCREEN: ScreenConsole = ScreenConsole {
    busy: AtomicBool::new(false),
    inner: UnsafeCell::new(None),
};

/// Atomic RMW hangs with the MMU off (RAM is Device memory then), and a crash dump can come
/// that early: use plain load/store until the MMU is on.
fn mmu_on() -> bool {
    let sctlr: u64;
    // SAFETY: register read.
    unsafe { core::arch::asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack)) };
    sctlr & 1 != 0
}

/// Take a console's flag.
fn try_lock_of(c: &ScreenConsole) -> bool {
    if !mmu_on() {
        if c.busy.load(Ordering::Relaxed) {
            return false;
        }
        c.busy.store(true, Ordering::Relaxed);
        return true;
    }
    c.busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Take the screen-console flag.
fn try_lock() -> bool {
    try_lock_of(&SCREEN)
}

/// Owner (CPU index + 1) of the line lock that keeps `kprintln!` lines from different CPUs
/// apart; 0 = free.
static PRINT_OWNER: AtomicU32 = AtomicU32::new(0);

/// Run `f` (one `kprintln!` line) with IRQs masked and the print lock held. Nested prints on
/// the same CPU (a fault while printing) go straight through; if another CPU holds the lock
/// for more than ~20 ms (it died mid-line), print anyway rather than hang.
pub fn with_print_lock<R>(f: impl FnOnce() -> R) -> R {
    let _irq = crate::sync::IrqGuard::new();
    if !mmu_on() {
        return f();
    }
    let me = crate::percpu::id() as u32 + 1;
    let mut took = false;
    if PRINT_OWNER.load(Ordering::Relaxed) != me {
        let start = crate::timer::counter();
        let limit = crate::timer::frequency() / 50;
        loop {
            if PRINT_OWNER
                .compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                took = true;
                break;
            }
            if limit != 0 && crate::timer::counter().wrapping_sub(start) > limit {
                break;
            }
            core::hint::spin_loop();
        }
    }
    let r = f();
    if took {
        PRINT_OWNER.store(0, Ordering::Release);
    }
    r
}

/// Run `f` on the screen console if one is installed and not already in use.
fn with_screen(f: impl FnOnce(&mut Console)) {
    if !try_lock() {
        return;
    }
    // SAFETY: we hold the `busy` flag.
    if let Some(c) = unsafe { &mut *SCREEN.inner.get() } {
        // Not while the display may still be reading the framebuffer.
        crate::display::wait();
        // The mouse cursor sits on the same framebuffer: take it off while the console draws.
        let cursor = crate::cursor::suspend();
        f(c);
        drop(cursor);
        crate::display::sync();
    }
    SCREEN.busy.store(false, Ordering::Release);
}

/// Run `f` on the LCD console if one is installed and not already in use, then hand the rows it
/// changed to the panel's driver. Nothing waits for the panel here: the driver sends them when it
/// next can (`rpi::lcd::pump`, after the print lock is released, or its own thread).
#[cfg(feature = "raspi")]
fn with_lcd(f: impl FnOnce(&mut Console)) {
    if !try_lock_of(&LCD) {
        return;
    }
    // SAFETY: we hold the `busy` flag.
    if let Some(c) = unsafe { &mut *LCD.inner.get() } {
        f(c);
        if let Some((lo, hi)) = c.take_dirty() {
            crate::rpi::lcd::mark_rows(lo, hi);
        }
    }
    LCD.busy.store(false, Ordering::Release);
}

/// From now on `kprintln!` output is also drawn on the LCD console.
#[cfg(feature = "raspi")]
pub fn install_lcd_console(console: Console) {
    while !try_lock_of(&LCD) {
        core::hint::spin_loop();
    }
    // SAFETY: we hold the `busy` flag.
    unsafe { *LCD.inner.get() = Some(console) };
    LCD.busy.store(false, Ordering::Release);
}

/// Stop drawing on the LCD console (the desktop has the panel now, or the crash screen).
#[cfg(feature = "raspi")]
pub fn remove_lcd_console() {
    while !try_lock_of(&LCD) {
        core::hint::spin_loop();
    }
    // SAFETY: we hold the `busy` flag.
    unsafe { *LCD.inner.get() = None };
    LCD.busy.store(false, Ordering::Release);
}

/// After a `kprintln!` line, outside the print lock: let the panel's driver send what changed.
#[inline]
pub fn after_print() {
    #[cfg(feature = "raspi")]
    crate::rpi::lcd::pump();
}

/// From now on `kprintln!` output is mirrored on screen.
pub fn install_screen(console: Console) {
    while !try_lock() {
        core::hint::spin_loop();
    }
    // SAFETY: we hold the `busy` flag.
    unsafe { *SCREEN.inner.get() = Some(console) };
    SCREEN.busy.store(false, Ordering::Release);
}

/// Stop mirroring to the screen (the desktop is about to own the framebuffer). The console's
/// state is dropped; the text is still in the kernel log.
pub fn remove_screen() {
    while !try_lock() {
        core::hint::spin_loop();
    }
    // SAFETY: we hold the `busy` flag.
    unsafe { *SCREEN.inner.get() = None };
    SCREEN.busy.store(false, Ordering::Release);
}

/// Show what the screen console has drawn so far (a no-op without a RAM shadow). Never waits.
pub fn flush_screen() {
    if !try_lock() {
        return;
    }
    // SAFETY: we hold the `busy` flag.
    if let Some(c) = unsafe { &mut *SCREEN.inner.get() } {
        c.flush();
    }
    SCREEN.busy.store(false, Ordering::Release);
}

/// Is `kprintln!` mirrored on the screen right now?
pub fn screen_installed() -> bool {
    // SAFETY: a racy peek at a word for a yes/no answer (used when a panic wants to know whether
    // its text already reaches the screen); a torn read cannot happen on one aligned word.
    unsafe { (*SCREEN.inner.get()).is_some() }
}

/// The writer behind `kprintln!`.
pub struct Sink;

impl Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let _ = crate::Uart.write_str(s);
        crate::klog::write(s);
        // The SD card log (rpi/logbuf.rs) gets everything the serial port and the screens get.
        #[cfg(feature = "raspi")]
        crate::rpi::logbuf::console(s);
        with_screen(|c| c.write_str(s));
        #[cfg(feature = "raspi")]
        with_lcd(|c| c.write_str(s));
        Ok(())
    }
}
