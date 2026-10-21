//! Tiny client library for milkOS desktop windows (the `MILK_WIN_*` syscalls, see
//! docs/processes.md). A [`Window`] is a pixel buffer shared with the window system: draw into
//! [`Window::pixels`] (or with the helpers here), call [`Window::commit`], and read input with
//! [`Window::event`].
//!
//! ```ignore
//! let mut w = gui::Window::new("Hello", 320, 200)?;
//! w.fill(0, 0, 320, 200, gui::MILK);
//! w.text(16, 16, "hello, desktop", gui::ESPRESSO);
//! w.commit();
//! while let Some(ev) = w.event(1000) { if ev == gui::Event::Close { break } }
//! ```

#![no_std]

extern crate alloc;

use rt::sys;

mod font {
    #![allow(dead_code)]
    include!("../../../kernel/src/font_data.rs");
}

// Syscall numbers (kernel/src/proc/syscall.rs).
const WIN_CREATE: usize = 4100;
const WIN_COMMIT: usize = 4101;
const WIN_EVENT: usize = 4102;
const WIN_TITLE: usize = 4103;
const WIN_DESTROY: usize = 4104;

/// A pixel: 0x00RRGGBB.
pub type Color = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

// The milkOS 1.0 palette (docs/ui-concepts/SPEC.md).
pub const MILK: Color = rgb(0xFF, 0xFD, 0xF8);
pub const CREAM: Color = rgb(0xFB, 0xF6, 0xEC);
pub const OAT: Color = rgb(0xEF, 0xE6, 0xD4);
pub const SAND: Color = rgb(0xE2, 0xD5, 0xBD);
pub const LATTE: Color = rgb(0xC4, 0xAE, 0x8F);
pub const COCOA: Color = rgb(0x6B, 0x58, 0x46);
pub const ESPRESSO: Color = rgb(0x3A, 0x2E, 0x28);
pub const STRAW: Color = rgb(0xE5, 0x8C, 0x97);
pub const HONEY: Color = rgb(0xEB, 0xB6, 0x50);
pub const MATCHA: Color = rgb(0x9C, 0xBF, 0x9B);
pub const BLUE: Color = rgb(0x7F, 0x93, 0xC9);
pub const LAV: Color = rgb(0xB9, 0xA9, 0xD6);
/// Fixed in both themes: text and glyphs on straw / honey / blue fills.
pub const INK: Color = rgb(0x3A, 0x2E, 0x27);

// Buttons (SPEC "Buttons"): straw with ink text, never cream on straw (2.42:1).
pub const STRAW_HOVER: Color = rgb(0xEA, 0x9A, 0xA4);
pub const STRAW_PRESSED: Color = rgb(0xDE, 0x7D, 0x89);
pub const DANGER: Color = rgb(0xB5, 0x47, 0x3B);
/// Text on danger, #FFFDF8 in both themes.
pub const DANGER_FG: Color = rgb(0xFF, 0xFD, 0xF8);

/// What kind of button (SPEC "Buttons").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// Straw, ink bold text.
    Primary,
    /// Oat with a 1px sand border, primary text.
    Secondary,
    /// #B5473B with near-white text.
    Danger,
    /// Oat with a 1px sand border, latte text (never the only signal).
    Disabled,
}

/// The theme's neutrals a button needs (the app knows its theme; gui does not).
#[derive(Clone, Copy, Debug)]
pub struct ButtonTheme {
    pub oat: Color,
    pub sand: Color,
    pub text: Color,
    pub latte: Color,
}

/// Special keys (printable keys arrive as their character).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Delete,
    Other(u32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Key(Key, Mods),
    /// Pointer moved with a button held (body coordinates).
    Drag {
        x: i32,
        y: i32,
    },
    /// Button 1 = left, 2 = right, 3 = middle.
    Down {
        x: i32,
        y: i32,
        button: u8,
    },
    Up {
        x: i32,
        y: i32,
        button: u8,
    },
    /// Wheel: positive moves towards the end of the content.
    Scroll(i32),
    /// The user closed the window: clean up and exit.
    Close,
    /// A file was dropped on the window at (x, y): `fd` is it, opened read-only for us
    /// (the user's drop is the grant; read it, then close it).
    Drop {
        x: i32,
        y: i32,
        fd: i32,
    },
}

fn word(b: &[u8], k: usize) -> u32 {
    u32::from_le_bytes([b[4 * k], b[4 * k + 1], b[4 * k + 2], b[4 * k + 3]])
}

