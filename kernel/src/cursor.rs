//! The mouse cursor: a small arrow sprite alpha-blended straight onto the ramfb framebuffer.
//!
//! It saves the pixels under it before drawing and puts them back before it moves, so it
//! never damages what is underneath. Anything else that draws on the framebuffer must take
//! the cursor off first, or it would draw over the sprite (and the saved pixels would go
//! stale): wrap such drawing in a [`suspend`] guard. The console (`sink.rs`) does.
//!
//! Self-contained on purpose: once a compositor exists, the cursor becomes its top layer and
//! this module either goes away or keeps only the sprite.
//!
//! Locking: one [`SpinLock`] (IRQs masked while held), so [`move_to`] is safe from thread
//! and IRQ context. The demo calls it from the main loop.

// Public API surface for the compositor; not all of it is used yet.
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

use crate::gfx::{Color, blend};
use crate::ramfb::{self, HEIGHT, WIDTH};
use crate::sync::SpinLock;

/// The arrow: `X` outline, `.` fill, anything else transparent. Hotspot is the tip (0, 0).
const ART: [&str; 19] = [
    "X",
    "XX",
    "X.X",
    "X..X",
    "X...X",
    "X....X",
    "X.....X",
    "X......X",
    "X.......X",
    "X........X",
    "X.........X",
    "X......XXXXX",
    "X...X..X",
    "X..XX..X",
    "X.X  X..X",
    "XX   X..X",
    "X     X..X",
    "      X..X",
    "       XX",
];
const ART_W: usize = 12;
const ART_H: usize = ART.len();
/// Sprite box: the art plus a 2-pixel drop shadow to the bottom right.
pub const SPRITE_W: usize = ART_W + 2;
pub const SPRITE_H: usize = ART_H + 2;
pub const HOTSPOT: (i32, i32) = (0, 0);

const OUTLINE: Color = 0x0018_1714;
const FILL: Color = 0x00FF_FCF2;

const fn art(r: isize, c: isize) -> u8 {
    if r < 0 || c < 0 || r as usize >= ART_H {
        return 0;
    }
    let row = ART[r as usize].as_bytes();
    if c as usize >= row.len() {
        return 0;
    }
    match row[c as usize] {
        b'X' => 1,
        b'.' => 2,
        _ => 0,
    }
}

/// ARGB pixels (alpha in the top byte), built at compile time.
const fn build_sprite() -> [u32; SPRITE_W * SPRITE_H] {
    let mut out = [0u32; SPRITE_W * SPRITE_H];
    let mut r = 0;
    while r < SPRITE_H {
        let mut c = 0;
        while c < SPRITE_W {
            let (ri, ci) = (r as isize, c as isize);
            out[r * SPRITE_W + c] = match art(ri, ci) {
                1 => 0xFF00_0000 | OUTLINE,
                2 => 0xFF00_0000 | FILL,
                _ if art(ri - 1, ci - 1) != 0 => 0x5000_0000, // soft shadow, near
                _ if art(ri - 2, ci - 2) != 0 => 0x2400_0000, // and far
                _ => 0,
            };
            c += 1;
        }
        r += 1;
    }
    out
}

/// The arrow as ARGB pixels (alpha in the top byte), `SPRITE_W` x `SPRITE_H`, hotspot [`HOTSPOT`].
pub static SPRITE: [u32; SPRITE_W * SPRITE_H] = build_sprite();

struct Cursor {
    fb: *mut u32,
    /// Pointer position (the hotspot).
    x: i32,
    y: i32,
    /// Top-left of the sprite currently on screen, if it is drawn.
    drawn_at: Option<(i32, i32)>,
    /// Nesting depth of `suspend` / `hide`.
    hidden: u32,
    save: [u32; SPRITE_W * SPRITE_H],
}

// SAFETY: `fb` is the static framebuffer; all access is under the lock.
unsafe impl Send for Cursor {}

static CURSOR: SpinLock<Cursor> = SpinLock::new(Cursor {
    fb: core::ptr::null_mut(),
    x: 0,
    y: 0,
    drawn_at: None,
    hidden: 0,
    save: [0; SPRITE_W * SPRITE_H],
});

