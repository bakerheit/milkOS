//! Host-test models of the 3.5-inch display's two chips: a panel that consumes the SPI byte
//! stream the driver produces (`SimPanel`, with its graphics memory and what a viewer would see
//! of it) and the XPT2046 touch chip (`SimTouch`). They know the controllers only as far as the
//! datasheets say, and they complain (in `errors`) about anything the real chips would not take:
//! a command that is not framed as 16-bit words on the shift-register kit, pixels in the wrong
//! format, a window outside the memory.
//!
//! One thing here is an assumption and not a datasheet fact: how the kit's glass is mounted
//! relative to the controller's memory. The model has the kit's picture mirrored left to right
//! at the controller's native orientation (which is why the portrait setting needs the column
//! mirror bit), and the viewer turns the display a quarter turn for each rotation step. The
//! tests prove the four rotation settings are four true rotations of each other under it; whether
//! the real glass agrees is for the first boot on the real board (`milk.lcd_rot=` and
//! `milk.lcd_madctl=` exist for the day it does not).

use std::vec::Vec;

use crate::lcd::{
    self, Bus, CASET, COLMOD, Config, Controller, DISPON, INVOFF, INVON, MADCTL, MADCTL_BGR,
    MADCTL_MV, MADCTL_MX, MADCTL_MY, NORON, PASET, RAMWR, Rotation, SLPOUT, SWRESET,
};
use crate::touch::{BURST_LEN, TouchBus};

pub struct SimPanel {
    pub kind: Controller,
    /// The controller's memory, portrait (320x480, or 240x320 for the ILI9341 and ST7789).
    pub nw: usize,
    pub nh: usize,
    /// Graphics memory in the controller's native orientation, `0x00RRGGBB` as the controller
    /// took the colour (before the BGR bit is applied).
    pub gram: Vec<u32>,
    pub display_on: bool,
    pub sleeping: bool,
    pub inverted: bool,
    pub madctl: u8,
    pub colmod: u8,
    caset: (u16, u16),
    paset: (u16, u16),
    cmd: Option<u8>,
    params: Vec<u8>,
    writing: bool,
    pos: (u16, u16),
    pending: Vec<u8>,
    /// Pixels written since the last reset of the counter.
    pub pixels_written: usize,
    /// Every transfer: (D/C high, bytes).
    pub log: Vec<(bool, Vec<u8>)>,
    /// The levels the reset line was driven to, in order.
    pub resets: Vec<bool>,
    /// Total milliseconds the driver waited.
    pub waited_ms: u32,
    /// Things a real chip would not accept.
    pub errors: Vec<&'static str>,
    /// Fail (return false from) the transfer with this index.
    pub fail_at: Option<usize>,
}

impl SimPanel {
    pub fn new(kind: Controller) -> SimPanel {
        let (nw, nh) = kind.native();
        SimPanel {
            kind,
            nw,
            nh,
            gram: std::vec![0; nw * nh],
            display_on: false,
            sleeping: true,
            inverted: false,
            madctl: 0,
            colmod: 0x66,
            caset: (0, nw as u16 - 1),
            paset: (0, nh as u16 - 1),
            cmd: None,
            params: Vec::new(),
            writing: false,
            pos: (0, 0),
            pending: Vec::new(),
            pixels_written: 0,
            log: Vec::new(),
            resets: Vec::new(),
            waited_ms: 0,
            errors: Vec::new(),
            fail_at: None,
        }
    }

    fn finish_command(&mut self) {
        let Some(cmd) = self.cmd.take() else { return };
        let p = core::mem::take(&mut self.params);
        match cmd {
            CASET | PASET => {
                if p.len() != 4 {
                    self.errors.push("window command without four parameters");
                    return;
                }
                let (a, b) = (
                    u16::from_be_bytes([p[0], p[1]]),
                    u16::from_be_bytes([p[2], p[3]]),
                );
                let limit = if self.madctl & MADCTL_MV != 0 {
                    if cmd == CASET { self.nh } else { self.nw }
                } else if cmd == CASET {
                    self.nw
                } else {
                    self.nh
                };
                if a > b || b as usize >= limit {
                    self.errors.push("window outside the memory");
                    return;
                }
                if cmd == CASET {
                    self.caset = (a, b);
                } else {
                    self.paset = (a, b);
                }
            }
            MADCTL => {
                if p.len() != 1 {
                    self.errors.push("madctl without one parameter");
                } else {
                    self.madctl = p[0];
                }
            }
            COLMOD => {
                if p.len() != 1 {
                    self.errors.push("colmod without one parameter");
                } else {
                    self.colmod = p[0];
                    let ok = p[0] == self.kind.colmod();
                    if !ok {
                        self.errors
                            .push("pixel format the controller cannot take over SPI");
                    }
                }
            }
            _ => {}
        }
    }

