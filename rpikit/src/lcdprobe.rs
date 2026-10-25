//! `milk.lcd=probe` (docs/raspberry-pi.md, "3.5-inch SPI display"): for a kit whose controller nobody knows. These
//! displays are write-only over SPI, so the chip cannot be read back; instead the kernel runs through the
//! candidate set-ups one after another and draws a card on each, and the person who sees a readable card
//! knows which set-up works and what to put in `cmdline.txt`.
//!
//! The pure parts are here: the list of set-ups ([`SETUPS`]), the card as a [`PixelSource`] ([`ProbeCard`]: the
//! set-up's name in big letters, the matching options under it, colour bars red, green, blue, white, black,
//! and a white border), and a tiny 5x7 font for it (own shapes, capitals and digits only, so the options are
//! shown in capitals: type them in lower case). The kernel (`kernel/src/rpi/lcd.rs`) sends them.

use crate::lcd::{Config, Controller, PixelSource, Rotation};
use core::fmt::Write;

/// How a set-up differs from the plain one: the controller, and the SPI clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub controller: Controller,
    /// The bit clock in Hz; 0 = whatever `milk.lcd_hz` says (default 16 MHz).
    pub hz: u32,
}

/// The slow clock: some kits' wiring or shift registers do not keep up with 16 MHz.
pub const SLOW_HZ: u32 = 4_000_000;

/// Candidate set-ups, in the order they are shown: each controller at the normal clock, then the slow one.
pub const SETUPS: [Setup; 6] = [
    Setup {
        controller: Controller::Ili9486,
        hz: 0,
    },
    Setup {
        controller: Controller::Ili9488,
        hz: 0,
    },
    Setup {
        controller: Controller::St7796,
        hz: 0,
    },
    Setup {
        controller: Controller::Ili9486,
        hz: SLOW_HZ,
    },
    Setup {
        controller: Controller::Ili9488,
        hz: SLOW_HZ,
    },
    Setup {
        controller: Controller::St7796,
        hz: SLOW_HZ,
    },
];

/// The two landscape rotations tried on each set-up (the second is only a memory access control byte, no new init).
pub const ROTATIONS: [Rotation; 2] = [Rotation::Deg90, Rotation::Deg270];
/// How long each card stays (ms): about 6 s per set-up with its init and two rotations.
pub const HOLD_MS: u32 = 2500;
/// How many times the whole list is shown before the boot goes on.
pub const ROUNDS: u32 = 2;

impl Setup {
    pub fn config(&self, rotation: Rotation) -> Config {
        let mut c = Config::new(self.controller);
        c.rotation = rotation;
        c
    }

    pub const fn is_slow(&self) -> bool {
        self.hz != 0
    }
}

