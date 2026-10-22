//! The powerbox (docs/security.md): a sandboxed process asks for a file, the wm shows its
//! own Open / Save panel, and only the file the user picks is granted to the process.
//!
//! Kernel <-> wm message: the `MILK_POWERBOX` syscall (proc/syscall.rs) calls [`ask`] on
//! the requesting thread. That queues a [`Req`] (pid, the process name, open or save, a
//! suggested name: nothing the process could use to steer the panel beyond a file name),
//! posts `Request::Powerbox` to the desktop and sleeps until there is an answer. The
//! desktop opens one panel at a time (`App::Powerbox`, [`Panel`]), which takes the oldest
//! request ([`take_next`]) and answers it ([`answer`]) with the chosen absolute path or
//! `None`. The syscall then adds the grant (`caps.with_fs_grant`), audits it, opens the file
//! and returns the fd. A signal to the requester withdraws the request ([`withdraw`]): its
//! panel closes by itself.
//!
//! The panel is drawn and driven by the wm only (the app can't draw on it, pre-fill a path
//! or click it), says which app is asking, and lists the real file system.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::fs::InodeType;
use crate::gfx::Rect;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::timer;

use super::apps::fsx;
use super::button::{self, Kind, State};
use super::event::Key;
use super::icons::{self, Icon};
use super::paint::{Painter, hit};
use super::theme::*;
use super::window::{
    KeyInput, PointerInput, PointerKind, WindowContents, hover_index, hover_move, paint_hover,
};
use super::{Request, request};

#[derive(Clone)]
pub struct Req {
    pub id: u64,
    pub pid: u32,
    /// The requesting process's name, as the kernel knows it.
    pub app: String,
    pub save: bool,
    pub suggested: String,
}

struct Broker {
    next: u64,
    queue: VecDeque<Req>,
    /// The request on screen, and whether its requester has given up.
    active: Option<(u64, bool)>,
    answers: Vec<(u64, Option<String>)>,
}

static BROKER: SpinLock<Broker> = SpinLock::new(Broker {
    next: 1,
    queue: VecDeque::new(),
    active: None,
    answers: Vec::new(),
});
static ANSWERED: WaitQueue = WaitQueue::new();
/// The desktop is running (else nobody would ever answer).
static UP: AtomicBool = AtomicBool::new(false);

pub fn set_up() {
    UP.store(true, Ordering::Release);
}

/// From the `MILK_POWERBOX` syscall: show the panel for process `pid` and wait.
/// Ok(Some(path)) = the user's choice, Ok(None) = cancelled (or no desktop), Err = a
/// signal interrupted the wait (the request is withdrawn).
pub fn ask(pid: u32, save: bool, suggested: &str) -> Result<Option<String>, ()> {
    if !UP.load(Ordering::Acquire) {
        return Ok(None);
    }
    let app = crate::proc::get(pid)
        .map(|p| String::from(p.inner.lock().name.as_str()))
        .unwrap_or_else(|| format!("pid {pid}"));
    let id = {
        let mut b = BROKER.lock();
        let id = b.next;
        b.next += 1;
        b.queue.push_back(Req {
            id,
            pid,
            app,
            save,
            suggested: String::from(suggested),
        });
        id
    };
    request(Request::Powerbox);
    loop {
        let got = {
            let mut b = BROKER.lock();
            let i = b.answers.iter().position(|a| a.0 == id);
            i.map(|i| b.answers.remove(i).1)
        };
        if let Some(path) = got {
            return Ok(path);
        }
        if crate::proc::signal::interrupted() {
            withdraw(id);
            return Err(());
        }
        ANSWERED.wait_until_timeout(
            || BROKER.lock().answers.iter().any(|a| a.0 == id),
            100_000_000,
        );
    }
}

/// The requester gave up (signal): drop it from the queue, or close its panel.
fn withdraw(id: u64) {
    let mut b = BROKER.lock();
    b.queue.retain(|r| r.id != id);
    if let Some((a, gone)) = b.active.as_mut()
        && *a == id
    {
        *gone = true;
    }
    b.answers.retain(|a| a.0 != id);
    drop(b);
    super::WAKE.wake_all();
}

/// The desktop: the next request to show (it becomes the active one).
fn take_next() -> Option<Req> {
    let mut b = BROKER.lock();
    if b.active.is_some() {
        return None;
    }
    let r = b.queue.pop_front()?;
    b.active = Some((r.id, false));
    Some(r)
}

