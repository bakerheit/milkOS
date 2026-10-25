//! The present transform: the desktop's logical framebuffer (`milk.fbsize=WxH`) shown through the
//! firmware's physical framebuffer (`milk.fbphys=PxQ`), turned by `milk.fbrotate=` and stretched to
//! fill the physical raster completely (docs/raspberry-pi.md, "HDMI displays").
//!
//! Why: an HDMI scaler board with a portrait glass takes only standard video modes (1024x768 here) and
//! stretches that raster over the whole glass, non-uniformly (480/1024 across, 800/768 down). The
//! Pi firmware's framebuffer does not stretch itself to the mode: given another aspect it centres the
//! framebuffer with black bars at a uniform scale ([`pillarbox`]). So the only undistorted picture
//! is a framebuffer that is exactly the raster, filled by the kernel with the inverse of the panel's
//! stretch: the desktop picture is turned, then resampled to `PxQ`, and the panel's own stretch
//! turns it back into the proportions the desktop was drawn in.
//!
//! The pipeline for a logical `lw x lh` picture and a physical `pw x ph` raster:
//!
//! 1. rotate the picture clockwise by 0, 90, 180 or 270 degrees (rotated size `rw x rh`: `lw x lh`,
//!    or `lh x lw` for a quarter turn);
//! 2. stretch the rotated picture to `pw x ph`, every physical pixel the area-weighted average of the
//!    rotated pixels under its footprint ([`axis`]: weights in 1/256, summing to exactly 256 so a flat
//!    colour stays exactly that colour);
//! 3. only the physical rectangles whose footprint touches a changed logical rectangle are converted
//!    ([`Plan::rect_to_physical`]).
//!
//! Everything here is pure (no allocation, no panics on hostile sizes): the kernel (`rpi/present.rs`)
//! holds the tables and the buffers and calls [`convert`]; the host tests run the same code on a model of
//! the panel. Also here: the option parsers, the calibration pattern `milk.fbtest=1` draws
//! ([`pattern_pixel`]) and the log line text.

use crate::lcd::Rotation;

/// The most taps one physical pixel averages over, per axis (a source/destination ratio of up to 11:1).
pub const MAX_TAPS: usize = 12;
/// The largest logical, rotated or physical dimension.
pub const MAX_DIM: u32 = 16384;

// ---- options ----------------------------------------------------------------------------------

/// What `milk.fbphys` asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhysRequest {
    /// Not given: the physical framebuffer is the logical one (exactly the behaviour before this existed,
    /// unless `milk.fbrotate` asks for a turn).
    Same,
    /// `auto`: the display mode the firmware reports as the physical size.
    Auto,
    /// This raster.
    Forced(u32, u32),
}

/// The value of `milk.fbphys`: absent, `auto` or `WxH` inside the sizes a framebuffer can have
/// ([`crate::fbsize::sane`]). `Err` says what to log; the caller then behaves as if it was absent.
pub fn parse_phys(v: Option<&[u8]>) -> Result<PhysRequest, &'static str> {
    let Some(v) = v else {
        return Ok(PhysRequest::Same);
    };
    if v.is_empty() {
        return Ok(PhysRequest::Same);
    }
    if v.eq_ignore_ascii_case(b"auto") {
        return Ok(PhysRequest::Auto);
    }
    match crate::fbsize::parse_size(v) {
        Some((w, h)) if crate::fbsize::sane(w, h) => Ok(PhysRequest::Forced(w, h)),
        Some(_) => Err("milk.fbphys is outside 640x400..4096x2160, ignored"),
        None => Err("milk.fbphys must be WxH (for example 1024x768) or auto, ignored"),
    }
}

/// The value of `milk.fbrotate`: 0, 90, 180 or 270 (clockwise, degrees). Absent is 0.
pub fn parse_rotate(v: Option<&[u8]>) -> Result<Rotation, &'static str> {
    match v {
        None | Some(b"") => Ok(Rotation::Deg0),
        Some(v) => Rotation::parse(v).ok_or("milk.fbrotate must be 0, 90, 180 or 270, using 0"),
    }
}

// ---- the plan ---------------------------------------------------------------------------------

/// Why a plan was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlanError {
    /// A dimension is 0 or above [`MAX_DIM`].
    Size,
    /// An axis is shrunk or stretched by more than [`MAX_TAPS`] allows.
    Ratio,
}

/// Which logical size goes through which rotation into which physical raster.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Plan {
    pub lw: u32,
    pub lh: u32,
    pub pw: u32,
    pub ph: u32,
    pub turn: Rotation,
}

/// A rectangle `[x0, x1) x [y0, y1)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Span {
    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

/// The weights of one physical pixel along one axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Axis {
    /// The first source pixel under the footprint.
    pub first: u32,
    /// How many source pixels (1..=[`MAX_TAPS`]).
    pub n: u32,
    /// The weight of source pixel `first + i`, in 1/256; the first `n` add up to exactly 256.
    pub w: [u16; MAX_TAPS],
}

impl Axis {
    pub const ZERO: Axis = Axis {
        first: 0,
        n: 0,
        w: [0; MAX_TAPS],
    };
}

/// The footprint of destination pixel `d` of `dst` on a source axis of `src` pixels: the source
/// pixels it overlaps and how much of its width each one fills. `d >= dst` or an empty axis gives
/// [`Axis::ZERO`]; a footprint wider than [`MAX_TAPS`] pixels keeps the first ones (a [`Plan`] never
/// has one).
pub fn axis(src: u32, dst: u32, d: u32) -> Axis {
    if src == 0 || dst == 0 || d >= dst {
        return Axis::ZERO;
    }
    let (src64, dst64, d64) = (src as u64, dst as u64, d as u64);
    let lo = d64 * src64;
    let hi = lo + src64;
    let first = lo / dst64;
    let end = hi.div_ceil(dst64).min(src64);
    let n = (end - first).min(MAX_TAPS as u64) as usize;
    let mut w = [0u16; MAX_TAPS];
    let mut rem = [0u64; MAX_TAPS];
    let mut sum = 0u64;
    for i in 0..n {
        let s = first + i as u64;
        let a = (s * dst64).max(lo);
        let b = ((s + 1) * dst64).min(hi);
        let ov = b.saturating_sub(a);
        w[i] = (ov * 256 / src64) as u16;
        rem[i] = ov * 256 % src64;
        sum += w[i] as u64;
    }
    // Hand the missing 256 - sum units to the largest remainders (the first of equals first), so the
    // weights add up to exactly 256.
    let mut missing = 256u64.saturating_sub(sum);
    while missing > 0 && n > 0 {
        let mut best = 0;
        for i in 1..n {
            if rem[i] > rem[best] {
                best = i;
            }
        }
        w[best] += 1;
        rem[best] = 0;
        missing -= 1;
    }
    Axis {
        first: first as u32,
        n: n as u32,
        w,
    }
}

