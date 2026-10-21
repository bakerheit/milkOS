//! The five tabs.

use crate::charts;
use crate::data::mono_ns;
use crate::{App, EVENT, FILTER, H, PID_CLEAR, QUIT, ROW, SEC_JUMP, SHOW_FILES, SORT, VERIFY, W};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind, bold_x, button, button_w, chip, text_c, text_r};
use appkit::{Rect, Theme};
use gui::{Color, Window};
use sysview::audit::{self, Dot};
use sysview::fmt;
use sysview::proc::{Proc, SortKey, is_system, sandbox_label};
use sysview::system::Pressure;

/// A chart line colour: the darker "ink" accents on the light theme (so thin lines hold 3:1
/// against cream), the plain accents on the dark one.
fn ink(th: &Theme, name: &str) -> Color {
    if th.dark {
        th.accent(name)
    } else {
        match name {
            "blue" => 0x4B63A8,
            "honey" => 0xA87812,
            "matcha" => 0x5B8A5E,
            _ => 0xC9566B,
        }
    }
}

fn state_word(c: char) -> &'static str {
    match c {
        'R' => "running",
        'S' => "sleeping",
        'D' => "waiting",
        'Z' => "zombie",
        'T' => "stopped",
        _ => "?",
    }
}

fn tile_colour(th: &Theme, name: &str) -> Color {
    let h = name
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    [th.straw, th.honey, th.matcha, th.blue, th.lav][(h % 5) as usize]
}

fn triangle(win: &mut Window, x: i32, y: i32, up: bool, c: Color) {
    for row in 0..6 {
        let half = if up { row } else { 5 - row };
        win.fill(x + 5 - half, y + row, 2 * half + 1, 1, c);
    }
}

