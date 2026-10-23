//! Dock magnification (macOS-style): as the pointer moves along the dock, each icon scales
//! with a smooth falloff over about three icon widths, the dock widens, and it all eases in
//! and out (150 ms) when the pointer enters or leaves. Integer maths only (soft-float).
//!
//! The bar does not move with the pointer: magnified it is the resting bar grown evenly to both
//! sides by as much as the icons can widen it at most (never past the screen), and the icons
//! are centred in it, so with the pointer at either end the dock only swells, it does not
//! slide. The icon under the pointer stays under it (it drifts by less than half its size, and
//! a demo clicking an icon's rest centre still lands on it). Icons stay bottom-aligned.
//!
//! While magnified the dock panel draws only the glass bar and the icons are drawn in the
//! compositor over everything (they rise above the bar), from the art's 96 px icons scaled
//! to size once per frame (crisp up to 2x of the 48 px dock).

use alloc::vec::Vec;

use crate::gfx::Rect;

use super::theme::*;

/// Ease time for entering / leaving (ns).
pub const EASE_NS: u64 = 150_000_000;
/// Falloff radius in icon pitches (the influence reaches this far to each side).
const RADIUS_PITCHES: i32 = 3;
/// Fixed point one.
const ONE: i32 = 1024;

/// Magnification state, owned by the desktop.
#[derive(Clone, Copy, Debug)]
pub struct Mag {
    /// Pointer x while over the dock (kept while easing out).
    pub x: i32,
    /// 0..=ONE: how far magnified (before easing).
    pub t: i32,
    /// Moving towards ONE (pointer over the dock) or 0.
    pub target: i32,
    pub last_ns: u64,
}

impl Mag {
    pub const fn new() -> Mag {
        Mag {
            x: 0,
            t: 0,
            target: 0,
            last_ns: 0,
        }
    }

    /// Advance the ease to `now`. True if `t` changed (or is still moving).
    pub fn step(&mut self, now: u64) -> bool {
        // slow- demos stretch it like the other animations.
        let slow = super::anim::SLOWMO
            .load(core::sync::atomic::Ordering::Relaxed)
            .max(1);
        let ease = EASE_NS * slow;
        let dt = now.saturating_sub(self.last_ns).min(ease);
        self.last_ns = now;
        if self.t == self.target {
            return false;
        }
        let d = ((ONE as u64 * dt) / ease).max(1) as i32;
        self.t = if self.t < self.target {
            (self.t + d).min(self.target)
        } else {
            (self.t - d).max(self.target)
        };
        true
    }

    /// The eased amount (smoothstep of t), 0..=ONE.
    pub fn eased(&self) -> i32 {
        smoothstep(self.t)
    }
}

/// 3x^2 - 2x^3 on 0..=ONE.
fn smoothstep(x: i32) -> i32 {
    let x = x.clamp(0, ONE) as i64;
    let one = ONE as i64;
    ((x * x * (3 * one - 2 * x)) / (one * one)) as i32
}

/// One laid-out icon: left x, size (square), bottom-aligned at `bottom`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub x: i32,
    pub size: i32,
}

/// The dock as drawn: the glass bar and every icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub bar: Rect,
    pub slots: Vec<Slot>,
    /// The icons' bottom edge (the rest icon's bottom).
    pub bottom: i32,
    /// Magnified at all (icons drawn by the compositor, not the dock panel).
    pub magnified: bool,
}

impl Layout {
    /// Which icon is at (x, y): within its column, from its top down to the bar's bottom.
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        if y >= self.bar.y + self.bar.h {
            return None;
        }
        self.slots
            .iter()
            .position(|s| x >= s.x && x < s.x + s.size && y >= self.bottom - s.size)
    }

    /// Everything the magnified icons cover (with room for badges), for damage.
    pub fn icons_rect(&self) -> Rect {
        let top = self
            .slots
            .iter()
            .map(|s| self.bottom - s.size)
            .min()
            .unwrap_or(self.bar.y);
        let top = top.min(self.bar.y) - 10;
        Rect::new(
            self.bar.x - 12,
            top,
            self.bar.w + 24,
            self.bar.y + self.bar.h - top,
        )
    }
}

