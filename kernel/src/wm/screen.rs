//! Display modes: 1024x768, 1280x800, 1440x900, switched live from Settings > Display &
//! Lock (saved as `resolution = WxH`, applied again at boot).
//!
//! A switch allocates the new back buffer and wallpaper first (so running out of memory
//! leaves the old mode untouched), then re-configures the display (`display::set_mode`: a
//! new framebuffer, and a new virtio-gpu resource + SET_SCANOUT or a fresh RAMFBCfg), tells
//! input the new pointer space, drops every cached panel (they re-layout from
//! `screen_w/h`: menu bar, dock, toasts, Skim), clamps windows
//! into the new work area (zoomed / snapped ones are re-fitted to it), rebuilds the lock
//! screen if it is up, and redraws everything.

use crate::gfx::Rect;
use crate::pmm::Use;
use crate::{display, input, kprintln, timer};

use super::Desktop;
use super::buffer::Buffer;
use super::theme::{self, screen_h, screen_w};
use super::wallpaper::Wallpaper;

pub const MODES: [(i32, i32); 3] = [(1024, 768), (1280, 800), (1440, 900)];

impl Desktop {
    /// Switch to `w` x `h`. False (nothing changed) if the mode isn't offered or there isn't
    /// the memory for it.
    pub(super) fn set_resolution(&mut self, w: i32, h: i32) -> bool {
        if !MODES.contains(&(w, h)) {
            return false;
        }
        if (w, h) == (screen_w(), screen_h()) {
            return true;
        }
        let t0 = timer::now_ns();
        let (old_w, old_h) = (screen_w(), screen_h());
        let old_area = Desktop::work_area();
        let Some(back) = Buffer::new(Use::Display, w as usize, h as usize, 0) else {
            return false;
        };
        // The wallpaper paints at screen_w/h.
        theme::set_screen(w, h);
        let kind = super::prefs::get().wallpaper_kind();
        let Some(wp) = Wallpaper::new(kind) else {
            theme::set_screen(old_w, old_h);
            return false;
        };
        let Some(fb) = display::set_mode(w as usize, h as usize) else {
            theme::set_screen(old_w, old_h);
            return false;
        };
        self.fb = fb;
        self.back = back;
        self.wp = wp;
        input::set_screen_size(w, h);
        self.pointer = input::pointer();
        self.skip_anims();
        self.cache.clear();
        self.thumbs.clear();
        // Windows: zoomed / snapped ones follow the work area, the rest stay inside it.
        let area = Desktop::work_area();
        for win in &mut self.windows {
            let r = win.rect;
            let filled = r.w >= old_area.w - 32 && r.h >= old_area.h - 32;
            let nr = if filled && win.restore.is_some() {
                Rect::new(area.x + 8, area.y + 8, area.w - 16, area.h - 16)
            } else {
                let (nw, nh) = (r.w.min(area.w), r.h.min(area.h));
                let nx = r.x.clamp(area.x, area.x + area.w - nw);
                let ny = r.y.clamp(area.y, area.y + area.h - nh);
                Rect::new(nx, ny, nw, nh)
            };
            if nr != r {
                win.set_rect(nr);
            }
            win.dirty = true;
        }
        self.relayout_lock();
        self.damage_all();
        if self.mode == super::Mode::Spaces {
            self.build_thumbs();
        }
        kprintln!(
            "wm: display {w}x{h} (was {old_w}x{old_h}), switched in {} ms",
            (timer::now_ns() - t0) / 1_000_000
        );
        true
    }
}
