//! Drawing for the Text Editor: gutter, rows with selection, find highlights and colours, the
//! caret, the find bar, the status bar and sheets. Sizes and colours follow SPEC-APPS.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind};
use appkit::{Rect, Theme};
use gui::{Color, Window, rgb, str_width};
use textbuf::syntax::{self, Role};
use textbuf::view;
use textbuf::{Pos, view::VRow};

use crate::*;

fn role_color(th: &Theme, r: Role) -> Color {
    let (light, dark) = match r {
        Role::Keyword => (rgb(0x3D, 0x5A, 0x99), rgb(0xA9, 0xBB, 0xEA)),
        Role::String => (rgb(0x42, 0x70, 0x4B), rgb(0xA8, 0xCF, 0xA6)),
        Role::Number => (rgb(0x8A, 0x5A, 0x00), rgb(0xEB, 0xB6, 0x50)),
        Role::Comment => (rgb(0x6B, 0x58, 0x46), rgb(0xBE, 0xA8, 0x90)),
        Role::Marker => (rgb(0xB5, 0x46, 0x5B), rgb(0xE5, 0x8C, 0x97)),
        _ => return th.espresso,
    };
    if th.dark { dark } else { light }
}

fn current_line_fill(th: &Theme) -> Color {
    if th.dark {
        rgb(0x38, 0x2D, 0x27)
    } else {
        rgb(0xF6, 0xEF, 0xE0)
    }
}

