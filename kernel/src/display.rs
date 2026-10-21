//! Bring-up of the screen: ramfb, boot splash, then the text console mirroring `kprintln!`.

use crate::{fbcon, kprintln, ramfb, sink, splash};

/// How long the boot splash stays up.
const SPLASH_MS: u64 = 1000;

/// Initialise the display. Does nothing (and logs to the UART only) if qemu has no ramfb.
pub fn init() {
    let Some(mut fb) = ramfb::init() else {
        kprintln!("display: no ramfb device, serial only");
        return;
    };
    splash::show(&mut fb, SPLASH_MS, fbcon::DEFAULT_BG);
    let console = fbcon::Console::new(fb);
    let (cols, rows) = console.size();
    sink::install_screen(console);
    kprintln!(
        "display: ramfb {}x{} XRGB8888, console {}x{} cells",
        ramfb::WIDTH,
        ramfb::HEIGHT,
        cols,
        rows
    );
}
