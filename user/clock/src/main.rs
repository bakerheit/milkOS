//! Clock: a clock, alarms, a stopwatch and a timer (docs/ui-concepts/SPEC-APPS.md, section 4).
//!
//! Alarms and timers are the files in ~/Library/Clock; `alarmd` rings them, so they keep working
//! with this window closed. The window edits the files and shows what is in them. The stopwatch
//! lives only in this window. `clock ring LABEL HH:MM` is the ringing panel `alarmd` opens.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{
    self, Kind, bold_c, bold_x, button, button_w, chip, field, segmented, switch, text_c, text_r,
};
use appkit::{Field, Hits, Rect, Theme, shapes};
use gui::{Event, Key, Window, blend, str_width};
use timekit::clock::{
    Alarm, Alarms, PRESETS_MIN, Stopwatch, Timer, Timers, fmt_countdown, fmt_days, fmt_in,
    fmt_stopwatch, remember,
};
use timekit::date::{MONTHS, WEEKDAYS, split_secs};
use timekit::text::{fmt_hm, parse_hm};

rt::main!(main);

const W: i32 = 600;
const H: i32 = 560;

// Hit ids.
const TAB: u32 = 10; // 10..13
const NEW_ALARM: u32 = 20;
const ROW: u32 = 1000; // + sorted index
const SWITCH: u32 = 2000; // + sorted index
const SHEET_SAVE: u32 = 30;
const SHEET_CANCEL: u32 = 31;
const SHEET_DELETE: u32 = 32;
const SHEET_DAY: u32 = 40; // 40..46
const SHEET_TIME: u32 = 50;
const SHEET_LABEL: u32 = 51;
const SW_START: u32 = 60;
const SW_LAP: u32 = 61;
const SW_RESET: u32 = 62;
const T_START: u32 = 70;
const T_PAUSE: u32 = 71;
const T_RESET: u32 = 72;
const T_PLUS: u32 = 73;
const T_PRESET: u32 = 80; // + minutes index
const T_CUSTOM: u32 = 79;
const T_LABEL: u32 = 78;
const T_OTHER_RM: u32 = 3000; // + timer id
const C_START: u32 = 90;
const C_CANCEL: u32 = 91;
const C_MINS: u32 = 92;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Clock,
    Alarms,
    Stopwatch,
    Timer,
}

struct AlarmSheet {
    id: Option<u32>,
    time: Field,
    label: Field,
    days: u8,
    focus: u8, // 0 time, 1 label
    error: bool,
}

struct CustomSheet {
    mins: Field,
}

struct App {
    th: Theme,
    tab: Tab,
    paths: timeio::Paths,
    alarms: Alarms,
    timers: Timers,
    recent: Vec<u32>,
    sw: Stopwatch,
    sheet: Option<AlarmSheet>,
    custom: Option<CustomSheet>,
    hits: Hits,
    scroll: i32,
    /// Timer tab, nothing running: the duration chosen, and the label being typed.
    chosen_ms: u64,
    t_label: Field,
    t_focus: bool,
}

fn main(args: &rt::Args) -> i32 {
    if args.get(1) == Some("ring") {
        return ring_panel(args);
    }
    let paths = timeio::Paths::from_args(args);
    let Ok(mut win) = Window::new("Clock", W, H) else {
        rt::println!("clock: no desktop to open a window on");
        return 1;
    };
    let tab = match args.get(1) {
        Some("alarms") => Tab::Alarms,
        Some("stopwatch") => Tab::Stopwatch,
        Some("timer") => Tab::Timer,
        _ => Tab::Clock,
    };
    let mut app = App {
        th: Theme::current(),
        tab,
        alarms: timeio::load_alarms(&paths).0,
        timers: timeio::load_timers(&paths).0,
        recent: timeio::load_recent(&paths),
        paths,
        sw: Stopwatch::default(),
        sheet: None,
        custom: None,
        hits: Hits::default(),
        scroll: 0,
        chosen_ms: 5 * 60_000,
        t_label: Field::new("", 24),
        t_focus: false,
    };
    let mut last_load = 0;
    loop {
        let wait = if matches!(app.tab, Tab::Clock | Tab::Stopwatch | Tab::Timer) {
            100
        } else {
            500
        };
        let ev = win.event(wait);
        let mut quit = false;
        match ev {
            Some(Event::Close) => quit = true,
            Some(Event::Down { x, y, button: 1 }) => {
                if let Some(id) = app.hits.at(x, y) {
                    app.click(id);
                } else {
                    app.t_focus = false;
                }
            }
            Some(Event::Key(k, m)) => {
                if (m.ctrl || m.meta) && k == Key::Char('q') {
                    quit = true;
                } else {
                    app.key(k, m);
                }
            }
            Some(Event::Scroll(d)) => app.scroll = (app.scroll + d * 24).clamp(0, 4000),
            // Settings changed the font or the theme: the next draw (below) uses them.
            Some(Event::Appearance { dark, .. }) => app.th = Theme::for_dark(dark),
            _ => {}
        }
        if quit {
            break;
        }
        // Pick up what alarmd (or the alarm and timer tools) changed, unless a sheet is open.
        let now = timeio::mono_ms();
        if app.sheet.is_none() && now.saturating_sub(last_load) > 700 {
            last_load = now;
            app.alarms = timeio::load_alarms(&app.paths).0;
            app.timers = timeio::load_timers(&app.paths).0;
            app.th = Theme::current();
        }
        app.draw(&mut win);
        win.commit();
    }
    0
}

