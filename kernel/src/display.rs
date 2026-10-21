//! Bring-up of the screen: ramfb and the boot splash. The splash stays up until the desktop
//! (`wm`) takes the framebuffer with [`take_framebuffer`]; if it never can, [`console_fallback`]
//! puts the old text console mirroring `kprintln!` on screen instead.

use crate::gfx::Surface;
use crate::sync::SpinLock;
use crate::{fbcon, kprintln, ramfb, sink, splash};

/// How long the boot splash's progress bar takes.
const SPLASH_MS: u64 = 1000;

/// The framebuffer between `init` and the desktop taking it.
static FRAMEBUFFER: SpinLock<Option<Surface<'static>>> = SpinLock::new(None);

/// Initialise the display. Does nothing (and logs to the UART only) if qemu has no ramfb.
pub fn init() {
    let Some(mut fb) = ramfb::init() else {
        kprintln!("display: no ramfb device, serial only");
        return;
    };
    splash::show(&mut fb, SPLASH_MS, None);
    *FRAMEBUFFER.lock() = Some(fb);
    kprintln!(
        "display: ramfb {}x{} XRGB8888, splash up until the desktop starts",
        ramfb::WIDTH,
        ramfb::HEIGHT
    );
}

/// Hand the framebuffer to its one owner (the compositor). `None` if there is no display or it
/// was already taken.
pub fn take_framebuffer() -> Option<Surface<'static>> {
    FRAMEBUFFER.lock().take()
}

/// No desktop after all: show the text console (mirroring `kprintln!`) on `fb`.
pub fn console_fallback(mut fb: Surface<'static>) {
    use crate::gfx::DrawTarget;
    fb.clear(fbcon::DEFAULT_BG);
    let console = fbcon::Console::new(fb);
    let (cols, rows) = console.size();
    sink::install_screen(console);
    kprintln!("display: text console {cols}x{rows} cells");
}
