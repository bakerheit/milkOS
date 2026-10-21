//! Files: browses the real file system (`crate::fs`). SPEC sidebar (160 wide, oat, sand edge),
//! an icon grid and a status bar. Click selects, double-click opens (folders here, text files in
//! Notes), Backspace or the "up" chip goes to the parent folder, the wheel scrolls.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::Rect;
use crate::timer;

use super::super::event::Key;
use super::super::icons::{self, Icon};
use super::super::paint::{Painter, hit};
use super::super::rtc;
use super::super::theme::*;
use super::super::window::{App, KeyInput, PointerInput, PointerKind, WindowContents};
use super::super::{Request, request};
use super::fsx::{self, Entry};
use super::notes;

const FAVS: [(&str, &str); 6] = [
    ("Home", fsx::HOME),
    ("Documents", "/home/milk/Documents"),
    ("Downloads", "/home/milk/Downloads"),
    ("Desktop", "/home/milk/Desktop"),
    ("Pictures", "/home/milk/Pictures"),
    ("Music", "/home/milk/Music"),
];
const VOLUME: &str = "/Volumes/Milk";
const SNAPDIR: &str = "/Volumes/Milk/.snapshots";
/// The Time Machine view (a list of milkfs snapshots), not a real path.
const TM: &str = "timemachine:";

/// One milkfs snapshot, from /sys/fs/milkfs/snapshots ("name gen=G created=T").
struct Snap {
    name: String,
    generation: u64,
    created: u64,
}

fn snapshots() -> Vec<Snap> {
    let text = crate::fs::read_to_string("/sys/fs/milkfs/snapshots").unwrap_or_default();
    let mut v: Vec<Snap> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let name = it.next()?;
            let mut s = Snap {
                name: String::from(name),
                generation: 0,
                created: 0,
            };
            for kv in it {
                match kv.split_once('=') {
                    Some(("gen", g)) => s.generation = g.parse().unwrap_or(0),
                    Some(("created", c)) => s.created = c.parse().unwrap_or(0),
                    _ => {}
                }
            }
            (s.generation > 0 || s.created > 0).then_some(s)
        })
        .collect();
    v.sort_by_key(|s| core::cmp::Reverse(s.created));
    v
}

fn when(secs: u64) -> String {
    let t = rtc::from_epoch(secs);
    format!(
        "{} {} {}, {:02}:{:02}",
        &rtc::DAYS[t.weekday as usize][..3],
        t.day,
        &rtc::MONTHS[t.month as usize - 1][..3],
        t.hour,
        t.minute
    )
}

/// If `path` is inside a snapshot: (snapshot name, the matching live path).
fn in_snapshot(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix(SNAPDIR)?.strip_prefix('/')?;
    let (name, inner) = rest.split_once('/').unwrap_or((rest, ""));
    let live = if inner.is_empty() {
        String::from(VOLUME)
    } else {
        format!("{VOLUME}/{inner}")
    };
    Some((String::from(name), live))
}
const GRID_TOP: i32 = 20;
const CELL_W: i32 = 100;
const CELL_H: i32 = 108;

/// A folder someone asked Files to show (`open <dir>`); the Files window picks it up.
static PENDING: crate::sync::SpinLock<Option<String>> = crate::sync::SpinLock::new(None);

pub fn show_path(path: &str) {
    *PENDING.lock() = Some(String::from(path));
}

pub struct Files {
    path: String,
    title: String,
    items: Vec<Entry>,
    error: Option<String>,
    selected: Option<usize>,
    scroll: i32,
    /// Last click: (item, time) for double-click detection.
    last_click: Option<(usize, u64)>,
    cols: i32,
    rows_visible: i32,
    /// Body size at the last draw (for hit-testing the status bar chip).
    size: (i32, i32),
    last_sec: u64,
    /// The Time Machine view's rows.
    snaps: Vec<Snap>,
}

impl Files {
    pub fn new() -> Files {
        let mut f = Files {
            path: String::new(),
            title: String::new(),
            items: Vec::new(),
            error: None,
            selected: None,
            scroll: 0,
            last_click: None,
            cols: 4,
            rows_visible: 3,
            size: (0, 0),
            last_sec: 0,
            snaps: Vec::new(),
        };
        let start = PENDING.lock().take();
        f.go(start.as_deref().unwrap_or("/home/milk/Documents"));
        f
    }

