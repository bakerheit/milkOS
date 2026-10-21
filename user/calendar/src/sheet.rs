//! The event sheet and the "add calendar" sheet: state, keys, clicks and drawing.

use crate::{App, H, SHEET, W};
use alloc::format;
use alloc::string::String;
use appkit::widgets::{self, Kind, bold_x, button, button_w, chip, field, switch};
use appkit::{Field, Rect, Theme};
use gui::{Key, Mods, Window, blend};
use timekit::Date;
use timekit::cal::{COLOURS, Event, Repeat};
use timekit::text::{fmt_hm, parse_hm};

const SAVE: u32 = SHEET;
const CANCEL: u32 = SHEET + 1;
const DELETE: u32 = SHEET + 2;
const ALL_DAY: u32 = SHEET + 3;
const REPEAT: u32 = SHEET + 10; // + 0..5
const ALERT: u32 = SHEET + 20; // + 0..6
const CAL: u32 = SHEET + 30; // + calendar index
const FIELD: u32 = SHEET + 50; // + field index
const CAL_SAVE: u32 = SHEET + 60;
const CAL_CANCEL: u32 = SHEET + 61;
const CAL_COLOUR: u32 = SHEET + 70; // + 0..5

const REPEATS: [(&str, Repeat); 5] = [
    ("Never", Repeat::None),
    ("Daily", Repeat::Daily),
    ("Weekly", Repeat::Weekly),
    ("Monthly", Repeat::Monthly),
    ("Yearly", Repeat::Yearly),
];
/// Minutes before the start; `None` is "no alert".
const ALERTS: [(&str, Option<u16>); 6] = [
    ("None", None),
    ("At time", Some(0)),
    ("5 min", Some(5)),
    ("15 min", Some(15)),
    ("1 hour", Some(60)),
    ("1 day", Some(1440)),
];

// Field indexes.
const F_TITLE: usize = 0;
const F_DATE: usize = 1;
const F_START: usize = 2;
const F_END: usize = 3;
const F_EVERY: usize = 4;
const F_UNTIL: usize = 5;
const F_NOTES: usize = 6;

pub struct EvSheet {
    pub id: Option<u32>,
    pub cal: usize,
    pub fields: [Field; 7],
    pub all_day: bool,
    pub repeat: usize,
    pub alert: usize,
    pub focus: usize,
    pub error: Option<&'static str>,
}

pub struct CalSheet {
    pub name: Field,
    pub colour: usize,
}

impl CalSheet {
    pub fn new() -> CalSheet {
        CalSheet {
            name: Field::new("", 24),
            colour: 0,
        }
    }
}

impl Default for CalSheet {
    fn default() -> Self {
        CalSheet::new()
    }
}

fn accepts(idx: usize, c: char) -> bool {
    match idx {
        F_DATE | F_UNTIL => c.is_ascii_digit() || c == '-',
        F_START | F_END => c.is_ascii_digit() || c == ':',
        F_EVERY => c.is_ascii_digit(),
        _ => true,
    }
}

impl App {
    pub fn new_event(&mut self, date: Date, hour_min: Option<u16>) {
        let start = hour_min.unwrap_or(if date == self.today {
            let (_, sod) = timekit::date::split_secs(timeio::now_secs());
            (((sod / 3600) + 1) % 24) as u16 * 60
        } else {
            9 * 60
        });
        let end = (start + 60).min(23 * 60 + 59);
        self.sheet = Some(EvSheet {
            id: None,
            cal: self.set.cals.iter().position(|c| !c.hidden).unwrap_or(0),
            fields: [
                Field::new("", 40),
                Field::new(&date.fmt(), 10),
                Field::new(&fmt_hm(start), 5),
                Field::new(&fmt_hm(end), 5),
                Field::numeric("1", 3),
                Field::new("", 10),
                Field::new("", 60),
            ],
            all_day: false,
            repeat: 0,
            alert: 0,
            focus: F_TITLE,
            error: None,
        });
    }