/// Visit the on-screen pixels of a sprite whose top-left is (ox, oy): (sprite index, fb index).
fn for_each_px(ox: i32, oy: i32, mut f: impl FnMut(usize, usize)) {
    for r in 0..SPRITE_H as i32 {
        let y = oy + r;
        if y < 0 || y >= HEIGHT as i32 {
            continue;
        }
        for c in 0..SPRITE_W as i32 {
            let x = ox + c;
            if x < 0 || x >= WIDTH as i32 {
                continue;
            }
            f(
                (r as usize) * SPRITE_W + c as usize,
                y as usize * WIDTH + x as usize,
            );
        }
    }
}

impl Cursor {
    fn draw(&mut self) {
        if self.fb.is_null() || self.hidden > 0 || self.drawn_at.is_some() {
            return;
        }
        let (ox, oy) = (self.x - HOTSPOT.0, self.y - HOTSPOT.1);
        let fb = self.fb;
        let save = &mut self.save;
        for_each_px(ox, oy, |si, fi| {
            // SAFETY: fi < WIDTH * HEIGHT (clipped above); aligned u32 accesses.
            unsafe {
                let p = fb.add(fi);
                let under = read_volatile(p);
                save[si] = under;
                let s = SPRITE[si];
                let a = (s >> 24) as u8;
                if a != 0 {
                    write_volatile(p, blend(under, s & 0x00FF_FFFF, a));
                }
            }
        });
        self.drawn_at = Some((ox, oy));
    }

    fn erase(&mut self) {
        let Some((ox, oy)) = self.drawn_at.take() else {
            return;
        };
        let fb = self.fb;
        let save = &self.save;
        for_each_px(ox, oy, |si, fi| {
            if SPRITE[si] >> 24 != 0 {
                // SAFETY: as in draw.
                unsafe { write_volatile(fb.add(fi), save[si]) };
            }
        });
    }
}

/// Show the cursor at the current pointer position. Does nothing without a framebuffer.
pub fn init() {
    let Some(fb) = ramfb::framebuffer() else {
        return;
    };
    let (x, y) = crate::input::pointer();
    let mut c = CURSOR.lock();
    c.fb = fb;
    c.x = x;
    c.y = y;
    c.draw();
}

/// Move the cursor hotspot to (x, y) (screen pixels, clamped).
pub fn move_to(x: i32, y: i32) {
    let mut c = CURSOR.lock();
    let (x, y) = (x.clamp(0, WIDTH as i32 - 1), y.clamp(0, HEIGHT as i32 - 1));
    if (x, y) == (c.x, c.y) {
        return;
    }
    c.erase();
    c.x = x;
    c.y = y;
    c.draw();
}

/// Where the cursor hotspot is.
pub fn position() -> (i32, i32) {
    let c = CURSOR.lock();
    (c.x, c.y)
}

/// Take the cursor off the screen (nests; undo with [`show`]).
pub fn hide() {
    let mut c = CURSOR.lock();
    c.erase();
    c.hidden += 1;
}

/// Undo one [`hide`]; the cursor reappears when the last one is undone.
pub fn show() {
    let mut c = CURSOR.lock();
    c.hidden = c.hidden.saturating_sub(1);
    c.draw();
}

/// Keeps the cursor off the screen while something else draws on the framebuffer.
pub struct Suspended(bool);

/// Hide the cursor until the returned guard drops. Never blocks: if the cursor is mid-update
/// (a panic inside the cursor code), it does nothing rather than deadlock.
pub fn suspend() -> Suspended {
    match CURSOR.try_lock() {
        Some(mut c) => {
            c.erase();
            c.hidden += 1;
            Suspended(true)
        }
        None => Suspended(false),
    }
}

impl Drop for Suspended {
    fn drop(&mut self) {
        if self.0
            && let Some(mut c) = CURSOR.try_lock()
        {
            c.hidden = c.hidden.saturating_sub(1);
            c.draw();
        }
    }
}