/// The panel's verdict on request `id`.
fn answer(id: u64, path: Option<String>) {
    let more = {
        let mut b = BROKER.lock();
        let gone = b.active.is_some_and(|(a, g)| a == id && g);
        if b.active.is_some_and(|(a, _)| a == id) {
            b.active = None;
        }
        if !gone {
            b.answers.push((id, path));
        }
        !b.queue.is_empty()
    };
    ANSWERED.wake_all();
    if more {
        request(Request::Powerbox);
    }
}

fn withdrawn(id: u64) -> bool {
    BROKER
        .lock()
        .active
        .is_some_and(|(a, gone)| a == id && gone)
}

/// Is a request waiting for a panel?
pub fn pending() -> bool {
    let b = BROKER.lock();
    b.active.is_none() && !b.queue.is_empty()
}

// ---- the panel ---------------------------------------------------------------------------

const PLACES: [(&str, &str); 6] = [
    ("Home", "/Users/milk"),
    ("Documents", "/Users/milk/Documents"),
    ("Downloads", "/Users/milk/Downloads"),
    ("Desktop", "/Users/milk/Desktop"),
    ("Pictures", "/Users/milk/Pictures"),
    ("Music", "/Users/milk/Music"),
];
const SIDE_W: i32 = 150;
const HEAD_H: i32 = 64;
const ROW_H: i32 = 26;
const FOOT_H: i32 = 54;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Place(usize),
    Row(usize),
    Up,
    Cancel,
    Choose,
    Name,
}

pub struct Panel {
    req: Option<Req>,
    title: String,
    dir: String,
    items: Vec<fsx::Entry>,
    sel: Option<usize>,
    scroll: usize,
    rows_visible: usize,
    /// Save: the file name being typed.
    name: String,
    hits: Vec<(Rect, Hit)>,
    last_click: Option<(usize, u64)>,
    done: bool,
    hover: Option<(i32, i32)>,
}

impl Panel {
    pub fn new() -> Panel {
        let req = take_next();
        let title = match &req {
            Some(r) if r.save => format!("Save: {} wants to save a file", r.app),
            Some(r) => format!("Open: {} wants a file", r.app),
            None => String::from("Open"),
        };
        let mut p = Panel {
            name: req.as_ref().map_or(String::new(), |r| r.suggested.clone()),
            done: req.is_none(),
            req,
            title,
            dir: String::new(),
            items: Vec::new(),
            sel: None,
            scroll: 0,
            rows_visible: 8,
            hits: Vec::new(),
            last_click: None,
            hover: None,
        };
        p.go("/Users/milk/Documents");
        p
    }

    fn save(&self) -> bool {
        self.req.as_ref().is_some_and(|r| r.save)
    }

    fn go(&mut self, dir: &str) {
        let Ok(mut items) = fsx::list(dir) else {
            return;
        };
        items.retain(|e| !e.name.starts_with('.'));
        items.sort_by(|a, b| {
            (b.kind == InodeType::Dir)
                .cmp(&(a.kind == InodeType::Dir))
                .then(
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase()),
                )
        });
        self.dir = String::from(dir);
        self.items = items;
        self.sel = None;
        self.scroll = 0;
    }

    /// The path the primary button would hand over, if any.
    fn choice(&self) -> Option<String> {
        if self.save() {
            let n = self.name.trim();
            (!n.is_empty() && !n.contains('/')).then(|| fsx::resolve(&self.dir, n))
        } else {
            let e = self.items.get(self.sel?)?;
            (e.kind != InodeType::Dir).then(|| fsx::resolve(&self.dir, &e.name))
        }
    }

    fn finish(&mut self, path: Option<String>) {
        if self.done {
            return;
        }
        self.done = true;
        if let Some(r) = &self.req {
            answer(r.id, path);
        }
    }

    fn activate(&mut self, i: usize) {
        let Some(e) = self.items.get(i) else {
            return;
        };
        let full = fsx::resolve(&self.dir, &e.name);
        if e.kind == InodeType::Dir {
            self.go(&full);
        } else if self.save() {
            self.name = e.name.clone();
        } else {
            self.finish(Some(full));
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        // Closed with the red light (or the desktop going away): that's a cancel.
        self.finish(None);
    }
}

