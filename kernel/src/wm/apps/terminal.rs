//! Terminal: espresso window with a scrollback that follows the kernel log live, and a line
//! editor feeding a [`Shell`].
//!
//! The shell is a hook: today [`BuiltinShell`] answers a handful of commands in-kernel. Once
//! `proc` and `vfs` land, implement [`Shell`] for a proxy that writes the line to a userland
//! `/Commands/msh` process's stdin and appends whatever it prints (and `run` can return before the
//! command finishes: push output later from `tick`).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{self, InodeType};
use crate::gfx::{Color, Rect, rgb};
use crate::{heap, klog, pmm, timer};

use super::super::clipboard;
use super::super::event::Key;
use super::super::paint::{Painter, hit};
use super::super::session::user_name;
use super::super::theme::*;
use super::super::tty;
use super::super::window::{
    App, KeyInput, Part, PointerInput, PointerKind, Strip, WindowContents, paint_strip, plus_rect,
    tab_close_rect, tab_rect,
};
use super::super::{Mode, Request, request, rtc, shell::fmt_uptime};
use super::fsx;

const MAX_LINES: usize = 2000;

/// Commands to run in the Terminal (Skim's `> cmd`), typed into the shell on its next tick.
static COMMANDS: crate::sync::SpinLock<Vec<String>> = crate::sync::SpinLock::new(Vec::new());

pub fn queue_command(cmd: &str) {
    COMMANDS.lock().push(String::from(cmd));
}

/// New tabs asked for from outside (Skim's `> cmd`, the "New Terminal tab" action) while a
/// Terminal window is open: the window opens them on its next tick.
static NEW_TABS: crate::sync::SpinLock<Vec<Option<String>>> =
    crate::sync::SpinLock::new(Vec::new());

pub fn queue_tab(cmd: Option<String>) {
    NEW_TABS.lock().push(cmd);
}

/// The body's tab strip (SPEC Tabs: 38 high), when the Terminal shares its window.
const STRIP_H: i32 = super::super::window::STRIP_H;
const PAD: i32 = 14;
const LINE_H: i32 = 18;

/// Output sink a shell writes to.
pub struct Output<'a> {
    lines: &'a mut Vec<(String, Color)>,
}

impl Output<'_> {
    pub fn line(&mut self, s: &str, c: Color) {
        for l in s.split('\n') {
            self.lines.push((String::from(l), c));
        }
    }
    pub fn out(&mut self, s: &str) {
        self.line(s, TERM_DIM);
    }
    pub fn good(&mut self, s: &str) {
        self.line(s, MATCHA);
    }
    pub fn bad(&mut self, s: &str) {
        self.line(s, STRAW);
    }
}

pub enum ShellResult {
    Done,
    /// Clear the scrollback.
    Clear,
}

/// What the terminal runs lines with. See the module docs for the userland plan.
pub trait Shell {
    fn prompt(&self) -> &str;
    fn run(&mut self, line: &str, out: &mut Output) -> ShellResult;
}

/// One tab: its scrollback, line editor or tty, escape parser and selection.
pub struct Tab {
    lines: Vec<(String, Color)>,
    /// Partial kernel-log line not yet ended by '\n'.
    partial: String,
    log_pos: u64,
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_pos: usize,
    /// Rows scrolled back from the bottom.
    back: usize,
    shell: Box<dyn Shell>,
    /// The tty its `msh` runs on (1..=9; keys go to it, its output is drawn here), or 0 for
    /// the built-in shell.
    tty: usize,
    /// Shown on the tab: "msh", or the command it was opened for.
    label: String,
    /// Follows the kernel log (the first tab).
    log: bool,
    /// A command to type once its msh is up (its first output: the prompt).
    pending: Option<String>,
    tty_gen: u64,
    /// The tty's current (unterminated) output line: the prompt and what is being typed.
    cur: String,
    /// ANSI escape parser state: 0 ground, 1 after ESC, 2 in CSI (ESC [), 3 ESC O.
    esc: u8,
    /// CSI parameter bytes collected so far.
    csi: String,
    /// SGR state: colour index (0 = default) and bold.
    sgr_fg: u8,
    sgr_bold: bool,
    /// A bare CR was seen: the next text overwrites the current line.
    cr: bool,
    /// The rows on screen at the last draw (for copying a selection).
    screen: Vec<String>,
    /// Mouse selection in screen cells: anchor and end (row, col). Cleared by new output.
    sel: Option<((usize, usize), (usize, usize))>,
}

impl Tab {
    fn blank(tty: usize, log: bool) -> Tab {
        Tab {
            lines: Vec::new(),
            partial: String::new(),
            log_pos: 0,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_pos: 0,
            back: 0,
            shell: Box::new(BuiltinShell::new()),
            tty,
            label: String::from("msh"),
            log,
            pending: None,
            tty_gen: 0,
            cur: String::new(),
            esc: 0,
            csi: String::new(),
            sgr_fg: 0,
            sgr_bold: false,
            cr: false,
            screen: Vec::new(),
            sel: None,
        }
    }

    /// The first tab: the kernel log, then msh on /dev/tty1.
    fn first() -> Tab {
        let mut t = Tab::blank(0, true);
        t.follow_log();
        t.lines.push((String::new(), TERM_DIM));
        if tty::start_shell(1) {
            t.tty = 1;
        }
        let banner = if t.tty != 0 {
            concat!(
                "milkOS ",
                env!("MILKOS_VERSION"),
                ": msh on /dev/tty1, the kernel log above (live)."
            )
        } else {
            concat!(
                "milkOS ",
                env!("MILKOS_VERSION"),
                ": the kernel log above (live). Type 'help'."
            )
        };
        t.lines.push((String::from(banner), MATCHA));
        t
    }

