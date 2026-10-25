//! The present transform on the Raspberry Pi (docs/raspberry-pi.md, "HDMI displays"; the pure parts and the
//! reasons are in `rpikit::fbpresent`).
//!
//! With `milk.fbphys=` or `milk.fbrotate=` on the command line the firmware's framebuffer is the *physical* raster
//! (the display mode, for example 1024x768) and everything that draws (the desktop, the boot log console, the
//! crash screen, the touch calibration) draws on a *logical* framebuffer in ordinary cached RAM, `milk.fbsize=`
//! big. [`display`](crate::display) hands out the logical one (`size`, `framebuffer`, the surface the desktop
//! owns) and calls [`present`] wherever it used to do nothing: only the physical rectangles whose footprint touches
//! a changed logical rectangle are converted (rotated, stretched, area-weighted), straight into the firmware's
//! buffer. Without those options nothing here is active and the firmware's buffer is the desktop's, as before.
//!
//! The crash path uses the same conversion: no allocation, no lock (the tables and buffers are made once at
//! start-up and only read afterwards), so it works with interrupts off and other CPUs running.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use rpikit::fbpresent::{self as pk, Axis, PhysRequest, Plan, Span};
use rpikit::lcd::Rotation;

use super::{fb, mailbox};
use crate::gfx::{Rect, Surface};
use crate::kprintln;
use crate::mmu::{PAGE_SIZE, phys_to_virt};
use crate::pmm::{self, Use};

/// What is converted, how, and where. Made once, never freed, never written again.
struct State {
    plan: Plan,
    cols: Vec<Axis>,
    rows: Vec<Axis>,
    /// The logical picture: `lw * lh` words in RAM, stride `lw`.
    logical: *mut u32,
    /// The firmware's framebuffer: `pw * ph` words, stride `pw`.
    phys: *mut u32,
}

// SAFETY: the pointers name memory owned for good by the display; the State is read-only after it is published.
unsafe impl Send for State {}
unsafe impl Sync for State {}

static STATE: AtomicPtr<State> = AtomicPtr::new(core::ptr::null_mut());

fn state() -> Option<&'static State> {
    // SAFETY: null, or a leaked Box that lives for good.
    unsafe { STATE.load(Ordering::Acquire).as_ref() }
}

/// What the command line asks for.
#[derive(Clone, Copy)]
pub struct Wanted {
    pub phys: PhysRequest,
    pub turn: Rotation,
}

impl Wanted {
    /// Is any conversion asked for at all?
    pub fn any(&self) -> bool {
        self.phys != PhysRequest::Same || self.turn != Rotation::Deg0
    }
}

/// `milk.fbphys=` and `milk.fbrotate=`. A value that cannot be used is said so and ignored.
pub fn wanted() -> Wanted {
    let phys = pk::parse_phys(crate::fwcfg::option("fbphys")).unwrap_or_else(|why| {
        kprintln!("present: {why}");
        PhysRequest::Same
    });
    let turn = pk::parse_rotate(crate::fwcfg::option("fbrotate")).unwrap_or_else(|why| {
        kprintln!("present: {why}");
        Rotation::Deg0
    });
    Wanted { phys, turn }
}

/// The raster to ask the firmware for: the logical size (nothing asked for, or only a turn), the size the
/// firmware reports for its display mode (`milk.fbphys=auto`: 1024x768 when it reports nothing usable), or the
/// size given.
pub fn raster(w: &Wanted, logical: (usize, usize)) -> (usize, usize) {
    match w.phys {
        PhysRequest::Same => logical,
        PhysRequest::Forced(p, q) => (p as usize, q as usize),
        PhysRequest::Auto => {
            let fw = if mailbox::probe() { mailbox::physical_size() } else { None };
            let ((p, q), src) = rpikit::fbsize::choose(rpikit::fbsize::Request::Auto, fw);
            kprintln!("present: milk.fbphys=auto: raster {p}x{q}: {}", src.describe());
            (p as usize, q as usize)
        }
    }
}

