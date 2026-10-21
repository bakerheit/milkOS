//! About milkOS: the logo, version, and live memory / uptime / frame-time stats.

use alloc::format;

use crate::{heap, pmm, timer};

use super::super::icons;
use super::super::paint::Painter;
use super::super::theme::*;
use super::super::window::WindowContents;

pub struct About {
    last_sec: u64,
}

impl About {
    pub fn new() -> About {
        About { last_sec: 0 }
    }
}

fn bar(p: &mut Painter, x: i32, y: i32, w: i32, pct: usize, c: crate::gfx::Color) {
    p.rrect(x, y, w, 10, 5, SAND, 255);
    p.rrect(x, y, (w * pct.min(100) as i32 / 100).max(10), 10, 5, c, 255);
}

impl WindowContents for About {
    fn title(&self) -> &str {
        "About milkOS"
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, _h: i32, _focused: bool) {
        let cx = w / 2;
        // The drop: blue outline, milk inside, like the boot splash.
        icons::drop_shape(p, cx, 76, 30, 66, BLUE);
        icons::drop_shape(p, cx, 76, 25, 55, MILK);
        p.circle(cx - 16, 74, 8, rgb_shine(), 255);
        let name_w = crate::font::str_width("milkOS", 2) + 2;
        let x = cx - name_w / 2;
        let x2 = p.text_x(x, 124, "milk", ESPRESSO, 2, true);
        p.text_x(x2, 124, "OS", BLUE, 2, true);
        p.text_center(0, w, 162, "Version 1.0  (kernel 0.0.1)", COCOA, false);
        p.text_center(0, w, 180, "aarch64, qemu virt, 1 core", COCOA, false);

        let s = pmm::stats();
        let hs = heap::stats();
        let st = super::super::stats();
        let total = (s.total_frames * 4096) >> 20;
        let free = (s.free_frames * 4096) >> 20;
        let used_pct = 100 - s.free_frames * 100 / s.total_frames.max(1);
        let lx = 24;
        let bw = w - 2 * lx;
        p.text(lx, 214, "Memory", ESPRESSO);
        p.text_right(
            w - lx,
            214,
            &format!("{} of {} MiB free", free, total),
            COCOA,
            1,
            false,
        );
        bar(p, lx, 234, bw, used_pct, HONEY);
        p.text(lx, 254, "Kernel heap", ESPRESSO);
        p.text_right(
            w - lx,
            254,
            &format!("{} / {} KiB", hs.used >> 10, hs.total >> 10),
            COCOA,
            1,
            false,
        );
        bar(p, lx, 274, bw, hs.used * 100 / hs.total.max(1), BLUE);
        p.text(lx, 296, "Uptime", ESPRESSO);
        let up = timer::now_ns() / 1_000_000_000;
        p.text_right(
            w - lx,
            296,
            &format!("{}:{:02}:{:02}", up / 3600, up / 60 % 60, up % 60),
            COCOA,
            1,
            false,
        );
        p.text(lx, 316, "Frame", ESPRESSO);
        let drag = if st.drag_frames > 0 {
            format!("   drag {} us", st.drag_avg_us)
        } else {
            alloc::string::String::new()
        };
        p.text_right(
            w - lx,
            316,
            &format!("full {} us{}", st.full_us, drag),
            COCOA,
            1,
            false,
        );
    }

    fn tick(&mut self, now: u64) -> bool {
        let s = now / 1_000_000_000;
        if s != self.last_sec {
            self.last_sec = s;
            return true;
        }
        false
    }
}

const fn rgb_shine() -> crate::gfx::Color {
    crate::gfx::rgb(0xCF, 0xE0, 0xF4)
}
