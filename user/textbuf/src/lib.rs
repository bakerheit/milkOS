//! The editor engine behind the Text Editor: a [`Buffer`] of lines with a cursor, a selection,
//! undo/redo, find/replace and UTF-8 in and out. It does no drawing and no I/O, so the app is
//! a thin shell and the rules live (and are tested) here.
//!
//! Positions are `(line, col)` with `col` counted in `char`s, not bytes. Text is stored one
//! `String` per line without its terminator; a buffer always has at least one (maybe empty)
//! line. Line endings (LF or CRLF), a final newline and a UTF-8 byte-order mark are remembered
//! when a file is loaded and written back the same way by [`Buffer::to_bytes`].
//!
//! Undo: every edit is recorded as (position, removed text, inserted text) inside a group.
//! Typing, backspace and delete coalesce into one group while the cursor stays put between
//! them; anything else (a newline, a paste, replace, moving the cursor, saving) starts a new
//! group, so one undo step is one thing the user would call "that edit". History is capped at
//! [`MAX_UNDO`] groups.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub mod syntax;
pub mod view;

/// How many undo groups are kept.
pub const MAX_UNDO: usize = 1000;

/// A place in the text: `line` (0-based) and `col` in chars.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    Crlf,
}

/// Why a file can't be opened as text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextError {
    /// Contains NUL bytes: a binary file.
    Binary,
    /// UTF-16 (byte-order mark found); only UTF-8 is supported.
    Utf16,
    /// Not valid UTF-8; the first bad byte is at this offset.
    InvalidUtf8(usize),
}

/// Options for find and replace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FindOpts {
    pub case_sensitive: bool,
    pub whole_word: bool,
}

