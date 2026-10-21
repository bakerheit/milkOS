//! Drawing: the sidebar, the header with quick add, the rows, the completed group, the toast.

use crate::edit::editing;
use crate::{
    App, CHECK, COMPLETED, H, LIST, NEW_LIST, QUICK, ROW, SEARCH, SIDE, SMART, UNDO, View, W,
};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind, bold_x, button, button_w, field, text_c, text_r};
use appkit::{Rect, Theme};
use gui::Window;
use timekit::cal::Repeat;
use timekit::date::WEEKDAYS;
use timekit::reminders::Reminder;
use timekit::text::fmt_hm;

const ROW_H: i32 = 62;
const HEAD_H: i32 = 30;
const TOP: i32 = 146;

enum Item {
    Heading(String, bool),
    Row(usize, Reminder),
    CompletedHeader(usize),
}

impl App {
    fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        let rows = self.open_rows();
        let mut last_day: Option<Option<timekit::Date>> = None;
        for (li, r) in rows {
            if self.view == View::Scheduled {
                // Grouped by day; everything before today is one "Overdue" group.
                let key = r.due.map(|d| {
                    if d < self.today {
                        self.today.add_days(-1000)
                    } else {
                        d
                    }
                });
                if last_day != Some(key) {
                    last_day = Some(key);
                    let (text, danger) = match r.due {
                        Some(d) if d < self.today => (String::from("Overdue"), true),
                        Some(d) => (self.day_word(d), false),
                        None => (String::from("No date"), false),
                    };
                    out.push(Item::Heading(text, danger));
                }
            }
            out.push(Item::Row(li, r));
        }
        let done = self.done_rows();
        if !done.is_empty() {
            out.push(Item::CompletedHeader(done.len()));
            if self.show_completed {
                out.extend(done.into_iter().map(|(li, r)| Item::Row(li, r)));
            }
        }
        out
    }

    fn day_word(&self, d: timekit::Date) -> String {
        if d == self.today {
            String::from("Today")
        } else if d == self.today.add_days(1) {
            String::from("Tomorrow")
        } else if d == self.today.add_days(-1) {
            String::from("Yesterday")
        } else {
            format!("{} {}", &WEEKDAYS[d.weekday() as usize][..3], d.fmt())
        }
    }

    /// "Today 09:00, weekly" / "Overdue: Yesterday" / "No date".
    fn due_line(&self, r: &Reminder) -> String {
        let mut s = match r.due {
            Some(d) => {
                let mut day = self.day_word(d);
                if let Some(t) = r.time {
                    day.push(' ');
                    day.push_str(&fmt_hm(t));
                }
                if r.overdue(self.today) {
                    format!("Overdue: {day}")
                } else {
                    day
                }
            }
            None => String::from("No date"),
        };
        if r.repeat != Repeat::None {
            let unit = match r.repeat {
                Repeat::Daily => ("daily", "days"),
                Repeat::Weekly => ("weekly", "weeks"),
                Repeat::Monthly => ("monthly", "months"),
                _ => ("yearly", "years"),
            };
            if r.every > 1 {
                s.push_str(&format!(", every {} {}", r.every, unit.1));
            } else {
                s.push_str(&format!(", {}", unit.0));
            }
        }
        s
    }

    pub fn draw_sidebar(&mut self, win: &mut Window) {
        let th = self.th;
        win.fill(0, 0, SIDE, H, th.oat);
        widgets::vline(win, SIDE - 1, 0, H, th.sand);
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(12, 10, SIDE - 24, 24),
            &self.search,
            "Search",
            self.search_focus,
            caret,
            SEARCH,
        );
        let t = self.today;
        let counts = [
            self.set.due_count(t),
            self.set.smart(|r| r.due.is_some()).len(),
            self.set.smart(|_| true).len(),
        ];
        let smart = [
            ("Today", View::Today, th.blue),
            ("Scheduled", View::Scheduled, th.straw),
            ("All", View::All, th.espresso),
        ];
        for (i, (name, v, c)) in smart.iter().enumerate() {
            let y = 48 + i as i32 * 30;
            let r = Rect::new(8, y, SIDE - 16, 26);
            if self.view == *v {
                win.rrect(r.x, r.y, r.w, r.h, 8, Theme::tint(th.oat, th.straw, 0x33));
            }
            win.disc(24, y + 13, 6, *c);
            bold_x(win, 40, y + 5, name, th.espresso, 1);
            let count_col = if self.view == *v {
                th.espresso
            } else {
                th.cocoa
            };
            text_r(
                win,
                SIDE - 20,
                y + 5,
                &format!("{}", counts[i]),
                count_col,
                1,
            );
            self.hits.add(r, SMART + i as u32);
        }
        win.text(16, 148, "LISTS", th.cocoa);
        for i in 0..self.set.lists.len() {
            let (name, acc, n) = (
                self.set.lists[i].name.clone(),
                th.accent(&self.set.lists[i].colour),
                self.set.open_in(i).len(),
            );
            let y = 172 + i as i32 * 30;
            if y > H - 80 {
                break;
            }
            let r = Rect::new(8, y, SIDE - 16, 26);
            if self.view == View::List(i) {
                win.rrect(r.x, r.y, r.w, r.h, 8, Theme::tint(th.oat, th.straw, 0x33));
            }
            win.disc(24, y + 13, 6, acc);
            win.text(40, y + 5, &widgets::fit(&name, SIDE - 40 - 40), th.espresso);
            let count_col = if self.view == View::List(i) {
                th.espresso
            } else {
                th.cocoa
            };
            text_r(win, SIDE - 20, y + 5, &format!("{n}"), count_col, 1);
            self.hits.add(r, LIST + i as u32);
        }
        let bw = button_w("Add list", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(12, H - 40, bw, 24),
            "Add list",
            Kind::Secondary,
            NEW_LIST,
            true,
            true,
        );
    }

    pub fn draw_main(&mut self, win: &mut Window) {
        let th = self.th;
        let x0 = SIDE + 24;
        let w = W - SIDE - 48;
        let items = self.items();
        // Total height, with the editor open on one row.
        let editor_h = 270 + 6;
        let height = |it: &Item, app: &App| match it {
            Item::Heading(..) => HEAD_H,
            Item::CompletedHeader(_) => 34,
            Item::Row(_, r) => ROW_H + if editing(app, r) { editor_h } else { 0 },
        };
        let total: i32 = items.iter().map(|i| height(i, self)).sum();
        let view_h = H - TOP;
        self.scroll = self.scroll.clamp(0, (total - view_h).max(0));
        let mut y = TOP - self.scroll;
        for (n, it) in items.iter().enumerate() {
            let h = height(it, self);
            if y + h < TOP || y > H {
                y += h;
                continue;
            }
            match it {
                Item::Heading(text, danger) => {
                    bold_x(
                        win,
                        x0,
                        y + 8,
                        text,
                        if *danger { th.danger } else { th.cocoa },
                        1,
                    );
                }
                Item::CompletedHeader(n) => {
                    bold_x(win, x0, y + 10, &format!("Completed ({n})"), th.cocoa, 1);
                    let label = if self.show_completed { "Hide" } else { "Show" };
                    text_r(win, x0 + w, y + 10, label, th.cocoa, 1);
                    self.hits.add(Rect::new(x0 + w - 60, y, 70, 34), COMPLETED);
                }
                Item::Row(li, r) => {
                    self.draw_row(win, x0, y, w, *li, r, n);
                    if editing(self, r) {
                        self.draw_editor(win, x0, y + ROW_H, w);
                    }
                }
            }
            y += h;
        }
        if items.is_empty() {
            text_c(win, x0 + w / 2, TOP + 80, "Nothing here.", th.cocoa, 1);
            text_c(
                win,
                x0 + w / 2,
                TOP + 104,
                "Type above and press Return to add a reminder.",
                th.cocoa,
                1,
            );
        }
        // The header repaints over whatever scrolled under it.
        win.fill(SIDE, 0, W - SIDE, TOP, th.milk);
        bold_x(win, x0, 14, &self.title(), th.espresso, 2);
        let open = self.open_rows();
        let overdue = open.iter().filter(|(_, r)| r.overdue(self.today)).count();
        let mut counts = format!(
            "{} reminder{}",
            open.len(),
            if open.len() == 1 { "" } else { "s" }
        );
        if overdue > 0 {
            counts.push_str(&format!(", {overdue} overdue"));
        }
        win.text(x0, 50, &counts, th.cocoa);
        self.draw_quick(win, x0, w);
        // The Undo toast after a delete.
        if self.undo.is_some() {
            let r = Rect::new(SIDE + (W - SIDE - 300) / 2, H - 52, 300, 36);
            win.rrect(r.x, r.y, r.w, r.h, 18, th.espresso);
            win.text(r.x + 18, r.y + 10, "Reminder deleted", th.milk);
            let bw = button_w("Undo", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(r.right() - 12 - bw, r.y + 6, bw, 24),
                "Undo",
                Kind::Primary,
                UNDO,
                true,
                true,
            );
        }
    }

    fn draw_quick(&mut self, win: &mut Window, x0: i32, w: i32) {
        let th = self.th;
        let r = Rect::new(x0, 76, w, 32);
        win.rrect(
            r.x,
            r.y,
            r.w,
            r.h,
            16,
            if self.quick_focus { th.straw } else { th.sand },
        );
        let b = if self.quick_focus { 2 } else { 1 };
        win.rrect(r.x + b, r.y + b, r.w - 2 * b, r.h - 2 * b, 16 - b, th.cream);
        let max_w = r.w - 32;
        if self.quick.text.is_empty() {
            win.text(
                r.x + 16,
                r.y + 8,
                &widgets::fit("Add a reminder, e.g. Call mum tomorrow at 5pm", max_w),
                th.cocoa,
            );
        } else {
            let shown = widgets::fit(&self.quick.text, max_w);
            win.text(r.x + 16, r.y + 8, &shown, th.espresso);
            if self.quick_focus && timeio::mono_ms() / 500 % 2 == 0 {
                let cx = r.x + 16 + (self.quick.cur.min((max_w / 8) as usize) as i32) * 8;
                win.fill(cx, r.y + 8, 2, 16, th.espresso);
            }
        }
        self.hits.add(r, QUICK);
        if !self.quick.text.trim().is_empty() {
            let q = timekit::quickadd::parse(self.quick.text.trim(), self.today);
            let list = self
                .set
                .lists
                .get(self.current_list())
                .map_or("", |l| l.name.as_str());
            let mut p = q.preview(self.today, list);
            if q.due.is_none() && self.view == View::Today {
                p = p.replace(", no date", ", due today");
            }
            win.text(x0 + 4, 114, &widgets::fit(&p, w - 8), th.cocoa);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &mut self,
        win: &mut Window,
        x0: i32,
        y: i32,
        w: i32,
        li: usize,
        r: &Reminder,
        n: usize,
    ) {
        let th = self.th;
        let selected = self.selected == Some(r.id);
        let (acc, acc_ink) = (
            th.accent(&self.set.lists[li].colour),
            th.accent_ink(&self.set.lists[li].colour),
        );
        let _ = n;
        if selected {
            win.rrect(
                x0 - 8,
                y,
                w + 16,
                ROW_H - 4,
                10,
                Theme::tint(th.milk, th.straw, 0x33),
            );
        }
        // The checkbox: a 22px circle with a 2px ring in the list's ink colour.
        let (cx, cy) = (x0 + 11, y + 22);
        if r.is_done() {
            win.disc(cx, cy, 11, acc);
            win.stroke(cx - 5, cy, cx - 1, cy + 4, 1, th.ink);
            win.stroke(cx - 1, cy + 4, cx + 6, cy - 4, 1, th.ink);
        } else {
            win.disc(cx, cy, 11, acc_ink);
            win.disc(cx, cy, 9, th.milk);
        }
        let tcol = if r.is_done() { th.cocoa } else { th.espresso };
        let tx = x0 + 36;
        let right = if matches!(self.view, View::List(_)) {
            0
        } else {
            110
        };
        let title = widgets::fit(&r.title, w - 36 - right - 8);
        win.text(tx, y + 10, &title, tcol);
        if r.is_done() {
            win.fill(tx, y + 18, gui::str_width(&title, 1), 1, th.cocoa);
        }
        let due = self.due_line(r);
        win.text(
            tx,
            y + 32,
            &widgets::fit(&due, w - 36 - right - 8),
            if r.overdue(self.today) {
                th.danger
            } else {
                th.cocoa
            },
        );
        if right > 0 {
            text_r(
                win,
                x0 + w,
                y + 10,
                &widgets::fit(&self.set.lists[li].name, 100),
                th.cocoa,
                1,
            );
        }
        widgets::hline(win, x0, y + ROW_H - 1, w, th.sand);
        self.hits
            .add(Rect::new(x0 - 8, y, w + 16, ROW_H - 4), ROW + r.id);
        self.hits.add(Rect::new(x0, y + 6, 30, 32), CHECK + r.id);
    }
}
