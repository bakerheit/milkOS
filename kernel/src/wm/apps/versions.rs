//! Browse All Versions: one file's history across the milkfs snapshots
//! (`fs::milkfs::versions`, docs/filesystem.md "Versions"). A timeline on the left, newest
//! first; on the right the chosen version and the current file side by side, lines that
//! differ tinted. "Restore this version" calls `restore_version`, which keeps the current
//! content in a `pre-restore-<time>` snapshot first when no snapshot has it, so a restore
//! never loses anything (and that version shows up in the timeline).
//!
//! Opened from Notes (File > Browse Versions...) and Files (right-click a file, or File >
//! Browse Versions... with a file selected), via [`show`].

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{self, milkfs};
use crate::gfx::{Color, Rect};
use crate::sync::SpinLock;

use super::super::button::{self, Kind, State};
use super::super::event::Key;
use super::super::paint::{Painter, hit};
use super::super::theme::*;
use super::super::window::{
    App, KeyInput, PointerInput, PointerKind, WindowContents, hover_index, hover_move, paint_hover,
};
use super::super::{Request, notify_as, request, rtc};
use super::{fsx, notes};

/// The file someone asked to see the versions of; the window picks it up.
static PENDING: SpinLock<Option<String>> = SpinLock::new(None);

/// Show the versions of `path` (opens the window, or retargets it).
pub fn show(path: &str) {
    *PENDING.lock() = Some(String::from(path));
    request(Request::OpenHere(App::Versions));
}

const LIST_W: i32 = 236;
const ROW_H: i32 = 52;
const BAR_H: i32 = 44;
const LINE_H: i32 = 18;
/// Previews read at most this much of a version.
const PREVIEW_MAX: usize = 64 * 1024;

enum Preview {
    Text(Vec<String>),
    Binary(u64),
    Missing(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Row(usize),
    Restore,
}

pub struct Versions {
    path: String,
    title: String,
    /// Oldest first, current last (as milkfs gives them).
    list: Vec<milkfs::Version>,
    error: Option<String>,
    /// Index into `list`.
    sel: usize,
    chosen: Preview,
    current: Preview,
    hits: Vec<(Rect, Hit)>,
    /// First timeline row shown (rows are newest first).
    scroll: usize,
    rows_visible: usize,
    hover: Option<(i32, i32)>,
}

/// Where a version's content can be read: `<volume>/.snapshots/<snapshot><path in volume>`.
/// The home folder and /Apps are binds of the same paths on the volume. A snapshot taken
/// before the folder layout changed has the home at `home/<user>` (fs::layout::migrate).
pub fn snapshot_path(path: &str, snapshot: &str) -> Option<String> {
    let vol = milkfs::mount_path()?;
    let rel = path.strip_prefix(vol.as_str()).unwrap_or(path);
    let p = format!("{vol}/.snapshots/{snapshot}{rel}");
    if fs::lstat(&p).is_err()
        && let Some(rest) = rel.strip_prefix("/Users/")
    {
        let old = format!("{vol}/.snapshots/{snapshot}/home/{rest}");
        if fs::lstat(&old).is_ok() {
            return Some(old);
        }
    }
    Some(p)
}

fn preview(path: &str) -> Preview {
    let Ok(st) = fs::stat(path) else {
        return Preview::Missing(String::from(
            "Not at this path in that snapshot (renamed since?)",
        ));
    };
    let Ok(mut data) = fs::read_file(path) else {
        return Preview::Missing(String::from("Can't read it"));
    };
    data.truncate(PREVIEW_MAX);
    match core::str::from_utf8(&data) {
        Ok(t) if !t.contains('\0') => Preview::Text(t.lines().map(String::from).collect()),
        _ => Preview::Binary(st.size),
    }
}

/// "Wed 21 Oct, 05:04:09" in local time.
fn when(unix: u64) -> String {
    let t = rtc::from_epoch(unix);
    format!(
        "{} {} {}, {:02}:{:02}:{:02}",
        &rtc::DAYS[t.weekday as usize][..3],
        t.day,
        &rtc::MONTHS[t.month as usize - 1][..3],
        t.hour,
        t.minute,
        t.second
    )
}

impl Versions {
    pub fn new() -> Versions {
        let mut v = Versions {
            path: String::new(),
            title: String::from("Versions"),
            list: Vec::new(),
            error: None,
            sel: 0,
            chosen: Preview::Text(Vec::new()),
            current: Preview::Text(Vec::new()),
            hits: Vec::new(),
            scroll: 0,
            rows_visible: 6,
            hover: None,
        };
        if let Some(p) = PENDING.lock().take() {
            v.load(&p);
        }
        v
    }

