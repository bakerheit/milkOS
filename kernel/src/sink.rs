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

/// Take the screen-console flag.
fn try_lock() -> bool {
    if !mmu_on() {
        if SCREEN.busy.load(Ordering::Relaxed) {
            return false;
        }
        SCREEN.busy.store(true, Ordering::Relaxed);
        return true;
    }
    SCREEN
        .busy
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
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
        // The mouse cursor sits on the same framebuffer: take it off while the console draws.
        let _cursor = crate::cursor::suspend();
        f(c);
    }
    SCREEN.busy.store(false, Ordering::Release);
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

/// The writer behind `kprintln!`.
pub struct Sink;

impl Write for Sink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let _ = crate::Uart.write_str(s);
        crate::klog::write(s);
        with_screen(|c| c.write_str(s));
        Ok(())
    }
}
