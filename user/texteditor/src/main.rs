//! Text Editor: plain text and code (docs/ui-concepts/SPEC-APPS.md, section 2). The buffer, undo,
//! find, wrapping and colouring live in the `textbuf` crate; this is the window around them.
//!
//! One file per window. Open with a path argument, the Open panel (Cmd-O) or by dropping a file
//! on the window. Keys: Cmd (or Ctrl) with O open, S save (Shift: Save As), Z / Shift-Z or Y
//! undo / redo, X C V A, F find (Option: with replace), G / Shift-G next / previous match,
//! L go to line, Q quit; Shift-L line numbers, Shift-W wrap, Shift-T tab width. Cmd-Up / Down
//! jump to the start / end of the text, Cmd-Left / Right to the start / end of the line (the
//! window system sends no Home, End or Page keys yet); Option-Left / Right move by word.

#![no_std]
#![no_main]

extern crate alloc;

mod draw;
mod io;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::{Field, Hits, Theme};
use gui::{Event, Key, Mods, Window};
use rt::sys;
use textbuf::syntax::{Lang, State};
use textbuf::view::{self, VRow};
use textbuf::{Buffer, FindOpts, Pos, TextError};

rt::main!(main);

pub const W: i32 = 700;
pub const H: i32 = 540;
pub const LINE_H: i32 = 18;
pub const CELL_W: i32 = 8;
pub const GUTTER_W: i32 = 48;
pub const PAD: i32 = 12;
pub const STATUS_H: i32 = 24;
pub const FIND_H: i32 = 40;

/// The user cancelled a panel.
const CANCELED: sys::Errno = sys::Errno(125);

// Hit ids.
pub const ID_FIND: u32 = 1;
pub const ID_REPL: u32 = 2;
pub const ID_PREV: u32 = 3;
pub const ID_NEXT: u32 = 4;
pub const ID_CASE: u32 = 5;
pub const ID_REPLACE: u32 = 6;
pub const ID_ALL: u32 = 7;
pub const ID_GOTO: u32 = 8;
pub const ID_SAVE: u32 = 10;
pub const ID_DISCARD: u32 = 11;
pub const ID_CANCEL: u32 = 12;
pub const ID_OK: u32 = 13;
pub const ID_ST_TAB: u32 = 20;
pub const ID_ST_WRAP: u32 = 21;
pub const ID_ST_GUTTER: u32 = 22;

pub struct Doc {
    pub buf: Buffer,
    /// Where it is saved, if we know (a path argument or Save As through the panel).
    pub path: Option<String>,
    pub name: String,
    pub lang: Lang,
    pub wrap: bool,
    /// Opened with bytes replaced (not valid UTF-8, or binary): saving would change the file.
    pub readonly: bool,
}

pub struct Find {
    pub find: Field,
    pub repl: Field,
    pub replace: bool,
    /// 0 = the find field has the keys, 1 = the replace field.
    pub focus: usize,
    pub opts: FindOpts,
}

pub enum Sheet {
    /// Quit with unsaved changes.
    Quit,
    Error(String),
}

pub struct App {
    pub win: Window,
    pub th: Theme,
    pub hits: Hits,
    pub home: String,
    pub doc: Doc,
    pub rows: Vec<VRow>,
    /// What `rows` was laid out for: buffer version, columns, wrap.
    pub rows_key: (u64, usize, bool),
    pub states: Vec<State>,
    pub states_key: (u64, Lang),
    pub scroll: usize,
    /// Horizontal scroll in cells (no wrap only).
    pub hscroll: usize,
    pub find: Option<Find>,
    pub goto: Option<Field>,
    pub sheet: Option<Sheet>,
    pub tab: usize,
    pub gutter: bool,
    pub drag: Option<Pos>,
    /// (time, x, y, count) of the last click, for double and triple clicks.
    pub click: (u64, i32, i32, u8),
    pub last_input: u64,
    /// When the last edit happened (a pause starts a new undo step).
    pub last_typed: u64,
    /// The display column up/down try to keep across short rows.
    pub goal_col: Option<usize>,
    pub msg: Option<(String, u64)>,
    pub title: String,
    pub caret_on: bool,
    pub quit: bool,
}

