//! Editing a reminder in place, and the add-list sheet.

use crate::{App, EDIT, SHEET};
use alloc::format;
use alloc::string::String;
use appkit::widgets::{self, Kind, bold_x, button, button_w, chip, field, switch};
use appkit::{Field, Rect, Theme};
use gui::{Key, Mods, Window, blend};
use timekit::Date;
use timekit::cal::{COLOURS, Repeat};
use timekit::reminders::{Reminder, ReminderList};
use timekit::text::{fmt_hm, parse_hm};

pub const F_TITLE: usize = 0;
pub const F_NOTES: usize = 1;
pub const F_DATE: usize = 2;
pub const F_TIME: usize = 3;
pub const F_EVERY: usize = 4;

// Editor control ids (all above `EDIT`).
pub const FIELD: u32 = EDIT; // + field index
pub const REPEAT: u32 = EDIT + 10; // + 0..5
pub const LISTC: u32 = EDIT + 20; // + list index
pub const ALERT: u32 = EDIT + 80;
pub const DATE_CHIP: u32 = EDIT + 81; // + 0 today, 1 tomorrow, 2 none
pub const SAVE: u32 = EDIT + 90;
pub const CANCEL: u32 = EDIT + 91;
pub const DELETE: u32 = EDIT + 92;

const REPEATS: [(&str, Repeat); 5] = [
    ("Never", Repeat::None),
    ("Daily", Repeat::Daily),
    ("Weekly", Repeat::Weekly),
    ("Monthly", Repeat::Monthly),
    ("Yearly", Repeat::Yearly),
];

// Add-list sheet ids.
const LIST_SAVE: u32 = SHEET;
const LIST_CANCEL: u32 = SHEET + 1;
const LIST_COLOUR: u32 = SHEET + 10; // + 0..5

pub struct Edit {
    pub id: u32,
    pub list: usize,
    pub fields: [Field; 5],
    pub repeat: usize,
    pub alert: bool,
    pub focus: usize,
    pub error: Option<&'static str>,
    /// Keeps the repeat's end date (not shown in the editor) when saving.
    pub until: Option<Date>,
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

fn accepts(idx: usize, c: char) -> bool {
    match idx {
        F_DATE => c.is_ascii_digit() || c == '-',
        F_TIME => c.is_ascii_digit() || c == ':',
        F_EVERY => c.is_ascii_digit(),
        _ => true,
    }
}

impl App {
    pub fn begin_edit(&mut self, id: u32) {
        let Some((list, r)) = self.set.get(id) else {
            return;
        };
        let r = r.clone();
        self.selected = Some(id);
        self.edit = Some(Edit {
            id,
            list,
            fields: [
                Field::new(&r.title, 80),
                Field::new(&r.notes, 120),
                Field::new(&r.due.map_or(String::new(), |d| d.fmt()), 10),
                Field::new(&r.time.map_or(String::new(), fmt_hm), 5),
                Field::numeric(&format!("{}", r.every), 3),
            ],
            repeat: REPEATS.iter().position(|x| x.1 == r.repeat).unwrap_or(0),
            alert: r.alert,
            focus: F_TITLE,
            error: None,
            until: r.until,
        });
    }

    /// Close the editor. With `save`, check and store the changes first; returns false (and keeps
    /// the editor open with a message) if something does not read.
    pub fn finish_edit(&mut self, save: bool) -> bool {
        if !save {
            self.edit = None;
            return true;
        }
        let Some(e) = &mut self.edit else { return true };
        let f = &e.fields;
        let title = f[F_TITLE].text.trim();
        if title.is_empty() {
            e.error = Some("Give the reminder a title");
            return false;
        }
        let due = if f[F_DATE].text.trim().is_empty() {
            None
        } else {
            match Date::parse(&f[F_DATE].text) {
                Some(d) => Some(d),
                None => {
                    e.error = Some("The date looks like 2026-10-21");
                    return false;
                }
            }
        };
        let time = if f[F_TIME].text.trim().is_empty() {
            None
        } else {
            match (parse_hm(&f[F_TIME].text), due) {
                (Some(t), Some(_)) => Some(t),
                (Some(_), None) => {
                    e.error = Some("A time needs a date");
                    return false;
                }
                (None, _) => {
                    e.error = Some("The time looks like 09:30");
                    return false;
                }
            }
        };
        let repeat = REPEATS[e.repeat].1;
        if repeat != Repeat::None && due.is_none() {
            e.error = Some("A repeating reminder needs a date");
            return false;
        }
        let (id, list, alert, until) = (e.id, e.list, e.alert, e.until);
        let notes = f[F_NOTES].text.clone();
        let every: u16 = f[F_EVERY].text.parse().unwrap_or(1).clamp(1, 999);
        let title = String::from(title);
        if let Some(r) = self.set.get_mut(id) {
            r.title = title;
            r.notes = notes;
            r.due = due;
            r.time = time;
            r.repeat = repeat;
            r.every = if repeat == Repeat::None { 1 } else { every };
            r.until = if repeat == Repeat::None { None } else { until };
            r.alert = alert;
        }
        self.set.move_to(id, list);
        self.edit = None;
        self.save();
        true
    }

