//! The screen: one framebuffer (XRGB8888, stride = width) behind a small interface, and the
//! boot splash. The splash stays up until the desktop (`wm`) takes the framebuffer with
//! [`take_framebuffer`]; if it never can, [`console_fallback`] puts the old text console
//! mirroring `kprintln!` on screen instead.
//!
//! Three backends. virtio-gpu (`virtio::gpu`) when qemu has `-device virtio-gpu-device`: frames
//! reach the screen only when presented, whole, so nothing tears. ramfb otherwise (old command
//! lines): qemu scans the buffer out of RAM whenever it likes, [`wait`] and [`present`] are
//! no-ops. On the Raspberry Pi the firmware's framebuffer (`rpi::fb`, through the mailbox),
//! which behaves like ramfb. Whoever writes framebuffer pixels follows one protocol, which suits both:
//! [`wait`], draw, then [`present`] the rects drawn (or [`sync`] for the whole screen).

use core::sync::atomic::{AtomicU8, Ordering};

use crate::gfx::{Rect, Surface};
use crate::sync::SpinLock;
use crate::virtio::gpu;
use crate::{fbcon, kprintln, ramfb, sink, splash};

/// The mode at boot.
pub const WIDTH: usize = ramfb::WIDTH;
pub const HEIGHT: usize = ramfb::HEIGHT;

/// How long the boot splash's progress bar takes.
const SPLASH_MS: u64 = 1000;

const NONE: u8 = 0;
const RAMFB: u8 = 1;
const GPU: u8 = 2;
/// The Pi's mailbox framebuffer.
#[cfg(feature = "raspi")]
const RPIFB: u8 = 3;
static BACKEND: AtomicU8 = AtomicU8::new(NONE);

fn backend() -> u8 {
    BACKEND.load(Ordering::Acquire)
}

/// The framebuffer between `init` and the desktop taking it.
static FRAMEBUFFER: SpinLock<Option<Surface<'static>>> = SpinLock::new(None);

/// Initialise the display. Does nothing (and logs to the UART only) without a display device.
pub fn init() {
    #[cfg(feature = "raspi")]
    if let Some(fb) = crate::rpi::fb::init(WIDTH, HEIGHT) {
        BACKEND.store(RPIFB, Ordering::Release);
        let (w, h) = crate::rpi::fb::size();
        return finish_init(fb, "rpi mailbox framebuffer", w, h);
    }
    let (fb, name) = match gpu::find().and_then(|t| gpu::init(t, WIDTH, HEIGHT)) {
        Some(fb) => {
            BACKEND.store(GPU, Ordering::Release);
            (fb, "virtio-gpu")
        }
        None => match ramfb::init() {
            Some(fb) => {
                BACKEND.store(RAMFB, Ordering::Release);
                (fb, "ramfb")
            }
            None => {
                kprintln!("display: no virtio-gpu or ramfb device, serial only");
                return;
            }
        },
    };
    finish_init(fb, name, WIDTH, HEIGHT)
}

/// The splash, the framebuffer parked for the desktop and the log line.
fn finish_init(mut fb: Surface<'static>, name: &str, w: usize, h: usize) {
    splash::show(&mut fb, SPLASH_MS, None);
    *FRAMEBUFFER.lock() = Some(fb);
    kprintln!("display: {name} {w}x{h} XRGB8888, splash up until the desktop starts");
}

/// Hand the framebuffer to its one owner (the compositor). `None` if there is no display or it
/// was already taken.
pub fn take_framebuffer() -> Option<Surface<'static>> {
    FRAMEBUFFER.lock().take()
}

/// No desktop after all: show the text console (mirroring `kprintln!`) on `fb`.
pub fn console_fallback(mut fb: Surface<'static>) {
    use crate::gfx::DrawTarget;
    wait();
    fb.clear(fbcon::DEFAULT_BG);
    sync();
    let console = fbcon::Console::new(fb);
    let (cols, rows) = console.size();
    sink::install_screen(console);
    kprintln!("display: text console {cols}x{rows} cells");
}

/// The current mode: (width, height).
pub fn size() -> (usize, usize) {
    match backend() {
        GPU => gpu::framebuffer().map_or((WIDTH, HEIGHT), |(_, w, h)| (w, h)),
        #[cfg(feature = "raspi")]
        RPIFB => crate::rpi::fb::size(),
        _ => ramfb::size(),
    }
}

/// Raw pointer to the live framebuffer ([`size`], stride = width), or `None` without a
/// display. For overlays and the panic screen; they follow the module's protocol too.
pub fn framebuffer() -> Option<*mut u32> {
    match backend() {
        GPU => gpu::framebuffer().map(|(p, _, _)| p),
        RAMFB => ramfb::framebuffer(),
        #[cfg(feature = "raspi")]
        RPIFB => crate::rpi::fb::framebuffer(),
        _ => None,
    }
}

/// Switch to `w` x `h`: returns the new framebuffer's surface; the old one is freed, so the
/// caller must stop using its old surface first. `None` (nothing changed) on failure.
/// Thread context only.
pub fn set_mode(w: usize, h: usize) -> Option<Surface<'static>> {
    match backend() {
        GPU => gpu::set_mode(w, h),
        RAMFB => ramfb::set_mode(w, h),
        #[cfg(feature = "raspi")]
        RPIFB => crate::rpi::fb::set_mode(w, h),
        _ => None,
    }
}

/// Wait until the display no longer reads the framebuffer. Call before writing pixels.
pub fn wait() {
    if backend() == GPU {
        gpu::wait();
    }
}

/// Show `rects` of the framebuffer, as one frame. Returns at once (see [`wait`]).
pub fn present(rects: &[Rect]) {
    if backend() == GPU {
        gpu::present(rects);
    }
}

/// Show the whole framebuffer and wait until it is on screen, so the caller may draw again
/// without a [`wait`]. For the slow paths (splash, console, cursor, fade).
pub fn sync() {
    if backend() == GPU {
        let (w, h) = size();
        gpu::present(&[Rect::new(0, 0, w as i32, h as i32)]);
        gpu::wait();
    }
}

/// [`sync`] from the panic handler: never blocks on a lock someone else may hold.
pub fn sync_panic() {
    if backend() == GPU {
        let (w, h) = size();
        gpu::try_present(&[Rect::new(0, 0, w as i32, h as i32)]);
    }
}
