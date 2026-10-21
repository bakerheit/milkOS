//! Milk Store: a desktop window (user/gui) for the `milk` package manager.
//!
//! It lists the packages of the milk index (name, version, description, a coloured tile,
//! installed / upgrade / trying badges), shows details, and runs `/bin/milk` for Install,
//! Try (in a milkfs clone), Keep, Discard, Remove, Upgrade, Update and Upgrade all, showing
//! milk's output live and the result. Everything goes through `/bin/milk`, so signatures,
//! clones and audit records work exactly as on the command line.
//!
//! Data: `MILK_TABLE=1 milk search` (milk's structured output, parsed with rt::table), the
//! index cache for versions and sizes, installed manifests (`/Applications/<name>/manifest`,
//! whose `dock` colour becomes the tile colour), `/sys/fs/milkfs/clones` for "trying".
//!
//! Keys (also what scripted tests use): Up/Down select, i install, t try, k keep,
//! d discard, r remove, u upgrade, g update (get the index), a upgrade all, q quit.
//!
//! Theme: the SPEC tokens ("Whole milk"). The wm does not publish its theme in /dev/wm's
//! status yet, so the Store reads `dark = true` from the desktop's settings file
//! (`/home/milk/.config/milk/settings`) and then uses the "Chocolate milk" values (the same
//! mapping as the wm's theme table).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use gui::{Color, Event, Key, Window, rgb};
use rt::table::Table;
use rt::{eprintln, sys};

rt::main!(main);

const W: i32 = 860;
const H: i32 = 560;
const TOP: i32 = 64;
const LIST_X: i32 = 16;
const LIST_W: i32 = 300;
const ROW_H: i32 = 64;
const CARD_X: i32 = LIST_X + LIST_W + 16;
const CARD_W: i32 = W - CARD_X - 16;
const BTN_Y: i32 = 290;
const BTN_H: i32 = 32;
const LOG_Y: i32 = 338;
const ENV: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb"];
const ENV_TABLE: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb", "MILK_TABLE=1"];

/// The palette in use (SPEC tokens, or their "Chocolate milk" values).
struct Theme {
    milk: Color,
    cream: Color,
    oat: Color,
    sand: Color,
    latte: Color,
    cocoa: Color,
    espresso: Color,
    dark: bool,
}

fn theme() -> Theme {
    let status = text("/dev/wm");
    let dark = status
        .lines()
        .find_map(|l| l.strip_prefix("theme "))
        .map(|t| t.trim() == "dark")
        .unwrap_or_else(|| {
            text("/home/milk/.config/milk/settings").lines().any(|l| {
                let (k, v) = l.split_once('=').unwrap_or((l, ""));
                k.trim() == "dark" && v.trim() == "true"
            })
        });
    if dark {
        Theme {
            milk: rgb(0x2E, 0x25, 0x20),
            cream: rgb(0x38, 0x2D, 0x27),
            oat: rgb(0x42, 0x35, 0x2D),
            sand: rgb(0x55, 0x46, 0x3C),
            latte: rgb(0x8C, 0x77, 0x64),
            cocoa: rgb(0xBE, 0xA8, 0x90),
            espresso: rgb(0xF4, 0xEB, 0xE0),
            dark,
        }
    } else {
        Theme {
            milk: gui::MILK,
            cream: gui::CREAM,
            oat: gui::OAT,
            sand: gui::SAND,
            latte: gui::LATTE,
            cocoa: gui::COCOA,
            espresso: gui::ESPRESSO,
            dark,
        }
    }
}

fn text(p: &str) -> String {
    sys::read_file(p)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

#[derive(Clone)]
struct Pkg {
    name: String,
    version: String,
    installed: Option<String>,
    desc: String,
    trying: bool,
    auto: bool,
    versions: Vec<String>,
    size: u64,
    depends: String,
    tile: Color,
}

impl Pkg {
    fn upgradable(&self) -> bool {
        self.installed
            .as_deref()
            .is_some_and(|v| rt::version::cmp(&self.version, v) == core::cmp::Ordering::Greater)
    }
}

/// Run /bin/milk and collect its stdout (blocking; for quick, local queries).
fn capture(args: &[&str], env: &[&str]) -> Vec<u8> {
    let Ok((r, w)) = sys::pipe() else {
        return Vec::new();
    };
    let mut argv = alloc::vec!["milk"];
    argv.extend_from_slice(args);
    match sys::fork() {
        Ok(0) => {
            let _ = sys::dup2(w, 1);
            let _ = sys::dup2(w, 2);
            let _ = sys::execve("/bin/milk", &argv, env);
            sys::exit(127);
        }
        Ok(pid) => {
            let _ = sys::close(w);
            let mut out = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match sys::read(r, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                    Err(e) if e.0 == 4 => continue,
                    Err(_) => break,
                }
            }
            let _ = sys::close(r);
            let _ = sys::wait4(pid as i32, 0);
            out
        }
        Err(_) => Vec::new(),
    }
}