fn decode(b: &[u8; 32]) -> Option<Event> {
    let (x, y) = (word(b, 1) as i32, word(b, 2) as i32);
    let extra = word(b, 5) as i32;
    Some(match word(b, 0) {
        1 => {
            let m = word(b, 4);
            let mods = Mods {
                shift: m & 1 != 0,
                ctrl: m & 2 != 0,
                alt: m & 4 != 0,
                meta: m & 8 != 0,
            };
            let key = match word(b, 3) {
                0x1_0001 => Key::Enter,
                0x1_0002 => Key::Backspace,
                0x1_0003 => Key::Tab,
                0x1_0004 => Key::Escape,
                0x1_0005 => Key::Up,
                0x1_0006 => Key::Down,
                0x1_0007 => Key::Left,
                0x1_0008 => Key::Right,
                0x1_0009 => Key::Delete,
                c => char::from_u32(c).map_or(Key::Other(c), Key::Char),
            };
            Event::Key(key, mods)
        }
        3 => Event::Drag { x, y },
        4 => Event::Down {
            x,
            y,
            button: extra as u8,
        },
        5 => Event::Up {
            x,
            y,
            button: extra as u8,
        },
        6 => Event::Scroll(extra),
        7 => Event::Close,
        8 => Event::Drop {
            x,
            y,
            fd: word(b, 3) as i32,
        },
        _ => return None,
    })
}

pub struct Window {
    id: u32,
    px: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Window {
    /// A window whose body is `w` x `h` (at most 1000 x 700).
    pub fn new(title: &str, w: i32, h: i32) -> sys::Result<Window> {
        let mut info = [0u8; 24];
        // SAFETY: valid pointers and lengths for MILK_WIN_CREATE.
        let r = unsafe {
            sys::syscall(
                WIN_CREATE,
                [
                    w as usize,
                    h as usize,
                    title.as_ptr() as usize,
                    title.len(),
                    info.as_mut_ptr() as usize,
                    0,
                ],
            )
        };
        if r < 0 {
            return Err(sys::Errno(-r as i32));
        }
        let addr = u64::from_le_bytes(info[..8].try_into().unwrap());
        Ok(Window {
            id: r as u32,
            px: addr as *mut u32,
            w,
            h,
        })
    }

    /// The body's pixels, row-major, `w` per row.
    pub fn pixels(&mut self) -> &mut [u32] {
        // SAFETY: the kernel mapped w*h pixels at px for as long as the window exists.
        unsafe { core::slice::from_raw_parts_mut(self.px, (self.w * self.h) as usize) }
    }

    /// Show everything drawn so far.
    pub fn commit(&self) {
        self.commit_rect(0, 0, 0, 0);
    }

    /// Show the changes inside this rect.
    pub fn commit_rect(&self, x: i32, y: i32, w: i32, h: i32) {
        // SAFETY: plain integers.
        unsafe {
            sys::syscall(
                WIN_COMMIT,
                [
                    self.id as usize,
                    x as usize,
                    y as usize,
                    w as usize,
                    h as usize,
                    0,
                ],
            )
        };
    }

    /// The next event, waiting up to `timeout_ms` (0 = just check). None on timeout.
    pub fn event(&self, timeout_ms: u64) -> Option<Event> {
        let mut b = [0u8; 32];
        // SAFETY: b is 32 writable bytes.
        let r = unsafe {
            sys::syscall(
                WIN_EVENT,
                [
                    self.id as usize,
                    b.as_mut_ptr() as usize,
                    timeout_ms as usize,
                    0,
                    0,
                    0,
                ],
            )
        };
        if r == 1 { decode(&b) } else { None }
    }

    pub fn set_title(&self, t: &str) {
        // SAFETY: valid pointer and length.
        unsafe {
            sys::syscall(
                WIN_TITLE,
                [self.id as usize, t.as_ptr() as usize, t.len(), 0, 0, 0],
            )
        };
    }

    // ---- drawing --------------------------------------------------------------------------

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let (x0, y0) = (x.max(0), y.max(0));
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        let stride = self.w as usize;
        let px = self.pixels();
        for yy in y0..y1 {
            for xx in x0..x1 {
                px[yy as usize * stride + xx as usize] = c;
            }
        }
    }

    /// Filled circle of radius `r` centred on (cx, cy), with soft edges.
    pub fn disc(&mut self, cx: i32, cy: i32, r: i32, c: Color) {
        let stride = self.w as usize;
        let (w, h) = (self.w, self.h);
        let px = self.pixels();
        for y in (cy - r - 1).max(0)..(cy + r + 2).min(h) {
            for x in (cx - r - 1).max(0)..(cx + r + 2).min(w) {
                // 4x4 samples per pixel.
                let mut n = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let dx = (x - cx) * 4 + sx - 2;
                        let dy = (y - cy) * 4 + sy - 2;
                        if dx * dx + dy * dy <= 16 * r * r {
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    let p = &mut px[y as usize * stride + x as usize];
                    *p = blend(*p, c, (n * 255 / 16) as u8);
                }
            }
        }
    }

