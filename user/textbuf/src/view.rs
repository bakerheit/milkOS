//! Turning lines into screen rows: soft wrap, tab stops, and the maths between a position in the
//! text (line, column in chars) and a place on screen (row, display column). Pure functions over a
//! [`Buffer`], so the app only draws.
//!
//! A *row* (`VRow`) is a piece of one line. With wrap off there is one row per line; with wrap on,
//! a long line is cut into rows of at most `cols` chars, preferably after a space. The display
//! column of a char counts a tab as the distance to the next tab stop (`tab` wide), so tabs line
//! up; wrapping counts a tab as one char (a line made of tabs wraps late), which is the price of
//! keeping row cutting simple.

use alloc::vec::Vec;

use crate::{Buffer, Pos};

/// One screen row: chars `start..end` of line `line`. `first` is false for wrapped continuations
/// (no line number).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub first: bool,
}

/// All the rows of the buffer. `cols` is how many chars fit across (ignored when `wrap` is off).
pub fn layout(b: &Buffer, cols: usize, wrap: bool) -> Vec<VRow> {
    let cols = cols.max(1);
    let mut rows = Vec::with_capacity(b.line_count());
    for line in 0..b.line_count() {
        let s = b.line(line);
        let n = s.chars().count();
        if !wrap || n <= cols {
            rows.push(VRow {
                line,
                start: 0,
                end: n,
                first: true,
            });
            continue;
        }
        let cs: Vec<char> = s.chars().collect();
        let mut start = 0;
        while start < n {
            let mut end = (start + cols).min(n);
            if end < n {
                // Break after the last space in the second half of the row, if there is one.
                if let Some(k) = (start + cols / 2..end).rev().find(|&k| cs[k] == ' ') {
                    end = k + 1;
                }
            }
            rows.push(VRow {
                line,
                start,
                end,
                first: start == 0,
            });
            start = end;
        }
    }
    rows
}

/// The row that holds `p`, and `p`'s column within that row's chars. A position at the very end
/// of a wrapped line belongs to its last row; one at a wrap point to the row it starts.
pub fn locate(rows: &[VRow], p: Pos) -> (usize, usize) {
    // First row of the line (rows are sorted by line, then start).
    let first = rows.partition_point(|r| r.line < p.line);
    let mut i = first;
    while i < rows.len() && rows[i].line == p.line {
        let r = rows[i];
        let last = i + 1 >= rows.len() || rows[i + 1].line != p.line;
        if p.col < r.end || last {
            return (i, p.col.saturating_sub(r.start).min(r.end - r.start));
        }
        i += 1;
    }
    (first.min(rows.len().saturating_sub(1)), 0)
}

/// The text position of char column `col` (clamped) in row `row`.
pub fn pos_at(rows: &[VRow], row: usize, col: usize) -> Pos {
    let r = rows[row.min(rows.len() - 1)];
    let mut c = r.start + col.min(r.end - r.start);
    // At the end of a wrapped (non-final) row the caret belongs to the next row's start: stay
    // inside this one so a click past the text lands on its last char.
    let next_same = rows
        .get(row.min(rows.len() - 1) + 1)
        .is_some_and(|n| n.line == r.line);
    if next_same && c == r.end && r.end > r.start {
        c = r.end - 1;
    }
    Pos::new(r.line, c)
}

/// Display column of char `col` of `line`: tabs advance to the next multiple of `tab`.
pub fn display_col(line: &str, col: usize, tab: usize) -> usize {
    let tab = tab.max(1);
    let mut d = 0;
    for (i, c) in line.chars().enumerate() {
        if i >= col {
            break;
        }
        d += if c == '\t' { tab - d % tab } else { 1 };
    }
    d
}

/// The char column whose cell contains display column `dcol` (past the end: the line length).
pub fn col_at_display(line: &str, dcol: usize, tab: usize) -> usize {
    let tab = tab.max(1);
    let mut d = 0;
    for (i, c) in line.chars().enumerate() {
        let w = if c == '\t' { tab - d % tab } else { 1 };
        if dcol < d + w {
            return i;
        }
        d += w;
    }
    line.chars().count()
}

/// The chars of `line` from `start` to `end` with tabs expanded to spaces, given that the row
/// starts at display column `d0`.
pub fn expand_tabs(line: &str, start: usize, end: usize, d0: usize, tab: usize) -> Vec<char> {
    let tab = tab.max(1);
    let mut out = Vec::new();
    let mut d = d0;
    for c in line.chars().skip(start).take(end.saturating_sub(start)) {
        if c == '\t' {
            let w = tab - d % tab;
            out.extend(core::iter::repeat_n(' ', w));
            d += w;
        } else {
            out.push(c);
            d += 1;
        }
    }
    out
}

/// The matching bracket for the one at `p` (or just before it), within `limit` lines: returns
/// (this bracket, its partner).
pub fn match_bracket(b: &Buffer, p: Pos, limit: usize) -> Option<(Pos, Pos)> {
    const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];
    let at = |pos: Pos| b.line(pos.line).chars().nth(pos.col);
    let cands = [Some(p), p.col.checked_sub(1).map(|c| Pos::new(p.line, c))];
    for here in cands.into_iter().flatten() {
        let Some(c) = at(here) else { continue };
        for &(open, close) in &PAIRS {
            if c == open || c == close {
                let (dir, want) = if c == open { (1i64, close) } else { (-1, open) };
                let mut depth = 0i32;
                let mut line = here.line as i64;
                let mut col = here.col as i64;
                let n = b.line_count() as i64;
                let lo = (here.line as i64 - limit as i64).max(0);
                let hi = (here.line as i64 + limit as i64).min(n - 1);
                while line >= lo && line <= hi {
                    let cs: Vec<char> = b.line(line as usize).chars().collect();
                    while col >= 0 && (col as usize) < cs.len() {
                        let x = cs[col as usize];
                        if x == c {
                            depth += 1;
                        } else if x == want {
                            depth -= 1;
                            if depth == 0 {
                                return Some((here, Pos::new(line as usize, col as usize)));
                            }
                        }
                        col += dir;
                    }
                    line += dir;
                    col = if dir > 0 {
                        0
                    } else if line >= 0 {
                        b.line(line as usize).chars().count() as i64 - 1
                    } else {
                        0
                    };
                }
                return None;
            }
        }
    }
    None
}
