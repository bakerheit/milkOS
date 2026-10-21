//! Window snapping, a nod to tiling window managers: drag a window by its title bar to the
//! left or right screen edge for a half, into a corner for a quarter, to the top edge to fill
//! the work area. A translucent preview shows where it will land; letting go there snaps it.
//! Dragging a snapped (or zoomed) window away gives it back its old size. Keyboard:
//! Ctrl+Alt+Left / Right halves, Ctrl+Alt+Up fills, Ctrl+Alt+Down restores.

use crate::gfx::Rect;

use super::Desktop;
use super::paint::grow;
use super::theme::*;

/// Gap around and between snapped windows.
const GAP: i32 = 8;
/// How close to an edge (px) the pointer must be to snap.
const EDGE: i32 = 6;
/// Corner zones: this far from the top / bottom of the work area along a side edge.
const CORNER: i32 = 72;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Fill,
}

/// The rect a zone stands for, inside the work area.
pub fn rect(z: Zone) -> Rect {
    let wa = Desktop::work_area();
    let (x0, y0, w, h) = (wa.x + GAP, wa.y + GAP, wa.w - 2 * GAP, wa.h - 2 * GAP);
    let (hw, hh) = ((w - GAP) / 2, (h - GAP) / 2);
    match z {
        Zone::Fill => Rect::new(x0, y0, w, h),
        Zone::Left => Rect::new(x0, y0, hw, h),
        Zone::Right => Rect::new(x0 + hw + GAP, y0, w - hw - GAP, h),
        Zone::TopLeft => Rect::new(x0, y0, hw, hh),
        Zone::TopRight => Rect::new(x0 + hw + GAP, y0, w - hw - GAP, hh),
        Zone::BottomLeft => Rect::new(x0, y0 + hh + GAP, hw, h - hh - GAP),
        Zone::BottomRight => Rect::new(x0 + hw + GAP, y0 + hh + GAP, w - hw - GAP, h - hh - GAP),
    }
}

/// Which zone the pointer is in while dragging a window, if any.
pub fn zone_at(x: i32, y: i32) -> Option<Zone> {
    let wa = Desktop::work_area();
    let top = y < wa.y + CORNER;
    let bottom = y >= wa.y + wa.h - CORNER;
    if x <= EDGE {
        return Some(if top {
            Zone::TopLeft
        } else if bottom {
            Zone::BottomLeft
        } else {
            Zone::Left
        });
    }
    if x >= screen_w() - 1 - EDGE {
        return Some(if top {
            Zone::TopRight
        } else if bottom {
            Zone::BottomRight
        } else {
            Zone::Right
        });
    }
    if y <= MENU_H + 2 {
        return Some(Zone::Fill);
    }
    None
}

impl Desktop {
    /// Show (or move, or hide) the snap preview.
    pub(super) fn set_snap_preview(&mut self, z: Option<Zone>) {
        let r = z.map(rect);
        if r == self.snap_preview {
            return;
        }
        for old in [self.snap_preview, r].into_iter().flatten() {
            self.damage(grow(&old, 2));
        }
        self.snap_preview = r;
    }

    /// Snap window `id` into `z`, remembering its free-floating rect.
    pub(super) fn snap_to(&mut self, id: u32, z: Zone) {
        self.damage_window(id);
        if let Some(w) = self.win_mut(id) {
            let keep = w.restore.unwrap_or(w.rect);
            w.set_rect(rect(z));
            w.restore = Some(keep);
        }
        self.damage_window(id);
    }

    /// Back to the free-floating rect (Ctrl+Alt+Down).
    pub(super) fn unsnap(&mut self, id: u32) {
        self.damage_window(id);
        if let Some(w) = self.win_mut(id)
            && let Some(r) = w.restore.take()
        {
            w.set_rect(r);
        }
        self.damage_window(id);
    }

    /// A drag started on a snapped / zoomed window: give it its old size back, under the
    /// pointer at the same relative spot of the title bar. Returns the new grab offset.
    pub(super) fn unsnap_for_drag(&mut self, id: u32, px: i32, dx: i32) -> Option<(i32, i32)> {
        let w = self.win(id)?;
        let old = w.restore?;
        let cur = w.rect;
        let rel = dx * old.w / cur.w.max(1);
        let r = Rect::new(px - rel, cur.y, old.w, old.h);
        self.damage_window(id);
        if let Some(w) = self.win_mut(id) {
            w.restore = None;
            w.set_rect(r);
        }
        self.damage_window(id);
        Some((rel, 12))
    }
}
