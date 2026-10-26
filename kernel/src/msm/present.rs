//! The desktop on the Pixel 2's glass. The window system draws a *logical* picture (`msm.desk=WxH`, by default half
//! the glass in each direction, so that text made for a 1024x768 screen is still legible on a 440 ppi panel) in
//! ordinary cached RAM. This module turns it into the glass: turned by `msm.rotate=` (a quarter turn puts a
//! landscape desktop on the portrait glass: turn the phone), stretched to the glass's raster with the Raspberry Pi's
//! area-weighted converter (`rpikit::fbpresent`, the same code and the same tests: a flat colour stays exactly that
//! colour, an exact 2x is a pixel doubling), and packed into the bootloader's framebuffer in its byte order, row
//! range by row range, with the cache lines cleaned for the display controller ([`super::fb::present`]).
//!
//! Only the physical rows that a changed logical rectangle touches are converted and sent. Nothing here allocates
//! after [`setup`] or takes a lock, so the crash path can call [`present_all`].

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use rpikit::fbpresent::{self as pk, Axis, Plan, Span};
use rpikit::msm::Opts;

use crate::gfx::{Rect, Surface};
use crate::kprintln;
use crate::mmu::{PAGE_SIZE, phys_to_virt};
use crate::pmm::{self, Use};

struct State {
    plan: Plan,
    cols: Vec<Axis>,
    rows: Vec<Axis>,
    /// The desktop's picture: `lw * lh` words, stride `lw`.
    logical: *mut u32,
    /// The glass-sized XRGB picture the conversion fills before it is packed (the logical buffer itself when nothing
    /// needs converting).
    raster: *mut u32,
    identity: bool,
}

// SAFETY: the pointers name memory owned for good by the display; the State is read-only after it is published.
unsafe impl Send for State {}
unsafe impl Sync for State {}

static STATE: AtomicPtr<State> = AtomicPtr::new(core::ptr::null_mut());
/// The desktop owns the glass. Until then the boot console does (it sends its own rows through `fb::present`), and
/// the display layer's "show the screen" calls, which the console's draws also make, must not copy the desktop's
/// still empty picture over it.
static LIVE: AtomicBool = AtomicBool::new(false);

/// The desktop takes the glass over (`display::take_framebuffer`).
pub fn start() {
    LIVE.store(true, Ordering::Release);
}

fn state() -> Option<&'static State> {
    // SAFETY: null, or a leaked Box that lives for good.
    unsafe { STATE.load(Ordering::Acquire).as_ref() }
}

fn alloc_words(words: usize) -> Option<*mut u32> {
    let pages = (words * 4).div_ceil(PAGE_SIZE);
    let pa = pmm::alloc_contiguous_zeroed_as(Use::Display, pages, 1)?;
    Some(phys_to_virt(pa) as *mut u32)
}

