//! Notes: a small multi-line text editor. The text lives in a global store (not in the window),
//! so it survives closing the window and the Today home's NOTES card can show it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::SpinLock;
use crate::timer;

use super::super::event::Key;
use super::super::paint::Painter;
use super::super::theme::*;
use super::super::window::{KeyInput, PointerInput, PointerKind, WindowContents};

struct Store {
    lines: Vec<String>,
    edited_ns: Option<u64>,
}

static STORE: SpinLock<Option<Store>> = SpinLock::new(None);

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock();
    let s = g.get_or_insert_with(|| Store {
        lines: [
            "# milkOS 1.0",
            "- ship the compositor",
            "- pick a nicer font",
            "- boot logo, a milk drop",
            "- moo",
        ]
        .iter()
        .map(|s| String::from(*s))
        .collect(),
        edited_ns: None,
    });
    f(s)
}

/// The note's lines (for the Today card).
pub fn snapshot() -> Vec<String> {
    with(|s| s.lines.clone())
}

pub fn edited_ago(now: u64) -> String {
    match with(|s| s.edited_ns) {
        None => String::from("Not edited yet"),
        Some(t) => {
            let m = (now.saturating_sub(t)) / 60_000_000_000;
            if m == 0 {
                String::from("Edited just now")
            } else {
                format!("Edited {m} min ago")
            }
        }
    }
}

const PAD: i32 = BODY_PAD;
const LINE_H: i32 = 20;

pub struct Notes {
    row: usize,
    col: usize,
    scroll: usize,
    visible_rows: usize,
}

impl Notes {
    pub fn new() -> Notes {
        let (row, col) = with(|s| {
            let r = s.lines.len() - 1;
            (r, s.lines[r].chars().count())
        });
        Notes {
            row,
            col,
            scroll: 0,
            visible_rows: 10,
        }
    }

    fn byte_at(line: &str, col: usize) -> usize {
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }

    fn keep_visible(&mut self) {
        if self.row < self.scroll {
            self.scroll = self.row;
        }
        if self.row >= self.scroll + self.visible_rows {
            self.scroll = self.row + 1 - self.visible_rows;
        }
    }
}