/// The largest `n` an [`axis`] of this ratio can have.
fn max_taps(src: u32, dst: u32) -> u32 {
    src.div_ceil(dst) + 1
}

/// `lo * num / den` rounded down and `hi * num / den` rounded up: the range `[lo, hi)` of one axis
/// scaled by `num / den`, widened to whole pixels.
fn scale_range(lo: u32, hi: u32, num: u32, den: u32) -> (u32, u32) {
    let a = (lo as u64 * num as u64 / den as u64) as u32;
    let b = (hi as u64 * num as u64).div_ceil(den as u64) as u32;
    (a, b)
}

impl Plan {
    /// A plan, or why not.
    pub fn new(lw: u32, lh: u32, pw: u32, ph: u32, turn: Rotation) -> Result<Plan, PlanError> {
        let ok = |v: u32| (1..=MAX_DIM).contains(&v);
        if !(ok(lw) && ok(lh) && ok(pw) && ok(ph)) {
            return Err(PlanError::Size);
        }
        let p = Plan {
            lw,
            lh,
            pw,
            ph,
            turn,
        };
        let (rw, rh) = p.rotated();
        let fits = |s: u32, d: u32| max_taps(s, d) <= MAX_TAPS as u32 && max_taps(d, s) <= MAX_TAPS as u32;
        if !(fits(rw, pw) && fits(rh, ph)) {
            return Err(PlanError::Ratio);
        }
        Ok(p)
    }

    /// The size of the rotated picture.
    pub fn rotated(&self) -> (u32, u32) {
        if self.turn.landscape() {
            (self.lh, self.lw)
        } else {
            (self.lw, self.lh)
        }
    }

    /// No rotation and the same size: the physical framebuffer is the logical one and nothing needs converting.
    pub fn is_identity(&self) -> bool {
        self.turn == Rotation::Deg0 && (self.lw, self.lh) == (self.pw, self.ph)
    }

    /// The weights of physical column `x` over the rotated picture's columns.
    pub fn col(&self, x: u32) -> Axis {
        axis(self.rotated().0, self.pw, x)
    }

    /// The weights of physical row `y` over the rotated picture's rows.
    pub fn row(&self, y: u32) -> Axis {
        axis(self.rotated().1, self.ph, y)
    }

    /// Rotated pixel `(rx, ry)` as a logical pixel (inside the picture for inputs inside the rotated one).
    pub fn logical_of(&self, rx: u32, ry: u32) -> (u32, u32) {
        match self.turn {
            Rotation::Deg0 => (rx, ry),
            Rotation::Deg90 => (ry, self.lh.wrapping_sub(1).wrapping_sub(rx)),
            Rotation::Deg180 => (
                self.lw.wrapping_sub(1).wrapping_sub(rx),
                self.lh.wrapping_sub(1).wrapping_sub(ry),
            ),
            Rotation::Deg270 => (self.lw.wrapping_sub(1).wrapping_sub(ry), rx),
        }
    }

    /// Logical pixel `(lx, ly)` as a rotated pixel.
    pub fn rotated_of(&self, lx: u32, ly: u32) -> (u32, u32) {
        match self.turn {
            Rotation::Deg0 => (lx, ly),
            Rotation::Deg90 => (self.lh.wrapping_sub(1).wrapping_sub(ly), lx),
            Rotation::Deg180 => (
                self.lw.wrapping_sub(1).wrapping_sub(lx),
                self.lh.wrapping_sub(1).wrapping_sub(ly),
            ),
            Rotation::Deg270 => (ly, self.lw.wrapping_sub(1).wrapping_sub(lx)),
        }
    }

    /// The physical pixel that holds the centre of logical pixel `(lx, ly)`. `None` outside the picture.
    pub fn point_to_physical(&self, lx: u32, ly: u32) -> Option<(u32, u32)> {
        if lx >= self.lw || ly >= self.lh {
            return None;
        }
        let (rx, ry) = self.rotated_of(lx, ly);
        let (rw, rh) = self.rotated();
        let px = ((2 * rx as u64 + 1) * self.pw as u64 / (2 * rw as u64)) as u32;
        let py = ((2 * ry as u64 + 1) * self.ph as u64 / (2 * rh as u64)) as u32;
        Some((px.min(self.pw - 1), py.min(self.ph - 1)))
    }

    /// The logical pixel that holds the centre of physical pixel `(px, py)`. `None` outside the raster.
    pub fn point_to_logical(&self, px: u32, py: u32) -> Option<(u32, u32)> {
        if px >= self.pw || py >= self.ph {
            return None;
        }
        let (rw, rh) = self.rotated();
        let rx = ((2 * px as u64 + 1) * rw as u64 / (2 * self.pw as u64)) as u32;
        let ry = ((2 * py as u64 + 1) * rh as u64 / (2 * self.ph as u64)) as u32;
        Some(self.logical_of(rx.min(rw - 1), ry.min(rh - 1)))
    }

    /// The physical pixels whose footprint touches the logical rectangle `[x0, x1) x [y0, y1)` (clipped to the
    /// picture; empty if nothing is left).
    pub fn rect_to_physical(&self, r: Span) -> Span {
        let none = Span {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        };
        let (x0, y0, x1, y1) = (r.x0, r.y0, r.x1.min(self.lw), r.y1.min(self.lh));
        if x1 <= x0 || y1 <= y0 {
            return none;
        }
        // The corners of the rectangle, rotated; the rotated rectangle is their bounding box.
        let a = self.rotated_of(x0, y0);
        let b = self.rotated_of(x1 - 1, y1 - 1);
        let (rx0, rx1) = (a.0.min(b.0), a.0.max(b.0) + 1);
        let (ry0, ry1) = (a.1.min(b.1), a.1.max(b.1) + 1);
        let (rw, rh) = self.rotated();
        let (px0, px1) = scale_range(rx0, rx1, self.pw, rw);
        let (py0, py1) = scale_range(ry0, ry1, self.ph, rh);
        Span {
            x0: px0,
            y0: py0,
            x1: px1.min(self.pw),
            y1: py1.min(self.ph),
        }
    }

