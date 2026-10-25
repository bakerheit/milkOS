//! `milk.lcd=probe` (docs/raspberry-pi.md, "3.5-inch SPI display"): for a kit whose controller nobody knows. These
//! displays are write-only over SPI, so the chip cannot be read back; instead the kernel runs through a
//! matrix of candidate set-ups one after another (controller, SPI mode, bit clock, D/C and reset pins) and
//! shows a sign on each, so the person who sees anything knows which set-up number it was and what to put
//! in `cmdline.txt`.
//!
//! What one set-up shows, in this order (each part is useful even when the next one is not readable):
//!
//! 1. the **number card**: the set-up's number, huge, white on black, for [`number_ms`] (one second);
//! 2. the **colour flashes**: the whole screen solid red, green, blue, white, [`flash_ms`] (0.4 s) each, so a
//!    person who sees only a colour change can still say at which set-up number it happened (at a slow
//!    clock a full screen takes longer than that to send, so the flash covers the rows that fit: from the top);
//! 3. the **text card**: the controller's name, the options that select the set-up (type them in lower case),
//!    `SETUP n OF N`, colour bars and a border, filling the whole addressed area, for [`HOLD_MS`].
//!
//! The pure parts are here: the matrix ([`SETUPS`]), the cards as [`PixelSource`]s, the banded drawing that
//! stops at a deadline ([`draw_banded`]), the options text, and a tiny 5x7 font (own shapes, capitals and
//! digits only). The kernel (`kernel/src/rpi/lcd.rs`) wires them to SPI0 and the pins.

use crate::lcd::{self, Bus, Config, Controller, PixelSource, Rect, Rotation};
use core::fmt::Write;

/// The usual display D/C and reset pins (GPIO numbers): what a set-up without pins of its own uses.
pub const DEFAULT_PINS: (u8, u8) = (24, 25);
/// GPIO pins driven high as a backlight switch while probing, when `milk.lcd_bl` names none and
/// `milk.lcd_probe_pins` is not 0 (never the pair in use as D/C or reset).
pub const BACKLIGHT_PINS: [u8; 2] = [18, 27];

pub const MHZ4: u32 = 4_000_000;
pub const MHZ16: u32 = 16_000_000;
pub const MHZ32: u32 = 32_000_000;

/// How a set-up differs from the plain one: the controller, the SPI mode and clock, the pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub controller: Controller,
    /// The bit clock in Hz.
    pub hz: u32,
    /// SPI mode 0..=3 (clock polarity and phase): the kits want 0 or 3.
    pub mode: u8,
    /// D/C and reset GPIOs of this set-up; `None`: the ones of the options (`milk.lcd_dc`, `milk.lcd_rst`).
    pub pins: Option<(u8, u8)>,
}

const fn st(controller: Controller, hz: u32, mode: u8, pins: Option<(u8, u8)>) -> Setup {
    Setup {
        controller,
        hz,
        mode,
        pins,
    }
}

use Controller::{
    Hx8357, Ili9341, Ili9481, Ili9486, Ili9486P, Ili9486P18, Ili9488, R61581, St7789, St7796,
};

/// Alternative D/C and reset pairs of the second round (what other 3.5-inch Raspberry Pi kits use).
pub const PINS_22_27: Option<(u8, u8)> = Some((22, 27));
pub const PINS_25_24: Option<(u8, u8)> = Some((25, 24));
pub const PINS_23_24: Option<(u8, u8)> = Some((23, 24));

/// Candidate set-ups, in the order they are shown. First every controller at the slow clock, SPI mode 0
/// and the default pins; then the likeliest ones at 16 and 32 MHz; then SPI mode 3 (slow) for the likeliest
/// three; then, as a second round, the other common D/C and reset pairs for the two likeliest
/// controllers (slow, mode 0). `milk.lcd_probe_pins=0` stops after the first part.
pub const SETUPS: [Setup; 21] = [
    st(Ili9486, MHZ4, 0, None),
    st(Ili9486P, MHZ4, 0, None),
    st(Ili9486P18, MHZ4, 0, None),
    st(Ili9488, MHZ4, 0, None),
    st(St7796, MHZ4, 0, None),
    st(Ili9481, MHZ4, 0, None),
    st(R61581, MHZ4, 0, None),
    st(Hx8357, MHZ4, 0, None),
    st(Ili9341, MHZ4, 0, None),
    st(St7789, MHZ4, 0, None),
    st(Ili9486, MHZ16, 0, None),
    st(Ili9486P, MHZ16, 0, None),
    st(Ili9341, MHZ16, 0, None),
    st(Ili9486, MHZ32, 0, None),
    st(Ili9486, MHZ4, 3, None),
    st(Ili9486P, MHZ4, 3, None),
    st(Ili9341, MHZ4, 3, None),
    st(Ili9486, MHZ4, 0, PINS_22_27),
    st(Ili9486, MHZ4, 0, PINS_25_24),
    st(Ili9486P, MHZ4, 0, PINS_22_27),
    st(Ili9486P, MHZ4, 0, PINS_25_24),
];