    pub fn edit_event(&mut self, id: u32) {
        let Some((cal, e)) = self.set.get(id) else {
            return;
        };
        let e = e.clone();
        let alert = ALERTS.iter().position(|a| a.1 == e.alert).unwrap_or(0);
        let repeat = REPEATS.iter().position(|r| r.1 == e.repeat).unwrap_or(0);
        self.sheet = Some(EvSheet {
            id: Some(id),
            cal,
            fields: [
                Field::new(&e.title, 40),
                Field::new(&e.date.fmt(), 10),
                Field::new(&e.start.map_or(String::new(), fmt_hm), 5),
                Field::new(&e.end.map_or(String::new(), fmt_hm), 5),
                Field::numeric(&format!("{}", e.every), 3),
                Field::new(&e.until.map_or(String::new(), |d| d.fmt()), 10),
                Field::new(&e.notes, 60),
            ],
            all_day: e.start.is_none(),
            repeat,
            alert,
            focus: F_TITLE,
            error: None,
        });
    }

    fn save_event(&mut self) {
        let Some(s) = &mut self.sheet else { return };
        let f = &s.fields;
        let title = f[F_TITLE].text.trim();
        if title.is_empty() {
            s.error = Some("Give the event a title");
            return;
        }
        let Some(date) = Date::parse(&f[F_DATE].text) else {
            s.error = Some("The date looks like 2026-10-21");
            return;
        };
        let (start, end) = if s.all_day {
            (None, None)
        } else {
            let Some(st) = parse_hm(&f[F_START].text) else {
                s.error = Some("The start looks like 09:30");
                return;
            };
            let en = if f[F_END].text.trim().is_empty() {
                None
            } else {
                match parse_hm(&f[F_END].text) {
                    Some(en) if en >= st => Some(en),
                    _ => {
                        s.error = Some("The end looks like 10:30, after the start");
                        return;
                    }
                }
            };
            (Some(st), en)
        };
        let repeat = REPEATS[s.repeat].1;
        let every: u16 = f[F_EVERY].text.parse().unwrap_or(1).clamp(1, 999);
        let until = if repeat == Repeat::None || f[F_UNTIL].text.trim().is_empty() {
            None
        } else {
            match Date::parse(&f[F_UNTIL].text) {
                Some(u) if u >= date => Some(u),
                _ => {
                    s.error = Some("Until looks like 2027-01-31, after the date");
                    return;
                }
            }
        };
        let mut e = Event::new(0, title, date);
        e.notes = f[F_NOTES].text.clone();
        e.start = start;
        e.end = end;
        e.repeat = repeat;
        e.every = if repeat == Repeat::None { 1 } else { every };
        e.until = until;
        e.alert = ALERTS[s.alert].1;
        let (id, cal) = (s.id, s.cal);
        match id {
            Some(id) => {
                if let Some(slot) = self.set.get_mut(id) {
                    e.id = id;
                    *slot = e;
                }
                self.set.move_event(id, cal);
            }
            None => {
                self.set.add(cal, e);
            }
        }
        self.cursor = date;
        self.save();
        self.sheet = None;
    }

    fn save_calendar(&mut self) {
        let Some(c) = self.cal_sheet.take() else {
            return;
        };
        let name = c.name.text.trim();
        if name.is_empty() || self.set.cals.iter().any(|x| x.name == name) {
            self.cal_sheet = Some(c);
            return;
        }
        self.set
            .cals
            .push(timekit::cal::Calendar::new(name, COLOURS[c.colour]));
        self.save();
    }

    pub fn sheet_click(&mut self, id: u32) {
        if self.cal_sheet.is_some() {
            match id {
                CAL_SAVE => self.save_calendar(),
                CAL_CANCEL => self.cal_sheet = None,
                c if (CAL_COLOUR..CAL_COLOUR + 5).contains(&c) => {
                    if let Some(s) = &mut self.cal_sheet {
                        s.colour = (c - CAL_COLOUR) as usize;
                    }
                }
                _ => {}
            }
            return;
        }
        match id {
            SAVE => self.save_event(),
            CANCEL => self.sheet = None,
            DELETE => {
                if let Some(i) = self.sheet.as_ref().and_then(|s| s.id) {
                    self.set.remove(i);
                    self.save();
                }
                self.sheet = None;
            }
            _ => {
                let Some(s) = &mut self.sheet else { return };
                s.error = None;
                match id {
                    ALL_DAY => s.all_day = !s.all_day,
                    r if (REPEAT..REPEAT + 5).contains(&r) => s.repeat = (r - REPEAT) as usize,
                    a if (ALERT..ALERT + 6).contains(&a) => s.alert = (a - ALERT) as usize,
                    c if (CAL..CAL + 50).contains(&c) => s.cal = (c - CAL) as usize,
                    f if (FIELD..FIELD + 7).contains(&f) => s.focus = (f - FIELD) as usize,
                    _ => {}
                }
            }
        }
    }