pub fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

fn new_doc(name: &str) -> Doc {
    Doc {
        buf: Buffer::new(),
        path: None,
        name: String::from(name),
        lang: Lang::Plain,
        wrap: true,
        readonly: false,
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl App {
    // ---- geometry -------------------------------------------------------------------------

    pub fn top(&self) -> i32 {
        if self.find.is_some() || self.goto.is_some() {
            FIND_H + 1
        } else {
            0
        }
    }

    pub fn gutter_w(&self) -> i32 {
        if self.gutter { GUTTER_W } else { 0 }
    }

    pub fn text_x0(&self) -> i32 {
        self.gutter_w() + PAD
    }

    pub fn cols(&self) -> usize {
        (((self.win.w - self.text_x0() - 8) / CELL_W).max(1)) as usize
    }

    pub fn vis_rows(&self) -> usize {
        (((self.win.h - STATUS_H - self.top()) / LINE_H).max(1)) as usize
    }

    // ---- layout caches ----------------------------------------------------------------------

    pub fn relayout(&mut self) {
        let key = (self.doc.buf.version(), self.cols(), self.doc.wrap);
        if self.rows.is_empty() || key != self.rows_key {
            self.rows = view::layout(&self.doc.buf, key.1, key.2);
            self.rows_key = key;
        }
        let skey = (self.doc.buf.version(), self.doc.lang);
        if self.doc.lang != Lang::Plain && (skey != self.states_key || self.states.is_empty()) {
            let b = &self.doc.buf;
            self.states =
                textbuf::syntax::states(self.doc.lang, (0..b.line_count()).map(|i| b.line(i)));
            self.states_key = skey;
        }
        let max = self.rows.len().saturating_sub(1);
        self.scroll = self.scroll.min(max);
    }

    /// Scroll so the caret is on screen.
    pub fn ensure_visible(&mut self) {
        self.relayout();
        let (row, c) = view::locate(&self.rows, self.doc.buf.cursor());
        let vis = self.vis_rows();
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + vis {
            self.scroll = row + 1 - vis;
        }
        if !self.doc.wrap {
            let line = self.doc.buf.line(self.rows[row].line);
            let d = view::display_col(line, self.rows[row].start + c, self.tab);
            let cols = self.cols();
            if d < self.hscroll {
                self.hscroll = d;
            } else if d >= self.hscroll + cols {
                self.hscroll = d + 1 - cols;
            }
        } else {
            self.hscroll = 0;
        }
    }

    // ---- the text under a point -------------------------------------------------------------

    /// The text position under a body point (clamped into the text).
    pub fn pos_at_point(&mut self, x: i32, y: i32) -> Pos {
        self.relayout();
        let r = ((y - self.top()).max(0) / LINE_H) as usize + self.scroll;
        let r = r.min(self.rows.len() - 1);
        let row = self.rows[r];
        let line = self.doc.buf.line(row.line);
        let part: String = line
            .chars()
            .skip(row.start)
            .take(row.end - row.start)
            .collect();
        let dcol = ((x - self.text_x0()).max(0) / CELL_W) as usize + self.hscroll;
        let col = view::col_at_display(&part, dcol, self.tab);
        view::pos_at(&self.rows, r, col)
    }

    // ---- documents --------------------------------------------------------------------------

    fn adopt(&mut self, name: &str, path: Option<String>, bytes: &[u8]) {
        let (buf, readonly) = match Buffer::from_bytes(bytes) {
            Ok(b) => (b, false),
            Err(e) => {
                let why = match e {
                    TextError::Binary => "This looks like a binary file, not text.",
                    TextError::Utf16 => "This file is UTF-16; the editor only reads UTF-8.",
                    TextError::InvalidUtf8(_) => "This file isn't valid UTF-8.",
                };
                self.sheet = Some(Sheet::Error(format!(
                    "{why} It is shown read-only (bad bytes replaced), so saving can't damage it."
                )));
                (Buffer::from_bytes_lossy(bytes), true)
            }
        };
        let lang = Lang::from_file_name(name);
        self.doc = Doc {
            buf,
            path,
            name: String::from(name),
            lang,
            wrap: lang.wraps_by_default(),
            readonly,
        };
        self.rows.clear();
        self.states.clear();
        self.scroll = 0;
        self.hscroll = 0;
    }

    fn open_path(&mut self, path: &str) {
        match io::load_path(path) {
            Ok(b) => self.adopt(base_name(path), Some(String::from(path)), &b),
            Err(e) => {
                self.doc = new_doc(base_name(path));
                self.doc.path = Some(String::from(path));
                self.sheet = Some(Sheet::Error(e));
            }
        }
    }

    fn open_fd(&mut self, fd: i32, how: &str) {
        let r = io::read_fd(fd);
        let _ = sys::close(fd);
        match r {
            Ok(b) => self.adopt("Untitled", None, &b),
            Err(e) => self.sheet = Some(Sheet::Error(e)),
        }
        self.flash(how);
    }

    /// Cmd-O: the Open panel.
    fn open_panel(&mut self) {
        if self.doc.buf.is_dirty() {
            self.flash("Save or close this file before opening another.");
            return;
        }
        match sys::powerbox_open() {
            Ok(fd) => self.open_fd(fd, "Opened. Save As to keep changes."),
            Err(e) if e == CANCELED => {}
            Err(e) => self.flash(&format!("Open: {}", e.name())),
        }
    }

    pub fn flash(&mut self, s: &str) {
        self.msg = Some((String::from(s), now_ms() + 4000));
    }

    fn save(&mut self, as_new: bool) -> bool {
        if self.doc.readonly {
            self.sheet = Some(Sheet::Error(String::from(
                "This file was opened read-only (it isn't valid UTF-8), so it can't be saved.",
            )));
            return false;
        }
        let bytes = self.doc.buf.to_bytes();
        let r = match (&self.doc.path, as_new) {
            (Some(p), false) => io::save_path(p, &bytes).map(|_| None),
            _ => match sys::powerbox_save(&self.doc.name) {
                Ok(fd) => {
                    let r = io::write_fd(fd, &bytes);
                    let _ = sys::close(fd);
                    r.map(|_| Some(()))
                }
                Err(e) if e == CANCELED => return false,
                Err(e) => Err(format!("Save: {}.", e.name())),
            },
        };
        match r {
            Ok(_) => {
                self.doc.buf.mark_saved();
                self.flash("Saved");
                true
            }
            Err(e) => {
                self.sheet = Some(Sheet::Error(e));
                false
            }
        }
    }

    // ---- editing helpers ----------------------------------------------------------------------

    fn editable(&mut self) -> bool {
        if self.doc.readonly {
            self.flash("Read-only: this file isn't valid UTF-8.");
            false
        } else {
            true
        }
    }

    fn typed(&mut self) {
        let now = now_ms();
        self.last_input = now;
        self.last_typed = now;
        self.msg = None;
        self.ensure_visible();
    }

    fn copy(&mut self) {
        if let Some(t) = self.doc.buf.selected_text() {
            io::clipboard_set(&t);
        }
    }

    fn paste(&mut self) {
        if !self.editable() {
            return;
        }
        let t = io::clipboard_get();
        if !t.is_empty() {
            self.doc.buf.insert_str(&t);
            self.typed();
        }
    }

    fn cut(&mut self) {
        if !self.editable() {
            return;
        }
        if let Some(t) = self.doc.buf.cut() {
            io::clipboard_set(&t);
            self.typed();
        }
    }

    // ---- find ---------------------------------------------------------------------------------

    fn open_find(&mut self, replace: bool) {
        self.goto = None;
        if self.find.is_none() {
            let seed = self
                .doc
                .buf
                .selected_text()
                .filter(|t| !t.contains('\n') && t.chars().count() < 60)
                .unwrap_or_default();
            self.find = Some(Find {
                find: Field::new(&seed, 80),
                repl: Field::new("", 80),
                replace,
                focus: 0,
                opts: FindOpts::default(),
            });
        }
        if let Some(f) = &mut self.find {
            f.replace |= replace;
            f.focus = 0;
            // An initial seed is selected text already.
        }
    }

    fn find_step(&mut self, back: bool) {
        let Some(f) = &self.find else { return };
        let (needle, opts) = (f.find.text.clone(), f.opts);
        if needle.is_empty() {
            return;
        }
        if !self.doc.buf.find_select(&needle, opts, back) {
            self.flash("No matches");
        }
        self.ensure_visible();
    }

    /// The find text changed: jump to the first match at or after where we are.
    fn find_changed(&mut self) {
        let Some(f) = &self.find else { return };
        let (needle, opts) = (f.find.text.clone(), f.opts);
        if needle.is_empty() {
            return;
        }
        let from = self
            .doc
            .buf
            .selection()
            .map_or(self.doc.buf.cursor(), |(a, _)| a);
        if let Some((a, b)) = self.doc.buf.find_next(&needle, opts, from) {
            self.doc.buf.select(a, b);
            self.ensure_visible();
        }
    }

    fn replace_one(&mut self) {
        if !self.editable() {
            return;
        }
        let Some(f) = &self.find else { return };
        let (needle, with, opts) = (f.find.text.clone(), f.repl.text.clone(), f.opts);
        self.doc.buf.replace_current(&needle, &with, opts);
        self.typed();
    }

    fn replace_all(&mut self) {
        if !self.editable() {
            return;
        }
        let Some(f) = &self.find else { return };
        let (needle, with, opts) = (f.find.text.clone(), f.repl.text.clone(), f.opts);
        let n = self.doc.buf.replace_all(&needle, &with, opts);
        self.flash(&if n == 1 {
            String::from("Replaced 1 match")
        } else {
            format!("Replaced {n} matches")
        });
        self.typed();
    }

    // ---- events ---------------------------------------------------------------------------------

    fn click_hit(&mut self, id: u32) {
        match id {
            ID_FIND => {
                if let Some(f) = &mut self.find {
                    f.focus = 0;
                }
            }
            ID_REPL => {
                if let Some(f) = &mut self.find {
                    f.focus = 1;
                }
            }
            ID_PREV => self.find_step(true),
            ID_NEXT => self.find_step(false),
            ID_CASE => {
                if let Some(f) = &mut self.find {
                    f.opts.case_sensitive = !f.opts.case_sensitive;
                }
                self.find_changed();
            }
            ID_REPLACE => self.replace_one(),
            ID_ALL => self.replace_all(),
            ID_ST_TAB => {
                self.tab = if self.tab == 4 { 2 } else { 4 };
                self.save_settings();
            }
            ID_ST_WRAP => self.doc.wrap = !self.doc.wrap,
            ID_ST_GUTTER => {
                self.gutter = !self.gutter;
                self.save_settings();
            }
            ID_SAVE => {
                self.sheet = None;
                if self.save(false) {
                    self.quit = true;
                }
            }
            ID_DISCARD => {
                self.sheet = None;
                self.quit = true;
            }
            ID_CANCEL | ID_OK => self.sheet = None,
            _ => {}
        }
    }

    fn save_settings(&self) {
        io::save_settings(
            &self.home,
            &io::Settings {
                tab: self.tab,
                gutter: self.gutter,
            },
        );
    }

    fn request_quit(&mut self) {
        if self.doc.buf.is_dirty() && !self.doc.readonly {
            self.sheet = Some(Sheet::Quit);
        } else {
            self.quit = true;
        }
    }

    fn mouse_down(&mut self, x: i32, y: i32, shift: bool) {
        self.last_input = now_ms();
        if let Some(id) = self.hits.at(x, y) {
            self.click_hit(id);
            return;
        }
        if self.sheet.is_some() {
            return;
        }
        let body_bottom = self.win.h - STATUS_H;
        if y < self.top() || y >= body_bottom {
            return;
        }
        self.relayout();
        // Click in the gutter: select that line.
        if self.gutter && x < GUTTER_W {
            let p = self.pos_at_point(self.text_x0(), y);
            self.doc.buf.select_line(p.line);
            self.drag = None;
            return;
        }
        let p = self.pos_at_point(x, y);
        let now = now_ms();
        let (t, lx, ly, n) = self.click;
        let count = if now.saturating_sub(t) < 450 && (lx - x).abs() < 6 && (ly - y).abs() < 6 {
            (n % 3) + 1
        } else {
            1
        };
        self.click = (now, x, y, count);
        match count {
            2 => {
                self.doc.buf.select_word_at(p);
                self.drag = None;
            }
            3 => {
                self.doc.buf.select_line(p.line);
                self.drag = None;
            }
            _ => {
                if shift {
                    self.doc.buf.set_cursor(p, true);
                    self.drag = self
                        .doc
                        .buf
                        .selection()
                        .map(|(a, b)| if a == p { b } else { a });
                } else {
                    self.doc.buf.set_cursor(p, false);
                    self.drag = Some(p);
                }
            }
        }
        self.ensure_visible();
    }

    fn mouse_drag(&mut self, x: i32, y: i32) {
        let Some(anchor) = self.drag else { return };
        let y = y.clamp(self.top(), self.win.h - STATUS_H - 1);
        let p = self.pos_at_point(x, y);
        self.doc.buf.select(anchor, p);
        // Dragging past the top or bottom edge scrolls.
        self.ensure_visible();
        self.last_input = now_ms();
    }

    fn scroll_by(&mut self, d: i32) {
        self.relayout();
        let max = self.rows.len().saturating_sub(1) as i32;
        self.scroll = (self.scroll as i32 + d).clamp(0, max) as usize;
    }

    fn key(&mut self, k: Key, m: Mods) {
        let cmd = m.meta || m.ctrl;
        self.last_input = now_ms();
        if let Some(sheet) = &self.sheet {
            match (sheet, k) {
                (Sheet::Quit, Key::Enter) => self.click_hit(ID_SAVE),
                (Sheet::Error(_), Key::Enter) | (_, Key::Escape) => self.sheet = None,
                _ => {}
            }
            return;
        }
        if cmd && let Key::Char(c) = k {
            return self.shortcut(c.to_ascii_lowercase(), m);
        }
        // Bars take the keys while open.
        if self.goto.is_some() {
            return self.goto_key(k);
        }
        if self.find.is_some() && self.find_key(k, m) {
            return;
        }
        match k {
            Key::Char(c) if !c.is_control() => {
                if self.editable() {
                    self.break_group_if_paused();
                    self.doc.buf.insert_char(c);
                    self.typed();
                }
            }
            Key::Enter => {
                if self.editable() {
                    self.doc.buf.newline();
                    self.typed();
                }
            }
            Key::Tab => {
                if !self.editable() {
                    return;
                }
                if m.shift {
                    self.doc.buf.dedent_lines(self.tab);
                } else {
                    let unit = " ".repeat(self.tab);
                    if !self.doc.buf.indent_lines(&unit) {
                        // Spaces up to the next tab stop.
                        let c = self.doc.buf.cursor();
                        let d = view::display_col(self.doc.buf.line(c.line), c.col, self.tab);
                        let n = self.tab - d % self.tab;
                        self.doc.buf.insert_str(&" ".repeat(n));
                    }
                }
                self.typed();
            }
            Key::Backspace => {
                if self.editable() {
                    if m.alt {
                        self.doc.buf.delete_word_back();
                    } else {
                        self.doc.buf.backspace();
                    }
                    self.typed();
                }
            }
            Key::Delete => {
                if self.editable() {
                    self.doc.buf.delete_forward();
                    self.typed();
                }
            }
            Key::Left => {
                if m.alt {
                    self.doc.buf.move_word_left(m.shift);
                } else {
                    self.doc.buf.move_left(m.shift);
                }
                self.ensure_visible();
            }
            Key::Right => {
                if m.alt {
                    self.doc.buf.move_word_right(m.shift);
                } else {
                    self.doc.buf.move_right(m.shift);
                }
                self.ensure_visible();
            }
            Key::Up | Key::Down => {
                let dir = if k == Key::Up { -1 } else { 1 };
                let n = if m.alt {
                    self.vis_rows() as isize - 1
                } else {
                    1
                };
                self.move_rows(dir * n, m.shift);
            }
            Key::Escape => {
                self.drag = None;
            }
            _ => {}
        }
    }

    /// A pause of a second in typing starts a new undo step.
    fn break_group_if_paused(&mut self) {
        if now_ms().saturating_sub(self.last_typed) > 1000 {
            self.doc.buf.break_undo_group();
        }
    }

    /// Move the caret by screen rows (so wrapped lines move by their rows).
    fn move_rows(&mut self, d: isize, extend: bool) {
        self.relayout();
        let c = self.doc.buf.cursor();
        let (row, col) = view::locate(&self.rows, c);
        let target = row as isize + d;
        if target < 0 {
            self.doc.buf.set_cursor(Pos::default(), extend);
        } else if target as usize >= self.rows.len() {
            self.doc.buf.doc_end_cursor(extend);
        } else {
            let line = self.doc.buf.line(self.rows[row].line);
            let d0 = view::display_col(line, self.rows[row].start + col, self.tab);
            let t = self.rows[target as usize];
            let tl = self.doc.buf.line(t.line);
            let part: String = tl.chars().skip(t.start).take(t.end - t.start).collect();
            let want = self.goal_col.unwrap_or(d0);
            let col = view::col_at_display(&part, want, self.tab);
            self.doc
                .buf
                .set_cursor(view::pos_at(&self.rows, target as usize, col), extend);
            self.goal_col = Some(want);
        }
        self.ensure_visible();
    }

    fn shortcut(&mut self, c: char, m: Mods) {
        match c {
            'o' => self.open_panel(),
            's' => {
                self.save(m.shift);
            }
            'z' => {
                if self.editable() {
                    if m.shift {
                        self.doc.buf.redo();
                    } else {
                        self.doc.buf.undo();
                    }
                    self.typed();
                }
            }
            'y' => {
                if self.editable() {
                    self.doc.buf.redo();
                    self.typed();
                }
            }
            'x' => self.cut(),
            'c' => self.copy(),
            'v' => self.paste(),
            'a' => self.doc.buf.select_all(),
            'f' => self.open_find(m.alt),
            'g' => {
                if self.find.is_none() {
                    self.open_find(false);
                }
                self.find_step(m.shift);
            }
            'l' if m.shift => {
                self.gutter = !self.gutter;
                self.save_settings();
            }
            'l' => {
                self.find = None;
                self.goto = Some(Field::numeric("", 9));
            }
            'w' if m.shift => self.doc.wrap = !self.doc.wrap,
            't' if m.shift => {
                self.tab = if self.tab == 4 { 2 } else { 4 };
                self.save_settings();
            }
            'q' | 'w' => self.request_quit(),
            _ => {}
        }
        self.ensure_visible();
    }

    fn goto_key(&mut self, k: Key) {
        match k {
            Key::Enter => {
                if let Some(f) = self.goto.take()
                    && let Ok(n) = f.text.parse::<usize>()
                {
                    self.doc.buf.goto_line(n);
                }
                self.ensure_visible();
            }
            Key::Escape => self.goto = None,
            _ => {
                if let Some(f) = &mut self.goto {
                    f.key(k, Mods::default());
                }
            }
        }
    }

    /// Keys for the find bar; true if it used the key.
    fn find_key(&mut self, k: Key, m: Mods) -> bool {
        match k {
            Key::Escape => {
                self.find = None;
                self.ensure_visible();
                true
            }
            Key::Enter => {
                self.find_step(m.shift);
                true
            }
            Key::Tab => {
                if let Some(f) = &mut self.find
                    && f.replace
                {
                    f.focus = 1 - f.focus;
                }
                true
            }
            Key::Up | Key::Down => false,
            _ => {
                let mut changed_find = false;
                if let Some(f) = &mut self.find {
                    let field = if f.focus == 0 {
                        &mut f.find
                    } else {
                        &mut f.repl
                    };
                    let changed = field.key(k, m);
                    changed_find = changed && f.focus == 0;
                    if !changed && !matches!(k, Key::Left | Key::Right) {
                        return false;
                    }
                }
                if changed_find {
                    self.find_changed();
                }
                true
            }
        }
    }

    fn on_close(&mut self) {
        // The window is already gone when we hear about it, so unsaved work can't be asked about:
        // keep a copy rather than lose it.
        if self.doc.buf.is_dirty() && !self.doc.readonly {
            let bytes = self.doc.buf.to_bytes();
            if let Some(p) = io::save_recovery(&self.home, &self.doc.name, &bytes) {
                rt::println!("texteditor: kept your unsaved changes in {p}");
            }
        }
    }
}

fn main(args: &rt::Args) -> i32 {
    let Ok(win) = Window::new("Text Editor", W, H) else {
        rt::println!("texteditor: no desktop to open a window on");
        return 1;
    };
    win.set_resizable(360, 240);
    let home = String::from(args.env("HOME").unwrap_or("/Users/milk"));
    let st = io::load_settings(&home);
    let mut app = App {
        win,
        th: Theme::current(),
        hits: Hits::default(),
        home,
        doc: new_doc("Untitled"),
        rows: Vec::new(),
        rows_key: (0, 0, false),
        states: Vec::new(),
        states_key: (0, Lang::Plain),
        scroll: 0,
        hscroll: 0,
        find: None,
        goto: None,
        sheet: None,
        tab: st.tab,
        gutter: st.gutter,
        drag: None,
        click: (0, 0, 0, 0),
        last_input: 0,
        last_typed: 0,
        goal_col: None,
        msg: None,
        title: String::new(),
        caret_on: true,
        quit: false,
    };
    if let Some(path) = args.get(1) {
        app.open_path(path);
    }
    let mut shown_caret = true;
    let mut shown_msg = false;
    app.draw();
    loop {
        let ev = app.win.event(100);
        let mut dirty = false;
        match ev {
            Some(Event::Close) => {
                app.on_close();
                break;
            }
            Some(Event::Resize { w, h }) => {
                let _ = app.win.resize(w, h);
                app.ensure_visible();
                dirty = true;
            }
            Some(Event::Down { x, y, button: 1 }) => {
                app.goal_col = None;
                app.mouse_down(x, y, false);
                dirty = true;
            }
            Some(Event::Drag { x, y }) => {
                app.mouse_drag(x, y);
                dirty = true;
            }
            Some(Event::Up { .. }) => app.drag = None,
            Some(Event::Scroll(d)) => {
                app.scroll_by(d * 3);
                dirty = true;
            }
            Some(Event::Key(k, m)) => {
                if !matches!(k, Key::Up | Key::Down) {
                    app.goal_col = None;
                }
                app.key(k, m);
                dirty = true;
            }
            Some(Event::Drop { fd, .. }) => {
                if app.doc.buf.is_dirty() {
                    let _ = sys::close(fd);
                    app.flash("Save or close this file before opening another.");
                } else {
                    app.open_fd(fd, "Opened. Save As to keep changes.");
                }
                dirty = true;
            }
            _ => {}
        }
        if app.quit {
            break;
        }
        let now = now_ms();
        let caret = now.saturating_sub(app.last_input) < 600 || (now / 500) % 2 == 0;
        let msg = app.msg.as_ref().is_some_and(|m| m.1 > now);
        if !msg {
            app.msg = None;
        }
        if dirty || caret != shown_caret || msg != shown_msg {
            shown_caret = caret;
            shown_msg = msg;
            app.caret_on = caret;
            app.draw();
        }
    }
    0
}