    /// The logical pixels that physical rectangle `[x0, x1) x [y0, y1)` is made from.
    pub fn rect_to_logical(&self, r: Span) -> Span {
        let none = Span {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        };
        let (x0, y0, x1, y1) = (r.x0, r.y0, r.x1.min(self.pw), r.y1.min(self.ph));
        if x1 <= x0 || y1 <= y0 {
            return none;
        }
        let (rw, rh) = self.rotated();
        let (rx0, rx1) = scale_range(x0, x1, rw, self.pw);
        let (ry0, ry1) = scale_range(y0, y1, rh, self.ph);
        let (rx1, ry1) = (rx1.min(rw), ry1.min(rh));
        let a = self.logical_of(rx0, ry0);
        let b = self.logical_of(rx1 - 1, ry1 - 1);
        Span {
            x0: a.0.min(b.0),
            y0: a.1.min(b.1),
            x1: a.0.max(b.0) + 1,
            y1: a.1.max(b.1) + 1,
        }
    }

    /// Offsets into the logical buffer of rotated pixel `(rx, ry)`: `base + rx * ax + ry * ay` (words,
    /// signed), for a logical buffer of stride `s`.
    fn offsets(&self, s: isize) -> (isize, isize, isize) {
        let (lw, lh) = (self.lw as isize, self.lh as isize);
        match self.turn {
            Rotation::Deg0 => (0, 1, s),
            Rotation::Deg90 => ((lh - 1) * s, -s, 1),
            Rotation::Deg180 => ((lh - 1) * s + lw - 1, -1, -s),
            Rotation::Deg270 => (lw - 1, s, -1),
        }
    }
}

// ---- the resampler ----------------------------------------------------------------------------

/// Mix one pixel into the two running sums: red+blue in two 16-bit lanes, green by itself.
#[inline(always)]
fn mix(rb: &mut u32, g: &mut u32, p: u32, w: u32) {
    *rb += (p & 0x00ff_00ff) * w;
    *g += ((p >> 8) & 0xff) * w;
}

/// The two sums (weights adding up to 256) back to a pixel, rounded.
#[inline(always)]
fn unmix(rb: u32, g: u32) -> u32 {
    (((rb + 0x0080_0080) >> 8) & 0x00ff_00ff) | (((g + 128) >> 8) << 8)
}

/// Convert the physical rectangle `r` of `dst` (stride `dst_stride` words) from the logical picture `src` (stride
/// `src_stride`), with the per-column and per-row weights `cols` (`pw` of them, from [`Plan::col`]) and `rows`
/// (`ph`, [`Plan::row`]). False, and nothing written, if the rectangle, the tables or the buffers do not fit
/// the plan. Nothing here allocates or panics.
#[allow(clippy::too_many_arguments)]
pub fn convert(
    plan: &Plan,
    cols: &[Axis],
    rows: &[Axis],
    src: &[u32],
    src_stride: usize,
    dst: &mut [u32],
    dst_stride: usize,
    r: Span,
) -> bool {
    let (pw, ph) = (plan.pw as usize, plan.ph as usize);
    if r.x1 as usize > pw || r.y1 as usize > ph || cols.len() != pw || rows.len() != ph {
        return false;
    }
    if r.is_empty() {
        return true;
    }
    let (lw, lh) = (plan.lw as usize, plan.lh as usize);
    if src_stride < lw || src.len() < (lh - 1) * src_stride + lw {
        return false;
    }
    if dst_stride < pw || dst.len() < (r.y1 as usize - 1) * dst_stride + r.x1 as usize {
        return false;
    }
    let (rw, rh) = plan.rotated();
    let (base, ax, ay) = plan.offsets(src_stride as isize);
    for py in r.y0 as usize..r.y1 as usize {
        let row = &rows[py];
        if row.n == 0 || row.first as usize + row.n as usize > rh as usize {
            return false;
        }
        let out = &mut dst[py * dst_stride..py * dst_stride + pw];
        for px in r.x0 as usize..r.x1 as usize {
            let col = &cols[px];
            if col.n == 0 || col.first as usize + col.n as usize > rw as usize {
                return false;
            }
            let (mut vrb, mut vg) = (0u32, 0u32);
            for ty in 0..row.n as usize {
                let ry = (row.first as usize + ty) as isize;
                let line = base + ry * ay;
                let (mut hrb, mut hg) = (0u32, 0u32);
                for tx in 0..col.n as usize {
                    let rx = (col.first as usize + tx) as isize;
                    // Inside the logical buffer: the tap ranges were checked against the rotated size.
                    let p = src[(line + rx * ax) as usize];
                    mix(&mut hrb, &mut hg, p, col.w[tx] as u32);
                }
                mix(&mut vrb, &mut vg, unmix(hrb, hg), row.w[ty] as u32);
            }
            out[px] = unmix(vrb, vg);
        }
    }
    true
}

// ---- the firmware and the panel ---------------------------------------------------------------

/// What the Pi firmware's legacy framebuffer does with a framebuffer of another aspect than the video
/// mode: a uniform scale to fit, centred, black bars on the free sides. Returns `(w, h, x, y)` of the
/// picture inside the raster. (Seen on a real panel: a 768x1280 framebuffer in a 1024x768 mode was a thin
/// centred strip with black bars left and right.)
pub fn pillarbox(sw: u32, sh: u32, dw: u32, dh: u32) -> (u32, u32, u32, u32) {
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return (0, 0, 0, 0);
    }
    // Compare sw/sh with dw/dh without division.
    let (w, h) = if sw as u64 * dh as u64 >= dw as u64 * sh as u64 {
        (dw, (sh as u64 * dw as u64 / sw as u64) as u32)
    } else {
        ((sw as u64 * dh as u64 / sh as u64) as u32, dh)
    };
    (w, h, (dw - w) / 2, (dh - h) / 2)
}

