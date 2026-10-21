//! Calendar: Day, Week, Month and Year views, a sidebar with a mini month and the calendar list,
//! an event sheet (docs/ui-concepts/SPEC-APPS.md, section 3).
//!
//! The data is one `.cal` file per calendar in ~/Library/Calendar (format in timekit/src/cal.rs,
//! docs/time-apps.md). `alarmd` rings the alerts, so they work with this window closed.

#![no_std]
#![no_main]

extern crate alloc;

mod sheet;
mod views;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind, bold_x, button, field, segmented, text_c};
use appkit::{Field, Hits, Rect, Theme};
use gui::{Event, Key, Window, blend};
use sheet::{CalSheet, EvSheet};
use timekit::Date;
use timekit::cal::CalendarSet;
use timekit::date::split_secs;

rt::main!(main);

pub const W: i32 = 940;
/// The spec says 676; the 1024x768 screen leaves room for a window this tall above the dock.
pub const H: i32 = 620;
pub const SIDE: i32 = 172;

// Hit ids. Ranges are far apart so a day number never collides with a button.
pub const VIEW: u32 = 10; // 10..13
pub const TODAY: u32 = 14;
pub const PREV: u32 = 15;
pub const NEXT: u32 = 16;
pub const NEW_EVENT: u32 = 17;
pub const SEARCH: u32 = 18;
pub const NEW_CAL: u32 = 19;
pub const CAL_TOGGLE: u32 = 100; // + calendar index
pub const DAY: u32 = 100_000; // + days since 1970 (a day cell or a mini-calendar day)
pub const EVENT: u32 = 1_000_000; // + event id
pub const MORE: u32 = 2_000_000; // + days: "+N more"
pub const SLOT: u32 = 3_000_000; // + days * 24 + hour (week/day grids)
pub const YEAR_DAY: u32 = 4_000_000; // + days (year view)
pub const SHEET: u32 = 5_000_000; // sheet controls

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Day,
    Week,
    Month,
    Year,
}

pub struct App {
    pub th: Theme,
    pub paths: timeio::Paths,
    pub set: CalendarSet,
    pub view: View,
    /// The selected day; the views follow it.
    pub cursor: Date,
    pub today: Date,
    pub hits: Hits,
    pub sheet: Option<EvSheet>,
    pub cal_sheet: Option<CalSheet>,
    pub search: Field,
    pub search_focus: bool,
    /// Week and day grids: pixels scrolled from midnight.
    pub hour_scroll: i32,
    pub last_click: (u32, u64),
}

