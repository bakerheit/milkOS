//! Where `kprintln!` output goes: the UART, the kernel log ring ([`crate::klog`], shown by the
//! desktop's Terminal), and the on-screen console if one is installed (only when the desktop
//! could not start, see `display::console_fallback`).
//!
//! The screen console sits behind a try-lock. If something (an interrupt handler, a panic) prints
//! while the console is already mid-write, the text still goes to the UART but is dropped from the
//! screen rather than deadlocking or corrupting the console state.

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

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

/// Take the flag. Deliberately a plain load + store, not an atomic swap: with the MMU off RAM is
/// Device memory, where exclusive load/store (what `swap` compiles to) never completes. On a single
/// core this is still safe, even against interrupts: a handler that sneaks in between the load and
/// the store runs to completion and releases the flag before we resume.
/// TODO(smp): switch to `swap` once the MMU maps RAM as Normal memory, and secondary cores run.
fn try_lock() -> bool {
    if SCREEN.busy.load(Ordering::Relaxed) {
        return false;
    }
    SCREEN.busy.store(true, Ordering::Relaxed);
    true
}

/// Run `f` on the screen console if one is installed and not already in use.
fn with_screen(f: impl FnOnce(&mut Console)) {
    if !try_lock() {
        return;
    }
    // SAFETY: we hold the `busy` flag.
    if let Some(c) = unsafe { &mut *SCREEN.inner.get() } {
        f(c);
    }
    SCREEN.busy.store(false, Ordering::Relaxed);
}

/// From now on `kprintln!` output is mirrored on screen.
pub fn install_screen(console: Console) {
    while !try_lock() {
        core::hint::spin_loop();
    }
    // SAFETY: we hold the `busy` flag.
    unsafe { *SCREEN.inner.get() = Some(console) };
    SCREEN.busy.store(false, Ordering::Relaxed);
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