    /// Another tab, with msh on tty `n` (2..=9) or the built-in shell (`n` = 0).
    fn extra(n: usize) -> Tab {
        let mut t = Tab::blank(n, false);
        let banner = if n != 0 {
            format!("msh on {}", tty::path(n))
        } else {
            String::from(concat!(
                "milkOS ",
                env!("MILKOS_VERSION"),
                " terminal. Type 'help'."
            ))
        };
        t.lines.push((banner, MATCHA));
        t
    }

    /// Type `cmd` and Enter into this tab's shell.
    fn run_command(&mut self, cmd: &str) {
        if self.tty != 0 {
            for b in cmd.bytes() {
                tty::input(self.tty, b);
            }
            tty::input(self.tty, b'\r');
        } else {
            self.input = String::from(cmd);
            self.cursor = self.input.chars().count();
            self.submit();
        }
    }

    /// Draw what msh wrote: lines into the scrollback, the unfinished one stays in `cur`.
    fn follow_tty(&mut self) -> bool {
        let Some(bytes) = tty::take_output(self.tty, &mut self.tty_gen) else {
            return false;
        };
        for c in String::from_utf8_lossy(&bytes).chars() {
            match (self.esc, c) {
                (1, '[') => {
                    self.esc = 2;
                    self.csi.clear();
                }
                (1, 'O') => self.esc = 3,
                (1, 'c') => {
                    // RIS: full reset.
                    self.esc = 0;
                    self.clear_screen();
                }
                (1, _) | (3, _) => self.esc = 0,
                (2, '\x40'..='\x7e') => {
                    self.esc = 0;
                    let params = core::mem::take(&mut self.csi);
                    self.csi_final(c, &params);
                }
                (2, c) => self.csi.push(c),
                (_, '\x1b') => self.esc = 1,
                (_, '\n') => {
                    self.cr = false;
                    let mut l = core::mem::take(&mut self.cur);
                    // The next line starts in the colour this one ended in.
                    if self.sgr_fg != 0 || self.sgr_bold {
                        self.cur.push(marker(self.sgr_fg, self.sgr_bold));
                        l.push(marker(0, false));
                    }
                    self.lines.push((l, TERM_FG));
                }
                (_, '\x08') => {
                    self.cur.pop();
                    // Don't leave a dangling colour marker as the "character" to erase next.
                    while self.cur.chars().last().is_some_and(is_marker) {
                        self.cur.pop();
                    }
                }
                (_, '\r') => self.cr = true,
                (_, '\x07') => {}
                (_, '\t') => {
                    self.take_cr();
                    let n = visible_len(&self.cur);
                    for _ in 0..(8 - n % 8) {
                        self.cur.push(' ');
                    }
                }
                (_, c) if !c.is_control() => {
                    self.take_cr();
                    self.cur.push(c);
                }
                _ => {}
            }
        }
        self.trim();
        self.back = 0;
        true
    }

    /// A bare CR followed by text: the text replaces the line (progress bars, `\r\x1b[K`).
    fn take_cr(&mut self) {
        if self.cr {
            self.cr = false;
            self.cur.clear();
            if self.sgr_fg != 0 || self.sgr_bold {
                self.cur.push(marker(self.sgr_fg, self.sgr_bold));
            }
        }
    }

    fn clear_screen(&mut self) {
        self.lines.clear();
        self.cur.clear();
        self.sel = None;
        self.sgr_fg = 0;
        self.sgr_bold = false;
    }

    /// The CSI sequences we act on: SGR colours (m), erase in display (J: 2/3 clear the
    /// screen), erase in line (K), cursor home (H, f). Cursor movement is ignored: this is a
    /// scrolling terminal, not a full screen one (yet).
    fn csi_final(&mut self, f: char, params: &str) {
        let nums: Vec<u32> = params
            .split(';')
            .map(|p| p.trim_start_matches('?').parse().unwrap_or(0))
            .collect();
        match f {
            'm' => {
                let mut i = 0;
                while i < nums.len() {
                    match nums[i] {
                        0 => {
                            self.sgr_fg = 0;
                            self.sgr_bold = false;
                        }
                        1 => self.sgr_bold = true,
                        22 => self.sgr_bold = false,
                        n @ 30..=37 => self.sgr_fg = (n - 30 + 1) as u8,
                        n @ 90..=97 => self.sgr_fg = (n - 90 + 9) as u8,
                        39 => self.sgr_fg = 0,
                        // 256-colour / truecolour: skip their arguments.
                        38 | 48 => {
                            i += if nums.get(i + 1) == Some(&5) { 2 } else { 4 };
                        }
                        _ => {}
                    }
                    i += 1;
                }
                self.cur.push(marker(self.sgr_fg, self.sgr_bold));
            }
            'J' if nums.first().is_some_and(|&n| n >= 2) => self.clear_screen(),
            'J' => {}
            'K' => {
                // Erase to the end of the line: the cursor is at the end, unless a CR moved
                // it to the start, in which case the whole line goes.
                if self.cr {
                    self.cr = false;
                    self.cur.clear();
                }
            }
            'H' | 'f' => {
                // Cursor home: on its own it's followed by J (clear) in practice.
                self.cr = true;
            }
            _ => {}
        }
    }

    fn sel_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = self.sel?;
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    fn selected_text(&self) -> String {
        let Some((a, b)) = self.sel_range() else {
            return String::new();
        };
        let mut out = String::new();
        for (r, row) in self.screen.iter().enumerate().take(b.0 + 1).skip(a.0) {
            let l: Vec<char> = row.chars().collect();
            let c0 = if r == a.0 { a.1.min(l.len()) } else { 0 };
            let c1 = if r == b.0 { b.1.min(l.len()) } else { l.len() };
            out.extend(&l[c0..c1.max(c0)]);
            if r != b.0 {
                out.push('\n');
            }
        }
        String::from(out.trim_end())
    }