fn now_parts() -> (timekit::Date, u32) {
    split_secs(timeio::now_secs())
}

fn fade(bg: u32, c: u32, pct: u32) -> u32 {
    blend(bg, c, (pct * 255 / 100) as u8)
}

impl App {
    fn save_alarms(&self) {
        let _ = timeio::save_alarms(&self.paths, &self.alarms);
    }

    fn save_timers(&self) {
        let _ = timeio::save_timers(&self.paths, &self.timers);
    }

    /// Alarm indexes in time order.
    fn sorted(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.alarms.list.len()).collect();
        v.sort_by_key(|&i| (self.alarms.list[i].at, self.alarms.list[i].id));
        v
    }

    /// The timer this window shows: a running one first, else a paused one.
    fn current_timer(&self) -> Option<usize> {
        let now = timeio::mono_ms();
        self.timers
            .list
            .iter()
            .enumerate()
            .filter(|(_, t)| t.running)
            .min_by_key(|(_, t)| t.remaining_ms(now))
            .map(|(i, _)| i)
            .or_else(|| self.timers.list.iter().position(|t| !t.running))
    }

    fn open_sheet(&mut self, id: Option<u32>) {
        let (time, label, days) = match id.and_then(|i| self.alarms.list.iter().find(|a| a.id == i))
        {
            Some(a) => (fmt_hm(a.at), a.label.clone(), a.days),
            None => (String::from("07:00"), String::new(), 0),
        };
        self.sheet = Some(AlarmSheet {
            id,
            time: Field::new(&time, 5),
            label: Field::new(&label, 24),
            days,
            focus: 0,
            error: false,
        });
    }

    fn save_sheet(&mut self) {
        let Some(s) = &mut self.sheet else { return };
        let Some(at) = parse_hm(&s.time.text) else {
            s.error = true;
            return;
        };
        let (id, label, days) = (s.id, s.label.text.clone(), s.days);
        match id.and_then(|i| self.alarms.list.iter_mut().find(|a| a.id == i)) {
            Some(a) => {
                a.at = at;
                a.label = label;
                a.days = days;
                a.enabled = true;
            }
            None => {
                self.alarms.add(Alarm::new(0, &label, at, days));
            }
        }
        self.save_alarms();
        self.sheet = None;
    }

    fn start_timer(&mut self, ms: u64, label: &str) {
        let now = timeio::mono_ms();
        self.timers.add(Timer::start(0, label, ms, now));
        self.save_timers();
    }

    fn click(&mut self, id: u32) {
        // Sheets first.
        if self.sheet.is_some() {
            match id {
                SHEET_SAVE => self.save_sheet(),
                SHEET_CANCEL => self.sheet = None,
                SHEET_DELETE => {
                    if let Some(i) = self.sheet.as_ref().and_then(|s| s.id) {
                        self.alarms.remove(i);
                        self.save_alarms();
                    }
                    self.sheet = None;
                }
                SHEET_TIME | SHEET_LABEL => {
                    if let Some(s) = &mut self.sheet {
                        s.focus = (id - SHEET_TIME) as u8;
                    }
                }
                d if (SHEET_DAY..SHEET_DAY + 7).contains(&d) => {
                    if let Some(s) = &mut self.sheet {
                        s.days ^= 1 << (d - SHEET_DAY);
                    }
                }
                _ => {}
            }
            return;
        }
        if self.custom.is_some() {
            match id {
                C_CANCEL => self.custom = None,
                C_START => self.finish_custom(),
                _ => {}
            }
            return;
        }
        match id {
            t if (TAB..TAB + 4).contains(&t) => {
                self.tab =
                    [Tab::Clock, Tab::Alarms, Tab::Stopwatch, Tab::Timer][(t - TAB) as usize];
                self.scroll = 0;
                self.t_focus = false;
            }
            NEW_ALARM => self.open_sheet(None),
            r if (ROW..ROW + 500).contains(&r) => {
                let order = self.sorted();
                if let Some(&i) = order.get((r - ROW) as usize) {
                    self.open_sheet(Some(self.alarms.list[i].id));
                }
            }
            s if (SWITCH..SWITCH + 500).contains(&s) => {
                let order = self.sorted();
                if let Some(&i) = order.get((s - SWITCH) as usize) {
                    let a = &mut self.alarms.list[i];
                    a.enabled = !a.enabled;
                    self.save_alarms();
                }
            }
            SW_START => {
                let now = timeio::mono_ms();
                if self.sw.running() {
                    self.sw.stop(now);
                } else {
                    self.sw.start(now);
                }
            }
            SW_LAP => self.sw.lap(timeio::mono_ms()),
            SW_RESET => self.sw.reset(),
            T_LABEL => self.t_focus = true,
            T_START => {
                let label = self.t_label.text.clone();
                self.start_timer(self.chosen_ms, &label);
                self.t_label.set("");
            }
            T_PAUSE => {
                let now = timeio::mono_ms();
                if let Some(i) = self.current_timer() {
                    let t = &mut self.timers.list[i];
                    if t.running {
                        t.pause(now);
                    } else {
                        t.resume(now);
                    }
                    self.save_timers();
                }
            }
            T_RESET => {
                if let Some(i) = self.current_timer() {
                    let id = self.timers.list[i].id;
                    self.timers.remove(id);
                    self.save_timers();
                }
            }
            T_PLUS => {
                if let Some(i) = self.current_timer() {
                    // Running: the deadline moves; paused: the time left grows. Either way mark_ms.
                    let t = &mut self.timers.list[i];
                    t.total_ms += 60_000;
                    t.mark_ms += 60_000;
                    self.save_timers();
                }
            }
            T_CUSTOM => {
                self.custom = Some(CustomSheet {
                    mins: Field::numeric("", 4),
                })
            }
            p if (T_PRESET..T_PRESET + 12).contains(&p) => {
                let list = self.preset_list();
                if let Some(&m) = list.get((p - T_PRESET) as usize) {
                    self.chosen_ms = m as u64 * 60_000;
                }
            }
            r if r >= T_OTHER_RM => {
                self.timers.remove(r - T_OTHER_RM);
                self.save_timers();
            }
            _ => {}
        }
    }

    fn preset_list(&self) -> Vec<u32> {
        let mut v: Vec<u32> = PRESETS_MIN.to_vec();
        v.extend(self.recent.iter().copied());
        v
    }

    fn finish_custom(&mut self) {
        let Some(c) = self.custom.take() else { return };
        let Ok(m) = c.mins.text.parse::<u32>() else {
            return;
        };
        if (1..=1440).contains(&m) {
            remember(&mut self.recent, m);
            let _ = timeio::save_recent(&self.paths, &self.recent);
            self.chosen_ms = m as u64 * 60_000;
        }
    }

    fn key(&mut self, k: Key, m: gui::Mods) {
        if let Some(s) = &mut self.sheet {
            match k {
                Key::Escape => self.sheet = None,
                Key::Enter => self.save_sheet(),
                Key::Tab => s.focus = 1 - s.focus,
                _ => {
                    s.error = false;
                    let f = if s.focus == 0 {
                        &mut s.time
                    } else {
                        &mut s.label
                    };
                    if s.focus == 0 {
                        // Time: digits and a colon only.
                        if let Key::Char(c) = k
                            && !(c.is_ascii_digit() || c == ':')
                        {
                            return;
                        }
                    }
                    f.key(k, m);
                }
            }
            return;
        }
        if let Some(c) = &mut self.custom {
            match k {
                Key::Escape => self.custom = None,
                Key::Enter => self.finish_custom(),
                _ => {
                    c.mins.key(k, m);
                }
            }
            return;
        }
        if self.t_focus && self.tab == Tab::Timer {
            match k {
                Key::Escape | Key::Enter | Key::Tab => self.t_focus = false,
                _ => {
                    self.t_label.key(k, m);
                }
            }
            return;
        }
        // Cmd/Ctrl-1..4 switch tabs; space starts/stops the stopwatch.
        if (m.ctrl || m.meta)
            && let Key::Char(c) = k
            && let Some(d) = c.to_digit(10)
            && (1..=4).contains(&d)
        {
            self.tab = [Tab::Clock, Tab::Alarms, Tab::Stopwatch, Tab::Timer][d as usize - 1];
            return;
        }
        if self.tab == Tab::Stopwatch && k == Key::Char(' ') {
            self.click(SW_START);
        }
    }

    // ---- drawing --------------------------------------------------------------------------

    fn draw(&mut self, win: &mut Window) {
        let th = self.th;
        self.hits.clear();
        win.fill(0, 0, W, H, th.milk);
        widgets::toolbar(win, &th);
        let sel = match self.tab {
            Tab::Clock => 0,
            Tab::Alarms => 1,
            Tab::Stopwatch => 2,
            Tab::Timer => 3,
        };
        segmented(
            win,
            &th,
            &mut self.hits,
            16,
            6,
            &["Clock", "Alarms", "Stopwatch", "Timer"],
            sel,
            TAB,
        );
        match self.tab {
            Tab::Clock => self.draw_clock(win),
            Tab::Alarms => self.draw_alarms(win),
            Tab::Stopwatch => self.draw_stopwatch(win),
            Tab::Timer => self.draw_timer(win),
        }
        if self.sheet.is_some() {
            self.dim(win);
            self.draw_alarm_sheet(win);
        } else if self.custom.is_some() {
            self.dim(win);
            self.draw_custom_sheet(win);
        }
    }

    fn dim(&mut self, win: &mut Window) {
        self.hits.modal();
        let ink = self.th.ink;
        for p in win.pixels() {
            *p = blend(*p, ink, 0x66);
        }
    }

    fn draw_clock(&mut self, win: &mut Window) {
        let th = self.th;
        let (date, sod) = now_parts();
        let (cx, cy, r) = (W / 2, 190, 110);
        // Face: a 3px ring, 12 ticks, hands.
        shapes::ring(win, cx, cy, r - 3, r, None, th.espresso);
        for i in 0..12 {
            let major = i % 3 == 0;
            let (x0, y0) = shapes::on_circle(cx, cy, (r - 8) as f64, i as f64 / 12.0);
            let (x1, y1) = shapes::on_circle(
                cx,
                cy,
                (r - if major { 22 } else { 15 }) as f64,
                i as f64 / 12.0,
            );
            shapes::line(
                win,
                x0,
                y0,
                x1,
                y1,
                if major { 3 } else { 2 },
                if major { th.espresso } else { th.cocoa },
            );
        }
        let (h, m, s) = (sod / 3600, sod / 60 % 60, sod % 60);
        let hour_turn = ((h % 12) as f64 * 60.0 + m as f64) / 720.0;
        let min_turn = (m as f64 * 60.0 + s as f64) / 3600.0;
        let sec_turn = s as f64 / 60.0;
        let (hx, hy) = shapes::on_circle(cx, cy, 58.0, hour_turn);
        shapes::line(win, cx, cy, hx, hy, 4, th.espresso);
        let (mx, my) = shapes::on_circle(cx, cy, 86.0, min_turn);
        shapes::line(win, cx, cy, mx, my, 3, th.espresso);
        let (sx, sy) = shapes::on_circle(cx, cy, 94.0, sec_turn);
        let (tx, ty) = shapes::on_circle(cx, cy, -18.0, sec_turn);
        shapes::line(win, tx, ty, sx, sy, 2, th.straw);
        win.disc(cx, cy, 9, th.milk);
        win.disc(cx, cy, 6, th.straw);
        // The time at 4x, then the date.
        let t = format!("{h:02}:{m:02}");
        bold_c(win, cx, 330, &t, th.espresso, 4);
        let wd = WEEKDAYS[date.weekday() as usize];
        let ds = format!("{wd} {} {} {}", date.d, MONTHS[date.m as usize - 1], date.y);
        text_c(win, cx, 410, &ds, th.cocoa, 1);
        text_c(
            win,
            cx,
            436,
            "Local time. World clocks need a time zone setting.",
            th.cocoa,
            1,
        );
    }

    fn draw_alarms(&mut self, win: &mut Window) {
        let th = self.th;
        bold_x(win, 16, 56, "Alarms", th.espresso, 2);
        let order = self.sorted();
        if !order.is_empty() {
            let bw = button_w("New alarm", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(W - 16 - bw, 58, bw, 24),
                "New alarm",
                Kind::Primary,
                NEW_ALARM,
                true,
                true,
            );
        }
        let now = timeio::now_secs();
        if order.is_empty() {
            text_c(win, W / 2, 230, "No alarms yet.", th.cocoa, 1);
            text_c(
                win,
                W / 2,
                256,
                "An alarm keeps working with this window closed.",
                th.cocoa,
                1,
            );
            let bw = button_w("New alarm", false);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(W / 2 - bw / 2, 290, bw, 28),
                "New alarm",
                Kind::Primary,
                NEW_ALARM,
                true,
                false,
            );
        }
        let top = 100;
        let area_h = H - 24 - top;
        let max_scroll = (order.len() as i32 * 80 - area_h).max(0);
        self.scroll = self.scroll.min(max_scroll);
        for (n, &i) in order.iter().enumerate() {
            let y = top + n as i32 * 80 - self.scroll;
            if y + 72 < top || y > H - 24 {
                continue;
            }
            let a = self.alarms.list[i].clone();
            let row = Rect::new(16, y, W - 32, 72);
            widgets::bordered(win, row, 14, th.cream, th.sand);
            let (tc, lc, dc) = if a.enabled {
                (th.espresso, th.espresso, th.cocoa)
            } else {
                (
                    fade(th.cream, th.espresso, 70),
                    fade(th.cream, th.espresso, 70),
                    fade(th.cream, th.cocoa, 70),
                )
            };
            win.text_x(32, y + 12, &fmt_hm(a.at), tc, 3);
            let label = if a.label.is_empty() {
                String::from("Alarm")
            } else {
                a.label.clone()
            };
            bold_x(win, 176, y + 14, &widgets::fit(&label, 250), lc, 1);
            win.text(176, y + 38, &fmt_days(a.days), dc);
            self.hits.add(row, ROW + n as u32);
            switch(
                win,
                &th,
                &mut self.hits,
                W - 16 - 16 - 44,
                y + 24,
                a.enabled,
                SWITCH + n as u32,
            );
        }
        let next = match self.alarms.next(now) {
            Some((_, t)) => {
                let (d, sod) = split_secs(t);
                let (today, _) = split_secs(now);
                let day = if d == today {
                    String::from("today")
                } else if d == today.add_days(1) {
                    String::from("tomorrow")
                } else {
                    format!("{} {}", &WEEKDAYS[d.weekday() as usize][..3], d.fmt())
                };
                format!(
                    "Next: {} {day}, {}",
                    fmt_hm((sod / 60) as u16),
                    fmt_in(t - now)
                )
            }
            None => String::from("No alarm is on"),
        };
        widgets::status_bar(win, &th, &next);
    }

    fn draw_stopwatch(&mut self, win: &mut Window) {
        let th = self.th;
        let now = timeio::mono_ms();
        let e = self.sw.elapsed_ms(now);
        bold_c(win, W / 2, 70, &fmt_stopwatch(e), th.espresso, 4);
        let y = 160;
        let running = self.sw.running();
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(W / 2 - 150, y, 88, 28),
            "Lap",
            Kind::Secondary,
            SW_LAP,
            running,
            false,
        );
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(W / 2 - 44, y, 88, 28),
            if running { "Stop" } else { "Start" },
            if running { Kind::Danger } else { Kind::Primary },
            SW_START,
            true,
            false,
        );
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(W / 2 + 62, y, 88, 28),
            "Reset",
            Kind::Secondary,
            SW_RESET,
            !running && e > 0,
            false,
        );
        // Lap table, newest first.
        let top = 220;
        win.text(40, top, "LAP", th.cocoa);
        text_r(win, 340, top, "LAP TIME", th.cocoa, 1);
        text_r(win, W - 40, top, "TOTAL", th.cocoa, 1);
        widgets::hline(win, 24, top + 22, W - 48, th.sand);
        let times = self.sw.lap_times();
        let (fast, slow) = if times.len() >= 2 {
            (
                times
                    .iter()
                    .enumerate()
                    .min_by_key(|&(_, t)| *t)
                    .map(|(i, _)| i),
                times
                    .iter()
                    .enumerate()
                    .max_by_key(|&(_, t)| *t)
                    .map(|(i, _)| i),
            )
        } else {
            (None, None)
        };
        if times.is_empty() {
            text_c(win, W / 2, top + 60, "Laps appear here.", th.cocoa, 1);
        }
        let rows = ((H - 24 - (top + 28)) / 28) as usize;
        for (n, i) in (0..times.len()).rev().take(rows).enumerate() {
            let y = top + 30 + n as i32 * 28;
            let col = if Some(i) == fast {
                th.good
            } else if Some(i) == slow {
                th.danger
            } else {
                th.espresso
            };
            win.text(40, y, &format!("{}", i + 1), th.espresso);
            // The arrow (not only the colour) marks the fastest and slowest lap.
            if Some(i) == fast {
                triangle(win, 76, y + 3, true, col);
            } else if Some(i) == slow {
                triangle(win, 76, y + 3, false, col);
            }
            text_r(win, 340, y, &fmt_stopwatch(times[i]), col, 1);
            text_r(
                win,
                W - 40,
                y,
                &fmt_stopwatch(self.sw.laps[i]),
                th.espresso,
                1,
            );
        }
        widgets::status_bar(
            win,
            &th,
            if running {
                "Running"
            } else if e > 0 {
                "Stopped"
            } else {
                "Press Start"
            },
        );
    }

    fn draw_timer(&mut self, win: &mut Window) {
        let th = self.th;
        let now = timeio::mono_ms();
        let cur = self.current_timer();
        let (cx, cy) = (W / 2, 190);
        shapes::ring(win, cx, cy, 122, 130, None, th.sand);
        let (remaining, total, label, running) = match cur {
            Some(i) => {
                let t = &self.timers.list[i];
                (t.remaining_ms(now), t.total_ms, t.label.clone(), t.running)
            }
            None => (self.chosen_ms, self.chosen_ms, String::new(), false),
        };
        let frac = if total == 0 {
            0.0
        } else {
            remaining as f64 / total as f64
        };
        if cur.is_some() {
            shapes::ring(win, cx, cy, 122, 130, Some(frac), th.straw);
        } else {
            shapes::ring(win, cx, cy, 122, 130, Some(1.0), th.straw);
        }
        bold_c(win, cx, cy - 32, &fmt_countdown(remaining), th.espresso, 4);
        if cur.is_some() {
            if !label.is_empty() {
                text_c(win, cx, cy - 62, &widgets::fit(&label, 200), th.cocoa, 1);
            }
            let ends = if running {
                let (_, sod) = split_secs(timeio::now_secs() + remaining.div_ceil(1000) as i64);
                format!("ends at {}", fmt_hm((sod / 60) as u16))
            } else {
                String::from("paused")
            };
            text_c(win, cx, cy + 46, &ends, th.cocoa, 1);
        } else {
            let r = Rect::new(cx - 80, cy + 36, 160, 28);
            field(
                win,
                &th,
                &mut self.hits,
                r,
                &self.t_label,
                "Label (optional)",
                self.t_focus,
                timeio::mono_ms() / 500 % 2 == 0,
                T_LABEL,
            );
        }
        // Buttons.
        let y = 352;
        if cur.is_some() {
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(cx - 150, y, 88, 28),
                "Reset",
                Kind::Secondary,
                T_RESET,
                true,
                false,
            );
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(cx - 44, y, 88, 28),
                if running { "Pause" } else { "Resume" },
                Kind::Primary,
                T_PAUSE,
                true,
                false,
            );
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(cx + 62, y, 88, 28),
                "+1 min",
                Kind::Secondary,
                T_PLUS,
                true,
                false,
            );
        } else {
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(cx - 44, y, 88, 28),
                "Start",
                Kind::Primary,
                T_START,
                true,
                false,
            );
        }
        // Presets.
        win.text(40, 408, "PRESETS", th.cocoa);
        let list = self.preset_list();
        let mut x = 40;
        let mut yy = 432;
        for (i, m) in list.iter().enumerate() {
            let l = format!("{m} min");
            let w = str_width(&l, 1) + 24;
            if x + w > W - 40 {
                x = 40;
                yy += 32;
            }
            let on = cur.is_none() && self.chosen_ms == *m as u64 * 60_000;
            chip(win, &th, &mut self.hits, x, yy, &l, on, T_PRESET + i as u32);
            x += w + 8;
        }
        if x + str_width("Custom", 1) + 24 > W - 40 {
            x = 40;
            yy += 32;
        }
        chip(win, &th, &mut self.hits, x, yy, "Custom", false, T_CUSTOM);
        // Other timers (started from the shell or earlier).
        let others: Vec<(u32, String, u64, bool)> = self
            .timers
            .list
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != cur)
            .map(|(_, t)| (t.id, t.label.clone(), t.remaining_ms(now), t.running))
            .collect();
        let mut oy = yy + 40;
        for (id, l, rem, run) in others.iter().take(2) {
            if oy + 20 > H - 24 {
                break;
            }
            let name = if l.is_empty() {
                String::from("Timer")
            } else {
                l.clone()
            };
            let s = format!(
                "{} {}{}",
                widgets::fit(&name, 200),
                fmt_countdown(*rem),
                if *run { "" } else { " (paused)" }
            );
            win.text(40, oy, &s, th.cocoa);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(W - 40 - 64, oy - 4, 64, 24),
                "Stop",
                Kind::Secondary,
                T_OTHER_RM + id,
                true,
                true,
            );
            oy += 28;
        }
        widgets::status_bar(
            win,
            &th,
            match (cur.is_some(), running) {
                (true, true) => "Rings even when this window is closed",
                (true, false) => "Paused",
                _ => "Pick a time and press Start",
            },
        );
    }

    fn sheet_frame(&mut self, win: &mut Window, w: i32, h: i32, title: &str) -> Rect {
        let th = self.th;
        let r = Rect::new((W - w) / 2, (H - h) / 2, w, h);
        win.rrect(
            r.x - 1,
            r.y + 6,
            r.w + 2,
            r.h + 2,
            16,
            blend(th.milk, th.ink, 0x33),
        );
        widgets::bordered(win, r, 16, th.milk, th.sand);
        bold_x(win, r.x + 16, r.y + 16, title, th.espresso, 2);
        r
    }

    fn draw_alarm_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(s) = self.sheet.take() else { return };
        let editing = s.id.is_some();
        let r = self.sheet_frame(
            win,
            400,
            340,
            if editing { "Edit alarm" } else { "New alarm" },
        );
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        win.text(r.x + 16, r.y + 64, "TIME (24 hour)", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.x + 16, r.y + 86, 120, 28),
            &s.time,
            "07:00",
            s.focus == 0,
            caret,
            SHEET_TIME,
        );
        if s.error {
            win.text(
                r.x + 148,
                r.y + 92,
                "Use hours:minutes, like 07:30",
                th.danger,
            );
        }
        win.text(r.x + 16, r.y + 132, "REPEAT", th.cocoa);
        let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let mut x = r.x + 16;
        for (i, n) in names.iter().enumerate() {
            let on = s.days & (1 << i) != 0;
            let w = chip(
                win,
                &th,
                &mut self.hits,
                x,
                r.y + 154,
                n,
                on,
                SHEET_DAY + i as u32,
            );
            x += w + 4;
        }
        text_c(
            win,
            r.x + r.w / 2,
            r.y + 186,
            &format!("{} (no day picked means once)", fmt_days(s.days)),
            th.cocoa,
            1,
        );
        win.text(r.x + 16, r.y + 214, "LABEL", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.x + 16, r.y + 236, r.w - 32, 28),
            &s.label,
            "Alarm",
            s.focus == 1,
            caret,
            SHEET_LABEL,
        );
        let by = r.y + r.h - 44;
        let mut bx = r.right() - 16;
        let sw = button_w("Save", false);
        bx -= sw;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(bx, by, sw, 28),
            "Save",
            Kind::Primary,
            SHEET_SAVE,
            true,
            false,
        );
        let cw = button_w("Cancel", false);
        bx -= cw + 8;
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(bx, by, cw, 28),
            "Cancel",
            Kind::Secondary,
            SHEET_CANCEL,
            true,
            false,
        );
        if editing {
            let dw = button_w("Delete", false);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(r.x + 16, by, dw, 28),
                "Delete",
                Kind::Danger,
                SHEET_DELETE,
                true,
                false,
            );
        }
        self.sheet = Some(s);
    }

    fn draw_custom_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(c) = self.custom.take() else { return };
        let r = self.sheet_frame(win, 340, 190, "Custom timer");
        win.text(r.x + 16, r.y + 60, "MINUTES", th.cocoa);
        field(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.x + 16, r.y + 82, 120, 28),
            &c.mins,
            "20",
            true,
            timeio::mono_ms() / 500 % 2 == 0,
            C_MINS,
        );
        let by = r.y + r.h - 44;
        let sw = button_w("Use", false);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.right() - 16 - sw, by, sw, 28),
            "Use",
            Kind::Primary,
            C_START,
            true,
            false,
        );
        let cw = button_w("Cancel", false);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.right() - 16 - sw - 8 - cw, by, cw, 28),
            "Cancel",
            Kind::Secondary,
            C_CANCEL,
            true,
            false,
        );
        self.custom = Some(c);
    }
}