    pub fn edit_click(&mut self, id: u32) {
        let today = self.today;
        match id {
            SAVE => {
                self.finish_edit(true);
            }
            CANCEL => {
                self.finish_edit(false);
            }
            DELETE => {
                if let Some(e) = self.edit.take() {
                    self.delete(e.id);
                }
            }
            _ => {
                let Some(e) = &mut self.edit else { return };
                e.error = None;
                match id {
                    ALERT => e.alert = !e.alert,
                    r if (REPEAT..REPEAT + 5).contains(&r) => e.repeat = (r - REPEAT) as usize,
                    l if (LISTC..LISTC + 50).contains(&l) => e.list = (l - LISTC) as usize,
                    f if (FIELD..FIELD + 5).contains(&f) => e.focus = (f - FIELD) as usize,
                    d if (DATE_CHIP..DATE_CHIP + 3).contains(&d) => {
                        let text = match d - DATE_CHIP {
                            0 => today.fmt(),
                            1 => today.add_days(1).fmt(),
                            _ => String::new(),
                        };
                        e.fields[F_DATE].set(&text);
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn edit_key(&mut self, k: Key, m: Mods) {
        let Some(e) = &mut self.edit else { return };
        match k {
            Key::Escape => {
                self.edit = None;
            }
            Key::Enter => {
                self.finish_edit(true);
            }
            Key::Tab => {
                let visible = |i: usize| i != F_EVERY || e.repeat != 0;
                let mut i = e.focus;
                for _ in 0..5 {
                    i = (i + 1) % 5;
                    if visible(i) {
                        break;
                    }
                }
                e.focus = i;
            }
            _ => {
                e.error = None;
                if let Key::Char(c) = k
                    && !accepts(e.focus, c)
                {
                    return;
                }
                let i = e.focus;
                e.fields[i].key(k, m);
            }
        }
    }

    // ---- the add-list sheet ----------------------------------------------------------------

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

    pub fn list_click(&mut self, id: u32) {
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
    }

    pub fn list_key(&mut self, k: Key, m: Mods) {
        let Some(c) = &mut self.list_sheet else {
            return;
        };
        match k {
            Key::Escape => self.list_sheet = None,
            Key::Enter => self.save_list(),
            _ => {
                c.name.key(k, m);
            }
        }
    }

    pub fn draw_list_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(c) = self.list_sheet.take() else {
            return;
        };
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        let r = Rect::new((win.w - 400) / 2, (win.h - 240) / 2, 400, 240);
        win.rrect(
            r.x - 1,
            r.y + 6,
            r.w + 2,
            r.h + 2,
            16,
            blend(th.milk, th.ink, 0x33),
        );
        widgets::bordered(win, r, 14, th.milk, th.sand);
        bold_x(win, r.x + 20, r.y + 16, "New list", th.espresso, 2);
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
            SHEET + 5,
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

    /// Draw the editor block for the reminder being edited, starting at `y`; returns its height.
    pub fn draw_editor(&mut self, win: &mut Window, x0: i32, y: i32, w: i32) -> i32 {
        let th = self.th;
        let Some(e) = self.edit.take() else { return 0 };
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        let h = 270;
        let r = Rect::new(x0 - 8, y, w + 16, h);
        widgets::bordered(win, r, 12, th.cream, th.sand);
        let f = &e.fields;
        let x = x0 + 8;
        let iw = w - 16;
        win.text(x, y + 10, "TITLE", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x + 70, y + 6, iw - 70, 26),
            &f[F_TITLE],
            "Reminder",
            e.focus == F_TITLE,
            caret,
            FIELD + F_TITLE as u32,
        );
        win.text(x, y + 44, "NOTES", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x + 70, y + 40, iw - 70, 26),
            &f[F_NOTES],
            "",
            e.focus == F_NOTES,
            caret,
            FIELD + F_NOTES as u32,
        );
        win.text(x, y + 78, "DATE", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x + 70, y + 74, 112, 26),
            &f[F_DATE],
            "no date",
            e.focus == F_DATE,
            caret,
            FIELD + F_DATE as u32,
        );
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(x + 190, y + 74, 64, 26),
            &f[F_TIME],
            "time",
            e.focus == F_TIME,
            caret,
            FIELD + F_TIME as u32,
        );
        let mut cx = x + 266;
        for (i, name) in ["Today", "Tomorrow", "None"].iter().enumerate() {
            cx += chip(
                win,
                &th,
                &mut self.hits,
                cx,
                y + 75,
                name,
                false,
                DATE_CHIP + i as u32,
            ) + 6;
        }
        win.text(x, y + 112, "REPEAT", th.cocoa);
        let mut cx = x + 70;
        for (i, (name, _)) in REPEATS.iter().enumerate() {
            cx += chip(
                win,
                &th,
                &mut self.hits,
                cx,
                y + 108,
                name,
                e.repeat == i,
                REPEAT + i as u32,
            ) + 6;
        }
        if e.repeat != 0 {
            win.text(x + 70, y + 144, "every", th.cocoa);
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(x + 124, y + 140, 48, 26),
                &f[F_EVERY],
                "1",
                e.focus == F_EVERY,
                caret,
                FIELD + F_EVERY as u32,
            );
        }
        win.text(x, y + 178, "LIST", th.cocoa);
        let mut cx = x + 70;
        for i in 0..self.set.lists.len() {
            let (name, colour) = (
                self.set.lists[i].name.clone(),
                self.set.lists[i].colour.clone(),
            );
            let cw = gui::str_width(&name, 1) + 24 + 14;
            let rect = Rect::new(cx, y + 174, cw, 24);
            let acc = th.accent(&colour);
            if e.list == i {
                widgets::bordered(win, rect, 8, Theme::tint(th.cream, acc, 0x66), acc);
            } else {
                widgets::bordered(win, rect, 8, th.oat, th.sand);
            }
            win.disc(cx + 14, y + 186, 5, acc);
            win.text(cx + 26, y + 178, &name, th.espresso);
            self.hits.add(rect, LISTC + i as u32);
            cx += cw + 6;
        }
        win.text(x, y + 212, "ALERT", th.cocoa);
        switch(win, &th, &mut self.hits, x + 70, y + 208, e.alert, ALERT);
        match e.error {
            Some(err) => win.text(x + 124, y + 212, err, th.danger),
            None => win.text(
                x + 124,
                y + 212,
                if e.alert {
                    "Rings when it is due"
                } else {
                    "Silent"
                },
                th.cocoa,
            ),
        };
        let bw = button_w("Save", true);
        let mut bx = r.right() - 12 - bw;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(bx, y + h - 34, bw, 24),
            "Save",
            Kind::Primary,
            SAVE,
            true,
            true,
        );
        let cw = button_w("Cancel", true);
        bx -= cw + 8;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(bx, y + h - 34, cw, 24),
            "Cancel",
            Kind::Secondary,
            CANCEL,
            true,
            true,
        );
        let dw = button_w("Delete", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.x + 12, y + h - 34, dw, 24),
            "Delete",
            Kind::Danger,
            DELETE,
            true,
            true,
        );
        self.edit = Some(e);
        h
    }
}

/// Used by the row drawing: is this reminder one the editor has open?
pub fn editing(app: &App, r: &Reminder) -> bool {
    app.edit.as_ref().is_some_and(|e| e.id == r.id)
}