    /// Type `text` into the shell (as if typed; newlines are Enter).
    fn paste(&mut self, text: &str) {
        if self.tty != 0 {
            for b in text.bytes() {
                tty::input(self.tty, if b == b'\n' { b'\r' } else { b });
            }
        } else {
            for c in text.chars() {
                if c == '\n' {
                    self.submit();
                } else if !c.is_control() {
                    let b = self.byte(self.cursor);
                    self.input.insert(b, c);
                    self.cursor += 1;
                }
            }
        }
    }

    /// A key typed into the window, as tty bytes.
    fn key_to_tty(&mut self, k: &KeyInput) -> bool {
        let n = self.tty;
        if k.mods.ctrl {
            match k.key {
                // Ctrl+L clears the window, unless a raw-mode program wants the key.
                Key::Char('l') if tty::canonical(n) => {
                    self.lines.clear();
                    return true;
                }
                Key::Char(c) if c.is_ascii_alphabetic() => {
                    tty::input(n, c.to_ascii_lowercase() as u8 - b'a' + 1);
                }
                _ => return false,
            }
            return false;
        }
        match k.key {
            Key::Enter => tty::input(n, b'\r'),
            Key::Backspace => tty::input(n, 0x7f),
            Key::Tab => tty::input(n, b'\t'),
            Key::Escape => tty::input(n, 0x1b),
            Key::Up | Key::Down | Key::Right | Key::Left | Key::Home | Key::End => {
                // xterm sequences: swallowed by the line discipline in cooked mode, read by
                // programs in raw mode.
                let f = match k.key {
                    Key::Up => b'A',
                    Key::Down => b'B',
                    Key::Right => b'C',
                    Key::Left => b'D',
                    Key::Home => b'H',
                    _ => b'F',
                };
                for b in [0x1b, b'[', f] {
                    tty::input(n, b);
                }
            }
            Key::Delete => {
                for b in *b"\x1b[3~" {
                    tty::input(n, b);
                }
            }
            Key::PageUp => {
                self.back += 8;
                return true;
            }
            _ => {
                if let Some(c) = k.ch {
                    let mut b = [0u8; 4];
                    for &x in c.encode_utf8(&mut b).as_bytes() {
                        tty::input(n, x);
                    }
                }
            }
        }
        false
    }

    /// Pull new kernel log text into the scrollback. True if anything arrived.
    fn follow_log(&mut self) -> bool {
        let mut got = String::new();
        self.log_pos = klog::read_since(self.log_pos, |b| {
            got.push_str(core::str::from_utf8(b).unwrap_or("?"));
        });
        if got.is_empty() {
            return false;
        }
        self.partial.push_str(&got);
        while let Some(i) = self.partial.find('\n') {
            let line: String = self.partial[..i].chars().filter(|c| *c != '\r').collect();
            self.partial.drain(..=i);
            self.lines.push((line, TERM_DIM));
        }
        self.trim();
        true
    }

    fn trim(&mut self) {
        if self.lines.len() > MAX_LINES {
            let n = self.lines.len() - MAX_LINES;
            self.lines.drain(..n);
        }
    }

    fn submit(&mut self) {
        let line = core::mem::take(&mut self.input);
        self.cursor = 0;
        self.lines
            .push((format!("{}{}", self.shell.prompt(), line), TERM_FG));
        if !line.trim().is_empty() {
            self.history.push(line.clone());
        }
        self.hist_pos = self.history.len();
        let mut out = Output {
            lines: &mut self.lines,
        };
        if let ShellResult::Clear = self.shell.run(line.trim(), &mut out) {
            self.lines.clear();
        }
        self.trim();
        self.back = 0;
    }

    fn byte(&self, col: usize) -> usize {
        self.input
            .char_indices()
            .nth(col)
            .map_or(self.input.len(), |(i, _)| i)
    }
}

// ---- ANSI colour markers ------------------------------------------------------------------
// Coloured output is kept in the scrollback as plain strings with marker characters from the
// Private Use Area (U+F700 + colour index, + 0x20 when bold) where the colour changes. They take
// no space when wrapping or drawing, and copy-paste strips them.

const MARK: u32 = 0xF700;

fn marker(fg: u8, bold: bool) -> char {
    char::from_u32(MARK + fg as u32 + if bold { 0x20 } else { 0 }).unwrap_or(' ')
}

fn is_marker(c: char) -> bool {
    (MARK..MARK + 0x40).contains(&(c as u32))
}

fn visible_len(s: &str) -> usize {
    s.chars().filter(|&c| !is_marker(c)).count()
}

pub fn strip_markers(s: &str) -> String {
    s.chars().filter(|&c| !is_marker(c)).collect()
}

/// ANSI colour `i` (1..=16; 0 = the line's own colour) for the espresso Terminal.
fn ansi(i: u32, bold: bool, base: Color) -> Color {
    let i = if bold && (1..=8).contains(&i) {
        i + 8
    } else {
        i
    };
    match i {
        1 => rgb(0x6A, 0x58, 0x4C),  // black (lifted, so it shows on espresso)
        2 => STRAW,                  // red
        3 => MATCHA,                 // green
        4 => HONEY,                  // yellow
        5 => BLUE,                   // blue
        6 => LAV,                    // magenta
        7 => rgb(0x8C, 0xC7, 0xC0),  // cyan
        8 => rgb(0xE6, 0xDA, 0xC8),  // white
        9 => rgb(0x9A, 0x86, 0x74),  // bright black
        10 => rgb(0xF2, 0xA7, 0xB0), // bright red
        11 => rgb(0xB8, 0xD8, 0xB6), // bright green
        12 => rgb(0xF4, 0xCD, 0x80), // bright yellow
        13 => rgb(0xA6, 0xB4, 0xDF), // bright blue
        14 => rgb(0xD2, 0xC6, 0xE8), // bright magenta
        15 => rgb(0xAE, 0xDE, 0xD8), // bright cyan
        16 => rgb(0xFF, 0xFF, 0xFF), // bright white
        _ => base,
    }
}

