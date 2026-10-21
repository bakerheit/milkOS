//! Tabs everywhere (docs/ui-concepts/familiar-plus.html, SPEC "Tabs"): any window can
//! become a tab in any other. Drag a window by its title bar onto another window's title
//! bar (a dashed straw target shows where it lands, the dragged window is a 92% ghost with a
//! straw edge) and let go: all its tabs join that window. Drag a tab down out of the strip
//! and it is its own window again, still under the pointer (so it can go straight into
//! another window). Tabs mix apps; a window looks as it always did until it has two tabs,
//! then its 32px title bar becomes the 38px strip.
//!
//! Terminal tabs (the shells inside one Terminal) and window tabs: a Terminal alone in its
//! window shows its shells in that window's strip (Terminal colours); a Terminal that is
//! one tab of a mixed window shows its shells in its own strip at the top of its body. So
//! there are two strips only when there really are two levels. Cmd+T / Cmd+W / Cmd+1..9 and
//! Ctrl+Tab are the Terminal's (shells); Cmd+Shift+[ and ] switch window tabs; Cmd+W closes
//! the Terminal's shell tab first, then the window tab, then the window.
//!
//! Window state lives in `Window` (window.rs: the active tab is `app` / `contents`, the
//! others are `parked`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::Rect;

use super::Desktop;
use super::shell::PanelKey;
use super::window::{App, Part, Window};

impl Desktop {
    /// The window (and tab) holding `app`.
    pub(super) fn find_app(&self, app: App) -> Option<(u32, usize)> {
        self.windows
            .iter()
            .find_map(|w| w.tab_of(app).map(|t| (w.id, t)))
    }

    /// Is `app` open anywhere (any tab of any window)?
    pub(super) fn has_app(&self, app: App) -> bool {
        self.find_app(app).is_some()
    }

    /// Keep each window's title bar as tall as its strip needs (32, or 38 with tabs),
    /// growing or shrinking the window so its body keeps its size.
    pub(super) fn sync_bars(&mut self) {
        let mut changed: Vec<u32> = Vec::new();
        for w in &self.windows {
            if w.want_bar() != w.bar_h {
                changed.push(w.id);
            }
        }
        for id in changed {
            self.damage_window(id);
            if let Some(w) = self.win_mut(id) {
                let want = w.want_bar();
                let d = want - w.bar_h;
                let r = w.rect;
                w.bar_h = want;
                w.set_rect(Rect::new(r.x, r.y, r.w, r.h + d));
                w.dirty = true;
            }
            self.damage_window(id);
        }
    }

    /// A press on a window's strip.
    pub(super) fn tab_press(&mut self, id: u32, part: Part, x: i32, y: i32) {
        let Some(w) = self.win_mut(id) else {
            return;
        };
        let inner = w.strip().is_some_and(|s| s.inner);
        match part {
            Part::Tab(i) if inner => w.contents.inner_select(i),
            Part::TabClose(i) if inner => w.contents.inner_close(i),
            Part::TabNew if inner => w.contents.inner_new(),
            Part::Tab(i) => {
                w.switch_to(i);
                self.drag = Some(super::Drag::Tab {
                    id,
                    i,
                    x0: x,
                    y0: y,
                });
                self.invalidate(PanelKey::MenuBar);
            }
            Part::TabClose(i) => self.close_tab(id, i),
            Part::TabNew => {
                // "+": pick something in Skim; it opens as a tab of this window.
                self.add_tab_to = Some(id);
                if self.launcher.is_none() {
                    self.toggle_launcher();
                }
            }
            _ => {}
        }
        if let Some(w) = self.win_mut(id) {
            w.dirty = true;
        }
    }

    /// Close tab `i` of window `id` (the window, when it is the last tab).
    pub(super) fn close_tab(&mut self, id: u32, i: usize) {
        let Some(w) = self.win_mut(id) else {
            return;
        };
        if w.tab_count() < 2 {
            self.close(id);
            return;
        }
        if let Some(t) = w.take_tab(i)
            && let App::Client(cid) = t.app
        {
            super::client::closed(cid);
        }
        self.damage_window(id);
        self.invalidate(PanelKey::Dock);
        self.invalidate(PanelKey::MenuBar);
    }

    /// The tab being dragged left the strip: make it a window under the pointer and keep
    /// dragging that.
    pub(super) fn tear_tab(&mut self, id: u32, i: usize, x: i32, y: i32) {
        let Some(src) = self.win_mut(id) else {
            return;
        };
        let (rect, space) = (src.rect, src.space);
        let Some(t) = src.take_tab(i) else {
            return;
        };
        self.damage_window(id);
        let nid = self.next_id;
        self.next_id += 1;
        let r = Rect::new(
            x - 120,
            y - 16,
            rect.w,
            rect.h - (super::window::STRIP_H - super::theme::TITLE_H),
        );
        let Some(mut w) = Window::new(nid, t.app, r, space, t.contents) else {
            return;
        };
        w.dirty = true;
        self.windows.push(w);
        self.set_focus(Some(nid));
        self.damage_window(nid);
        self.drag = Some(super::Drag::Move {
            id: nid,
            dx: 120,
            dy: 16,
            fresh: false,
        });
    }

