//! The four views and the search results.

use crate::{App, DAY, EVENT, MORE, SLOT, YEAR_DAY};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, bold_c, bold_x, text_c, text_r};
use appkit::{Rect, Theme};
use gui::Window;
use timekit::Date;
use timekit::cal::Event;
use timekit::date::{MONTHS, WEEKDAYS, month_grid};
use timekit::text::fmt_hm;

const CHIP_H: i32 = 18;

impl App {
    /// An event chip: a 3px bar in the calendar's colour, the colour at 27% as fill, the title.
    fn chip(&mut self, win: &mut Window, r: Rect, cal: usize, e: &Event, under: u32, time: bool) {
        let th = self.th;
        let acc = th.accent(&self.set.cals[cal].colour);
        win.rrect(r.x, r.y, r.w, r.h, 4, Theme::tint(under, acc, 0x44));
        win.fill(r.x, r.y + 2, 3, r.h - 4, acc);
        let t = if time && let Some(s) = e.start {
            format!("{} {}", fmt_hm(s), e.title)
        } else {
            e.title.clone()
        };
        win.text(r.x + 7, r.y + 1, &widgets::fit(&t, r.w - 10), th.espresso);
        self.hits.add(r, EVENT + e.id);
    }

    pub fn draw_month(&mut self, win: &mut Window) {
        let th = self.th;
        let b = App::body();
        let c = self.cursor;
        let first = Date {
            y: c.y,
            m: c.m,
            d: 1,
        };
        let grid = month_grid(c.y, c.m, true);
        let rows = grid.iter().filter(|r| r.iter().any(|&d| d != 0)).count() as i32;
        let head = 24;
        let cell_h = (b.h - head) / rows;
        let start = first.add_days(-(first.weekday() as i64));
        for col in 0..7 {
            let x0 = b.x + col * b.w / 7;
            let x1 = b.x + (col + 1) * b.w / 7;
            text_c(
                win,
                (x0 + x1) / 2,
                b.y + 4,
                &WEEKDAYS[col as usize][..3],
                th.cocoa,
                1,
            );
        }
        for r in 0..rows {
            for col in 0..7 {
                let date = start.add_days((r * 7 + col) as i64);
                let x0 = b.x + col * b.w / 7;
                let x1 = b.x + (col + 1) * b.w / 7;
                let y = b.y + head + r * cell_h;
                let cell = Rect::new(x0, y, x1 - x0, cell_h);
                let outside = date.m != c.m;
                let under = if outside { blend_oat(&th) } else { th.milk };
                win.fill(cell.x, cell.y, cell.w, cell.h, under);
                if date == self.cursor {
                    win.fill(
                        cell.x,
                        cell.y,
                        cell.w,
                        cell.h,
                        Theme::tint(under, th.straw, 0x22),
                    );
                }
                widgets::hline(win, cell.x, cell.y, cell.w, th.sand);
                widgets::vline(win, cell.x, cell.y, cell.h, th.sand);
                self.hits.add(cell, DAY + date.days() as u32);
                // The day number; today is a straw circle with an ink numeral.
                let n = format!("{}", date.d);
                let nx = cell.x + 8;
                if date == self.today {
                    win.disc(nx + 4 * n.len() as i32, cell.y + 14, 12, th.straw);
                    win.text(nx, cell.y + 6, &n, th.ink);
                } else {
                    let col_t = if outside { th.latte } else { th.espresso };
                    win.text(nx, cell.y + 6, &n, col_t);
                }
                // Events: as many chips as fit, then "+N more".
                let evs = self.event_list(date);
                let room = ((cell.h - 30) / (CHIP_H + 1)).max(0) as usize;
                let shown = if evs.len() > room {
                    room.saturating_sub(1)
                } else {
                    evs.len()
                };
                for (i, (ci, e)) in evs.iter().take(shown).enumerate() {
                    let cr = Rect::new(
                        cell.x + 3,
                        cell.y + 28 + i as i32 * (CHIP_H + 1),
                        cell.w - 6,
                        CHIP_H,
                    );
                    self.chip(win, cr, *ci, e, under, false);
                }
                if evs.len() > shown {
                    let y = cell.y + 28 + shown as i32 * (CHIP_H + 1);
                    let mr = Rect::new(cell.x + 3, y, cell.w - 6, CHIP_H);
                    win.text(
                        mr.x + 4,
                        mr.y + 1,
                        &format!("+{} more", evs.len() - shown),
                        th.cocoa,
                    );
                    self.hits.add(mr, MORE + date.days() as u32);
                }
            }
        }
        widgets::hline(win, b.x, b.y + head + rows * cell_h, b.w, th.sand);
    }