    fn load(&mut self, path: &str) {
        self.path = String::from(path);
        self.title = format!("Versions of {}", fsx::basename(path));
        self.scroll = 0;
        match milkfs::versions(path) {
            Ok(list) => {
                self.error = None;
                // Start on the newest older version: that's what you came for.
                self.sel = list.len().saturating_sub(2);
                self.list = list;
            }
            Err(e) => {
                self.list.clear();
                self.sel = 0;
                self.error = Some(match e {
                    crate::fs::errno::Errno::EXDEV | crate::fs::errno::Errno::ENODEV => {
                        String::from("Versions live on the milkfs disk; this file isn't on it.")
                    }
                    crate::fs::errno::Errno::EISDIR => {
                        String::from("That's a folder; pick a file to see its versions.")
                    }
                    e => format!("No versions: {e}"),
                });
            }
        }
        self.current = preview(path);
        self.load_chosen();
    }

    fn load_chosen(&mut self) {
        self.chosen = match self.list.get(self.sel).and_then(|v| v.snapshot.as_deref()) {
            Some(s) => match snapshot_path(&self.path, s) {
                Some(p) => preview(&p),
                None => Preview::Missing(String::from("No milkfs volume")),
            },
            None => preview(&self.path),
        };
    }

    fn select(&mut self, i: usize) {
        if i < self.list.len() && i != self.sel {
            self.sel = i;
            self.load_chosen();
        }
    }

    fn restore(&mut self) {
        let Some(snap) = self.list.get(self.sel).and_then(|v| v.snapshot.clone()) else {
            return;
        };
        let name = String::from(fsx::basename(&self.path));
        match milkfs::restore_version(&self.path, &snap) {
            Ok(()) => {
                let _ = fs::sync_all();
                // Notes shows the file: give it the restored text.
                if notes::current_path() == self.path {
                    notes::open_file(&self.path);
                }
                let taken = self.list[self.sel].taken;
                notify_as(
                    "Versions",
                    &format!("Restored {name}"),
                    &format!("to the version of {}", when(taken)),
                );
                let path = self.path.clone();
                self.load(&path);
                self.sel = self.list.len().saturating_sub(1);
                self.load_chosen();
            }
            Err(e) => notify_as("Versions", "Restore failed", &format!("{name}: {e}")),
        }
    }

