//! Window animations: open (grow + fade in), close (shrink + fade out), minimise (fly to the
//! dock) and restore (fly back). 150 ms, ease-out, and skippable: any click or key press
//! finishes them at once. (The SPEC needs none for 1.0; these are deliberately short.)
//!
//! While a window animates, the normal window pass skips it and [`Desktop::draw_anims`] draws
//! its surface scaled and faded at the interpolated rect.

use alloc::boxed::Box;

use crate::gfx::Rect;
use crate::timer;

use super::paint::{Painter, grow};
use super::window::Window;
use super::{Desktop, theme::*};

pub const DURATION_NS: u64 = 150_000_000;

/// Slow-motion factor (demo scripts named `slow-*` set 20 so screenshots catch a frame).
pub static SLOWMO: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

fn duration() -> u64 {
    DURATION_NS * SLOWMO.load(core::sync::atomic::Ordering::Relaxed)
}

pub enum Target {
    /// A window still in the list (open, restore, minimise).
    Live(u32),
    /// A window that has been closed; the animation owns it until it ends.
    Dead(Box<Window>),
}

/// How a minimise / restore moves: the genie (the window pours into its dock icon, the
/// bottom first) or a plain scale.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Scale,
    /// Into the dock (true) or out of it (false).
    Genie(bool),
}

/// The genie takes a little longer than the other animations (macOS's is about 0.5 s).
pub const GENIE_NS: u64 = 380_000_000;

pub struct Anim {
    pub target: Target,
    shape: Shape,
    /// 0..=1024 through the animation (the genie draws from this).
    t: i32,
    from: Rect,
    to: Rect,
    a0: u8,
    a1: u8,
    start: u64,
    /// Rect and alpha as of the last step (what gets drawn, and the damage to undo).
    last: Rect,
    alpha: u8,
}

/// `r` scaled by `k`/1000 about its centre.
fn scaled(r: Rect, k: i32) -> Rect {
    let (w, h) = (r.w * k / 1000, r.h * k / 1000);
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w.max(1), h.max(1))
}

/// `r` plus the window shadow's reach (SPEC: grow 10, down 12).
fn shadowed(r: &Rect) -> Rect {
    let g = grow(r, 10);
    Rect::new(g.x, g.y, g.w, g.h + 12)
}

fn lerp(a: i32, b: i32, t: i32) -> i32 {
    a + (b - a) * t / 1024
}

impl Anim {
    fn at(&self, now: u64) -> (Rect, u8, bool) {
        let dur = match self.shape {
            Shape::Scale => duration(),
            Shape::Genie(_) => GENIE_NS * SLOWMO.load(core::sync::atomic::Ordering::Relaxed),
        };
        let el = now.saturating_sub(self.start).min(dur);
        let t = (el * 1024 / dur) as i64;
        if let Shape::Genie(_) = self.shape {
            // Everything it can touch, for damage; the shape itself comes from `t`.
            return (self.from.union(&self.to), 255, el >= dur);
        }
        // Ease out (cubic).
        let inv = 1024 - t;
        let e = (1024 - inv * inv * inv / (1024 * 1024)) as i32;
        let r = Rect::new(
            lerp(self.from.x, self.to.x, e),
            lerp(self.from.y, self.to.y, e),
            lerp(self.from.w, self.to.w, e).max(1),
            lerp(self.from.h, self.to.h, e).max(1),
        );
        let a = lerp(self.a0 as i32, self.a1 as i32, e).clamp(0, 255) as u8;
        (r, a, el >= dur)
    }
}

impl Desktop {
    fn add_anim(&mut self, target: Target, from: Rect, to: Rect, a0: u8, a1: u8) {
        self.add_anim_shaped(target, from, to, a0, a1, Shape::Scale);
    }

    fn add_anim_shaped(
        &mut self,
        target: Target,
        from: Rect,
        to: Rect,
        a0: u8,
        a1: u8,
        shape: Shape,
    ) {
        // One animation per window.
        if let Target::Live(id) = &target {
            let id = *id;
            self.anims
                .retain(|a| !matches!(a.target, Target::Live(x) if x == id));
        }
        self.damage(shadowed(&from.union(&to)));
        self.anims.push(Anim {
            target,
            shape,
            t: 0,
            from,
            to,
            a0,
            a1,
            start: timer::now_ns(),
            last: from,
            alpha: a0,
        });
    }

    pub(super) fn anim_open(&mut self, id: u32) {
        if let Some(r) = self.win(id).map(|w| w.rect) {
            self.add_anim(Target::Live(id), scaled(r, 920), r, 0, 255);
        }
    }

    pub(super) fn anim_close(&mut self, w: Window) {
        let r = w.rect;
        self.add_anim(Target::Dead(Box::new(w)), r, scaled(r, 900), 255, 0);
    }

    pub(super) fn anim_minimise(&mut self, id: u32) {
        if let Some(w) = self.win(id) {
            let r = w.rect;
            let dock = self.dock_slot(w.app);
            self.add_anim_shaped(Target::Live(id), r, dock, 255, 90, Shape::Genie(true));
        }
    }

    pub(super) fn anim_restore(&mut self, id: u32) {
        if let Some(w) = self.win(id) {
            let r = w.rect;
            let dock = self.dock_slot(w.app);
            self.add_anim_shaped(Target::Live(id), r, dock, 255, 90, Shape::Genie(false));
        }
    }

