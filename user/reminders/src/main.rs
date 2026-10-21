//! Reminders (docs/ui-concepts/SPEC-DISK-REMINDERS.md, section 2): lists with due dates, times and
//! repeats; quick add with a preview line; editing in place. alarmd rings them; this window edits
//! the files in ~/Library/Reminders (format in timekit/src/reminders.rs, docs/time-apps.md).

#![no_std]
#![no_main]

extern crate alloc;

mod edit;
mod view;

use alloc::string::String;
use alloc::vec::Vec;
use appkit::{Field, Hits, Theme};
use edit::{Edit, ListSheet};
use gui::{Event, Key, Window, blend};
use timekit::Date;
use timekit::date::split_secs;
use timekit::reminders::{Reminder, ReminderSet};

rt::main!(main);

pub const W: i32 = 780;
pub const H: i32 = 620;
pub const SIDE: i32 = 188;

// Hit ids.
pub const SMART: u32 = 10; // 10..13
pub const LIST: u32 = 100; // + list index
pub const NEW_LIST: u32 = 20;
pub const COMPLETED: u32 = 21;
pub const QUICK: u32 = 22;
pub const SEARCH: u32 = 23;
pub const UNDO: u32 = 24;
pub const CHECK: u32 = 1_000_000; // + reminder id
pub const ROW: u32 = 2_000_000; // + reminder id
pub const EDIT: u32 = 3_000_000; // editor controls
pub const SHEET: u32 = 4_000_000; // add-list sheet

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Today,
    Scheduled,
    All,
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
    pub search: Field,
    pub search_focus: bool,
    pub show_completed: bool,
    pub scroll: i32,
    pub selected: Option<u32>,
    pub edit: Option<Edit>,
    pub list_sheet: Option<ListSheet>,
    /// The last deleted reminder, its list, and when (for the Undo toast, 5 seconds).
    pub undo: Option<(Reminder, usize, u64)>,
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
        quick: Field::new("", 80),
        quick_focus: false,
        search: Field::new("", 30),
        search_focus: false,
        show_completed: true,
        scroll: 0,
        selected: None,
        edit: None,
        list_sheet: None,
        undo: None,
    };
    let mut last_load = 0;
    loop {
        match win.event(500) {
            Some(Event::Close) => break,
            Some(Event::Down { x, y, button: 1 }) => {
                app.quick_focus = false;
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
            Some(Event::Scroll(d)) => app.scroll = (app.scroll + d * 31).max(0),
            _ => {}
        }
        let now = timeio::mono_ms();
        if let Some((_, _, t)) = &app.undo
            && now.saturating_sub(*t) > 5000
        {
            app.undo = None;
        }
        if app.edit.is_none() && app.list_sheet.is_none() && now.saturating_sub(last_load) > 1500 {
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

    pub fn current_list(&self) -> usize {
        match self.view {
            View::List(i) if i < self.set.lists.len() => i,
            _ => 0,
        }
    }

    pub fn title(&self) -> String {
        match self.view {
            View::Today => String::from("Today"),
            View::Scheduled => String::from("Scheduled"),
            View::All => String::from("All"),
            View::List(i) => self
                .set
                .lists
                .get(i)
                .map_or(String::new(), |l| l.name.clone()),
        }
    }

    /// The open reminders this view shows (with their list), after the search filter.
    pub fn open_rows(&self) -> Vec<(usize, Reminder)> {
        let t = self.today;
        let all: Vec<(usize, Reminder)> = match self.view {
            View::Today => self.set.smart(|r| r.due.is_some_and(|d| d <= t)),
            View::Scheduled => self.set.smart(|r| r.due.is_some()),
            View::All => self.set.smart(|_| true),
            View::List(i) if i < self.set.lists.len() => {
                self.set.open_in(i).into_iter().map(|r| (i, r)).collect()
            }
            View::List(_) => Vec::new(),
        }
        .into_iter()
        .map(|(l, r)| (l, r.clone()))
        .collect();
        let q = self.search.text.to_lowercase();
        all.into_iter()
            .filter(|(_, r)| q.is_empty() || r.title.to_lowercase().contains(&q))
            .collect()
    }

    /// Completed reminders, in a list view and in All.
    pub fn done_rows(&self) -> Vec<(usize, Reminder)> {
        let lists: Vec<usize> = match self.view {
            View::List(i) => alloc::vec![i],
            View::All => (0..self.set.lists.len()).collect(),
            _ => Vec::new(),
        };
        let q = self.search.text.to_lowercase();
        lists
            .into_iter()
            .filter(|&i| i < self.set.lists.len())
            .flat_map(|i| self.set.done_in(i).into_iter().map(move |r| (i, r.clone())))
            .filter(|(_, r)| q.is_empty() || r.title.to_lowercase().contains(&q))
            .collect()
    }

    fn click(&mut self, id: u32) {
        if self.list_sheet.is_some() {
            self.list_click(id);
            return;
        }
        if (EDIT..SHEET).contains(&id) {
            self.edit_click(id);
            return;
        }
        // Clicking anywhere else saves a valid edit in progress (and stays if it does not read).
        if self.edit.is_some() && !self.finish_edit(true) {
            return;
        }
        match id {
            s if (SMART..SMART + 3).contains(&s) => {
                self.view = [View::Today, View::Scheduled, View::All][(s - SMART) as usize];
                self.scroll = 0;
                self.selected = None;
            }
            l if (LIST..LIST + 100).contains(&l) => {
                self.view = View::List((l - LIST) as usize);
                self.scroll = 0;
                self.selected = None;
            }
            NEW_LIST => self.list_sheet = Some(ListSheet::new()),
            COMPLETED => self.show_completed = !self.show_completed,
            QUICK => self.quick_focus = true,
            SEARCH => self.search_focus = true,
            UNDO => self.undo_delete(),
            c if (CHECK..ROW).contains(&c) => self.toggle_done(c - CHECK),
            r if (ROW..EDIT).contains(&r) => {
                let id = r - ROW;
                if self.selected == Some(id) {
                    self.begin_edit(id);
                } else {
                    self.selected = Some(id);
                }
            }
            _ => {}
        }
    }

    pub fn toggle_done(&mut self, id: u32) {
        let today = self.today;
        if let Some(r) = self.set.get_mut(id) {
            if r.is_done() {
                r.reopen();
            } else {
                r.complete(today);
            }
            self.save();
        }
    }

    pub fn delete(&mut self, id: u32) {
        if let Some((li, r)) = self.set.get(id).map(|(l, r)| (l, r.clone())) {
            self.set.remove(id);
            self.undo = Some((r, li, timeio::mono_ms()));
            self.selected = None;
            self.save();
        }
    }

    fn undo_delete(&mut self) {
        if let Some((r, li, _)) = self.undo.take() {
            let id = r.id;
            let li = li.min(self.set.lists.len() - 1);
            self.set.lists[li].items.push(r);
            self.selected = Some(id);
            self.save();
        }
    }

    fn key(&mut self, k: Key, m: gui::Mods) {
        if self.list_sheet.is_some() {
            self.list_key(k, m);
            return;
        }
        if self.edit.is_some() {
            self.edit_key(k, m);
            return;
        }
        if self.search_focus {
            match k {
                Key::Escape => {
                    self.search.set("");
                    self.search_focus = false;
                }
                Key::Enter | Key::Tab => self.search_focus = false,
                _ => {
                    self.search.key(k, m);
                }
            }
            return;
        }
        if self.quick_focus {
            match k {
                Key::Enter => self.quick_add(),
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
                'n' => self.quick_focus = true,
                'f' => self.search_focus = true,
                'l' => self.list_sheet = Some(ListSheet::new()),
                _ => {}
            }
            return;
        }
        match (k, self.selected) {
            (Key::Char(' '), Some(id)) => self.toggle_done(id),
            (Key::Delete | Key::Backspace, Some(id)) => self.delete(id),
            (Key::Enter, Some(id)) => self.begin_edit(id),
            (Key::Up | Key::Down, _) => self.move_selection(k == Key::Down),
            (Key::Escape, _) => self.selected = None,
            (Key::Char('n'), _) => self.quick_focus = true,
            _ => {}
        }
    }

    fn move_selection(&mut self, down: bool) {
        let ids: Vec<u32> = self
            .open_rows()
            .iter()
            .map(|(_, r)| r.id)
            .chain(self.done_rows().iter().map(|(_, r)| r.id))
            .collect();
        if ids.is_empty() {
            return;
        }
        let i = self.selected.and_then(|s| ids.iter().position(|&x| x == s));
        let n = match (i, down) {
            (None, _) => 0,
            (Some(i), true) => (i + 1).min(ids.len() - 1),
            (Some(i), false) => i.saturating_sub(1),
        };
        self.selected = Some(ids[n]);
    }

    /// Add what is in the quick field, read with the quick-add grammar.
    fn quick_add(&mut self) {
        let text = String::from(self.quick.text.trim());
        if text.is_empty() {
            return;
        }
        let q = timekit::quickadd::parse(&text, self.today);
        let mut r = Reminder::new(0, &q.title);
        r.due = q.due;
        r.time = q.time;
        r.repeat = q.repeat;
        r.every = q.every;
        // In Today, a reminder with no day is due today.
        if r.due.is_none() && self.view == View::Today {
            r.due = Some(self.today);
        }
        let list = self.current_list();
        let id = self.set.add(list, r);
        self.selected = Some(id);
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
        if self.list_sheet.is_some() {
            self.hits.modal();
            let ink = th.ink;
            for p in win.pixels() {
                *p = blend(*p, ink, 0x47);
            }
            self.draw_list_sheet(win);
        }
    }
}