    fn draw_pane(
        p: &mut Painter,
        r: Rect,
        label: &str,
        pv: &Preview,
        other: &Preview,
        tint: Color,
    ) {
        p.text_p(r.x, r.y, label, COCOA, true);
        let body = Rect::new(r.x, r.y + 24, r.w, r.h - 24);
        p.rrect_r(body, 10, MILK, 255);
        p.rrect_stroke(body.x, body.y, body.w, body.h, 10, 1, SAND, 255);
        let mut bp = p.sub(Rect::new(body.x + 10, body.y + 8, body.w - 20, body.h - 16));
        match pv {
            Preview::Text(lines) => {
                let others: BTreeSet<&str> = match other {
                    Preview::Text(o) => o.iter().map(String::as_str).collect(),
                    _ => BTreeSet::new(),
                };
                let cols = ((body.w - 20) / 8).max(1) as usize;
                for (k, l) in lines.iter().enumerate() {
                    let y = k as i32 * LINE_H;
                    if y > body.h {
                        break;
                    }
                    if !others.contains(l.as_str()) {
                        bp.fill_a(-6, y - 1, body.w - 8, LINE_H, tint, 0x50);
                    }
                    // File contents: a monospace grid, like Notes.
                    let s: String = l.chars().take(cols).collect();
                    bp.mono(0, y, &s, ESPRESSO);
                }
                if lines.is_empty() {
                    bp.text_p(0, 0, "(empty)", COCOA, false);
                }
            }
            Preview::Binary(n) => {
                bp.text_p(
                    0,
                    0,
                    &format!("{} (not text)", fsx::human_size(*n)),
                    COCOA,
                    false,
                );
            }
            Preview::Missing(why) => {
                bp.text_p(0, 0, why, COCOA, false);
            }
        }
    }
}

impl WindowContents for Versions {
    fn title(&self) -> &str {
        &self.title
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.hits.clear();
        // Timeline, newest first.
        p.fill(0, 0, LIST_W, h, OAT);
        p.fill(LIST_W - 1, 0, 1, h, SAND);
        p.text(16, 16, "VERSIONS", COCOA);
        let n = self.list.len();
        self.rows_visible = ((h - 44 - BAR_H) / ROW_H).max(1) as usize;
        let max_scroll = n.saturating_sub(self.rows_visible);
        self.scroll = self.scroll.min(max_scroll);
        // Keep the selection visible.
        let pos = n.saturating_sub(1 + self.sel);
        if pos < self.scroll {
            self.scroll = pos;
        } else if pos >= self.scroll + self.rows_visible {
            self.scroll = pos + 1 - self.rows_visible;
        }
        for (row, i) in (0..n)
            .rev()
            .enumerate()
            .skip(self.scroll)
            .take(self.rows_visible)
        {
            let v = &self.list[i];
            let y = 40 + (row - self.scroll) as i32 * ROW_H;
            let r = Rect::new(8, y, LIST_W - 16, ROW_H - 4);
            if i == self.sel {
                p.rrect_r(r, R_ROW, STRAW, SEL_SIDEBAR_A);
            }
            // The timeline: a line through dots.
            p.fill(22, y - 4, 2, ROW_H, SAND);
            let dot = if v.snapshot.is_none() { HONEY } else { COCOA };
            p.circle(17, y + 10, 12, dot, 255);
            let title = if v.snapshot.is_none() {
                String::from("Now")
            } else {
                when(v.taken)
            };
            p.text_p(36, y + 3, &title, ESPRESSO, i == self.sel);
            let sub = match &v.snapshot {
                Some(s) => format!("{}  {}", fsx::human_size(v.size), s),
                None => format!("{}  current file", fsx::human_size(v.size)),
            };
            let sub = crate::sysfont::Font::body(false).truncate(&sub, LIST_W - 48);
            let c = if i == self.sel { ESPRESSO } else { COCOA };
            p.text_p(36, y + 22, &sub, c, false);
            self.hits.push((r, Hit::Row(i)));
        }
        let x0 = LIST_W + 16;
        if let Some(e) = &self.error {
            p.text_p(x0, 20, &fsx::pretty(&self.path), ESPRESSO, true);
            p.text_p(x0, 44, e, COCOA, false);
            return;
        }
        // Side by side: the chosen version and now.
        let pw = (w - x0 - 16 - 12) / 2;
        let ph = h - 16 - BAR_H - 8;
        let chosen_label = match self.list.get(self.sel) {
            Some(v) if v.snapshot.is_some() => when(v.taken),
            _ => String::from("Current file"),
        };
        Self::draw_pane(
            p,
            Rect::new(x0, 14, pw, ph),
            &chosen_label,
            &self.chosen,
            &self.current,
            STRAW,
        );
        Self::draw_pane(
            p,
            Rect::new(x0 + pw + 12, 14, pw, ph),
            "Current file",
            &self.current,
            &self.chosen,
            MATCHA,
        );
        // Bottom bar.
        let by = h - BAR_H;
        p.fill(LIST_W, by, w - LIST_W, 1, SAND);
        p.text_p(
            x0,
            by + 14,
            &format!(
                "{} version{} of {}",
                n,
                if n == 1 { "" } else { "s" },
                fsx::pretty(&self.path)
            ),
            COCOA,
            false,
        );
        let can = self
            .list
            .get(self.sel)
            .is_some_and(|v| v.snapshot.is_some());
        let label = "Restore this version";
        let bw = button::dialog_w(label);
        let br = Rect::new(w - 16 - bw, by + 8, bw, button::DIALOG_H);
        let kind = if can { Kind::Primary } else { Kind::Disabled };
        let hov = can && self.hover.is_some_and(|(x, y)| hit(&br, x, y));
        button::paint(p, br, label, kind, State::hover(hov));
        if can {
            self.hits.push((br, Hit::Restore));
        }
        if let Some(i) = hover_index(&self.hits, self.hover) {
            let (r, h) = self.hits[i];
            if h != Hit::Row(self.sel) && h != Hit::Restore {
                paint_hover(p, r, matches!(h, Hit::Row(_)));
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
        match self
            .hits
            .iter()
            .find(|(r, _)| hit(r, e.x, e.y))
            .map(|x| x.1)
        {
            Some(Hit::Row(i)) => self.select(i),
            Some(Hit::Restore) => self.restore(),
            None => return false,
        }
        true
    }

    fn scroll(&mut self, dy: i32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll as i32 + dy).max(0) as usize;
        old != self.scroll
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        match k.key {
            // Up = newer, Down = older (the list is newest first).
            Key::Up => self.select(self.sel + 1),
            Key::Down if self.sel > 0 => self.select(self.sel - 1),
            _ => return false,
        }
        true
    }

    fn drop_path(&mut self, path: &str) -> bool {
        self.load(path);
        true
    }

    fn tick(&mut self, _now: u64) -> bool {
        let asked = PENDING.lock().take();
        if let Some(p) = asked {
            self.load(&p);
            return true;
        }
        false
    }
}