    fn go(&mut self, path: &str) {
        self.path = String::from(path);
        self.title = match (path, in_snapshot(path)) {
            (TM, _) => String::from("Time Machine"),
            ("/", _) => String::from("milk"),
            (p, Some((snap, _))) => format!("{} ({snap})", fsx::basename(p)),
            (p, None) => String::from(fsx::basename(p)),
        };
        self.selected = None;
        self.scroll = 0;
        self.last_click = None;
        self.reload();
    }

    fn reload(&mut self) {
        if self.path == TM {
            self.snaps = snapshots();
            self.items.clear();
            self.error = None;
            return;
        }
        match fsx::list(&self.path) {
            Ok(v) => {
                self.items = v;
                self.error = None;
            }
            Err(e) => {
                self.items.clear();
                self.error = Some(format!("{e}"));
            }
        }
    }

    fn cell(&self, i: usize) -> (i32, i32) {
        let i = i as i32;
        (
            SIDEBAR_W + 24 + (i % self.cols) * CELL_W,
            GRID_TOP + (i / self.cols - self.scroll) * CELL_H,
        )
    }

    fn open(&mut self, i: usize) {
        let Some(e) = self.items.get(i) else {
            return;
        };
        let full = fsx::resolve(&self.path, &e.name);
        if fsx::is_dir(&full) {
            self.go(&full);
        } else if fsx::is_text(&e.name) {
            notes::open_file(&full);
            request(Request::OpenHere(App::Notes));
        } else {
            super::super::notify(&e.name, "No app opens this kind of file yet");
        }
    }

    fn side_rows() -> Vec<(i32, &'static str, &'static str)> {
        let mut v: Vec<(i32, &'static str, &'static str)> = FAVS
            .iter()
            .enumerate()
            .map(|(i, (n, p))| (44 + i as i32 * 28, *n, *p))
            .collect();
        let mut devs: Vec<(&'static str, &'static str)> = alloc::vec![("milk", "/")];
        if crate::fs::stat(VOLUME).is_ok() {
            devs.push(("Milk", VOLUME));
            devs.push(("Time Machine", TM));
        }
        devs.push(("proc", "/proc"));
        for (i, (n, p)) in devs.into_iter().enumerate() {
            v.push((250 + i as i32 * 28, n, p));
        }
        v
    }

    /// Copy the selected item of a snapshot back into the live tree.
    fn restore(&mut self) {
        let Some(e) = self.selected.and_then(|i| self.items.get(i)) else {
            return;
        };
        let Some((snap, live_dir)) = in_snapshot(&self.path) else {
            return;
        };
        let src = fsx::resolve(&self.path, &e.name);
        let dst = fsx::resolve(&live_dir, &e.name);
        let r = if fsx::is_dir(&src) {
            crate::fs::copy_tree(&src, &dst)
        } else {
            let _ = crate::fs::mkdir_all(&live_dir, 0o755);
            crate::fs::read_file(&src).and_then(|d| crate::fs::write_file(&dst, &d))
        };
        let _ = crate::fs::sync_all();
        match r {
            Ok(()) => {
                super::super::notify(&format!("Restored {}", e.name), &format!("from {snap}"))
            }
            Err(err) => super::super::notify("Restore failed", &format!("{}: {err}", e.name)),
        }
    }

    fn restore_chip(w: i32, h: i32) -> Rect {
        Rect::new(w - 56 - 84, h - STATUS_H + 3, 76, 18)
    }

    fn draw_snapshots(&self, p: &mut Painter, w: i32) {
        let x = SIDEBAR_W + 20;
        p.text_x(x, 14, "Time Machine", ESPRESSO, 2, true);
        p.text_p(
            x,
            50,
            "Snapshots of /Volumes/Milk. Open one to browse it, select a file, Restore.",
            COCOA,
            false,
        );
        if self.snaps.is_empty() {
            p.text_p(
                x,
                84,
                "No snapshots yet (Settings > Time Machine > Back up now).",
                LATTE,
                false,
            );
        }
        for (i, sn) in self.snaps.iter().enumerate() {
            let y = 76 + i as i32 * 44 - self.scroll * 44;
            if y < 70 {
                continue;
            }
            let r = Rect::new(x - 8, y, w - x - 12, 40);
            if self.selected == Some(i) {
                p.rrect_r(r, R_ROW, STRAW, SEL_ROW_A);
            }
            icons::draw(p, x, y + 4, 32, Icon::Clock, CREAM, 255, ESPRESSO, true);
            p.text_p(x + 44, y + 3, &sn.name, ESPRESSO, true);
            let sub = format!("{}   generation {}", when(sn.created), sn.generation);
            p.text_p(x + 44, y + 21, &sub, COCOA, false);
        }
    }
}