impl WindowContents for Notes {
    fn title(&self) -> &str {
        "ideas.md"
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "Format", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, focused: bool) {
        self.visible_rows = ((h - 2 * PAD - STATUS_H) / LINE_H).max(1) as usize;
        self.keep_visible();
        let cols = ((w - 2 * PAD) / 8).max(1) as usize;
        let (lines, row, col) = with(|s| (s.lines.clone(), self.row, self.col));
        for (i, l) in lines
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(self.visible_rows)
        {
            let y = PAD + (i - self.scroll) as i32 * LINE_H;
            let shown: String = l.chars().take(cols).collect();
            if l.starts_with('#') {
                p.bold(PAD, y, &shown, ESPRESSO);
            } else if l.starts_with("- ") {
                p.text(PAD, y, &shown[..2.min(shown.len())], HONEY);
                p.text(PAD + 16, y, shown.get(2..).unwrap_or(""), ESPRESSO);
            } else {
                p.text(PAD, y, &shown, ESPRESSO);
            }
            if focused && i == row {
                let cx = PAD + col.min(cols) as i32 * 8;
                p.fill(cx, y - 1, 2, 18, STRAW);
            }
        }
        let sy = h - STATUS_H;
        p.fill(0, sy, w, STATUS_H, CREAM);
        p.fill(0, sy, w, 1, SAND);
        let words: usize = lines.iter().map(|l| l.split_whitespace().count()).sum();
        p.text(
            12,
            sy + 4,
            &format!("Ln {}, Col {}   {} words", row + 1, col + 1, words),
            COCOA,
        );
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        let (row, col) = (self.row, self.col);
        let changed = with(|s| {
            let line_len = |s: &Store, r: usize| s.lines[r].chars().count();
            match k.key {
                Key::Left => {
                    if self.col > 0 {
                        self.col -= 1;
                    } else if self.row > 0 {
                        self.row -= 1;
                        self.col = line_len(s, self.row);
                    }
                    false
                }
                Key::Right => {
                    if self.col < line_len(s, self.row) {
                        self.col += 1;
                    } else if self.row + 1 < s.lines.len() {
                        self.row += 1;
                        self.col = 0;
                    }
                    false
                }
                Key::Up => {
                    self.row = self.row.saturating_sub(1);
                    self.col = self.col.min(line_len(s, self.row));
                    false
                }
                Key::Down => {
                    self.row = (self.row + 1).min(s.lines.len() - 1);
                    self.col = self.col.min(line_len(s, self.row));
                    false
                }
                Key::Home => {
                    self.col = 0;
                    false
                }
                Key::End => {
                    self.col = line_len(s, self.row);
                    false
                }
                Key::Enter => {
                    let l = &mut s.lines[self.row];
                    let at = Self::byte_at(l, self.col);
                    let rest = l.split_off(at);
                    // Keep list bullets going.
                    let bullet = l.starts_with("- ");
                    let rest = if bullet && !rest.starts_with("- ") {
                        format!("- {rest}")
                    } else {
                        rest
                    };
                    self.row += 1;
                    self.col = if bullet { 2 } else { 0 };
                    s.lines.insert(self.row, rest);
                    true
                }
                Key::Backspace => {
                    if self.col > 0 {
                        let l = &mut s.lines[self.row];
                        let at = Self::byte_at(l, self.col - 1);
                        l.remove(at);
                        self.col -= 1;
                        true
                    } else if self.row > 0 {
                        let l = s.lines.remove(self.row);
                        self.row -= 1;
                        self.col = line_len(s, self.row);
                        s.lines[self.row].push_str(&l);
                        true
                    } else {
                        false
                    }
                }
                Key::Delete => {
                    let n = line_len(s, self.row);
                    if self.col < n {
                        let l = &mut s.lines[self.row];
                        let at = Self::byte_at(l, self.col);
                        l.remove(at);
                        true
                    } else if self.row + 1 < s.lines.len() {
                        let l = s.lines.remove(self.row + 1);
                        s.lines[self.row].push_str(&l);
                        true
                    } else {
                        false
                    }
                }
                _ => match k.ch {
                    Some(c) if !c.is_control() => {
                        let l = &mut s.lines[self.row];
                        let at = Self::byte_at(l, self.col);
                        l.insert(at, c);
                        self.col += 1;
                        true
                    }
                    Some('\t') => {
                        let l = &mut s.lines[self.row];
                        let at = Self::byte_at(l, self.col);
                        l.insert_str(at, "  ");
                        self.col += 2;
                        true
                    }
                    _ => false,
                },
            }
        });
        if changed {
            with(|s| s.edited_ns = Some(timer::now_ns()));
        }
        changed || (row, col) != (self.row, self.col)
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        if !matches!(e.kind, PointerKind::Down(_) | PointerKind::Drag) {
            return false;
        }
        let r = ((e.y - PAD).max(0) / LINE_H) as usize + self.scroll;
        with(|s| {
            self.row = r.min(s.lines.len() - 1);
            let c = ((e.x - PAD + 4).max(0) / 8) as usize;
            self.col = c.min(s.lines[self.row].chars().count());
        });
        true
    }

    fn scroll(&mut self, dy: i32) -> bool {
        let n = with(|s| s.lines.len());
        let max = n.saturating_sub(self.visible_rows);
        let new = (self.scroll as i32 + dy).clamp(0, max as i32) as usize;
        if new != self.scroll {
            self.scroll = new;
            // Keep the caret on screen.
            self.row = self
                .row
                .clamp(self.scroll, self.scroll + self.visible_rows - 1)
                .min(n - 1);
            return true;
        }
        false
    }
}