/// Draw a scrollback row with its colour markers; `start` is the colour active at its start.
fn draw_row(
    p: &mut Painter,
    x: i32,
    y: i32,
    s: &str,
    base: Color,
    start: (u32, bool),
) -> (u32, bool) {
    let (mut fg, mut bold) = start;
    let mut run = String::new();
    let mut cx = x;
    let flush = |p: &mut Painter, run: &mut String, cx: &mut i32, fg: u32, bold: bool| {
        if !run.is_empty() {
            let c = ansi(fg, bold, base);
            p.mono(*cx, y, run, c);
            if bold {
                p.mono(*cx + 1, y, run, c);
            }
            *cx += 8 * run.chars().count() as i32;
            run.clear();
        }
    };
    for ch in s.chars() {
        if is_marker(ch) {
            flush(p, &mut run, &mut cx, fg, bold);
            let v = ch as u32 - MARK;
            fg = v & 0x1f;
            bold = v & 0x20 != 0;
        } else {
            run.push(ch);
        }
    }
    flush(p, &mut run, &mut cx, fg, bold);
    (fg, bold)
}

/// Split `s` into rows of at most `cols` visible characters (markers don't count).
fn wrap(s: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut row = String::new();
    let mut n = 0;
    for c in s.chars() {
        if !is_marker(c) {
            if n == cols {
                out.push(core::mem::take(&mut row));
                n = 0;
            }
            n += 1;
        }
        row.push(c);
    }
    out.push(row);
    out
}

impl Tab {
    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, focused: bool) {
        let cols = ((w - 2 * PAD) / 8).max(4) as usize;
        let rows = ((h - 2 * PAD + 2) / LINE_H).max(1) as usize;
        if self.tty != 0 {
            // Tell programs on the tty (TIOCGWINSZ); SIGWINCH only on a real change.
            tty::set_winsize(self.tty, rows as u16, cols as u16);
        }
        // The prompt line (with the cursor) is the last visual row.
        let prompt = if self.tty != 0 {
            self.cur.clone()
        } else {
            format!("{}{}", self.shell.prompt(), self.input)
        };
        let prompt_rows = wrap(&prompt, cols);
        let mut vis: Vec<(String, Color)> = Vec::new();
        let need = rows + self.back;
        for (l, c) in self.lines.iter().rev() {
            if vis.len() + prompt_rows.len() >= need {
                break;
            }
            for seg in wrap(l, cols).into_iter().rev() {
                vis.push((seg, *c));
            }
        }
        vis.reverse();
        for seg in &prompt_rows {
            vis.push((seg.clone(), TERM_FG));
        }
        let max_back = vis.len().saturating_sub(rows);
        self.back = self.back.min(max_back);
        let end = vis.len() - self.back;
        let start = end.saturating_sub(rows);
        self.screen = vis[start..end]
            .iter()
            .map(|(s, _)| strip_markers(s))
            .collect();
        if let Some((a, b)) = self.sel_range() {
            for r in a.0..=b.0.min(self.screen.len().saturating_sub(1)) {
                let c0 = if r == a.0 { a.1 } else { 0 };
                let c1 = if r == b.0 { b.1 } else { cols };
                if c1 > c0 {
                    p.fill_a(
                        PAD + c0 as i32 * 8,
                        PAD + r as i32 * LINE_H - 1,
                        (c1 - c0) as i32 * 8,
                        LINE_H,
                        STRAW,
                        SEL_ROW_A,
                    );
                }
            }
        }
        // Colour carries across wrapped rows of the same line, so track it from the top of
        // the visible window (rows above `start` may set it; good enough to start plain).
        let mut col = (0u32, false);
        for (k, (s, c)) in vis[start..end].iter().enumerate() {
            col = draw_row(p, PAD, PAD + k as i32 * LINE_H, s, *c, col);
            if !s.is_empty() && k + 1 < end - start && vis[start + k + 1].1 != *c {
                col = (0, false);
            }
        }
        // Cursor block.
        if self.back == 0 {
            let pos = if self.tty != 0 {
                visible_len(&self.cur)
            } else {
                self.shell.prompt().chars().count() + self.cursor
            };
            let (r, ccol) = (pos / cols, pos % cols);
            let row_on_screen = (vis.len() - prompt_rows.len() + r) as i32 - start as i32;
            let (x, y) = (PAD + ccol as i32 * 8, PAD + row_on_screen * LINE_H);
            if focused {
                p.fill(x, y, 8, 16, TERM_FG);
                let ch = self.input.chars().nth(self.cursor).unwrap_or(' ');
                let mut b = [0u8; 4];
                p.mono(x, y, ch.encode_utf8(&mut b), TERM_BG);
            } else {
                p.rrect_stroke(x, y, 8, 16, 0, 1, TERM_DIM, 255);
            }
        }
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        // Copy / paste: Cmd+C / Cmd+V, or Ctrl+Shift+C / V (plain Ctrl+C is the interrupt).
        if let Key::Char(c @ ('c' | 'v')) = k.key
            && (k.mods.meta || (k.mods.ctrl && k.mods.shift))
        {
            if c == 'c' {
                let t = self.selected_text();
                if !t.is_empty() {
                    clipboard::set(&t);
                }
                self.sel = None;
                return true;
            }
            let text = clipboard::get();
            self.paste(&text);
            return true;
        }
        self.sel = None;
        self.back = 0;
        if self.tty != 0 {
            return self.key_to_tty(k);
        }
        if k.mods.ctrl {
            match k.key {
                Key::Char('c') => {
                    let line = format!("{}{}^C", self.shell.prompt(), self.input);
                    self.lines.push((line, TERM_FG));
                    self.input.clear();
                    self.cursor = 0;
                }
                Key::Char('l') => self.lines.clear(),
                Key::Char('a') => self.cursor = 0,
                Key::Char('e') => self.cursor = self.input.chars().count(),
                Key::Char('u') => {
                    self.input.clear();
                    self.cursor = 0;
                }
                _ => return false,
            }
            return true;
        }
        match k.key {
            Key::Enter => self.submit(),
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let b = self.byte(self.cursor);
                    self.input.remove(b);
                }
            }
            Key::Delete => {
                if self.cursor < self.input.chars().count() {
                    let b = self.byte(self.cursor);
                    self.input.remove(b);
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.input.chars().count()),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = self.input.chars().count(),
            Key::Up => {
                if self.hist_pos > 0 {
                    self.hist_pos -= 1;
                    self.input = self.history[self.hist_pos].clone();
                    self.cursor = self.input.chars().count();
                }
            }
            Key::Down => {
                if self.hist_pos + 1 < self.history.len() {
                    self.hist_pos += 1;
                    self.input = self.history[self.hist_pos].clone();
                } else {
                    self.hist_pos = self.history.len();
                    self.input.clear();
                }
                self.cursor = self.input.chars().count();
            }
            Key::PageUp => self.back += 8,
            Key::PageDown => {}
            _ => match k.ch {
                Some(c) if !c.is_control() && self.input.len() < 200 => {
                    let b = self.byte(self.cursor);
                    self.input.insert(b, c);
                    self.cursor += 1;
                }
                _ => return false,
            },
        }
        true
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let rows = self.screen.len().max(1);
        let r = ((e.y - PAD).max(0) / LINE_H) as usize;
        let c = ((e.x - PAD + 4).max(0) / 8) as usize;
        let cell = (r.min(rows - 1), c);
        match e.kind {
            PointerKind::Down(_) => {
                self.sel = Some((cell, cell));
                true
            }
            PointerKind::Drag => {
                if let Some((a, _)) = self.sel {
                    self.sel = Some((a, cell));
                }
                true
            }
            PointerKind::Up(_) => {
                if self.sel.is_some_and(|(a, b)| a == b) {
                    self.sel = None;
                    return true;
                }
                false
            }
        }
    }

    fn scroll(&mut self, dy: i32) -> bool {
        self.sel = None;
        let old = self.back;
        self.back = (self.back as i32 - dy * 3).max(0) as usize;
        old != self.back
    }

    fn tick(&mut self, _now: u64) -> bool {
        let log = self.log && self.follow_log();
        let tty = self.tty != 0 && self.follow_tty();
        if tty && let Some(c) = self.pending.take() {
            self.run_command(&c);
        }
        if log || tty {
            self.sel = None; // rows moved under it
        }
        log || tty
    }
}

