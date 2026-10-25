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
/// The Pi's 3.5-inch SPI panel as the screen (no HDMI framebuffer): the desktop draws in RAM and
/// the panel shows it, scaled down (rpi/lcd.rs).
#[cfg(feature = "raspi")]
const LCDFB: u8 = 4;
static BACKEND: AtomicU8 = AtomicU8::new(NONE);

fn backend() -> u8 {
    BACKEND.load(Ordering::Acquire)
}

/// The framebuffer between `init` and the desktop taking it.
static FRAMEBUFFER: SpinLock<Option<Surface<'static>>> = SpinLock::new(None);

/// Initialise the display. Does nothing (and logs to the UART only) without a display device.
pub fn init() {
    #[cfg(feature = "raspi")]
    {
        use crate::rpi::{fb, lcd, present};
        // The panel on the GPIO header (if the command line asked for one and it came up) shows
        // the boot log first: it is the only screen a board without an HDMI cable has.
        lcd::boot_console();
        let hdmi = if lcd::primary_wanted() {
            None
        } else {
            // The size the firmware's display mode has (800x480 for the 4-inch HDMI panel), or
            // milk.fbsize=; 1024x768 when it says nothing usable. A size the firmware refuses
            // falls back to 1024x768 too.
            let (w, h) = fb::choose_size();
            // milk.fbphys= / milk.fbrotate=: the firmware's framebuffer may be another size than the desktop's
            // (rpi/present.rs); without them (rw, rh) is (w, h) and nothing else changes.
            let want = present::wanted();
            let (rw, rh) = present::raster(&want, (w, h));
            fb::init(rw, rh)
                .or_else(|| {
                    if (rw, rh) == (WIDTH, HEIGHT) {
                        return None;
                    }
                    kprintln!("rpi fb: {rw}x{rh} did not work, trying {WIDTH}x{HEIGHT}");
                    fb::init(WIDTH, HEIGHT)
                })
                .map(|surface| {
                    // What the firmware really gave is the physical raster; the desktop draws {w}x{h} if asked to convert.
                    present::setup((w, h), fb::size(), &want).unwrap_or(surface)
                })
        };
        if let Some(fb) = hdmi {
            BACKEND.store(RPIFB, Ordering::Release);
            let (w, h) = size();
            let name = if present::active() {
                "rpi mailbox framebuffer (converted into the firmware raster)"
            } else {
                "rpi mailbox framebuffer"
            };
            return finish_init_boot_log(fb, name, w, h);
        }
        if lcd::ready()
            && let Some(fb) = lcd::primary_surface(WIDTH, HEIGHT)
        {
            BACKEND.store(LCDFB, Ordering::Release);
            *FRAMEBUFFER.lock() = Some(fb);
            kprintln!(
                "display: the SPI panel is the screen, the desktop draws {WIDTH}x{HEIGHT} in RAM and the panel shows it"
            );
            return;
        }
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

/// How many times the 8x16 console font is magnified on the boot log and the crash screen: 1 on
/// a 1024-wide screen, 2 from 1600 up, or what the `fbscale` boot option (command line
/// `milk.fbscale=N`) says.
pub fn console_scale() -> usize {
    let (w, _) = size();
    let mut buf = [0u8; 4];
    let opt = crate::fwcfg::find_file("opt/milkos/fbscale")
        .filter(|f| (f.size as usize) <= buf.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut buf[..f.size as usize]))
        .and_then(|f| {
            core::str::from_utf8(&buf[..f.size as usize])
                .ok()?
                .trim()
                .parse::<usize>()
                .ok()
        });
    opt.unwrap_or(if w >= 1600 { 2 } else { 1 }).clamp(1, 4)
}

/// Physical address of the console's RAM copy of the screen (0: none) and its size in pages.
static SHADOW_PA: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static SHADOW_PAGES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// The RAM copy of the screen the text consoles draw on (allocated on first use, never freed: the crash
/// screen needs it when the desktop has long taken the framebuffer over). `None` without memory.
pub fn console_shadow(w: usize, h: usize) -> Option<Surface<'static>> {
    let pages = (w * h * 4).div_ceil(crate::mmu::PAGE_SIZE);
    if SHADOW_PA.load(Ordering::Acquire) == 0 {
        let pa = crate::pmm::alloc_contiguous_zeroed_as(crate::pmm::Use::Display, pages, 1)?;
        SHADOW_PAGES.store(pages, Ordering::Release);
        SHADOW_PA.store(pa, Ordering::Release);
    }
    if SHADOW_PAGES.load(Ordering::Acquire) < pages {
        return None; // a bigger screen than the copy was made for
    }
    let va = crate::mmu::phys_to_virt(SHADOW_PA.load(Ordering::Acquire));
    // SAFETY: pages we own for good; the one console that draws on it at a time is serialised by its caller.
    Some(unsafe { Surface::from_raw(va as *mut u32, w, h, w) })
}

