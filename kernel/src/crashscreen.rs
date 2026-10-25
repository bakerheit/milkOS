//! What a crash shows on the screen of a board nobody has a serial cable on (the Raspberry Pi:
//! HDMI is the only output). The text of a panic or exception dump is in the kernel log, so this
//! paints the end of the log as a full-screen text console, in big letters, over whatever was
//! there, and `kprintln!` output keeps going to the screen console if one is installed.
//!
//! Called with locks possibly held and other CPUs running: no allocation, no waiting on locks
//! (the log tail is read with a try-lock). If the boot console is still on screen (the crash came
//! before the desktop started) it already shows everything and is left as it is: the screen is not
//! cleared, so the boot log stays readable above the dump.

use crate::fbcon::Console;
use crate::gfx::Surface;
use crate::{display, klog, sink};

/// How much of the log to paint: more than any screen of text holds.
const TAIL_BYTES: usize = 12 * 1024;

/// Show the crash on screen (once per crash; a second call repaints). Returns whether a screen
/// was there.
pub fn show() -> bool {
    // Keep the compositor from painting over the text.
    crate::wm::FROZEN.store(true, core::sync::atomic::Ordering::SeqCst);
    // The 3.5-inch SPI panel on the GPIO header, if there is one: the same text on its own console.
    #[cfg(feature = "raspi")]
    let lcd = show_on_lcd();
    #[cfg(not(feature = "raspi"))]
    let lcd = false;
    #[cfg(feature = "raspi")]
    if display::lcd_is_primary() {
        return lcd;
    }
    let Some(ptr) = display::framebuffer() else {
        return lcd;
    };
    if sink::screen_installed() {
        // The boot console has the text already: make sure the last lines are out.
        sink::flush_screen();
        display::sync_panic();
        return true;
    }
    let (w, h) = display::size();
    // SAFETY: the live framebuffer; whatever else was drawing is stopped or about to be.
    let fb = unsafe { Surface::from_raw(ptr, w, h, w) };
    // Drawn in RAM and written out in rows: scrolling reads, and reading video memory is slow.
    let mut console = match display::console_shadow(w, h) {
        Some(shadow) => Console::with_shadow(fb, shadow, display::console_scale()),
        None => Console::with_scale(fb, display::console_scale()),
    };
    // The end of the log, which holds the exception dump or the panic message with its
    // location, then a last line saying what this is.
    klog::tail(TAIL_BYTES, |bytes| {
        for chunk in bytes.utf8_chunks() {
            console.write_str(chunk.valid());
        }
    });
    console.write_str(
        "\n\x1b[1;31mmilkOS stopped.\x1b[0m The green LED flashes. Photograph this screen.\n",
    );
    console.flush();
    display::sync_panic();
    true
}

/// The crash screen on the SPI panel: the end of the log (as much as fits: 58 x 18 characters)
/// and the closing line. Nothing allocates, nothing waits for a lock.
#[cfg(feature = "raspi")]
fn show_on_lcd() -> bool {
    crate::rpi::lcd::crash_paint(|console| {
        klog::tail(TAIL_BYTES, |bytes| {
            for chunk in bytes.utf8_chunks() {
                console.write_str(chunk.valid());
            }
        });
        console.write_str("\n\x1b[1;31mmilkOS stopped.\x1b[0m The LED flashes.\n");
    })
}
