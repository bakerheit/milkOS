//! The resistive touch chip that comes with the 3.5-inch kits (docs/raspberry-pi.md, "3.5-inch SPI
//! display"): an XPT2046 (an ADS7846 compatible) on SPI chip select 1, with its pen-down line on a
//! GPIO. Everything here is pure: the command bytes and how to read the answers, the filter that
//! turns a handful of noisy readings into one, the affine calibration from raw readings to screen
//! pixels (and the fit that computes it from four touches), and the tracker that turns a stream of
//! "touching here / not touching" into pointer down, move and up events. The kernel feeds it from SPI.
//!
//! The chip is a 12-bit converter on a 4 wire resistive panel. One conversion is three bytes on the
//! wire: the control byte, then two bytes in which the result comes back as `0 D11..D0 000`.

use crate::lcd::Rotation;

/// The most the chip's serial clock may be (its datasheet says 2 MHz).
pub const MAX_HZ: u32 = 2_000_000;

/// A converter channel (the A2..A0 bits of the control byte).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Y = 1,
    Z1 = 3,
    Z2 = 4,
    X = 5,
}

/// The control byte for a 12-bit differential conversion of `ch`; `power_down` is PD1 PD0 (0: power
/// down after the conversion, which switches the pen-down line back on; 1: keep the converter on).
pub const fn control(ch: Channel, power_down: u8) -> u8 {
    0x80 | ((ch as u8) << 4) | (power_down & 3)
}

/// Bytes of one burst of readings (five conversions of three bytes).
pub const BURST_LEN: usize = 15;

/// The bytes to send for one reading: Z1, Z2, Y, X, then Y once more with the converter powered
/// down (that conversion's answer is not used; it leaves the chip ready to signal the next touch).
pub const fn burst() -> [u8; BURST_LEN] {
    let mut b = [0u8; BURST_LEN];
    b[0] = control(Channel::Z1, 1);
    b[3] = control(Channel::Z2, 1);
    b[6] = control(Channel::Y, 1);
    b[9] = control(Channel::X, 1);
    b[12] = control(Channel::Y, 0);
    b
}

/// The 12-bit result in the two bytes after a control byte.
pub const fn decode(hi: u8, lo: u8) -> u16 {
    ((((hi as u16) << 8) | lo as u16) >> 3) & 0xfff
}

/// One reading of the chip.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Raw {
    pub x: u16,
    pub y: u16,
    pub z1: u16,
    pub z2: u16,
}

impl Raw {
    /// How hard the pen presses, larger is harder: 0 when nothing touches (`z1` is 0 then and `z2`
    /// is full scale).
    pub fn pressure(&self) -> u16 {
        (self.z1 + 4095).saturating_sub(self.z2).min(4095)
    }
}

/// The answer of a [`burst`] (`rx` is what came back while it was sent, at least [`BURST_LEN`]
/// bytes), or `None` if it is too short.
pub fn parse_burst(rx: &[u8]) -> Option<Raw> {
    if rx.len() < BURST_LEN {
        return None;
    }
    let at = |g: usize| decode(rx[3 * g + 1], rx[3 * g + 2]);
    Some(Raw {
        z1: at(0),
        z2: at(1),
        y: at(2),
        x: at(3),
    })
}

/// How many readings are taken and filtered into one.
pub const SAMPLES: usize = 5;
/// The pressure from which a reading counts as a touch.
pub const PRESSURE_MIN: u16 = 400;
/// Readings of one touch that differ by more than this (raw units) are noise: no touch.
pub const SPREAD_MAX: u16 = 300;

/// A filtered touch: raw position and pressure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Touch {
    pub x: u16,
    pub y: u16,
    pub pressure: u16,
}

fn median(v: &mut [u16]) -> u16 {
    // Insertion sort: five numbers.
    for i in 1..v.len() {
        let mut j = i;
        while j > 0 && v[j - 1] > v[j] {
            v.swap(j - 1, j);
            j -= 1;
        }
    }
    v[v.len() / 2]
}

/// The touch in a set of readings: those that press hard enough and are not stuck at either end of
/// the scale (what an open circuit reads), at least half of them, whose median is taken for each of
/// x, y and the pressure. `None` if the readings do not agree, or there is no touch.
pub fn filter(samples: &[Raw]) -> Option<Touch> {
    let mut xs = [0u16; SAMPLES];
    let mut ys = [0u16; SAMPLES];
    let mut zs = [0u16; SAMPLES];
    let mut n = 0;
    for s in samples.iter().take(SAMPLES) {
        let edge = |v: u16| v < 8 || v > 4087;
        if s.pressure() >= PRESSURE_MIN && !edge(s.x) && !edge(s.y) {
            xs[n] = s.x;
            ys[n] = s.y;
            zs[n] = s.pressure();
            n += 1;
        }
    }
    if n == 0 || n * 2 < samples.len().min(SAMPLES) {
        return None;
    }
    let spread = |v: &[u16]| v.iter().max().unwrap() - v.iter().min().unwrap();
    if spread(&xs[..n]) > SPREAD_MAX || spread(&ys[..n]) > SPREAD_MAX {
        return None;
    }
    Some(Touch {
        x: median(&mut xs[..n]),
        y: median(&mut ys[..n]),
        pressure: median(&mut zs[..n]),
    })
}

