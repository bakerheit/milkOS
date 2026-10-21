//! Notes: a small multi-line text editor. The text lives in a global store (not in the window),
//! so it survives closing the window and the Today home's NOTES card can show it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::SpinLock;
use crate::{fs, timer};

use super::super::clipboard;
use super::super::event::Key;
use super::super::paint::Painter;
use super::super::theme::*;
use super::super::window::{KeyInput, PointerInput, PointerKind, WindowContents};
use super::fsx;

struct Store {
    /// The file the note is saved to (autosaved after every edit).
    path: String,
    lines: Vec<String>,
    edited_ns: Option<u64>,
    /// Bumped when a different file is opened, so open windows reset their caret.
    generation: u64,
}

static STORE: SpinLock<Option<Store>> = SpinLock::new(None);

const DEFAULT_TEXT: &str = "# milkOS 1.0
- ship the compositor
- pick a nicer font
- boot logo, a milk drop
- moo";

fn load(path: &str) -> Option<Vec<String>> {
    let text = fs::read_to_string(path).ok()?;
    let mut v: Vec<String> = text.lines().map(String::from).collect();
    if v.is_empty() {
        v.push(String::new());
    }
    Some(v)
}

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    if STORE.lock().is_none() {
        // First use: the note lives in ~/Documents/ideas.md. File I/O outside the lock.
        let path = format!("{}/ideas.md", fsx::DOCS);
        let lines = load(&path).unwrap_or_else(|| {
            let _ = fs::write_file(&path, DEFAULT_TEXT.as_bytes());
            DEFAULT_TEXT.lines().map(String::from).collect()
        });
        let mut g = STORE.lock();
        if g.is_none() {
            *g = Some(Store {
                path,
                lines,
                edited_ns: None,
                generation: 0,
            });
        }
    }
    let mut g = STORE.lock();
    f(g.as_mut().unwrap())
}

fn save() {
    let (path, mut text) = with(|s| (s.path.clone(), s.lines.join("\n")));
    text.push('\n');
    if let Err(e) = fs::write_file(&path, text.as_bytes()) {
        crate::kprintln!("notes: saving {path} failed: {e}");
    }
}

/// Open a text file in Notes (the Notes window follows).
pub fn open_file(path: &str) {
    let lines = load(path).unwrap_or_else(|| alloc::vec![String::new()]);
    with(|s| {
        s.path = String::from(path);
        s.lines = lines;
        s.edited_ns = None;
        s.generation += 1;
    });
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
    generation: u64,
    title: String,
    /// Selection anchor; the selection runs from here to the caret.
    anchor: Option<(usize, usize)>,
}

impl Notes {
    pub fn new() -> Notes {
        let mut n = Notes {
            row: 0,
            col: 0,
            scroll: 0,
            visible_rows: 10,
            generation: u64::MAX,
            title: String::new(),
            anchor: None,
        };
        n.sync();
        n
    }

    /// Follow the store to a newly opened file: caret to the end, new title.
    fn sync(&mut self) -> bool {
        let (generation, path, row, col) = with(|s| {
            let r = s.lines.len() - 1;
            (s.generation, s.path.clone(), r, s.lines[r].chars().count())
        });
        if generation == self.generation {
            return false;
        }
        self.generation = generation;
        self.title = String::from(fsx::basename(&path));
        self.row = row;
        self.col = col;
        self.scroll = 0;
        true
    }

    fn byte_at(line: &str, col: usize) -> usize {
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }

    /// The selection as (start, end), ordered; None if empty.
    fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let c = (self.row, self.col);
        match a.cmp(&c) {
            core::cmp::Ordering::Less => Some((a, c)),
            core::cmp::Ordering::Greater => Some((c, a)),
            core::cmp::Ordering::Equal => None,
        }
    }

    fn selected_text(&self, s: &Store) -> String {
        let Some(((r0, c0), (r1, c1))) = self.selection() else {
            return String::new();
        };
        let mut out = String::new();
        for r in r0..=r1 {
            let l: Vec<char> = s.lines[r].chars().collect();
            let a = if r == r0 { c0.min(l.len()) } else { 0 };
            let b = if r == r1 { c1.min(l.len()) } else { l.len() };
            out.extend(&l[a..b]);
            if r != r1 {
                out.push('\n');
            }
        }
        out
    }

    /// Delete the selection (if any); the caret goes to its start. True if something went.
    fn delete_selection(&mut self, s: &mut Store) -> bool {
        let Some(((r0, c0), (r1, c1))) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let head: String = s.lines[r0].chars().take(c0).collect();
        let tail: String = s.lines[r1].chars().skip(c1).collect();
        s.lines.drain(r0 + 1..=r1);
        s.lines[r0] = head + &tail;
        self.row = r0;
        self.col = c0;
        self.anchor = None;
        true
    }

    /// Insert `text` (may contain newlines) at the caret.
    fn insert_text(&mut self, s: &mut Store, text: &str) {
        let line = &mut s.lines[self.row];
        let at = Self::byte_at(line, self.col);
        let rest = line.split_off(at);
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or("");
        s.lines[self.row].push_str(first);
        self.col += first.chars().count();
        for p in parts {
            self.row += 1;
            s.lines.insert(self.row, String::from(p));
            self.col = p.chars().count();
        }
        s.lines[self.row].push_str(&rest);
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
        &self.title
    }

    fn tick(&mut self, _now: u64) -> bool {
        self.sync()
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
            if let Some(((r0, c0), (r1, c1))) = self.selection()
                && (r0..=r1).contains(&i)
            {
                let n = l.chars().count();
                let a = if i == r0 { c0 } else { 0 };
                // The line break counts as one selected cell.
                let b = if i == r1 { c1 } else { n + 1 };
                let (a, b) = (a.min(cols), b.min(cols + 1));
                if b > a {
                    p.fill_a(
                        PAD + a as i32 * 8,
                        y - 1,
                        (b - a) as i32 * 8,
                        18,
                        STRAW,
                        SEL_ROW_A,
                    );
                }
            }
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
        // Clipboard and select-all: Cmd or Ctrl + A / C / X / V.
        if (k.mods.meta || k.mods.ctrl)
            && let Key::Char(c) = k.key
        {
            let changed = with(|s| match c {
                'a' => {
                    self.anchor = Some((0, 0));
                    self.row = s.lines.len() - 1;
                    self.col = s.lines[self.row].chars().count();
                    false
                }
                'c' => {
                    let t = self.selected_text(s);
                    if !t.is_empty() {
                        clipboard::set(&t);
                    }
                    false
                }
                'x' => {
                    let t = self.selected_text(s);
                    if !t.is_empty() {
                        clipboard::set(&t);
                    }
                    self.delete_selection(s)
                }
                'v' => {
                    let t = clipboard::get();
                    self.delete_selection(s);
                    self.insert_text(s, &t);
                    !t.is_empty()
                }
                _ => false,
            });
            if changed {
                with(|s| s.edited_ns = Some(timer::now_ns()));
                save();
            }
            return true;
        }
        let moving = matches!(
            k.key,
            Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End
        );
        if moving {
            if k.mods.shift {
                if self.anchor.is_none() {
                    self.anchor = Some((self.row, self.col));
                }
            } else {
                self.anchor = None;
            }
        }
        let had_selection = self.selection().is_some();
        let changed = with(|s| {
            // Typing, Enter or erasing replaces the selection.
            if !moving && had_selection {
                let erase = matches!(k.key, Key::Backspace | Key::Delete);
                let types = k.ch.is_some_and(|c| !c.is_control()) || k.key == Key::Enter;
                if erase || types {
                    self.delete_selection(s);
                    if erase {
                        return true;
                    }
                }
            }
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
            save();
        }
        changed || (row, col) != (self.row, self.col) || had_selection
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        if let PointerKind::Up(_) = e.kind {
            if self.selection().is_none() {
                self.anchor = None;
            }
            return false;
        }
        let r = ((e.y - PAD).max(0) / LINE_H) as usize + self.scroll;
        with(|s| {
            self.row = r.min(s.lines.len() - 1);
            let c = ((e.x - PAD + 4).max(0) / 8) as usize;
            self.col = c.min(s.lines[self.row].chars().count());
        });
        if let PointerKind::Down(_) = e.kind {
            self.anchor = Some((self.row, self.col));
        }
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
