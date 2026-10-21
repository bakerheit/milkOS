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

pub struct Anim {
    pub target: Target,
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
        let dur = duration();
        let el = now.saturating_sub(self.start).min(dur);
        let t = (el * 1024 / dur) as i64;
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
        // One animation per window.
        if let Target::Live(id) = &target {
            let id = *id;
            self.anims
                .retain(|a| !matches!(a.target, Target::Live(x) if x == id));
        }
        self.damage(shadowed(&from.union(&to)));
        self.anims.push(Anim {
            target,
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
            self.add_anim(Target::Live(id), r, dock, 255, 90);
        }
    }

    pub(super) fn anim_restore(&mut self, id: u32) {
        if let Some(w) = self.win(id) {
            let r = w.rect;
            let dock = self.dock_slot(w.app);
            self.add_anim(Target::Live(id), dock, r, 90, 255);
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
        let radius = (R_WINDOW * r.w / w.rect.w.max(1)).max(2);
        p.blit_scaled_rrect(r, &w.surf, radius, alpha);
    }
}
