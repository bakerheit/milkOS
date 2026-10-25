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

/// The display's own size: what the framebuffer was at boot (1024x768 unless the Raspberry Pi's
/// firmware has a panel or monitor of another size, `milk.fbsize=`, rpi/fb.rs).
static NATIVE: [core::sync::atomic::AtomicI32; 2] = [
    core::sync::atomic::AtomicI32::new(1024),
    core::sync::atomic::AtomicI32::new(768),
];

pub fn native() -> (i32, i32) {
    use core::sync::atomic::Ordering::Relaxed;
    (NATIVE[0].load(Relaxed), NATIVE[1].load(Relaxed))
}

pub fn set_native(w: i32, h: i32) {
    use core::sync::atomic::Ordering::Relaxed;
    NATIVE[0].store(w, Relaxed);
    NATIVE[1].store(h, Relaxed);
}

/// The modes this display offers. On one of the standard sizes: the three [`MODES`], as always. On
/// any other size (an 800x480 panel, a 1920x1080 television): its own size first, then the
/// standard modes that fit inside it (none, for the panel: the firmware would have to scale a
/// bigger picture down).
pub fn modes() -> alloc::vec::Vec<(i32, i32)> {
    let (nw, nh) = native();
    if MODES.contains(&(nw, nh)) {
        return MODES.to_vec();
    }
    let mut v = alloc::vec![(nw, nh)];
    v.extend(MODES.iter().copied().filter(|&(w, h)| w <= nw && h <= nh));
    v
}

/// Is `w` x `h` a mode of this display ([`modes`])?
pub fn offered(w: i32, h: i32) -> bool {
    modes().contains(&(w, h))
}

impl Desktop {
    /// Switch to `w` x `h`. False (nothing changed) if the mode isn't offered or there isn't
    /// the memory for it.
    pub(super) fn set_resolution(&mut self, w: i32, h: i32) -> bool {
        if !offered(w, h) {
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
