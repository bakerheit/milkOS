//! Terminal: espresso window with a scrollback that follows the kernel log live, and a line
//! editor feeding a [`Shell`].
//!
//! The shell is a hook: today [`BuiltinShell`] answers a handful of commands in-kernel. Once
//! `proc` and `vfs` land, implement [`Shell`] for a proxy that writes the line to a userland
//! `/bin/sh` process's stdin and appends whatever it prints (and `run` can return before the
//! command finishes: push output later from `tick`).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{self, InodeType};
use crate::gfx::Color;
use crate::{heap, klog, pmm, timer};

use super::super::event::Key;
use super::super::paint::Painter;
use super::super::theme::*;
use super::super::window::{App, KeyInput, WindowContents};
use super::super::{Mode, Request, request, rtc, shell::fmt_uptime};
use super::fsx;

const MAX_LINES: usize = 2000;
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
        self.line(s, LATTE);
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

pub struct Terminal {
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
    /// Attached to `msh` on /dev/tty1: keys go to the tty, its output is drawn here.
    tty: bool,
    tty_gen: u64,
    /// The tty's current (unterminated) output line: the prompt and what is being typed.
    cur: String,
    /// Inside an escape sequence in the tty output (swallowed; msh runs with TERM=dumb).
    esc: u8,
}

impl Terminal {
    pub fn new() -> Terminal {
        let mut t = Terminal {
            lines: Vec::new(),
            partial: String::new(),
            log_pos: 0,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            hist_pos: 0,
            back: 0,
            shell: Box::new(BuiltinShell::new()),
            tty: false,
            tty_gen: 0,
            cur: String::new(),
            esc: 0,
        };
        t.follow_log();
        t.lines.push((String::new(), LATTE));
        t.tty = super::super::tty::start_shell();
        let banner = if t.tty {
            "milkOS 1.0 terminal: msh on /dev/tty1. Kernel log above, live."
        } else {
            "milkOS 1.0 terminal. Kernel log above, live. Type 'help'."
        };
        t.lines.push((String::from(banner), MATCHA));
        t
    }

    /// Draw what msh wrote: lines into the scrollback, the unfinished one stays in `cur`.
    fn follow_tty(&mut self) -> bool {
        let Some(bytes) = super::super::tty::take_output(&mut self.tty_gen) else {
            return false;
        };
        for c in String::from_utf8_lossy(&bytes).chars() {
            match (self.esc, c) {
                (1, '[') => self.esc = 2,
                (1, _) => self.esc = 0,
                (2, '\x40'..='\x7e') => self.esc = 0,
                (2, _) => {}
                (_, '\x1b') => self.esc = 1,
                (_, '\n') => {
                    let l = core::mem::take(&mut self.cur);
                    self.lines.push((l, CREAM));
                }
                (_, '\x08') => {
                    self.cur.pop();
                }
                (_, '\r') => {}
                (_, '\t') => self.cur.push_str("    "),
                (_, c) if !c.is_control() => self.cur.push(c),
                _ => {}
            }
        }
        self.trim();
        self.back = 0;
        true
    }