// ---- the window: tabs -----------------------------------------------------------------------

/// The Terminal window: tabs, each its own tty and msh. Cmd+T opens one, Cmd+W closes one
/// (the window when it is the last), Cmd+1..9 and Ctrl+Tab switch.
pub struct Terminal {
    tabs: Vec<Tab>,
    active: usize,
    title: String,
    /// The window draws the shell tabs in its title strip (the Terminal is alone in its
    /// window); otherwise, with 2+ shells, they are drawn at the top of the body.
    external: bool,
    /// Strip hover states (only for the strip drawn in the body).
    tab_hover: Option<usize>,
    close_hover: Option<usize>,
    /// Body width at the last draw (for hit-testing the strip).
    width: i32,
}

impl Terminal {
    pub fn new() -> Terminal {
        let mut t = Terminal {
            tabs: alloc::vec![Tab::first()],
            active: 0,
            title: String::new(),
            external: false,
            tab_hover: None,
            close_hover: None,
            width: 520,
        };
        t.retitle();
        t
    }

    fn retitle(&mut self) {
        let tab = &self.tabs[self.active];
        self.title = match (tab.tty, self.tabs.len()) {
            (0, _) => format!("{}: sh", user_name()),
            (_, 1) => format!("{}: msh", user_name()),
            (n, _) => format!("{}: {}  (tty{n})", user_name(), tab.label),
        };
    }

    fn top(&self) -> i32 {
        if self.tabs.len() > 1 && !self.external {
            STRIP_H
        } else {
            0
        }
    }