/// How many set-ups run when the pin sweeps are off (`milk.lcd_probe_pins=0`): all but the pin round.
pub const SETUPS_WITHOUT_PIN_ROUND: usize = 17;

/// Every set-up is shown in this rotation: landscape (the card fills the whole addressed area, so
/// whichever way the glass is mounted something shows; the readable orientation is for the winner to fix
/// with `milk.lcd_rot=`).
pub const ROTATION: Rotation = Rotation::Deg90;

/// How long the text card stays (ms); the number card stays as long, a colour flash two fifths of it
/// (1000, 1000 and 400 ms). `milk.lcd_probe_ms=` changes it.
pub const HOLD_MS: u32 = 1000;
/// How many times the whole list is shown before the boot goes on.
pub const ROUNDS: u32 = 1;

/// How long the number card stays, given the text card's hold.
pub const fn number_ms(hold: u32) -> u32 {
    hold
}
/// How long each colour flash lasts.
pub const fn flash_ms(hold: u32) -> u32 {
    hold * 2 / 5
}
/// The flash colours, in order.
pub const FLASHES: [u32; 4] = [0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0x00ff_ffff];

impl Setup {
    pub fn config(&self) -> Config {
        let mut c = Config::new(self.controller);
        c.rotation = ROTATION;
        c
    }

    /// Is this a slow-clock set-up (4 MHz or less)?
    pub const fn is_slow(&self) -> bool {
        self.hz <= MHZ4
    }

    /// The D/C and reset pins this set-up uses, given those of the options.
    pub const fn pins_or(&self, base: (u8, u8)) -> (u8, u8) {
        match self.pins {
            Some(p) => p,
            None => base,
        }
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

/// The options that select `setup` on a real boot, as `cmdline.txt` words, in the order shown on the card:
/// `milk.lcd=NAME`, `milk.lcd_hz=N`, `milk.lcd_spi_mode=M` (if not 0), and `milk.lcd_dc=` / `milk.lcd_rst=`
/// (if not the usual 24 and 25).
pub fn option_words(setup: &Setup, base_pins: (u8, u8)) -> ([Text<24>; 5], usize) {
    let mut w = [Text::<24>::new(); 5];
    let mut n = 0;
    let _ = write!(w[n], "milk.lcd={}", setup.controller.name());
    n += 1;
    let _ = write!(w[n], "milk.lcd_hz={}", setup.hz);
    n += 1;
    if setup.mode != 0 {
        let _ = write!(w[n], "milk.lcd_spi_mode={}", setup.mode);
        n += 1;
    }
    let (dc, rst) = setup.pins_or(base_pins);
    if (dc, rst) != DEFAULT_PINS {
        let _ = write!(w[n], "milk.lcd_dc={dc}");
        n += 1;
        let _ = write!(w[n], "milk.lcd_rst={rst}");
        n += 1;
    }
    (w, n)
}

/// All the option words on one line (the log's version of the card).
pub fn options(setup: &Setup, base_pins: (u8, u8)) -> Text<112> {
    let (w, n) = option_words(setup, base_pins);
    let mut t = Text::new();
    for (i, x) in w[..n].iter().enumerate() {
        if i > 0 {
            let _ = t.write_char(' ');
        }
        let _ = t.write_str(x.as_str());
    }
    t
}

/// The option words packed into lines of at most `max` characters (at most four lines).
pub fn option_lines(setup: &Setup, base_pins: (u8, u8), max: usize) -> ([Text<48>; 4], usize) {
    let (w, n) = option_words(setup, base_pins);
    let mut lines = [Text::<48>::new(); 4];
    let mut k = 0;
    for x in &w[..n] {
        let cur = &mut lines[k];
        if cur.len > 0 && cur.len + 1 + x.len > max && k + 1 < lines.len() {
            k += 1;
        }
        let cur = &mut lines[k];
        if cur.len > 0 {
            let _ = cur.write_char(' ');
        }
        let _ = cur.write_str(x.as_str());
    }
    (lines, k + 1)
}

/// The name shown in big letters.
pub fn label(setup: &Setup) -> Text<16> {
    let mut t = Text::new();
    for c in setup.controller.name().bytes() {
        let _ = t.write_char(c.to_ascii_uppercase() as char);
    }
    t
}

/// What the log and the card say about the clock, mode and pins: `4 MHZ MODE 0 DC24 RST25`.
pub fn detail(setup: &Setup, base_pins: (u8, u8)) -> Text<40> {
    let (dc, rst) = setup.pins_or(base_pins);
    let mut t = Text::new();
    let _ = write!(
        t,
        "{} MHZ MODE {} DC{dc} RST{rst}",
        setup.hz / 1_000_000,
        setup.mode
    );
    t
}

/// The same for the klog line: `ili9486p 4 MHz mode 0 DC24 RST25`.
pub fn describe(setup: &Setup, base_pins: (u8, u8)) -> Text<64> {
    let (dc, rst) = setup.pins_or(base_pins);
    let mut t = Text::new();
    let _ = write!(
        t,
        "{} {} MHz mode {} DC{dc} RST{rst}",
        setup.controller.name(),
        setup.hz / 1_000_000,
        setup.mode
    );
    t
}

/// What the boot options say about the probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeOptions {
    /// How many times the list is shown (`milk.lcd_probe_rounds=`, 1..=10).
    pub rounds: u32,
    /// The text card's hold in ms (`milk.lcd_probe_ms=`, 50..=20000); see [`number_ms`], [`flash_ms`].
    pub hold_ms: u32,
    /// Start at this set-up, counting from 1 (`milk.lcd_probe_from=`).
    pub from: usize,
    /// The pin sweeps: the backlight pins driven high, and the round of other D/C and reset pairs
    /// (`milk.lcd_probe_pins=0` turns both off).
    pub pins: bool,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        ProbeOptions {
            rounds: ROUNDS,
            hold_ms: HOLD_MS,
            from: 1,
            pins: true,
        }
    }
}