    /// Rounded rect (radius `r`), anti-aliased corners.
    pub fn rrect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Color) {
        let r = r.min(w / 2).min(h / 2);
        self.fill(x + r, y, w - 2 * r, h, c);
        self.fill(x, y + r, r, h - 2 * r, c);
        self.fill(x + w - r, y + r, r, h - 2 * r, c);
        for (cx, cy) in [
            (x + r, y + r),
            (x + w - r - 1, y + r),
            (x + r, y + h - r - 1),
            (x + w - r - 1, y + h - r - 1),
        ] {
            self.quarter(cx, cy, r, c, x, y, w, h);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quarter(&mut self, cx: i32, cy: i32, r: i32, c: Color, bx: i32, by: i32, bw: i32, bh: i32) {
        // Draw the disc but only inside the rect's corner box.
        let stride = self.w as usize;
        let (w, h) = (self.w, self.h);
        let px = self.pixels();
        let (left, top) = (cx < bx + bw / 2, cy < by + bh / 2);
        let xs = if left { bx..cx + 1 } else { cx..bx + bw };
        let ys = if top { by..cy + 1 } else { cy..by + bh };
        for y in ys.clone() {
            if y < 0 || y >= h {
                continue;
            }
            for x in xs.clone() {
                if x < 0 || x >= w {
                    continue;
                }
                let mut n = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let dx = (x - cx) * 4 + sx - 2;
                        let dy = (y - cy) * 4 + sy - 2;
                        if dx * dx + dy * dy <= 16 * r * r {
                            n += 1;
                        }
                    }
                }
                if n > 0 {
                    let p = &mut px[y as usize * stride + x as usize];
                    *p = blend(*p, c, (n * 255 / 16) as u8);
                }
            }
        }
    }

    /// A line of round dots of radius `r` from (x0, y0) to (x1, y1) (a brush stroke).
    pub fn stroke(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, r: i32, c: Color) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let steps = dx.abs().max(dy.abs()).max(1);
        let every = (r / 2).max(1);
        let mut i = 0;
        while i <= steps {
            self.disc(x0 + dx * i / steps, y0 + dy * i / steps, r, c);
            i += every;
        }
        self.disc(x1, y1, r, c);
    }

    /// Text in Unifont 8x16 (the kernel's font), `scale`x. Returns the end x.
    pub fn text_x(&mut self, x: i32, y: i32, s: &str, c: Color, scale: i32) -> i32 {
        let stride = self.w as usize;
        let (w, h) = (self.w, self.h);
        let px = self.pixels();
        let mut cx = x;
        for ch in s.chars() {
            let cp = ch as u32;
            let g = if (font::FIRST_CHAR..=font::LAST_CHAR).contains(&cp) {
                &font::GLYPHS[(cp - font::FIRST_CHAR) as usize]
            } else {
                &font::REPLACEMENT
            };
            for (row, bits) in g.iter().enumerate() {
                for col in 0..8 {
                    if bits & (0x80 >> col) == 0 {
                        continue;
                    }
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let (xx, yy) = (cx + col * scale + sx, y + row as i32 * scale + sy);
                            if xx >= 0 && yy >= 0 && xx < w && yy < h {
                                px[yy as usize * stride + xx as usize] = c;
                            }
                        }
                    }
                }
            }
            cx += 8 * scale;
        }
        cx
    }

    pub fn text(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.text_x(x, y, s, c, 1)
    }

    /// Faux bold, like the shell's titles.
    pub fn bold(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.text(x, y, s, c);
        self.text(x + 1, y, s, c)
    }

    /// A button (SPEC "Buttons"): 24 high is a fully rounded pill, taller is a dialog button
    /// (radius 8). The label is bold and centred. A primary keeps its ink label in every
    /// state: rest straw, `hover` #EA9AA4, `pressed` #DE7D89.
    /// `r` is (x, y, w, h).
    pub fn button(
        &mut self,
        r: (i32, i32, i32, i32),
        label: &str,
        kind: ButtonKind,
        hover: bool,
        pressed: bool,
        th: ButtonTheme,
    ) {
        let (x, y, w, h) = r;
        let r = if h <= 24 { h / 2 } else { 8 };
        let (fill, text, border) = match kind {
            ButtonKind::Primary => {
                let fill = if pressed {
                    STRAW_PRESSED
                } else if hover {
                    STRAW_HOVER
                } else {
                    STRAW
                };
                (fill, INK, None)
            }
            ButtonKind::Secondary => (th.oat, th.text, Some(th.sand)),
            ButtonKind::Danger => (DANGER, DANGER_FG, None),
            ButtonKind::Disabled => (th.oat, th.latte, Some(th.sand)),
        };
        if let Some(b) = border {
            self.rrect(x, y, w, h, r, b);
            self.rrect(x + 1, y + 1, w - 2, h - 2, (r - 1).max(0), fill);
        } else {
            self.rrect(x, y, w, h, r, fill);
        }
        let tw = str_width(label, 1) + 1;
        self.bold(x + (w - tw) / 2, y + (h - 16) / 2, label, text);
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: plain integer; the kernel unmaps the buffer.
        unsafe { sys::syscall(WIN_DESTROY, [self.id as usize, 0, 0, 0, 0, 0]) };
    }
}

/// Blend `src` over `dst` at `a` (0..=255).
pub fn blend(dst: Color, src: Color, a: u8) -> Color {
    let a = a as u32;
    let ia = 255 - a;
    let ch = |s: u32| {
        let (d, x) = ((dst >> s) & 0xFF, (src >> s) & 0xFF);
        ((x * a + d * ia + 127) / 255) << s
    };
    ch(16) | ch(8) | ch(0)
}

pub fn str_width(s: &str, scale: i32) -> i32 {
    s.chars().count() as i32 * 8 * scale
}