impl Default for FindOpts {
    fn default() -> FindOpts {
        FindOpts {
            case_sensitive: true,
            whole_word: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Typing,
    Backspace,
    Delete,
    Other,
}

#[derive(Clone, Debug)]
struct Edit {
    at: Pos,
    removed: String,
    inserted: String,
}

#[derive(Clone, Copy, Debug)]
struct Sel {
    cursor: Pos,
    anchor: Option<Pos>,
}

#[derive(Clone, Debug)]
struct Group {
    id: u64,
    edits: Vec<Edit>,
    before: Sel,
    after: Sel,
}

pub struct Buffer {
    lines: Vec<String>,
    eol: Eol,
    final_newline: bool,
    bom: bool,
    lossy: bool,
    cursor: Pos,
    anchor: Option<Pos>,
    want_col: usize,
    undo: Vec<Group>,
    redo: Vec<Group>,
    open: Option<Kind>,
    next_id: u64,
    saved_id: Option<u64>,
    /// Bumped on every change to the text (edit, undo, redo), so views can cache.
    version: u64,
    /// Id of the state with an empty undo stack (changes when old history is dropped).
    base_id: u64,
    /// Loaded from a file that was just one line break (an empty buffer can't tell it from "").
    lone_newline: bool,
    /// Copy the current line's indentation on Enter.
    pub auto_indent: bool,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Byte offset of char column `col` in `s` (clamped to the end).
fn byte_of(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(i, _)| i)
}

/// `p` after the leading `n` chars of its line were removed, for each `(line, n)` in `edits`.
fn dedented(edits: &[(usize, usize)], p: Pos) -> Pos {
    let n = edits.iter().find(|e| e.0 == p.line).map_or(0, |e| e.1);
    Pos::new(p.line, p.col.saturating_sub(n))
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Where text `s` inserted at `at` ends.
fn end_of(at: Pos, s: &str) -> Pos {
    let mut n = 0;
    let mut last = s;
    for (i, piece) in s.split('\n').enumerate() {
        n = i;
        last = piece;
    }
    if n == 0 {
        Pos::new(at.line, at.col + chars(s))
    } else {
        Pos::new(at.line + n, chars(last))
    }
}

impl Default for Buffer {
    fn default() -> Buffer {
        Buffer::new()
    }
}

impl Buffer {
    /// An empty buffer.
    pub fn new() -> Buffer {
        Buffer {
            lines: alloc::vec![String::new()],
            eol: Eol::Lf,
            final_newline: true,
            bom: false,
            lossy: false,
            cursor: Pos::default(),
            anchor: None,
            want_col: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            open: None,
            next_id: 1,
            saved_id: Some(0),
            version: 0,
            base_id: 0,
            lone_newline: false,
            auto_indent: true,
        }
    }

    // ---- loading and saving ---------------------------------------------------------------

    /// Load a file's bytes. Refuses binary data, UTF-16 and invalid UTF-8; use
    /// [`Buffer::from_bytes_lossy`] to open those anyway (bad bytes become U+FFFD).
    pub fn from_bytes(b: &[u8]) -> Result<Buffer, TextError> {
        let body = b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(b);
        if body.starts_with(&[0xFF, 0xFE]) || body.starts_with(&[0xFE, 0xFF]) {
            return Err(TextError::Utf16);
        }
        if body.contains(&0) {
            return Err(TextError::Binary);
        }
        match core::str::from_utf8(body) {
            Ok(s) => Ok(Buffer::from_parts(s, b.len() != body.len(), false)),
            Err(e) => Err(TextError::InvalidUtf8(
                e.valid_up_to() + (b.len() - body.len()),
            )),
        }
    }

    /// Like [`Buffer::from_bytes`] but never fails on encoding: invalid UTF-8 is replaced
    /// and [`Buffer::is_lossy`] says so (saving would change those bytes). NUL bytes stay.
    pub fn from_bytes_lossy(b: &[u8]) -> Buffer {
        let body = b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(b);
        let s = String::from_utf8_lossy(body);
        let lossy = matches!(s, alloc::borrow::Cow::Owned(_));
        Buffer::from_parts(&s, b.len() != body.len(), lossy)
    }

    fn from_parts(s: &str, bom: bool, lossy: bool) -> Buffer {
        let crlf = s.matches("\r\n").count();
        let lf = s.matches('\n').count() - crlf;
        let eol = if crlf > lf { Eol::Crlf } else { Eol::Lf };
        let final_newline = s.is_empty() || s.ends_with('\n');
        let mut lines: Vec<String> = s
            .split('\n')
            .map(|l| String::from(l.strip_suffix('\r').unwrap_or(l)))
            .collect();
        if final_newline && lines.len() > 1 {
            lines.pop();
        }
        let mut b = Buffer::new();
        b.lines = lines;
        b.eol = eol;
        b.final_newline = final_newline;
        b.lone_newline = s == "\n" || s == "\r\n";
        b.bom = bom;
        b.lossy = lossy;
        b
    }

    /// The file as bytes: the buffer's line ending, its final newline (or lack of one), and a
    /// byte-order mark if it had one.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        if self.bom {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        let nl: &[u8] = match self.eol {
            Eol::Lf => b"\n",
            Eol::Crlf => b"\r\n",
        };
        if self.lines.len() == 1 && self.lines[0].is_empty() {
            // An empty file stays empty, unless it was a lone line break.
            if self.lone_newline {
                out.extend_from_slice(nl);
            }
            return out;
        }
        for (i, l) in self.lines.iter().enumerate() {
            out.extend_from_slice(l.as_bytes());
            if i + 1 < self.lines.len() || self.final_newline {
                out.extend_from_slice(nl);
            }
        }
        out
    }

    /// The text with `\n` between lines (no final newline added).
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn eol(&self) -> Eol {
        self.eol
    }

    pub fn set_eol(&mut self, e: Eol) {
        if self.eol != e {
            self.eol = e;
            self.saved_id = None;
        }
    }

    /// True if opened with [`Buffer::from_bytes_lossy`] and bytes had to be replaced.
    pub fn is_lossy(&self) -> bool {
        self.lossy
    }

    /// The next edit starts a new undo group (a pause in typing).
    pub fn break_undo_group(&mut self) {
        self.open = None;
    }

    /// Unsaved changes? Undoing back to the saved state makes it clean again.
    pub fn is_dirty(&self) -> bool {
        self.saved_id != Some(self.top_id())
    }

    /// Call after writing the file. The next edit starts a new undo group.
    pub fn mark_saved(&mut self) {
        self.saved_id = Some(self.top_id());
        self.open = None;
    }

    fn top_id(&self) -> u64 {
        self.undo.last().map_or(self.base_id, |g| g.id)
    }

    // ---- reading --------------------------------------------------------------------------

    /// A number that changes whenever the text does.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, i: usize) -> &str {
        self.lines.get(i).map_or("", |s| s.as_str())
    }