impl ProbeOptions {
    /// Read the options through `get` (the value of `milk.<name>`, if present). Out-of-range numbers
    /// are clamped, unreadable ones ignored.
    pub fn parse<'a>(get: impl Fn(&str) -> Option<&'a [u8]>) -> ProbeOptions {
        let num = |name: &str| get(name).and_then(crate::cmdline::parse_num);
        let d = ProbeOptions::default();
        let mut o = ProbeOptions {
            rounds: num("lcd_probe_rounds").map_or(d.rounds, |v| v.clamp(1, 10) as u32),
            hold_ms: num("lcd_probe_ms").map_or(d.hold_ms, |v| v.clamp(50, 20_000) as u32),
            from: 1,
            pins: !matches!(
                get("lcd_probe_pins"),
                Some(b"0" | b"off" | b"no" | b"false")
            ),
        };
        let count = o.count();
        o.from = num("lcd_probe_from").map_or(1, |v| v.clamp(1, count as i64) as usize);
        o
    }

    /// How many set-ups a round has with these options.
    pub const fn count(&self) -> usize {
        if self.pins {
            SETUPS.len()
        } else {
            SETUPS_WITHOUT_PIN_ROUND
        }
    }
}

/// Rows per band of [`draw_banded`].
pub const BAND_ROWS: usize = 40;