/// The wire: one full duplex transfer on the chip's chip select, at no more than [`MAX_HZ`].
pub trait TouchBus {
    /// Send `tx` and store what comes back in `rx` (same length). False if the transfer failed.
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> bool;
}

/// Read the chip [`SAMPLES`] times and filter. `None`: no touch, or the bus failed.
pub fn read<B: TouchBus>(bus: &mut B) -> Option<Touch> {
    let tx = burst();
    let mut raws = [Raw::default(); SAMPLES];
    for r in raws.iter_mut() {
        let mut rx = [0u8; BURST_LEN];
        if !bus.transfer(&tx, &mut rx) {
            return None;
        }
        *r = parse_burst(&rx)?;
    }
    filter(&raws)
}

// ---- calibration --------------------------------------------------------------------------------

/// An affine map from raw readings to screen pixels:
/// `x = ((a * rx + b * ry) >> 16) + c` and `y = ((d * rx + e * ry) >> 16) + f`: the factors are
/// 16.16 fixed point, the offsets whole pixels. The boot option is `milk.touch_cal=a,b,c,d,e,f`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cal {
    pub a: i32,
    pub b: i32,
    pub c: i32,
    pub d: i32,
    pub e: i32,
    pub f: i32,
}

impl Cal {
    pub fn apply(&self, rx: i32, ry: i32) -> (i32, i32) {
        let (rx, ry) = (rx as i64, ry as i64);
        (
            (((self.a as i64 * rx + self.b as i64 * ry) >> 16) + self.c as i64) as i32,
            (((self.d as i64 * rx + self.e as i64 * ry) >> 16) + self.f as i64) as i32,
        )
    }

    /// Six comma separated numbers (decimal or `0x` hex, signed).
    pub fn parse(s: &[u8]) -> Option<Cal> {
        let mut v = [0i64; 6];
        let mut it = s.split(|&c| c == b',');
        for slot in v.iter_mut() {
            *slot = crate::cmdline::parse_num(it.next()?)?;
        }
        if it.next().is_some() || v.iter().any(|&n| n != n as i32 as i64) {
            return None;
        }
        Some(Cal {
            a: v[0] as i32,
            b: v[1] as i32,
            c: v[2] as i32,
            d: v[3] as i32,
            e: v[4] as i32,
            f: v[5] as i32,
        })
    }

