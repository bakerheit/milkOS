//! The reminder sheet and the "add list" sheet.

use crate::{App, H, SHEET, W};
use alloc::format;
use alloc::string::String;
use appkit::widgets::{self, Kind, bold_x, button, button_w, chip, field, switch};
use appkit::{Field, Rect, Theme};
use gui::{Key, Mods, Window, blend};
use timekit::Date;
use timekit::cal::{COLOURS, Repeat};
use timekit::reminders::{Reminder, ReminderList};
use timekit::text::{fmt_hm, parse_hm};

const SAVE: u32 = SHEET;
const CANCEL: u32 = SHEET + 1;
const DELETE: u32 = SHEET + 2;
const FLAG: u32 = SHEET + 3;
const REPEAT: u32 = SHEET + 10; // + 0..5
const LISTC: u32 = SHEET + 20; // + list index
const FIELD: u32 = SHEET + 50; // + field index
const DATE_CHIP: u32 = SHEET + 60; // + 0 today, 1 tomorrow, 2 none
const LIST_SAVE: u32 = SHEET + 70;
const LIST_CANCEL: u32 = SHEET + 71;
const LIST_COLOUR: u32 = SHEET + 80; // + 0..5

const REPEATS: [(&str, Repeat); 5] = [
    ("Never", Repeat::None),
    ("Daily", Repeat::Daily),
    ("Weekly", Repeat::Weekly),
    ("Monthly", Repeat::Monthly),
    ("Yearly", Repeat::Yearly),
];

// Field indexes.
const F_TITLE: usize = 0;
const F_NOTES: usize = 1;
const F_DATE: usize = 2;
const F_TIME: usize = 3;
const F_EVERY: usize = 4;
const F_UNTIL: usize = 5;

pub struct RemSheet {
    pub id: Option<u32>,
    pub list: usize,
    pub fields: [Field; 6],
    pub repeat: usize,
    pub flagged: bool,
    pub focus: usize,
    pub error: Option<&'static str>,
}

pub struct ListSheet {
    pub name: Field,
    pub colour: usize,
}

impl ListSheet {
    pub fn new() -> ListSheet {
        ListSheet {
            name: Field::new("", 24),
            colour: 0,
        }
    }
}

impl Default for ListSheet {
    fn default() -> Self {
        ListSheet::new()
    }
}

/// "daily", "every 2 weeks".
pub fn repeat_word(r: &Reminder) -> String {
    let unit = match r.repeat {
        Repeat::None => return String::new(),
        Repeat::Daily => ("daily", "days"),
        Repeat::Weekly => ("weekly", "weeks"),
        Repeat::Monthly => ("monthly", "months"),
        Repeat::Yearly => ("yearly", "years"),
    };
    if r.every <= 1 {
        String::from(unit.0)
    } else {
        format!("every {} {}", r.every, unit.1)
    }
}

fn accepts(idx: usize, c: char) -> bool {
    match idx {
        F_DATE | F_UNTIL => c.is_ascii_digit() || c == '-',
        F_TIME => c.is_ascii_digit() || c == ':',
        F_EVERY => c.is_ascii_digit(),
        _ => true,
    }
}

impl App {
    pub fn new_reminder(&mut self) {
        let list = match self.view {
            crate::View::List(i) => i,
            _ => 0,
        };
        let due = if self.view == crate::View::Today {
            self.today.fmt()
        } else {
            String::new()
        };
        self.sheet = Some(RemSheet {
            id: None,
            list,
            fields: [
                Field::new(self.quick.text.trim(), 60),
                Field::new("", 80),
                Field::new(&due, 10),
                Field::new("", 5),
                Field::numeric("1", 3),
                Field::new("", 10),
            ],
            repeat: 0,
            flagged: self.view == crate::View::Flagged,
            focus: F_TITLE,
            error: None,
        });
        self.quick.set("");
    }

    pub fn edit_reminder(&mut self, id: u32) {
        let Some((list, r)) = self.set.get(id) else {
            return;
        };
        let r = r.clone();
        self.sheet = Some(RemSheet {
            id: Some(id),
            list,
            fields: [
                Field::new(&r.title, 60),
                Field::new(&r.notes, 80),
                Field::new(&r.due.map_or(String::new(), |d| d.fmt()), 10),
                Field::new(&r.time.map_or(String::new(), fmt_hm), 5),
                Field::numeric(&format!("{}", r.every), 3),
                Field::new(&r.until.map_or(String::new(), |d| d.fmt()), 10),
            ],
            repeat: REPEATS.iter().position(|x| x.1 == r.repeat).unwrap_or(0),
            flagged: r.flagged,
            focus: F_TITLE,
            error: None,
        });
    }

