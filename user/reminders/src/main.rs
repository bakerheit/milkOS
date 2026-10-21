//! Reminders: lists with due dates, times and repeats. alarmd rings them; this window edits the
//! files in ~/Library/Reminders (format in timekit/src/reminders.rs, docs/time-apps.md).

#![no_std]
#![no_main]

extern crate alloc;

mod sheet;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind, bold_x, button, field, text_c, text_r};
use appkit::{Field, Hits, Rect, Theme};
use gui::{Event, Key, Window, blend};
use sheet::{ListSheet, RemSheet};
use timekit::Date;
use timekit::date::{WEEKDAYS, split_secs};
use timekit::reminders::{Reminder, ReminderSet};

rt::main!(main);

pub const W: i32 = 800;
pub const H: i32 = 600;
pub const SIDE: i32 = 200;

// Hit ids.
pub const SMART: u32 = 10; // 10..14
pub const LIST: u32 = 100; // + list index
pub const NEW_LIST: u32 = 20;
pub const SHOW_DONE: u32 = 21;
pub const QUICK: u32 = 22;
pub const NEW_REM: u32 = 23;
pub const CHECK: u32 = 1_000_000; // + reminder id
pub const ROW: u32 = 2_000_000; // + reminder id
pub const SHEET: u32 = 3_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Today,
    Scheduled,
    All,
    Flagged,
    List(usize),
}

pub struct App {
    pub th: Theme,
    pub paths: timeio::Paths,
    pub set: ReminderSet,
    pub view: View,
    pub today: Date,
    pub hits: Hits,
    pub quick: Field,
    pub quick_focus: bool,
    pub show_done: bool,
    pub scroll: i32,
    pub sheet: Option<RemSheet>,
    pub list_sheet: Option<ListSheet>,
}

fn main(args: &rt::Args) -> i32 {
    let paths = timeio::Paths::from_args(args);
    let Ok(mut win) = Window::new("Reminders", W, H) else {
        rt::println!("reminders: no desktop to open a window on");
        return 1;
    };
    let mut app = App {
        th: Theme::current(),
        set: timeio::load_reminders(&paths).0,
        paths,
        view: View::Today,
        today: split_secs(timeio::now_secs()).0,
        hits: Hits::default(),
        quick: Field::new("", 60),
        quick_focus: false,
        show_done: false,
        scroll: 0,
        sheet: None,
        list_sheet: None,
    };
    let mut last_load = 0;
    loop {
        match win.event(500) {
            Some(Event::Close) => break,
            Some(Event::Down { x, y, button: 1 }) => {
                app.quick_focus = false;
                if let Some(id) = app.hits.at(x, y) {
                    app.click(id);
                }
            }
            Some(Event::Key(k, m)) => {
                if (m.ctrl || m.meta) && k == Key::Char('q') {
                    break;
                }
                app.key(k, m);
            }
            Some(Event::Scroll(d)) => app.scroll = (app.scroll + d * 26).max(0),
            _ => {}
        }
        let now = timeio::mono_ms();
        if app.sheet.is_none() && app.list_sheet.is_none() && now.saturating_sub(last_load) > 1500 {
            last_load = now;
            app.set = timeio::load_reminders(&app.paths).0;
            app.th = Theme::current();
            app.today = split_secs(timeio::now_secs()).0;
        }
        app.draw(&mut win);
        win.commit();
    }
    0
}

impl App {
    pub fn save(&self) {
        let _ = timeio::save_reminders(&self.paths, &self.set);
    }

