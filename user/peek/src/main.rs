//! Peek: a tiny sandboxed text viewer, the powerbox demo (docs/security.md).
//!
//! It drops all file system access at start (`sandbox("fs-none net=none ...")`), proves it
//! by trying to open ~/Documents/todo.txt directly (EACCES, audited as sandbox.deny), and
//! then gets files only through the desktop's trusted panel: `o` / the Open button asks for
//! one to read (`powerbox_open`), `s` / Save a copy writes what it shows to a file the user
//! picks (`powerbox_save`). Each choice is one file, granted by the user, logged as
//! powerbox.grant.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use gui::{Event, Key, Window};
use rt::{println, sys};

rt::main!(main);

const W: i32 = 520;
const H: i32 = 340;
const BAR: i32 = 48;
const OPEN: (i32, i32, i32) = (12, 10, 128);
const SAVE: (i32, i32, i32) = (148, 10, 128);

struct App {
    win: Window,
    status: String,
    lines: Vec<String>,
    text: Vec<u8>,
}

fn read_fd(fd: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    while out.len() < 64 * 1024 {
        match sys::read(fd, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
    out
}

impl App {
    fn draw(&mut self) {
        let w = &mut self.win;
        w.fill(0, 0, W, H, gui::MILK);
        w.fill(0, 0, W, BAR, gui::CREAM);
        w.fill(0, BAR - 1, W, 1, gui::SAND);
        for (label, (x, y, bw)) in [("Open a file...", OPEN), ("Save a copy...", SAVE)] {
            w.rrect(x, y, bw, 28, 8, gui::OAT);
            let tx = x + (bw - gui::str_width(label, 1)) / 2;
            w.text(tx, y + 6, label, gui::ESPRESSO);
        }
        w.text(296, 16, "sandboxed: no fs, no net", gui::COCOA);
        let mut y = BAR + 14;
        for l in self.lines.iter().take(((H - BAR - 54) / 18) as usize) {
            let s: String = l.chars().take(((W - 32) / 8) as usize).collect();
            // The file's text is monospace content.
            w.mono(16, y, &s, gui::ESPRESSO);
            y += 18;
        }
        w.fill(0, H - 34, W, 1, gui::SAND);
        let st: String = self.status.chars().take(((W - 32) / 8) as usize).collect();
        w.text(16, H - 25, &st, gui::COCOA);
        w.commit();
    }

    fn show(&mut self, fd: i32, how: &str) {
        self.text = read_fd(fd);
        let _ = sys::close(fd);
        let s = String::from_utf8_lossy(&self.text).into_owned();
        self.lines = s.lines().map(String::from).collect();
        self.status = format!("{how}: got a file, {} bytes", self.text.len());
    }

    fn open(&mut self) {
        match sys::powerbox_open() {
            Ok(fd) => self.show(fd, "powerbox"),
            Err(e) => self.status = format!("powerbox open: {}", e.name()),
        }
        println!("peek: {}", self.status);
        self.draw();
    }

    fn save(&mut self) {
        match sys::powerbox_save("peek copy.txt") {
            Ok(fd) => {
                let r = sys::write_all(fd, &self.text);
                let _ = sys::close(fd);
                self.status = match r {
                    Ok(()) => format!("powerbox: saved {} bytes", self.text.len()),
                    Err(e) => format!("save failed: {}", e.name()),
                };
            }
            Err(e) => self.status = format!("powerbox save: {}", e.name()),
        }
        println!("peek: {}", self.status);
        self.draw();
    }
}

fn main(_args: &rt::Args) -> i32 {
    // The system font's files, read while we still may (the sandbox has no file system).
    gui::text::preload();
    // Before anything else: no file system, no network, no spawning, no clipboard.
    if let Err(e) = sys::sandbox("fs-none net=none spawn=0 clipboard=0") {
        println!("peek: can't sandbox myself: {}", e.name());
        return 1;
    }
    let win = match Window::new("Peek", W, H) {
        Ok(w) => w,
        Err(e) => {
            println!("peek: no window: {}", e.name());
            return 1;
        }
    };
    // Show that the sandbox holds: a plain open of a real file is refused.
    let probe = "/Users/milk/Documents/todo.txt";
    let direct = match sys::open(probe, sys::O_RDONLY) {
        Ok(fd) => {
            let _ = sys::close(fd);
            String::from("direct open of ~/Documents/todo.txt WORKED (not sandboxed?)")
        }
        Err(e) => format!("direct open of ~/Documents/todo.txt: {} (good)", e.name()),
    };
    println!("peek: {direct}");
    let mut app = App {
        win,
        status: direct,
        lines: alloc::vec![
            String::from("Peek can't see your files."),
            String::from(""),
            String::from("Press o (or Open a file...) and pick one in the"),
            String::from("desktop's panel, or drop a file here from Files:"),
            String::from("only that file is shared with Peek."),
        ],
        text: Vec::new(),
    };
    app.draw();
    loop {
        let Some(ev) = app.win.event(1000) else {
            continue;
        };
        let inside = |x: i32, y: i32, b: (i32, i32, i32)| {
            x >= b.0 && x < b.0 + b.2 && y >= b.1 && y < b.1 + 28
        };
        match ev {
            Event::Close => break,
            Event::Down { x, y, .. } if inside(x, y, OPEN) => app.open(),
            Event::Down { x, y, .. } if inside(x, y, SAVE) => app.save(),
            Event::Key(Key::Char('o'), _) => app.open(),
            Event::Key(Key::Char('s'), _) => app.save(),
            // Settings changed the font: redraw in it.
            Event::Appearance { .. } => app.draw(),
            Event::Drop { fd, .. } => {
                app.show(fd, "dropped");
                println!("peek: {}", app.status);
                app.draw();
            }
            _ => {}
        }
    }
    0
}