impl WindowContents for Files {
    fn title(&self) -> &str {
        &self.title
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "View", "Go", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.size = (w, h);
        self.cols = ((w - SIDEBAR_W - 24) / CELL_W).max(1);
        self.rows_visible = ((h - STATUS_H - GRID_TOP) / CELL_H).max(1);
        // Sidebar.
        p.fill(0, 0, SIDEBAR_W, h, OAT);
        p.fill(SIDEBAR_W - 1, 0, 1, h, SAND);
        p.text(16, 20, "FAVOURITES", COCOA);
        p.text(16, 228, "DEVICES", COCOA);
        for (y, name, path) in Self::side_rows() {
            if path == self.path {
                p.rrect(8, y, SIDEBAR_W - 16, 24, R_BUTTON, STRAW, SEL_SIDEBAR_A);
            }
            p.text(18, y + 4, name, ESPRESSO);
        }
        if self.path == TM {
            let mut gp = p.sub(Rect::new(0, 0, w, h - STATUS_H));
            self.draw_snapshots(&mut gp, w);
            let sy = h - STATUS_H;
            p.fill(0, sy, w, STATUS_H, CREAM);
            p.fill(0, sy, w, 1, SAND);
            let n = self.snaps.len();
            p.text(
                SIDEBAR_W + 12,
                sy + 4,
                &format!("{n} snapshot{}", if n == 1 { "" } else { "s" }),
                COCOA,
            );
            return;
        }
        // Grid.
        let mut gp = p.sub(Rect::new(0, 0, w, h - STATUS_H));
        if let Some(e) = &self.error {
            gp.text(SIDEBAR_W + 24, GRID_TOP, &format!("Can't open: {e}"), STRAW);
        } else if self.items.is_empty() {
            gp.text(SIDEBAR_W + 24, GRID_TOP, "This folder is empty", LATTE);
        }
        for (i, e) in self.items.iter().enumerate() {
            let (cx, cy) = self.cell(i);
            if cy + CELL_H < 0 || cy > h {
                continue;
            }
            if self.selected == Some(i) {
                gp.rrect(cx - 4, cy - 4, 100, 96, R_ROW, STRAW, SEL_ROW_A);
            }
            let icon = fsx::icon_for(e);
            let (bg, ba, fg) = if icon == Icon::Folder {
                (HONEY, FOLDER_A, COCOA)
            } else {
                (CREAM, 255, ESPRESSO)
            };
            icons::draw(
                &mut gp,
                cx + 12,
                cy,
                56,
                icon,
                bg,
                ba,
                fg,
                icon != Icon::Folder,
            );
            // Names longer than the cell get an ellipsis.
            let name: String = if e.name.chars().count() > 11 {
                let mut s: String = e.name.chars().take(10).collect();
                s.push('~');
                s
            } else {
                e.name.clone()
            };
            gp.text_center(cx - 6, 92, cy + 64, &name, ESPRESSO, false);
        }
        // Status bar: count (or the selection), the path, and an "up" chip.
        let sy = h - STATUS_H;
        p.fill(0, sy, w, STATUS_H, CREAM);
        p.fill(0, sy, w, 1, SAND);
        let info = match self.selected.and_then(|i| self.items.get(i)) {
            Some(e) => format!("{}, {}", e.name, fsx::human_size(e.size)),
            None => format!(
                "{} item{}",
                self.items.len(),
                if self.items.len() == 1 { "" } else { "s" }
            ),
        };
        p.text(SIDEBAR_W + 12, sy + 4, &info, COCOA);
        if let Some((snap, _)) = in_snapshot(&self.path) {
            if self.selected.is_some() {
                let r = Self::restore_chip(w, h);
                p.rrect_r(r, R_BUTTON, STRAW, 255);
                p.text_p_center(r.x, r.w, sy + 4, "Restore", MILK, true);
            } else {
                p.text_right(
                    w - 64,
                    sy + 4,
                    &format!("{snap}, read-only"),
                    LATTE,
                    1,
                    false,
                );
            }
        }
        if self.path != "/" {
            let up = Self::up_chip(w, h);
            p.rrect(up.x, up.y, up.w, up.h, R_BUTTON, OAT, 255);
            p.text_center(up.x, up.w, sy + 4, "up", COCOA, false);
        }
        if in_snapshot(&self.path).is_none() {
            p.text_right(w - 64, sy + 4, &fsx::pretty(&self.path), LATTE, 1, false);
        }
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let PointerKind::Down(_) = e.kind else {
            return false;
        };
        if e.x < SIDEBAR_W {
            for (y, _, path) in Self::side_rows() {
                if hit(&Rect::new(8, y, SIDEBAR_W - 16, 24), e.x, e.y) {
                    self.go(path);
                    return true;
                }
            }
            return false;
        }
        if self.path == TM {
            let x = SIDEBAR_W + 20;
            let row = (e.y - 76) / 44 + self.scroll;
            if e.y >= 76 && (row as usize) < self.snaps.len() && e.x >= x - 8 {
                let name = self.snaps[row as usize].name.clone();
                let root = format!("{SNAPDIR}/{name}");
                let home = format!("{root}/home/milk");
                self.go(if fsx::is_dir(&home) { &home } else { &root });
                return true;
            }
            return false;
        }
        if in_snapshot(&self.path).is_some()
            && self.selected.is_some()
            && hit(&Self::restore_chip(self.size.0, self.size.1), e.x, e.y)
        {
            self.restore();
            return true;
        }
        if self.path != "/" && hit(&Self::up_chip(self.size.0, self.size.1), e.x, e.y) {
            let up = fsx::parent(&self.path);
            self.go(&up);
            return true;
        }
        let sel = (0..self.items.len()).find(|&i| {
            let (cx, cy) = self.cell(i);
            hit(&Rect::new(cx - 4, cy - 4, 100, 96), e.x, e.y)
        });
        let now = timer::now_ns();
        if let Some(i) = sel
            && self
                .last_click
                .is_some_and(|(j, t)| j == i && now - t < 450_000_000)
        {
            self.last_click = None;
            self.open(i);
            return true;
        }
        self.last_click = sel.map(|i| (i, now));
        if sel != self.selected {
            self.selected = sel;
            return true;
        }
        false
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        match k.key {
            Key::Backspace | Key::Up if k.key == Key::Backspace || k.mods.meta => {
                let up = fsx::parent(&self.path);
                self.go(&up);
                true
            }
            Key::Enter => {
                if let Some(i) = self.selected {
                    self.open(i);
                }
                true
            }
            Key::Right | Key::Left | Key::Down | Key::Up => {
                let n = self.items.len();
                if n == 0 {
                    return false;
                }
                let c = self.cols as usize;
                let i = self.selected.unwrap_or(0);
                let j = match k.key {
                    Key::Right => (i + 1).min(n - 1),
                    Key::Left => i.saturating_sub(1),
                    Key::Down => (i + c).min(n - 1),
                    _ => i.saturating_sub(c),
                };
                self.selected = Some(j);
                // Keep it visible.
                let row = (j / c) as i32;
                if row < self.scroll {
                    self.scroll = row;
                } else if row >= self.scroll + self.rows_visible {
                    self.scroll = row - self.rows_visible + 1;
                }
                true
            }
            _ => false,
        }
    }