impl App {
    pub fn draw_processes(&mut self, win: &mut Window) {
        let th = self.th;
        let procs = self.shown_procs();
        let has_caps = self.s.procs.iter().any(|p| p.caps.is_some());
        // Quit button, top right.
        let sel_proc: Option<Proc> = self.sel.and_then(|pid| self.s.find(pid)).cloned();
        let can_quit = sel_proc.as_ref().is_some_and(|p| !is_system(p));
        let qw = button_w("Quit", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(W - 12 - qw, 8, qw, 24),
            "Quit",
            Kind::Danger,
            QUIT,
            can_quit,
            true,
        );
        // Columns: (label, key, x, right edge or 0 for left aligned).
        let cols: Vec<(&str, SortKey, i32, i32)> = if has_caps {
            alloc::vec![
                ("Name", SortKey::Name, 16, 0),
                ("PID", SortKey::Pid, 0, 290),
                ("State", SortKey::State, 300, 0),
                ("CPU %", SortKey::Cpu, 0, 480),
                ("Memory", SortKey::Memory, 0, 580),
                ("Threads", SortKey::Threads, 0, 650)
            ]
        } else {
            alloc::vec![
                ("Name", SortKey::Name, 16, 0),
                ("PID", SortKey::Pid, 0, 340),
                ("State", SortKey::State, 356, 0),
                ("CPU %", SortKey::Cpu, 0, 520),
                ("Memory", SortKey::Memory, 0, 640),
                ("Threads", SortKey::Threads, 0, 736)
            ]
        };
        // Header (28 high).
        win.fill(0, 41, W, 28, th.cream);
        widgets::hline(win, 0, 68, W, th.sand);
        for (i, (label, key, x, right)) in cols.iter().enumerate() {
            let lw = gui::str_width(label, 1);
            let lx = if *right > 0 { right - lw } else { *x };
            let active = *key == self.sort;
            bold_x(
                win,
                lx,
                47,
                label,
                if active { th.espresso } else { th.cocoa },
                1,
            );
            if active {
                triangle(win, lx + lw + 6, 52, !self.desc, th.cocoa);
            }
            let r = Rect::new(lx - 6, 41, lw + 26, 28);
            self.hits.add(r, SORT + i as u32);
        }
        if has_caps {
            bold_x(win, 666, 47, "Sandbox", th.cocoa, 1);
        }
        // Rows.
        let top = 69;
        let area = H - 24 - top - if self.sel.is_some() { 158 } else { 0 };
        let rows = self.rows_visible();
        let max_scroll = ((procs.len() as i32 - rows) * 28).max(0);
        self.scroll = self.scroll.clamp(0, max_scroll) / 28 * 28;
        self.shown.clear();
        let first = (self.scroll / 28) as usize;
        for (n, p) in procs.iter().enumerate() {
            self.shown.push(p.pid);
            if n < first || (n - first) as i32 * 28 + 28 > area {
                continue;
            }
            let y = top + (n - first) as i32 * 28;
            let selected = self.sel == Some(p.pid);
            let row = Rect::new(0, y, W, 28);
            if selected {
                win.fill(0, y, W, 28, Theme::tint(th.milk, th.straw, 0x33));
            } else if n % 2 == 1 {
                win.fill(0, y, W, 28, Theme::tint(th.milk, th.oat, 0x59));
            }
            // Icon tile and name.
            win.rrect(16, y + 4, 20, 20, 5, tile_colour(&th, &p.name));
            let initial: String = p
                .name
                .chars()
                .take(1)
                .flat_map(|c| c.to_uppercase())
                .collect();
            text_c(win, 26, y + 6, &initial, th.ink, 1);
            win.text(
                44,
                y + 6,
                &widgets::fit(&p.name, if has_caps { 190 } else { 240 }),
                th.espresso,
            );
            text_r(win, cols[1].3, y + 6, &format!("{}", p.pid), th.espresso, 1);
            win.text(cols[2].2, y + 6, state_word(p.state), th.espresso);
            text_r(win, cols[3].3, y + 6, &fmt::pct10(p.cpu10), th.espresso, 1);
            text_r(win, cols[4].3, y + 6, &fmt::mb(p.rss_kb), th.espresso, 1);
            text_r(
                win,
                cols[5].3,
                y + 6,
                &format!("{}", p.threads),
                th.espresso,
                1,
            );
            if has_caps {
                let (label, fill) = match sandbox_label(p) {
                    Some(l) => (l, Theme::tint(th.milk, th.matcha, 0x55)),
                    None if is_system(p) => ("System", th.oat),
                    None => ("", th.milk),
                };
                if !label.is_empty() {
                    let w = gui::str_width(label, 1) + 20;
                    win.rrect(666, y + 3, w, 22, 11, fill);
                    win.text(676, y + 6, label, th.espresso);
                }
            }
            self.hits.add(row, ROW + n as u32);
        }
        if procs.is_empty() {
            text_c(win, W / 2, 140, "No process matches.", th.cocoa, 1);
        }
        // The detail panel for the selected process.
        if let Some(p) = sel_proc {
            let panel = Rect::new(12, H - 24 - 154, W - 24, 148);
            widgets::bordered(win, panel, 12, th.cream, th.sand);
            let parent = self.s.find(p.ppid).map_or(String::from("none"), |pp| {
                format!("{} ({})", pp.name, pp.pid)
            });
            bold_x(win, panel.x + 16, panel.y + 12, &p.name, th.espresso, 1);
            let age = fmt::ago(mono_ns(), p.start_ns);
            win.text(
                panel.x + 16,
                panel.y + 34,
                &format!("PID {}   parent {}   started {} ago", p.pid, parent, age),
                th.cocoa,
            );
            let exe = rt::sys::readlink(&format!("/proc/{}/exe", p.pid))
                .unwrap_or_else(|_| String::from("not available"));
            win.text(
                panel.x + 16,
                panel.y + 54,
                &widgets::fit(&format!("Program   {exe}"), panel.w - 32),
                th.cocoa,
            );
            let fds = rt::sys::read_dir(&format!("/proc/{}/fd", p.pid)).map_or(0, |v| v.len());
            win.text(
                panel.x + 16,
                panel.y + 74,
                &format!("{fds} open files   {} threads", p.threads),
                th.cocoa,
            );
            let (n, last) = self.s.refusals(p.pid);
            if n > 0 {
                let t = format!(
                    "Refused {n} time{}: {}",
                    if n == 1 { "" } else { "s" },
                    last.unwrap_or_default()
                );
                win.text(
                    panel.x + 16,
                    panel.y + 94,
                    &widgets::fit(&t, panel.w - 32),
                    th.danger,
                );
            } else {
                win.text(panel.x + 16, panel.y + 94, "Nothing refused", th.cocoa);
            }
            if is_system(&p) {
                win.text(
                    panel.x + 16,
                    panel.y + 114,
                    "System processes can not be quit",
                    th.cocoa,
                );
            }
            let bw = button_w("Show Files", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(panel.right() - 16 - bw, panel.y + 12, bw, 24),
                "Show Files",
                Kind::Secondary,
                SHOW_FILES,
                true,
                true,
            );
            let sw = button_w("Security", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(panel.right() - 16 - bw - 8 - sw, panel.y + 12, sw, 24),
                "Security",
                Kind::Secondary,
                SEC_JUMP,
                true,
                true,
            );
        }
    }