/// Lay out `n` icons of `base` px (gap and padding from the theme) at rest, or magnified to
/// `max_pct` percent around pointer x `px` by `eased` (0..=ONE).
pub fn layout(n: usize, base: i32, px: i32, eased: i32, max_pct: i32) -> Layout {
    let n_i = n as i32;
    let pitch = base + DOCK_GAP;
    let rest_w = 2 * DOCK_PAD + n_i * base + (n_i - 1).max(0) * DOCK_GAP;
    let rest_x = (screen_w() - rest_w) / 2;
    let bar_h = dock_h();
    let bar_y = screen_h() - DOCK_BOTTOM - bar_h;
    let bottom = bar_y + DOCK_PAD + base;
    let rest = |i: i32| rest_x + DOCK_PAD + i * pitch;
    if eased <= 0 || n == 0 || max_pct <= 100 {
        return Layout {
            bar: Rect::new(rest_x, bar_y, rest_w, bar_h),
            slots: (0..n_i)
                .map(|i| Slot {
                    x: rest(i),
                    size: base,
                })
                .collect(),
            bottom,
            magnified: false,
        };
    }
    // Sizes from each icon's rest centre's distance to the pointer (cosine-like falloff), at
    // full magnification `extra` px bigger under the pointer.
    let radius = RADIUS_PITCHES * pitch;
    let grow = |px: i32, extra: i32| -> Vec<i32> {
        (0..n_i)
            .map(|i| {
                let c = rest(i) + base / 2;
                let x = ((c - px).abs() * ONE / radius.max(1)).min(ONE);
                let f = ONE - smoothstep(x); // 1 at the pointer, 0 at the radius
                (extra as i64 * f as i64 / ONE as i64) as i32
            })
            .collect()
    };
    let mut extra = (max_pct - 100) * base / 100;
    // The widest the icons get over all pointer places (a quarter pitch apart): the bar is that
    // wide whenever magnified, so it never slides or breathes as the pointer moves along it.
    let widest = |extra: i32| -> i32 {
        (0..4 * n_i)
            .map(|q| grow(rest(0) + q * pitch / 4, extra).iter().sum::<i32>())
            .max()
            .unwrap_or(0)
    };
    let mut wmax = widest(extra);
    // Never wider than the screen (4px from the edges): magnify less instead of clipping or
    // moving the dock.
    let room = screen_w() - 8 - rest_w;
    if wmax > room {
        extra = extra * room.max(0) / wmax.max(1);
        wmax = widest(extra);
    }
    let sizes: Vec<i32> = grow(px, extra)
        .into_iter()
        .map(|g| base + (g as i64 * eased as i64 / ONE as i64) as i32)
        .collect();
    // The bar: its rest place, grown evenly to both sides by `eased`; the icons are centred in it.
    let bar_w = rest_w + (wmax as i64 * eased as i64 / ONE as i64) as i32;
    let bar_x = (screen_w() - bar_w) / 2;
    let content: i32 = sizes.iter().sum::<i32>() + (n_i - 1).max(0) * DOCK_GAP;
    let mut x = bar_x + DOCK_PAD + (bar_w - 2 * DOCK_PAD - content) / 2;
    let mut xs = alloc::vec![0i32; n];
    for i in 0..n {
        xs[i] = x;
        x += sizes[i] + DOCK_GAP;
    }
    Layout {
        bar: Rect::new(bar_x, bar_y, bar_w, bar_h),
        slots: xs
            .into_iter()
            .zip(sizes)
            .map(|(x, size)| Slot { x, size })
            .collect(),
        bottom,
        magnified: true,
    }
}