/// A small triangle, pointing up (fastest) or down (slowest), 10px wide.
fn triangle(win: &mut Window, x: i32, y: i32, up: bool, c: u32) {
    for row in 0..9 {
        let half = if up { row / 2 } else { (8 - row) / 2 };
        win.fill(x + 4 - half, y + row, 2 * half + 1, 1, c);
    }
}

/// `clock ring LABEL HH:MM`: the panel alarmd opens when an alarm rings.
fn ring_panel(args: &rt::Args) -> i32 {
    let label = args.get(2).unwrap_or("");
    let time = args.get(3).unwrap_or("");
    let paths = timeio::Paths::from_args(args);
    let Ok(mut win) = Window::new("Alarm", 360, 240) else {
        return 1;
    };
    let th = Theme::current();
    let mut hits = Hits::default();
    let started = timeio::mono_ms();
    let mut last_chime = 0;
    loop {
        let now = timeio::mono_ms();
        // The alarm sound on a 2 second loop, for at most five minutes.
        if now - last_chime >= 2000 {
            timeio::chime();
            last_chime = now;
        }
        timeio::reap_children();
        if now - started > 300_000 {
            break;
        }
        win.fill(0, 0, 360, 240, th.milk);
        text_c_big(&mut win, &th, time, label);
        hits.clear();
        let snooze = "Snooze 9 min";
        let sw = button_w(snooze, false);
        button(
            &mut win,
            &th,
            &mut hits,
            Rect::new(360 / 2 - sw - 6, 184, sw, 28),
            snooze,
            Kind::Secondary,
            1,
            true,
            false,
        );
        button(
            &mut win,
            &th,
            &mut hits,
            Rect::new(360 / 2 + 6, 184, 88, 28),
            "Stop",
            Kind::Primary,
            2,
            true,
            false,
        );
        win.commit();
        match win.event(250) {
            Some(Event::Close) => break,
            Some(Event::Key(Key::Escape, _)) | Some(Event::Key(Key::Enter, _)) => break,
            Some(Event::Down { x, y, button: 1 }) => match hits.at(x, y) {
                Some(1) => {
                    let (mut alarms, _) = timeio::load_alarms(&paths);
                    alarms.snooze(label, timeio::now_secs(), 9);
                    let _ = timeio::save_alarms(&paths, &alarms);
                    break;
                }
                Some(2) => break,
                _ => {}
            },
            _ => {}
        }
    }
    0
}

fn text_c_big(win: &mut Window, th: &Theme, time: &str, label: &str) {
    bold_c(
        win,
        180,
        40,
        if time.is_empty() { "Alarm" } else { time },
        th.espresso,
        4,
    );
    let l = if label.is_empty() { "Alarm" } else { label };
    bold_c(win, 180, 130, &widgets::fit(l, 320), th.espresso, 1);
}