    /// SPEC: Terminal tabs read "1 msh", "2 msh".
    fn tab_titles(&self) -> Vec<String> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, t)| format!("{} {}", i + 1, t.label))
            .collect()
    }

    /// The body strip's tab / close / + under (x, y).
    fn strip_hit(&self, x: i32, y: i32) -> Option<Part> {
        if y >= STRIP_H {
            return None;
        }
        let n = self.tabs.len();
        for i in 0..n {
            let r = tab_rect(i, n, self.width, 8);
            if hit(&tab_close_rect(r), x, y) {
                return Some(Part::TabClose(i));
            }
            if hit(&r, x, y) {
                return Some(Part::Tab(i));
            }
        }
        hit(&plus_rect(n, self.width, 8), x, y).then_some(Part::TabNew)
    }

    /// Open a tab (msh on a free tty, or the built-in shell if there is no msh), running
    /// `cmd` once its shell is up.
    fn new_tab(&mut self, cmd: Option<&str>) {
        if self.tabs.len() >= tty::MAX {
            super::super::notify_as("Terminal", "Terminal", "Nine tabs is all there are");
            return;
        }
        let n = if self.tabs[0].tty != 0 {
            match tty::open_tab() {
                Some(n) => n,
                None => {
                    super::super::notify_as(
                        "Terminal",
                        "Terminal",
                        "No free terminal for a new tab",
                    );
                    return;
                }
            }
        } else {
            0
        };
        let mut t = Tab::extra(n);
        if let Some(c) = cmd {
            t.label = String::from(c);
            if n == 0 {
                t.run_command(c);
            } else {
                t.pending = Some(String::from(c));
            }
        }
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
        self.retitle();
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(i);
        tty::hangup(t.tty);
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.retitle();
    }

    fn select(&mut self, i: usize) -> bool {
        if i >= self.tabs.len() || i == self.active {
            return false;
        }
        self.active = i;
        self.tabs[i].sel = None;
        self.retitle();
        true
    }

    /// The shell tabs at the top of the body (SPEC Tabs, Terminal colours; first tab at x8,
    /// there are no traffic lights here).
    fn draw_strip(&mut self, p: &mut Painter, w: i32) {
        p.fill(0, 0, w, STRIP_H, TERM_BAR);
        let s = Strip {
            titles: self.tab_titles(),
            active: self.active,
            terminal: true,
            inner: true,
        };
        paint_strip(
            p,
            &s,
            w,
            8,
            self.tab_hover,
            self.close_hover,
            TERM_BG,
            TERM_FG,
            true,
        );
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // The window closed: hang up every tab's tty but the first (tty1 keeps its shell).
        for t in &self.tabs {
            tty::hangup(t.tty);
        }
    }
}

impl WindowContents for Terminal {
    fn title(&self) -> &str {
        &self.title
    }

    fn dark(&self) -> bool {
        true
    }

    fn menus(&self) -> &'static [&'static str] {
        &["Shell", "Edit", "View", "Window", "Help"]
    }

    fn inner_tabs(&self) -> Option<(Vec<String>, usize)> {
        (self.tabs.len() > 1).then(|| (self.tab_titles(), self.active))
    }

    fn inner_select(&mut self, i: usize) {
        self.select(i);
    }

    fn inner_close(&mut self, i: usize) {
        self.close_tab(i);
    }

    fn inner_new(&mut self) {
        self.new_tab(None);
    }

    fn set_strip_external(&mut self, on: bool) {
        self.external = on;
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        if self.top() == 0 {
            return false;
        }
        let (t, c) = match self.strip_hit(x, y) {
            Some(Part::Tab(i)) => (Some(i), None),
            Some(Part::TabClose(i)) => (Some(i), Some(i)),
            _ => (None, None),
        };
        let changed = t != self.tab_hover || c != self.close_hover;
        self.tab_hover = t;
        self.close_hover = c;
        changed
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, focused: bool) {
        self.width = w;
        let top = self.top();
        if top > 0 {
            self.draw_strip(p, w);
        }
        let mut sp = p.sub(Rect::new(0, top, w, h - top));
        self.tabs[self.active].draw(&mut sp, w, h - top, focused);
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        if k.mods.meta {
            match k.key {
                Key::Char('t') => {
                    self.new_tab(None);
                    return true;
                }
                Key::Char(c @ '1'..='9') => return self.select(c as usize - '1' as usize),
                Key::Char('{') | Key::Char('[') => {
                    let n = self.tabs.len();
                    return self.select((self.active + n - 1) % n);
                }
                Key::Char('}') | Key::Char(']') => {
                    return self.select((self.active + 1) % self.tabs.len());
                }
                _ => {}
            }
        }
        if k.mods.ctrl && k.key == Key::Tab {
            let n = self.tabs.len();
            let to = if k.mods.shift {
                (self.active + n - 1) % n
            } else {
                (self.active + 1) % n
            };
            return self.select(to);
        }
        self.tabs[self.active].key(k)
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let top = self.top();
        if top > 0 && e.y < top {
            if let PointerKind::Down(_) = e.kind {
                match self.strip_hit(e.x, e.y) {
                    Some(Part::TabNew) => self.new_tab(None),
                    Some(Part::TabClose(i)) => self.close_tab(i),
                    Some(Part::Tab(i)) => return self.select(i),
                    _ => return false,
                }
                return true;
            }
            return false;
        }
        let mut e = *e;
        e.y -= top;
        self.tabs[self.active].pointer(&e)
    }

    fn scroll(&mut self, dy: i32) -> bool {
        self.tabs[self.active].scroll(dy)
    }

    /// A file dropped here: its quoted path typed at the prompt (no Enter), like macOS.
    fn drop_path(&mut self, path: &str) -> bool {
        let q = alloc::format!("{} ", super::super::dnd::quote(path));
        let tab = &mut self.tabs[self.active];
        if tab.tty != 0 {
            for b in q.bytes() {
                tty::input(tab.tty, b);
            }
        } else {
            tab.input.push_str(&q);
            tab.cursor = tab.input.chars().count();
        }
        true
    }

    fn close_part(&mut self) -> bool {
        if self.tabs.len() > 1 {
            self.close_tab(self.active);
            return true;
        }
        false
    }

    fn tick(&mut self, now: u64) -> bool {
        let mut redraw = false;
        let tabs: Vec<Option<String>> = core::mem::take(&mut *NEW_TABS.lock());
        for cmd in tabs {
            self.new_tab(cmd.as_deref());
            redraw = true;
        }
        let cmds: Vec<String> = core::mem::take(&mut *COMMANDS.lock());
        for c in cmds {
            self.tabs[self.active].run_command(&c);
        }
        // Every tab drains its tty (so background tabs keep up); only the shown one redraws.
        for (i, t) in self.tabs.iter_mut().enumerate() {
            if t.tick(now) && i == self.active {
                redraw = true;
            }
        }
        redraw
    }
}