/// The Raspberry Pi's start: no serial cable, so the screen is the console. The kernel log so far
/// is replayed onto it and `kprintln!` is mirrored there until the desktop takes the screen over
/// ([`take_framebuffer`]); a crash shows its text where the boot log was ([`crate::crashscreen`]).
/// No splash.
#[cfg(feature = "raspi")]
fn finish_init_boot_log(fb: Surface<'static>, name: &str, w: usize, h: usize) {
    // SAFETY: a second view of the framebuffer; only the boot console draws until the desktop starts.
    let view =
        unsafe { Surface::from_raw(framebuffer().unwrap_or(core::ptr::null_mut()), w, h, w) };
    *FRAMEBUFFER.lock() = Some(fb);
    // The console works in a RAM copy of the screen (video memory is slow to read, and scrolling reads it)
    // and writes the changed rows out. The copy is kept for the crash screen.
    let mut console = match console_shadow(w, h) {
        Some(shadow) => fbcon::Console::with_shadow(view, shadow, console_scale()),
        None => fbcon::Console::with_scale(view, console_scale()),
    };
    // What was printed before the screen existed.
    crate::klog::read_since(0, |bytes| {
        for chunk in bytes.utf8_chunks() {
            console.write_str(chunk.valid());
        }
    });
    console.flush();
    sink::install_screen(console);
    #[cfg(feature = "raspi")]
    {
        band();
        // The whole picture to the glass (a no-op unless the present transform is in use).
        sync();
    }
    kprintln!(
        "display: {name} {w}x{h} XRGB8888, the boot log is on screen until the desktop starts"
    );
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
    // The desktop draws the whole screen from here on: the boot console steps aside.
    if sink::screen_installed() {
        sink::remove_screen();
    }
    let fb = FRAMEBUFFER.lock().take();
    // The SPI panel, if there is one, shows the desktop too from here on.
    #[cfg(feature = "raspi")]
    if fb.is_some() {
        // The touch calibration helper (milk.touch_cal=ask) has the screen until it is done.
        crate::rpi::touch::desktop_starting();
        crate::rpi::lcd::desktop_started();
    }
    fb
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
        RPIFB => crate::rpi::present::logical_size().unwrap_or_else(crate::rpi::fb::size),
        #[cfg(feature = "raspi")]
        LCDFB => crate::rpi::lcd::primary_size(),
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
        RPIFB => crate::rpi::present::logical_framebuffer().or_else(crate::rpi::fb::framebuffer),
        #[cfg(feature = "raspi")]
        LCDFB => crate::rpi::lcd::primary_framebuffer(),
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
        RPIFB if crate::rpi::present::active() => {
            // The logical size is the one the command line gave; the firmware's raster is not ours to change.
            kprintln!("display: {w}x{h} refused: the desktop is presented through milk.fbphys/milk.fbrotate at a fixed size");
            None
        }
        #[cfg(feature = "raspi")]
        RPIFB => crate::rpi::fb::set_mode(w, h),
        #[cfg(feature = "raspi")]
        LCDFB => crate::rpi::lcd::primary_set_mode(w, h),
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
    // The Pi's logical framebuffer goes to the firmware's raster, rotated and stretched (rpi/present.rs); a no-op without it.
    #[cfg(feature = "raspi")]
    crate::rpi::present::present(rects);
    // The SPI panel shows what the desktop drew, in its own time.
    #[cfg(feature = "raspi")]
    crate::rpi::lcd::note_source(rects);
}

/// Show the whole framebuffer and wait until it is on screen, so the caller may draw again
/// without a [`wait`]. For the slow paths (splash, console, cursor, fade).
pub fn sync() {
    if backend() == GPU {
        let (w, h) = size();
        gpu::present(&[Rect::new(0, 0, w as i32, h as i32)]);
        gpu::wait();
    }
    #[cfg(feature = "raspi")]
    {
        crate::rpi::present::present_all();
        crate::rpi::lcd::note_source_all();
    }
}

/// [`sync`] for the pixel rows `[lo, hi)` only (a text console that knows which rows it changed).
pub fn sync_rows(lo: usize, hi: usize) {
    let (w, h) = size();
    let (lo, hi) = (lo.min(h), hi.min(h));
    if hi <= lo {
        return;
    }
    let r = Rect::new(0, lo as i32, w as i32, (hi - lo) as i32);
    if backend() == GPU {
        gpu::present(&[r]);
        gpu::wait();
    }
    #[cfg(feature = "raspi")]
    {
        crate::rpi::present::present(&[r]);
        crate::rpi::lcd::note_source(&[r]);
    }
}

/// The green sign-of-life band across the top of the screen (the boot console clears the screen when it starts).
#[cfg(feature = "raspi")]
fn band() {
    if let Some(p) = framebuffer() {
        let (w, h) = size();
        crate::rpi::fb::sign_of_life(p as usize, w, h);
        sync_rows(0, crate::rpi::fb::BAND_H);
    }
}

/// Is the SPI panel the screen (there is no HDMI framebuffer)?
#[cfg(feature = "raspi")]
pub fn lcd_is_primary() -> bool {
    backend() == LCDFB
}

/// [`sync`] from the panic handler: never blocks on a lock someone else may hold.
pub fn sync_panic() {
    if backend() == GPU {
        let (w, h) = size();
        gpu::try_present(&[Rect::new(0, 0, w as i32, h as i32)]);
    }
    // No lock, no allocation: the tables and buffers were made at start-up.
    #[cfg(feature = "raspi")]
    crate::rpi::present::present_all();
}
