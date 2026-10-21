//! Notification centre: click the menu bar clock. A glass panel on the right with a Today
//! summary (date, windows, uptime, memory, cpu) and the notification history: every toast
//! (also the ones Focus mode held back), grouped by the app that posted it, newest group
//! first, with Clear all. Esc or a click outside closes it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::Rect;
use crate::{pmm, timer};

use super::paint::{Painter, hit};
use super::shell::PanelKey;
use super::theme::*;
use super::{Desktop, rtc};

pub const W: i32 = 340;
const MAX_KEPT: usize = 60;
const PER_GROUP: usize = 3;

pub struct Notice {
    pub app: String,
    pub title: String,
    pub sub: String,
    pub at_ns: u64,
}

pub fn rect() -> Rect {
    let top = MENU_H + 8;
    let h = (screen_h() - top - dock_h() - DOCK_BOTTOM - 16).min(640);
    Rect::new(screen_w() - 12 - W, top, W, h)
}

fn ago(now: u64, then: u64) -> String {
    let s = now.saturating_sub(then) / 1_000_000_000;
    match s {
        0..60 => String::from("now"),
        60..3600 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

/// Groups: (app, its notices newest first), the group with the newest notice first.
fn groups(h: &[Notice]) -> Vec<(&str, Vec<&Notice>)> {
    let mut g: Vec<(&str, Vec<&Notice>)> = Vec::new();
    for n in h.iter().rev() {
        match g.iter_mut().find(|x| x.0 == n.app) {
            Some(x) => x.1.push(n),
            None => g.push((n.app.as_str(), alloc::vec![n])),
        }
    }
    g
}

const CLEAR: Rect = Rect::new(W - 16 - 76, 0, 76, 22);
const LIST_Y: i32 = 160;

impl Desktop {
    pub(super) fn record_notice(&mut self, app: &str, title: &str, sub: &str) {
        self.history.push(Notice {
            app: String::from(app),
            title: String::from(title),
            sub: String::from(sub),
            at_ns: timer::now_ns(),
        });
        if self.history.len() > MAX_KEPT {
            self.history.remove(0);
        }
        if self.notif {
            self.invalidate(PanelKey::Notif);
        }
    }

    pub(super) fn toggle_notif(&mut self) {
        self.notif = !self.notif;
        self.control = false;
        self.set_menu(None);
        if self.notif {
            // Toasts on screen go: they are in the list now (and would show through).
            let ids: Vec<u32> = self.toasts.iter().map(|t| t.id).collect();
            self.toasts.clear();
            for id in ids {
                self.invalidate(PanelKey::Toast(id));
            }
        } else {
            self.invalidate(PanelKey::Notif);
        }
    }

    /// Where Clear all is (panel coordinates), for clicks.
    fn clear_rect(&self) -> Rect {
        Rect::new(CLEAR.x, LIST_Y - 2, CLEAR.w, CLEAR.h)
    }

    pub(super) fn paint_notif(&self, p: &mut Painter, solid: bool) {
        let (tbg, ta) = super::shell::tile_colour(solid);
        let now = timer::now_ns();
        // Today.
        let t = rtc::now();
        p.text(16, 16, "TODAY", COCOA);
        p.rrect(12, 36, W - 24, 104, 16, tbg, ta);
        p.text_p(
            26,
            46,
            &format!(
                "{}, {} {}",
                rtc::DAYS[t.weekday as usize],
                t.day,
                rtc::MONTHS[t.month as usize - 1]
            ),
            ESPRESSO,
            true,
        );
        p.text_right(
            W - 26,
            44,
            &format!("{:02}:{:02}", t.hour, t.minute),
            ESPRESSO,
            2,
            true,
        );
        let n = self.windows.len();
        let up = now / 1_000_000_000;
        p.text_p(
            26,
            72,
            &format!(
                "{n} window{} open, up {}:{:02}:{:02}",
                if n == 1 { "" } else { "s" },
                up / 3600,
                up / 60 % 60,
                up % 60
            ),
            COCOA,
            false,
        );
        let s = pmm::stats();
        p.text_p(
            26,
            92,
            &format!(
                "cpu {}%, {} of {} MiB free",
                self.stats.cpu_pct,
                (s.free_frames * 4096) >> 20,
                (s.total_frames * 4096) >> 20
            ),
            COCOA,
            false,
        );
        let note = super::apps::notes::snapshot();
        let first = note
            .iter()
            .find(|l| !l.trim().is_empty())
            .cloned()
            .unwrap_or_default();
        let first: String = first.chars().take(36).collect();
        p.text_p(26, 112, &format!("Note: {first}"), LATTE, false);
        // Notifications.
        let ly = LIST_Y;
        p.text(16, ly, "NOTIFICATIONS", COCOA);
        if !self.history.is_empty() {
            let c = self.clear_rect();
            p.rrect_r(c, R_BUTTON, tbg, ta);
            p.text_p_center(c.x, c.w, c.y + 3, "Clear all", ESPRESSO, false);
        }
        let h = rect().h;
        let mut y = ly + 26;
        if self.history.is_empty() {
            p.text_p(18, y + 4, "No notifications. All quiet.", LATTE, false);
            return;
        }
        let gs = groups(&self.history);
        let total_groups = gs.len();
        for (gi, (app, items)) in gs.into_iter().enumerate() {
            let shown = items.len().min(PER_GROUP);
            let gh = 28 + shown as i32 * 44 + if items.len() > PER_GROUP { 18 } else { 0 };
            if y + gh > h - 12 {
                let rest = total_groups - gi;
                p.text_p(
                    18,
                    y + 2,
                    &format!("and {rest} more app{}", if rest == 1 { "" } else { "s" }),
                    LATTE,
                    false,
                );
                break;
            }
            p.rrect(12, y, W - 24, gh - 6, 14, tbg, ta);
            let app: String = app
                .chars()
                .take(1)
                .flat_map(char::to_uppercase)
                .chain(app.chars().skip(1))
                .collect();
            p.text_p(24, y + 8, &app, ESPRESSO, true);
            p.text_right(W - 24, y + 8, &format!("{}", items.len()), LATTE, 1, false);
            let mut iy = y + 30;
            for n in items.iter().take(PER_GROUP) {
                let title: String = n.title.chars().take(30).collect();
                p.text_p(24, iy, &title, ESPRESSO, false);
                p.text_right(W - 24, iy, &ago(now, n.at_ns), LATTE, 1, false);
                let sub: String = n.sub.chars().take(40).collect();
                p.text_p(24, iy + 18, &sub, COCOA, false);
                iy += 44;
            }
            if items.len() > PER_GROUP {
                p.text_p(
                    24,
                    iy - 4,
                    &format!("+ {} older", items.len() - PER_GROUP),
                    LATTE,
                    false,
                );
            }
            y += gh;
        }
    }

    pub(super) fn click_notif(&mut self, x: i32, y: i32) {
        if !self.history.is_empty() && hit(&self.clear_rect(), x, y) {
            self.history.clear();
            self.invalidate(PanelKey::Notif);
        }
    }

    /// Once a minute (the clock, "n min ago").
    pub(super) fn notif_tick(&mut self) {
        if self.notif {
            self.invalidate(PanelKey::Notif);
        }
    }
}