    fn save_reminder(&mut self) {
        let Some(s) = &mut self.sheet else { return };
        let f = &s.fields;
        let title = f[F_TITLE].text.trim();
        if title.is_empty() {
            s.error = Some("Give the reminder a title");
            return;
        }
        let due = if f[F_DATE].text.trim().is_empty() {
            None
        } else {
            match Date::parse(&f[F_DATE].text) {
                Some(d) => Some(d),
                None => {
                    s.error = Some("The date looks like 2026-10-21");
                    return;
                }
            }
        };
        let time = if f[F_TIME].text.trim().is_empty() {
            None
        } else {
            match parse_hm(&f[F_TIME].text) {
                Some(t) if due.is_some() => Some(t),
                Some(_) => {
                    s.error = Some("A time needs a date");
                    return;
                }
                None => {
                    s.error = Some("The time looks like 09:30");
                    return;
                }
            }
        };
        let repeat = REPEATS[s.repeat].1;
        if repeat != Repeat::None && due.is_none() {
            s.error = Some("A repeating reminder needs a date");
            return;
        }
        let until = if repeat == Repeat::None || f[F_UNTIL].text.trim().is_empty() {
            None
        } else {
            match (Date::parse(&f[F_UNTIL].text), due) {
                (Some(u), Some(d)) if u >= d => Some(u),
                _ => {
                    s.error = Some("Until looks like 2027-01-31, after the date");
                    return;
                }
            }
        };
        let mut r = Reminder::new(0, title);
        r.notes = f[F_NOTES].text.clone();
        r.due = due;
        r.time = time;
        r.repeat = repeat;
        r.every = if repeat == Repeat::None {
            1
        } else {
            f[F_EVERY].text.parse().unwrap_or(1).clamp(1, 999)
        };
        r.until = until;
        r.flagged = s.flagged;
        let (id, list) = (s.id, s.list);
        match id {
            Some(id) => {
                if let Some(slot) = self.set.get_mut(id) {
                    r.id = id;
                    r.done = slot.done;
                    *slot = r;
                }
                self.set.move_to(id, list);
            }
            None => {
                self.set.add(list, r);
            }
        }
        self.save();
        self.sheet = None;
    }

    fn save_list(&mut self) {
        let Some(c) = self.list_sheet.take() else {
            return;
        };
        let name = c.name.text.trim();
        if name.is_empty() || self.set.lists.iter().any(|l| l.name == name) {
            self.list_sheet = Some(c);
            return;
        }
        self.set
            .lists
            .push(ReminderList::new(name, COLOURS[c.colour]));
        self.view = crate::View::List(self.set.lists.len() - 1);
        self.save();
    }