/// Set the conversion up for the glass `fb::geometry()` describes. Returns the surface the desktop draws on, or
/// `None` (nothing changed, the log says why).
pub fn setup(opts: &Opts) -> Option<Surface<'static>> {
    let Some(g) = super::fb::geometry() else {
        kprintln!("present: no framebuffer, no desktop");
        return None;
    };
    let (pw, ph) = (g.width, g.height);
    let (lw, lh) = rpikit::msm::desktop_size(pw, ph, opts.rotate, opts.desk);
    let plan = match Plan::new(lw as u32, lh as u32, pw as u32, ph as u32, opts.rotate) {
        Ok(p) => p,
        Err(e) => {
            kprintln!(
                "present: desktop {lw}x{lh} turned {} onto {pw}x{ph} cannot be done ({e:?}): try msm.desk=",
                opts.rotate.degrees()
            );
            return None;
        }
    };
    let identity = plan.is_identity();
    let Some(logical) = alloc_words(lw * lh) else {
        kprintln!("present: no memory for the {lw}x{lh} desktop");
        return None;
    };
    let raster = if identity {
        logical
    } else {
        match alloc_words(pw * ph) {
            Some(r) => r,
            None => {
                kprintln!("present: no memory for the {pw}x{ph} glass-sized buffer");
                return None;
            }
        }
    };
    let cols: Vec<Axis> = (0..pw as u32).map(|x| plan.col(x)).collect();
    let rows: Vec<Axis> = (0..ph as u32).map(|y| plan.row(y)).collect();
    let (rw, rh) = plan.rotated();
    let mut b = [0u8; 16];
    let k = pk::ratio3(pw as u32, rw, &mut b);
    let sx = core::str::from_utf8(&b[..k]).unwrap_or("?");
    let mut b2 = [0u8; 16];
    let k = pk::ratio3(ph as u32, rh, &mut b2);
    let sy = core::str::from_utf8(&b2[..k]).unwrap_or("?");
    if identity {
        kprintln!("present: desktop {lw}x{lh} is the glass {pw}x{ph}: no conversion");
    } else {
        kprintln!(
            "present: desktop {lw}x{lh} -> turn {} clockwise -> {rw}x{rh} -> stretch x{sx} y{sy} -> glass {pw}x{ph} {} stride {}",
            opts.rotate.degrees(),
            g.fmt.name(),
            g.stride
        );
    }
    let s = Box::leak(Box::new(State { plan, cols, rows, logical, raster, identity }));
    STATE.store(s as *mut State, Ordering::Release);
    // SAFETY: the logical buffer is ours for good (lw * lh words); the desktop owns this one surface.
    Some(unsafe { Surface::from_raw(logical, lw, lh, lw) })
}

/// The desktop's size, once [`setup`] has run.
pub fn logical_size() -> Option<(usize, usize)> {
    state().map(|s| (s.plan.lw as usize, s.plan.lh as usize))
}

/// The desktop's picture.
pub fn logical_framebuffer() -> Option<*mut u32> {
    state().map(|s| s.logical)
}

/// Convert the glass rows the physical span covers and send them.
fn run(s: &State, span: Span) {
    if span.is_empty() {
        return;
    }
    let (pw, ph) = (s.plan.pw as usize, s.plan.ph as usize);
    if !s.identity {
        let (lw, lh) = (s.plan.lw as usize, s.plan.lh as usize);
        // SAFETY: both buffers are exactly these sizes and live for good; another CPU converting the same
        // region at the same time writes the same words (benign, as it is for the framebuffer itself).
        let (src, dst) = unsafe {
            (
                core::slice::from_raw_parts(s.logical, lw * lh),
                core::slice::from_raw_parts_mut(s.raster, pw * ph),
            )
        };
        pk::convert(&s.plan, &s.cols, &s.rows, src, lw, dst, pw, span);
    }
    // SAFETY: the raster is pw * ph words that live for good.
    let shadow = unsafe { Surface::from_raw(s.raster, pw, ph, pw) };
    super::fb::present(&shadow, span.y0 as usize, (span.y1 as usize).min(ph));
}

/// Show the logical rectangles `rects` on the glass.
pub fn present(rects: &[Rect]) {
    let Some(s) = state().filter(|_| LIVE.load(Ordering::Acquire)) else { return };
    for r in rects {
        if r.w <= 0 || r.h <= 0 {
            continue;
        }
        let clip = |v: i32| v.max(0) as u32;
        let span = s.plan.rect_to_physical(Span {
            x0: clip(r.x),
            y0: clip(r.y),
            x1: clip(r.x.saturating_add(r.w)),
            y1: clip(r.y.saturating_add(r.h)),
        });
        run(s, span);
    }
}

/// Show the whole desktop.
pub fn present_all() {
    let Some(s) = state().filter(|_| LIVE.load(Ordering::Acquire)) else { return };
    run(s, Span { x0: 0, y0: 0, x1: s.plan.pw, y1: s.plan.ph });
}
