//! Skim's search: one query over everything, results grouped by kind.
//!
//! * Applications and desktop places (the launcher entries in shell.rs)
//! * Actions apps registered (`wm::actions`)
//! * Files under the home folder (a small index, rebuilt lazily when Skim opens and it is
//!   older than 30 s)
//! * Settings sections
//! * `= 2+2*3` the calculator: + - * / % ^, parentheses, decimals
//! * `> ls /bin` a shell command, run in a new Terminal tab

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{self, InodeType};
use crate::gfx::Color;
use crate::sync::SpinLock;
use crate::timer;

use super::actions;
use super::apps::settings::SECTIONS;
use super::icons::Icon;
use super::shell::{self, fuzzy};
use super::theme::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Kind {
    Calc,
    Command,
    App,
    /// Tabs of tabbed windows (tabs everywhere).
    Tab,
    Action,
    Setting,
    File,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Calc => "CALCULATOR",
            Kind::Command => "COMMAND",
            Kind::App => "APPLICATIONS",
            Kind::Action => "ACTIONS",
            Kind::Setting => "SETTINGS",
            Kind::Tab => "TABS",
            Kind::File => "FILES",
        }
    }

    /// How many results of this kind to show at most (one more each with the extra rows of
    /// a taller screen).
    fn cap(self) -> usize {
        let more = (launch_rows() > LAUNCH_MAX_ROWS) as usize;
        match self {
            Kind::Calc | Kind::Command => 1,
            Kind::App => 2 + more,
            Kind::Action => 2 + more,
            Kind::Setting => 1,
            Kind::Tab => 2 + more,
            Kind::File => 2 + more,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Act {
    Entry(shell::Action),
    Action(actions::ActionFn),
    Open(String),
    Setting(usize),
    Calc(String),
    Command(String),
    /// An installed package (`packages::get`).
    Package(usize),
    /// Tab `1` of window `0`.
    Tab(u32, usize),
}

/// Every tab of every tabbed window (window, tab, title, app), set by the desktop before a
/// search.
static TABS: SpinLock<Vec<(u32, usize, String, String)>> = SpinLock::new(Vec::new());

pub fn set_tabs(v: Vec<(u32, usize, String, String)>) {
    *TABS.lock() = v;
}

#[derive(Clone)]
pub struct Item {
    pub kind: Kind,
    pub title: String,
    pub sub: String,
    pub icon: Icon,
    pub bg: Color,
    pub fg: Color,
    pub act: Act,
}

// ---- the file index ---------------------------------------------------------------------------

struct Index {
    built_ns: u64,
    paths: Vec<(String, InodeType)>,
}

static INDEX: SpinLock<Index> = SpinLock::new(Index {
    built_ns: 0,
    paths: Vec::new(),
});

const MAX_FILES: usize = 4000;

fn walk(dir: &str, depth: usize, out: &mut Vec<(String, InodeType)>) {
    if depth > 6 || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = fs::readdir(dir) else {
        return;
    };
    for e in entries {
        if e.name.starts_with('.') || out.len() >= MAX_FILES {
            continue;
        }
        let path = format!("{dir}/{}", e.name);
        let kind = e.kind;
        out.push((path.clone(), kind));
        if kind == InodeType::Dir {
            walk(&path, depth + 1, out);
        }
    }
}

/// Rebuild the home folder index if it is more than 30 s old (called when Skim opens).
pub fn refresh_index() {
    let now = timer::now_ns();
    let built = INDEX.lock().built_ns;
    if built != 0 && now - built < 30_000_000_000 {
        return;
    }
    let mut paths = Vec::new();
    walk("/home/milk", 0, &mut paths);
    let mut ix = INDEX.lock();
    ix.paths = paths;
    ix.built_ns = now.max(1);
}

// ---- the calculator ---------------------------------------------------------------------------

struct Calc<'a> {
    s: &'a [u8],
    i: usize,
}

impl Calc<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i] == b' ' {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if op == b'+' { v + r } else { v - r };
        }
        Some(v)
    }
    fn term(&mut self) -> Option<f64> {
        let mut v = self.power()?;
        while let Some(op @ (b'*' | b'/' | b'%' | b'x')) = self.peek() {
            self.i += 1;
            let r = self.power()?;
            v = match op {
                b'*' | b'x' => v * r,
                b'/' if r != 0.0 => v / r,
                b'%' if r != 0.0 => v - r * trunc(v / r),
                _ => return None,
            };
        }
        Some(v)
    }
    fn power(&mut self) -> Option<f64> {
        let b = self.unary()?;
        if self.peek() == Some(b'^') {
            self.i += 1;
            let e = self.power()?; // right associative
            if e != trunc(e) || !(-64.0..=64.0).contains(&e) {
                return None;
            }
            let mut r = 1.0;
            for _ in 0..(e as i64).unsigned_abs() {
                r *= b;
            }
            return Some(if e < 0.0 { 1.0 / r } else { r });
        }
        Some(b)
    }
    fn unary(&mut self) -> Option<f64> {
        match self.peek()? {
            b'-' => {
                self.i += 1;
                Some(-self.unary()?)
            }
            b'+' => {
                self.i += 1;
                self.unary()
            }
            b'(' => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() != Some(b')') {
                    return None;
                }
                self.i += 1;
                Some(v)
            }
            _ => self.number(),
        }
    }
    fn number(&mut self) -> Option<f64> {
        self.ws();
        let start = self.i;
        let (mut v, mut frac, mut seen) = (0.0f64, 0.0f64, false);
        while let Some(&c) = self.s.get(self.i) {
            match c {
                b'0'..=b'9' => {
                    let d = (c - b'0') as f64;
                    if frac == 0.0 {
                        v = v * 10.0 + d;
                    } else {
                        v += d * frac;
                        frac /= 10.0;
                    }
                    seen = true;
                }
                b'.' if frac == 0.0 => frac = 0.1,
                b',' | b'_' => {}
                _ => break,
            }
            self.i += 1;
        }
        (seen && self.i > start).then_some(v)
    }
}