    pub fn sheet_click(&mut self, id: u32) {
        if self.list_sheet.is_some() {
            match id {
                LIST_SAVE => self.save_list(),
                LIST_CANCEL => self.list_sheet = None,
                c if (LIST_COLOUR..LIST_COLOUR + 5).contains(&c) => {
                    if let Some(s) = &mut self.list_sheet {
                        s.colour = (c - LIST_COLOUR) as usize;
                    }
                }
                _ => {}
            }
            return;
        }
        match id {
            SAVE => self.save_reminder(),
            CANCEL => self.sheet = None,
            DELETE => {
                if let Some(i) = self.sheet.as_ref().and_then(|s| s.id) {
                    self.set.remove(i);
                    self.save();
                }
                self.sheet = None;
            }
            _ => {
                let today = self.today;
                let Some(s) = &mut self.sheet else { return };
                s.error = None;
                match id {
                    FLAG => s.flagged = !s.flagged,
                    r if (REPEAT..REPEAT + 5).contains(&r) => s.repeat = (r - REPEAT) as usize,
                    l if (LISTC..LISTC + 50).contains(&l) => s.list = (l - LISTC) as usize,
                    f if (FIELD..FIELD + 6).contains(&f) => s.focus = (f - FIELD) as usize,
                    d if (DATE_CHIP..DATE_CHIP + 3).contains(&d) => {
                        let text = match d - DATE_CHIP {
                            0 => today.fmt(),
                            1 => today.add_days(1).fmt(),
                            _ => String::new(),
                        };
                        s.fields[F_DATE].set(&text);
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn sheet_key(&mut self, k: Key, m: Mods) {
        if let Some(c) = &mut self.list_sheet {
            match k {
                Key::Escape => self.list_sheet = None,
                Key::Enter => self.save_list(),
                _ => {
                    c.name.key(k, m);
                }
            }
            return;
        }
        let Some(s) = &mut self.sheet else { return };
        match k {
            Key::Escape => self.sheet = None,
            Key::Enter => self.save_reminder(),
            Key::Tab => {
                let visible = |i: usize| match i {
                    F_EVERY | F_UNTIL => s.repeat != 0,
                    _ => true,
                };
                let mut i = s.focus;
                for _ in 0..6 {
                    i = (i + 1) % 6;
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
        if self.list_sheet.is_some() {
            self.draw_list_sheet(win);
        } else {
            self.draw_rem_sheet(win);
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

    fn draw_rem_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(s) = self.sheet.take() else { return };
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        let r = self.frame(
            win,
            480,
            540,
            if s.id.is_some() {
                "Edit reminder"
            } else {
                "New reminder"
            },
        );
        let (x, iw) = (r.x + 20, r.w - 40);
        let f = &s.fields;
        let label = |win: &mut Window, y: i32, t: &str| {
            win.text(x, y, t, th.cocoa);
        };
        label(win, r.y + 60, "TITLE");
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 80, iw, 28),
            &f[F_TITLE],
            "Reminder",
            s.focus == F_TITLE,
            caret,
            FIELD + F_TITLE as u32,
        );
        label(win, r.y + 120, "NOTES");
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 140, iw, 28),
            &f[F_NOTES],
            "",
            s.focus == F_NOTES,
            caret,
            FIELD + F_NOTES as u32,
        );
        label(win, r.y + 180, "DUE");
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 200, 112, 28),
            &f[F_DATE],
            "no date",
            s.focus == F_DATE,
            caret,
            FIELD + F_DATE as u32,
        );
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x + 124, r.y + 200, 64, 28),
            &f[F_TIME],
            "09:00",
            s.focus == F_TIME,
            caret,
            FIELD + F_TIME as u32,
        );
        let mut cx = x + 204;
        for (i, name) in ["Today", "Tomorrow", "None"].iter().enumerate() {
            cx += chip(
                win,
                &th,
                &mut self.hits,
                cx,
                r.y + 202,
                name,
                false,
                DATE_CHIP + i as u32,
            ) + 6;
        }
        label(win, r.y + 244, "REPEAT");
        let mut cx = x;
        for (i, (name, _)) in REPEATS.iter().enumerate() {
            cx += chip(
                win,
                &th,
                &mut self.hits,
                cx,
                r.y + 264,
                name,
                s.repeat == i,
                REPEAT + i as u32,
            ) + 6;
        }
        if s.repeat != 0 {
            win.text(x, r.y + 300, "every", th.cocoa);
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(x + 52, r.y + 294, 48, 28),
                &f[F_EVERY],
                "1",
                s.focus == F_EVERY,
                caret,
                FIELD + F_EVERY as u32,
            );
            win.text(x + 116, r.y + 300, "until", th.cocoa);
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(x + 168, r.y + 294, 112, 28),
                &f[F_UNTIL],
                "no end",
                s.focus == F_UNTIL,
                caret,
                FIELD + F_UNTIL as u32,
            );
        }
        label(win, r.y + 340, "LIST");
        let mut cx = x;
        let mut cy = r.y + 360;
        for i in 0..self.set.lists.len() {
            let (name, colour) = (
                self.set.lists[i].name.clone(),
                self.set.lists[i].colour.clone(),
            );
            let w = gui::str_width(&name, 1) + 24 + 14;
            if cx + w > x + iw {
                cx = x;
                cy += 30;
            }
            let rect = Rect::new(cx, cy, w, 24);
            let acc = th.accent(&colour);
            if s.list == i {
                widgets::bordered(win, rect, 8, Theme::tint(th.milk, acc, 0x66), acc);
            } else {
                widgets::bordered(win, rect, 8, th.oat, th.sand);
            }
            win.disc(cx + 14, cy + 12, 5, acc);
            win.text(cx + 26, cy + 4, &name, th.espresso);
            self.hits.add(rect, LISTC + i as u32);
            cx += w + 6;
        }
        win.text(x, r.y + 428, "Flagged", th.espresso);
        switch(win, &th, &mut self.hits, x + 80, r.y + 424, s.flagged, FLAG);
        if let Some(err) = s.error {
            win.text(x, r.y + 462, err, th.danger);
        }
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

    fn draw_list_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(c) = self.list_sheet.take() else {
            return;
        };
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        let r = self.frame(win, 400, 240, "New list");
        let x = r.x + 20;
        win.text(x, r.y + 60, "NAME", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x, r.y + 80, r.w - 40, 28),
            &c.name,
            "Groceries",
            true,
            caret,
            FIELD,
        );
        win.text(x, r.y + 120, "COLOUR", th.cocoa);
        let mut cx = x;
        for (i, name) in COLOURS.iter().enumerate() {
            let acc = th.accent(name);
            if c.colour == i {
                win.disc(cx + 16, r.y + 158, 16, th.espresso);
                win.disc(cx + 16, r.y + 158, 14, th.milk);
            }
            win.disc(cx + 16, r.y + 158, 11, acc);
            self.hits
                .add(Rect::new(cx, r.y + 142, 32, 32), LIST_COLOUR + i as u32);
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
            LIST_SAVE,
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
            LIST_CANCEL,
            true,
            false,
        );
        self.list_sheet = Some(c);
    }
}