fn palette_tile(name: &str) -> Color {
    let tiles = [gui::STRAW, gui::HONEY, gui::MATCHA, gui::BLUE, gui::LAV];
    // FNV-1a, so similar names still spread over the accents.
    let h = name.bytes().fold(0x811c_9dc5u32, |a, b| {
        (a ^ b as u32).wrapping_mul(0x0100_0193)
    });
    tiles[(h % tiles.len() as u32) as usize]
}

fn parse_colour(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    (h.len() == 6)
        .then(|| u32::from_str_radix(h, 16).ok())
        .flatten()
}

fn load() -> Vec<Pkg> {
    let t = Table::parse(&capture(&["search"], ENV_TABLE));
    let col = |n: &str| t.col(n);
    let (Some(cn), Some(cv), Some(ci), Some(cd)) = (
        col("name"),
        col("version"),
        col("installed"),
        col("description"),
    ) else {
        return Vec::new();
    };
    let clones = text("/sys/fs/milkfs/clones");
    let cache = text("/Applications/.milk/index.txt");
    t.rows
        .iter()
        .map(|r| {
            let name = r[cn].text();
            let installed = Some(r[ci].text()).filter(|v| !v.is_empty());
            let manifest = text(&alloc::format!("/Applications/{name}/manifest"));
            let field = |k: &str| {
                manifest
                    .lines()
                    .find_map(|l| l.strip_prefix(k).and_then(|x| x.strip_prefix(' ')))
                    .map(String::from)
            };
            let tile = field("dock")
                .and_then(|d| d.split_whitespace().find_map(parse_colour))
                .unwrap_or_else(|| palette_tile(&name));
            let mut versions = Vec::new();
            let mut size = 0;
            for l in cache.lines() {
                let w: Vec<&str> = l.split(' ').collect();
                if w.len() > 4 && w[1] == name {
                    versions.push(String::from(w[2]));
                    size = w[4].parse().unwrap_or(size);
                }
            }
            versions.sort_by(|a, b| rt::version::cmp(a, b));
            Pkg {
                trying: clones
                    .lines()
                    .any(|l| l.split(' ').next() == Some(&alloc::format!("try-{name}"))),
                auto: sys::stat_path(&alloc::format!("/Applications/{name}/.auto"), false).is_ok(),
                depends: field("depends").unwrap_or_default(),
                version: r[cv].text(),
                desc: r[cd].text(),
                installed,
                versions,
                size,
                tile,
                name,
            }
        })
        .collect()
}

/// A running /bin/milk.
struct Job {
    pid: u32,
    fd: i32,
    label: String,
    started_ns: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    None,
    Ok,
    Failed,
}