/// Is the conversion in use?
pub fn active() -> bool {
    state().is_some()
}

/// The logical size, when the conversion is in use.
pub fn logical_size() -> Option<(usize, usize)> {
    state().map(|s| (s.plan.lw as usize, s.plan.lh as usize))
}

/// The logical framebuffer, when the conversion is in use.
pub fn logical_framebuffer() -> Option<*mut u32> {
    state().map(|s| s.logical)
}

fn log_ratio(prefix: &str, n: u32, d: u32) -> alloc::string::String {
    let mut b = [0u8; 16];
    let k = pk::ratio3(n, d, &mut b);
    alloc::format!("{prefix} {}", core::str::from_utf8(&b[..k]).unwrap_or("?"))
}

/// Set the conversion up for a logical `lw x lh` desktop over the firmware framebuffer that is now `pw x ph`
/// (what the firmware really gave). Returns the logical surface the desktop draws on, or `None` (the
/// firmware's own buffer stays the screen) when nothing is asked for, nothing needs converting or it cannot be done.
pub fn setup(logical: (usize, usize), actual: (usize, usize), w: &Wanted) -> Option<Surface<'static>> {
    if !w.any() {
        return None;
    }
    let (lw, lh) = (logical.0 as u32, logical.1 as u32);
    let (pw, ph) = (actual.0 as u32, actual.1 as u32);
    let plan = match Plan::new(lw, lh, pw, ph, w.turn) {
        Ok(p) => p,
        Err(e) => {
            kprintln!("present: {lw}x{lh} rotated {} into {pw}x{ph} cannot be done ({e:?}): the firmware's {pw}x{ph} is the screen", w.turn.degrees());
            return None;
        }
    };
    if plan.is_identity() {
        kprintln!("present: logical {lw}x{lh} is the firmware raster {pw}x{ph} and not turned: no conversion");
        return None;
    }
    let phys = fb::framebuffer()?;
    let pages = (lw as usize * lh as usize * 4).div_ceil(PAGE_SIZE);
    let Some(pa) = pmm::alloc_contiguous_zeroed_as(Use::Display, pages, 1) else {
        kprintln!("present: no memory for the {lw}x{lh} logical framebuffer: the firmware's {pw}x{ph} is the screen");
        return None;
    };
    let logical_ptr = phys_to_virt(pa) as *mut u32;
    let cols: Vec<Axis> = (0..pw).map(|x| plan.col(x)).collect();
    let rows: Vec<Axis> = (0..ph).map(|y| plan.row(y)).collect();
    let (rw, rh) = plan.rotated();
    let ax = cols.iter().map(|a| a.n).max().unwrap_or(0);
    let ay = rows.iter().map(|a| a.n).max().unwrap_or(0);
    kprintln!(
        "present: logical {lw}x{lh} -> rotate {} clockwise -> {rw}x{rh} -> stretch ({}, {}) -> firmware raster {pw}x{ph}",
        w.turn.degrees(),
        log_ratio("x", pw, rw),
        log_ratio("y", ph, rh)
    );
    let corner = |x: u32, y: u32| plan.point_to_physical(x, y).unwrap_or((0, 0));
    let (a, b, c, d) = (corner(0, 0), corner(lw - 1, 0), corner(lw - 1, lh - 1), corner(0, lh - 1));
    kprintln!(
        "present: logical corners ({},{}) -> ({},{}), ({},{}) -> ({},{}), ({},{}) -> ({},{}), ({},{}) -> ({},{})",
        0,
        0,
        a.0,
        a.1,
        lw - 1,
        0,
        b.0,
        b.1,
        lw - 1,
        lh - 1,
        c.0,
        c.1,
        0,
        lh - 1,
        d.0,
        d.1
    );
    let s = Box::new(State {
        plan,
        cols,
        rows,
        logical: logical_ptr,
        phys,
    });
    let t0 = counter();
    let s = Box::leak(s);
    // Black first (the logical buffer is zeroed), so the first thing on the glass is a clean frame.
    run(s, Span { x0: 0, y0: 0, x1: pw, y1: ph });
    let us = (counter() - t0) * 1_000_000 / counter_hz();
    STATE.store(s as *mut State, Ordering::Release);
    kprintln!(
        "present: area-weighted filter, up to {ax} taps across and {ay} down, tables {} entries, logical buffer {} KiB; a full {pw}x{ph} frame took {us} us",
        pw + ph,
        pages * PAGE_SIZE / 1024
    );
    // SAFETY: the logical buffer is ours for good (lw * lh words); the desktop owns this one surface.
    Some(unsafe { Surface::from_raw(logical_ptr, lw as usize, lh as usize, lw as usize) })
}

