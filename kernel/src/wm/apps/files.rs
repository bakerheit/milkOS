//! Files: browses the real file system (`crate::fs`). SPEC "Files window": a 40px toolbar
//! (back / forward, a path bar whose parts are clickable, search, the Time Machine clock and
//! a grid / list switch), a 168px sidebar (FAVOURITES, DEVICES, SHARED) and a status bar that
//! says what is selected and how much room is left. Click selects, double-click opens
//! (folders here, text files in Notes, WAVs in Music), Backspace goes to the parent folder,
//! the wheel scrolls.
//!
//! Share (the toolbar pill, File > Share..., or dropping a file on a NEARBY device) opens
//! the Milk Drop sheet over the body (apps/milkdrop.rs). NEARBY lists the machines the
//! `drop daemon` has heard (/tmp/drop/peers, no waiting).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::Rect;
use crate::sysfont::Font;
use crate::timer;

use super::super::button::{self, Kind, State};
use super::super::event::Key;
use super::super::icons::{self, Icon};
use super::super::paint::{Painter, hit};
use super::super::rtc;
use super::super::theme::*;
use super::super::window::{App, KeyInput, PointerInput, PointerKind, WindowContents};
use super::super::{Request, request};
use super::fsx::{self, Entry};
use super::milkdrop;
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
const MAC: &str = "/Volumes/Mac";
const SNAPDIR: &str = "/Volumes/Milk/.snapshots";
/// The Time Machine view (a list of milkfs snapshots), not a real path.
const TM: &str = "timemachine:";

/// SPEC Files window.
pub const TOOL_H: i32 = 40;
pub const SIDE_W: i32 = 168;
const SIDE_ROW: i32 = 26;
const SEARCH_W: i32 = 150;
const GRID_TOP: i32 = TOOL_H + 16;
const CELL_W: i32 = 100;
const CELL_H: i32 = 108;
const LIST_TOP: i32 = TOOL_H + 8;
const LIST_ROW: i32 = 28;

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

/// Free space on the Milk disk (milkfs: "blocks: N total, M free (4 KiB)"), for paths that
/// live on it. None elsewhere (tmpfs, proc, FAT, the Mac folder).
fn available(path: &str) -> Option<u64> {
    let on_milk = path.starts_with(fsx::HOME) || path.starts_with(VOLUME);
    if !on_milk {
        return None;
    }
    let info = crate::fs::read_to_string("/sys/fs/milkfs/info").ok()?;
    let line = info.lines().find(|l| l.starts_with("blocks:"))?;
    let free: u64 = line
        .split(',')
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(free * 4096)
}

/// "212 GB", "1.0 MB": the status bar's sizes (one decimal under 10).
fn size_label(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{} {}", v as u64, UNITS[u])
    }
}

/// A folder's entries as Files shows them: dot files (.config, .cache) hidden.
fn listing(path: &str) -> crate::fs::Result<Vec<Entry>> {
    let mut v = fsx::list(path)?;
    v.retain(|e| !e.name.starts_with('.'));
    Ok(v)
}

/// A folder someone asked Files to show (`open <dir>`); the Files window picks it up.
static PENDING: crate::sync::SpinLock<Option<String>> = crate::sync::SpinLock::new(None);

pub fn show_path(path: &str) {
    *PENDING.lock() = Some(String::from(path));
}

/// Toolbar controls.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Tool {
    Back,
    Forward,
    /// A part of the path bar: go there.
    Crumb(String),
    Search,
    Share,
    Clock,
    View,
}

pub struct Files {
    path: String,
    title: String,
    /// Everything in the folder, and what the search leaves of it (shown).
    all: Vec<Entry>,
    items: Vec<Entry>,
    error: Option<String>,
    selected: Option<usize>,
    scroll: i32,
    /// Last click: (item, time) for double-click detection.
    last_click: Option<(usize, u64)>,
    cols: i32,
    rows_visible: i32,
    /// Body size at the last draw (for hit-testing).
    size: (i32, i32),
    last_sec: u64,
    /// The Time Machine view's rows.
    snaps: Vec<Snap>,
    /// Right-click menu: where, and for which item.
    ctx: Option<(i32, i32, usize)>,
    /// History for back / forward.
    back: Vec<String>,
    fwd: Vec<String>,
    /// The search text, and whether keys go to it.
    search: String,
    searching: bool,
    /// List view instead of the icon grid.
    list: bool,
    /// Toolbar hit areas (from the last draw) and the pointer, for hover.
    tools: Vec<(Rect, Tool)>,
    hover: Option<(i32, i32)>,
    /// Free bytes where we are (Milk disk only).
    avail: Option<u64>,
    /// Milk Drop devices the daemon has heard: (name, receiving).
    nearby: Vec<(String, bool)>,
    /// The Milk Drop sheet, while sharing.
    share: Option<milkdrop::Sheet>,
    now: u64,
}