/// Cell position of char index `k` of a row, given the running positions (past the end each
/// extra char is one cell, for the selected line break).
fn cellpos(prefix: &[usize], k: usize) -> usize {
    let n = prefix.len() - 1;
    if k <= n {
        prefix[k]
    } else {
        prefix[n] + (k - n)
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_cells(
    win: &mut Window,
    x0: i32,
    y: i32,
    hs: usize,
    cols: usize,
    a: usize,
    b: usize,
    c: Color,
) {
    let lo = a.max(hs);
    let hi = b.min(hs + cols);
    if lo < hi {
        win.fill(
            x0 + (lo - hs) as i32 * CELL_W,
            y,
            (hi - lo) as i32 * CELL_W,
            LINE_H,
            c,
        );
    }
}

impl App {
    pub fn draw(&mut self) {
        self.th = Theme::current();
        self.relayout();
        self.hits.clear();
        let th = self.th;
        let (w, h) = (self.win.w, self.win.h);
        self.win.fill(0, 0, w, h, th.milk);
        let top = self.top();
        let vis = self.vis_rows();
        let x0 = self.text_x0();
        let cols = self.cols();
        let gutter = self.gutter_w();
        let cur = self.doc.buf.cursor();
        let sel = self.doc.buf.selection();
        let (cur_row, cur_col) = view::locate(&self.rows, cur);
        let pair = view::match_bracket(&self.doc.buf, cur, 300);
        let (needle, fopts) = self
            .find
            .as_ref()
            .map(|f| (f.find.text.clone(), f.opts))
            .unwrap_or_default();

        // Gutter background first, so the rows draw over its edge only on their own side.
        if gutter > 0 {
            self.win.fill(0, top, gutter, h - top - STATUS_H, th.cream);
            self.win
                .fill(gutter - 1, top, 1, h - top - STATUS_H, th.sand);
        }
        let last = (self.scroll + vis).min(self.rows.len());
        for r in self.scroll..last {
            let row = self.rows[r];
            let y = top + (r - self.scroll) as i32 * LINE_H;
            let line = self.doc.buf.line(row.line);
            let part: String = line
                .chars()
                .skip(row.start)
                .take(row.end - row.start)
                .collect();
            let n = row.end - row.start;
            // Cell position of each char (tabs to the next stop).
            let mut prefix = Vec::with_capacity(n + 1);
            let mut d = 0usize;
            prefix.push(0);
            for c in part.chars() {
                d += if c == '\t' {
                    self.tab - d % self.tab
                } else {
                    1
                };
                prefix.push(d);
            }
            let hs = self.hscroll;
            // Current line.
            if row.line == cur.line {
                self.win
                    .fill(gutter, y, w - gutter, LINE_H, current_line_fill(&th));
            }
            // Syntax roles per char of this row.
            let mut roles: Vec<Role> = alloc::vec![Role::Text; n];
            if self.doc.lang != syntax::Lang::Plain {
                let mut st = self.states.get(row.line).copied().unwrap_or_default();
                for (a, b, role) in syntax::highlight(self.doc.lang, line, &mut st) {
                    for k in a.max(row.start)..b.min(row.end) {
                        roles[k - row.start] = role;
                    }
                }
            }
            // Inline code / fenced blocks get an oat tint.
            let mut k = 0;
            while k < n {
                if roles[k] == Role::Code {
                    let s = k;
                    while k < n && roles[k] == Role::Code {
                        k += 1;
                    }
                    fill_cells(&mut self.win, x0, y, hs, cols, prefix[s], prefix[k], th.oat);
                } else {
                    k += 1;
                }
            }
            // Find matches.
            if !needle.is_empty() {
                for (s, e) in self.doc.buf.find_in_line(row.line, &needle, fopts) {
                    let (s2, e2) = (s.max(row.start), e.min(row.end));
                    if s2 >= e2 {
                        continue;
                    }
                    let (a, b) = (prefix[s2 - row.start], prefix[e2 - row.start]);
                    let is_cur = sel == Some((Pos::new(row.line, s), Pos::new(row.line, e)));
                    if is_cur {
                        let c = Theme::tint(th.milk, th.straw, 0x66);
                        fill_cells(&mut self.win, x0, y, hs, cols, a, b, c);
                        let (lo, hi) = (a.max(hs), b.min(hs + cols));
                        if lo < hi {
                            let (rx, rw) =
                                (x0 + (lo - hs) as i32 * CELL_W, (hi - lo) as i32 * CELL_W);
                            widgets::hline(&mut self.win, rx, y, rw, th.straw);
                            widgets::hline(&mut self.win, rx, y + LINE_H - 1, rw, th.straw);
                        }
                    } else {
                        let c = Theme::tint(th.milk, th.honey, 0x66);
                        fill_cells(&mut self.win, x0, y, hs, cols, a, b, c);
                    }
                }
            }
            // Selection.
            if let Some((a, b)) = sel
                && row.line >= a.line
                && row.line <= b.line
            {
                let len = self.doc.buf.line_len(row.line);
                let lo = if row.line == a.line { a.col } else { 0 };
                let hi = if row.line == b.line { b.col } else { len + 1 };
                let is_last_row = row.end == len;
                let s = lo.max(row.start);
                let e = hi.min(row.end + usize::from(is_last_row));
                if s < e {
                    let c = Theme::tint(th.milk, th.straw, 0x54);
                    let (ca, cb) = (
                        cellpos(&prefix, s - row.start),
                        cellpos(&prefix, e - row.start),
                    );
                    fill_cells(&mut self.win, x0, y, hs, cols, ca, cb, c);
                }
            }
            // Bracket pair boxes.
            if let Some((p, q)) = pair {
                for b in [p, q] {
                    if b.line == row.line && b.col >= row.start && b.col < row.end {
                        let cx = cellpos(&prefix, b.col - row.start);
                        if cx >= hs && cx < hs + cols {
                            let rx = x0 + (cx - hs) as i32 * CELL_W;
                            widgets::hline(&mut self.win, rx, y, CELL_W, th.honey);
                            widgets::hline(&mut self.win, rx, y + LINE_H - 1, CELL_W, th.honey);
                            widgets::vline(&mut self.win, rx, y, LINE_H, th.honey);
                            widgets::vline(&mut self.win, rx + CELL_W - 1, y, LINE_H, th.honey);
                        }
                    }
                }
            }
            // Text, in runs of one role.
            let mut cells: Vec<(char, Role)> = Vec::with_capacity(prefix[n]);
            for (i, c) in part.chars().enumerate() {
                if c == '\t' {
                    cells.extend(core::iter::repeat_n(
                        (' ', roles[i]),
                        prefix[i + 1] - prefix[i],
                    ));
                } else {
                    cells.push((c, roles[i]));
                }
            }
            let mut i = hs;
            let end = (hs + cols).min(cells.len());
            while i < end {
                let role = cells[i].1;
                let mut j = i;
                let mut run = String::new();
                while j < end && cells[j].1 == role {
                    run.push(cells[j].0);
                    j += 1;
                }
                let color = role_color(&th, role);
                let x = x0 + (i - hs) as i32 * CELL_W;
                self.win.text(x, y + 1, &run, color);
                if matches!(role, Role::Heading | Role::Strong) {
                    self.win.text(x + 1, y + 1, &run, color);
                }
                i = j;
            }
            // Line number.
            if gutter > 0 && row.first {
                let num = format!("{}", row.line + 1);
                let col = if row.line == cur.line {
                    th.espresso
                } else {
                    th.cocoa
                };
                self.win
                    .text(gutter - 10 - str_width(&num, 1), y + 1, &num, col);
            }
            // Caret.
            if r == cur_row
                && self.caret_on
                && self.sheet.is_none()
                && self.find_focus_text().is_none()
            {
                let cx = cellpos(&prefix, cur_col);
                if cx >= hs && cx <= hs + cols {
                    self.win.fill(
                        x0 + (cx - hs) as i32 * CELL_W,
                        y + 1,
                        2,
                        LINE_H - 2,
                        th.espresso,
                    );
                }
            }
        }
        if self.find.is_some() || self.goto.is_some() {
            self.draw_bar();
        }
        self.draw_status();
        if self.sheet.is_some() {
            self.draw_sheet();
        }
        let title = self.window_title();
        if title != self.title {
            self.win.set_title(&title);
            self.title = title;
        }
        self.win.commit();
    }

    /// While a bar has the keys, the caret belongs to it.
    fn find_focus_text(&self) -> Option<()> {
        (self.goto.is_some() || self.find.is_some()).then_some(())
    }

    fn window_title(&self) -> String {
        let mark = if self.doc.buf.is_dirty() {
            " \u{b7}"
        } else {
            ""
        };
        format!("{}{}", self.doc.name, mark)
    }

    fn draw_bar(&mut self) {
        let th = self.th;
        let w = self.win.w;
        self.win.fill(0, 0, w, FIND_H, th.cream);
        widgets::hline(&mut self.win, 0, FIND_H, w, th.sand);
        let caret = self.caret_on;
        if let Some(g) = &self.goto {
            self.win.text(12, 12, "Go to line:", th.cocoa);
            widgets::field(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(108, 8, 110, 24),
                g,
                "",
                true,
                caret,
                ID_GOTO,
            );
            self.win
                .text(232, 12, "Return to go, Esc to cancel", th.cocoa);
            return;
        }
        let Some(f) = &self.find else { return };
        let mut x = 12;
        widgets::field(
            &mut self.win,
            &th,
            &mut self.hits,
            Rect::new(x, 8, 150, 24),
            &f.find,
            "Find",
            f.focus == 0,
            caret,
            ID_FIND,
        );
        x += 158;
        // "2 of 3".
        let total = if self.doc.buf.line_count() <= 20_000 {
            self.doc.buf.count_matches(&f.find.text, f.opts)
        } else {
            0
        };
        let label = if f.find.text.is_empty() {
            String::new()
        } else if total == 0 {
            String::from("No matches")
        } else {
            let before = self.match_index(&f.find.text, f.opts);
            format!("{} of {}", before, total)
        };
        let lw = 88;
        if w >= 620 || f.replace {
            self.win.text(x, 12, &widgets::fit(&label, lw), th.cocoa);
        }
        x += lw + 4;
        widgets::button(
            &mut self.win,
            &th,
            &mut self.hits,
            Rect::new(x, 8, 24, 24),
            "<",
            Kind::Secondary,
            ID_PREV,
            true,
            true,
        );
        x += 28;
        widgets::button(
            &mut self.win,
            &th,
            &mut self.hits,
            Rect::new(x, 8, 24, 24),
            ">",
            Kind::Secondary,
            ID_NEXT,
            true,
            true,
        );
        x += 32;
        x += widgets::chip(
            &mut self.win,
            &th,
            &mut self.hits,
            x,
            8,
            "Aa",
            f.opts.case_sensitive,
            ID_CASE,
        ) + 8;
        if f.replace {
            widgets::field(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, 8, 150, 24),
                &f.repl,
                "Replace",
                f.focus == 1,
                caret,
                ID_REPL,
            );
            x += 158;
            widgets::button(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, 8, 72, 24),
                "Replace",
                Kind::Secondary,
                ID_REPLACE,
                true,
                true,
            );
            x += 76;
            widgets::button(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, 8, 40, 24),
                "All",
                Kind::Secondary,
                ID_ALL,
                true,
                true,
            );
        }
    }

    /// Which match (1-based) the selection is, or the next one after the caret.
    fn match_index(&self, needle: &str, opts: textbuf::FindOpts) -> usize {
        let at = self
            .doc
            .buf
            .selection()
            .map_or(self.doc.buf.cursor(), |(a, _)| a);
        let mut n = 0;
        for line in 0..=at.line.min(self.doc.buf.line_count() - 1) {
            for (s, _) in self.doc.buf.find_in_line(line, needle, opts) {
                if Pos::new(line, s) < at {
                    n += 1;
                }
            }
        }
        (n + 1).min(self.doc.buf.count_matches(needle, opts))
    }

    fn draw_status(&mut self) {
        let th = self.th;
        let (w, h) = (self.win.w, self.win.h);
        let y = h - STATUS_H;
        self.win.fill(0, y, w, STATUS_H, th.cream);
        widgets::hline(&mut self.win, 0, y, w, th.sand);
        let cur = self.doc.buf.cursor();
        let d = view::display_col(self.doc.buf.line(cur.line), cur.col, self.tab);
        let eol = match self.doc.buf.eol() {
            textbuf::Eol::Lf => "LF",
            textbuf::Eol::Crlf => "CRLF",
        };
        let mut segs: Vec<(String, Option<u32>)> = alloc::vec![
            (format!("Ln {}, Col {}", cur.line + 1, d + 1), None),
            (String::from("UTF-8"), None),
            (String::from(eol), None),
            (format!("Spaces: {}", self.tab), Some(ID_ST_TAB)),
            (
                format!("Wrap: {}", if self.doc.wrap { "on" } else { "off" }),
                Some(ID_ST_WRAP)
            ),
            (String::from(self.doc.lang.name()), None),
        ];
        let right = match &self.msg {
            Some((m, _)) => m.clone(),
            None if self.doc.readonly => String::from("Read only"),
            None if self.doc.buf.is_dirty() => String::from("Edited"),
            None => String::from("Saved"),
        };
        let room = w - 24 - str_width(&right, 1);
        // Drop fields from the right while they don't fit.
        while segs.len() > 1 && segs.iter().map(|s| str_width(&s.0, 1) + 32).sum::<i32>() > room {
            segs.pop();
        }
        let mut x = 12;
        for (text, id) in &segs {
            self.win.text(x, y + 4, text, th.cocoa);
            let sw = str_width(text, 1);
            if let Some(id) = id {
                self.hits.add(Rect::new(x - 4, y, sw + 8, STATUS_H), *id);
            }
            x += sw + 32;
        }
        widgets::text_r(&mut self.win, w - 12, y + 4, &right, th.cocoa, 1);
    }

    fn draw_sheet(&mut self) {
        let th = self.th;
        self.hits.modal();
        let ink = th.ink;
        for p in self.win.pixels() {
            *p = gui::blend(*p, ink, 0x66);
        }
        let (w, h) = (self.win.w, self.win.h);
        let card_w = 400.min(w - 24);
        let (title, body, buttons): (String, String, Vec<(&str, Kind, u32)>) = match &self.sheet {
            Some(Sheet::Quit) => (
                format!("Save changes to \"{}\"?", widgets::fit(&self.doc.name, 220)),
                String::from("Your changes will be lost if you don't save them."),
                alloc::vec![
                    ("Save", Kind::Primary, ID_SAVE),
                    ("Don't Save", Kind::Danger, ID_DISCARD),
                    ("Cancel", Kind::Secondary, ID_CANCEL)
                ],
            ),
            Some(Sheet::Error(m)) => (
                String::from("Text Editor"),
                m.clone(),
                alloc::vec![("OK", Kind::Primary, ID_OK)],
            ),
            None => return,
        };
        // Wrap the body to the card.
        let per = ((card_w - 40) / CELL_W).max(8) as usize;
        let mut lines: Vec<String> = Vec::new();
        for para in body.split('\n') {
            let mut cur = String::new();
            for word in para.split(' ') {
                if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > per {
                    lines.push(core::mem::take(&mut cur));
                }
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(word);
            }
            lines.push(cur);
        }
        let card_h = 20 + 24 + lines.len() as i32 * 18 + 16 + 28 + 20;
        let r = Rect::new((w - card_w) / 2, ((h - card_h) / 2).max(8), card_w, card_h);
        widgets::bordered(&mut self.win, r, 12, th.cream, th.sand);
        widgets::bold_x(&mut self.win, r.x + 20, r.y + 20, &title, th.espresso, 1);
        for (i, l) in lines.iter().enumerate() {
            self.win
                .text(r.x + 20, r.y + 20 + 24 + i as i32 * 18, l, th.cocoa);
        }
        let mut x = r.right() - 20;
        for (label, kind, id) in buttons.iter().rev() {
            let bw = widgets::button_w(label, false);
            x -= bw;
            widgets::button(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, r.bottom() - 20 - 28, bw, 28),
                label,
                *kind,
                *id,
                true,
                false,
            );
            x -= 8;
        }
    }
}

#[allow(dead_code)]
fn _unused(_: VRow) {}