    /// While window `moving` is dragged: is the pointer on another window's title bar?
    /// If so, the y just below that window's strip (where the ghost hangs).
    pub(super) fn update_drop_target(&mut self, moving: u32, x: i32, y: i32) -> Option<i32> {
        let target = self
            .windows
            .iter()
            .rev()
            .filter(|w| w.id != moving && self.visible(w))
            .find(|w| {
                let r = w.rect;
                x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
            })
            .filter(|w| y < w.rect.y + w.bar_h + 1)
            .filter(|w| self.fits_into(moving, w))
            .map(|w| (w.id, w.rect.y + w.bar_h + 6));
        let below = target.map(|t| t.1);
        let target = target.map(|t| t.0);
        for w in &mut self.windows {
            let hint = Some(w.id) == target;
            let ghost = w.id == moving && target.is_some();
            if w.drop_hint != hint || w.ghost != ghost {
                w.drop_hint = hint;
                w.ghost = ghost;
                w.dirty = true;
            }
        }
        if target.is_some() {
            // Ghost alpha / edge changes are compositing, not surface changes.
            self.damage_window(moving);
        }
        for id in self.windows.iter().map(|w| w.id).collect::<Vec<_>>() {
            if self.win(id).is_some_and(|w| w.dirty) {
                self.damage_window(id);
            }
        }
        below
    }

    /// Would every tab of window `moving` fit in `into`'s body (with the 38px strip)? Client
    /// apps draw a fixed size buffer, so they never join a smaller tab group.
    fn fits_into(&self, moving: u32, into: &Window) -> bool {
        let Some(m) = self.win(moving) else {
            return false;
        };
        let (mw, mh) = m.min_body();
        let b = into.body();
        let strip = super::window::STRIP_H - into.bar_h; // 6 more once it has tabs
        mw <= b.w && mh <= b.h - strip
    }

    /// The window being dragged was let go: merge it into the window it was over, if any.
    /// True if it merged.
    pub(super) fn drop_window(&mut self, moving: u32) -> bool {
        let target = self.windows.iter().find(|w| w.drop_hint).map(|w| w.id);
        for w in &mut self.windows {
            if w.drop_hint || w.ghost {
                w.drop_hint = false;
                w.ghost = false;
                w.dirty = true;
            }
        }
        let Some(into) = target else {
            self.damage_window(moving);
            return false;
        };
        self.merge(moving, into);
        true
    }

    /// All of window `from`'s tabs join window `into` (the last one active).
    pub(super) fn merge(&mut self, from: u32, into: u32) {
        let Some(i) = self.idx(from) else {
            return;
        };
        self.damage_window(from);
        let mut w = self.windows.remove(i);
        self.thumbs.remove(&from);
        let tabs = w.take_all();
        if let Some(t) = self.win_mut(into) {
            for p in tabs {
                t.add_tab(p.app, p.contents);
            }
        }
        self.set_focus(Some(into));
        self.damage_window(into);
        self.invalidate(PanelKey::Dock);
        self.invalidate(PanelKey::MenuBar);
    }

    /// Cmd+Shift+[ / ]: the previous / next tab of the focused window.
    pub(super) fn cycle_tab(&mut self, d: i32) {
        let Some(id) = self.focus else {
            return;
        };
        if let Some(w) = self.win_mut(id) {
            let n = w.tab_count() as i32;
            if n > 1 {
                let to = (w.active as i32 + d).rem_euclid(n) as usize;
                w.switch_to(to);
            }
        }
        self.damage_window(id);
        self.invalidate(PanelKey::MenuBar);
    }

    /// Strip hover states for the window under the pointer.
    pub(super) fn hover_tabs(&mut self, x: i32, y: i32) {
        let under = self.window_at(x, y);
        let part = under.and_then(|id| self.win(id)).and_then(|w| w.hit(x, y));
        for w in &mut self.windows {
            let (t, c) = if Some(w.id) == under {
                match part {
                    Some(Part::Tab(i)) => (Some(i), None),
                    Some(Part::TabClose(i)) => (Some(i), Some(i)),
                    _ => (None, None),
                }
            } else {
                (None, None)
            };
            if w.tab_hover != t || w.close_hover != c {
                w.tab_hover = t;
                w.close_hover = c;
                w.dirty = true;
            }
        }
    }

    /// What Spaces and the Window menu call a window: its tab, and how many more.
    pub(super) fn window_label(w: &Window) -> String {
        let t = String::from(w.contents.title());
        match w.tab_count() {
            1 => t,
            n => format!("{t} + {} tab{}", n - 1, if n == 2 { "" } else { "s" }),
        }
    }

    /// Every tab of every window, for Skim: (window, tab, title, app name).
    pub(super) fn tab_list(&self) -> Vec<(u32, usize, String, String)> {
        let mut v = Vec::new();
        for w in &self.windows {
            if w.tab_count() < 2 {
                continue;
            }
            let titles = w.tab_titles();
            for (i, t) in titles.into_iter().enumerate() {
                let app = w.tab_app(i).map_or("", |a| a.name());
                v.push((w.id, i, t, String::from(app)));
            }
        }
        v
    }

    /// Skim picked a tab: its window, that tab.
    pub(super) fn show_tab(&mut self, id: u32, i: usize) {
        if let Some(w) = self.win_mut(id) {
            w.switch_to(i);
        }
        self.show_window(id);
        self.damage_window(id);
    }
}