/// What the `drop daemon` heard on the link ("name ip fp recv seen" lines).
fn nearby() -> Vec<(String, bool)> {
    let text = crate::fs::read_to_string("/tmp/drop/peers").unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 4).then(|| (String::from(f[0]), f[3] == "1"))
        })
        .collect()
}

const DROP_PREFIX: &str = "drop:";

/// The right-click menu's rows.
const CTX: [&str; 2] = ["Open", "Browse All Versions..."];
const CTX_W: i32 = 210;
const CTX_ROW: i32 = 26;

/// What a sidebar row goes to.
struct SideRow {
    y: i32,
    label: String,
    path: String,
}

impl Files {
    pub fn new() -> Files {
        let mut f = Files {
            path: String::new(),
            title: String::new(),
            all: Vec::new(),
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
            ctx: None,
            back: Vec::new(),
            fwd: Vec::new(),
            search: String::new(),
            searching: false,
            list: false,
            tools: Vec::new(),
            hover: None,
            avail: None,
            nearby: nearby(),
            share: None,
            now: 0,
        };
        let start = PENDING.lock().take();
        f.show(start.as_deref().unwrap_or("/home/milk/Documents"));
        f
    }

    /// Go somewhere new (remembered for Back).
    fn go(&mut self, path: &str) {
        if path == self.path {
            return;
        }
        if !self.path.is_empty() {
            self.back.push(self.path.clone());
        }
        self.fwd.clear();
        self.show(path);
    }

    fn go_back(&mut self) {
        if let Some(p) = self.back.pop() {
            self.fwd.push(self.path.clone());
            self.show(&p);
        }
    }

    fn go_forward(&mut self) {
        if let Some(p) = self.fwd.pop() {
            self.back.push(self.path.clone());
            self.show(&p);
        }
    }

    fn show(&mut self, path: &str) {
        self.path = String::from(path);
        self.title = match (path, in_snapshot(path)) {
            (TM, _) => String::from("Time Machine"),
            ("/", _) => String::from("This machine"),
            (VOLUME, _) => String::from("Milk disk"),
            (p, Some((snap, _))) => format!("{} ({snap})", fsx::basename(p)),
            (p, None) => String::from(fsx::basename(p)),
        };
        self.selected = None;
        self.scroll = 0;
        self.last_click = None;
        self.search.clear();
        self.searching = false;
        self.avail = available(path);
        self.reload();
    }

    fn reload(&mut self) {
        if self.path == TM {
            self.snaps = snapshots();
            self.all.clear();
            self.items.clear();
            self.error = None;
            return;
        }
        match listing(&self.path) {
            Ok(v) => {
                self.all = v;
                self.error = None;
            }
            Err(e) => {
                self.all.clear();
                self.error = Some(format!("{e}"));
            }
        }
        self.filter();
    }

    /// Apply the search to the listing.
    fn filter(&mut self) {
        let q = self.search.to_ascii_lowercase();
        self.items = self
            .all
            .iter()
            .filter(|e| q.is_empty() || e.name.to_ascii_lowercase().contains(&q))
            .map(|e| Entry {
                name: e.name.clone(),
                kind: e.kind,
                size: e.size,
            })
            .collect();
        if self.selected.is_some_and(|i| i >= self.items.len()) {
            self.selected = None;
        }
    }

    /// Item `i`'s hit rect (grid cell or list row), in body coordinates.
    fn item_rect(&self, i: usize) -> Rect {
        let i = i as i32;
        if self.list {
            let y = LIST_TOP + (i - self.scroll) * LIST_ROW;
            Rect::new(SIDE_W + 8, y, self.size.0 - SIDE_W - 16, LIST_ROW - 2)
        } else {
            let (cx, cy) = self.cell(i as usize);
            Rect::new(cx - 4, cy - 4, 100, 96)
        }
    }