    /// Where the memory address `(c, r)` of the window lands in the graphics memory.
    fn gram_index(&self, c: u16, r: u16) -> usize {
        let (mut x, mut y) = if self.madctl & MADCTL_MV != 0 {
            (r as usize, c as usize)
        } else {
            (c as usize, r as usize)
        };
        if self.madctl & MADCTL_MX != 0 {
            x = self.nw - 1 - x;
        }
        if self.madctl & MADCTL_MY != 0 {
            y = self.nh - 1 - y;
        }
        y * self.nw + x
    }

    fn put_pixel(&mut self, colour: u32) {
        let (c, r) = self.pos;
        let i = self.gram_index(c, r);
        if i < self.gram.len() {
            self.gram[i] = colour;
        }
        self.pixels_written += 1;
        self.pos.0 += 1;
        if self.pos.0 > self.caset.1 {
            self.pos.0 = self.caset.0;
            self.pos.1 += 1;
            if self.pos.1 > self.paset.1 {
                self.pos.1 = self.paset.0;
            }
        }
    }

    fn data(&mut self, bytes: &[u8]) {
        if self.writing {
            let bpp = if self.colmod == 0x66 { 3 } else { 2 };
            for &b in bytes {
                self.pending.push(b);
                if self.pending.len() == bpp {
                    let colour = if bpp == 2 {
                        lcd::rgb565_to_xrgb(u16::from_be_bytes([self.pending[0], self.pending[1]]))
                    } else {
                        let f = |v: u8| ((v & 0xfc) as u32) | ((v >> 6) as u32);
                        f(self.pending[0]) << 16 | f(self.pending[1]) << 8 | f(self.pending[2])
                    };
                    self.pending.clear();
                    self.put_pixel(colour);
                }
            }
        } else if self.kind.word16() {
            if bytes.len() % 2 != 0 {
                self.errors
                    .push("odd number of bytes in a 16-bit parameter transfer");
            }
            for w in bytes.chunks(2) {
                if w[0] != 0 {
                    self.errors
                        .push("16-bit parameter word with a nonzero high byte");
                }
                self.params.push(*w.last().unwrap());
            }
        } else {
            self.params.extend_from_slice(bytes);
        }
    }

    fn command(&mut self, bytes: &[u8]) {
        self.finish_command();
        self.writing = false;
        self.pending.clear();
        let cmd = if self.kind.word16() {
            if bytes.len() != 2 || bytes[0] != 0 {
                self.errors.push("command not sent as one 16-bit word");
                return;
            }
            bytes[1]
        } else {
            if bytes.len() != 1 {
                self.errors.push("command transfer is not one byte");
                return;
            }
            bytes[0]
        };
        self.cmd = Some(cmd);
        match cmd {
            SWRESET => {
                self.sleeping = true;
                self.display_on = false;
                self.madctl = 0;
                self.inverted = false;
                self.caset = (0, self.nw as u16 - 1);
                self.paset = (0, self.nh as u16 - 1);
            }
            SLPOUT => self.sleeping = false,
            DISPON => {
                if self.sleeping {
                    self.errors
                        .push("display on while the controller still sleeps");
                }
                self.display_on = true;
            }
            INVON => self.inverted = true,
            INVOFF => self.inverted = false,
            NORON => {}
            RAMWR => {
                if self.sleeping {
                    self.errors.push("memory write while the controller sleeps");
                }
                self.writing = true;
                self.pos = (self.caset.0, self.paset.0);
            }
            _ => {}
        }
    }