struct App {
    win: Window,
    th: Theme,
    pkgs: Vec<Pkg>,
    sel: usize,
    job: Option<Job>,
    log: Vec<String>,
    partial: String,
    outcome: Outcome,
    last_label: String,
    repo: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Act {
    Install,
    Try,
    Keep,
    Discard,
    Remove,
    Upgrade,
}

const ACTS: [(Act, &str); 6] = [
    (Act::Install, "Install"),
    (Act::Try, "Try"),
    (Act::Keep, "Keep"),
    (Act::Discard, "Discard"),
    (Act::Remove, "Remove"),
    (Act::Upgrade, "Upgrade"),
];

/// Word-wrap `s` to `cols` characters; words longer than a line are split.
fn wrap(s: &str, cols: usize) -> Vec<String> {
    let cols = cols.max(1);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        let len = line.chars().count();
        if len > 0 && len + 1 + word.len() > cols {
            out.push(core::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        while line.chars().count() + word.len() > cols {
            let room = cols - line.chars().count();
            line.extend(word.drain(..room));
            out.push(core::mem::take(&mut line));
        }
        line.extend(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// The action buttons: (action, label, x, width), sized to their labels.
fn act_buttons() -> Vec<(Act, &'static str, i32, i32)> {
    let mut x = CARD_X + 20;
    ACTS.iter()
        .map(|&(a, label)| {
            let w = gui::str_width(label, 1) + 28;
            let r = (a, label, x, w);
            x += w + 6;
            r
        })
        .collect()
}

impl App {
    fn cur(&self) -> Option<&Pkg> {
        self.pkgs.get(self.sel)
    }

    fn enabled(&self, a: Act) -> bool {
        if self.job.is_some() {
            return false;
        }
        let Some(p) = self.cur() else { return false };
        match a {
            Act::Install => p.installed.is_none() && !p.trying,
            Act::Try => p.installed.is_none() && !p.trying,
            Act::Keep | Act::Discard => p.trying,
            Act::Remove => p.installed.is_some() && !p.trying,
            Act::Upgrade => p.upgradable() && !p.trying,
        }
    }

    fn start(&mut self, args: &[&str]) {
        if self.job.is_some() {
            return;
        }
        let Ok((r, w)) = sys::pipe() else { return };
        // Only our end is non-blocking (the child's writes must not fail when full).
        // SAFETY: fcntl(fd, F_SETFL, O_RDONLY | O_NONBLOCK).
        unsafe { sys::syscall(25, [r as usize, 4, 0o4000, 0, 0, 0]) };
        let mut argv = alloc::vec!["milk"];
        argv.extend_from_slice(args);
        let label = argv.join(" ");
        match sys::fork() {
            Ok(0) => {
                let _ = sys::dup2(w, 1);
                let _ = sys::dup2(w, 2);
                let _ = sys::execve("/bin/milk", &argv, ENV);
                sys::exit(127);
            }
            Ok(pid) => {
                let _ = sys::close(w);
                self.log.clear();
                self.partial.clear();
                self.log.push(alloc::format!("$ {label}"));
                self.outcome = Outcome::None;
                self.job = Some(Job {
                    pid,
                    fd: r,
                    label,
                    started_ns: sys::clock_ns(sys::CLOCK_MONOTONIC),
                });
            }
            Err(e) => self.log.push(alloc::format!("fork: {e}")),
        }
    }

    fn act(&mut self, a: Act) {
        if !self.enabled(a) {
            return;
        }
        let name = self.cur().map(|p| p.name.clone()).unwrap_or_default();
        let verb = match a {
            Act::Install => "install",
            Act::Try => "try",
            Act::Keep => "keep",
            Act::Discard => "discard",
            Act::Remove => "remove",
            Act::Upgrade => "upgrade",
        };
        if a == Act::Upgrade {
            self.start(&[verb, &name, "-y"]);
        } else {
            self.start(&[verb, &name]);
        }
    }

    /// Read what milk printed; reap it when it is done. True if anything changed.
    fn pump(&mut self) -> bool {
        let Some(job) = self.job.as_ref() else {
            return false;
        };
        let (fd, pid) = (job.fd, job.pid);
        let mut changed = false;
        let mut buf = [0u8; 2048];
        loop {
            match sys::read(fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    self.partial.push_str(&String::from_utf8_lossy(&buf[..n]));
                    while let Some(i) = self.partial.find('\n') {
                        let line: String = self.partial.drain(..=i).collect();
                        self.log.push(String::from(line.trim_end()));
                    }
                    changed = true;
                }
                Err(_) => break, // EAGAIN: nothing more for now
            }
        }
        if let Ok((p, st)) = sys::wait4(pid as i32, sys::WNOHANG)
            && p == pid
        {
            if !self.partial.is_empty() {
                let l = core::mem::take(&mut self.partial);
                self.log.push(l);
            }
            let _ = sys::close(fd);
            self.outcome = if st == 0 {
                Outcome::Ok
            } else {
                Outcome::Failed
            };
            let job = self.job.take().unwrap();
            self.last_label = job.label;
            // Reload, keeping the selection on the same package.
            let name = self.cur().map(|p| p.name.clone());
            self.pkgs = load();
            if let Some(n) = name
                && let Some(i) = self.pkgs.iter().position(|p| p.name == n)
            {
                self.sel = i;
            }
            self.sel = self.sel.min(self.pkgs.len().saturating_sub(1));
            changed = true;
        }
        changed
    }

    // ---- drawing --------------------------------------------------------------------------

    fn heading(&mut self, x: i32, y: i32, s: &str, c: Color) {
        self.win.text_x(x, y, s, c, 2);
        self.win.text_x(x + 1, y, s, c, 2);
    }

    fn button(&mut self, x: i32, y: i32, w: i32, label: &str, primary: bool, on: bool) {
        let th = &self.th;
        let (bg, fg) = match (on, primary) {
            (false, _) => (th.oat, th.latte),
            (true, true) => (gui::STRAW, gui::MILK),
            (true, false) => (th.oat, th.espresso),
        };
        self.win.rrect(x, y, w, BTN_H, 8, bg);
        let tw = gui::str_width(label, 1);
        self.win.text(x + (w - tw) / 2, y + 8, label, fg);
    }

    fn chip(&mut self, x: i32, y: i32, label: &str, c: Color) -> i32 {
        let w = gui::str_width(label, 1) + 16;
        let bg = gui::blend(self.th.milk, c, 0x66);
        self.win.rrect(x - w, y, w, 22, 8, bg);
        self.win.text(x - w + 8, y + 3, label, self.th.espresso);
        w
    }

    fn tile(&mut self, x: i32, y: i32, size: i32, p_name: &str, c: Color) {
        self.win.rrect(x, y, size, size, size * 24 / 100, c);
        let letter: String = p_name
            .chars()
            .take(1)
            .flat_map(|c| c.to_uppercase())
            .collect();
        let scale = if size >= 56 { 3 } else { 2 };
        let tw = gui::str_width(&letter, scale);
        self.win.text_x(
            x + (size - tw) / 2,
            y + (size - 16 * scale) / 2,
            &letter,
            gui::MILK,
            scale,
        );
    }

    fn draw(&mut self) {
        let (milk, cream, oat, sand, cocoa, espresso) = (
            self.th.milk,
            self.th.cream,
            self.th.oat,
            self.th.sand,
            self.th.cocoa,
            self.th.espresso,
        );
        self.win.fill(0, 0, W, H, cream);
        // Top bar.
        self.heading(16, 14, "Milk Store", espresso);
        let n = self.pkgs.len();
        let inst = self.pkgs.iter().filter(|p| p.installed.is_some()).count();
        let ups = self.pkgs.iter().filter(|p| p.upgradable()).count();
        let sub = alloc::format!(
            "{n} packages, {inst} installed, {ups} upgrades \u{b7} {}",
            self.repo
        );
        let room = ((W - 16 - 104 - 8 - 72 - 12 - 184) / 8) as usize;
        let sub: String = sub.chars().take(room).collect();
        self.win.text(184, 22, &sub, cocoa);
        let idle = self.job.is_none();
        self.button(W - 16 - 104 - 8 - 72, 14, 72, "Update", false, idle);
        self.button(
            W - 16 - 104,
            14,
            104,
            "Upgrade all",
            ups > 0,
            idle && ups > 0,
        );
        self.win.fill(0, TOP - 1, W, 1, sand);

        // List.
        if self.pkgs.is_empty() {
            self.win.rrect(LIST_X, TOP + 12, LIST_W, 96, 12, milk);
            self.win
                .text(LIST_X + 16, TOP + 28, "No package index yet.", espresso);
            self.win
                .text(LIST_X + 16, TOP + 52, "Update fetches it from", cocoa);
            self.win
                .text(LIST_X + 16, TOP + 70, "/etc/milk/repos.", cocoa);
        }
        let pkgs = self.pkgs.clone();
        for (i, p) in pkgs.iter().enumerate() {
            let y = TOP + 12 + i as i32 * (ROW_H + 4);
            if y + ROW_H > H - 12 {
                break;
            }
            let bg = if i == self.sel {
                gui::blend(milk, gui::STRAW, 0x44)
            } else {
                milk
            };
            self.win.rrect(LIST_X, y, LIST_W, ROW_H, 12, bg);
            self.tile(LIST_X + 12, y + 10, 44, &p.name, p.tile);
            self.win.bold(LIST_X + 68, y + 12, &p.name, espresso);
            let mut desc: String = p.desc.chars().take(26).collect();
            if p.desc.chars().count() > 26 {
                desc.push_str("...");
            }
            self.win.text(LIST_X + 68, y + 34, &desc, cocoa);
            let right = LIST_X + LIST_W - 10;
            let (label, c) = if p.trying {
                (String::from("trying"), gui::BLUE)
            } else if p.upgradable() {
                (alloc::format!("{} new", p.version), gui::HONEY)
            } else if p.installed.is_some() {
                (String::from("installed"), gui::MATCHA)
            } else {
                (p.version.clone(), sand)
            };
            self.chip(right, y + 10, &label, c);
        }

        // Detail card.
        self.win
            .rrect(CARD_X, TOP + 12, CARD_W, H - TOP - 28, 20, milk);
        if let Some(p) = pkgs.get(self.sel) {
            let (x, y) = (CARD_X + 20, TOP + 32);
            self.tile(x, y, 64, &p.name, p.tile);
            self.heading(x + 80, y, &p.name, espresso);
            let ver = match &p.installed {
                Some(v) if *v != p.version => {
                    alloc::format!("{v} installed, {} available", p.version)
                }
                Some(v) => alloc::format!(
                    "{v} installed{}",
                    if p.auto { " (as a dependency)" } else { "" }
                ),
                None if p.trying => alloc::format!("{} in a clone (try-{})", p.version, p.name),
                None => alloc::format!("version {}", p.version),
            };
            self.win.text(x + 80, y + 40, &ver, cocoa);
            // Description, wrapped.
            let cols = ((CARD_W - 40) / 8) as usize;
            let mut ly = y + 84;
            let mut lines = wrap(&p.desc, cols);
            if lines.len() > 2 {
                lines.truncate(2);
                let last = &mut lines[1];
                while last.chars().count() + 3 > cols {
                    last.pop();
                }
                last.push_str("...");
            }
            for l in &lines {
                self.win.text(x, ly, l, espresso);
                ly += 18;
            }
            ly = ly.max(y + 84 + 36) + 8;
            let facts = [
                ("versions", p.versions.join(" ")),
                ("size", alloc::format!("{} KiB", p.size.div_ceil(1024))),
                (
                    "depends",
                    if p.depends.is_empty() {
                        String::from("-")
                    } else {
                        p.depends.clone()
                    },
                ),
            ];
            for (k, v) in facts {
                self.win.text(x, ly, k, cocoa);
                self.win.text(x + 80, ly, &v, espresso);
                ly += 18;
            }
            // Buttons.
            for (a, label, bx, bw) in act_buttons() {
                let on = self.enabled(a);
                let primary = matches!(a, Act::Install | Act::Keep | Act::Upgrade);
                self.button(bx, BTN_Y, bw, label, primary, on);
            }
        }

        // Log + progress.
        let lx = CARD_X + 20;
        let lw = CARD_W - 40;
        let lh = H - 28 - LOG_Y - 36;
        self.win.rrect(lx, LOG_Y, lw, lh, 12, oat);
        let cols = ((lw - 24) / 8) as usize;
        let rows = ((lh - 16) / 18) as usize;
        // Wrap long lines (URLs, signatures) and show the newest that fit.
        let wrapped: Vec<String> = self.log.iter().flat_map(|l| wrap(l, cols)).collect();
        let start = wrapped.len().saturating_sub(rows);
        for (i, l) in wrapped[start..].iter().enumerate() {
            let c = if l.starts_with("$ ") {
                cocoa
            } else if l.contains("refused") || l.contains("failed") || l.contains("BAD") {
                gui::STRAW
            } else {
                espresso
            };
            self.win.text(lx + 12, LOG_Y + 8 + i as i32 * 18, l, c);
        }
        let sy = LOG_Y + lh + 10;
        match (&self.job, self.outcome) {
            (Some(j), _) => {
                // Indeterminate progress: a straw segment sliding along a sand track.
                let t = (sys::clock_ns(sys::CLOCK_MONOTONIC) - j.started_ns) / 20_000_000;
                self.win.rrect(lx, sy + 6, 160, 8, 4, sand);
                let off = (t % 120) as i32;
                self.win.rrect(lx + off, sy + 6, 40, 8, 4, gui::STRAW);
                let label = alloc::format!("running {}", j.label);
                self.win.text(lx + 172, sy, &label, cocoa);
            }
            (None, Outcome::Ok) => {
                let w = self.chip(lx + 56, sy - 2, "done", gui::MATCHA);
                let _ = w;
                let l = self.last_label.clone();
                self.win.text(lx + 64, sy + 1, &l, cocoa);
            }
            (None, Outcome::Failed) => {
                self.chip(lx + 64, sy - 2, "failed", gui::STRAW);
                let l = self.last_label.clone();
                self.win.text(lx + 72, sy + 1, &l, cocoa);
            }
            (None, Outcome::None) => {
                let hint = if self.th.dark {
                    "Chocolate milk"
                } else {
                    "Whole milk"
                };
                let s = alloc::format!("keys: i t k d r u, g update, a upgrade all \u{b7} {hint}");
                self.win.text(lx, sy + 1, &s, self.th.latte);
            }
        }
        self.win.commit();
    }

    fn click(&mut self, x: i32, y: i32) {
        // Top bar buttons.
        if (14..14 + BTN_H).contains(&y) {
            if (W - 16 - 104 - 8 - 72..W - 16 - 104 - 8).contains(&x) {
                self.start(&["update"]);
            } else if (W - 16 - 104..W - 16).contains(&x)
                && self.pkgs.iter().any(|p| p.upgradable())
            {
                self.start(&["upgrade", "-y"]);
            }
            return;
        }
        // List rows.
        if (LIST_X..LIST_X + LIST_W).contains(&x) && y > TOP + 12 {
            let i = ((y - TOP - 12) / (ROW_H + 4)) as usize;
            if i < self.pkgs.len() {
                self.sel = i;
            }
            return;
        }
        // Action buttons.
        if (BTN_Y..BTN_Y + BTN_H).contains(&y) {
            for (a, _, bx, bw) in act_buttons() {
                if (bx..bx + bw).contains(&x) {
                    self.act(a);
                }
            }
        }
    }
}

fn main(_args: &rt::Args) -> i32 {
    let win = match Window::new("Milk Store", W, H) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("store: no window ({e}); it needs the desktop");
            return 1;
        }
    };
    let repo = text("/etc/milk/repos")
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let l = l.strip_prefix("http://").unwrap_or(l);
            String::from(l.trim_end_matches('/'))
        })
        .unwrap_or_else(|| String::from("no repository"));
    let mut app = App {
        win,
        th: theme(),
        pkgs: load(),
        sel: 0,
        job: None,
        log: Vec::new(),
        partial: String::new(),
        outcome: Outcome::None,
        last_label: String::new(),
        repo,
    };
    if app.pkgs.is_empty() {
        app.start(&["update"]); // first run: fetch the index
    }
    app.draw();
    loop {
        let busy = app.job.is_some();
        let ev = app.win.event(if busy { 40 } else { 2000 });
        let mut dirty = busy; // animate the progress bar
        if let Some(ev) = ev {
            dirty = true;
            match ev {
                Event::Close => return 0,
                Event::Down { x, y, button: 1 } => app.click(x, y),
                Event::Scroll(n) => {
                    app.sel = (app.sel as i32 + n.signum())
                        .clamp(0, app.pkgs.len() as i32 - 1)
                        .max(0) as usize;
                }
                Event::Key(k, _) => match k {
                    Key::Up => app.sel = app.sel.saturating_sub(1),
                    Key::Down => app.sel = (app.sel + 1).min(app.pkgs.len().saturating_sub(1)),
                    Key::Char('i') => app.act(Act::Install),
                    Key::Char('t') => app.act(Act::Try),
                    Key::Char('k') => app.act(Act::Keep),
                    Key::Char('d') => app.act(Act::Discard),
                    Key::Char('r') => app.act(Act::Remove),
                    Key::Char('u') => app.act(Act::Upgrade),
                    Key::Char('g') => app.start(&["update"]),
                    Key::Char('a') => {
                        if app.pkgs.iter().any(|p| p.upgradable()) {
                            app.start(&["upgrade", "-y"]);
                        }
                    }
                    Key::Char('q') => return 0,
                    _ => {}
                },
                _ => {}
            }
        }
        if app.pump() {
            dirty = true;
        }
        if dirty {
            app.draw();
        }
    }
}