    fn cell(&self, i: usize) -> (i32, i32) {
        let i = i as i32;
        (
            SIDE_W + 20 + (i % self.cols) * CELL_W,
            GRID_TOP + (i / self.cols - self.scroll) * CELL_H,
        )
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        if x < SIDE_W || y < TOOL_H || y >= self.size.1 - STATUS_H {
            return None;
        }
        (0..self.items.len()).find(|&i| hit(&self.item_rect(i), x, y))
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
        } else if e.name.to_ascii_lowercase().ends_with(".wav") {
            super::music::play_path(&full);
            request(Request::OpenHere(App::Music));
        } else {
            super::super::notify_as("Files", &e.name, "No app opens this kind of file yet");
        }
    }

    /// The sidebar: FAVOURITES, NEARBY (Milk Drop devices; a row's path is "drop:<name>"),
    /// DEVICES (this machine, the Milk disk, FAT volumes) and SHARED (the Mac folder).
    /// Returns the group labels too: (y, label).
    fn side_rows(&self) -> (Vec<SideRow>, Vec<(i32, &'static str)>) {
        let mut rows = Vec::new();
        let mut labels = Vec::new();
        let mut y = TOOL_H + 12;
        let group = |rows: &mut Vec<SideRow>,
                     labels: &mut Vec<(i32, &'static str)>,
                     y: &mut i32,
                     name: &'static str,
                     items: Vec<(String, String)>| {
            if items.is_empty() {
                return;
            }
            labels.push((*y, name));
            *y += 22;
            for (label, path) in items {
                rows.push(SideRow { y: *y, label, path });
                *y += SIDE_ROW;
            }
            *y += 8;
        };
        let favs = FAVS
            .iter()
            .map(|(n, p)| (String::from(*n), String::from(*p)))
            .collect();
        group(&mut rows, &mut labels, &mut y, "FAVOURITES", favs);
        let near = self
            .nearby
            .iter()
            .map(|(n, _)| (n.clone(), format!("{DROP_PREFIX}{n}")))
            .collect();
        group(&mut rows, &mut labels, &mut y, "NEARBY", near);
        // "milk" and "Milk" read like a typo: say what each one is.
        let mut devs = alloc::vec![(String::from("This machine"), String::from("/"))];
        if crate::fs::stat(VOLUME).is_ok() {
            devs.push((String::from("Milk disk"), String::from(VOLUME)));
        }
        // Other volumes (FAT disks) under /Volumes.
        if let Ok(v) = fsx::list("/Volumes") {
            for e in v {
                let p = format!("/Volumes/{}", e.name);
                if p != VOLUME && p != MAC && !e.name.contains("-try") {
                    devs.push((e.name, p));
                }
            }
        }
        group(&mut rows, &mut labels, &mut y, "DEVICES", devs);
        let mut shared = Vec::new();
        if crate::fs::stat(MAC).is_ok() {
            shared.push((String::from("Mac"), String::from(MAC)));
        }
        group(&mut rows, &mut labels, &mut y, "SHARED", shared);
        (rows, labels)
    }

    /// The path bar's parts: (label, path). Home-relative under the home folder.
    fn crumbs(&self) -> Vec<(String, String)> {
        if self.path == TM {
            return alloc::vec![(String::from("Time Machine"), String::from(TM))];
        }
        let mut v = Vec::new();
        let (root_label, root, rest) = if let Some(r) = self.path.strip_prefix(fsx::HOME) {
            ("Home", String::from(fsx::HOME), r)
        } else if let Some((snap, _)) = in_snapshot(&self.path) {
            v.push((String::from("Time Machine"), String::from(TM)));
            let root = format!("{SNAPDIR}/{snap}");
            let rest = &self.path[root.len().min(self.path.len())..];
            // The snapshot's own name stands for its root.
            v.push((snap.clone(), root.clone()));
            let mut cur = root;
            for part in rest.split('/').filter(|s| !s.is_empty()) {
                cur = format!("{cur}/{part}");
                v.push((String::from(part), cur.clone()));
            }
            return v;
        } else {
            ("This machine", String::from("/"), self.path.as_str())
        };
        v.push((String::from(root_label), root.clone()));
        let mut cur = if root == "/" { String::new() } else { root };
        for part in rest.split('/').filter(|s| !s.is_empty()) {
            cur = format!("{cur}/{part}");
            v.push((String::from(part), cur.clone()));
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
            Ok(()) => super::super::notify_as(
                "Files",
                &format!("Restored {}", e.name),
                &format!("from {snap}"),
            ),
            Err(err) => {
                super::super::notify_as("Files", "Restore failed", &format!("{}: {err}", e.name))
            }
        }
    }

    fn ctx_rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        let rh = CTX.len() as i32 * CTX_ROW + 12;
        Rect::new(x.min(w - CTX_W - 4), y.min(h - rh - 4), CTX_W, rh)
    }

    fn restore_chip(w: i32, h: i32) -> Rect {
        Rect::new(w - 12 - 84, h - STATUS_H + 2, 84, 20)
    }

    fn hovering(&self, r: &Rect) -> bool {
        self.hover.is_some_and(|(x, y)| hit(r, x, y))
    }

    /// The 40px toolbar. Also records its hit areas.
    fn draw_toolbar(&mut self, p: &mut Painter, w: i32) {
        self.tools.clear();
        p.fill(0, 0, w, TOOL_H, CREAM);
        p.fill(0, TOOL_H - 1, w, 1, SAND);
        // Back and forward: 24px oat circles, latte when there is nowhere to go.
        for (k, (tool, can)) in [
            (Tool::Back, !self.back.is_empty()),
            (Tool::Forward, !self.fwd.is_empty()),
        ]
        .into_iter()
        .enumerate()
        {
            let r = Rect::new(12 + k as i32 * 30, 8, 24, 24);
            let hov = can && self.hovering(&r);
            p.circle(r.x, r.y, 24, if hov { SAND } else { OAT }, 255);
            let c = if can { ESPRESSO } else { LATTE };
            let glyph = if tool == Tool::Back { "<" } else { ">" };
            p.text_center(r.x, r.w, r.y + 4, glyph, c, true);
            if can {
                self.tools.push((r, tool));
            }
        }
        // From the right: view switch, Time Machine clock, Share, search.
        let view = Rect::new(w - 12 - 24, 8, 24, 24);
        let clock = Rect::new(view.x - 8 - 24, 8, 24, 24);
        let share_w = button::pill_w("Share");
        let share = Rect::new(clock.x - 8 - share_w, 8, share_w, button::PILL_H);
        let search = Rect::new(share.x - 8 - SEARCH_W, 8, SEARCH_W, 24);
        let bar = Rect::new(72, 8, (search.x - 12 - 72).max(80), 24);
        // Path bar: milk, 1px sand, radius 12; each part clickable.
        p.rrect_r(bar, 12, MILK, 255);
        p.rrect_stroke(bar.x, bar.y, bar.w, bar.h, 12, 1, SAND, 255);
        let crumbs = self.crumbs();
        let sep = "  >  ";
        let sep_w = crate::sysfont::width(sep, false);
        let widths: Vec<i32> = crumbs
            .iter()
            .map(|(l, _)| crate::sysfont::width(l, false))
            .collect();
        // Leave out leading parts until it fits (a "~" stands for them).
        let room = bar.w - 24;
        let mut first = 0;
        let total = |from: usize| -> i32 {
            widths[from..].iter().sum::<i32>() + sep_w * (widths.len() - from) as i32
        };
        while first + 1 < crumbs.len() && total(first) > room {
            first += 1;
        }
        let mut x = bar.x + 12;
        if first > 0 {
            x = p.text_p(x, bar.y + 4, "~", COCOA, false);
            x = p.text_p(x, bar.y + 4, sep, LATTE, false);
        }
        let last = crumbs.len().saturating_sub(1);
        for (k, (label, path)) in crumbs.iter().enumerate().skip(first) {
            let lw = widths[k];
            let r = Rect::new(x - 2, bar.y + 2, lw + 4, 20);
            let here = k == last;
            if !here && self.hovering(&r) {
                p.rrect_r(r, 8, OAT, 255);
            }
            let end = p.text_p(x, bar.y + 4, label, ESPRESSO, here);
            if !here {
                self.tools.push((r, Tool::Crumb(path.clone())));
            }
            x = end;
            if k != last {
                x = p.text_p(x, bar.y + 4, sep, COCOA, false);
            }
            if x > bar.x + bar.w - 12 {
                break;
            }
        }
        // Search: the same style; typing filters this folder.
        p.rrect_r(search, 12, MILK, 255);
        let edge = if self.searching { HONEY } else { SAND };
        p.rrect_stroke(search.x, search.y, search.w, search.h, 12, 1, edge, 255);
        if self.search.is_empty() && !self.searching {
            p.text_p(search.x + 12, search.y + 4, "Search", COCOA, false);
        } else {
            let shown = Font::body(false).tail(&self.search, search.w - 28);
            let end = p.text_p(search.x + 12, search.y + 4, shown, ESPRESSO, false);
            if self.searching {
                p.fill(end + 1, search.y + 4, 2, 16, ESPRESSO);
            }
        }
        self.tools.push((search, Tool::Search));
        // Share: a secondary pill, primary while a file is selected (SPEC).
        let kind = if self.document_path().is_some() {
            Kind::Primary
        } else {
            Kind::Secondary
        };
        let st = State::hover(self.hovering(&share));
        button::paint(p, share, "Share", kind, st);
        self.tools.push((share, Tool::Share));
        // Time Machine: a clock (it is not a device).
        let tm = self.path == TM || in_snapshot(&self.path).is_some();
        let hov = self.hovering(&clock);
        p.circle(
            clock.x,
            clock.y,
            24,
            if tm {
                HONEY
            } else if hov {
                SAND
            } else {
                OAT
            },
            255,
        );
        let fg = if tm { INK } else { ESPRESSO };
        icons::draw(
            p,
            clock.x + 2,
            clock.y + 2,
            20,
            Icon::Clock,
            OAT,
            0,
            fg,
            false,
        );
        self.tools.push((clock, Tool::Clock));
        // View: grid or list (shows the one you would switch to).
        let hov = self.hovering(&view);
        p.rrect_r(view, 8, if hov { SAND } else { OAT }, 255);
        if self.list {
            for (dx, dy) in [(0, 0), (8, 0), (0, 8), (8, 8)] {
                p.rrect(view.x + 5 + dx, view.y + 5 + dy, 6, 6, 1, ESPRESSO, 255);
            }
        } else {
            for k in 0..3 {
                p.fill(view.x + 6, view.y + 7 + k * 5, 12, 2, ESPRESSO);
            }
        }
        self.tools.push((view, Tool::View));
    }

    fn draw_sidebar(&self, p: &mut Painter, h: i32) {
        p.fill(0, TOOL_H, SIDE_W, h - TOOL_H, OAT);
        p.fill(SIDE_W - 1, TOOL_H, 1, h - TOOL_H, SAND);
        let (rows, labels) = self.side_rows();
        for (y, l) in labels {
            p.text(16, y, l, COCOA);
        }
        for r in rows {
            if r.y + 24 > h - STATUS_H {
                break;
            }
            if r.path == self.path {
                p.rrect(8, r.y, SIDE_W - 16, 24, R_BUTTON, STRAW, SEL_SIDEBAR_A);
            }
            if let Some(name) = r.path.strip_prefix(DROP_PREFIX) {
                // A dot: matcha while that machine is receiving, latte when not.
                let on = self.nearby.iter().any(|(n, rv)| n == name && *rv);
                p.circle(18, r.y + 8, 9, if on { MATCHA } else { LATTE }, 255);
                let l = Font::body(false).truncate(&r.label, SIDE_W - 34 - 10);
                p.text(34, r.y + 4, &l, ESPRESSO);
            } else {
                let l = Font::body(false).truncate(&r.label, SIDE_W - 18 - 10);
                p.text(18, r.y + 4, &l, ESPRESSO);
            }
        }
    }

    fn draw_snapshots(&self, p: &mut Painter, w: i32) {
        let x = SIDE_W + 20;
        let top = TOOL_H;
        p.text_x(x, top + 14, "Time Machine", ESPRESSO, 2, true);
        let body = Font::body(false);
        let room = w - x - 16;
        let intro = "Snapshots of the Milk disk: open one to browse it or restore files.";
        p.text_p(x, top + 50, &body.truncate(intro, room), COCOA, false);
        if self.snaps.is_empty() {
            let none = "No snapshots yet (Settings > Time Machine > Back up now).";
            p.text_p(x, top + 84, &body.truncate(none, room), COCOA, false);
        }
        for (i, sn) in self.snaps.iter().enumerate() {
            let y = top + 76 + i as i32 * 44 - self.scroll * 44;
            if y < top + 70 {
                continue;
            }
            let r = Rect::new(x - 8, y, w - x - 12, 40);
            if self.selected == Some(i) {
                p.rrect_r(r, R_ROW, STRAW, SEL_ROW_A);
            }
            icons::draw(p, x, y + 4, 32, Icon::Clock, CREAM, 255, ESPRESSO, true);
            p.text_p(x + 44, y + 3, &sn.name, ESPRESSO, true);
            let sub = format!("{}   generation {}", when(sn.created), sn.generation);
            let c = if self.selected == Some(i) {
                ESPRESSO
            } else {
                COCOA
            };
            p.text_p(x + 44, y + 21, &sub, c, false);
        }
    }

    fn draw_items(&self, p: &mut Painter, w: i32, h: i32) {
        if let Some(e) = &self.error {
            p.text_p(
                SIDE_W + 24,
                GRID_TOP,
                &format!("Can't open: {e}"),
                COCOA,
                false,
            );
            return;
        }
        if self.items.is_empty() {
            let msg = if self.search.is_empty() {
                "This folder is empty"
            } else {
                "Nothing here matches"
            };
            let area = Rect::new(SIDE_W, TOOL_H, w - SIDE_W, h - TOOL_H - STATUS_H);
            p.text_p_center(area.x, area.w, area.y + area.h / 2 - 8, msg, COCOA, false);
            if self.search.is_empty() {
                // A faint dashed outline where dropped files would go.
                let r = Rect::new(area.x + 24, area.y + 24, area.w - 48, area.h - 48);
                let mut x = r.x + 12;
                while x < r.x + r.w - 12 {
                    p.fill(x, r.y, 6, 1, SAND);
                    p.fill(x, r.y + r.h - 1, 6, 1, SAND);
                    x += 10;
                }
                let mut y = r.y + 12;
                while y < r.y + r.h - 12 {
                    p.fill(r.x, y, 1, 6, SAND);
                    p.fill(r.x + r.w - 1, y, 1, 6, SAND);
                    y += 10;
                }
            }
            return;
        }
        for (i, e) in self.items.iter().enumerate() {
            let r = self.item_rect(i);
            if r.y + r.h < TOOL_H || r.y > h {
                continue;
            }
            let sel = self.selected == Some(i);
            let icon = fsx::icon_for(e);
            let (bg, ba, fg) = if icon == Icon::Folder {
                (HONEY, FOLDER_A, COCOA)
            } else {
                (CREAM, 255, ESPRESSO)
            };
            if self.list {
                if sel {
                    p.rrect_r(r, R_ROW, STRAW, SEL_ROW_A);
                }
                if !icons::file_tile(p, r.x + 8, r.y + 2, 22, fsx::art_kind(e)) {
                    icons::draw(
                        p,
                        r.x + 8,
                        r.y + 2,
                        22,
                        icon,
                        bg,
                        ba,
                        fg,
                        icon != Icon::Folder,
                    );
                }
                let what = if e.kind == crate::fs::InodeType::Dir {
                    String::from("folder")
                } else {
                    fsx::human_size(e.size)
                };
                let room = r.w - 40 - 10 - crate::sysfont::width(&what, false) - 12;
                let name = Font::body(false).truncate(&e.name, room);
                p.text_p(r.x + 40, r.y + 5, &name, ESPRESSO, false);
                p.text_right(
                    r.x + r.w - 10,
                    r.y + 5,
                    &what,
                    if sel { ESPRESSO } else { COCOA },
                    1,
                    false,
                );
                continue;
            }
            let (cx, cy) = self.cell(i);
            if sel {
                p.rrect(cx - 4, cy - 4, 100, 96, R_ROW, STRAW, SEL_ROW_A);
            }
            if !icons::file_tile(p, cx + 12, cy, 56, fsx::art_kind(e)) {
                icons::draw(p, cx + 12, cy, 56, icon, bg, ba, fg, icon != Icon::Folder);
            }
            // Names longer than the cell get cut.
            let name = Font::body(false).truncate(&e.name, 92);
            p.text_center(cx - 6, 92, cy + 64, &name, ESPRESSO, false);
        }
    }

    fn draw_status(&self, p: &mut Painter, w: i32, h: i32) {
        let sy = h - STATUS_H;
        p.fill(0, sy, w, STATUS_H, CREAM);
        p.fill(0, sy, w, 1, SAND);
        if self.path == TM {
            let n = self.snaps.len();
            p.text(
                SIDE_W + 12,
                sy + 4,
                &format!("{n} snapshot{}", if n == 1 { "" } else { "s" }),
                COCOA,
            );
            return;
        }
        let n = self.items.len();
        let info = match self.selected.and_then(|i| self.items.get(i)) {
            Some(e) if e.kind == crate::fs::InodeType::Dir => format!("1 of {n} selected"),
            Some(e) => format!("1 of {n} selected, {}", size_label(e.size)),
            None => format!("{n} item{}", if n == 1 { "" } else { "s" }),
        };
        p.text(SIDE_W + 12, sy + 4, &info, COCOA);
        if let Some((snap, _)) = in_snapshot(&self.path) {
            if self.selected.is_some() {
                let r = Self::restore_chip(w, h);
                let st = State::hover(self.hovering(&r));
                button::paint(p, r, "Restore", Kind::Primary, st);
            } else {
                p.text_right(
                    w - 12,
                    sy + 4,
                    &format!("{snap}, read-only"),
                    COCOA,
                    1,
                    false,
                );
            }
        } else if let Some(a) = self.avail {
            p.text_right(
                w - 12,
                sy + 4,
                &format!("{} available", size_label(a)),
                COCOA,
                1,
                false,
            );
        }
    }
}