/// The in-kernel stand-in for a real shell. Works on the real file system (`crate::fs`).
pub struct BuiltinShell {
    cwd: String,
    /// The folder before the last `go`, for `go back` (empty at first).
    prev: String,
    prompt: String,
}

impl BuiltinShell {
    pub fn new() -> BuiltinShell {
        let mut s = BuiltinShell {
            cwd: String::from(fsx::HOME),
            prev: String::new(),
            prompt: String::new(),
        };
        s.set_prompt();
        s
    }

    fn set_prompt(&mut self) {
        let here = fsx::pretty(&self.cwd);
        let short = if here == "~" || here == "/" {
            here
        } else {
            String::from(fsx::basename(&self.cwd))
        };
        self.prompt = format!("{}@milk {short} % ", user_name());
    }

    fn path(&self, arg: &str) -> String {
        fsx::resolve(&self.cwd, arg)
    }

    /// `files list [-l] TARGET`.
    fn files_list(&self, target: &str, long: bool, out: &mut Output) {
        let p = self.path(target);
        match fsx::list(&p) {
            Ok(items) if long => {
                for e in items {
                    let kind = match e.kind {
                        InodeType::Dir => "folder",
                        InodeType::Symlink => "link",
                        InodeType::CharDev | InodeType::BlockDev => "device",
                        _ => "file",
                    };
                    out.out(&format!(
                        "{kind:<6} {:>8}  {}",
                        fsx::human_size(e.size),
                        e.name
                    ));
                }
            }
            Ok(items) => {
                let mut line = String::new();
                for e in items {
                    let name = if e.kind == InodeType::Dir {
                        format!("{}/", e.name)
                    } else {
                        e.name
                    };
                    if line.len() + name.len() + 2 > 60 {
                        out.good(&line);
                        line.clear();
                    }
                    line.push_str(&name);
                    line.push_str("  ");
                }
                if !line.is_empty() {
                    out.good(&line);
                }
            }
            Err(e) => out.bad(&format!("files: {target}: {e}")),
        }
    }
}

const COW: &str = r"        \   ^__^
         \  (oo)\_______
            (__)\       )\/\
                ||----w |
                ||     ||";

impl Shell for BuiltinShell {
    fn prompt(&self) -> &str {
        &self.prompt
    }