    /// The reminders the current view shows, with their list index; open first, then done ones if
    /// asked for.
    pub fn shown(&self) -> Vec<(usize, Reminder)> {
        let t = self.today;
        let mut v: Vec<(usize, Reminder)> = match self.view {
            View::Today => self.set.smart(|r| r.due.is_some_and(|d| d <= t)),
            View::Scheduled => self.set.smart(|r| r.due.is_some()),
            View::All => self.set.smart(|_| true),
            View::Flagged => self.set.smart(|r| r.flagged),
            View::List(i) if i < self.set.lists.len() => {
                self.set.open_in(i).into_iter().map(|r| (i, r)).collect()
            }
            View::List(_) => Vec::new(),
        }
        .into_iter()
        .map(|(l, r)| (l, r.clone()))
        .collect();
        if self.show_done {
            let lists: Vec<usize> = match self.view {
                View::List(i) => alloc::vec![i],
                _ => (0..self.set.lists.len()).collect(),
            };
            for li in lists.into_iter().filter(|&i| i < self.set.lists.len()) {
                v.extend(self.set.done_in(li).into_iter().map(|r| (li, r.clone())));
            }
        }
        v
    }

    pub fn title(&self) -> String {
        match self.view {
            View::Today => String::from("Today"),
            View::Scheduled => String::from("Scheduled"),
            View::All => String::from("All"),
            View::Flagged => String::from("Flagged"),
            View::List(i) => self
                .set
                .lists
                .get(i)
                .map_or(String::new(), |l| l.name.clone()),
        }
    }

    fn click(&mut self, id: u32) {
        if self.sheet.is_some() || self.list_sheet.is_some() {
            self.sheet_click(id);
            return;
        }
        match id {
            s if (SMART..SMART + 4).contains(&s) => {
                self.view =
                    [View::Today, View::Scheduled, View::All, View::Flagged][(s - SMART) as usize];
                self.scroll = 0;
            }
            l if (LIST..LIST + 100).contains(&l) => {
                self.view = View::List((l - LIST) as usize);
                self.scroll = 0;
            }
            NEW_LIST => self.list_sheet = Some(ListSheet::new()),
            SHOW_DONE => self.show_done = !self.show_done,
            QUICK => self.quick_focus = true,
            NEW_REM => self.new_reminder(),
            c if (CHECK..ROW).contains(&c) => {
                let today = self.today;
                if let Some(r) = self.set.get_mut(c - CHECK) {
                    if r.is_done() {
                        r.reopen();
                    } else {
                        r.complete(today);
                    }
                    self.save();
                }
            }
            r if (ROW..SHEET).contains(&r) => self.edit_reminder(r - ROW),
            _ => {}
        }
    }

    fn key(&mut self, k: Key, m: gui::Mods) {
        if self.sheet.is_some() || self.list_sheet.is_some() {
            self.sheet_key(k, m);
            return;
        }
        if self.quick_focus {
            match k {
                Key::Enter => {
                    let title = String::from(self.quick.text.trim());
                    if !title.is_empty() {
                        self.quick_add(&title);
                    }
                }
                Key::Escape => {
                    self.quick.set("");
                    self.quick_focus = false;
                }
                _ => {
                    self.quick.key(k, m);
                }
            }
            return;
        }
        if (m.ctrl || m.meta)
            && let Key::Char(c) = k
        {
            match c {
                'n' => self.new_reminder(),
                'l' => self.list_sheet = Some(ListSheet::new()),
                _ => {}
            }
            return;
        }
        if let Key::Char('n') = k {
            self.quick_focus = true;
        }
    }

    /// Add what was typed in the quick field to the current list; Today adds it due today.
    fn quick_add(&mut self, title: &str) {
        let list = match self.view {
            View::List(i) => i,
            _ => 0,
        };
        let mut r = Reminder::new(0, title);
        if self.view == View::Today {
            r.due = Some(self.today);
        }
        r.flagged = self.view == View::Flagged;
        self.set.add(list, r);
        self.quick.set("");
        self.save();
    }

    // ---- drawing --------------------------------------------------------------------------