    /// A key typed into the window, as tty bytes.
    fn key_to_tty(&mut self, k: &KeyInput) -> bool {
        use super::super::tty;
        if k.mods.ctrl {
            match k.key {
                Key::Char('l') => {
                    self.lines.clear();
                    return true;
                }
                Key::Char(c) if c.is_ascii_alphabetic() => {
                    tty::input(c.to_ascii_lowercase() as u8 - b'a' + 1);
                }
                _ => return false,
            }
            return false;
        }
        match k.key {
            Key::Enter => tty::input(b'\r'),
            Key::Backspace => tty::input(0x7f),
            Key::Tab => tty::input(b'\t'),
            Key::PageUp => {
                self.back += 8;
                return true;
            }
            _ => {
                if let Some(c) = k.ch {
                    let mut b = [0u8; 4];
                    for &x in c.encode_utf8(&mut b).as_bytes() {
                        tty::input(x);
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
            self.lines.push((line, LATTE));
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
            .push((format!("{}{}", self.shell.prompt(), line), CREAM));
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

/// Split `s` into chunks of at most `cols` characters.
fn wrap(s: &str, cols: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return alloc::vec![String::new()];
    }
    chars.chunks(cols).map(|c| c.iter().collect()).collect()
}

impl WindowContents for Terminal {
    fn title(&self) -> &str {
        if self.tty {
            "andrew: msh"
        } else {
            "andrew: sh"
        }
    }

    fn dark(&self) -> bool {
        true
    }

    fn menus(&self) -> &'static [&'static str] {
        &["Shell", "Edit", "View", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, focused: bool) {
        let cols = ((w - 2 * PAD) / 8).max(4) as usize;
        let rows = ((h - 2 * PAD + 2) / LINE_H).max(1) as usize;
        if self.tty {
            // Tell programs on /dev/tty1 (TIOCGWINSZ); SIGWINCH only on a real change.
            super::super::tty::set_winsize(rows as u16, cols as u16);
        }
        // The prompt line (with the cursor) is the last visual row.
        let prompt = if self.tty {
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
            vis.push((seg.clone(), CREAM));
        }
        let max_back = vis.len().saturating_sub(rows);
        self.back = self.back.min(max_back);
        let end = vis.len() - self.back;
        let start = end.saturating_sub(rows);
        for (k, (s, c)) in vis[start..end].iter().enumerate() {
            p.text(PAD, PAD + k as i32 * LINE_H, s, *c);
        }
        // Cursor block.
        if self.back == 0 {
            let pos = if self.tty {
                self.cur.chars().count()
            } else {
                self.shell.prompt().chars().count() + self.cursor
            };
            let (r, ccol) = (pos / cols, pos % cols);
            let row_on_screen = (vis.len() - prompt_rows.len() + r) as i32 - start as i32;
            let (x, y) = (PAD + ccol as i32 * 8, PAD + row_on_screen * LINE_H);
            if focused {
                p.fill(x, y, 8, 16, CREAM);
                let ch = self.input.chars().nth(self.cursor).unwrap_or(' ');
                let mut b = [0u8; 4];
                p.text(x, y, ch.encode_utf8(&mut b), ESPRESSO);
            } else {
                p.rrect_stroke(x, y, 8, 16, 0, 1, LATTE, 255);
            }
        }
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        self.back = 0;
        if self.tty {
            return self.key_to_tty(k);
        }
        if k.mods.ctrl {
            match k.key {
                Key::Char('c') => {
                    let line = format!("{}{}^C", self.shell.prompt(), self.input);
                    self.lines.push((line, CREAM));
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

    fn scroll(&mut self, dy: i32) -> bool {
        let old = self.back;
        self.back = (self.back as i32 - dy * 3).max(0) as usize;
        old != self.back
    }

    fn tick(&mut self, _now: u64) -> bool {
        let log = self.follow_log();
        let tty = self.tty && self.follow_tty();
        log || tty
    }
}

/// The in-kernel stand-in for a real shell. Works on the real file system (`crate::fs`).
pub struct BuiltinShell {
    cwd: String,
    prompt: String,
}

impl BuiltinShell {
    pub fn new() -> BuiltinShell {
        let mut s = BuiltinShell {
            cwd: String::from(fsx::HOME),
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
        self.prompt = format!("andrew@milk {short} % ");
    }

    fn path(&self, arg: &str) -> String {
        fsx::resolve(&self.cwd, arg)
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
                out.out("files:    ls [-l] cd pwd cat mkdir touch rm  echo text > file");
                out.out("          edit <file> (opens it in Notes)");
                out.out("system:   uname uptime date mem log whoami clear");
                out.out("desktop:  open <files|notes|about|terminal>  notify <text>");
                out.out("          today  spaces  cowsay [text]");
                out.out("keys: Ctrl+Space Skim, F3 Spaces, F4 Today, Ctrl+Left/Right spaces,");
                out.out("      Cmd+W close, Cmd+M minimise, Cmd+Tab cycle windows");
            }
            "clear" => return ShellResult::Clear,
            "uname" => {
                if arg.contains('a') {
                    out.out("milkOS 1.0 milk 0.0.1 aarch64 qemu-virt");
                } else {
                    out.out("milkOS");
                }
            }
            "uptime" => out.out(&format!("up {}", fmt_uptime(timer::now_ns()))),
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
            "mem" | "free" => {
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
            "log" | "dmesg" => {
                let mut text = String::new();
                klog::read_since(0, |b| text.push_str(core::str::from_utf8(b).unwrap_or("?")));
                for l in text.lines() {
                    out.out(l);
                }
            }
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
            "whoami" => out.out("andrew"),
            "hostname" => out.out("milk"),
            "ls" => {
                let long = rest.first() == Some(&"-l");
                let target = rest
                    .iter()
                    .find(|a| !a.starts_with('-'))
                    .copied()
                    .unwrap_or(".");
                let p = self.path(target);
                match fsx::list(&p) {
                    Ok(items) if long => {
                        for e in items {
                            let kind = match e.kind {
                                InodeType::Dir => 'd',
                                InodeType::Symlink => 'l',
                                InodeType::CharDev => 'c',
                                InodeType::BlockDev => 'b',
                                _ => '-',
                            };
                            out.out(&format!(
                                "{kind} {:>8}  {}",
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
                    Err(e) => out.bad(&format!("ls: {target}: {e}")),
                }
            }
            "cd" => {
                let p = self.path(if arg.is_empty() { "~" } else { arg.as_str() });
                if fsx::is_dir(&p) {
                    self.cwd = p;
                    self.set_prompt();
                } else {
                    out.bad(&format!("cd: {arg}: not a directory"));
                }
            }
            "pwd" => out.out(&self.cwd.clone()),
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
            "mkdir" => {
                for a in &rest {
                    if let Err(e) = fs::mkdir_all(&self.path(a), 0o755) {
                        out.bad(&format!("mkdir: {a}: {e}"));
                    }
                }
            }
            "touch" => {
                for a in &rest {
                    let p = self.path(a);
                    if fs::stat(&p).is_err()
                        && let Err(e) = fs::write_file(&p, b"")
                    {
                        out.bad(&format!("touch: {a}: {e}"));
                    }
                }
            }
            "rm" => {
                for a in &rest {
                    let p = self.path(a);
                    let r = if fsx::is_dir(&p) {
                        fs::rmdir(&p)
                    } else {
                        fs::unlink(&p)
                    };
                    if let Err(e) = r {
                        out.bad(&format!("rm: {a}: {e}"));
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
                out.line(&format!(" {}", "_".repeat(n + 2)), CREAM);
                out.line(&format!("< {t} >"), CREAM);
                out.line(&format!(" {}", "-".repeat(n + 2)), CREAM);
                out.line(COW, CREAM);
            }
            "exit" => out.out("there is nowhere to go. this is the kernel."),
            _ => out.bad(&format!("{cmd}: command not found (try 'help')")),
        }
        ShellResult::Done
    }
}