    /// Is this window being drawn by an animation (so the normal pass skips it)?
    pub(super) fn animating(&self, id: u32) -> bool {
        self.anims
            .iter()
            .any(|a| matches!(a.target, Target::Live(x) if x == id))
    }

    /// Finish every animation now (input skips them).
    pub(super) fn skip_anims(&mut self) {
        for a in &mut self.anims {
            a.start = 0;
        }
    }

    /// Advance: damage what moved, drop finished animations. True while any run.
    pub(super) fn step_anims(&mut self, now: u64) -> bool {
        if self.anims.is_empty() {
            return false;
        }
        let mut dmg = alloc::vec::Vec::new();
        self.anims.retain_mut(|a| {
            let (r, alpha, done) = a.at(now);
            let dur = match a.shape {
                Shape::Scale => duration(),
                Shape::Genie(_) => GENIE_NS * SLOWMO.load(core::sync::atomic::Ordering::Relaxed),
            };
            a.t = (now.saturating_sub(a.start).min(dur) * 1024 / dur.max(1)) as i32;
            // Include the shadow area: the window's own shadow comes back when it ends.
            dmg.push(shadowed(&a.last.union(&r)));
            a.last = r;
            a.alpha = alpha;
            !done
        });
        for d in dmg {
            self.damage(d);
        }
        true
    }
}

/// Draw the running animations (after the windows, before the shell).
pub fn draw(anims: &[Anim], windows: &[Window], p: &mut Painter, d: Rect) {
    for a in anims {
        let (r, alpha) = (a.last, a.alpha);
        if !r.intersects(&d) || alpha == 0 {
            continue;
        }
        let w: &Window = match &a.target {
            Target::Live(id) => match windows.iter().find(|w| w.id == *id) {
                Some(w) => w,
                None => continue,
            },
            Target::Dead(w) => w,
        };
        if let Shape::Genie(into) = a.shape {
            // `from` is always the window, `to` the dock icon; restoring runs it backwards.
            let t = if into { a.t } else { 1024 - a.t };
            genie(p, d, &w.surf, a.from, a.to, t);
            continue;
        }
        let radius = (R_WINDOW * r.w / w.rect.w.max(1)).max(2);
        p.blit_scaled_rrect(r, &w.surf, radius, alpha);
    }
}

/// 3t^2 - 2t^3 on 0..=1024.
fn smooth(t: i32) -> i32 {
    let t = t.clamp(0, 1024) as i64;
    ((t * t * (3 * 1024 - 2 * t)) / (1024 * 1024)) as i32
}

/// The genie at `t` (0 = the window, 1024 = inside the dock icon `to`): its bottom heads for
/// the icon first while its sides pinch in towards it, then the top follows it down. Each
/// output row samples one row of the window, stretched between its interpolated edges.
fn genie(p: &mut Painter, d: Rect, surf: &super::buffer::Buffer, from: Rect, to: Rect, t: i32) {
    let fall = smooth(t * 1024 / 600); // the bottom reaches the icon by 60%
    let follow = smooth((t - 230) * 1024 / 794); // the top starts after about a fifth
    let pinch = smooth(t * 1024 / 520); // the sides pinch in early
    let bottom = lerp(from.y + from.h, to.y + to.h, fall);
    let top = lerp(from.y, to.y, follow).min(bottom - 1);
    let h = (bottom - top).max(1);
    let alpha = if t > 900 {
        (255 - (t - 900) * 2).clamp(0, 255) as u8
    } else {
        255
    };
    let y0 = top.max(d.y);
    let y1 = bottom.min(d.y + d.h);
    let mut acc: alloc::vec::Vec<[u32; 4]> = alloc::vec::Vec::new();
    // Edges in 1/256 px, source rows in 16.16: nothing rounds to whole pixels before the
    // filter, so the squeezed window stays continuous.
    let lerp256 =
        |a: i32, b: i32, w: i32| ((a as i64) << 8) + (((b - a) as i64) << 8) * w as i64 / 1024;
    for y in y0..y1 {
        let v = (2 * (y - top) + 1) * 512 / h; // the row's centre: 0 at the top, 1024 at the bottom
        // The bottom rows pinch the most; once the top is on its way, everything follows.
        let w = (pinch * smooth(v) / 1024).max(follow);
        let x0 = lerp256(from.x, to.x, w);
        let x1 = lerp256(from.x + from.w, to.x + to.w, w);
        // How far the edges move across this row (for anti-aliasing shallow edges).
        let wt = (pinch * smooth((y - top) * 1024 / h) / 1024).max(follow);
        let wb = (pinch * smooth((y + 1 - top) * 1024 / h) / 1024).max(follow);
        let slope = (
            lerp256(from.x, to.x, wb) - lerp256(from.x, to.x, wt),
            lerp256(from.x + from.w, to.x + to.w, wb) - lerp256(from.x + from.w, to.x + to.w, wt),
        );
        let sy0 = (((y - top) as i64) << 16) * from.h as i64 / h as i64;
        let sy1 = (((y + 1 - top) as i64) << 16) * from.h as i64 / h as i64;
        p.blit_span_filtered(y, x0, x1, surf, sy0, sy1, slope, R_WINDOW, alpha, &mut acc);
    }
}