/// Is (x, y) in the zone that magnifies a dock laid out as `l` at full size `max_px`?
pub fn in_zone(l: &Layout, max_px: i32, x: i32, y: i32) -> bool {
    let top = l.bottom - max_px - 6;
    y >= top && y < l.bar.y + l.bar.h && x >= l.bar.x - 8 && x < l.bar.x + l.bar.w + 8
}

// ---- the launch bounce ----------------------------------------------------------------------

/// One hop (up and down), ns.
pub const HOP_NS: u64 = 400_000_000;
/// How high a hop goes, as a fraction of the icon (x/1000).
const HOP_H: i32 = 450;
/// Give up bouncing after this long (the app never showed a window).
const BOUNCE_MAX_NS: u64 = 10_000_000_000;

/// An icon hopping while its app starts (macOS-style): full hops while waiting, then once
/// the app's window is up the hop in flight lands and one small last hop follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounce {
    pub tile: usize,
    pub start_ns: u64,
    /// When the app's first window appeared.
    pub ready_ns: Option<u64>,
}

fn slow() -> u64 {
    super::anim::SLOWMO
        .load(core::sync::atomic::Ordering::Relaxed)
        .max(1)
}

impl Bounce {
    /// How far up the icon is at `now` (px, for an icon of `size`), or None once landed.
    /// Each hop is a gravity parabola (fast up, slow at the top, fast down).
    pub fn lift(&self, now: u64, size: i32) -> Option<i32> {
        let hop = HOP_NS * slow();
        let e = now.saturating_sub(self.start_ns);
        if e > BOUNCE_MAX_NS * slow() {
            return None;
        }
        let k = e / hop;
        // Hops at full height until ready; then the current one lands, one half-height
        // hop follows (at least two hops in all, so an instant app still gets a bounce).
        let last_full = match self.ready_ns {
            Some(r) => r.saturating_sub(self.start_ns) / hop,
            None => u64::MAX,
        };
        let h = if k <= last_full {
            HOP_H
        } else if k == last_full + 1 {
            HOP_H * 4 / 10
        } else {
            return None;
        };
        let u = ((e % hop) * 1024 / hop) as i64; // 0..1024 through this hop
        let y = 4 * u * (1024 - u) / 1024; // 0..1024 at the top
        Some((size as i64 * h as i64 * y / (1000 * 1024)) as i32)
    }
}

// ---- apps added and removed: the neighbours slide aside ----------------------------------

/// How long the dock takes to make room for a new icon (or close a gap).
pub const SLIDE_NS: u64 = 260_000_000;

/// The dock as it was before its tiles changed, by tile name.
#[derive(Clone, Debug)]
pub struct Slide {
    pub start_ns: u64,
    pub from: Vec<(alloc::string::String, i32, i32)>,
    pub from_bar: Rect,
}

impl Slide {
    /// The eased progress (0..=ONE), or None when done.
    pub fn progress(&self, now: u64) -> Option<i32> {
        let d = SLIDE_NS * slow();
        let e = now.saturating_sub(self.start_ns);
        (e < d).then(|| smoothstep((e * ONE as u64 / d) as i32))
    }

    /// `l` part of the way from where things were: old icons slide from their old places,
    /// new ones grow from nothing in theirs, the bar follows.
    pub fn apply(&self, l: &mut Layout, names: &[alloc::string::String], e: i32) {
        let lerp = |a: i32, b: i32| a + (b - a) * e / ONE;
        for (s, n) in l.slots.iter_mut().zip(names) {
            match self.from.iter().find(|(m, _, _)| m == n) {
                Some(&(_, x, size)) => {
                    s.x = lerp(x, s.x);
                    s.size = lerp(size, s.size);
                }
                None => {
                    let size = lerp(0, s.size).max(1);
                    s.x += (s.size - size) / 2;
                    s.size = size;
                }
            }
        }
        let b = self.from_bar;
        l.bar = Rect::new(lerp(b.x, l.bar.x), l.bar.y, lerp(b.w, l.bar.w), l.bar.h);
        l.magnified = true;
    }
}