    fn run(&mut self, line: &str, out: &mut Output) -> ShellResult {
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else {
            return ShellResult::Done;
        };
        let rest: Vec<&str> = parts.collect();
        let arg = rest.join(" ");
        match cmd {
            "help" => {
                out.out("files:    files list [-l] | create NAME | delete [--now] PATH,  trash");
                out.out("          go FOLDER (go: home, go back)  where  cat  echo text > file");
                out.out("          edit <file> (opens it in Notes)");
                out.out(
                    "system:   system [info|uptime|memory]  users me  services logs  date clear",
                );
                out.out("desktop:  open <files|notes|about|terminal>  notify <text>");
                out.out("          today  spaces  cowsay [text]");
                out.out("keys: Ctrl+Space Skim, F3 Spaces, F4 Today, Ctrl+Left/Right spaces,");
                out.out("      Cmd+W close, Cmd+M minimise, Cmd+Tab cycle windows");
            }
            "clear" => return ShellResult::Clear,
            "date" => {
                let t = rtc::now();
                out.out(&format!(
                    "{} {} {} {} {:02}:{:02}:{:02}",
                    &rtc::DAYS[t.weekday as usize][..3],
                    t.day,
                    &rtc::MONTHS[t.month as usize - 1][..3],
                    t.year,
                    t.hour,
                    t.minute,
                    t.second
                ));
            }
            // The built-in versions of a few msh commands (user/systools has the whole thing).
            "system" => match rest.first().copied().unwrap_or("info") {
                "info" => {
                    let s = pmm::stats();
                    out.out("name     milk");
                    out.out(concat!("system   milkOS ", env!("MILKOS_VERSION")));
                    out.out("machine  aarch64 qemu-virt");
                    out.out(&format!("uptime   {}", fmt_uptime(timer::now_ns())));
                    out.out(&format!(
                        "memory   {} MiB, {} MiB free",
                        (s.total_frames * 4096) >> 20,
                        (s.free_frames * 4096) >> 20
                    ));
                }
                "uptime" => out.out(&format!("up {}", fmt_uptime(timer::now_ns()))),
                "memory" => {
                    let s = pmm::stats();
                    let h = heap::stats();
                    out.out(&format!(
                        "ram  {} MiB total, {} MiB free",
                        (s.total_frames * 4096) >> 20,
                        (s.free_frames * 4096) >> 20
                    ));
                    out.out(&format!(
                        "heap {} KiB total, {} KiB used, {} free blocks",
                        h.total >> 10,
                        h.used >> 10,
                        h.free_blocks
                    ));
                }
                other => out.bad(&format!(
                    "system: there's no system {other} here (info, uptime, memory)"
                )),
            },
            "users" => match rest.first().copied().unwrap_or("me") {
                "me" => out.out(user_name()),
                other => out.bad(&format!("users: there's no users {other} here (me)")),
            },
            "services" => match rest.first().copied() {
                // The kernel's log: what the built-in shell can see without milkd.
                Some("logs") => {
                    let mut text = String::new();
                    klog::read_since(0, |b| text.push_str(core::str::from_utf8(b).unwrap_or("?")));
                    for l in text.lines() {
                        out.out(l);
                    }
                }
                _ => out.bad("services: only services logs works here (the kernel's log)"),
            },
            "echo" => match arg.split_once('>') {
                Some((text, file)) => {
                    let (append, file) = match file.strip_prefix('>') {
                        Some(f) => (true, f.trim()),
                        None => (false, file.trim()),
                    };
                    let p = self.path(file);
                    let mut data = if append {
                        fs::read_to_string(&p).unwrap_or_default()
                    } else {
                        String::new()
                    };
                    data.push_str(text.trim());
                    data.push('\n');
                    if let Err(e) = fs::write_file(&p, data.as_bytes()) {
                        out.bad(&format!("echo: {file}: {e}"));
                    }
                }
                None => out.out(&arg),
            },
            "hostname" => out.out("milk"),
            // A little of `files` (user/filetools has the whole thing, in msh).
            "files" => {
                let sub = rest.first().copied().unwrap_or("help");
                let now = rest.contains(&"--now");
                let long = rest.contains(&"-l") || rest.contains(&"--long");
                let args: Vec<&str> = rest
                    .iter()
                    .skip(1)
                    .filter(|a| !a.starts_with('-'))
                    .copied()
                    .collect();
                match sub {
                    "list" => self.files_list(args.first().copied().unwrap_or("."), long, out),
                    "create" => {
                        for a in &args {
                            let p = self.path(a);
                            let r = if a.ends_with('/') {
                                fs::mkdir_all(&p, 0o755)
                            } else if fs::stat(&p).is_err() {
                                fs::write_file(&p, b"")
                            } else {
                                Ok(())
                            };
                            if let Err(e) = r {
                                out.bad(&format!("files: {a}: {e}"));
                            }
                        }
                    }
                    "delete" => {
                        for a in &args {
                            let p = self.path(a);
                            if !fsx::is_dir(&p) && a.ends_with('/') {
                                out.bad(&format!("files: {} is a file, not a folder", a.trim_end_matches('/')));
                                continue;
                            }
                            let r = if now {
                                if fsx::is_dir(&p) { fs::rmdir(&p) } else { fs::unlink(&p) }
                                    .map_err(|e| format!("{a}: {e}"))
                            } else {
                                super::super::trash::move_to_trash(&p).map(|_| ())
                            };
                            if let Err(m) = r {
                                out.bad(&format!("files: {m}"));
                            }
                        }
                    }
                    _ => out.out("files list [-l] [PATH] | create NAME (NAME/ a folder) | delete [--now] PATH"),
                }
            }
            "trash" => {
                let items = super::super::trash::items();
                if items.is_empty() {
                    out.out("The Trash is empty.");
                }
                for it in items {
                    out.out(&format!("{}  (from {})", it.name, fsx::pretty(it.from())));
                }
            }
            "go" => {
                let args: &[&str] = if arg.is_empty() { &[] } else { &[arg.as_str()] };
                match filekit::path::go_target(args, "~", &self.prev) {
                    Ok(dir) => {
                        let p = self.path(&dir);
                        if fsx::is_dir(&p) {
                            self.prev = core::mem::replace(&mut self.cwd, p);
                            self.set_prompt();
                        } else {
                            let is_file = fs::stat(&p).is_ok();
                            let errno = if is_file { 20 } else { 2 };
                            out.bad(&format!(
                                "go: {}",
                                filekit::words::say_go(&dir, errno, is_file)
                            ));
                        }
                    }
                    Err(m) => out.bad(&format!("go: {m}")),
                }
            }
            "where" => out.out(&self.cwd.clone()),
            "cat" => {
                for a in &rest {
                    match fs::read_to_string(&self.path(a)) {
                        Ok(t) => {
                            for l in t.lines() {
                                out.out(l);
                            }
                        }
                        Err(e) => out.bad(&format!("cat: {a}: {e}")),
                    }
                }
            }
            "edit" => {
                if arg.is_empty() {
                    request(Request::OpenHere(App::Notes));
                } else {
                    let p = self.path(&arg);
                    if fs::stat(&p).is_err() {
                        let _ = fs::write_file(&p, b"");
                    }
                    super::notes::open_file(&p);
                    request(Request::OpenHere(App::Notes));
                    out.good(&format!("editing {}", fsx::pretty(&p)));
                }
            }
            "open" => {
                let app = match arg.as_str() {
                    "files" | "Files" => Some(App::Files),
                    "notes" | "Notes" => Some(App::Notes),
                    "about" | "About" => Some(App::About),
                    "terminal" | "term" => Some(App::Terminal),
                    _ => None,
                };
                match app {
                    Some(a) => {
                        request(Request::Open(a));
                        out.good(&format!("opening {}", a.name()));
                    }
                    None => out.bad("open what? files, notes, about or terminal"),
                }
            }
            "today" => request(Request::Mode(Mode::Home)),
            "spaces" => request(Request::Mode(Mode::Spaces)),
            "notify" => {
                let t = if arg.is_empty() {
                    "Hello"
                } else {
                    arg.as_str()
                };
                super::super::notify(t, "from the terminal");
                out.good("sent");
            }
            "cowsay" => {
                let t = if arg.is_empty() { "moo" } else { arg.as_str() };
                let n = t.chars().count();
                out.line(&format!(" {}", "_".repeat(n + 2)), TERM_FG);
                out.line(&format!("< {t} >"), TERM_FG);
                out.line(&format!(" {}", "-".repeat(n + 2)), TERM_FG);
                out.line(COW, TERM_FG);
            }
            "exit" => out.out("there is nowhere to go. this is the kernel."),
            _ => out.bad(&format!("{cmd}: command not found (try 'help')")),
        }
        ShellResult::Done
    }
}