/// The system counter and its frequency: the display comes up before the timer is initialised, but a conversion
/// should still be timed (the log of a real board then says what a frame costs there).
fn counter() -> u64 {
    let v: u64;
    // SAFETY: reading the generic counter has no side effects.
    unsafe { core::arch::asm!("isb", "mrs {}, cntpct_el0", out(reg) v, options(nomem, nostack)) };
    v
}

fn counter_hz() -> u64 {
    let v: u64;
    // SAFETY: as above.
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) v, options(nomem, nostack)) };
    // The firmware sets it; if it did not, the Pi's 54 MHz (as the timer driver assumes).
    if v == 0 { 54_000_000 } else { v }
}

/// Convert one physical rectangle.
fn run(s: &State, span: Span) {
    if span.is_empty() {
        return;
    }
    let (lw, lh) = (s.plan.lw as usize, s.plan.lh as usize);
    let (pw, ph) = (s.plan.pw as usize, s.plan.ph as usize);
    // SAFETY: the two buffers are exactly these sizes and live for good; another CPU converting the same
    // region at the same time writes the same words (the data race is benign, as it is for the framebuffer itself).
    let (src, dst) = unsafe {
        (
            core::slice::from_raw_parts(s.logical, lw * lh),
            core::slice::from_raw_parts_mut(s.phys, pw * ph),
        )
    };
    pk::convert(&s.plan, &s.cols, &s.rows, src, lw, dst, pw, span);
}

/// Show the logical rectangles `rects`: convert the physical pixels they touch. False (nothing done) when the
/// conversion is not in use.
pub fn present(rects: &[Rect]) -> bool {
    let Some(s) = state() else {
        return false;
    };
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
    true
}

/// Show the whole logical picture. False when the conversion is not in use.
pub fn present_all() -> bool {
    let Some(s) = state() else {
        return false;
    };
    run(s, Span { x0: 0, y0: 0, x1: s.plan.pw, y1: s.plan.ph });
    true
}

/// `milk.fbtest=1`: fill the logical screen with the calibration pattern (`rpikit::fbpresent::pattern_pixel`) and
/// show it. A photograph of the real panel then shows the geometry at a glance: a white border all the way round,
/// a red, green, blue and yellow marker in the logical top left, top right, bottom right and bottom left corners,
/// cross-hairs, a diagonal, colour bars and a grid whose squares are square when nothing is distorted.
pub fn fbtest(mut fb: Surface<'static>) {
    use crate::gfx::DrawTarget;
    let (w, h) = (fb.width(), fb.height());
    let stride = fb.stride();
    let px = fb.pixels_mut();
    for y in 0..h {
        for x in 0..w {
            px[y * stride + x] = pk::pattern_pixel(w as u32, h as u32, x as u32, y as u32);
        }
    }
    crate::display::sync();
    kprintln!(
        "present: fbtest pattern {w}x{h} is on the screen (milk.fbtest=1): white border; markers red top left, green top right, blue bottom right, yellow bottom left (of the logical picture); cross-hairs, a magenta diagonal, 8 colour bars, a grid every {} px",
        pk::GRID
    );
}