fn main(args: &rt::Args) -> i32 {
    let paths = timeio::Paths::from_args(args);
    let Ok(mut win) = Window::new("Calendar", W, H) else {
        rt::println!("calendar: no desktop to open a window on");
        return 1;
    };
    let today = split_secs(timeio::now_secs()).0;
    let mut app = App {
        th: Theme::current(),
        set: timeio::load_calendars(&paths).0,
        paths,
        view: View::Month,
        cursor: today,
        today,
        hits: Hits::default(),
        sheet: None,
        cal_sheet: None,
        search: Field::new("", 40),
        search_focus: false,
        hour_scroll: 7 * 48,
        last_click: (0, 0),
    };
    let mut last_load = 0;
    loop {
        let ev = win.event(500);
        match ev {
            Some(Event::Close) => break,
            Some(Event::Down { x, y, button: 1 }) => {
                app.search_focus = false;
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
            Some(Event::Scroll(d)) => app.scroll(d),
            _ => {}
        }
        let now = timeio::mono_ms();
        if app.sheet.is_none() && app.cal_sheet.is_none() && now.saturating_sub(last_load) > 1500 {
            last_load = now;
            app.set = timeio::load_calendars(&app.paths).0;
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
        let _ = timeio::save_calendars(&self.paths, &self.set);
    }

    fn scroll(&mut self, d: i32) {
        self.hour_scroll = (self.hour_scroll + d * 24).clamp(0, 24 * 48 - 300);
    }

    /// Move the cursor by a step that suits the view (a day, week, month or year).
    fn step(&mut self, dir: i32) {
        self.cursor = match self.view {
            View::Day => self.cursor.add_days(dir as i64),
            View::Week => self.cursor.add_days(7 * dir as i64),
            View::Month => self.cursor.add_months(dir),
            View::Year => self.cursor.add_months(12 * dir),
        };
    }

    fn click(&mut self, id: u32) {
        if self.sheet.is_some() || self.cal_sheet.is_some() {
            self.sheet_click(id);
            return;
        }
        let now = timeio::mono_ms();
        let double = self.last_click.0 == id && now.saturating_sub(self.last_click.1) < 450;
        self.last_click = (id, now);
        match id {
            v if (VIEW..VIEW + 4).contains(&v) => {
                self.view = [View::Day, View::Week, View::Month, View::Year][(v - VIEW) as usize];
            }
            TODAY => self.cursor = self.today,
            PREV => self.step(-1),
            NEXT => self.step(1),
            NEW_EVENT => self.new_event(self.cursor, None),
            SEARCH => self.search_focus = true,
            NEW_CAL => self.cal_sheet = Some(CalSheet::new()),
            c if (CAL_TOGGLE..CAL_TOGGLE + 50).contains(&c) => {
                let i = (c - CAL_TOGGLE) as usize;
                if let Some(cal) = self.set.cals.get_mut(i) {
                    cal.hidden = !cal.hidden;
                    self.save();
                }
            }
            d if (DAY..EVENT).contains(&d) => {
                let date = Date::from_days((d - DAY) as i64);
                self.cursor = date;
                if double {
                    self.new_event(date, None);
                }
            }
            e if (EVENT..MORE).contains(&e) => self.edit_event(e - EVENT),
            m if (MORE..SLOT).contains(&m) => {
                self.cursor = Date::from_days((m - MORE) as i64);
                self.view = View::Day;
            }
            s if (SLOT..YEAR_DAY).contains(&s) => {
                let v = s - SLOT;
                let date = Date::from_days((v / 24) as i64);
                self.cursor = date;
                if double {
                    self.new_event(date, Some((v % 24) as u16 * 60));
                }
            }
            y if (YEAR_DAY..SHEET).contains(&y) => {
                self.cursor = Date::from_days((y - YEAR_DAY) as i64);
                if double {
                    self.view = View::Day;
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, k: Key, m: gui::Mods) {
        if self.sheet.is_some() || self.cal_sheet.is_some() {
            self.sheet_key(k, m);
            return;
        }
        if self.search_focus {
            match k {
                Key::Escape | Key::Enter => {
                    if k == Key::Escape {
                        self.search.set("");
                    }
                    self.search_focus = false;
                }
                _ => {
                    self.search.key(k, m);
                }
            }
            return;
        }
        if (m.ctrl || m.meta)
            && let Key::Char(c) = k
        {
            match c {
                '1' => self.view = View::Day,
                '2' => self.view = View::Week,
                '3' => self.view = View::Month,
                '4' => self.view = View::Year,
                'f' => self.search_focus = true,
                'n' => self.new_event(self.cursor, None),
                _ => {}
            }
            return;
        }
        match k {
            Key::Left => self.cursor = self.cursor.add_days(-1),
            Key::Right => self.cursor = self.cursor.add_days(1),
            Key::Up => self.cursor = self.cursor.add_days(-7),
            Key::Down => self.cursor = self.cursor.add_days(7),
            Key::Enter => self.new_event(self.cursor, None),
            Key::Char('t') => self.cursor = self.today,
            Key::Char('d') => self.view = View::Day,
            Key::Char('w') => self.view = View::Week,
            Key::Char('m') => self.view = View::Month,
            Key::Char('y') => self.view = View::Year,
            Key::Char('[') => self.step(-1),
            Key::Char(']') => self.step(1),
            _ => {}
        }
    }

    // ---- drawing --------------------------------------------------------------------------

    fn draw(&mut self, win: &mut Window) {
        let th = self.th;
        self.hits.clear();
        win.fill(0, 0, W, H, th.milk);
        self.draw_sidebar(win);
        self.draw_toolbar(win);
        if self.search.text.is_empty() {
            match self.view {
                View::Month => self.draw_month(win),
                View::Week => self.draw_week(win, 7),
                View::Day => self.draw_week(win, 1),
                View::Year => self.draw_year(win),
            }
        } else {
            self.draw_search(win);
        }
        self.draw_status(win);
        if self.sheet.is_some() || self.cal_sheet.is_some() {
            self.hits.modal();
            let ink = th.ink;
            for p in win.pixels() {
                *p = blend(*p, ink, 0x66);
            }
            self.draw_sheets(win);
        }
    }

    fn draw_toolbar(&mut self, win: &mut Window) {
        let th = self.th;
        win.fill(SIDE, 0, W - SIDE, 40, th.cream);
        widgets::hline(win, SIDE, 40, W - SIDE, th.sand);
        let sel = match self.view {
            View::Day => 0,
            View::Week => 1,
            View::Month => 2,
            View::Year => 3,
        };
        let sw = segmented(
            win,
            &th,
            &mut self.hits,
            SIDE + 12,
            6,
            &["Day", "Week", "Month", "Year"],
            sel,
            VIEW,
        );
        // Right cluster: Today, previous, next.
        let mut x = W - 12;
        x -= 28;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, 8, 28, 24),
            ">",
            Kind::Secondary,
            NEXT,
            true,
            true,
        );
        x -= 32;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, 8, 28, 24),
            "<",
            Kind::Secondary,
            PREV,
            true,
            true,
        );
        let tw = widgets::button_w("Today", true);
        x -= tw + 4;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, 8, tw, 24),
            "Today",
            Kind::Secondary,
            TODAY,
            true,
            true,
        );
        // The title in 2x, in what is left between.
        let title = self.title();
        let room = x - (SIDE + 12 + sw + 16) - 8;
        let (scale, tx) = if gui::str_width(&title, 2) <= room {
            (2, SIDE + 12 + sw + 16)
        } else {
            (1, SIDE + 12 + sw + 16)
        };
        bold_x(
            win,
            tx,
            if scale == 2 { 4 } else { 12 },
            &title,
            th.espresso,
            scale,
        );
    }

    fn title(&self) -> String {
        use timekit::date::MONTHS;
        let c = self.cursor;
        match self.view {
            View::Month => format!("{} {}", MONTHS[c.m as usize - 1], c.y),
            View::Year => format!("{}", c.y),
            View::Day => format!("{} {} {}", c.d, &MONTHS[c.m as usize - 1][..3], c.y),
            View::Week => {
                let start = c.add_days(-(c.weekday() as i64));
                let end = start.add_days(6);
                if start.m == end.m {
                    format!(
                        "{}-{} {} {}",
                        start.d,
                        end.d,
                        &MONTHS[end.m as usize - 1][..3],
                        end.y
                    )
                } else {
                    format!(
                        "{} {}-{} {} {}",
                        start.d,
                        &MONTHS[start.m as usize - 1][..3],
                        end.d,
                        &MONTHS[end.m as usize - 1][..3],
                        end.y
                    )
                }
            }
        }
    }

    fn draw_sidebar(&mut self, win: &mut Window) {
        let th = self.th;
        win.fill(0, 0, SIDE, H, th.oat);
        widgets::vline(win, SIDE - 1, 0, H, th.sand);
        // Search sits at the top of the sidebar (the toolbar has no room for it at 16px text).
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(12, 8, SIDE - 24, 24),
            &self.search,
            "Search",
            self.search_focus,
            caret,
            SEARCH,
        );
        // The mini month of the cursor.
        let c = self.cursor;
        let title = format!("{} {}", &timekit::date::MONTHS[c.m as usize - 1][..3], c.y);
        bold_x(win, 14, 46, &title, th.espresso, 1);
        let cell = 21;
        for (i, l) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
            text_c(win, 14 + i as i32 * cell + cell / 2, 68, l, th.cocoa, 1);
        }
        let counts = self.set.month_counts(c.y, c.m);
        for (r, row) in timekit::date::month_grid(c.y, c.m, true).iter().enumerate() {
            for (col, &d) in row.iter().enumerate() {
                if d == 0 {
                    continue;
                }
                let date = Date { y: c.y, m: c.m, d };
                let (x, y) = (14 + col as i32 * cell, 88 + r as i32 * 20);
                let r = Rect::new(x, y, cell, 20);
                if date == self.today {
                    win.disc(x + cell / 2, y + 10, 10, th.straw);
                } else if date == self.cursor {
                    win.rrect(
                        x + 1,
                        y + 1,
                        cell - 2,
                        18,
                        9,
                        Theme::tint(th.oat, th.straw, 0x66),
                    );
                }
                let col_t = if date == self.today {
                    th.ink
                } else {
                    th.espresso
                };
                text_c(win, x + cell / 2, y + 2, &format!("{d}"), col_t, 1);
                if counts[d as usize - 1] > 0 && date != self.today {
                    win.fill(x + cell / 2 - 1, y + 17, 3, 2, th.cocoa);
                }
                self.hits.add(r, DAY + date.days() as u32);
            }
        }
        // The calendar list.
        win.text(14, 226, "CALENDARS", th.cocoa);
        for i in 0..self.set.cals.len() {
            let (name, colour, hidden) = {
                let c = &self.set.cals[i];
                (c.name.clone(), c.colour.clone(), c.hidden)
            };
            let y = 250 + i as i32 * 26;
            if y > H - 100 {
                break;
            }
            let acc = th.accent(&colour);
            if hidden {
                win.disc(22, y + 9, 7, th.sand);
                win.disc(22, y + 9, 5, th.oat);
            } else {
                win.disc(22, y + 9, 7, acc);
            }
            let tcol = if hidden { th.cocoa } else { th.espresso };
            win.text(36, y + 1, &widgets::fit(&name, SIDE - 48), tcol);
            self.hits
                .add(Rect::new(8, y, SIDE - 16, 20), CAL_TOGGLE + i as u32);
        }
        let ny = 250 + self.set.cals.len() as i32 * 26;
        if ny < H - 100 {
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(12, ny + 2, SIDE - 24, 24),
                "Add calendar",
                Kind::Secondary,
                NEW_CAL,
                true,
                true,
            );
        }
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(12, H - 44, SIDE - 24, 28),
            "New event",
            Kind::Primary,
            NEW_EVENT,
            true,
            false,
        );
    }

    fn draw_status(&mut self, win: &mut Window) {
        let th = self.th;
        win.fill(SIDE, H - 24, W - SIDE, 24, th.cream);
        widgets::hline(win, SIDE, H - 24, W - SIDE, th.sand);
        let n = self.set.on(self.cursor).len();
        let wd = timekit::date::WEEKDAYS[self.cursor.weekday() as usize];
        let s = format!(
            "{wd} {}: {}",
            self.cursor.fmt(),
            match n {
                0 => String::from("nothing planned"),
                1 => String::from("1 event"),
                n => format!("{n} events"),
            }
        );
        win.text(SIDE + 12, H - 20, &s, th.cocoa);
    }

    /// The main area below the toolbar and above the status bar.
    pub fn body() -> Rect {
        Rect::new(SIDE, 41, W - SIDE, H - 41 - 24)
    }

    pub fn event_list(&self, d: Date) -> Vec<(usize, timekit::cal::Event)> {
        self.set
            .on(d)
            .into_iter()
            .map(|(c, e)| (c, e.clone()))
            .collect()
    }
}
