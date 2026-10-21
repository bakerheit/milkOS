//! Activity Monitor: what is running and how much it uses, Quit and Force Quit, and a readable
//! view of the audit log (docs/ui-concepts/SPEC-APPS.md, section 1). Reads /proc; the logic is
//! in `user/sysview`.

#![no_std]
#![no_main]

extern crate alloc;

mod charts;
mod data;
mod tabs;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind, bold_x, button, button_w, field, segmented, text_r};
use appkit::{Field, Hits, Rect, Theme};
use data::{Sampler, mono_ns};
use gui::{Event, Key, Window, blend};
use sysview::audit::{Filter, Verdict};
use sysview::proc::{Proc, SortKey};

rt::main!(main);

pub const W: i32 = 800;
pub const H: i32 = 600;

// Hit ids.
pub const TAB: u32 = 10; // 10..14
pub const SORT: u32 = 20; // 20..26
pub const QUIT: u32 = 30;
pub const SHOW_FILES: u32 = 31;
pub const SEC_JUMP: u32 = 32;
pub const INTERVAL: u32 = 33;
pub const VERIFY: u32 = 34;
pub const PID_CLEAR: u32 = 35;
pub const SEARCH: u32 = 36;
pub const FILTER: u32 = 40; // 40..46
pub const Q_CANCEL: u32 = 60;
pub const Q_QUIT: u32 = 61;
pub const Q_FORCE: u32 = 62;
pub const RAW_CLOSE: u32 = 63;
pub const ROW: u32 = 1000; // + row index
pub const EVENT: u32 = 100_000; // + record index

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Processes,
    Cpu,
    Memory,
    Disk,
    Security,
}

const TABS: [Tab; 5] = [
    Tab::Processes,
    Tab::Cpu,
    Tab::Memory,
    Tab::Disk,
    Tab::Security,
];
/// Refresh interval choices in ms (0 = paused).
const INTERVALS: [u64; 5] = [500, 1000, 2000, 5000, 0];

pub struct QuitAsk {
    pub pid: u32,
    pub name: String,
    /// When the polite request was sent (monotonic ms); `None` while still asking.
    pub asked_ms: Option<u64>,
}

pub struct App {
    pub th: Theme,
    pub tab: Tab,
    pub s: Sampler,
    pub hits: Hits,
    pub search: Field,
    pub search_focus: bool,
    pub sort: SortKey,
    pub desc: bool,
    pub sel: Option<u32>,
    pub scroll: i32,
    pub interval: usize,
    pub quit: Option<QuitAsk>,
    pub filter: Filter,
    pub pid_filter: Option<u32>,
    pub raw: Option<usize>,
    pub verdict: Option<Verdict>,
    pub verified_wall_ns: u64,
    pub sec_scroll: i32,
    /// Rows drawn by the last frame, for the click ids.
    pub shown: Vec<u32>,
}