// ---- the log lines ----------------------------------------------------------------------------

/// `n / d` with three decimals ("2.133"), into `out`; returns the length.
pub fn ratio3(n: u32, d: u32, out: &mut [u8; 16]) -> usize {
    let milli = if d == 0 { 0 } else { (n as u64 * 1000 + d as u64 / 2) / d as u64 };
    let mut tmp = [0u8; 12];
    let mut k = 0;
    let mut m = milli;
    loop {
        tmp[k] = b'0' + (m % 10) as u8;
        k += 1;
        m /= 10;
        if m == 0 && k >= 4 {
            break;
        }
    }
    let mut n = 0;
    while k > 3 {
        k -= 1;
        out[n] = tmp[k];
        n += 1;
    }
    out[n] = b'.';
    n += 1;
    while k > 0 {
        k -= 1;
        out[n] = tmp[k];
        n += 1;
    }
    n
}

// ---- the calibration pattern (milk.fbtest=1) ---------------------------------------------------

/// Marker squares in the four corners: top left red, top right green, bottom right blue, bottom left yellow.
pub const CORNER_COLORS: [u32; 4] = [0x00e0_2020, 0x0020_c040, 0x0030_60f0, 0x00f0_d020];
/// The colour bars, left to right.
pub const BAR_COLORS: [u32; 8] = [
    0x00ff_ffff,
    0x00ff_ff00,
    0x0000_ffff,
    0x0000_ff00,
    0x00ff_00ff,
    0x00ff_0000,
    0x0000_00ff,
    0x0000_0000,
];
/// Spacing of the grid lines.
pub const GRID: u32 = 80;