    /// `a,b,c,d,e,f` as text, into `out`; returns the length.
    pub fn write(&self, out: &mut [u8; 96]) -> usize {
        let mut n = 0;
        for (i, v) in [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .enumerate()
        {
            if i > 0 {
                out[n] = b',';
                n += 1;
            }
            let mut digits = [0u8; 12];
            let mut k = 0;
            let mut m = v.unsigned_abs();
            loop {
                digits[k] = b'0' + (m % 10) as u8;
                k += 1;
                m /= 10;
                if m == 0 {
                    break;
                }
            }
            if *v < 0 {
                out[n] = b'-';
                n += 1;
            }
            while k > 0 {
                k -= 1;
                out[n] = digits[k];
                n += 1;
            }
        }
        n
    }

    /// The map a kit of this kind needs in each rotation, measured on the glass the kit's own
    /// documentation describes: raw x 300..3932 spans the panel's short side and raw y 294..3801 the
    /// long one (portrait, no swap, no inversion); the quarter turns are derived from that. The
    /// result is for a screen of the size [`Rotation`] gives (480x320 landscape, 320x480 portrait).
    /// This is a good starting point, not a calibration: `milk.touch_cal=` overrides it.
    pub fn default_for(rot: Rotation) -> Cal {
        // Portrait pixel from raw: px = (rx - 300) * 320 / 3632; py = (ry - 294) * 480 / 3507.
        let kx = 320i64 * 65536 / 3632;
        let ky = 480i64 * 65536 / 3507;
        let cx = -(300 * 320 / 3632);
        let cy = -(294 * 480 / 3507);
        let (px_max, py_max) = (319i64, 479i64);
        let k = |v: i64| v as i32;
        match rot {
            Rotation::Deg0 => Cal {
                a: k(kx),
                b: 0,
                c: k(cx),
                d: 0,
                e: k(ky),
                f: k(cy),
            },
            // Landscape: x = py, y = 319 - px.
            Rotation::Deg90 => Cal {
                a: 0,
                b: k(ky),
                c: k(cy),
                d: k(-kx),
                e: 0,
                f: k(px_max - cx),
            },
            Rotation::Deg180 => Cal {
                a: k(-kx),
                b: 0,
                c: k(px_max - cx),
                d: 0,
                e: k(-ky),
                f: k(py_max - cy),
            },
            // x = 479 - py, y = px.
            Rotation::Deg270 => Cal {
                a: 0,
                b: k(-ky),
                c: k(py_max - cy),
                d: k(kx),
                e: 0,
                f: k(cx),
            },
        }
    }
}

/// Solve the 3x3 system `m * s = v` (Gaussian elimination, partial pivoting).
fn solve3(mut m: [[f64; 3]; 3], mut v: [f64; 3]) -> Option<[f64; 3]> {
    for col in 0..3 {
        let mut piv = col;
        for r in col + 1..3 {
            if m[r][col].abs() > m[piv][col].abs() {
                piv = r;
            }
        }
        if m[piv][col].abs() < 1e-9 {
            return None;
        }
        m.swap(col, piv);
        v.swap(col, piv);
        for r in col + 1..3 {
            let f = m[r][col] / m[col][col];
            for c in col..3 {
                m[r][c] -= f * m[col][c];
            }
            v[r] -= f * v[col];
        }
    }
    let mut s = [0f64; 3];
    for r in (0..3).rev() {
        let mut t = v[r];
        for c in r + 1..3 {
            t -= m[r][c] * s[c];
        }
        s[r] = t / m[r][r];
    }
    Some(s)
}

/// The calibration that maps the raw readings `raw` to the screen points `screen` (the same number
/// of each, three or more, not on one line): a least squares fit of the six numbers. `None` if
/// they do not determine a map, or the result does not fit the fixed point format.
pub fn fit(raw: &[(i32, i32)], screen: &[(i32, i32)]) -> Option<Cal> {
    if raw.len() != screen.len() || raw.len() < 3 {
        return None;
    }
    let mut m = [[0f64; 3]; 3];
    let (mut vx, mut vy) = ([0f64; 3], [0f64; 3]);
    for (r, s) in raw.iter().zip(screen) {
        let row = [r.0 as f64, r.1 as f64, 1.0];
        for i in 0..3 {
            for j in 0..3 {
                m[i][j] += row[i] * row[j];
            }
            vx[i] += row[i] * s.0 as f64;
            vy[i] += row[i] * s.1 as f64;
        }
    }
    let sx = solve3(m, vx)?;
    let sy = solve3(m, vy)?;
    let fx = |v: f64| {
        let r = (v * 65536.0 + if v < 0.0 { -0.5 } else { 0.5 }) as i64;
        i32::try_from(r).ok()
    };
    let px = |v: f64| i32::try_from((v + if v < 0.0 { -0.5 } else { 0.5 }) as i64).ok();
    Some(Cal {
        a: fx(sx[0])?,
        b: fx(sx[1])?,
        c: px(sx[2])?,
        d: fx(sy[0])?,
        e: fx(sy[1])?,
        f: px(sy[2])?,
    })
}

/// The four points the calibration helper asks for: inset from the corners.
pub fn cal_targets(w: i32, h: i32) -> [(i32, i32); 4] {
    let m = 32;
    [
        (m, m),
        (w - 1 - m, m),
        (w - 1 - m, h - 1 - m),
        (m, h - 1 - m),
    ]
}

// ---- pointer events -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// The finger came down here.
    Down(i32, i32),
    /// It moved (by at least two pixels) while down.
    Move(i32, i32),
    /// It lifted.
    Up,
}

/// Turns a reading per poll (the screen position of a touch, or none) into pointer events. A touch
/// needs two readings in a row to begin and a lift three missing ones to end, so a single glitch
/// neither clicks nor drops a drag.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tracker {
    down: bool,
    pressed: u8,
    released: u8,
    last: (i32, i32),
}

impl Tracker {
    pub const fn new() -> Tracker {
        Tracker {
            down: false,
            pressed: 0,
            released: 0,
            last: (0, 0),
        }
    }
    pub fn is_down(&self) -> bool {
        self.down
    }
    pub fn position(&self) -> (i32, i32) {
        self.last
    }
    pub fn update(&mut self, sample: Option<(i32, i32)>) -> Option<Event> {
        match sample {
            Some(p) => {
                self.released = 0;
                self.pressed = self.pressed.saturating_add(1);
                if !self.down {
                    if self.pressed >= 2 {
                        self.down = true;
                        self.last = p;
                        return Some(Event::Down(p.0, p.1));
                    }
                } else if (p.0 - self.last.0).abs() >= 2 || (p.1 - self.last.1).abs() >= 2 {
                    self.last = p;
                    return Some(Event::Move(p.0, p.1));
                }
                None
            }
            None => {
                self.pressed = 0;
                self.released = self.released.saturating_add(1);
                if self.down && self.released >= 3 {
                    self.down = false;
                    return Some(Event::Up);
                }
                None
            }
        }
    }
}