fn trunc(v: f64) -> f64 {
    (v as i64) as f64
}

/// Evaluate a calculator expression; None if it doesn't parse or divides by zero.
pub fn calc(src: &str) -> Option<f64> {
    let mut c = Calc {
        s: src.as_bytes(),
        i: 0,
    };
    let v = c.expr()?;
    (c.peek().is_none() && v.is_finite()).then_some(v)
}

/// A friendly number: integers without a decimal point, otherwise up to 8 places, trimmed.
pub fn show_number(v: f64) -> String {
    if v == trunc(v) && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.8}");
    String::from(s.trim_end_matches('0').trim_end_matches('.'))
}

// ---- search -----------------------------------------------------------------------------------

/// Everything matching `q`, grouped by kind (in `Kind` order), each kind capped.
pub fn search(q: &str) -> Vec<Item> {
    let q = q.trim_start();
    let mut out: Vec<(i32, Item)> = Vec::new();
    // Calculator and commands own the query when it starts with = or >.
    if let Some(expr) = q.strip_prefix('=') {
        let (title, act) = match calc(expr) {
            Some(v) => {
                let r = show_number(v);
                (format!("= {r}"), Act::Calc(r))
            }
            None if expr.trim().is_empty() => {
                (String::from("Type a sum"), Act::Calc(String::new()))
            }
            None => (
                String::from("Can't work that out"),
                Act::Calc(String::new()),
            ),
        };
        return alloc::vec![Item {
            kind: Kind::Calc,
            title,
            sub: format!("{}   Enter copies the answer", expr.trim()),
            icon: Icon::Calc,
            bg: CREAM,
            fg: ESPRESSO,
            act,
        }];
    }
    if let Some(cmd) = q.strip_prefix('>') {
        let cmd = cmd.trim();
        return alloc::vec![Item {
            kind: Kind::Command,
            title: if cmd.is_empty() {
                String::from("Type a command")
            } else {
                format!("Run  {cmd}")
            },
            sub: String::from("in a new Terminal tab"),
            icon: Icon::Term,
            bg: TERM_BG,
            fg: TERM_FG,
            act: Act::Command(String::from(cmd)),
        }];
    }
    let empty = q.trim().is_empty();
    // Applications and places.
    for (k, e) in shell::ENTRIES.iter().enumerate() {
        let score = if empty {
            Some(100 - k as i32)
        } else {
            fuzzy(q, e.name)
        };
        if let Some(s) = score {
            out.push((
                s,
                Item {
                    kind: Kind::App,
                    title: String::from(e.name),
                    sub: String::from(e.sub),
                    icon: e.icon,
                    bg: e.bg,
                    fg: e.fg,
                    act: Act::Entry(e.action),
                },
            ));
        }
    }
    // Installed packages, with the apps.
    for (k, p) in super::packages::all().into_iter().enumerate() {
        let score = if empty {
            Some(50 - k as i32)
        } else {
            fuzzy(q, &p.label).or_else(|| fuzzy(q, &p.name))
        };
        if let Some(s) = score {
            out.push((
                s,
                Item {
                    kind: Kind::App,
                    sub: if p.description.is_empty() {
                        format!("Installed with milk ({})", p.name)
                    } else {
                        p.description.clone()
                    },
                    title: p.label,
                    icon: Icon::Package,
                    bg: p.color,
                    fg: INK,
                    act: Act::Package(k),
                },
            ));
        }
    }
    if !empty {
        // Tabs of tabbed windows, by their title or their app.
        for (wid, i, title, app) in TABS.lock().iter() {
            let s = fuzzy(q, title).or_else(|| fuzzy(q, app).map(|s| s - 10));
            if let Some(s) = s {
                out.push((
                    s,
                    Item {
                        kind: Kind::Tab,
                        title: title.clone(),
                        sub: format!("{app}, a tab"),
                        icon: Icon::Spaces,
                        bg: MILK,
                        fg: ESPRESSO,
                        act: Act::Tab(*wid, *i),
                    },
                ));
            }
        }
        // Actions: match the name, or "app name".
        for a in actions::all() {
            let s = fuzzy(q, a.name)
                .or_else(|| fuzzy(q, &format!("{} {}", a.app, a.name)).map(|s| s - 20));
            if let Some(s) = s {
                out.push((
                    s,
                    Item {
                        kind: Kind::Action,
                        title: String::from(a.name),
                        sub: String::from(a.app),
                        icon: Icon::Action,
                        bg: CREAM,
                        fg: STRAW,
                        act: Act::Action(a.run),
                    },
                ));
            }
        }
        // Settings sections.
        for (i, name) in SECTIONS.iter().enumerate() {
            if let Some(s) = fuzzy(q, name) {
                out.push((
                    s,
                    Item {
                        kind: Kind::Setting,
                        title: String::from(*name),
                        sub: String::from("Settings"),
                        icon: Icon::Gear,
                        bg: LATTE,
                        fg: ESPRESSO,
                        act: Act::Setting(i),
                    },
                ));
            }
        }
        // Files, by name (folders a little ahead).
        let ix = INDEX.lock();
        for (path, kind) in &ix.paths {
            let name = super::apps::fsx::basename(path);
            if let Some(s) = fuzzy(q, name) {
                let dir = super::apps::fsx::pretty(&super::apps::fsx::parent(path));
                let folder = *kind == InodeType::Dir;
                out.push((
                    s + if folder { 2 } else { 0 },
                    Item {
                        kind: Kind::File,
                        title: String::from(name),
                        sub: dir,
                        icon: if folder { Icon::Folder } else { Icon::Doc },
                        bg: if folder { HONEY } else { CREAM },
                        fg: if folder { INK } else { ESPRESSO },
                        act: Act::Open(path.clone()),
                    },
                ));
            }
        }
    }
    // Group by kind; kinds ordered by their best match (so a file that matches well beats a
    // weak app match), best first within a kind; each kind capped, SPEC's 6 rows in all.
    out.sort_by(|a, b| a.1.kind.cmp(&b.1.kind).then(b.0.cmp(&a.0)));
    let mut groups: Vec<(i32, Vec<Item>)> = Vec::new();
    for (score, it) in out {
        match groups.last_mut() {
            Some((_, g)) if g[0].kind == it.kind => {
                if g.len() < if empty { 4 } else { it.kind.cap() } {
                    g.push(it);
                }
            }
            _ => groups.push((score, alloc::vec![it])),
        }
    }
    if !empty {
        groups.sort_by_key(|g| core::cmp::Reverse(g.0));
    }
    groups
        .into_iter()
        .flat_map(|(_, g)| g)
        .take(launch_rows())
        .collect()
}

/// Rows as drawn: a header before each kind, then its items. (header?, item index)
pub fn rows(items: &[Item]) -> Vec<(Option<Kind>, Option<usize>)> {
    let mut v = Vec::new();
    let mut last = None;
    for (i, it) in items.iter().enumerate() {
        if last != Some(it.kind) {
            last = Some(it.kind);
            v.push((Some(it.kind), None));
        }
        v.push((None, Some(i)));
    }
    v
}

pub const HEADER_H: i32 = 26;