/// Draw `src` over the whole screen of `cfg` from the top in bands of [`BAND_ROWS`] rows, asking
/// `stop()` before every band after the first and giving up when it says so: a slow clock cannot send a
/// full screen in the time a flash lasts, and the rows that fit are a better sign than none. Returns
/// the number of rows drawn, or `None` if the bus failed.
pub fn draw_banded<B: Bus, S: PixelSource>(
    cfg: &Config,
    bus: &mut B,
    src: &mut S,
    stop: &mut dyn FnMut() -> bool,
) -> Option<usize> {
    let (w, h) = (cfg.width(), cfg.height());
    let mut y = 0;
    while y < h {
        if y > 0 && stop() {
            break;
        }
        let n = BAND_ROWS.min(h - y);
        if !lcd::draw_rect(cfg, bus, Rect::new(0, y, w, n), src) {
            return None;
        }
        y += n;
    }
    Some(y)
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

// ---- the cards ---------------------------------------------------------------------------------------

pub const BG: u32 = 0x0030_3030;
pub const WHITE: u32 = 0x00ff_ffff;
pub const BLACK: u32 = 0x0000_0000;
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

/// One text line of a card.
#[derive(Clone, Copy)]
struct Line<const N: usize> {
    text: Text<N>,
    y: usize,
    scale: usize,
}

impl<const N: usize> Line<N> {
    const fn empty() -> Self {
        Line {
            text: Text::new(),
            y: 0,
            scale: 1,
        }
    }
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

/// The biggest scale (at most `max`) at which `chars` characters fit the width `w`.
fn fit_scale(chars: usize, w: usize, max: usize) -> usize {
    let mut scale = max;
    while scale > 1 && chars * 6 * scale + 2 * MARGIN > w {
        scale -= 1;
    }
    scale
}

fn on_border(x: usize, y: usize, w: usize, h: usize) -> bool {
    x < BORDER || y < BORDER || x + BORDER >= w || y + BORDER >= h
}

/// The number card: the set-up's number, huge, white on black, under it `OF N` and the controller's name.
/// It is the first thing a set-up shows, for a person who looks up only now and then.
#[derive(Clone, Copy)]
pub struct NumberCard {
    w: usize,
    h: usize,
    number: Line<4>,
    of: Line<12>,
    name: Line<16>,
}

impl NumberCard {
    /// Set-up `index` (from 1) of `count`, on a `w` x `h` screen.
    pub fn new(index: usize, count: usize, setup: &Setup, w: usize, h: usize) -> NumberCard {
        let mut number = Text::<4>::new();
        let _ = write!(number, "{index}");
        // Tall: about 55% of the height, as wide as the width allows.
        let mut scale = fit_scale(number.len, w, 40).min(h * 55 / 100 / 7).max(1);
        if scale == 0 {
            scale = 1;
        }
        let y0 = BORDER + 8;
        let y1 = y0 + 7 * scale + 10;
        let small = if w >= 400 { 3 } else { 2 };
        let mut of = Text::<12>::new();
        let _ = write!(of, "OF {count}");
        NumberCard {
            w,
            h,
            number: Line {
                text: number,
                y: y0,
                scale,
            },
            of: Line {
                text: of,
                y: y1,
                scale: small,
            },
            name: Line {
                text: label(setup),
                y: y1 + 7 * small + 10,
                scale: small.min(fit_scale(label(setup).len, w, small)),
            },
        }
    }

    pub fn pixel(&self, x: usize, y: usize) -> u32 {
        if on_border(x, y, self.w, self.h)
            || self.number.lit(x, y)
            || self.of.lit(x, y)
            || self.name.lit(x, y)
        {
            WHITE
        } else {
            BLACK
        }
    }

    /// The scale of the big number (for tests).
    pub fn number_scale(&self) -> usize {
        self.number.scale
    }
}

impl PixelSource for NumberCard {
    fn row(&mut self, y: usize, x: usize, out: &mut [u32]) {
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.pixel(x + i, y);
        }
    }
}

/// The text card for one set-up, `w` x `h` pixels (the screen as rotated): the controller's name in big
/// letters, the options that select it, `SETUP n OF N` and the clock, mode and pins, then colour bars and
/// a white border.
#[derive(Clone, Copy)]
pub struct ProbeCard {
    w: usize,
    h: usize,
    name: Line<16>,
    opts: [Line<48>; 4],
    info: Line<24>,
    detail: Line<40>,
    bars_y: usize,
}

impl ProbeCard {
    /// `index` of `count` (from 1) names the place in the list; `base_pins` are the D/C and reset the
    /// options name.
    pub fn new(
        index: usize,
        count: usize,
        setup: &Setup,
        base_pins: (u8, u8),
        w: usize,
        h: usize,
    ) -> ProbeCard {
        let name = label(setup);
        let scale = fit_scale(name.len, w, 6);
        let small = if w >= 400 { 2 } else { 1 };
        let chars = (w - 2 * MARGIN) / (6 * small);
        let (lines, n) = option_lines(setup, base_pins, chars);
        let y0 = BORDER + 8;
        let mut y = y0 + 7 * scale + 8;
        let line_h = 7 * small + 6;
        let mut opts = [Line::<48>::empty(); 4];
        for (k, o) in opts.iter_mut().enumerate().take(n) {
            *o = Line {
                text: lines[k],
                y,
                scale: small,
            };
            y += line_h;
        }
        let big = small + 1;
        let mut info_t = Text::<24>::new();
        let _ = write!(info_t, "SETUP {index} OF {count}");
        let info = Line {
            text: info_t,
            y: y + 4,
            scale: big,
        };
        y += 4 + 7 * big + 6;
        let detail = Line {
            text: detail(setup, base_pins),
            y,
            scale: small,
        };
        y += line_h + 4;
        ProbeCard {
            w,
            h,
            name: Line {
                text: name,
                y: y0,
                scale,
            },
            opts,
            info,
            detail,
            bars_y: y,
        }
    }

    /// The colour of pixel `(x, y)` (`0x00RRGGBB`).
    pub fn pixel(&self, x: usize, y: usize) -> u32 {
        if on_border(x, y, self.w, self.h) {
            return WHITE;
        }
        if self.name.lit(x, y)
            || self.info.lit(x, y)
            || self.detail.lit(x, y)
            || self.opts.iter().any(|o| o.lit(x, y))
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