    pub fn draw_cpu(&mut self, win: &mut Window) {
        let th = self.th;
        let n = self.s.cores.len();
        let card_w = (W - 48) / 2;
        let card_h = 124;
        for (i, core) in self.s.cores.iter().enumerate() {
            let (cx, cy) = (
                16 + (i as i32 % 2) * (card_w + 16),
                54 + (i as i32 / 2) * (card_h + 12),
            );
            let card = Rect::new(cx, cy, card_w, card_h);
            widgets::bordered(win, card, 14, th.cream, th.sand);
            bold_x(win, cx + 16, cy + 10, &format!("Core {i}"), th.espresso, 1);
            text_r(
                win,
                cx + card_w - 16,
                cy + 10,
                &format!("{}%", core.last()),
                th.espresso,
                1,
            );
            charts::columns(
                win,
                Rect::new(cx + 16, cy + 36, card_w - 32, card_h - 52),
                core,
                100,
                ink(&th, "blue"),
                th.milk,
            );
        }
        let rows = n.div_ceil(2) as i32;
        let y = 54 + rows * (card_h + 12) + 4;
        // Load, busiest, interrupts.
        bold_x(win, 16, y, "Load average", th.espresso, 1);
        let load = match self.s.load {
            Some([a, b, c]) => format!(
                "1 min {}.{:02}    5 min {}.{:02}    15 min {}.{:02}",
                a / 100,
                a % 100,
                b / 100,
                b % 100,
                c / 100,
                c % 100
            ),
            None => String::from("not available"),
        };
        win.text(16, y + 22, &load, th.cocoa);
        bold_x(win, 16, y + 54, "Busiest now", th.espresso, 1);
        let mut busy = self.s.procs.clone();
        sysview::proc::sort(&mut busy, SortKey::Cpu, true);
        for (k, p) in busy.iter().take(3).enumerate() {
            win.text(
                16,
                y + 76 + k as i32 * 20,
                &format!(
                    "{:<14} {:>6}%",
                    widgets::fit(&p.name, 112),
                    fmt::pct10(p.cpu10)
                ),
                th.cocoa,
            );
        }
        bold_x(win, 420, y, "Interrupts per second", th.espresso, 1);
        for (k, (label, v)) in [
            ("Timer", self.s.irq_rate[0]),
            ("Network", self.s.irq_rate[1]),
            ("Input", self.s.irq_rate[2]),
            ("Sound", self.s.irq_rate[3]),
        ]
        .iter()
        .enumerate()
        {
            win.text(420, y + 22 + k as i32 * 20, label, th.cocoa);
            text_r(
                win,
                W - 16,
                y + 22 + k as i32 * 20,
                &format!("{v}"),
                th.espresso,
                1,
            );
        }
    }

