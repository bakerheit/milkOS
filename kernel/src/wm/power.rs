//! Restart / shut down (PSCI over HVC, which qemu's virt machine implements; the Raspberry Pi has no PSCI and
//! uses its power manager, `rpi/power.rs`), and the kernel panic screen.

use core::fmt::Write;

use crate::display;
use crate::font;
use crate::gfx::{DrawTarget, Surface};

use super::theme::*;

#[cfg(not(feature = "raspi"))]
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
#[cfg(not(feature = "raspi"))]
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

#[cfg(not(feature = "raspi"))]
fn psci(fid: u64) {
    // SAFETY: a PSCI call; for these two functions it does not return on success.
    unsafe { core::arch::asm!("hvc #0", inout("x0") fid => _, options(nomem, nostack)) };
}

pub fn shut_down() {
    // Commit the filesystems first: the last second of writes and the newest search-index
    // save would otherwise be lost with the power.
    let _ = crate::fs::sync_all();
    #[cfg(feature = "raspi")]
    {
        crate::kprintln!("wm: shutting down (power manager, halt)");
        // The SD card log: this boot's end, everything on the card, before the power goes.
        crate::rpi::logfile::shutdown_flush(false);
        crate::rpi::power::power_off();
    }
    #[cfg(not(feature = "raspi"))]
    {
        crate::kprintln!("wm: shutting down (PSCI SYSTEM_OFF)");
        psci(PSCI_SYSTEM_OFF);
    }
}

pub fn restart() {
    let _ = crate::fs::sync_all();
    #[cfg(feature = "raspi")]
    {
        crate::kprintln!("wm: restarting (power manager, full reset)");
        crate::rpi::logfile::shutdown_flush(true);
        crate::rpi::power::restart();
    }
    #[cfg(not(feature = "raspi"))]
    {
        crate::kprintln!("wm: restarting (PSCI SYSTEM_RESET)");
        psci(PSCI_SYSTEM_RESET);
    }
}

/// A fixed buffer to format into without allocating (the heap lock may be held mid-panic).
struct Fixed {
    buf: [u8; 1024],
    len: usize,
}

impl Write for Fixed {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Draw a panic card straight onto the framebuffer. No allocation, no locks: called from the
/// panic handler, possibly in the middle of a frame.
pub fn panic_screen(info: &core::panic::PanicInfo) {
    // Other CPUs keep running: stop the compositor from painting over the card.
    super::FROZEN.store(true, core::sync::atomic::Ordering::SeqCst);
    if crate::platform::SCREEN_ONLY {
        // No serial cable on the Pi: the end of the log (the panic message is in it) on a
        // full-screen text console, then the LED.
        crate::crashscreen::show();
        #[cfg(feature = "raspi")]
        crate::rpi::led::blink_forever();
        #[cfg(not(feature = "raspi"))]
        return;
    }
    let Some(ptr) = display::framebuffer() else {
        return;
    };
    // SAFETY: the static framebuffer; we are panicking, nothing else will draw again.
    let (fw, fh) = display::size();
    let mut fb = unsafe { Surface::from_raw(ptr, fw, fh, fw) };
    let mut msg = Fixed {
        buf: [0; 1024],
        len: 0,
    };
    let _ = write!(msg, "{info}");
    let text = core::str::from_utf8(&msg.buf[..msg.len]).unwrap_or("(unprintable)");
    let (w, h) = (640, 300);
    let (x, y) = ((screen_w() - w) / 2, (screen_h() - h) / 2);
    fb.fill_rect_alpha(0, 0, screen_w(), screen_h(), ESPRESSO, a(450));
    fb.fill_rounded_rect(x, y, w, h, R_CARD, MILK, 255);
    fb.rounded_rect(x, y, w, h, R_CARD, 1, SAND, 255);
    fb.fill_rounded_rect(x + 24, y + 24, 40, 40, 10, STRAW, 255);
    font::draw_str_alpha(&mut fb, x + 36, y + 28, "!", INK, 255, 2);
    font::draw_str_alpha(&mut fb, x + 37, y + 28, "!", INK, 255, 2);
    font::draw_str_alpha(&mut fb, x + 80, y + 24, "milkOS stopped", ESPRESSO, 255, 2);
    font::draw_str_alpha(&mut fb, x + 81, y + 24, "milkOS stopped", ESPRESSO, 255, 2);
    font::draw_str_alpha(&mut fb, x + 80, y + 60, "kernel panic", COCOA, 255, 1);
    // The message, wrapped at 74 columns, up to 9 lines.
    let cols = ((w - 48) / 8) as usize;
    let mut row = 0;
    for line in text.lines() {
        let mut rest = line;
        while !rest.is_empty() && row < 9 {
            let cut = rest.char_indices().nth(cols).map_or(rest.len(), |(i, _)| i);
            font::draw_str_alpha(
                &mut fb,
                x + 24,
                y + 92 + row * 18,
                &rest[..cut],
                ESPRESSO,
                255,
                1,
            );
            rest = &rest[cut..];
            row += 1;
        }
    }
    font::draw_str_alpha(
        &mut fb,
        x + 24,
        y + h - 36,
        "Details are on the serial console. Restart qemu to try again.",
        COCOA,
        255,
        1,
    );
    display::sync_panic();
}