fn main(_args: &rt::Args) -> i32 {
    let Ok(mut win) = Window::new("Activity Monitor", W, H) else {
        rt::println!("activity: no desktop to open a window on");
        return 1;
    };
    let mut app = App {
        th: Theme::current(),
        tab: Tab::Processes,
        s: Sampler::default(),
        hits: Hits::default(),
        search: Field::new("", 30),
        search_focus: false,
        sort: SortKey::Cpu,
        desc: true,
        sel: None,
        scroll: 0,
        interval: 1,
        quit: None,
        filter: Filter::All,
        pid_filter: None,
        raw: None,
        verdict: None,
        verified_wall_ns: 0,
        sec_scroll: 0,
        shown: Vec::new(),
    };
    app.s.refresh();
    let mut last = mono_ns() / 1_000_000;
    loop {
        let ev = win.event(100);
        match ev {
            Some(Event::Close) => break,
            Some(Event::Down { x, y, button: 1 }) => {
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
            Some(Event::Scroll(d)) => match app.tab {
                Tab::Security => app.sec_scroll = (app.sec_scroll + d * 28).max(0),
                _ => app.scroll = (app.scroll + d * 28).max(0),
            },
            _ => {}
        }
        let now = mono_ns() / 1_000_000;
        let every = INTERVALS[app.interval];
        if every != 0 && now.saturating_sub(last) >= every {
            last = now;
            app.s.refresh();
            app.th = Theme::current();
        }
        app.check_quit(now);
        app.draw(&mut win);
        win.commit();
    }
    0
}

impl App {
    pub fn shown_procs(&self) -> Vec<Proc> {
        let mut v: Vec<Proc> = sysview::proc::filter(&self.s.procs, &self.search.text)
            .into_iter()
            .cloned()
            .collect();
        sysview::proc::sort(&mut v, self.sort, self.desc);
        v
    }

    fn click(&mut self, id: u32) {
        if self.quit.is_some() || self.raw.is_some() {
            match id {
                Q_CANCEL => self.quit = None,
                Q_QUIT => self.ask_quit(),
                Q_FORCE => {
                    if let Some(q) = self.quit.take() {
                        let _ = rt::sys::kill(q.pid as i32, 9);
                        self.s.refresh();
                    }
                }
                RAW_CLOSE => self.raw = None,
                _ => {}
            }
            return;
        }
        match id {
            t if (TAB..TAB + 5).contains(&t) => {
                self.tab = TABS[(t - TAB) as usize];
                self.search_focus = false;
                if self.tab == Tab::Security && self.verdict.is_none() {
                    self.verify();
                }
            }
            s if (SORT..SORT + 6).contains(&s) => {
                let key = [
                    SortKey::Name,
                    SortKey::Pid,
                    SortKey::State,
                    SortKey::Cpu,
                    SortKey::Memory,
                    SortKey::Threads,
                ][(s - SORT) as usize];
                if self.sort == key {
                    self.desc = !self.desc;
                } else {
                    self.sort = key;
                    self.desc = matches!(key, SortKey::Cpu | SortKey::Memory | SortKey::Threads);
                }
            }
            SEARCH => self.search_focus = true,
            QUIT => self.open_quit(),
            SHOW_FILES => {
                if let Some(pid) = self.sel {
                    let cwd = rt::sys::readlink(&format!("/proc/{pid}/cwd"))
                        .unwrap_or_else(|_| String::from("/"));
                    if let Ok(fd) = rt::sys::open("/dev/wm", rt::sys::O_WRONLY | rt::sys::O_CLOEXEC)
                    {
                        let _ = rt::sys::write_all(fd, format!("open {cwd}\n").as_bytes());
                        let _ = rt::sys::close(fd);
                    }
                }
            }
            SEC_JUMP => {
                self.pid_filter = self.sel;
                self.tab = Tab::Security;
                self.filter = Filter::All;
                if self.verdict.is_none() {
                    self.verify();
                }
            }
            PID_CLEAR => self.pid_filter = None,
            INTERVAL => self.interval = (self.interval + 1) % INTERVALS.len(),
            VERIFY => self.verify(),
            f if (FILTER..FILTER + 6).contains(&f) => {
                self.filter = sysview::audit::FILTERS[(f - FILTER) as usize].0
            }
            r if (ROW..EVENT).contains(&r) => {
                if let Some(&pid) = self.shown.get((r - ROW) as usize) {
                    self.sel = Some(pid);
                }
            }
            e if e >= EVENT => self.raw = Some((e - EVENT) as usize),
            _ => {}
        }
    }

    fn open_quit(&mut self) {
        let Some(pid) = self.sel else { return };
        let Some(p) = self.s.find(pid) else { return };
        if sysview::proc::is_system(p) {
            return;
        }
        self.quit = Some(QuitAsk {
            pid,
            name: p.name.clone(),
            asked_ms: None,
        });
    }

    fn ask_quit(&mut self) {
        if let Some(q) = &mut self.quit
            && q.asked_ms.is_none()
        {
            let _ = rt::sys::kill(q.pid as i32, 15);
            q.asked_ms = Some(mono_ns() / 1_000_000);
        }
    }

    /// Close the quit sheet once the process is gone.
    fn check_quit(&mut self, _now: u64) {
        if let Some(q) = &self.quit
            && q.asked_ms.is_some()
            && rt::sys::stat_path(&format!("/proc/{}", q.pid), true).is_err()
        {
            self.quit = None;
            self.sel = None;
            self.s.refresh();
        }
    }

    fn verify(&mut self) {
        let out = data::capture("/bin/audit", &["audit", "verify"]);
        self.verdict = sysview::audit::parse_verify(&out);
        self.verified_wall_ns = data::wall_ns();
    }

    fn key(&mut self, k: Key, m: gui::Mods) {
        if self.quit.is_some() || self.raw.is_some() {
            match k {
                Key::Escape => {
                    self.quit = None;
                    self.raw = None;
                }
                Key::Enter if self.quit.as_ref().is_some_and(|q| q.asked_ms.is_none()) => {
                    self.ask_quit()
                }
                _ => {}
            }
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
                    self.scroll = 0;
                }
            }
            return;
        }
        if (m.ctrl || m.meta)
            && let Key::Char(c) = k
        {
            match c {
                '1'..='5' => self.tab = TABS[c as usize - '1' as usize],
                'f' => self.search_focus = true,
                _ => {}
            }
            return;
        }
        match (self.tab, k) {
            (Tab::Processes, Key::Up | Key::Down) => {
                let procs = self.shown_procs();
                let i = self.sel.and_then(|p| procs.iter().position(|x| x.pid == p));
                let n = match (i, k) {
                    (None, _) => 0,
                    (Some(i), Key::Up) => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(procs.len().saturating_sub(1)),
                };
                self.sel = procs.get(n).map(|p| p.pid);
                // Keep the row in view.
                let (top, rows) = (n as i32 * 28, self.rows_visible());
                if top < self.scroll {
                    self.scroll = top;
                } else if top >= self.scroll + rows * 28 {
                    self.scroll = (top - (rows - 1) * 28).max(0);
                }
            }
            (Tab::Processes, Key::Delete) => self.open_quit(),
            (Tab::Processes, Key::Escape) => self.sel = None,
            _ => {}
        }
    }

    /// How many table rows fit above the summary bar (and the detail panel).
    pub fn rows_visible(&self) -> i32 {
        let detail = if self.sel.is_some() { 158 } else { 0 };
        (H - 24 - detail - 69) / 28
    }

    // ---- drawing --------------------------------------------------------------------------

    fn draw(&mut self, win: &mut Window) {
        let th = self.th;
        self.hits.clear();
        win.fill(0, 0, W, H, th.milk);
        widgets::toolbar(win, &th);
        let sel = TABS.iter().position(|&t| t == self.tab).unwrap_or(0);
        let sw = segmented(
            win,
            &th,
            &mut self.hits,
            12,
            6,
            &["Processes", "CPU", "Memory", "Disk", "Security"],
            sel,
            TAB,
        );
        let caret = mono_ns() / 500_000_000 % 2 == 0;
        if matches!(self.tab, Tab::Processes | Tab::Security) {
            field(
                win,
                &th,
                &mut self.hits,
                Rect::new(12 + sw + 12, 8, 150, 24),
                &self.search,
                "Search",
                self.search_focus,
                caret,
                SEARCH,
            );
        }
        match self.tab {
            Tab::Processes => self.draw_processes(win),
            Tab::Cpu => self.draw_cpu(win),
            Tab::Memory => self.draw_memory(win),
            Tab::Disk => self.draw_disk(win),
            Tab::Security => self.draw_security(win),
        }
        self.draw_summary(win);
        if self.quit.is_some() || self.raw.is_some() {
            self.hits.modal();
            let ink = th.ink;
            for p in win.pixels() {
                *p = blend(*p, ink, 0x66);
            }
            self.draw_sheets(win);
        }
    }

    fn draw_summary(&mut self, win: &mut Window) {
        let th = self.th;
        win.fill(0, H - 24, W, 24, th.cream);
        widgets::hline(win, 0, H - 24, W, th.sand);
        let threads: u32 = self.s.procs.iter().map(|p| p.threads).sum();
        let m = &self.s.mem;
        let s = format!(
            "{} processes   {} threads   Memory {} of {}   CPU {}%",
            self.s.procs.len(),
            threads,
            sysview::fmt::mb0(m.used_kb()),
            sysview::fmt::mb0(m.total_kb),
            self.s.total_cpu_pct()
        );
        win.text(12, H - 20, &s, th.cocoa);
        let every = match INTERVALS[self.interval] {
            0 => String::from("paused (click to change)"),
            500 => String::from("updates every 0.5 s"),
            ms => format!("updates every {} s", ms / 1000),
        };
        let w = gui::str_width(&every, 1);
        text_r(win, W - 12, H - 20, &every, th.cocoa, 1);
        self.hits
            .add(Rect::new(W - 12 - w - 4, H - 24, w + 8, 24), INTERVAL);
    }

    fn draw_sheets(&mut self, win: &mut Window) {
        let th = self.th;
        if let Some(q) = &self.quit {
            let r = Rect::new((W - 420) / 2, (H - 190) / 2, 420, 190);
            win.rrect(
                r.x - 1,
                r.y + 6,
                r.w + 2,
                r.h + 2,
                16,
                blend(th.milk, th.ink, 0x33),
            );
            widgets::bordered(win, r, 16, th.milk, th.sand);
            let (title, body, force) = match q.asked_ms {
                None => (
                    format!("Quit {}?", q.name),
                    String::from("It will be asked to finish and close."),
                    false,
                ),
                Some(t) => {
                    let waited = (mono_ns() / 1_000_000).saturating_sub(t);
                    if waited < 5000 {
                        (
                            format!("Quitting {}...", q.name),
                            String::from("Waiting for it to close."),
                            false,
                        )
                    } else {
                        (
                            format!("{} has not quit", q.name),
                            String::from("Force Quit ends it at once; anything unsaved is lost."),
                            true,
                        )
                    }
                }
            };
            bold_x(win, r.x + 20, r.y + 18, &title, th.espresso, 2);
            win.text(r.x + 20, r.y + 70, &body, th.cocoa);
            let by = r.bottom() - 48;
            let mut bx = r.right() - 20;
            if force {
                let fw = button_w("Force Quit", false);
                bx -= fw;
                button(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(bx, by, fw, 28),
                    "Force Quit",
                    Kind::Danger,
                    Q_FORCE,
                    true,
                    false,
                );
                bx -= 8;
            } else if q.asked_ms.is_none() {
                let qw = button_w("Quit", false);
                bx -= qw;
                button(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(bx, by, qw, 28),
                    "Quit",
                    Kind::Primary,
                    Q_QUIT,
                    true,
                    false,
                );
                bx -= 8;
            }
            let cw = button_w(
                if q.asked_ms.is_some() {
                    "Close"
                } else {
                    "Cancel"
                },
                false,
            );
            bx -= cw;
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(bx, by, cw, 28),
                if q.asked_ms.is_some() {
                    "Close"
                } else {
                    "Cancel"
                },
                Kind::Secondary,
                Q_CANCEL,
                true,
                false,
            );
        } else if let Some(i) = self.raw {
            let r = Rect::new((W - 700) / 2, (H - 240) / 2, 700, 240);
            win.rrect(
                r.x - 1,
                r.y + 6,
                r.w + 2,
                r.h + 2,
                16,
                blend(th.milk, th.ink, 0x33),
            );
            widgets::bordered(win, r, 16, th.milk, th.sand);
            bold_x(win, r.x + 20, r.y + 18, "Raw record", th.espresso, 2);
            if let Some(rec) = self.s.audit.get(i) {
                // The line, wrapped to the sheet.
                let per = ((r.w - 40) / 8) as usize;
                for (n, chunk) in rec.line.as_bytes().chunks(per).take(8).enumerate() {
                    win.text(
                        r.x + 20,
                        r.y + 64 + n as i32 * 18,
                        &String::from_utf8_lossy(chunk),
                        th.cocoa,
                    );
                }
            }
            let cw = button_w("Close", false);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(r.right() - 20 - cw, r.bottom() - 48, cw, 28),
                "Close",
                Kind::Primary,
                RAW_CLOSE,
                true,
                false,
            );
        }
    }
}