impl Files {
    /// Open the Milk Drop sheet for the selected file. False if no file is selected.
    fn open_share(&mut self, device: Option<&str>) -> bool {
        let Some(path) = self.document_path() else {
            return false;
        };
        self.share = Some(milkdrop::Sheet::new(&path, device));
        true
    }

    fn draw_files(&mut self, p: &mut Painter, w: i32, h: i32) {
        self.size = (w, h);
        let area_h = h - TOOL_H - STATUS_H;
        if self.list {
            self.cols = 1;
            self.rows_visible = ((area_h - 8) / LIST_ROW).max(1);
        } else {
            self.cols = ((w - SIDE_W - 20) / CELL_W).max(1);
            self.rows_visible = ((area_h - 16) / CELL_H).max(1);
        }
        self.draw_toolbar(p, w);
        self.draw_sidebar(p, h);
        {
            // Clipped to the area between the toolbar and the status bar (body coordinates).
            let mut gp = p.clipped(Rect::new(SIDE_W, TOOL_H, w - SIDE_W, area_h));
            if self.path == TM {
                self.draw_snapshots(&mut gp, w);
            } else {
                self.draw_items(&mut gp, w, h);
            }
        }
        self.draw_status(p, w, h);
        // The right-click menu, over everything.
        if let Some((x, y, i)) = self.ctx {
            let r = Self::ctx_rect(x, y, w, h);
            p.shadow(r, 10, &SHADOW_FLOAT);
            p.rrect_r(r, 10, MILK, 255);
            p.rrect_stroke(r.x, r.y, r.w, r.h, 10, 1, SAND, 255);
            let file = self
                .items
                .get(i)
                .is_some_and(|e| e.kind != crate::fs::InodeType::Dir);
            for (k, label) in CTX.iter().enumerate() {
                let enabled = k == 0 || file;
                p.text_p(
                    r.x + 14,
                    r.y + 6 + k as i32 * CTX_ROW + 4,
                    label,
                    if enabled { ESPRESSO } else { LATTE },
                    false,
                );
            }
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
        self.draw_files(p, w, h);
        // The Milk Drop sheet, over the body under the toolbar.
        let now = self.now;
        if let Some(sh) = &mut self.share {
            let r = sh.rect(w, h, TOOL_H);
            sh.draw(p, r, now);
        }
    }

    fn commands(&self) -> Vec<(&'static str, &'static str)> {
        if self.document_path().is_some() {
            alloc::vec![("Share...", "share")]
        } else {
            Vec::new()
        }
    }

    fn command(&mut self, name: &str) -> bool {
        match name {
            "share" => self.open_share(None),
            _ => false,
        }
    }

    fn drop_zone(&self, x: i32, y: i32, _path: &str) -> Option<Rect> {
        if x >= SIDE_W || self.share.is_some() {
            return None;
        }
        let (rows, _) = self.side_rows();
        rows.into_iter()
            .filter(|r| r.path.starts_with(DROP_PREFIX))
            .map(|r| Rect::new(8, r.y, SIDE_W - 16, 24))
            .find(|r| hit(r, x, y))
    }

    fn drop_at(&mut self, x: i32, y: i32, path: &str) -> bool {
        let (rows, _) = self.side_rows();
        let device = rows.into_iter().find_map(|r| {
            let name = r.path.strip_prefix(DROP_PREFIX)?;
            hit(&Rect::new(8, r.y, SIDE_W - 16, 24), x, y).then(|| String::from(name))
        });
        match device {
            Some(d) if !fsx::is_dir(path) => {
                self.share = Some(milkdrop::Sheet::new(path, Some(&d)));
                true
            }
            _ => false,
        }
    }

    fn drag_start(&mut self, x: i32, y: i32) -> Option<String> {
        if self.path == TM {
            return None;
        }
        let i = self.item_at(x, y)?;
        self.selected = Some(i);
        self.last_click = None;
        Some(fsx::resolve(&self.path, &self.items[i].name))
    }

    fn document_path(&self) -> Option<String> {
        let e = self.items.get(self.selected?)?;
        (e.kind != crate::fs::InodeType::Dir && in_snapshot(&self.path).is_none())
            .then(|| fsx::resolve(&self.path, &e.name))
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        if let Some(sh) = &mut self.share {
            return sh.hover(x, y);
        }
        let new = (x >= 0).then_some((x, y));
        let before = self.hover;
        self.hover = new;
        let which = |at: Option<(i32, i32)>| {
            at.and_then(|(x, y)| self.tools.iter().position(|(r, _)| hit(r, x, y)))
        };
        let chip = |at: Option<(i32, i32)>| {
            at.is_some_and(|(x, y)| hit(&Self::restore_chip(self.size.0, self.size.1), x, y))
        };
        which(before) != which(new) || chip(before) != chip(new)
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let PointerKind::Down(button) = e.kind else {
            return false;
        };
        // The Milk Drop sheet is modal over the body.
        if let Some(sh) = &mut self.share {
            match sh.click(e.x, e.y) {
                milkdrop::Out::Close => self.share = None,
                milkdrop::Out::Redraw | milkdrop::Out::Nothing => {}
            }
            return true;
        }
        // An open right-click menu takes the next click.
        if let Some((x, y, i)) = self.ctx.take() {
            let r = Self::ctx_rect(x, y, self.size.0, self.size.1);
            if hit(&r, e.x, e.y) {
                let k = ((e.y - r.y - 6) / CTX_ROW).clamp(0, CTX.len() as i32 - 1);
                match k {
                    0 => self.open(i),
                    _ => {
                        if let Some(p) = self.document_path() {
                            super::versions::show(&p);
                        }
                    }
                }
            }
            return true;
        }
        if button == super::super::event::Button::Right {
            if let Some(i) = self.item_at(e.x, e.y)
                && self.path != TM
            {
                self.selected = Some(i);
                self.ctx = Some((e.x, e.y, i));
            }
            return true;
        }
        // Toolbar.
        if e.y < TOOL_H {
            let tool = self
                .tools
                .iter()
                .find(|(r, _)| hit(r, e.x, e.y))
                .map(|(_, t)| t.clone());
            self.searching = tool == Some(Tool::Search);
            match tool {
                Some(Tool::Back) => self.go_back(),
                Some(Tool::Forward) => self.go_forward(),
                Some(Tool::Crumb(p)) => self.go(&p),
                Some(Tool::Clock) => self.go(TM),
                Some(Tool::Share) => {
                    if !self.open_share(None) {
                        super::super::notify_as(
                            "Files",
                            "Pick a file to share",
                            "Select a file, then Share sends it with Milk Drop",
                        );
                    }
                }
                Some(Tool::View) => {
                    self.list = !self.list;
                    self.scroll = 0;
                }
                Some(Tool::Search) | None => {}
            }
            return true;
        }
        self.searching = false;
        if e.x < SIDE_W {
            let (rows, _) = self.side_rows();
            for r in rows {
                if hit(&Rect::new(8, r.y, SIDE_W - 16, 24), e.x, e.y) {
                    self.go(&r.path);
                    return true;
                }
            }
            return true;
        }
        if self.path == TM {
            let x = SIDE_W + 20;
            let row = (e.y - TOOL_H - 76) / 44 + self.scroll;
            if e.y >= TOOL_H + 76 && (row as usize) < self.snaps.len() && e.x >= x - 8 {
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
        let sel = self.item_at(e.x, e.y);
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
        true
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        if let Some(sh) = &mut self.share {
            match sh.key(k) {
                milkdrop::Out::Close => self.share = None,
                milkdrop::Out::Redraw => {}
                milkdrop::Out::Nothing => return false,
            }
            return true;
        }
        // Typing into the search box.
        if self.searching && !k.mods.meta && !k.mods.ctrl {
            match k.key {
                Key::Char(c) if !c.is_control() => {
                    self.search.push(c);
                    self.filter();
                    self.scroll = 0;
                    return true;
                }
                Key::Backspace => {
                    self.search.pop();
                    self.filter();
                    return true;
                }
                Key::Escape => {
                    self.search.clear();
                    self.searching = false;
                    self.filter();
                    return true;
                }
                Key::Enter => {
                    self.searching = false;
                    if self.items.len() == 1 {
                        self.open(0);
                    }
                    return true;
                }
                _ => {}
            }
        }
        match k.key {
            // Cmd+F: search.
            Key::Char('f') if k.mods.meta => {
                self.searching = true;
                true
            }
            // Cmd+[ / Cmd+]: back / forward.
            Key::Char('[') if k.mods.meta => {
                self.go_back();
                true
            }
            Key::Char(']') if k.mods.meta => {
                self.go_forward();
                true
            }
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
                let j = match (k.key, self.list) {
                    (Key::Right, false) => (i + 1).min(n - 1),
                    (Key::Left, false) => i.saturating_sub(1),
                    (Key::Down, _) => (i + c).min(n - 1),
                    (Key::Up, _) => i.saturating_sub(c),
                    _ => i,
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
        self.now = now;
        let mut sheet = false;
        if let Some(sh) = &mut self.share {
            sheet = sh.tick(now);
            if sh.finished() {
                self.share = None;
                sheet = true;
            }
        }
        let pending = PENDING.lock().take();
        if let Some(p) = pending {
            self.go(&p);
            return true;
        }
        // Pick up changes made elsewhere (Terminal, Notes autosave) about once a second.
        let sec = now / 1_000_000_000;
        if sec == self.last_sec {
            return sheet;
        }
        self.last_sec = sec;
        let near = nearby();
        if near != self.nearby {
            self.nearby = near;
            sheet = true;
        }
        if self.path == TM {
            let n = snapshots();
            if n.len() != self.snaps.len() {
                self.snaps = n;
                return true;
            }
            return sheet;
        }
        let avail = available(&self.path);
        let Ok(new) = listing(&self.path) else {
            return sheet;
        };
        let same = new.len() == self.all.len()
            && new
                .iter()
                .zip(&self.all)
                .all(|(a, b)| a.name == b.name && a.size == b.size);
        if same && avail == self.avail {
            return sheet;
        }
        self.avail = avail;
        self.all = new;
        self.filter();
        true
    }
}