    fn scroll(&mut self, dy: i32) -> bool {
        let rows = (self.items.len() as i32 + self.cols - 1) / self.cols;
        let max = (rows - self.rows_visible).max(0);
        let s = (self.scroll + dy.signum()).clamp(0, max);
        if s != self.scroll {
            self.scroll = s;
            return true;
        }
        false
    }

    fn tick(&mut self, now: u64) -> bool {
        let pending = PENDING.lock().take();
        if let Some(p) = pending {
            self.go(&p);
            return true;
        }
        // Pick up changes made elsewhere (Terminal, Notes autosave) about once a second.
        let sec = now / 1_000_000_000;
        if sec == self.last_sec {
            return false;
        }
        self.last_sec = sec;
        if self.path == TM {
            let n = snapshots();
            if n.len() != self.snaps.len() {
                self.snaps = n;
                return true;
            }
            return false;
        }
        let Ok(new) = fsx::list(&self.path) else {
            return false;
        };
        let same = new.len() == self.items.len()
            && new
                .iter()
                .zip(&self.items)
                .all(|(a, b)| a.name == b.name && a.size == b.size);
        if same {
            return false;
        }
        self.items = new;
        if self.selected.is_some_and(|i| i >= self.items.len()) {
            self.selected = None;
        }
        true
    }
}

impl Files {
    fn up_chip(w: i32, h: i32) -> Rect {
        Rect::new(w - 56, h - STATUS_H + 3, 48, 18)
    }
}
