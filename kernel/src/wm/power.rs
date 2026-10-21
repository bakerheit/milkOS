//! Restart / shut down (PSCI over HVC, which qemu's virt machine implements), and the kernel
//! panic screen.

use core::fmt::Write;

use crate::font;
use crate::gfx::{DrawTarget, Surface};
use crate::ramfb;

use super::theme::*;

const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

fn psci(fid: u64) {
    // SAFETY: a PSCI call; for these two functions it does not return on success.
    unsafe { core::arch::asm!("hvc #0", inout("x0") fid => _, options(nomem, nostack)) };
}

pub fn shut_down() {
    crate::kprintln!("wm: shutting down (PSCI SYSTEM_OFF)");
    psci(PSCI_SYSTEM_OFF);
}

pub fn restart() {
    crate::kprintln!("wm: restarting (PSCI SYSTEM_RESET)");
    psci(PSCI_SYSTEM_RESET);
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
    let Some(ptr) = ramfb::framebuffer() else {
        return;
    };
    // SAFETY: the static framebuffer; we are panicking, nothing else will draw again.
    let mut fb = unsafe { Surface::from_raw(ptr, ramfb::WIDTH, ramfb::HEIGHT, ramfb::WIDTH) };
    let mut msg = Fixed {
        buf: [0; 1024],
        len: 0,
    };
    let _ = write!(msg, "{info}");
    let text = core::str::from_utf8(&msg.buf[..msg.len]).unwrap_or("(unprintable)");
    let (w, h) = (640, 300);
    let (x, y) = ((SCREEN_W - w) / 2, (SCREEN_H - h) / 2);
    fb.fill_rect_alpha(0, 0, SCREEN_W, SCREEN_H, ESPRESSO, a(450));
    fb.fill_rounded_rect(x, y, w, h, R_CARD, MILK, 255);
    fb.rounded_rect(x, y, w, h, R_CARD, 1, SAND, 255);
    fb.fill_rounded_rect(x + 24, y + 24, 40, 40, 10, STRAW, 255);
    font::draw_str_alpha(&mut fb, x + 36, y + 28, "!", MILK, 255, 2);
    font::draw_str_alpha(&mut fb, x + 37, y + 28, "!", MILK, 255, 2);
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
}