/// A small fixed text buffer (no allocation: the card is made in the kernel's init path).
#[derive(Clone, Copy)]
pub struct Text<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Text<N> {
    pub const fn new() -> Self {
        Text {
            buf: [0; N],
            len: 0,
        }
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for Text<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            if self.len < N {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// `milk.lcd=ili9486 milk.lcd_rot=90` (and `milk.lcd_hz=4000000` for a slow set-up): what goes in `cmdline.txt`,
/// in two parts that fit a line each.
pub fn options(setup: &Setup, rotation: Rotation) -> (Text<24>, Text<48>) {
    let mut a = Text::new();
    let mut b = Text::new();
    let _ = write!(a, "milk.lcd={}", setup.controller.name());
    let _ = write!(b, "milk.lcd_rot={}", rotation.degrees());
    if setup.is_slow() {
        let _ = write!(b, " milk.lcd_hz={}", setup.hz);
    }
    (a, b)
}

/// The name shown in big letters.
pub fn label(setup: &Setup) -> Text<16> {
    let mut t = Text::new();
    for c in setup.controller.name().bytes() {
        let _ = t.write_char(c.to_ascii_uppercase() as char);
    }
    if setup.is_slow() {
        let _ = t.write_str(" SLOW");
    }
    t
}

// ---- the font ---------------------------------------------------------------------------------------

/// Five-pixel-wide, seven-row glyphs, one row per byte (bit 4 is the leftmost pixel).
const fn g(rows: [u8; 7]) -> [u8; 7] {
    rows
}

const BOX: [u8; 7] = g([
    0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111,
]);

/// The glyph for `c` (lower case is shown as capitals; anything unknown is a box).
pub fn glyph(c: u8) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        b' ' => [0; 7],
        b'0' => g([
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ]),
        b'1' => g([
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ]),
        b'2' => g([
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ]),
        b'3' => g([
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ]),
        b'4' => g([
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ]),
        b'5' => g([
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ]),
        b'6' => g([
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ]),
        b'7' => g([
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ]),
        b'8' => g([
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ]),
        b'9' => g([
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ]),
        b'A' => g([
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ]),
        b'B' => g([
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ]),
        b'C' => g([
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ]),
        b'D' => g([
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ]),
        b'E' => g([
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ]),
        b'F' => g([
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ]),
        b'G' => g([
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ]),
        b'H' => g([
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ]),
        b'I' => g([
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ]),
        b'J' => g([
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ]),
        b'K' => g([
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ]),
        b'L' => g([
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ]),
        b'M' => g([
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ]),
        b'N' => g([
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ]),
        b'O' => g([
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ]),
        b'P' => g([
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ]),
        b'Q' => g([
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ]),
        b'R' => g([
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ]),
        b'S' => g([
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ]),
        b'T' => g([
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ]),
        b'U' => g([
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ]),
        b'V' => g([
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ]),
        b'W' => g([
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ]),
        b'X' => g([
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ]),
        b'Y' => g([
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ]),
        b'Z' => g([
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ]),
        b'=' => g([
            0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000,
        ]),
        b'.' => g([
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ]),
        b'_' => g([
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ]),
        b'-' => g([
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ]),
        b':' => g([
            0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000,
        ]),
        b'/' => g([
            0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000,
        ]),
        _ => BOX,
    }
}

// ---- the card ----------------------------------------------------------------------------------------

pub const BG: u32 = 0x0030_3030;
pub const WHITE: u32 = 0x00ff_ffff;
/// The colour bars, left to right.
pub const BARS: [u32; 5] = [
    0x00ff_0000,
    0x0000_ff00,
    0x0000_00ff,
    0x00ff_ffff,
    0x0000_0000,
];
/// Border width and margin.
pub const BORDER: usize = 4;
const MARGIN: usize = 12;

/// One text line of the card.
#[derive(Clone, Copy)]
struct Line<const N: usize> {
    text: Text<N>,
    y: usize,
    scale: usize,
}

impl<const N: usize> Line<N> {
    /// Is pixel `(x, y)` a lit pixel of this line's text?
    fn lit(&self, x: usize, y: usize) -> bool {
        let h = 7 * self.scale;
        if y < self.y || y >= self.y + h || x < MARGIN {
            return false;
        }
        let adv = 6 * self.scale;
        let i = (x - MARGIN) / adv;
        let col = ((x - MARGIN) % adv) / self.scale;
        if i >= self.text.len || col >= 5 {
            return false;
        }
        let row = (y - self.y) / self.scale;
        glyph(self.text.buf[i])[row] & (0x10 >> col) != 0
    }
}

/// The card for one set-up and rotation, `w` x `h` pixels (the panel as rotated).
#[derive(Clone, Copy)]
pub struct ProbeCard {
    w: usize,
    h: usize,
    name: Line<16>,
    opt_a: Line<24>,
    opt_b: Line<48>,
    info: Line<40>,
    bars_y: usize,
}

impl ProbeCard {
    /// `index` of `count` (from 1) names the place in the list.
    pub fn new(
        index: usize,
        count: usize,
        setup: &Setup,
        rotation: Rotation,
        w: usize,
        h: usize,
    ) -> ProbeCard {
        let name = label(setup);
        // The biggest scale (at most 6) at which the name fits the width.
        let mut scale = 6;
        while scale > 1 && name.len * 6 * scale + 2 * MARGIN > w {
            scale -= 1;
        }
        let small = if w >= 400 { 2 } else { 1 };
        let (a, b) = options(setup, rotation);
        let mut info = Text::<40>::new();
        let _ = write!(info, "SETUP {index} OF {count}  ROT {}", rotation.degrees());
        let y0 = BORDER + 8;
        let y1 = y0 + 7 * scale + 10;
        let line_h = 7 * small + 6;
        ProbeCard {
            w,
            h,
            name: Line {
                text: name,
                y: y0,
                scale,
            },
            opt_a: Line {
                text: a,
                y: y1,
                scale: small,
            },
            opt_b: Line {
                text: b,
                y: y1 + line_h,
                scale: small,
            },
            info: Line {
                text: info,
                y: y1 + 2 * line_h,
                scale: small,
            },
            bars_y: y1 + 3 * line_h + 8,
        }
    }

    /// The colour of pixel `(x, y)` (`0x00RRGGBB`).
    pub fn pixel(&self, x: usize, y: usize) -> u32 {
        if x < BORDER || y < BORDER || x + BORDER >= self.w || y + BORDER >= self.h {
            return WHITE;
        }
        if self.name.lit(x, y)
            || self.opt_a.lit(x, y)
            || self.opt_b.lit(x, y)
            || self.info.lit(x, y)
        {
            return WHITE;
        }
        let bars_x0 = BORDER + 8;
        let bars_x1 = self.w - BORDER - 8;
        if y >= self.bars_y && y + BORDER + 8 < self.h && x >= bars_x0 && x < bars_x1 {
            return BARS[(x - bars_x0) * BARS.len() / (bars_x1 - bars_x0)];
        }
        BG
    }

    /// Where the bars start (for tests and the doc).
    pub fn bars_top(&self) -> usize {
        self.bars_y
    }
}

impl PixelSource for ProbeCard {
    fn row(&mut self, y: usize, x: usize, out: &mut [u32]) {
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.pixel(x + i, y);
        }
    }
}