    fn draw(&mut self, win: &mut Window) {
        let th = self.th;
        self.hits.clear();
        win.fill(0, 0, W, H, th.milk);
        self.draw_sidebar(win);
        self.draw_main(win);
        if self.sheet.is_some() || self.list_sheet.is_some() {
            self.hits.modal();
            let ink = th.ink;
            for p in win.pixels() {
                *p = blend(*p, ink, 0x66);
            }
            self.draw_sheets(win);
        }
    }

    fn draw_sidebar(&mut self, win: &mut Window) {
        let th = self.th;
        win.fill(0, 0, SIDE, H, th.oat);
        widgets::vline(win, SIDE - 1, 0, H, th.sand);
        let t = self.today;
        let counts = [
            self.set.due_count(t),
            self.set.smart(|r| r.due.is_some()).len(),
            self.set.smart(|_| true).len(),
            self.set.smart(|r| r.flagged).len(),
        ];
        let smart = [
            ("Today", View::Today, th.blue),
            ("Scheduled", View::Scheduled, th.straw),
            ("All", View::All, th.espresso),
            ("Flagged", View::Flagged, th.honey),
        ];
        for (i, (name, v, c)) in smart.iter().enumerate() {
            let y = 14 + i as i32 * 34;
            let r = Rect::new(8, y, SIDE - 16, 30);
            if self.view == *v {
                win.rrect(r.x, r.y, r.w, r.h, 10, Theme::tint(th.oat, th.straw, 0x55));
            }
            win.disc(26, y + 15, 7, *c);
            bold_x(win, 42, y + 7, name, th.espresso, 1);
            text_r(
                win,
                SIDE - 20,
                y + 7,
                &format!("{}", counts[i]),
                th.cocoa,
                1,
            );
            self.hits.add(r, SMART + i as u32);
        }
        win.text(16, 160, "MY LISTS", th.cocoa);
        for i in 0..self.set.lists.len() {
            let l = &self.set.lists[i];
            let (name, acc, n) = (
                l.name.clone(),
                th.accent(&l.colour),
                self.set.open_in(i).len(),
            );
            let y = 184 + i as i32 * 34;
            if y > H - 100 {
                break;
            }
            let r = Rect::new(8, y, SIDE - 16, 30);
            if self.view == View::List(i) {
                win.rrect(r.x, r.y, r.w, r.h, 10, Theme::tint(th.oat, th.straw, 0x55));
            }
            win.disc(26, y + 15, 7, acc);
            win.text(42, y + 7, &widgets::fit(&name, SIDE - 42 - 44), th.espresso);
            text_r(win, SIDE - 20, y + 7, &format!("{n}"), th.cocoa, 1);
            self.hits.add(r, LIST + i as u32);
        }
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(12, H - 44, SIDE - 24, 28),
            "Add list",
            Kind::Secondary,
            NEW_LIST,
            true,
            false,
        );
    }

    fn draw_main(&mut self, win: &mut Window) {
        let th = self.th;
        let x0 = SIDE + 24;
        let w = W - SIDE - 48;
        bold_x(win, x0, 20, &self.title(), th.espresso, 2);
        let rows = self.shown();
        let open = rows.iter().filter(|(_, r)| !r.is_done()).count();
        text_r(win, W - 24, 28, &format!("{open} open"), th.cocoa, 1);
        // The quick-add field and the New button.
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x0, 66, w - 120, 28),
            &self.quick,
            "New reminder (press Return)",
            self.quick_focus,
            caret,
            QUICK,
        );
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(x0 + w - 108, 66, 108, 28),
            "Details",
            Kind::Primary,
            NEW_REM,
            true,
            false,
        );
        // The rows.
        let top = 112;
        let row_h = 52;
        let area = H - 36 - top;
        let max_scroll = (rows.len() as i32 * row_h - area).max(0);
        self.scroll = self.scroll.clamp(0, max_scroll) / row_h * row_h;
        if rows.is_empty() {
            text_c(win, x0 + w / 2, top + 80, "Nothing here.", th.cocoa, 1);
            text_c(
                win,
                x0 + w / 2,
                top + 104,
                "Type above and press Return to add a reminder.",
                th.cocoa,
                1,
            );
        }
        let first = (self.scroll / row_h) as usize;
        for (n, (li, r)) in rows.iter().enumerate().skip(first) {
            let y = top + (n - first) as i32 * row_h;
            if y + row_h > H - 30 {
                break;
            }
            let row = Rect::new(x0 - 8, y, w + 16, row_h - 4);
            win.rrect(
                row.x,
                row.y,
                row.w,
                row.h,
                10,
                if n % 2 == 0 { th.cream } else { th.milk },
            );
            let acc = th.accent(&self.set.lists[*li].colour);
            let cy = y + 22;
            // The checkbox: a ring in the list colour, filled when done.
            win.disc(x0 + 12, cy, 11, acc);
            win.disc(x0 + 12, cy, 9, if r.is_done() { acc } else { th.milk });
            if r.is_done() {
                win.stroke(x0 + 7, cy, x0 + 11, cy + 4, 1, th.ink);
                win.stroke(x0 + 11, cy + 4, x0 + 18, cy - 4, 1, th.ink);
            }
            self.hits.add(Rect::new(x0, y, 28, row_h - 4), CHECK + r.id);
            let tcol = if r.is_done() { th.cocoa } else { th.espresso };
            let tx = x0 + 36;
            let title = widgets::fit(&r.title, w - 36 - 70);
            win.text(tx, y + 6, &title, tcol);
            if r.is_done() {
                win.fill(tx, y + 14, gui::str_width(&title, 1), 1, th.cocoa);
            }
            if r.flagged {
                bold_x(win, x0 + w - 16, y + 6, "!", th.honey, 1);
            }
            let (sub, late) = self.subtitle(*li, r);
            win.text(
                tx,
                y + 26,
                &widgets::fit(&sub, w - 36 - 20),
                if late { th.danger } else { th.cocoa },
            );
            self.hits.add(row, ROW + r.id);
            // The checkbox sits above the row's own hit.
            self.hits.add(Rect::new(x0, y, 28, row_h - 4), CHECK + r.id);
        }
        // The status bar.
        win.fill(SIDE, H - 24, W - SIDE, 24, th.cream);
        widgets::hline(win, SIDE, H - 24, W - SIDE, th.sand);
        let label = if self.show_done {
            "Hide completed"
        } else {
            "Show completed"
        };
        let lw = gui::str_width(label, 1);
        win.text(
            SIDE + 12,
            H - 20,
            &format!("{} lists", self.set.lists.len()),
            th.cocoa,
        );
        text_r(win, W - 12, H - 20, label, th.cocoa, 1);
        self.hits
            .add(Rect::new(W - 12 - lw - 6, H - 24, lw + 12, 24), SHOW_DONE);
    }

    /// The second line of a row: the due day and time, repeat, the list in smart views.
    fn subtitle(&self, li: usize, r: &Reminder) -> (String, bool) {
        let mut parts: Vec<String> = Vec::new();
        if let Some(d) = r.due {
            let day = if d == self.today {
                String::from("Today")
            } else if d == self.today.add_days(1) {
                String::from("Tomorrow")
            } else if d == self.today.add_days(-1) {
                String::from("Yesterday")
            } else {
                format!("{} {}", &WEEKDAYS[d.weekday() as usize][..3], d.fmt())
            };
            parts.push(match r.time {
                Some(t) => format!("{day} {}", timekit::text::fmt_hm(t)),
                None => day,
            });
        }
        if r.repeat != timekit::cal::Repeat::None {
            parts.push(sheet::repeat_word(r));
        }
        if !matches!(self.view, View::List(_)) {
            parts.push(self.set.lists[li].name.clone());
        }
        if parts.is_empty() {
            parts.push(String::from("No date"));
        }
        (parts.join("  -  "), r.overdue(self.today))
    }
}