    pub fn draw_memory(&mut self, win: &mut Window) {
        let th = self.th;
        let m = self.s.mem;
        let apps: u64 = self.s.procs.iter().map(|p| p.rss_kb).sum();
        bold_x(
            win,
            16,
            54,
            &format!("{} used of {}", fmt::mb0(m.used_kb()), fmt::mb0(m.total_kb)),
            th.espresso,
            2,
        );
        let bar = Rect::new(16, 100, W - 32, 28);
        let other = m
            .total_kb
            .saturating_sub(apps + m.heap_used_kb + m.cache_kb + m.free_kb);
        let segs = [
            (apps, th.honey),
            (m.heap_used_kb, th.blue),
            (other, th.sand),
            (m.cache_kb, th.lav),
            (m.free_kb, th.oat),
        ];
        charts::stacked(win, bar, &segs, m.total_kb, th.oat);
        // Legend.
        let legend = [
            ("Apps", apps, th.honey),
            ("Kernel heap", m.heap_used_kb, th.blue),
            ("System", other, th.sand),
            ("Page cache", m.cache_kb, th.lav),
            ("Free", m.free_kb, th.oat),
        ];
        let mut x = 16;
        for (name, kb, c) in legend {
            win.rrect(x, 142, 14, 14, 4, c);
            let t = format!("{name} {}", fmt::mb0(kb));
            win.text(x + 20, 141, &t, th.espresso);
            x += 20 + gui::str_width(&t, 1) + 22;
        }
        // Pressure.
        bold_x(win, 16, 178, "Memory pressure", th.espresso, 1);
        let (word, _) = match m.pressure() {
            Pressure::Low => ("Low", th.matcha),
            Pressure::Medium => ("Medium", th.honey),
            Pressure::High => ("High", th.danger),
        };
        text_r(win, W - 16, 178, word, th.espresso, 1);
        let g = Rect::new(16, 204, W - 32, 14);
        let zone = |frac_a: i32, frac_b: i32| {
            ((g.x + g.w * frac_a / 100), (g.w * (frac_b - frac_a) / 100))
        };
        for (a, b, col) in [(0, 60, th.matcha), (60, 85, th.honey), (85, 100, th.danger)] {
            let (zx, zw) = zone(a, b);
            win.fill(zx, g.y, zw - 1, g.h, col);
        }
        let used_pct = (m.used_kb() * 100).checked_div(m.total_kb).unwrap_or(0) as i32;
        let mx = g.x + g.w * used_pct / 100;
        win.fill(mx - 1, g.y - 4, 3, g.h + 8, th.espresso);
        // The biggest processes.
        bold_x(win, 16, 244, "Biggest processes", th.espresso, 1);
        let mut v = self.s.procs.clone();
        sysview::proc::sort(&mut v, SortKey::Memory, true);
        let max = v.first().map_or(1, |p| p.rss_kb.max(1));
        for (k, p) in v.iter().take(6).enumerate() {
            let y = 270 + k as i32 * 28;
            win.text(16, y + 2, &widgets::fit(&p.name, 150), th.espresso);
            charts::bar(
                win,
                Rect::new(180, y + 4, W - 180 - 130, 14),
                (p.rss_kb * 1000 / max) as u32,
                ink(&th, "honey"),
                th.oat,
            );
            text_r(win, W - 16, y + 2, &fmt::mb(p.rss_kb), th.espresso, 1);
        }
        win.text(
            16,
            H - 24 - 24,
            "The page cache is memory the system can give back whenever a program needs it.",
            th.cocoa,
        );
    }

    pub fn draw_disk(&mut self, win: &mut Window) {
        let th = self.th;
        let card_w = (W - 48) / 2;
        let card_h = 150;
        for (i, (name, ring, rate, colour)) in [
            ("Reads", &self.s.disk_read, self.s.disk_rate.0, "blue"),
            ("Writes", &self.s.disk_write, self.s.disk_rate.1, "straw"),
        ]
        .iter()
        .enumerate()
        {
            let cx = 16 + i as i32 * (card_w + 16);
            widgets::bordered(
                win,
                Rect::new(cx, 54, card_w, card_h),
                14,
                th.cream,
                th.sand,
            );
            bold_x(win, cx + 16, 64, name, th.espresso, 1);
            text_r(win, cx + card_w - 16, 64, &fmt::rate(*rate), th.espresso, 1);
            let max = ring.max().max(64);
            charts::columns(
                win,
                Rect::new(cx + 16, 90, card_w - 32, card_h - 52),
                ring,
                max,
                ink(&th, colour),
                th.milk,
            );
        }
        bold_x(win, 16, 224, "Free space", th.espresso, 1);
        let mut y = 252;
        if let Some((total, free)) = self.s.milk_blocks {
            let used = total.saturating_sub(free);
            win.text(16, y, "Milk disk", th.espresso);
            text_r(
                win,
                W - 16,
                y,
                &format!("{} free of {}", fmt::mb0(free * 4), fmt::mb0(total * 4)),
                th.cocoa,
                1,
            );
            charts::bar(
                win,
                Rect::new(16, y + 24, W - 32, 14),
                (used * 1000 / total.max(1)) as u32,
                ink(&th, "blue"),
                th.oat,
            );
            y += 64;
        }
        let ram = self.s.mem.total_kb;
        win.text(16, y, "Root (in memory)", th.espresso);
        text_r(
            win,
            W - 16,
            y,
            &format!("{} used", fmt::mb(self.s.ram_disk_kb)),
            th.cocoa,
            1,
        );
        charts::bar(
            win,
            Rect::new(16, y + 24, W - 32, 14),
            (self.s.ram_disk_kb * 1000 / ram.max(1)) as u32,
            ink(&th, "honey"),
            th.oat,
        );
    }