    pub fn sheet_key(&mut self, k: Key, m: Mods) {
        if let Some(c) = &mut self.cal_sheet {
            match k {
                Key::Escape => self.cal_sheet = None,
                Key::Enter => self.save_calendar(),
                _ => {
                    c.name.key(k, m);
                }
            }
            return;
        }
        let Some(s) = &mut self.sheet else { return };
        match k {
            Key::Escape => self.sheet = None,
            Key::Enter => self.save_event(),
            Key::Tab => {
                // Next visible field.
                let visible = |i: usize| match i {
                    F_START | F_END => !s.all_day,
                    F_EVERY | F_UNTIL => s.repeat != 0,
                    _ => true,
                };
                let mut i = s.focus;
                for _ in 0..7 {
                    i = (i + 1) % 7;
                    if visible(i) {
                        break;
                    }
                }
                s.focus = i;
            }
            _ => {
                s.error = None;
                if let Key::Char(c) = k
                    && !accepts(s.focus, c)
                {
                    return;
                }
                let i = s.focus;
                s.fields[i].key(k, m);
            }
        }
    }

    pub fn draw_sheets(&mut self, win: &mut Window) {
        if self.cal_sheet.is_some() {
            self.draw_cal_sheet(win);
        } else {
            self.draw_event_sheet(win);
        }
    }

    fn frame(&self, win: &mut Window, w: i32, h: i32, title: &str) -> Rect {
        let th = self.th;
        let r = Rect::new((W - w) / 2, (H - h) / 2, w, h);
        win.rrect(
            r.x - 1,
            r.y + 6,
            r.w + 2,
            r.h + 2,
            18,
            blend(th.milk, th.ink, 0x33),
        );
        widgets::bordered(win, r, 18, th.milk, th.sand);
        bold_x(win, r.x + 20, r.y + 16, title, th.espresso, 2);
        r
    }

