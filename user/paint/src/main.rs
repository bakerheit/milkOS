//! Paint: the first userland desktop app. A canvas, a milk palette, three brush sizes,
//! Clear and Save (writes ~/Pictures/painting.ppm). Keys: 1-8 colours, [ ] brush size,
//! c clear, s save. Proof that a process can own a window (gui crate, MILK_WIN_* syscalls).
//! Resizable: a bigger window is a bigger canvas (what was drawn stays put).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use gui::{Event, Key, Window};
use rt::{println, sys};

rt::main!(main);

const W: i32 = 560;
const H: i32 = 380;
/// The toolbar needs this much width; a little canvas below it.
const MIN_W: i32 = 560;
const MIN_H: i32 = 200;
const BAR: i32 = 48;
const PALETTE: [u32; 8] = [
    gui::ESPRESSO,
    gui::STRAW,
    gui::HONEY,
    gui::MATCHA,
    gui::BLUE,
    gui::LAV,
    gui::COCOA,
    gui::MILK,
];
const SIZES: [i32; 3] = [2, 5, 10];

struct App {
    win: Window,
    color: usize,
    size: usize,
    last: Option<(i32, i32)>,
    saved: bool,
}

fn swatch(i: usize) -> (i32, i32) {
    (12 + i as i32 * 34, 10)
}

fn size_button(i: usize) -> (i32, i32) {
    (300 + i as i32 * 34, 10)
}

const CLEAR: (i32, i32, i32) = (418, 10, 60);
const SAVE: (i32, i32, i32) = (486, 10, 60);

impl App {
    fn toolbar(&mut self) {
        let w = &mut self.win;
        let ww = w.w;
        w.fill(0, 0, ww, BAR, gui::CREAM);
        w.fill(0, BAR - 1, ww, 1, gui::SAND);
        for (i, &c) in PALETTE.iter().enumerate() {
            let (x, y) = swatch(i);
            if i == self.color {
                w.rrect(x - 3, y - 3, 34, 34, 10, gui::HONEY);
            }
            w.rrect(x, y, 28, 28, 8, gui::SAND);
            w.rrect(x + 1, y + 1, 26, 26, 7, c);
        }
        for (i, &s) in SIZES.iter().enumerate() {
            let (x, y) = size_button(i);
            let bg = if i == self.size { gui::OAT } else { gui::CREAM };
            w.rrect(x, y, 28, 28, 8, bg);
            w.disc(x + 14, y + 14, s.min(10), gui::ESPRESSO);
        }
        for (label, (x, y, bw)) in [
            ("Clear", CLEAR),
            (if self.saved { "Saved" } else { "Save" }, SAVE),
        ] {
            w.rrect(x, y, bw, 28, 8, gui::OAT);
            let tx = x + (bw - gui::str_width(label, 1)) / 2;
            w.text(tx, y + 6, label, gui::ESPRESSO);
        }
        w.commit_rect(0, 0, ww, BAR);
    }

    fn clear(&mut self) {
        let (w, h) = (self.win.w, self.win.h);
        self.win.fill(0, BAR, w, h - BAR, gui::MILK);
        let hint = "Draw with the mouse.  1-8 colours, [ ] size, c clear, s save";
        self.win.text(16, h - 24, hint, gui::COCOA);
        self.win.commit();
    }

    fn dab(&mut self, x: i32, y: i32) {
        let r = SIZES[self.size];
        let c = PALETTE[self.color];
        let (x0, y0) = self.last.unwrap_or((x, y));
        let y = y.max(BAR + r);
        let y0 = y0.max(BAR + r);
        self.win.stroke(x0, y0, x, y, r, c);
        let (minx, miny) = (x0.min(x) - r - 2, y0.min(y) - r - 2);
        let (maxx, maxy) = (x0.max(x) + r + 2, y0.max(y) + r + 2);
        self.win.commit_rect(minx, miny, maxx - minx, maxy - miny);
        self.last = Some((x, y));
        if self.saved {
            self.saved = false;
            self.toolbar();
        }
    }

    fn save(&mut self) {
        let (w, h) = (self.win.w as usize, (self.win.h - BAR) as usize);
        let mut data: Vec<u8> = format!("P6\n{w} {h}\n255\n").into_bytes();
        let px = self.win.pixels();
        for y in 0..h {
            for x in 0..w {
                let p = px[(y + BAR as usize) * w + x];
                data.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            }
        }
        let path = "/Users/milk/Pictures/painting.ppm";
        const O_WRONLY: u32 = 1;
        const O_CREAT: u32 = 0o100;
        const O_TRUNC: u32 = 0o1000;
        match sys::open(path, O_WRONLY | O_CREAT | O_TRUNC) {
            Ok(fd) => {
                let _ = sys::write_all(fd, &data);
                let _ = sys::close(fd);
                println!("paint: saved {path}");
                self.saved = true;
                self.win.set_title("Paint - painting.ppm");
            }
            Err(e) => println!("paint: cannot save {path}: {e:?}"),
        }
        self.toolbar();
    }

    fn click(&mut self, x: i32, y: i32) -> bool {
        if y >= BAR {
            return false;
        }
        let inside = |bx: i32, by: i32, bw: i32| x >= bx && x < bx + bw && y >= by && y < by + 28;
        for i in 0..PALETTE.len() {
            let (bx, by) = swatch(i);
            if inside(bx, by, 28) {
                self.color = i;
            }
        }
        for i in 0..SIZES.len() {
            let (bx, by) = size_button(i);
            if inside(bx, by, 28) {
                self.size = i;
            }
        }
        if inside(CLEAR.0, CLEAR.1, CLEAR.2) {
            self.clear();
        }
        if inside(SAVE.0, SAVE.1, SAVE.2) {
            self.save();
            return true;
        }
        self.toolbar();
        true
    }
}

fn main(_args: &rt::Args) -> i32 {
    let win = match Window::new("Paint", W, H) {
        Ok(w) => w,
        Err(e) => {
            println!("paint: no window ({e:?}); is the desktop running?");
            return 1;
        }
    };
    let mut app = App {
        win,
        color: 0,
        size: 1,
        last: None,
        saved: false,
    };
    app.win.set_resizable(MIN_W, MIN_H);
    app.toolbar();
    app.clear();
    loop {
        let Some(ev) = app.win.event(5000) else {
            continue;
        };
        match ev {
            Event::Close => break,
            Event::Down { x, y, .. } => {
                if !app.click(x, y) {
                    app.last = None;
                    app.dab(x, y);
                }
            }
            Event::Drag { x, y } => {
                if app.last.is_some() {
                    app.dab(x, y);
                }
            }
            Event::Up { .. } => app.last = None,
            // Settings changed the font: the toolbar's labels are redrawn in it.
            Event::Appearance { .. } => {
                app.toolbar();
                app.win.commit();
            }
            Event::Resize { w, h } => {
                // The kernel keeps the picture in the top-left; the new edges are milk.
                if app.win.resize(w.max(MIN_W), h.max(MIN_H)).is_ok() {
                    app.toolbar();
                    app.win.commit();
                }
            }
            Event::Key(Key::Char(c), _) => match c {
                '1'..='8' => {
                    app.color = c as usize - '1' as usize;
                    app.toolbar();
                }
                '[' => {
                    app.size = app.size.saturating_sub(1);
                    app.toolbar();
                }
                ']' => {
                    app.size = (app.size + 1).min(SIZES.len() - 1);
                    app.toolbar();
                }
                'c' => app.clear(),
                's' => app.save(),
                _ => {}
            },
            _ => {}
        }
    }
    0
}