    /// Length of line `i` in chars.
    pub fn line_len(&self, i: usize) -> usize {
        chars(self.line(i))
    }

    pub fn char_count(&self) -> usize {
        self.lines.iter().map(|l| chars(l)).sum::<usize>() + self.lines.len() - 1
    }

    /// Digits needed to print the largest line number (for the gutter).
    pub fn gutter_digits(&self) -> usize {
        let mut n = self.lines.len();
        let mut d = 1;
        while n >= 10 {
            n /= 10;
            d += 1;
        }
        d
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    /// The selection as (start, end), if any text is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if a == self.cursor {
            None
        } else if a < self.cursor {
            Some((a, self.cursor))
        } else {
            Some((self.cursor, a))
        }
    }

    /// The text between two positions (`\n` between lines).
    pub fn slice(&self, a: Pos, b: Pos) -> String {
        let (a, b) = (self.clamp(a), self.clamp(b));
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if a.line == b.line {
            let l = self.line(a.line);
            return String::from(&l[byte_of(l, a.col)..byte_of(l, b.col)]);
        }
        let first = self.line(a.line);
        let mut s = String::from(&first[byte_of(first, a.col)..]);
        for i in a.line + 1..b.line {
            s.push('\n');
            s.push_str(self.line(i));
        }
        s.push('\n');
        let last = self.line(b.line);
        s.push_str(&last[..byte_of(last, b.col)]);
        s
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|(a, b)| self.slice(a, b))
    }

    /// `p` moved inside the text (line and column clamped).
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.lines.len() - 1);
        Pos::new(line, p.col.min(self.line_len(line)))
    }

    fn doc_end(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos::new(line, self.line_len(line))
    }

    // ---- cursor and selection -------------------------------------------------------------

    /// Put the cursor at `p`. With `extend` the selection grows from where it was (shift+arrow
    /// or drag); without, any selection is dropped.
    pub fn set_cursor(&mut self, p: Pos, extend: bool) {
        let p = self.clamp(p);
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = p;
        self.want_col = p.col;
        self.open = None;
    }

    pub fn select(&mut self, a: Pos, b: Pos) {
        self.anchor = Some(self.clamp(a));
        self.cursor = self.clamp(b);
        self.want_col = self.cursor.col;
        self.open = None;
    }

    pub fn select_all(&mut self) {
        let end = self.doc_end();
        self.select(Pos::default(), end);
    }

    pub fn select_line(&mut self, line: usize) {
        let line = line.min(self.lines.len() - 1);
        self.select(Pos::new(line, 0), Pos::new(line, self.line_len(line)));
    }

    /// Cursor to the start of 1-based line `n` (clamped).
    pub fn goto_line(&mut self, n: usize) {
        self.set_cursor(Pos::new(n.saturating_sub(1), 0), false);
    }

    pub fn move_left(&mut self, extend: bool) {
        if !extend && let Some((a, _)) = self.selection() {
            return self.set_cursor(a, false);
        }
        let c = self.cursor;
        let p = if c.col > 0 {
            Pos::new(c.line, c.col - 1)
        } else if c.line > 0 {
            Pos::new(c.line - 1, self.line_len(c.line - 1))
        } else {
            c
        };
        self.set_cursor(p, extend);
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && let Some((_, b)) = self.selection() {
            return self.set_cursor(b, false);
        }
        let c = self.cursor;
        let p = if c.col < self.line_len(c.line) {
            Pos::new(c.line, c.col + 1)
        } else if c.line + 1 < self.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            c
        };
        self.set_cursor(p, extend);
    }

    /// Up (`dy` < 0) or down by lines, keeping the column the cursor "wants" across short
    /// lines. Past the first or last line the cursor goes to the start or end of the text.
    pub fn move_lines(&mut self, dy: isize, extend: bool) {
        let want = self.want_col;
        let target = self.cursor.line as isize + dy;
        let inside = target >= 0 && (target as usize) < self.lines.len();
        let p = if target < 0 {
            Pos::default()
        } else if !inside {
            self.doc_end()
        } else {
            Pos::new(target as usize, want.min(self.line_len(target as usize)))
        };
        self.set_cursor(p, extend);
        if inside {
            self.want_col = want;
        }
    }

    pub fn home(&mut self, extend: bool) {
        // First press: first non-blank; again: column 0.
        let l = self.line(self.cursor.line);
        let indent = l.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let col = if self.cursor.col == indent || indent == chars(l) {
            0
        } else {
            indent
        };
        self.set_cursor(Pos::new(self.cursor.line, col), extend);
    }

    pub fn end(&mut self, extend: bool) {
        let n = self.line_len(self.cursor.line);
        self.set_cursor(Pos::new(self.cursor.line, n), extend);
    }

    pub fn doc_start_cursor(&mut self, extend: bool) {
        self.set_cursor(Pos::default(), extend);
    }

    pub fn doc_end_cursor(&mut self, extend: bool) {
        let e = self.doc_end();
        self.set_cursor(e, extend);
    }

    fn word_left_of(&self, p: Pos) -> Pos {
        if p.col == 0 {
            return if p.line > 0 {
                Pos::new(p.line - 1, self.line_len(p.line - 1))
            } else {
                p
            };
        }
        let cs: Vec<char> = self.line(p.line).chars().collect();
        let mut i = p.col.min(cs.len());
        while i > 0 && !is_word(cs[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word(cs[i - 1]) {
            i -= 1;
        }
        Pos::new(p.line, i)
    }

    fn word_right_of(&self, p: Pos) -> Pos {
        let cs: Vec<char> = self.line(p.line).chars().collect();
        if p.col >= cs.len() {
            return if p.line + 1 < self.lines.len() {
                Pos::new(p.line + 1, 0)
            } else {
                p
            };
        }
        let mut i = p.col;
        while i < cs.len() && !is_word(cs[i]) {
            i += 1;
        }
        while i < cs.len() && is_word(cs[i]) {
            i += 1;
        }
        Pos::new(p.line, i)
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let p = self.word_left_of(self.cursor);
        self.set_cursor(p, extend);
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let p = self.word_right_of(self.cursor);
        self.set_cursor(p, extend);
    }

    /// Select the word under `p` (double click); a run of spaces or punctuation if not on a word.
    pub fn select_word_at(&mut self, p: Pos) {
        let p = self.clamp(p);
        let cs: Vec<char> = self.line(p.line).chars().collect();
        if cs.is_empty() {
            return self.select(p, p);
        }
        let i = p.col.min(cs.len() - 1);
        let class = |c: char| {
            if is_word(c) {
                0
            } else if c.is_whitespace() {
                1
            } else {
                2
            }
        };
        let k = class(cs[i]);
        let (mut a, mut b) = (i, i + 1);
        while a > 0 && class(cs[a - 1]) == k {
            a -= 1;
        }
        while b < cs.len() && class(cs[b]) == k {
            b += 1;
        }
        self.select(Pos::new(p.line, a), Pos::new(p.line, b));
    }

    // ---- primitive edits ------------------------------------------------------------------

    /// Remove `a..b` (a <= b, both valid) and return the removed text.
    fn raw_delete(&mut self, a: Pos, b: Pos) -> String {
        self.version += 1;
        let removed = self.slice(a, b);
        let (ba, bb) = (
            byte_of(self.line(a.line), a.col),
            byte_of(self.line(b.line), b.col),
        );
        if a.line == b.line {
            self.lines[a.line].replace_range(ba..bb, "");
        } else {
            let tail = String::from(&self.lines[b.line][bb..]);
            self.lines[a.line].truncate(ba);
            self.lines[a.line].push_str(&tail);
            self.lines.drain(a.line + 1..=b.line);
        }
        removed
    }

    /// Insert `s` (only `\n` for line breaks) at `p`; returns where it ends.
    fn raw_insert(&mut self, p: Pos, s: &str) -> Pos {
        self.version += 1;
        let end = end_of(p, s);
        let at = byte_of(self.line(p.line), p.col);
        if !s.contains('\n') {
            self.lines[p.line].insert_str(at, s);
            return end;
        }
        let tail = self.lines[p.line].split_off(at);
        let mut pieces = s.split('\n');
        self.lines[p.line].push_str(pieces.next().unwrap_or(""));
        let mut at_line = p.line;
        let rest: Vec<&str> = pieces.collect();
        for (i, piece) in rest.iter().enumerate() {
            at_line += 1;
            let mut l = String::from(*piece);
            if i + 1 == rest.len() {
                l.push_str(&tail);
            }
            self.lines.insert(at_line, l);
        }
        end
    }

    fn sel(&self) -> Sel {
        Sel {
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }

    /// Start (or continue) an undo group for an edit of this kind.
    fn begin(&mut self, kind: Kind) {
        let joins = kind != Kind::Other
            && self.open == Some(kind)
            && self
                .undo
                .last()
                .is_some_and(|g| g.after.cursor == self.cursor)
            && self.anchor.is_none();
        if !joins {
            let id = self.next_id;
            self.next_id += 1;
            let before = self.sel();
            self.undo.push(Group {
                id,
                edits: Vec::new(),
                before,
                after: before,
            });
            if self.undo.len() > MAX_UNDO {
                self.base_id = self.undo.remove(0).id;
            }
            self.redo.clear();
        }
        self.open = Some(kind);
    }

    /// Replace `a..b` by `s` as part of the open group, and record it.
    fn apply(&mut self, a: Pos, b: Pos, s: &str) -> Pos {
        let removed = self.raw_delete(a, b);
        let end = self.raw_insert(a, s);
        if let Some(g) = self.undo.last_mut() {
            g.edits.push(Edit {
                at: a,
                removed,
                inserted: String::from(s),
            });
        }
        end
    }

    fn finish(&mut self) {
        let s = self.sel();
        if let Some(g) = self.undo.last_mut() {
            g.after = s;
        }
    }

    /// Delete the selection inside the open group. True if there was one.
    fn drop_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        self.apply(a, b, "");
        self.cursor = a;
        self.anchor = None;
        self.want_col = a.col;
        true
    }

    // ---- editing --------------------------------------------------------------------------

    /// Insert text at the cursor, replacing the selection. `\r\n` and lone `\r` become line
    /// breaks. One undo step unless it is a single plain character typed after other typing.
    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() && self.selection().is_none() {
            return;
        }
        let norm;
        let s = if s.contains('\r') {
            norm = s.replace("\r\n", "\n").replace('\r', "\n");
            norm.as_str()
        } else {
            s
        };
        let single = chars(s) == 1 && !s.contains('\n');
        let kind = if single && self.selection().is_none() {
            Kind::Typing
        } else {
            Kind::Other
        };
        self.begin(kind);
        self.drop_selection();
        let end = self.apply(self.cursor, self.cursor, s);
        self.cursor = end;
        self.want_col = end.col;
        self.finish();
        if kind == Kind::Other {
            self.open = None;
        }
    }

    pub fn insert_char(&mut self, c: char) {
        let mut b = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut b));
    }

    /// Enter: a line break, with the current line's indentation if `auto_indent` is on.
    pub fn newline(&mut self) {
        let mut s = String::from("\n");
        if self.auto_indent {
            let start = self.selection().map_or(self.cursor, |(a, _)| a);
            let l = self.line(start.line);
            let ws: String = l
                .chars()
                .take(start.col)
                .take_while(|c| *c == ' ' || *c == '\t')
                .collect();
            s.push_str(&ws);
        }
        self.insert_str(&s);
    }

    /// Backspace: the selection, else the char before the cursor (joining lines at column 0).
    pub fn backspace(&mut self) {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        let c = self.cursor;
        let prev = if c.col > 0 {
            Pos::new(c.line, c.col - 1)
        } else if c.line > 0 {
            Pos::new(c.line - 1, self.line_len(c.line - 1))
        } else {
            return;
        };
        self.begin(Kind::Backspace);
        self.apply(prev, c, "");
        self.cursor = prev;
        self.want_col = prev.col;
        self.finish();
    }

    /// Delete: the selection, else the char after the cursor (joining lines at the end).
    pub fn delete_forward(&mut self) {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        let c = self.cursor;
        let next = if c.col < self.line_len(c.line) {
            Pos::new(c.line, c.col + 1)
        } else if c.line + 1 < self.lines.len() {
            Pos::new(c.line + 1, 0)
        } else {
            return;
        };
        self.begin(Kind::Delete);
        self.apply(c, next, "");
        self.finish();
    }

    /// Ctrl+Backspace: the word before the cursor.
    pub fn delete_word_back(&mut self) {
        if self.selection().is_some() {
            return self.delete_selection();
        }
        let p = self.word_left_of(self.cursor);
        if p == self.cursor {
            return;
        }
        self.begin(Kind::Other);
        self.apply(p, self.cursor, "");
        self.cursor = p;
        self.want_col = p.col;
        self.finish();
        self.open = None;
    }

    pub fn delete_selection(&mut self) {
        if self.selection().is_none() {
            self.anchor = None;
            return;
        }
        self.begin(Kind::Other);
        self.drop_selection();
        self.finish();
        self.open = None;
    }

    /// Cut: remove the selection and return it (for the clipboard).
    pub fn cut(&mut self) -> Option<String> {
        let t = self.selected_text()?;
        self.delete_selection();
        Some(t)
    }

    /// Delete the line(s) the cursor or selection touches.
    pub fn delete_lines(&mut self) {
        let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
        // A selection that ends at column 0 of a later line doesn't touch that line.
        let last = if b.col == 0 && b.line > a.line {
            b.line - 1
        } else {
            b.line
        };
        let (from, to) = if last + 1 < self.lines.len() {
            (Pos::new(a.line, 0), Pos::new(last + 1, 0))
        } else if a.line > 0 {
            (
                Pos::new(a.line - 1, self.line_len(a.line - 1)),
                self.doc_end(),
            )
        } else {
            (Pos::new(0, 0), self.doc_end())
        };
        self.begin(Kind::Other);
        self.apply(from, to, "");
        self.anchor = None;
        self.cursor = self.clamp(Pos::new(a.line, 0));
        self.want_col = 0;
        self.finish();
        self.open = None;
    }

    /// Tab with a selection: indent every touched line by `unit`; returns false (and does
    /// nothing) if there is no multi-line selection, so the caller can insert a tab instead.
    pub fn indent_lines(&mut self, unit: &str) -> bool {
        let Some((a, b)) = self.selection() else {
            return false;
        };
        if a.line == b.line {
            return false;
        }
        let last = if b.col == 0 { b.line - 1 } else { b.line };
        let u = chars(unit);
        self.begin(Kind::Other);
        for line in a.line..=last {
            self.apply(Pos::new(line, 0), Pos::new(line, 0), unit);
        }
        let (mut na, mut nb) = (a, b);
        na.col += u;
        if b.col > 0 {
            nb.col += u;
        }
        self.anchor = Some(if self.anchor == Some(a) { na } else { nb });
        self.cursor = if self.cursor == a { na } else { nb };
        self.finish();
        self.open = None;
        true
    }

    /// Shift+Tab: remove up to `width` columns of leading spaces (or one tab) from the
    /// cursor's line, or from every touched line of a selection.
    pub fn dedent_lines(&mut self, width: usize) {
        let (a, b) = self.selection().unwrap_or((self.cursor, self.cursor));
        let last = if b.col == 0 && b.line > a.line {
            b.line - 1
        } else {
            b.line
        };
        let mut edits: Vec<(usize, usize)> = Vec::new();
        for line in a.line..=last {
            let l = self.line(line);
            let n = if l.starts_with('\t') {
                1
            } else {
                l.chars().take(width).take_while(|c| *c == ' ').count()
            };
            if n > 0 {
                edits.push((line, n));
            }
        }
        if edits.is_empty() {
            return;
        }
        self.begin(Kind::Other);
        for &(line, n) in &edits {
            self.apply(Pos::new(line, 0), Pos::new(line, n), "");
        }
        self.cursor = dedented(&edits, self.cursor);
        self.anchor = self.anchor.map(|a| dedented(&edits, a));
        self.want_col = self.cursor.col;
        self.finish();
        self.open = None;
    }

    // ---- undo / redo ----------------------------------------------------------------------

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Undo one group. False if there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(g) = self.undo.pop() else {
            return false;
        };
        for e in g.edits.iter().rev() {
            let end = end_of(e.at, &e.inserted);
            self.raw_delete(e.at, end);
            self.raw_insert(e.at, &e.removed);
        }
        self.cursor = g.before.cursor;
        self.anchor = g.before.anchor;
        self.want_col = self.cursor.col;
        self.redo.push(g);
        self.open = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(g) = self.redo.pop() else {
            return false;
        };
        for e in &g.edits {
            let end = end_of(e.at, &e.removed);
            self.raw_delete(e.at, end);
            self.raw_insert(e.at, &e.inserted);
        }
        self.cursor = g.after.cursor;
        self.anchor = g.after.anchor;
        self.want_col = self.cursor.col;
        self.undo.push(g);
        self.open = None;
        true
    }

    // ---- find and replace -----------------------------------------------------------------

    /// All matches of `needle` on one line, as (start col, end col), not overlapping.
    fn matches_in(&self, line: usize, needle: &[char], o: FindOpts) -> Vec<(usize, usize)> {
        let cs: Vec<char> = self.line(line).chars().collect();
        let mut out = Vec::new();
        if needle.is_empty() || needle.len() > cs.len() {
            return out;
        }
        let same = |a: char, b: char| {
            if o.case_sensitive {
                a == b
            } else {
                fold(a) == fold(b)
            }
        };
        let mut i = 0;
        while i + needle.len() <= cs.len() {
            let hit = (0..needle.len()).all(|k| same(cs[i + k], needle[k]));
            let j = i + needle.len();
            let word_ok = !o.whole_word
                || ((i == 0 || !is_word(cs[i - 1])) && (j == cs.len() || !is_word(cs[j])));
            if hit && word_ok {
                out.push((i, j));
                i = j;
            } else {
                i += 1;
            }
        }
        out
    }

    /// The first match at or after `from` (wrapping round to the top). A needle with a line
    /// break matches nothing: search is per line.
    pub fn find_next(&self, needle: &str, o: FindOpts, from: Pos) -> Option<(Pos, Pos)> {
        if needle.is_empty() || needle.contains('\n') {
            return None;
        }
        let n: Vec<char> = needle.chars().collect();
        let from = self.clamp(from);
        let count = self.lines.len();
        for k in 0..=count {
            let line = (from.line + k) % count;
            for (s, e) in self.matches_in(line, &n, o) {
                let hit = Pos::new(line, s);
                // First line: only matches starting at/after `from`. Wrapped last visit
                // (k == count): only those before it.
                let ok = if k == 0 {
                    hit >= from
                } else if k == count {
                    hit < from
                } else {
                    true
                };
                if ok {
                    return Some((hit, Pos::new(line, e)));
                }
            }
        }
        None
    }

    /// The last match that ends at or before `from` (wrapping round to the bottom).
    pub fn find_prev(&self, needle: &str, o: FindOpts, from: Pos) -> Option<(Pos, Pos)> {
        if needle.is_empty() || needle.contains('\n') {
            return None;
        }
        let n: Vec<char> = needle.chars().collect();
        let from = self.clamp(from);
        let count = self.lines.len();
        for k in 0..=count {
            let line = (from.line + count - (k % count)) % count;
            for (s, e) in self.matches_in(line, &n, o).into_iter().rev() {
                let end = Pos::new(line, e);
                let ok = if k == 0 {
                    end <= from
                } else if k == count {
                    end > from
                } else {
                    true
                };
                if ok {
                    return Some((Pos::new(line, s), end));
                }
            }
        }
        None
    }

    /// Select the next match after the cursor/selection (or the previous one, if `back`).
    /// Returns whether one was found.
    pub fn find_select(&mut self, needle: &str, o: FindOpts, back: bool) -> bool {
        let hit = if back {
            let from = self.selection().map_or(self.cursor, |(a, _)| a);
            self.find_prev(needle, o, from)
        } else {
            let from = self.selection().map_or(self.cursor, |(_, b)| b);
            self.find_next(needle, o, from)
        };
        match hit {
            Some((a, b)) => {
                self.select(a, b);
                true
            }
            None => false,
        }
    }

    /// The matches of `needle` on one line, as (start col, end col).
    pub fn find_in_line(&self, line: usize, needle: &str, o: FindOpts) -> Vec<(usize, usize)> {
        if needle.is_empty() || needle.contains('\n') {
            return Vec::new();
        }
        let n: Vec<char> = needle.chars().collect();
        self.matches_in(line, &n, o)
    }

    /// How many matches of `needle` there are in the whole buffer.
    pub fn count_matches(&self, needle: &str, o: FindOpts) -> usize {
        if needle.is_empty() || needle.contains('\n') {
            return 0;
        }
        let n: Vec<char> = needle.chars().collect();
        (0..self.lines.len())
            .map(|l| self.matches_in(l, &n, o).len())
            .sum()
    }

    /// Replace the selection with `with` if the selection is itself a match of `needle`, then
    /// select the next match. Returns whether a replacement was made.
    pub fn replace_current(&mut self, needle: &str, with: &str, o: FindOpts) -> bool {
        if let Some((a, b)) = self.selection()
            && a.line == b.line
        {
            let n: Vec<char> = needle.chars().collect();
            if self.matches_in(a.line, &n, o).contains(&(a.col, b.col)) {
                self.insert_str(with);
                self.find_select(needle, o, false);
                return true;
            }
        }
        self.find_select(needle, o, false);
        false
    }

    /// Replace every match; one undo step. Returns how many were replaced.
    pub fn replace_all(&mut self, needle: &str, with: &str, o: FindOpts) -> usize {
        if needle.is_empty() || needle.contains('\n') || with.contains('\n') {
            return 0;
        }
        let n: Vec<char> = needle.chars().collect();
        let mut hits: Vec<(usize, usize, usize)> = Vec::new();
        for line in 0..self.lines.len() {
            for (s, e) in self.matches_in(line, &n, o) {
                hits.push((line, s, e));
            }
        }
        if hits.is_empty() {
            return 0;
        }
        self.begin(Kind::Other);
        // Back to front, so earlier positions stay valid while we edit.
        for &(line, s, e) in hits.iter().rev() {
            self.apply(Pos::new(line, s), Pos::new(line, e), with);
        }
        self.anchor = None;
        self.cursor = self.clamp(self.cursor);
        self.finish();
        self.open = None;
        hits.len()
    }
}

#[cfg(test)]
mod tests;