    fn draw_event_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(s) = self.sheet.take() else { return };
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        let r = self.frame(
            win,
            480,
            560,
            if s.id.is_some() {
                "Edit event"
            } else {
                "New event"
            },
        );
        let (x, iw) = (r.x + 20, r.w - 40);
        let f = &s.fields;
        let label = |win: &mut Window, y: i32, t: &str| {
            win.text(x, y, t, th.cocoa);
        };
        // Title.
        label(win, r.y + 60, "TITLE");
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 80, iw, 28),
            &f[F_TITLE],
            "Event title",
            s.focus == F_TITLE,
            caret,
            FIELD + F_TITLE as u32,
        );
        // When.
        label(win, r.y + 120, "WHEN");
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 140, 112, 28),
            &f[F_DATE],
            "2026-10-21",
            s.focus == F_DATE,
            caret,
            FIELD + F_DATE as u32,
        );
        if !s.all_day {
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(x + 124, r.y + 140, 64, 28),
                &f[F_START],
                "09:00",
                s.focus == F_START,
                caret,
                FIELD + F_START as u32,
            );
            win.text(x + 194, r.y + 146, "to", th.cocoa);
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(x + 216, r.y + 140, 64, 28),
                &f[F_END],
                "10:00",
                s.focus == F_END,
                caret,
                FIELD + F_END as u32,
            );
        }
        win.text(x + iw - 44 - 8 - 56, r.y + 146, "All day", th.espresso);
        switch(
            win,
            &th,
            &mut self.hits,
            x + iw - 44,
            r.y + 142,
            s.all_day,
            ALL_DAY,
        );
        // Repeat.
        label(win, r.y + 184, "REPEAT");
        let mut cx = x;
        for (i, (name, _)) in REPEATS.iter().enumerate() {
            cx += chip(
                win,
                &th,
                &mut self.hits,
                cx,
                r.y + 204,
                name,
                s.repeat == i,
                REPEAT + i as u32,
            ) + 6;
        }
        if s.repeat != 0 {
            win.text(x, r.y + 240, "every", th.cocoa);
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(x + 52, r.y + 234, 48, 28),
                &f[F_EVERY],
                "1",
                s.focus == F_EVERY,
                caret,
                FIELD + F_EVERY as u32,
            );
            win.text(x + 116, r.y + 240, "until", th.cocoa);
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(x + 168, r.y + 234, 112, 28),
                &f[F_UNTIL],
                "no end",
                s.focus == F_UNTIL,
                caret,
                FIELD + F_UNTIL as u32,
            );
        }
        // Alert.
        label(win, r.y + 280, "ALERT");
        let mut cx = x;
        for (i, (name, _)) in ALERTS.iter().enumerate() {
            cx += chip(
                win,
                &th,
                &mut self.hits,
                cx,
                r.y + 300,
                name,
                s.alert == i,
                ALERT + i as u32,
            ) + 6;
        }
        // Calendar.
        label(win, r.y + 344, "CALENDAR");
        let mut cx = x;
        let mut cy = r.y + 364;
        for i in 0..self.set.cals.len() {
            let (name, colour) = (
                self.set.cals[i].name.clone(),
                self.set.cals[i].colour.clone(),
            );
            let w = gui::str_width(&name, 1) + 24 + 14;
            if cx + w > x + iw {
                cx = x;
                cy += 30;
            }
            let on = s.cal == i;
            let rect = Rect::new(cx, cy, w, 24);
            if on {
                widgets::bordered(
                    win,
                    rect,
                    8,
                    Theme::tint(th.milk, th.accent(&colour), 0x66),
                    th.accent(&colour),
                );
            } else {
                widgets::bordered(win, rect, 8, th.oat, th.sand);
            }
            win.disc(cx + 14, cy + 12, 5, th.accent(&colour));
            win.text(cx + 26, cy + 4, &name, th.espresso);
            self.hits.add(rect, CAL + i as u32);
            cx += w + 6;
        }
        // Notes.
        label(win, r.y + 424, "NOTES");
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 444, iw, 28),
            &f[F_NOTES],
            "",
            s.focus == F_NOTES,
            caret,
            FIELD + F_NOTES as u32,
        );
        if let Some(err) = s.error {
            win.text(x, r.y + 482, err, th.danger);
        }
        // Buttons.
        let by = r.bottom() - 48;
        let sw = button_w("Save", false);
        let mut bx = r.right() - 20 - sw;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(bx, by, sw, 28),
            "Save",
            Kind::Primary,
            SAVE,
            true,
            false,
        );
        let cw = button_w("Cancel", false);
        bx -= cw + 8;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(bx, by, cw, 28),
            "Cancel",
            Kind::Secondary,
            CANCEL,
            true,
            false,
        );
        if s.id.is_some() {
            let dw = button_w("Delete", false);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(x, by, dw, 28),
                "Delete",
                Kind::Danger,
                DELETE,
                true,
                false,
            );
        }
        self.sheet = Some(s);
    }

    fn draw_cal_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(c) = self.cal_sheet.take() else {
            return;
        };
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        let r = self.frame(win, 400, 240, "New calendar");
        let x = r.x + 20;
        win.text(x, r.y + 60, "NAME", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 80, r.w - 40, 28),
            &c.name,
            "Garden",
            true,
            caret,
            FIELD,
        );
        win.text(x, r.y + 120, "COLOUR", th.cocoa);
        let mut cx = x;
        for (i, name) in COLOURS.iter().enumerate() {
            let rect = Rect::new(cx, r.y + 142, 32, 32);
            let acc = th.accent(name);
            if c.colour == i {
                win.disc(cx + 16, r.y + 158, 16, th.espresso);
                win.disc(cx + 16, r.y + 158, 14, th.milk);
            }
            win.disc(cx + 16, r.y + 158, 11, acc);
            self.hits.add(rect, CAL_COLOUR + i as u32);
            cx += 40;
        }
        let by = r.bottom() - 48;
        let sw = button_w("Add", false);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.right() - 20 - sw, by, sw, 28),
            "Add",
            Kind::Primary,
            CAL_SAVE,
            true,
            false,
        );
        let cw = button_w("Cancel", false);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.right() - 20 - sw - 8 - cw, by, cw, 28),
            "Cancel",
            Kind::Secondary,
            CAL_CANCEL,
            true,
            false,
        );
        self.cal_sheet = Some(c);
    }
}