    /// What a viewer sees of the glass, when the display is held the way `rot` means: upright
    /// pixels, `width x height` of that rotation, `0x00RRGGBB`. Row-major.
    pub fn visible(&self, rot: Rotation) -> (usize, usize, Vec<u32>) {
        let (nw, nh) = (self.nw, self.nh);
        // The picture as shown in the portrait setting: the kit's glass mirrors the memory.
        let portrait = |c: usize, r: usize| {
            let mut v = self.gram[r * nw + (nw - 1 - c)];
            // The glass has BGR colour filters; the BGR bit puts that right.
            if self.madctl & MADCTL_BGR == 0 {
                v = (v & 0xff00) | ((v & 0xff) << 16) | ((v >> 16) & 0xff);
            }
            // The usual ST7796 (and ST7789) module is an IPS glass that shows inverted unless the
            // inversion is on.
            if self.inverted != self.kind.usually_inverted() {
                v = !v & 0x00ff_ffff;
            }
            v
        };
        match rot {
            Rotation::Deg0 => {
                let mut out = Vec::new();
                for r in 0..nh {
                    for c in 0..nw {
                        out.push(portrait(c, r));
                    }
                }
                (nw, nh, out)
            }
            Rotation::Deg180 => {
                let mut out = Vec::new();
                for r in 0..nh {
                    for c in 0..nw {
                        out.push(portrait(nw - 1 - c, nh - 1 - r));
                    }
                }
                (nw, nh, out)
            }
            Rotation::Deg90 => {
                // Landscape (c', r'): portrait column nw - 1 - r', row c'.
                let mut out = Vec::new();
                for r in 0..nw {
                    for c in 0..nh {
                        out.push(portrait(nw - 1 - r, c));
                    }
                }
                (nh, nw, out)
            }
            Rotation::Deg270 => {
                // Landscape (c', r'): portrait column r', row nh - 1 - c'.
                let mut out = Vec::new();
                for r in 0..nw {
                    for c in 0..nh {
                        out.push(portrait(r, nh - 1 - c));
                    }
                }
                (nh, nw, out)
            }
        }
    }

    /// The same, for a driver configuration.
    pub fn visible_for(&self, cfg: &Config) -> Vec<u32> {
        self.visible(cfg.rotation).2
    }
}

impl Bus for SimPanel {
    fn write(&mut self, data: bool, bytes: &[u8]) -> bool {
        let n = self.log.len();
        self.log.push((data, bytes.to_vec()));
        if self.fail_at == Some(n) {
            return false;
        }
        if data {
            self.data(bytes);
        } else {
            self.command(bytes);
        }
        true
    }
    fn reset_pin(&mut self, high: bool) {
        self.resets.push(high);
    }
    fn delay_ms(&mut self, ms: u32) {
        self.waited_ms += ms;
    }
}

/// The XPT2046 with a pen on it (or not).
#[derive(Default)]
pub struct SimTouch {
    /// Raw position and pressure of the pen, if it touches.
    pub pen: Option<(u16, u16, u16)>,
    /// Added to x and y of successive bursts, cycling (noise).
    pub noise: Vec<(i16, i16)>,
    pub bursts: usize,
    pub fail: bool,
    /// Control bytes seen.
    pub controls: Vec<u8>,
}

impl TouchBus for SimTouch {
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> bool {
        if self.fail {
            return false;
        }
        let (dx, dy) = if self.noise.is_empty() {
            (0, 0)
        } else {
            self.noise[self.bursts % self.noise.len()]
        };
        if tx.len() == BURST_LEN {
            self.bursts += 1;
        }
        for (g, group) in tx.chunks(3).enumerate() {
            let ctl = group[0];
            if ctl & 0x80 == 0 {
                continue;
            }
            self.controls.push(ctl);
            let v: u16 = match (self.pen, (ctl >> 4) & 7) {
                (None, 3) => 0,
                (None, 4) => 4095,
                (None, _) => 0,
                (Some((x, _, _)), 5) => (x as i32 + dx as i32).clamp(0, 4095) as u16,
                (Some((_, y, _)), 1) => (y as i32 + dy as i32).clamp(0, 4095) as u16,
                (Some((_, _, p)), 3) => p,
                (Some(_), 4) => 4095,
                _ => 0,
            };
            let w = v << 3;
            if 3 * g + 2 < rx.len() {
                rx[3 * g] = 0;
                rx[3 * g + 1] = (w >> 8) as u8;
                rx[3 * g + 2] = w as u8;
            }
        }
        true
    }
}