/// The pixel `(x, y)` of the calibration pattern for a `w x h` screen (`0x00RRGGBB`): a white border 2 px wide, a
/// 40 px marker in each corner (red, green, blue, yellow clockwise from the top left), cross-hairs through the
/// middle, a magenta diagonal from the top left to the bottom right, eight colour bars (white, yellow, cyan, green,
/// magenta, red, blue, black) between the cross-hair and the bottom edge, a dim grid every 80 px (squares stay
/// square when the geometry is right) and a dark background. Later layers cover earlier ones in this order:
/// background, grid, bars, diagonal, cross-hairs, markers, border.
pub fn pattern_pixel(w: u32, h: u32, x: u32, y: u32) -> u32 {
    if w < 16 || h < 16 || x >= w || y >= h {
        return 0;
    }
    let (xi, yi) = (x as i64, y as i64);
    let (wi, hi) = (w as i64, h as i64);
    if x < 2 || y < 2 || x >= w - 2 || y >= h - 2 {
        return 0x00ff_ffff;
    }
    // Markers: 40x40, 6 px in from the border.
    let m = |x0: i64, y0: i64| xi >= x0 && xi < x0 + 40 && yi >= y0 && yi < y0 + 40;
    if m(6, 6) {
        return CORNER_COLORS[0];
    }
    if m(wi - 46, 6) {
        return CORNER_COLORS[1];
    }
    if m(wi - 46, hi - 46) {
        return CORNER_COLORS[2];
    }
    if m(6, hi - 46) {
        return CORNER_COLORS[3];
    }
    // Cross-hairs, 2 px wide, through the middle.
    if xi == wi / 2 - 1 || xi == wi / 2 || yi == hi / 2 - 1 || yi == hi / 2 {
        return 0x00d0_d0d0;
    }
    // The diagonal: within a pixel (vertically) of the line from (0,0) to (w,h).
    if (yi * wi - xi * hi).abs() <= wi {
        return 0x00ff_00ff;
    }
    // Colour bars: below the cross-hair, 48 px tall, from x = 16 to w - 16.
    let by0 = hi / 2 + 24;
    if yi >= by0 && yi < by0 + 48 && xi >= 16 && xi < wi - 16 {
        let k = ((xi - 16) * 8 / (wi - 32)) as usize;
        return BAR_COLORS[k.min(7)];
    }
    // The grid.
    if x % GRID == 0 || y % GRID == 0 {
        return 0x0060_6060;
    }
    0x0018_1818
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    const ROTS: [Rotation; 4] = [Rotation::Deg0, Rotation::Deg90, Rotation::Deg180, Rotation::Deg270];

    fn tables(p: &Plan) -> (Vec<Axis>, Vec<Axis>) {
        ((0..p.pw).map(|x| p.col(x)).collect(), (0..p.ph).map(|y| p.row(y)).collect())
    }

    fn render(p: &Plan, src: &[u32]) -> Vec<u32> {
        let (c, r) = tables(p);
        let mut dst = vec![0xdead_beefu32; (p.pw * p.ph) as usize];
        let full = Span { x0: 0, y0: 0, x1: p.pw, y1: p.ph };
        assert!(convert(p, &c, &r, src, p.lw as usize, &mut dst, p.pw as usize, full));
        dst
    }

    fn pattern(w: u32, h: u32) -> Vec<u32> {
        (0..h).flat_map(|y| (0..w).map(move |x| pattern_pixel(w, h, x, y))).collect()
    }

    #[test]
    fn axis_weights_add_up_and_cover_the_footprint() {
        // Every ratio in a wide table: weights sum to exactly 256, taps are contiguous and inside the source.
        for (src, dst) in [(800, 1024), (480, 1024), (800, 768), (480, 768), (1024, 480), (768, 800), (1, 1), (1, 7), (7, 1), (13, 13), (4096, 640), (640, 4096), (1000, 999), (999, 1000)] {
            for d in 0..dst {
                let a = axis(src, dst, d);
                assert!(a.n >= 1 && a.n as usize <= MAX_TAPS, "{src}->{dst} d={d}: n={}", a.n);
                let sum: u32 = a.w[..a.n as usize].iter().map(|&w| w as u32).sum();
                assert_eq!(sum, 256, "{src}->{dst} d={d}");
                assert!(a.first + a.n <= src, "{src}->{dst} d={d}");
                assert!(a.w[a.n as usize..].iter().all(|&w| w == 0));
            }
        }
    }

    #[test]
    fn axis_matches_a_float_model_and_known_cases() {
        // 2x upscale: every destination pixel is one source pixel.
        for d in 0..8 {
            let a = axis(4, 8, d);
            assert_eq!((a.first, a.n, a.w[0]), (d / 2, 1, 256));
        }
        // 2x downscale: two source pixels, half each.
        let a = axis(8, 4, 1);
        assert_eq!((a.first, a.n, a.w[0], a.w[1]), (2, 2, 128, 128));
        // 3:2: destination pixel 1 spans source [1.5, 3): half of pixel 1 and all of pixel 2 -> 1/3, 2/3 -> 85.33, 170.67.
        let a = axis(3, 2, 1);
        assert_eq!((a.first, a.n), (1, 2));
        assert_eq!(a.w[0] + a.w[1], 256);
        assert!(a.w[0] == 85 || a.w[0] == 86);
        // Float model for the panel's ratios.
        for (src, dst) in [(480u32, 1024u32), (800, 768), (1024, 480), (768, 800)] {
            for d in 0..dst {
                let a = axis(src, dst, d);
                let (lo, hi) = (d as f64 * src as f64 / dst as f64, (d + 1) as f64 * src as f64 / dst as f64);
                for i in 0..a.n {
                    let s = (a.first + i) as f64;
                    let ov = (hi.min(s + 1.0) - lo.max(s)) / (hi - lo);
                    assert!((a.w[i as usize] as f64 / 256.0 - ov).abs() <= 1.0 / 256.0 + 1e-9, "{src}->{dst} d={d} i={i}");
                }
            }
        }
        // Hostile arguments give nothing, not a panic.
        assert_eq!(axis(0, 5, 0), Axis::ZERO);
        assert_eq!(axis(5, 0, 0), Axis::ZERO);
        assert_eq!(axis(5, 5, 5), Axis::ZERO);
        assert_eq!(axis(u32::MAX, 1, 0).n as usize, MAX_TAPS);
        let _ = axis(u32::MAX, u32::MAX, u32::MAX - 1);
    }

    #[test]
    fn plans_refuse_hostile_sizes() {
        assert_eq!(Plan::new(0, 480, 1024, 768, Rotation::Deg0), Err(PlanError::Size));
        assert_eq!(Plan::new(800, 480, 0, 768, Rotation::Deg0), Err(PlanError::Size));
        assert_eq!(Plan::new(800, 480, 1024, MAX_DIM + 1, Rotation::Deg0), Err(PlanError::Size));
        assert_eq!(Plan::new(u32::MAX, 480, 1024, 768, Rotation::Deg0), Err(PlanError::Size));
        assert_eq!(Plan::new(16384, 480, 640, 768, Rotation::Deg0), Err(PlanError::Ratio));
        assert_eq!(Plan::new(800, 480, 16384, 768, Rotation::Deg90), Err(PlanError::Ratio));
        assert!(Plan::new(800, 480, 1024, 768, Rotation::Deg90).is_ok());
        assert!(Plan::new(4096, 2160, 640, 400, Rotation::Deg270).is_ok());
        assert!(Plan::new(640, 400, 4096, 2160, Rotation::Deg0).is_ok());
        assert!(Plan::new(1, 1, 1, 1, Rotation::Deg180).is_ok());
        assert!(Plan::new(1024, 768, 1024, 768, Rotation::Deg0).unwrap().is_identity());
        assert!(!Plan::new(1024, 768, 1024, 768, Rotation::Deg180).unwrap().is_identity());
        assert!(!Plan::new(800, 480, 1024, 768, Rotation::Deg0).unwrap().is_identity());
    }

    #[test]
    fn rotations_map_the_corners_clockwise() {
        // A 4x2 picture: the top left goes to the top right when turned 90 clockwise.
        let p = Plan::new(4, 2, 4, 2, Rotation::Deg0).unwrap();
        assert_eq!(p.rotated_of(0, 0), (0, 0));
        let p = Plan::new(4, 2, 2, 4, Rotation::Deg90).unwrap();
        assert_eq!(p.rotated(), (2, 4));
        assert_eq!(p.rotated_of(0, 0), (1, 0)); // top left -> top right
        assert_eq!(p.rotated_of(3, 0), (1, 3)); // top right -> bottom right
        assert_eq!(p.rotated_of(3, 1), (0, 3)); // bottom right -> bottom left
        assert_eq!(p.rotated_of(0, 1), (0, 0)); // bottom left -> top left
        let p = Plan::new(4, 2, 4, 2, Rotation::Deg180).unwrap();
        assert_eq!(p.rotated_of(0, 0), (3, 1));
        let p = Plan::new(4, 2, 2, 4, Rotation::Deg270).unwrap();
        assert_eq!(p.rotated_of(0, 0), (0, 3)); // top left -> bottom left
        assert_eq!(p.rotated_of(3, 0), (0, 0)); // top right -> top left
        // logical_of is the inverse of rotated_of for every pixel, every rotation, odd sizes too.
        for turn in ROTS {
            for (w, h) in [(5u32, 3u32), (1, 7), (800, 480), (4, 4)] {
                let (pw, ph) = if turn.landscape() { (h, w) } else { (w, h) };
                let p = Plan::new(w, h, pw, ph, turn).unwrap();
                for y in 0..h {
                    for x in 0..w {
                        let (rx, ry) = p.rotated_of(x, y);
                        let (rw, rh) = p.rotated();
                        assert!(rx < rw && ry < rh);
                        assert_eq!(p.logical_of(rx, ry), (x, y), "{turn:?} {w}x{h}");
                    }
                }
            }
        }
    }

    #[test]
    fn four_quarter_turns_are_the_identity() {
        // Composing four 90 degree plans returns every pixel to where it started.
        let (w, h) = (6u32, 4u32);
        let src: Vec<u32> = (0..w * h).collect();
        let mut cur = (src.clone(), w, h);
        for _ in 0..4 {
            let (pw, ph) = (cur.2, cur.1);
            let p = Plan::new(cur.1, cur.2, pw, ph, Rotation::Deg90).unwrap();
            cur = (render(&p, &cur.0), pw, ph);
        }
        assert_eq!((cur.1, cur.2), (w, h));
        assert_eq!(cur.0, src);
    }

    #[test]
    fn exact_fills_and_no_overflow() {
        for turn in ROTS {
            for (lw, lh, pw, ph) in [(800, 480, 1024, 768), (480, 800, 1024, 768), (640, 1067, 1024, 768), (1024, 768, 1024, 768), (1920, 1080, 1024, 768), (640, 400, 4096, 2160), (4096, 2160, 640, 400)] {
                let Ok(p) = Plan::new(lw, lh, pw, ph, turn) else { continue };
                for c in [0x0000_0000u32, 0x00ff_ffff, 0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0x0012_3456, 0x00fe_dcba, 0x0001_0101] {
                    let src = vec![c; (lw * lh) as usize];
                    let out = render(&p, &src);
                    assert!(out.iter().all(|&v| v == c), "{turn:?} {lw}x{lh} -> {pw}x{ph}: {c:06x}");
                }
            }
        }
        // The top byte of the source is ignored and the result has none.
        let p = Plan::new(8, 8, 16, 8, Rotation::Deg0).unwrap();
        let out = render(&p, &vec![0xff12_3456; 64]);
        assert!(out.iter().all(|&v| v == 0x0012_3456));
    }

    #[test]
    fn the_filter_is_an_average() {
        // A 2x1 picture of black and white stretched to 3x1: the middle pixel is the average of both.
        let p = Plan::new(2, 1, 3, 1, Rotation::Deg0).unwrap();
        let out = render(&p, &[0x0000_0000, 0x00ff_ffff]);
        assert_eq!(out[0], 0);
        assert_eq!(out[2], 0x00ff_ffff);
        let mid = out[1] & 0xff;
        assert!((126..=129).contains(&mid), "{mid}");
        assert_eq!(out[1], mid * 0x0001_0101);
        // 4x1 -> 2x1: pairs averaged.
        let p = Plan::new(4, 1, 2, 1, Rotation::Deg0).unwrap();
        let out = render(&p, &[0x0000_0000, 0x00ff_ffff, 0x00ff_0000, 0x0000_ff00]);
        assert_eq!(out[0], 0x0080_8080); // black and white
        assert_eq!(out[1], 0x0080_8000); // red and green
    }

    #[test]
    fn rect_mapping_covers_what_changes() {
        // Change one logical pixel; exactly the physical pixels of rect_to_physical (and no others) may change.
        for turn in ROTS {
            let (lw, lh) = (40u32, 24u32);
            let (pw, ph) = if turn.landscape() { (30, 52) } else { (53, 31) };
            let p = Plan::new(lw, lh, pw, ph, turn).unwrap();
            let base = vec![0x0010_2030u32; (lw * lh) as usize];
            let before = render(&p, &base);
            for (x, y) in [(0u32, 0u32), (39, 0), (0, 23), (39, 23), (17, 9), (20, 12), (1, 22)] {
                let mut src = base.clone();
                src[(y * lw + x) as usize] = 0x00f0_f0f0;
                let after = render(&p, &src);
                let r = p.rect_to_physical(Span { x0: x, y0: y, x1: x + 1, y1: y + 1 });
                assert!(!r.is_empty());
                let mut changed = 0;
                for py in 0..ph {
                    for px in 0..pw {
                        let i = (py * pw + px) as usize;
                        let inside = px >= r.x0 && px < r.x1 && py >= r.y0 && py < r.y1;
                        if before[i] != after[i] {
                            changed += 1;
                            assert!(inside, "{turn:?} pixel ({x},{y}) changed physical ({px},{py}) outside {r:?}");
                        }
                    }
                }
                assert!(changed > 0, "{turn:?} ({x},{y}) changed nothing");
                // The reverse mapping of that rectangle contains the pixel.
                let back = p.rect_to_logical(r);
                assert!(x >= back.x0 && x < back.x1 && y >= back.y0 && y < back.y1, "{turn:?} ({x},{y}) not in {back:?}");
            }
            // A partial conversion over a rect equals the full conversion there and leaves the rest alone.
            let mut src = base.clone();
            for y in 5..15 {
                for x in 8..30 {
                    src[(y * lw + x) as usize] = 0x00c0_8040 ^ (x * 7 + y * 13);
                }
            }
            let full = render(&p, &src);
            let (c, r) = tables(&p);
            let mut dst = before.clone();
            let span = p.rect_to_physical(Span { x0: 8, y0: 5, x1: 30, y1: 15 });
            assert!(convert(&p, &c, &r, &src, lw as usize, &mut dst, pw as usize, span));
            assert_eq!(dst, full, "{turn:?}: converting only the dirty rect gives the full picture");
        }
        // Empty and out of range.
        let p = Plan::new(40, 24, 53, 31, Rotation::Deg0).unwrap();
        assert!(p.rect_to_physical(Span { x0: 5, y0: 5, x1: 5, y1: 9 }).is_empty());
        assert!(p.rect_to_physical(Span { x0: 50, y0: 5, x1: 90, y1: 9 }).is_empty());
        let all = p.rect_to_physical(Span { x0: 0, y0: 0, x1: u32::MAX, y1: u32::MAX });
        assert_eq!(all, Span { x0: 0, y0: 0, x1: 53, y1: 31 });
        let back = p.rect_to_logical(Span { x0: 0, y0: 0, x1: u32::MAX, y1: u32::MAX });
        assert_eq!(back, Span { x0: 0, y0: 0, x1: 40, y1: 24 });
    }

    #[test]
    fn points_round_trip() {
        for turn in ROTS {
            for (lw, lh, pw, ph) in [(800u32, 480u32, 1024u32, 768u32), (480, 800, 1024, 768), (640, 1067, 1024, 768), (800, 480, 800, 480)] {
                let p = Plan::new(lw, lh, pw, ph, turn).unwrap();
                let (rw, rh) = p.rotated();
                // Logical -> physical -> logical: back to the same pixel where every axis is stretched up
                // (rotated size <= physical), and within the shrink ratio where it is shrunk.
                let slack_x = rw.div_ceil(pw);
                let slack_y = rh.div_ceil(ph);
                for y in (0..lh).step_by(3) {
                    for x in (0..lw).step_by(3) {
                        let (px, py) = p.point_to_physical(x, y).unwrap();
                        assert!(px < pw && py < ph);
                        let (bx, by) = p.point_to_logical(px, py).unwrap();
                        let (rx, ry) = p.rotated_of(x, y);
                        let (brx, bry) = p.rotated_of(bx, by);
                        assert!(rx.abs_diff(brx) < slack_x.max(1) && ry.abs_diff(bry) < slack_y.max(1), "{turn:?} {lw}x{lh}: ({x},{y}) -> ({px},{py}) -> ({bx},{by})");
                    }
                }
                // Physical -> logical -> physical: the pixel's footprint overlaps the logical pixel's footprint.
                for py in (0..ph).step_by(5) {
                    for px in (0..pw).step_by(5) {
                        let (lx, ly) = p.point_to_logical(px, py).unwrap();
                        let (qx, qy) = p.point_to_physical(lx, ly).unwrap();
                        let (rw, rh) = p.rotated();
                        assert!(qx.abs_diff(px) <= pw.div_ceil(rw), "{turn:?}");
                        assert!(qy.abs_diff(py) <= ph.div_ceil(rh), "{turn:?}");
                    }
                }
            }
        }
        let p = Plan::new(800, 480, 1024, 768, Rotation::Deg90).unwrap();
        assert_eq!(p.point_to_physical(800, 0), None);
        assert_eq!(p.point_to_physical(0, 480), None);
        assert_eq!(p.point_to_logical(1024, 0), None);
        assert_eq!(p.point_to_logical(0, 768), None);
    }

    #[test]
    fn the_owners_glass_shows_the_desktop_undistorted() {
        // The model of the panel: the firmware raster (1024x768) is stretched over the 480x800 glass,
        // non-uniformly; the glass is fitted so that a quarter turn clockwise (what milk.fbrotate=90 prepares) is
        // how the viewer sees the desktop. We turn the glass image back (counter-clockwise) and compare it with the logical picture.
        let (lw, lh) = (800u32, 480u32);
        let (pw, ph) = (1024u32, 768u32);
        let (gw, gh) = (480u32, 800u32);
        let logical = smooth_picture(lw, lh);
        let present = Plan::new(lw, lh, pw, ph, Rotation::Deg90).unwrap();
        let raster = render(&present, &logical);
        // The panel: stretch 1024x768 to 480x800 (the same area filter, no turn).
        let panel = Plan::new(pw, ph, gw, gh, Rotation::Deg0).unwrap();
        let glass = render(&panel, &raster);
        // Undo the glass: rotate 270 clockwise (= 90 counter-clockwise), exact permutation.
        let back = Plan::new(gw, gh, gh, gw, Rotation::Deg270).unwrap();
        let seen = render(&back, &glass);
        assert_eq!((back.pw, back.ph), (lw, lh));
        let (mut sum, mut worst) = (0u64, 0u32);
        for (a, b) in logical.iter().zip(&seen) {
            for sh in [0, 8, 16] {
                let d = ((a >> sh) & 0xff).abs_diff((b >> sh) & 0xff);
                sum += d as u64;
                worst = worst.max(d);
            }
        }
        let mean = sum as f64 / (logical.len() * 3) as f64;
        assert!(mean < 1.5, "mean error {mean}");
        assert!(worst < 12, "worst error {worst}");
        // The same desktop without the turn (800x480 stretched to the raster, then the glass): 800 logical columns
        // become 480 glass columns (x0.6) and 480 logical rows become 800 glass rows (x1.67): 2.8 times too tall.
        let squeeze = (800.0 / 480.0) / (480.0 / 800.0);
        assert!(squeeze > 2.7, "{squeeze}");
    }

    /// A picture with a smooth gradient and a few sharp-ish marks, in all three channels.
    fn smooth_picture(w: u32, h: u32) -> Vec<u32> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / (w - 1)) as u32;
                let g = (y * 255 / (h - 1)) as u32;
                let b = ((x + y) * 255 / (w + h - 2)) as u32;
                v.push((r << 16) | (g << 8) | b);
            }
        }
        v
    }

    #[test]
    fn the_pattern_survives_the_trip_where_it_must() {
        // The calibration pattern through rotate 90 + stretch: the corner markers land in the corners they should
        // (red top left of the *logical* picture ends top right of the rotated one), checked at the physical points.
        let (lw, lh, pw, ph) = (800u32, 480u32, 1024u32, 768u32);
        let p = Plan::new(lw, lh, pw, ph, Rotation::Deg90).unwrap();
        let raster = render(&p, &pattern(lw, lh));
        let at = |x: u32, y: u32| raster[(y * pw + x) as usize];
        // Logical marker centres (26, 26) red, (773, 26) green, (773, 453) blue, (26, 453) yellow.
        for (l, want) in [((26u32, 26u32), CORNER_COLORS[0]), ((773, 26), CORNER_COLORS[1]), ((773, 453), CORNER_COLORS[2]), ((26, 453), CORNER_COLORS[3])] {
            let (px, py) = p.point_to_physical(l.0, l.1).unwrap();
            assert_eq!(at(px, py), want, "marker at logical {l:?} -> physical ({px},{py})");
        }
        // Rotated 90 clockwise: red (logical top left) is top right of the raster, green bottom right, blue bottom left, yellow top left.
        assert_eq!(at(pw - 30, 30), CORNER_COLORS[0]);
        assert_eq!(at(pw - 30, ph - 30), CORNER_COLORS[1]);
        assert_eq!(at(30, ph - 30), CORNER_COLORS[2]);
        assert_eq!(at(30, 30), CORNER_COLORS[3]);
        // The border is white all the way round the raster.
        for x in [0, 1, 500, pw - 1] {
            assert_eq!(at(x, 0), 0x00ff_ffff);
            assert_eq!(at(x, ph - 1), 0x00ff_ffff);
        }
        for y in [0, 1, 300, ph - 1] {
            assert_eq!(at(0, y), 0x00ff_ffff);
            assert_eq!(at(pw - 1, y), 0x00ff_ffff);
        }
    }

    #[test]
    fn pattern_golden_points() {
        // The same points are checked by scripts/fbpresent_check.py's own copy of the pattern.
        let g = |x, y| pattern_pixel(800, 480, x, y);
        assert_eq!(g(0, 0), 0x00ff_ffff); // border
        assert_eq!(g(799, 479), 0x00ff_ffff);
        assert_eq!(g(1, 300), 0x00ff_ffff);
        assert_eq!(g(10, 10), CORNER_COLORS[0]);
        assert_eq!(g(790, 10), CORNER_COLORS[1]);
        assert_eq!(g(790, 470), CORNER_COLORS[2]);
        assert_eq!(g(10, 470), CORNER_COLORS[3]);
        assert_eq!(g(400, 100), 0x00d0_d0d0); // vertical cross-hair
        assert_eq!(g(100, 240), 0x00d0_d0d0); // horizontal cross-hair
        assert_eq!(g(150, 90), 0x00ff_00ff); // the diagonal at x = 150, y = 90
        assert_eq!(g(30, 200), 0x0018_1818); // plain background
        assert_eq!(g(160, 200), 0x0060_6060); // grid line x = 160
        assert_eq!(g(21, 280), BAR_COLORS[0]); // first colour bar
        assert_eq!(g(779, 280), BAR_COLORS[7]); // last colour bar (black)
        assert_eq!(g(300, 280), BAR_COLORS[2]);
        assert_eq!(pattern_pixel(15, 15, 3, 3), 0); // too small to draw
        assert_eq!(pattern_pixel(800, 480, 800, 0), 0);
    }

    #[test]
    fn firmware_letterboxing_matches_the_photo() {
        // milk.fbsize=768x1280 in the 1024x768 mode: a 460x768 strip centred in the raster.
        let (w, h, x, y) = pillarbox(768, 1280, 1024, 768);
        assert_eq!((w, h, x, y), (460, 768, 282, 0));
        // The panel then stretches the raster over the 480x800 glass: the strip is 216 glass pixels wide, 45% of the glass.
        let glass_w = w as u64 * 480 / 1024;
        assert_eq!(glass_w, 215);
        assert!((44..=46).contains(&(glass_w * 100 / 480)));
        // A framebuffer of the raster's own aspect fills it; a wider one gets bars top and bottom.
        assert_eq!(pillarbox(1024, 768, 1024, 768), (1024, 768, 0, 0));
        assert_eq!(pillarbox(2048, 768, 1024, 768), (1024, 384, 0, 192));
        assert_eq!(pillarbox(0, 5, 1024, 768), (0, 0, 0, 0));
        assert_eq!(pillarbox(5, 5, 0, 0), (0, 0, 0, 0));
    }

    #[test]
    fn options() {
        assert_eq!(parse_phys(None), Ok(PhysRequest::Same));
        assert_eq!(parse_phys(Some(b"")), Ok(PhysRequest::Same));
        assert_eq!(parse_phys(Some(b"auto")), Ok(PhysRequest::Auto));
        assert_eq!(parse_phys(Some(b"AUTO")), Ok(PhysRequest::Auto));
        assert_eq!(parse_phys(Some(b"1024x768")), Ok(PhysRequest::Forced(1024, 768)));
        assert!(parse_phys(Some(b"1024")).is_err());
        assert!(parse_phys(Some(b"100x100")).is_err());
        assert!(parse_phys(Some(b"9000x9000")).is_err());
        assert!(parse_phys(Some(b"-1x5")).is_err());
        assert_eq!(parse_rotate(None), Ok(Rotation::Deg0));
        assert_eq!(parse_rotate(Some(b"90")), Ok(Rotation::Deg90));
        assert_eq!(parse_rotate(Some(b"270")), Ok(Rotation::Deg270));
        assert_eq!(parse_rotate(Some(b"180")), Ok(Rotation::Deg180));
        assert!(parse_rotate(Some(b"45")).is_err());
        assert!(parse_rotate(Some(b"-90")).is_err());
        assert!(parse_rotate(Some(b"ninety")).is_err());
    }

    #[test]
    fn ratio_text() {
        let s = |n, d| {
            let mut b = [0u8; 16];
            let k = ratio3(n, d, &mut b);
            std::string::String::from_utf8(b[..k].to_vec()).unwrap()
        };
        assert_eq!(s(1024, 480), "2.133");
        assert_eq!(s(768, 800), "0.960");
        assert_eq!(s(1, 1), "1.000");
        assert_eq!(s(4096, 640), "6.400");
        assert_eq!(s(1, 1000), "0.001");
        assert_eq!(s(5, 0), "0.000");
        assert_eq!(s(100000, 3), "33333.333");
    }

    #[test]
    fn bad_buffers_are_refused_not_followed() {
        let p = Plan::new(8, 8, 8, 8, Rotation::Deg0).unwrap();
        let (c, r) = tables(&p);
        let src = vec![0u32; 64];
        let mut dst = vec![0u32; 64];
        let full = Span { x0: 0, y0: 0, x1: 8, y1: 8 };
        assert!(convert(&p, &c, &r, &src, 8, &mut dst, 8, full));
        assert!(!convert(&p, &c, &r, &src[..63], 8, &mut dst, 8, full)); // source too short
        assert!(!convert(&p, &c, &r, &src, 7, &mut dst, 8, full)); // stride below the width
        assert!(!convert(&p, &c, &r, &src, 8, &mut dst[..63], 8, full)); // destination too short
        assert!(!convert(&p, &c, &r, &src, 8, &mut dst, 8, Span { x0: 0, y0: 0, x1: 9, y1: 8 })); // rect outside
        assert!(!convert(&p, &c[..7], &r, &src, 8, &mut dst, 8, full)); // table too short
        let mut bad = c.clone();
        bad[3] = Axis { first: 7, n: 4, w: [64; MAX_TAPS] }; // taps outside the source
        assert!(!convert(&p, &bad, &r, &src, 8, &mut dst, 8, full));
        bad[3] = Axis::ZERO;
        assert!(!convert(&p, &bad, &r, &src, 8, &mut dst, 8, full));
        // An empty rectangle is fine and writes nothing.
        let mut d2 = vec![7u32; 64];
        assert!(convert(&p, &c, &r, &src, 8, &mut d2, 8, Span { x0: 3, y0: 3, x1: 3, y1: 3 }));
        assert!(d2.iter().all(|&v| v == 7));
    }
}