impl WindowContents for Panel {
    fn title(&self) -> &str {
        &self.title
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "Window", "Help"]
    }

    fn wants_close(&self) -> bool {
        self.done || self.req.as_ref().is_some_and(|r| withdrawn(r.id))
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.hits.clear();
        let save = self.save();
        let app = self.req.as_ref().map_or(String::from("An app"), |r| {
            format!("{} (pid {})", r.app, r.pid)
        });
        // Who is asking, and what they get: drawn by milkOS, never by the app.
        p.fill(0, 0, w, HEAD_H, CREAM);
        p.fill(0, HEAD_H - 1, w, 1, SAND);
        icons::draw(p, 16, 12, 40, Icon::Folder, HONEY, 255, INK, false);
        let line = if save {
            format!("{app} wants to save a file")
        } else {
            format!("{app} wants to open a file")
        };
        p.text_p(68, 14, &line, ESPRESSO, true);
        p.text_p(
            68,
            34,
            "Only the file you choose is shared with it. This panel is milkOS's, not the app's.",
            COCOA,
            false,
        );
        // Places.
        p.fill(0, HEAD_H, SIDE_W, h - HEAD_H, OAT);
        p.fill(SIDE_W - 1, HEAD_H, 1, h - HEAD_H, SAND);
        for (i, (name, path)) in PLACES.iter().enumerate() {
            let y = HEAD_H + 12 + i as i32 * 28;
            let r = Rect::new(8, y, SIDE_W - 16, 24);
            if self.dir == *path {
                p.rrect_r(r, R_BUTTON, STRAW, SEL_SIDEBAR_A);
            }
            p.text_p(18, y + 4, name, ESPRESSO, false);
            self.hits.push((r, Hit::Place(i)));
        }
        // Where we are, and up.
        let x0 = SIDE_W + 16;
        p.text_p(x0, HEAD_H + 10, &fsx::pretty(&self.dir), COCOA, true);
        if self.dir != "/" {
            let up = Rect::new(w - 16 - 44, HEAD_H + 8, 44, 22);
            p.rrect_r(up, R_BUTTON, OAT, 255);
            p.text_p_center(up.x, up.w, up.y + 3, "up", COCOA, false);
            self.hits.push((up, Hit::Up));
        }
        // The folder's contents.
        let ly = HEAD_H + 40;
        let list_h = h - ly - FOOT_H - 4;
        self.rows_visible = (list_h / ROW_H).max(1) as usize;
        if self.items.is_empty() {
            p.text_p(x0, ly + 4, "This folder is empty", COCOA, false);
        }
        for (k, (i, e)) in self
            .items
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(self.rows_visible)
            .enumerate()
        {
            let y = ly + k as i32 * ROW_H;
            let r = Rect::new(x0 - 8, y, w - x0 - 8, ROW_H - 2);
            if self.sel == Some(i) {
                p.rrect_r(r, R_ROW, STRAW, SEL_ROW_A);
            }
            let folder = e.kind == InodeType::Dir;
            let icon = fsx::icon_for(e);
            let (bg, fg) = if folder {
                (HONEY, INK)
            } else {
                (CREAM, ESPRESSO)
            };
            if !icons::file_tile(p, x0, y + 1, 22, fsx::art_kind(e)) {
                icons::draw(p, x0, y + 1, 22, icon, bg, 255, fg, !folder);
            }
            let size = if folder {
                String::new()
            } else {
                fsx::human_size(e.size)
            };
            let room = w - 24 - crate::sysfont::width(&size, false) - 12 - (x0 + 32);
            let name = crate::sysfont::Font::body(false).truncate(&e.name, room);
            p.text_p(x0 + 32, y + 4, &name, ESPRESSO, false);
            if !folder {
                let c = if self.sel == Some(i) { ESPRESSO } else { COCOA };
                p.text_right(w - 24, y + 4, &size, c, 1, false);
            }
            self.hits.push((r, Hit::Row(i)));
        }
        // Footer: the name (save), Cancel, Open / Save.
        let fy = h - FOOT_H;
        p.fill(SIDE_W, fy, w - SIDE_W, 1, SAND);
        if save {
            p.text_p(x0, fy + 17, "Save as", COCOA, false);
            let nr = Rect::new(x0 + 64, fy + 12, w - x0 - 64 - 16 - 2 * 100 - 16, 28);
            p.rrect_r(nr, R_BUTTON, MILK, 255);
            p.rrect_stroke(nr.x, nr.y, nr.w, nr.h, R_BUTTON, 1, HONEY, 255);
            let end = p.text_p(nr.x + 10, nr.y + 6, &self.name, ESPRESSO, false);
            p.fill(end + 1, nr.y + 6, 2, 16, ESPRESSO);
            self.hits.push((nr, Hit::Name));
        }
        let ok = self.choice().is_some();
        // Dialog buttons (SPEC Buttons): 28 high, the primary on the far right.
        let cancel = Rect::new(w - 16 - 2 * 100 - 8, fy + 13, 100, button::DIALOG_H);
        let choose = Rect::new(w - 16 - 100, fy + 13, 100, button::DIALOG_H);
        let on = |r: &Rect| self.hover.is_some_and(|(x, y)| hit(r, x, y));
        let (hc, hk) = (on(&cancel), ok && on(&choose));
        button::paint(p, cancel, "Cancel", Kind::Secondary, State::hover(hc));
        let kind = if ok { Kind::Primary } else { Kind::Disabled };
        let label = if save { "Save" } else { "Open" };
        button::paint(p, choose, label, kind, State::hover(hk));
        self.hits.push((cancel, Hit::Cancel));
        if ok {
            self.hits.push((choose, Hit::Choose));
        }
        if let Some(i) = hover_index(&self.hits, self.hover) {
            let (r, h) = self.hits[i];
            let row = matches!(h, Hit::Row(_) | Hit::Place(_));
            let btn = matches!(h, Hit::Cancel | Hit::Choose);
            if !btn && h != Hit::Name && self.sel.is_none_or(|s| h != Hit::Row(s)) {
                paint_hover(p, r, row);
            }
        }
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        hover_move(&self.hits, &mut self.hover, x, y)
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let PointerKind::Down(_) = e.kind else {
            return false;
        };
        let Some(h) = self
            .hits
            .iter()
            .find(|(r, _)| hit(r, e.x, e.y))
            .map(|x| x.1)
        else {
            return false;
        };
        match h {
            Hit::Place(i) => self.go(PLACES[i].1),
            Hit::Up => {
                let up = fsx::parent(&self.dir);
                self.go(&up);
            }
            Hit::Row(i) => {
                let now = timer::now_ns();
                let double = self
                    .last_click
                    .is_some_and(|(j, t)| j == i && now - t < 450_000_000);
                self.last_click = Some((i, now));
                self.sel = Some(i);
                if double {
                    self.last_click = None;
                    self.activate(i);
                } else if self.save()
                    && let Some(e) = self.items.get(i)
                    && e.kind != InodeType::Dir
                {
                    self.name = e.name.clone();
                }
            }
            Hit::Cancel => self.finish(None),
            Hit::Choose => {
                let c = self.choice();
                self.finish(c);
            }
            Hit::Name => {}
        }
        true
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        match k.key {
            Key::Escape => self.finish(None),
            Key::Enter => {
                if let Some(i) = self.sel
                    && self.items.get(i).is_some_and(|e| e.kind == InodeType::Dir)
                {
                    self.activate(i);
                } else if let Some(c) = self.choice() {
                    self.finish(Some(c));
                }
            }
            Key::Up => self.sel = Some(self.sel.map_or(0, |s| s.saturating_sub(1))),
            Key::Down => {
                let n = self.items.len();
                if n > 0 {
                    self.sel = Some(self.sel.map_or(0, |s| (s + 1).min(n - 1)));
                }
            }
            Key::Backspace if self.save() => {
                self.name.pop();
            }
            Key::Backspace => {
                let up = fsx::parent(&self.dir);
                self.go(&up);
            }
            _ => match k.ch {
                Some(c) if self.save() && !c.is_control() && c != '/' && self.name.len() < 120 => {
                    self.name.push(c)
                }
                _ => return false,
            },
        }
        if let Some(s) = self.sel {
            if s < self.scroll {
                self.scroll = s;
            } else if s >= self.scroll + self.rows_visible {
                self.scroll = s + 1 - self.rows_visible;
            }
        }
        true
    }

    fn scroll(&mut self, dy: i32) -> bool {
        let max = self.items.len().saturating_sub(self.rows_visible);
        let old = self.scroll;
        self.scroll = (self.scroll as i32 + dy).clamp(0, max as i32) as usize;
        old != self.scroll
    }
}