    /// The week (7 columns) or the day (1 column) with hour rows.
    pub fn draw_week(&mut self, win: &mut Window, cols: i32) {
        let th = self.th;
        let b = App::body();
        let gutter = 56;
        let c = self.cursor;
        let first = if cols == 7 {
            c.add_days(-(c.weekday() as i64))
        } else {
            c
        };
        let col_w = (b.w - gutter) / cols;
        let col_x = |i: i32| b.x + gutter + i * col_w;
        // Header and the all-day strip, measured first so the grid knows where it starts.
        let head = 44;
        let mut all_day_rows = 0;
        for i in 0..cols {
            let n = self
                .event_list(first.add_days(i as i64))
                .iter()
                .filter(|(_, e)| e.start.is_none())
                .count();
            all_day_rows = all_day_rows.max(n.min(2));
        }
        let strip = if all_day_rows > 0 {
            all_day_rows as i32 * (CHIP_H + 2) + 4
        } else {
            0
        };
        let top = b.y + head + strip;
        let grid_h = b.bottom() - top;
        let max_scroll = (24 * 48 - grid_h).max(0);
        self.hour_scroll = self.hour_scroll.clamp(0, max_scroll);
        let scroll = self.hour_scroll;
        // Hour rows and slots.
        for h in 0..24 {
            let y = top + h * 48 - scroll;
            if y + 48 < top || y > b.bottom() {
                continue;
            }
            widgets::hline(win, b.x + gutter, y.max(top), b.w - gutter, th.sand);
            if y - 8 >= top {
                text_r(
                    win,
                    b.x + gutter - 8,
                    y - 8,
                    &format!("{h:02}:00"),
                    th.cocoa,
                    1,
                );
            }
            for i in 0..cols {
                let date = first.add_days(i as i64);
                let r = Rect::new(
                    col_x(i),
                    y.max(top),
                    col_w,
                    (48 - (top - y).max(0)).min(b.bottom() - y.max(top)),
                );
                if r.h > 0 {
                    self.hits.add(r, SLOT + date.days() as u32 * 24 + h as u32);
                }
            }
        }
        for i in 0..=cols {
            widgets::vline(win, col_x(i).min(b.right() - 1), top, grid_h, th.sand);
        }
        // Timed events, side by side when they overlap.
        for i in 0..cols {
            let date = first.add_days(i as i64);
            let evs: Vec<(usize, Event)> = self
                .event_list(date)
                .into_iter()
                .filter(|(_, e)| e.start.is_some())
                .collect();
            let mut lane_end: Vec<u16> = Vec::new();
            let mut placed: Vec<(usize, usize)> = Vec::new(); // (index, lane)
            for (n, (_, e)) in evs.iter().enumerate() {
                let (s, en) = span(e);
                let lane = lane_end
                    .iter()
                    .position(|&end| end <= s)
                    .unwrap_or_else(|| {
                        lane_end.push(0);
                        lane_end.len() - 1
                    });
                lane_end[lane] = en;
                placed.push((n, lane));
            }
            let lanes = lane_end.len().max(1) as i32;
            for (n, lane) in placed {
                let (ci, e) = &evs[n];
                let (s, en) = span(e);
                let y = top + s as i32 * 48 / 60 - scroll;
                let h = ((en - s) as i32 * 48 / 60).max(CHIP_H + 2);
                if y + h < top || y > b.bottom() {
                    continue;
                }
                let lw = (col_w - 4) / lanes;
                let (cy0, cy1) = (y.max(top), (y + h).min(b.bottom()));
                let r = Rect::new(col_x(i) + 2 + lane as i32 * lw, cy0, lw - 1, cy1 - cy0);
                let acc = th.accent(&self.set.cals[*ci].colour);
                win.rrect(r.x, r.y, r.w, r.h, 5, Theme::tint(th.milk, acc, 0x66));
                win.fill(r.x, r.y + 2, 3, r.h - 4, acc);
                if r.h >= CHIP_H {
                    win.text(
                        r.x + 7,
                        r.y + 1,
                        &widgets::fit(&e.title, r.w - 10),
                        th.espresso,
                    );
                }
                if r.h >= 2 * CHIP_H {
                    win.text(
                        r.x + 7,
                        r.y + 1 + CHIP_H,
                        &widgets::fit(&fmt_hm(e.start.unwrap_or(0)), r.w - 10),
                        th.cocoa,
                    );
                }
                self.hits.add(r, EVENT + e.id);
            }
            // The now line on today's column.
            if date == self.today {
                let (_, sod) = timekit::date::split_secs(timeio::now_secs());
                let y = top + (sod / 60) as i32 * 48 / 60 - scroll;
                if y >= top && y < b.bottom() {
                    win.fill(col_x(i), y - 1, col_w, 2, th.straw);
                    win.disc(col_x(i) + 4, y, 5, th.straw);
                }
            }
        }
        // The header on top of whatever scrolled under it.
        win.fill(b.x, b.y, b.w, head + strip, th.milk);
        widgets::hline(win, b.x, top - 1, b.w, th.sand);
        for i in 0..cols {
            let date = first.add_days(i as i64);
            let cx = col_x(i) + col_w / 2;
            let label = format!("{} {}", &WEEKDAYS[date.weekday() as usize][..3], date.d);
            let hr = Rect::new(col_x(i), b.y, col_w, head);
            if date == self.cursor && cols > 1 {
                win.fill(
                    hr.x + 1,
                    hr.y,
                    hr.w - 1,
                    hr.h,
                    Theme::tint(th.milk, th.straw, 0x22),
                );
            }
            if date == self.today {
                bold_c(win, cx, b.y + 8, &label, th.espresso, 1);
                win.fill(cx - 40, b.y + 30, 80, 3, th.straw);
            } else {
                text_c(win, cx, b.y + 8, &label, th.cocoa, 1);
            }
            self.hits.add(hr, DAY + date.days() as u32);
            // All-day chips.
            let all: Vec<(usize, Event)> = self
                .event_list(date)
                .into_iter()
                .filter(|(_, e)| e.start.is_none())
                .collect();
            for (k, (ci, e)) in all.iter().take(2).enumerate() {
                let r = Rect::new(
                    col_x(i) + 2,
                    b.y + head + 2 + k as i32 * (CHIP_H + 2),
                    col_w - 4,
                    CHIP_H,
                );
                self.chip(win, r, *ci, e, th.milk, false);
            }
        }
        if strip > 0 {
            text_r(
                win,
                b.x + gutter - 8,
                b.y + head + 4,
                "all day",
                th.cocoa,
                1,
            );
        }
    }

