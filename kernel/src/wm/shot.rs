//! Screenshots. Cmd+Shift+3: the whole screen. Cmd+Shift+4: drag a region (Esc cancels).
//! The picture is the composited frame without the cursor or the selection overlay, saved
//! as `~/Pictures/Screenshot YYYY-MM-DD at HH.MM.SS.png` (png.rs) by a short-lived
//! thread, so encoding never holds up a frame. A shutter sound at the moment of capture
//! (if sounds are on), a toast once the file is written.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::Rect;
use crate::sched;

use super::paint::Painter;
use super::theme::*;
use super::{Desktop, notify_as, notify_quiet, prefs, rtc};

pub const DIR: &str = "/home/milk/Pictures";

/// The dim outside the selection (and over everything before the drag starts).
const VEIL: u8 = a(250);

/// Region selection in progress (Cmd+Shift+4).
#[derive(Clone, Copy)]
pub struct Select {
    /// Where the drag started, once it has.
    pub start: Option<(i32, i32)>,
    pub now: (i32, i32),
}

impl Select {
    pub fn rect(&self) -> Option<Rect> {
        let (x0, y0) = self.start?;
        let (x1, y1) = self.now;
        let r = Rect::new(
            x0.min(x1),
            y0.min(y1),
            (x1 - x0).abs() + 1,
            (y1 - y0).abs() + 1,
        );
        Some(r)
    }
}

fn file_name() -> String {
    let t = rtc::now();
    format!(
        "Screenshot {:04}-{:02}-{:02} at {:02}.{:02}.{:02}.png",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

impl Desktop {
    /// Cmd+Shift+4: start choosing a region.
    pub(super) fn shot_region_start(&mut self) {
        self.close_launcher();
        self.set_menu(None);
        self.shot = Some(Select {
            start: None,
            now: self.pointer,
        });
        self.damage_all();
    }

    pub(super) fn shot_cancel(&mut self) {
        if self.shot.take().is_some() {
            self.damage_all();
        }
    }

    pub(super) fn shot_move(&mut self, x: i32, y: i32) {
        let Some(s) = self.shot.as_mut() else {
            return;
        };
        let old = s.rect();
        s.now = (x, y);
        let new = s.rect();
        // The overlay (dim + frame + size label) changes around both rects.
        for r in [old, new].into_iter().flatten() {
            self.damage(super::paint::grow(&r, 40));
        }
    }

    pub(super) fn shot_press(&mut self) {
        if let Some(s) = self.shot.as_mut() {
            s.start = Some(self.pointer);
            s.now = self.pointer;
            self.damage_all();
        }
    }

    pub(super) fn shot_release(&mut self) {
        let Some(s) = self.shot.take() else {
            return;
        };
        self.damage_all();
        match s.rect() {
            Some(r) if r.w >= 4 && r.h >= 4 => self.capture(Some(r)),
            _ => {} // a click without a drag: nothing
        }
    }

    /// Take the picture of `region` (None: the whole screen) and save it in the background.
    pub(super) fn capture(&mut self, region: Option<Rect>) {
        let full = Rect::new(0, 0, screen_w(), screen_h());
        let r = region.map_or(full, |r| super::paint::intersect(&r, &full));
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        // Compose once without the cursor (and without the selection overlay).
        self.shot_hide = true;
        self.damage(r);
        self.render_and_compose();
        let (w, h) = (r.w as usize, r.h as usize);
        let mut px: Vec<u32> = Vec::with_capacity(w * h);
        for y in r.y..r.y + r.h {
            let row = self.back.row(y as usize);
            px.extend_from_slice(&row[r.x as usize..(r.x + r.w) as usize]);
        }
        self.shot_hide = false;
        self.damage(self.cursor_rect());
        if prefs::get().sound {
            crate::audio::play_system(crate::audio::Sound::Shutter);
        }
        let name = file_name();
        drop(sched::spawn("screenshot", move || {
            let t0 = crate::timer::now_ns();
            let png = super::png::encode(&px, w, h, w);
            let ms = (crate::timer::now_ns() - t0) / 1_000_000;
            let _ = crate::fs::mkdir_all(DIR, 0o755);
            let path = format!("{DIR}/{name}");
            match crate::fs::write_file(&path, &png) {
                Ok(()) => {
                    let _ = crate::fs::sync_all();
                    crate::kprintln!(
                        "wm: screenshot {w}x{h} -> {path} ({} bytes, encoded in {ms} ms)",
                        png.len()
                    );
                    notify_quiet(
                        "Screenshot",
                        "Screenshot saved",
                        &format!("~/Pictures/{name}"),
                    );
                }
                Err(e) => notify_as("Screenshot", "Screenshot not saved", &format!("{e}")),
            }
        }));
    }
}

/// Draw the region selection: everything dimmed but the selection, a frame, its size.
pub(super) fn draw(s: &Select, p: &mut Painter, d: Rect) {
    let Some(r) = s.rect() else {
        // Not dragging yet: the veil says "pick a region".
        p.fill_a(d.x, d.y, d.w, d.h, INK, VEIL);
        return;
    };
    // Dim around the selection (four bands).
    let bands = [
        Rect::new(0, 0, screen_w(), r.y),
        Rect::new(0, r.y + r.h, screen_w(), screen_h() - r.y - r.h),
        Rect::new(0, r.y, r.x, r.h),
        Rect::new(r.x + r.w, r.y, screen_w() - r.x - r.w, r.h),
    ];
    for b in bands {
        let c = super::paint::intersect(&b, &d);
        if c.w > 0 && c.h > 0 {
            p.fill_a(c.x, c.y, c.w, c.h, INK, VEIL);
        }
    }
    // A 1px frame just outside the selection.
    p.fill(r.x - 1, r.y - 1, r.w + 2, 1, MILK);
    p.fill(r.x - 1, r.y + r.h, r.w + 2, 1, MILK);
    p.fill(r.x - 1, r.y, 1, r.h, MILK);
    p.fill(r.x + r.w, r.y, 1, r.h, MILK);
    let label = format!("{} x {}", r.w, r.h);
    let lw = crate::font::str_width(&label, 1) + 12;
    let (lx, ly) = (
        (r.x + r.w - lw).max(0),
        if r.y + r.h + 26 < screen_h() {
            r.y + r.h + 6
        } else {
            r.y - 24
        },
    );
    p.rrect(lx, ly, lw, 20, 6, INK, a(700));
    p.text(lx + 6, ly + 2, &label, MILK);
}