    pub fn draw_security(&mut self, win: &mut Window) {
        let th = self.th;
        // The banner.
        let banner = Rect::new(12, 52, W - 24, 56);
        let (ok, title, line) = match &self.verdict {
            Some(v) if v.ok => (
                true,
                String::from("Log is intact"),
                format!(
                    "{} records, verified {}",
                    v.records,
                    fmt::clock(self.verified_wall_ns)
                ),
            ),
            Some(v) => {
                let (seq, why) = v.broken_at.clone().unwrap_or((0, String::new()));
                (
                    false,
                    String::from("The log has been changed"),
                    format!("First bad record {seq}: {why}"),
                )
            }
            None => (
                true,
                String::from("Not checked yet"),
                String::from("Press Verify again"),
            ),
        };
        let accent = if ok { th.matcha } else { th.danger };
        widgets::bordered(win, banner, 12, Theme::tint(th.milk, accent, 0x22), accent);
        bold_x(
            win,
            banner.x + 16,
            banner.y + 10,
            &title,
            if ok { th.espresso } else { th.danger },
            1,
        );
        win.text(banner.x + 16, banner.y + 30, &line, th.cocoa);
        let vw = button_w("Verify again", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(banner.right() - 16 - vw, banner.y + 16, vw, 24),
            "Verify again",
            Kind::Secondary,
            VERIFY,
            true,
            true,
        );
        // Filter chips.
        let mut x = 12;
        for (i, (f, label)) in audit::FILTERS.iter().enumerate() {
            x += chip(
                win,
                &th,
                &mut self.hits,
                x,
                120,
                label,
                self.filter == *f,
                FILTER + i as u32,
            ) + 6;
        }
        if let Some(pid) = self.pid_filter {
            let name = self
                .s
                .find(pid)
                .map_or(format!("pid {pid}"), |p| format!("{} ({pid})", p.name));
            x += 8;
            chip(
                win,
                &th,
                &mut self.hits,
                x,
                120,
                &format!("{name}  x"),
                true,
                PID_CLEAR,
            );
        }
        // Events, newest first.
        let mut idx: Vec<usize> = (0..self.s.audit.len())
            .filter(|&i| {
                let r = &self.s.audit[i];
                audit::matches(r, self.filter)
                    && audit::search_hit(r, &self.search.text)
                    && self.pid_filter.is_none_or(|p| r.pid == p)
            })
            .collect();
        idx.reverse();
        let top = 152;
        let row_h = 48;
        let rows = ((H - 24 - top) / row_h) as usize;
        let max_scroll = ((idx.len() as i32 - rows as i32) * row_h).max(0);
        self.sec_scroll = self.sec_scroll.clamp(0, max_scroll) / row_h * row_h;
        let first = (self.sec_scroll / row_h) as usize;
        if idx.is_empty() {
            text_c(win, W / 2, top + 60, "Nothing to show.", th.cocoa, 1);
        }
        for (n, &i) in idx.iter().skip(first).take(rows).enumerate() {
            let r = &self.s.audit[i];
            let d = audit::describe(r);
            let y = top + n as i32 * row_h;
            let row = Rect::new(12, y, W - 24, row_h - 4);
            if n % 2 == 1 {
                win.rrect(
                    row.x,
                    row.y,
                    row.w,
                    row.h,
                    8,
                    Theme::tint(th.milk, th.oat, 0x59),
                );
            }
            let dot = match d.dot {
                Dot::Danger => th.danger,
                Dot::Good => th.matcha,
                Dot::Info => th.blue,
                Dot::Trust => th.honey,
                Dot::Plain => th.latte,
            };
            win.disc(row.x + 14, y + 20, 5, dot);
            bold_x(
                win,
                row.x + 32,
                y + 4,
                &widgets::fit(&d.sentence, row.w - 200),
                if d.dot == Dot::Danger {
                    th.danger
                } else {
                    th.espresso
                },
                1,
            );
            win.text(
                row.x + 32,
                y + 24,
                &widgets::fit(&d.detail, row.w - 200),
                th.cocoa,
            );
            text_r(
                win,
                row.right() - 8,
                y + 4,
                &fmt::clock(r.time_ns),
                th.espresso,
                1,
            );
            text_r(
                win,
                row.right() - 8,
                y + 24,
                &widgets::fit(&r.event, 160),
                th.cocoa,
                1,
            );
            self.hits.add(row, EVENT + i as u32);
        }
    }
}