    pub fn draw_year(&mut self, win: &mut Window) {
        let th = self.th;
        let b = App::body();
        let year = self.cursor.y;
        let (cw, ch) = (b.w / 4, b.h / 3);
        for m in 0..12u8 {
            let (col, row) = ((m % 4) as i32, (m / 4) as i32);
            let (x0, y0) = (b.x + col * cw, b.y + row * ch);
            let month = m + 1;
            bold_x(win, x0 + 16, y0 + 8, MONTHS[m as usize], th.espresso, 1);
            let cell = (cw - 32) / 7;
            for (i, l) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
                text_c(
                    win,
                    x0 + 16 + i as i32 * cell + cell / 2,
                    y0 + 30,
                    l,
                    th.cocoa,
                    1,
                );
            }
            let counts = self.set.month_counts(year, month);
            for (r, rowd) in month_grid(year, month, true).iter().enumerate() {
                for (cc, &d) in rowd.iter().enumerate() {
                    if d == 0 {
                        continue;
                    }
                    let date = Date {
                        y: year,
                        m: month,
                        d,
                    };
                    let (x, y) = (x0 + 16 + cc as i32 * cell, y0 + 50 + r as i32 * 20);
                    let rect = Rect::new(x, y, cell, 20);
                    if date == self.today {
                        win.disc(x + cell / 2, y + 10, 10, th.straw);
                    } else if date == self.cursor {
                        win.rrect(
                            x + 1,
                            y,
                            cell - 2,
                            20,
                            9,
                            Theme::tint(th.milk, th.straw, 0x66),
                        );
                    }
                    let col_t = if date == self.today {
                        th.ink
                    } else {
                        th.espresso
                    };
                    text_c(win, x + cell / 2, y + 2, &format!("{d}"), col_t, 1);
                    if counts[d as usize - 1] > 0 && date != self.today {
                        win.fill(x + cell / 2 - 2, y + 18, 4, 3, th.straw);
                    }
                    self.hits.add(rect, YEAR_DAY + date.days() as u32);
                }
            }
        }
    }

    pub fn draw_search(&mut self, win: &mut Window) {
        let th = self.th;
        let b = App::body();
        let q: String = self.search.text.to_lowercase();
        bold_x(
            win,
            b.x + 16,
            b.y + 12,
            &format!("Results for \"{}\"", self.search.text),
            th.espresso,
            1,
        );
        let mut found: Vec<(Date, usize, Event)> = Vec::new();
        for (ci, c) in self.set.cals.iter().enumerate().filter(|(_, c)| !c.hidden) {
            for e in &c.events {
                if e.title.to_lowercase().contains(&q) || e.notes.to_lowercase().contains(&q) {
                    found.push((e.date, ci, e.clone()));
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.id.cmp(&b.2.id)));
        if found.is_empty() {
            text_c(
                win,
                b.x + b.w / 2,
                b.y + 120,
                "Nothing matches.",
                th.cocoa,
                1,
            );
            return;
        }
        let rows = ((b.h - 48) / 30) as usize;
        for (i, (d, ci, e)) in found.iter().take(rows).enumerate() {
            let y = b.y + 44 + i as i32 * 30;
            let r = Rect::new(b.x + 12, y, b.w - 24, 26);
            widgets::bordered(win, r, 8, th.cream, th.sand);
            win.disc(r.x + 14, y + 13, 5, th.accent(&self.set.cals[*ci].colour));
            win.text(r.x + 30, y + 5, &d.fmt(), th.cocoa);
            let when = e.start.map_or(String::from("all day"), fmt_hm);
            win.text(r.x + 130, y + 5, &when, th.cocoa);
            win.text(
                r.x + 210,
                y + 5,
                &widgets::fit(&e.title, r.w - 230),
                th.espresso,
            );
            self.hits.add(r, EVENT + e.id);
        }
    }
}

/// An event's start and end in minutes (one hour long when it has no end).
fn span(e: &Event) -> (u16, u16) {
    let s = e.start.unwrap_or(0);
    (s, e.end.unwrap_or((s + 60).min(24 * 60)).max(s + 30))
}

fn blend_oat(th: &Theme) -> u32 {
    gui::blend(th.milk, th.oat, 0x73)
}
