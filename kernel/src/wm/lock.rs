//! The lock screen: a big clock on the blurred wallpaper. Ctrl+Cmd+Q, the logo menu's
//! "Lock Screen", Skim's "Lock screen" action, `/dev/wm` "lock", or N minutes without input
//! (Settings > Display & Lock; Never by default). No passwords: a click or any key unlocks.
//!
//! The whole screen is one cached buffer (the wallpaper's glass layer plus the clock), built
//! when the screen locks and again each minute. Compose blits it over everything (skipping
//! what is under it once it is fully up), then brightness and the cursor. Locking and
//! unlocking fade over 220 ms. While locked, input only moves the cursor or unlocks; apps
//! keep running underneath (music keeps playing).

use alloc::format;

use crate::gfx::Rect;
use crate::timer;

use super::buffer::Buffer;
use super::icons;
use super::paint::Painter;
use super::theme::*;
use super::{Desktop, rtc};

const FADE_NS: u64 = 220_000_000;

pub struct Lock {
    buf: Buffer,
    /// The minute the clock shows (rebuilt when it changes).
    minute: u64,
    fade_start: u64,
    /// Fading out (unlocking): dropped when the fade ends.
    leaving: bool,
    /// The fade in has ended and its last (opaque) frame was composed.
    settled: bool,
}

fn fade_ns() -> u64 {
    FADE_NS * super::anim::SLOWMO.load(core::sync::atomic::Ordering::Relaxed)
}

/// Clock geometry: 8x smooth digits.
const CLOCK_SCALE: i32 = 8;
const CLOCK_Y: i32 = 176;

fn paint(buf: &mut Buffer, glass: &Buffer) {
    let mut p = Painter::new(buf);
    p.blit_same(Rect::new(0, 0, SCREEN_W, SCREEN_H), glass);
    // A soft veil so the clock reads on any wallpaper.
    p.fill_a(0, 0, SCREEN_W, SCREEN_H, MILK, a(250));
    let t = rtc::now();
    let clock = format!("{:02}:{:02}", t.hour, t.minute);
    let cw = crate::font::str_width(&clock, CLOCK_SCALE);
    p.text_x(
        (SCREEN_W - cw) / 2,
        CLOCK_Y,
        &clock,
        ESPRESSO,
        CLOCK_SCALE,
        false,
    );
    let date = format!(
        "{}, {} {}",
        rtc::DAYS[t.weekday as usize],
        t.day,
        rtc::MONTHS[t.month as usize - 1]
    );
    let dw = crate::font::str_width(&date, 2);
    p.text_x(
        (SCREEN_W - dw) / 2,
        CLOCK_Y + 16 * CLOCK_SCALE + 18,
        &date,
        COCOA,
        2,
        true,
    );
    // Who is here, and how to get back in.
    let cx = SCREEN_W / 2;
    let ay = SCREEN_H - 230;
    p.circle(cx - 36, ay, 72, CREAM, 255);
    p.ring(cx - 36, ay, 72, 2, SAND, 255);
    icons::drop_shape(&mut p, cx, ay + 42, 17, 37, BLUE);
    icons::drop_shape(&mut p, cx, ay + 42, 13, 30, MILK);
    p.text_p_center(0, SCREEN_W, ay + 84, "andrew", ESPRESSO, true);
    p.text_p_center(
        0,
        SCREEN_W,
        ay + 106,
        "Click or press any key to unlock",
        COCOA,
        false,
    );
}

impl Desktop {
    /// Demo scripts: as if the last input was `secs` earlier.
    pub(super) fn pretend_idle(&mut self, secs: u64) {
        self.idle_skew += secs * 1_000_000_000;
    }

    pub(super) fn locked(&self) -> bool {
        self.lock.as_ref().is_some_and(|l| !l.leaving)
    }

    /// Lock now (no-op when locked).
    pub(super) fn lock_screen(&mut self) {
        if self.locked() {
            return;
        }
        let Some(mut buf) = Buffer::new(SCREEN_W as usize, SCREEN_H as usize, 0) else {
            return;
        };
        paint(&mut buf, &self.wp.glass);
        // Close the transient things: they would be stale (or confusing) after unlocking.
        self.close_launcher();
        self.set_menu(None);
        self.control = false;
        self.drag = None;
        self.set_snap_preview(None);
        let now = timer::now_ns();
        self.lock = Some(Lock {
            buf,
            minute: rtc::now().minute as u64,
            fade_start: now,
            leaving: false,
            settled: false,
        });
        self.damage_all();
        crate::kprintln!("wm: screen locked");
    }

    pub(super) fn unlock_screen(&mut self) {
        if let Some(l) = self.lock.as_mut()
            && !l.leaving
        {
            l.leaving = true;
            l.fade_start = timer::now_ns();
            self.last_input = l.fade_start;
            self.idle_skew = 0;
            self.damage_all();
            crate::kprintln!("wm: screen unlocked");
        }
    }

    /// Lock opacity now (0..=255).
    pub(super) fn lock_alpha(&self, now: u64) -> u8 {
        let Some(l) = &self.lock else {
            return 0;
        };
        let t = (now.saturating_sub(l.fade_start) * 255 / fade_ns().max(1)).min(255) as u8;
        if l.leaving { 255 - t } else { t }
    }

    /// Per frame: the idle timer, the minute ticking over, fades. True while fading.
    pub(super) fn lock_tick(&mut self, now: u64) -> bool {
        let mins = super::prefs::get().lock_mins as u64;
        if self.lock.is_none() {
            let idle = now.saturating_sub(self.last_input) + self.idle_skew;
            if mins > 0 && idle > mins * 60_000_000_000 {
                self.lock_screen();
            }
            return false;
        }
        let glass = &self.wp.glass;
        let Some(l) = self.lock.as_mut() else {
            return false;
        };
        let fading = now.saturating_sub(l.fade_start) < fade_ns();
        if !fading && l.leaving {
            self.lock = None;
            self.damage_all();
            return false;
        }
        let m = rtc::now().minute as u64;
        let new_minute = m != l.minute;
        if new_minute {
            l.minute = m;
            paint(&mut l.buf, glass);
        }
        // After the fade in, one more full frame at full opacity.
        let settle = !fading && !l.settled;
        l.settled |= settle;
        if fading || settle {
            self.damage_all();
        } else if new_minute {
            self.damage(Rect::new(0, CLOCK_Y - 8, SCREEN_W, 16 * CLOCK_SCALE + 80));
        }
        fading
    }
}

/// Composite the lock screen over `d` (last, before brightness and the cursor).
pub(super) fn draw(lock: &Option<Lock>, p: &mut Painter, d: Rect, alpha: u8) {
    if let Some(l) = lock {
        if alpha == 255 {
            p.blit_same(d, &l.buf);
        } else if alpha > 0 {
            p.blit_rrect(d.x, d.y, &l.buf, d.x, d.y, d.w, d.h, 0, alpha);
        }
    }
}
