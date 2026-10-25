//! The Pixel 2 (Qualcomm MSM8998) pieces that are pure (docs/pixel2.md): the options on the
//! kernel command line, the pixel formats of the bootloader's framebuffer and how a row of the
//! kernel's XRGB picture is packed into them, the colour test pattern of the first image, the
//! RAM that is left once the tree's reserved regions are taken out, and what a GICv3's
//! identification registers say.
//!
//! Nothing here touches hardware: the kernel's `msm` module reads the registers and the host
//! tests feed these functions made-up values.

use crate::dtb::MemRange;

// ---- options -----------------------------------------------------------------------------

/// The value of option `msm.<name>` on the kernel command line.
pub fn option<'a>(cmdline: &'a [u8], name: &str) -> Option<&'a [u8]> {
    crate::cmdline::value_prefixed(cmdline, "msm.", name)
}

/// Parse a number: decimal, or hex with a `0x` prefix.
pub fn parse_num(v: &[u8]) -> Option<u64> {
    let s = core::str::from_utf8(v).ok()?;
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// How long the colour pattern stays up before the text console takes over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hold {
    Seconds(u32),
    /// `msm.hold=inf`: the pattern stays (no console at all), for looking at the colours in peace.
    Forever,
}

/// `msm.crashtest=`: stop on purpose, to look at the crash screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CrashTest {
    /// A panic with the console on screen.
    Panic,
    /// A data abort (a store to an unmapped address) with the console on screen.
    Abort,
    /// A panic after the colour pattern, before the console exists: the crash screen builds one.
    Early,
}

/// The `msm.<name>=` options of the kernel command line (docs/pixel2.md). Anything wrong is
/// reported in `warnings` and the default is used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Opts {
    pub fmt: Fmt,
    /// Bytes per row, if the default (width * bytes per pixel) is not right.
    pub stride: Option<usize>,
    pub width: usize,
    pub height: usize,
    /// Physical base and size of the framebuffer, instead of what the tree says.
    pub fb: Option<(usize, usize)>,
    pub hold: Hold,
    /// Magnification of the console font (1..=4).
    pub scale: usize,
    /// Kick the display controller's CTL `n` after drawing (a command-mode panel needs a refresh
    /// to show RAM writes). Off unless asked.
    pub kick: Option<usize>,
    /// Ask PSCI for its version (one `smc`). Off unless asked.
    pub psci: bool,
    /// Log a few display controller registers (read only), to learn how the panel is driven.
    pub mdp_dump: bool,
    /// Pet the SoC's watchdog once a second (one register write), so the phone is not reset while
    /// nobody looks after it. Off unless asked.
    pub wdt_pet: bool,
    pub crashtest: Option<CrashTest>,
}

pub const MAX_WARNINGS: usize = 8;

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            fmt: Fmt::Bgrx,
            stride: None,
            width: 1080,
            height: 1920,
            fb: None,
            hold: Hold::Seconds(5),
            scale: 2,
            kick: None,
            psci: false,
            mdp_dump: false,
            wdt_pet: false,
            crashtest: None,
        }
    }
}

impl Opts {
    /// Parse the options out of `cmdline`. Returns them and the number of warnings, which are in
    /// the array (what was wrong, as short text).
    pub fn parse(cmdline: &[u8]) -> (Opts, [&'static str; MAX_WARNINGS], usize) {
        let mut o = Opts::default();
        let mut w = [""; MAX_WARNINGS];
        let mut n = 0;
        let mut warn = |msg: &'static str| {
            if n < MAX_WARNINGS {
                w[n] = msg;
                n += 1;
            }
        };
        if let Some(v) = option(cmdline, "fmt") {
            match Fmt::parse(v) {
                Some(f) => o.fmt = f,
                None => warn("msm.fmt: not one of bgrx rgbx xrgb xbgr rgb888 bgr888 rgb565"),
            }
        }
        if let Some(v) = option(cmdline, "stride") {
            match parse_num(v) {
                Some(s) if (1..=1 << 20).contains(&s) => o.stride = Some(s as usize),
                _ => warn("msm.stride: want a number of bytes"),
            }
        }
        if let Some(v) = option(cmdline, "size") {
            let parsed = v.iter().position(|&c| c == b'x').and_then(|x| {
                Some((parse_num(&v[..x])?, parse_num(&v[x + 1..])?))
            });
            match parsed {
                Some((w_, h)) if (16..=8192).contains(&w_) && (16..=8192).contains(&h) => {
                    o.width = w_ as usize;
                    o.height = h as usize;
                }
                _ => warn("msm.size: want WIDTHxHEIGHT, 16 to 8192"),
            }
        }
        if let Some(v) = option(cmdline, "fb") {
            let parsed = match v.iter().position(|&c| c == b':') {
                Some(c) => parse_num(&v[..c]).zip(parse_num(&v[c + 1..])),
                None => parse_num(v).map(|b| (b, 0)),
            };
            match parsed {
                Some((b, sz)) if b % 4096 == 0 && b != 0 => o.fb = Some((b as usize, sz as usize)),
                _ => warn("msm.fb: want a page aligned address, or ADDRESS:SIZE"),
            }
        }
        if let Some(v) = option(cmdline, "hold") {
            if v == b"inf" {
                o.hold = Hold::Forever;
            } else {
                match parse_num(v) {
                    Some(s) if s <= 3600 => o.hold = Hold::Seconds(s as u32),
                    _ => warn("msm.hold: want seconds (0 to 3600) or inf"),
                }
            }
        }
        if let Some(v) = option(cmdline, "scale") {
            match parse_num(v) {
                Some(s) if (1..=4).contains(&s) => o.scale = s as usize,
                _ => warn("msm.scale: want 1 to 4"),
            }
        }
        if let Some(v) = option(cmdline, "kick") {
            match v.strip_prefix(b"ctl").and_then(parse_num) {
                Some(i) if i < 8 => o.kick = Some(i as usize),
                _ => warn("msm.kick: want ctl0 to ctl7"),
            }
        }
        if let Some(v) = option(cmdline, "psci") {
            match v {
                b"1" | b"on" | b"version" => o.psci = true,
                b"0" | b"off" => {}
                _ => warn("msm.psci: want 1 or 0"),
            }
        }
        if let Some(v) = option(cmdline, "mdp") {
            match v {
                b"dump" => o.mdp_dump = true,
                b"off" => {}
                _ => warn("msm.mdp: want dump"),
            }
        }
        if let Some(v) = option(cmdline, "wdt") {
            match v {
                b"pet" => o.wdt_pet = true,
                b"off" => {}
                _ => warn("msm.wdt: want pet"),
            }
        }
        if let Some(v) = option(cmdline, "crashtest") {
            match v {
                b"panic" => o.crashtest = Some(CrashTest::Panic),
                b"abort" => o.crashtest = Some(CrashTest::Abort),
                b"early" => o.crashtest = Some(CrashTest::Early),
                _ => warn("msm.crashtest: want panic, abort or early"),
            }
        }
        (o, w, n)
    }
}

// ---- pixel formats -----------------------------------------------------------------------

/// How a pixel sits in the framebuffer's memory. The 32-bit names list the bytes in memory
/// order, lowest address first; `x` is the byte nobody is supposed to look at (written 0xff,
/// so a controller that does read it as alpha shows an opaque pixel).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    /// B, G, R, x: the little-endian word `0xxxRRGGBB` (DRM XRGB8888 / ARGB8888), the kernel's own.
    Bgrx,
    /// R, G, B, x: the word `0xxxBBGGRR` (DRM XBGR8888 / ABGR8888).
    Rgbx,
    /// x, R, G, B (DRM BGRX8888).
    Xrgb,
    /// x, B, G, R (DRM RGBX8888).
    Xbgr,
    /// R, G, B: 3 bytes (DRM BGR888).
    Rgb888,
    /// B, G, R: 3 bytes (DRM RGB888).
    Bgr888,
    /// 16 bits, little-endian, red in the top five bits.
    Rgb565,
}

impl Fmt {
    pub const ALL: [Fmt; 7] = [
        Fmt::Bgrx,
        Fmt::Rgbx,
        Fmt::Xrgb,
        Fmt::Xbgr,
        Fmt::Rgb888,
        Fmt::Bgr888,
        Fmt::Rgb565,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Fmt::Bgrx => "bgrx",
            Fmt::Rgbx => "rgbx",
            Fmt::Xrgb => "xrgb",
            Fmt::Xbgr => "xbgr",
            Fmt::Rgb888 => "rgb888",
            Fmt::Bgr888 => "bgr888",
            Fmt::Rgb565 => "rgb565",
        }
    }

    pub fn parse(v: &[u8]) -> Option<Fmt> {
        Fmt::ALL.into_iter().find(|f| f.name().as_bytes() == v)
    }

    pub fn bytes(self) -> usize {
        match self {
            Fmt::Bgrx | Fmt::Rgbx | Fmt::Xrgb | Fmt::Xbgr => 4,
            Fmt::Rgb888 | Fmt::Bgr888 => 3,
            Fmt::Rgb565 => 2,
        }
    }

    /// Write the pixel `0x00RRGGBB` (the kernel's XRGB) at the start of `dst`, which holds at
    /// least [`bytes`](Self::bytes) bytes.
    #[inline]
    pub fn put(self, dst: &mut [u8], xrgb: u32) {
        let (r, g, b) = ((xrgb >> 16) as u8, (xrgb >> 8) as u8, xrgb as u8);
        match self {
            Fmt::Bgrx => dst[..4].copy_from_slice(&[b, g, r, 0xff]),
            Fmt::Rgbx => dst[..4].copy_from_slice(&[r, g, b, 0xff]),
            Fmt::Xrgb => dst[..4].copy_from_slice(&[0xff, r, g, b]),
            Fmt::Xbgr => dst[..4].copy_from_slice(&[0xff, b, g, r]),
            Fmt::Rgb888 => dst[..3].copy_from_slice(&[r, g, b]),
            Fmt::Bgr888 => dst[..3].copy_from_slice(&[b, g, r]),
            Fmt::Rgb565 => {
                let v = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
                dst[..2].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

/// The geometry of the framebuffer the bootloader left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Geometry {
    pub width: usize,
    pub height: usize,
    /// Bytes from the start of one row to the start of the next.
    pub stride: usize,
    pub fmt: Fmt,
}

impl Geometry {
    /// The assumed layout: `width` x `height` pixels in `fmt`, rows packed (stride = width * bytes
    /// per pixel) unless `stride` says otherwise.
    pub fn new(width: usize, height: usize, fmt: Fmt, stride: Option<usize>) -> Geometry {
        Geometry {
            width,
            height,
            stride: stride.unwrap_or(width * fmt.bytes()),
            fmt,
        }
    }

    /// Bytes the picture occupies (the last row needs only `width` pixels, not a full stride).
    pub fn bytes_needed(&self) -> usize {
        if self.height == 0 {
            return 0;
        }
        (self.height - 1) * self.stride + self.width * self.fmt.bytes()
    }

    /// Is the geometry sane inside `avail` bytes: non-zero, rows at least a row of pixels long,
    /// everything inside the region?
    pub fn fits(&self, avail: usize) -> bool {
        self.width > 0
            && self.height > 0
            && self.stride >= self.width * self.fmt.bytes()
            && self.bytes_needed() <= avail
    }
}

/// Copy rows `lo..hi` of a `width`-wide XRGB `src` (rows `src_stride` pixels apart) into `dst`
/// (the framebuffer region) in the geometry's format. Rows beyond the picture or the buffer are
/// ignored. Returns the byte range of `dst` that was written (for the cache clean).
pub fn pack_rows(g: &Geometry, dst: &mut [u8], src: &[u32], src_stride: usize, lo: usize, hi: usize) -> (usize, usize) {
    let hi = hi.min(g.height);
    let bpp = g.fmt.bytes();
    let mut first = usize::MAX;
    let mut last = 0;
    for y in lo..hi {
        let row = &src[y * src_stride..y * src_stride + g.width];
        let base = y * g.stride;
        if base + g.width * bpp > dst.len() {
            break;
        }
        for (x, &px) in row.iter().enumerate() {
            g.fmt.put(&mut dst[base + x * bpp..], px);
        }
        first = first.min(base);
        last = base + g.width * bpp;
    }
    if first == usize::MAX { (0, 0) } else { (first, last) }
}

// ---- the colour test pattern -----------------------------------------------------------

pub const RED: u32 = 0x00ff_0000;
pub const GREEN: u32 = 0x0000_ff00;
pub const BLUE: u32 = 0x0000_00ff;
pub const WHITE: u32 = 0x00ff_ffff;
pub const BLACK: u32 = 0x0000_0000;
pub const GRAY: u32 = 0x0060_6060;

/// Width of the black border around each block, and of the margin between blocks and the edge.
pub const BORDER: usize = 12;
pub const MARGIN: usize = 24;

/// One of the four blocks: where it is and what it is called.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Block {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub color: u32,
    pub name: &'static str,
}

/// The four blocks of a `w` x `h` screen: red top left, green top right, blue bottom left,
/// white bottom right, in a 2 x 2 grid with a margin around and between them.
pub fn blocks(w: usize, h: usize) -> [Block; 4] {
    let bw = w.saturating_sub(3 * MARGIN) / 2;
    let bh = h.saturating_sub(3 * MARGIN) / 2;
    let x1 = MARGIN;
    let x2 = MARGIN * 2 + bw;
    let y1 = MARGIN;
    let y2 = MARGIN * 2 + bh;
    let mk = |x, y, color, name| Block { x, y, w: bw, h: bh, color, name };
    [
        mk(x1, y1, RED, "RED"),
        mk(x2, y1, GREEN, "GREEN"),
        mk(x1, y2, BLUE, "BLUE"),
        mk(x2, y2, WHITE, "WHITE"),
    ]
}

/// Is (x, y) within 1.5 pixels of the line from the top left corner to the bottom right one?
fn near_diagonal(x: usize, y: usize, w: usize, h: usize) -> bool {
    let (dx, dy) = (w as i64 - 1, h as i64 - 1);
    let cross = x as i64 * dy - y as i64 * dx;
    // distance = |cross| / sqrt(dx^2 + dy^2) <= 1.5, squared and times four.
    cross * cross * 4 <= 9 * (dx * dx + dy * dy)
}

/// The colour (`0x00RRGGBB`) of pixel (x, y) of the pattern for a `w` x `h` screen: gray
/// background; the four blocks, each with a black border; a 3-pixel black diagonal from the top
/// left corner to the bottom right and a 3-pixel white one the other way (a stride that is wrong
/// shears them); a 1-pixel white frame on the very edge (cropping shows as a missing side).
pub fn pattern_pixel(x: usize, y: usize, w: usize, h: usize) -> u32 {
    if x == 0 || y == 0 || x + 1 == w || y + 1 == h {
        return WHITE;
    }
    if near_diagonal(x, y, w, h) {
        return BLACK;
    }
    if near_diagonal(w - 1 - x, y, w, h) {
        return WHITE;
    }
    for b in blocks(w, h) {
        if x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h {
            let inner = x >= b.x + BORDER
                && x < b.x + b.w - BORDER
                && y >= b.y + BORDER
                && y < b.y + b.h - BORDER;
            return if inner { b.color } else { BLACK };
        }
    }
    GRAY
}

/// Draw the pattern straight into the framebuffer region `dst`, no allocation (the first thing
/// the image does). Returns the byte range written.
pub fn fill_pattern(g: &Geometry, dst: &mut [u8]) -> (usize, usize) {
    let bpp = g.fmt.bytes();
    let mut last = 0;
    for y in 0..g.height {
        let base = y * g.stride;
        if base + g.width * bpp > dst.len() {
            break;
        }
        for x in 0..g.width {
            g.fmt.put(&mut dst[base + x * bpp..], pattern_pixel(x, y, g.width, g.height));
        }
        last = base + g.width * bpp;
    }
    (0, last)
}

// ---- RAM -----------------------------------------------------------------------------------

/// Take the regions in `reserved` out of the RAM `banks`: whole pages only (a reserved region is
/// rounded out to pages, a bank is rounded in), banks that end up empty are dropped. Returns
/// how many banks were stored in `out` (the rest of the input is dropped when `out` is full).
pub fn carve(banks: &[MemRange], reserved: &[MemRange], page: usize, out: &mut [MemRange]) -> usize {
    let mut n = 0;
    for b in banks {
        let Some(mut lo) = b.base.checked_next_multiple_of(page) else {
            continue;
        };
        let Some(end) = b.base.checked_add(b.size) else {
            continue;
        };
        let hi = end / page * page;
        // Reserved regions inside [lo, hi), by start address.
        let mut cuts = [(0usize, 0usize); 64];
        let mut k = 0;
        for r in reserved {
            let (Some(rs), Some(re)) = (
                Some(r.base / page * page),
                r.base.checked_add(r.size).and_then(|e| e.checked_next_multiple_of(page)),
            ) else {
                continue;
            };
            if re > lo && rs < hi && re > rs && k < cuts.len() {
                cuts[k] = (rs, re);
                k += 1;
            }
        }
        cuts[..k].sort_unstable();
        for &(rs, re) in &cuts[..k] {
            if rs > lo && n < out.len() {
                out[n] = MemRange { base: lo, size: rs - lo };
                n += 1;
            }
            lo = lo.max(re);
        }
        if hi > lo && n < out.len() {
            out[n] = MemRange { base: lo, size: hi - lo };
            n += 1;
        }
    }
    n
}

// ---- GICv3 ----------------------------------------------------------------------------------

/// GICD_PIDR2.ArchRev: 3 = GICv3, 4 = GICv4 (1, 2: GICv1/v2 have the same field).
pub fn gic_arch_rev(pidr2: u32) -> u32 {
    (pidr2 >> 4) & 0xf
}

/// What GICD_TYPER says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GicdTyper {
    /// Interrupt lines (32 * (ITLinesNumber + 1)).
    pub lines: u32,
    /// CPUs the GIC can route to without affinity routing (GICv2 compatibility; 1 to 8).
    pub cpus: u32,
    /// Interrupt identifier bits.
    pub id_bits: u32,
    pub security_extn: bool,
    pub lpis: bool,
    /// Affinity 3 is implemented (GICv3 "A3V").
    pub a3v: bool,
}

pub fn decode_gicd_typer(t: u32) -> GicdTyper {
    GicdTyper {
        lines: ((t & 0x1f) + 1) * 32,
        cpus: ((t >> 5) & 7) + 1,
        id_bits: ((t >> 19) & 0x1f) + 1,
        security_extn: t & (1 << 10) != 0,
        lpis: t & (1 << 17) != 0,
        a3v: t & (1 << 24) != 0,
    }
}

/// One redistributor frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Redist {
    /// Offset of its first register page from the start of the region.
    pub offset: usize,
    /// Aff3.Aff2.Aff1.Aff0, one byte each: what MPIDR says for the CPU it belongs to.
    pub affinity: u32,
    pub processor: u16,
    pub last: bool,
    /// The frame is four pages (GICv4: it has the virtual-LPI pages), not two.
    pub vlpis: bool,
}

/// Walk the redistributor frames of a region of `size` bytes. `typer(offset)` returns the 64-bit
/// GICR_TYPER of the frame at `offset` (at +0x8 of its first page). Stops at the frame marked
/// last, when the region ends, or after `out.len()` frames. Returns how many were stored.
pub fn redistributors(size: usize, mut typer: impl FnMut(usize) -> u64, out: &mut [Redist]) -> usize {
    let mut off = 0;
    let mut n = 0;
    while n < out.len() {
        let t = typer(off);
        let vlpis = t & 2 != 0;
        let stride = if vlpis { 0x4_0000 } else { 0x2_0000 };
        if off + stride > size {
            break;
        }
        out[n] = Redist {
            offset: off,
            affinity: (t >> 32) as u32,
            processor: (t >> 8) as u16,
            last: t & 0x10 != 0,
            vlpis,
        };
        n += 1;
        if t & 0x10 != 0 {
            break;
        }
        off += stride;
    }
    n
}

/// MPIDR_EL1 affinity (Aff3 in bits 39:32, Aff2..0 in 23:0) as the GICR_TYPER affinity word.
pub fn mpidr_affinity(mpidr: u64) -> u32 {
    (((mpidr >> 32) & 0xff) << 24) as u32 | (mpidr & 0xff_ffff) as u32
}

// ---- the watchdog ----------------------------------------------------------------------

/// The registers of the Snapdragon's non-secure watchdog (`qcom,msm-watchdog`), from its offsets in
/// Linux's driver; times are in ticks of its 32.768 kHz clock.
pub mod wdt {
    pub const RST: usize = 0x04;
    pub const EN: usize = 0x08;
    pub const STS: usize = 0x0c;
    pub const BARK_TIME: usize = 0x10;
    pub const BITE_TIME: usize = 0x14;
    pub const CLOCK_HZ: u32 = 32_768;

    /// Milliseconds a tick count is.
    pub fn ms(ticks: u32) -> u64 {
        ticks as u64 * 1000 / CLOCK_HZ as u64
    }
}

// ---- tests ---------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    #[test]
    fn options_are_found_with_their_prefix() {
        let cl = b"androidboot.hardware=walleye msm.fmt=rgbx msm.hold=0x10 milk.fmt=no msm.size=\"1080x1920\"";
        assert_eq!(option(cl, "fmt"), Some(&b"rgbx"[..]));
        assert_eq!(option(cl, "hold"), Some(&b"0x10"[..]));
        assert_eq!(option(cl, "size"), Some(&b"1080x1920"[..]));
        assert_eq!(option(cl, "stride"), None);
        assert_eq!(parse_num(b"0x10"), Some(16));
        assert_eq!(parse_num(b"4320"), Some(4320));
        assert_eq!(parse_num(b"0x"), None);
        assert_eq!(parse_num(b"x"), None);
        // `milk.` options are untouched.
        assert_eq!(crate::cmdline::value(cl, "fmt"), Some(&b"no"[..]));
    }


    #[test]
    fn options_have_defaults_and_report_what_is_wrong() {
        let (o, _, n) = Opts::parse(b"androidboot.hardware=walleye");
        assert_eq!(n, 0);
        assert_eq!(o, Opts::default());
        assert_eq!((o.width, o.height, o.fmt, o.scale), (1080, 1920, Fmt::Bgrx, 2));
        assert_eq!(o.hold, Hold::Seconds(5));
        let (o, _, n) = Opts::parse(
            b"msm.fmt=rgbx msm.stride=4352 msm.size=720x1280 msm.fb=0x9d600000:0x1000000 msm.hold=inf msm.scale=3 msm.kick=ctl1 msm.psci=1",
        );
        assert_eq!(n, 0);
        assert_eq!(o.fmt, Fmt::Rgbx);
        assert_eq!(o.stride, Some(4352));
        assert_eq!((o.width, o.height), (720, 1280));
        assert_eq!(o.fb, Some((0x9d60_0000, 0x100_0000)));
        assert_eq!(o.hold, Hold::Forever);
        assert_eq!((o.scale, o.kick, o.psci), (3, Some(1), true));
        assert!(Opts::parse(b"msm.mdp=dump").0.mdp_dump);
        assert!(!Opts::parse(b"msm.mdp=off").0.mdp_dump);
        assert_eq!(Opts::parse(b"msm.mdp=2").2, 1);
        assert_eq!(Opts::parse(b"msm.crashtest=abort").0.crashtest, Some(CrashTest::Abort));
        assert_eq!(Opts::parse(b"msm.crashtest=early").0.crashtest, Some(CrashTest::Early));
        assert_eq!(Opts::parse(b"msm.crashtest=panic").0.crashtest, Some(CrashTest::Panic));
        assert_eq!(Opts::parse(b"msm.crashtest=boom").2, 1);
        let (o, _, _) = Opts::parse(b"msm.fb=0x9d600000 msm.hold=0");
        assert_eq!(o.fb, Some((0x9d60_0000, 0)));
        assert_eq!(o.hold, Hold::Seconds(0));
        // Bad values: a warning each, the default stays.
        let (o, w, n) = Opts::parse(b"msm.fmt=argb msm.stride=0 msm.size=big msm.fb=0x123 msm.hold=-1 msm.scale=9 msm.kick=ctl9 msm.psci=maybe msm.fmt=bgrx");
        assert_eq!(n, 8);
        assert_eq!(o, Opts::default());
        assert!(w[0].starts_with("msm.fmt") && w[7].starts_with("msm.psci"));
        // More warnings than room are dropped quietly.
        let (_, _, n) = Opts::parse(b"msm.fmt=a msm.stride=a msm.size=a msm.fb=a msm.hold=a msm.scale=a msm.kick=a msm.psci=a");
        assert_eq!(n, MAX_WARNINGS);
    }

    #[test]
    fn watchdog_times_are_ticks_of_the_slow_clock() {
        assert_eq!(wdt::ms(32_768), 1000);
        assert_eq!(wdt::ms(0), 0);
        // Linux's default bark and bite on this SoC: 11 s and 14 s.
        assert_eq!(wdt::ms(360_448), 11_000);
        assert_eq!(wdt::ms(u32::MAX), 131_071_999);
        assert!(Opts::parse(b"msm.wdt=pet").0.wdt_pet);
        assert!(!Opts::parse(b"msm.wdt=off").0.wdt_pet);
        assert_eq!(Opts::parse(b"msm.wdt=1").2, 1);
    }

    #[test]
    fn every_format_packs_the_primaries() {
        let cases: [(Fmt, [u8; 4]); 4] = [
            (Fmt::Bgrx, [0x33, 0x22, 0x11, 0xff]),
            (Fmt::Rgbx, [0x11, 0x22, 0x33, 0xff]),
            (Fmt::Xrgb, [0xff, 0x11, 0x22, 0x33]),
            (Fmt::Xbgr, [0xff, 0x33, 0x22, 0x11]),
        ];
        for (f, want) in cases {
            let mut d = [0u8; 4];
            f.put(&mut d, 0x0011_2233);
            assert_eq!(d, want, "{f:?}");
            assert_eq!(f.bytes(), 4);
        }
        let mut d = [0u8; 4];
        Fmt::Rgb888.put(&mut d, 0x0011_2233);
        assert_eq!(&d[..3], &[0x11, 0x22, 0x33]);
        Fmt::Bgr888.put(&mut d, 0x0011_2233);
        assert_eq!(&d[..3], &[0x33, 0x22, 0x11]);
        Fmt::Rgb565.put(&mut d, 0x00ff_0000);
        assert_eq!(u16::from_le_bytes([d[0], d[1]]), 0xf800);
        Fmt::Rgb565.put(&mut d, 0x0000_ff00);
        assert_eq!(u16::from_le_bytes([d[0], d[1]]), 0x07e0);
        Fmt::Rgb565.put(&mut d, 0x0000_00ff);
        assert_eq!(u16::from_le_bytes([d[0], d[1]]), 0x001f);
        assert_eq!(Fmt::parse(b"rgbx"), Some(Fmt::Rgbx));
        assert_eq!(Fmt::parse(b"RGBX"), None);
        for f in Fmt::ALL {
            assert_eq!(Fmt::parse(f.name().as_bytes()), Some(f));
        }
    }

    #[test]
    fn geometry_defaults_and_limits() {
        let g = Geometry::new(1080, 1920, Fmt::Bgrx, None);
        assert_eq!(g.stride, 4320);
        assert_eq!(g.bytes_needed(), 1919 * 4320 + 4320);
        // The Pixel 2's splash region is 36 MiB: room to spare.
        assert!(g.fits(0x240_0000));
        assert!(!g.fits(8_000_000));
        // A stride shorter than a row is nonsense.
        assert!(!Geometry::new(1080, 1920, Fmt::Bgrx, Some(4000)).fits(usize::MAX));
        // A padded stride is allowed, and the last row needs no padding.
        let p = Geometry::new(10, 3, Fmt::Rgb888, Some(32));
        assert_eq!(p.bytes_needed(), 2 * 32 + 30);
        assert!(!Geometry::new(0, 5, Fmt::Bgrx, None).fits(1000));
        assert_eq!(Geometry::new(4, 0, Fmt::Bgrx, None).bytes_needed(), 0);
    }

    #[test]
    fn rows_are_packed_with_the_stride_and_nothing_else_is_touched() {
        let g = Geometry::new(3, 4, Fmt::Rgbx, Some(16));
        let src: Vec<u32> = (0..12).map(|i| 0x0010_0000 + i).collect();
        let mut dst = vec![0xeeu8; 4 * 16];
        let (a, b) = pack_rows(&g, &mut dst, &src, 3, 1, 3);
        assert_eq!((a, b), (16, 2 * 16 + 12));
        // Row 0 and row 3 untouched, padding bytes untouched.
        assert!(dst[..16].iter().all(|&v| v == 0xee));
        assert!(dst[48..].iter().all(|&v| v == 0xee));
        assert_eq!(&dst[16..20], &[0x10, 0x00, 3, 0xff]);
        assert!(dst[28..32].iter().all(|&v| v == 0xee));
        assert_eq!(&dst[32..36], &[0x10, 0x00, 6, 0xff]);
        // Rows past the picture are ignored.
        assert_eq!(pack_rows(&g, &mut dst, &src, 3, 4, 9), (0, 0));
        // A buffer too short for a row stops there.
        let mut small = vec![0u8; 20];
        assert_eq!(pack_rows(&g, &mut small, &src, 3, 0, 4), (0, 12));
    }

    #[test]
    fn the_pattern_has_what_a_person_needs_to_see() {
        let (w, h) = (1080, 1920);
        let b = blocks(w, h);
        // Four blocks that do not overlap and fit with their margins.
        for (i, a) in b.iter().enumerate() {
            assert!(a.x + a.w + MARGIN <= w && a.y + a.h + MARGIN <= h, "{}", a.name);
            for c in &b[i + 1..] {
                let apart = a.x + a.w <= c.x || c.x + c.w <= a.x || a.y + a.h <= c.y || c.y + c.h <= a.y;
                assert!(apart, "{} and {} overlap", a.name, c.name);
            }
        }
        assert_eq!(b.map(|x| x.name), ["RED", "GREEN", "BLUE", "WHITE"]);
        // The centre of each block is its colour (away from the diagonals), its edge is black.
        for blk in b {
            let (cx, cy) = (blk.x + BORDER + 20, blk.y + blk.h / 2);
            let got = pattern_pixel(cx, cy, w, h);
            // Not on a diagonal at these points? pick a point that is not.
            if got == BLACK || got == WHITE && blk.color != WHITE {
                continue;
            }
            assert_eq!(got, blk.color, "{}", blk.name);
            assert_eq!(pattern_pixel(blk.x + 2, blk.y + blk.h / 2 + 7, w, h), BLACK, "{} border", blk.name);
        }
        // The frame on every edge, white.
        assert_eq!(pattern_pixel(0, 500, w, h), WHITE);
        assert_eq!(pattern_pixel(w - 1, 500, w, h), WHITE);
        assert_eq!(pattern_pixel(500, 0, w, h), WHITE);
        assert_eq!(pattern_pixel(500, h - 1, w, h), WHITE);
        // The gap between blocks is gray, the diagonals cross the middle.
        assert_eq!(pattern_pixel(w / 2 + 300, MARGIN / 2, w, h), GRAY);
        assert_eq!(pattern_pixel(w / 2, h / 2, w, h), BLACK);
        // Both diagonals reach the corners' neighbourhood: black from the top left, white from the top right.
        assert_eq!(pattern_pixel(3, 4, w, h), BLACK);
        assert_eq!(pattern_pixel(w - 4, 5, w, h), WHITE);
        // A diagonal is thin: far from it the background is not black.
        assert_ne!(pattern_pixel(w / 4, h * 3 / 4 + 40, w, h), BLACK);
    }

    #[test]
    fn the_diagonal_is_one_connected_line() {
        // Every row has exactly one run of black pixels per diagonal, a few pixels wide: a wrong
        // stride would bend it, so this is what the person looks for.
        let (w, h) = (108, 192);
        for y in 2..h - 2 {
            let xs: Vec<usize> = (1..w - 1)
                .filter(|&x| {
                    // Only count the main diagonal's black, not the border of a block.
                    let blk = blocks(w, h).iter().any(|b| x >= b.x && x < b.x + b.w && y >= b.y && y < b.y + b.h);
                    !blk && pattern_pixel(x, y, w, h) == BLACK
                })
                .collect();
            if xs.is_empty() {
                continue;
            }
            assert!(xs.last().unwrap() - xs.first().unwrap() < 8, "row {y}: {xs:?}");
        }
        // And it moves right as it goes down.
        let first = |y| (1..w - 1).find(|&x| pattern_pixel(x, y, w, h) == BLACK).unwrap();
        assert!(first(80) < first(150));
    }

    #[test]
    fn fill_pattern_writes_every_pixel_in_the_chosen_format() {
        let (w, h) = (64, 48);
        for f in [Fmt::Bgrx, Fmt::Rgbx, Fmt::Rgb888, Fmt::Rgb565] {
            let g = Geometry::new(w, h, f, Some(w * f.bytes() + 8));
            let mut buf = vec![0xaau8; g.bytes_needed()];
            let (lo, hi) = fill_pattern(&g, &mut buf);
            assert_eq!((lo, hi), (0, g.bytes_needed()));
            // The top-left corner is the white frame.
            let mut want = [0u8; 4];
            f.put(&mut want, WHITE);
            assert_eq!(&buf[..f.bytes()], &want[..f.bytes()], "{f:?}");
            // The padding after a row is not written.
            assert!(buf[w * f.bytes()..g.stride].iter().all(|&v| v == 0xaa));
        }
    }

    fn mr(base: usize, size: usize) -> MemRange {
        MemRange { base, size }
    }

    #[test]
    fn carving_takes_the_reserved_regions_out_of_ram() {
        let ram = [mr(0x8000_0000, 0x7f00_0000)];
        // Walleye-like: a fixed region in the middle, one at the very start of a bank, two touching.
        let res = [
            mr(0x8580_0000, 0x370_0000),
            mr(0x8ab0_0000, 0x70_0000),
            mr(0x8b20_0000, 0x1a0_0000),
        ];
        let mut out = [mr(0, 0); 16];
        let n = carve(&ram, &res, 4096, &mut out);
        // The last two reservations touch: one gap between them is not left.
        assert_eq!(n, 3);
        assert_eq!((out[0].base, out[0].size), (0x8000_0000, 0x580_0000));
        assert_eq!((out[1].base, out[1].size), (0x88f0_0000, 0x1c0_0000));
        assert_eq!(out[2].base, 0x8cc0_0000);
        assert_eq!(out[2].base + out[2].size, 0x8000_0000 + 0x7f00_0000);
        // Nothing overlaps a reserved region.
        for b in &out[..n] {
            for r in &res {
                assert!(b.base + b.size <= r.base || r.base + r.size <= b.base);
            }
        }
        // The total is the RAM minus the reserved bytes.
        let total: usize = out[..n].iter().map(|b| b.size).sum();
        assert_eq!(total, 0x7f00_0000 - 0x370_0000 - 0x70_0000 - 0x1a0_0000);
    }

    #[test]
    fn carving_rounds_reservations_out_and_banks_in() {
        let mut out = [mr(0, 0); 8];
        // A reservation inside one page takes the whole page.
        let n = carve(&[mr(0x1000, 0x4000)], &[mr(0x2100, 0x10)], 4096, &mut out);
        assert_eq!(n, 2);
        assert_eq!((out[0].base, out[0].size), (0x1000, 0x1000));
        assert_eq!((out[1].base, out[1].size), (0x3000, 0x2000));
        // A bank with ragged ends loses the partial pages.
        let n = carve(&[mr(0x1100, 0x2e00)], &[], 4096, &mut out);
        assert_eq!(n, 1);
        assert_eq!((out[0].base, out[0].size), (0x2000, 0x1000));
        // Reservations that cover a bank leave nothing; ones outside change nothing.
        assert_eq!(carve(&[mr(0x1000, 0x1000)], &[mr(0, 0x10000)], 4096, &mut out), 0);
        let n = carve(&[mr(0x1000, 0x1000)], &[mr(0x9000, 0x1000)], 4096, &mut out);
        assert_eq!((n, out[0].base, out[0].size), (1, 0x1000, 0x1000));
        // Overlapping reservations are fine, in any order.
        let n = carve(
            &[mr(0, 0x10000)],
            &[mr(0x6000, 0x3000), mr(0x4000, 0x3000), mr(0x4000, 0x1000)],
            4096,
            &mut out,
        );
        assert_eq!(n, 2);
        assert_eq!((out[0].base, out[0].size), (0, 0x4000));
        assert_eq!((out[1].base, out[1].size), (0x9000, 0x7000));
        // Hostile values do not panic.
        let _ = carve(&[mr(usize::MAX - 5, 100), mr(0, 0)], &[mr(usize::MAX, usize::MAX)], 4096, &mut out);
        // A full output drops the rest without writing past it.
        let mut tiny = [mr(0, 0); 1];
        assert_eq!(carve(&[mr(0, 0x10000)], &[mr(0x4000, 0x1000)], 4096, &mut tiny), 1);
    }

    #[test]
    fn gic_registers_decode() {
        assert_eq!(gic_arch_rev(0x3b), 3);
        assert_eq!(gic_arch_rev(0x4b), 4);
        // 8 CPUs worth, 1020 lines (ITLinesNumber 31), 10 id bits... a made-up TYPER.
        let t = decode_gicd_typer(0x0000_0000 | 31 | (7 << 5) | (1 << 10) | (9 << 19) | (1 << 24));
        assert_eq!(t.lines, 1024);
        assert_eq!(t.cpus, 8);
        assert_eq!(t.id_bits, 10);
        assert!(t.security_extn && t.a3v && !t.lpis);
        assert_eq!(decode_gicd_typer(0).lines, 32);
        assert_eq!(decode_gicd_typer(1 << 17).lpis, true);
    }

    #[test]
    fn redistributors_are_walked_to_the_last() {
        // Eight frames of two pages, the Pixel 2's: affinities 0x000, 0x001, 0x002, 0x003, 0x100..0x103.
        let affs = [0x0u32, 1, 2, 3, 0x100, 0x101, 0x102, 0x103];
        let typer = |off: usize| {
            let i = off / 0x2_0000;
            let last = if i == 7 { 0x10 } else { 0 };
            ((affs[i] as u64) << 32) | ((i as u64) << 8) | last
        };
        let mut out = [Redist::default(); 16];
        let n = redistributors(0x10_0000, typer, &mut out);
        assert_eq!(n, 8);
        assert!(out[7].last && !out[6].last);
        assert_eq!(out[4].affinity, 0x100);
        assert_eq!(out[5].offset, 5 * 0x2_0000);
        assert_eq!(out[3].processor, 3);
        // A region too small for the last frames stops early.
        assert_eq!(redistributors(0x6_0000, typer, &mut out), 3);
        // The output limit stops it too.
        assert_eq!(redistributors(0x10_0000, typer, &mut out[..2]), 2);
        // GICv4 frames are four pages.
        let v4 = |off: usize| if off == 0 { 2u64 } else { 0x10 | 2 };
        let n = redistributors(0x10_0000, v4, &mut out);
        assert_eq!(n, 2);
        assert_eq!(out[1].offset, 0x4_0000);
        assert!(out[0].vlpis);
        // A frame that never says last ends with the region.
        assert_eq!(redistributors(0x8_0000, |_| 0, &mut out), 4);
    }

    #[test]
    fn mpidr_maps_to_the_redistributor_affinity() {
        assert_eq!(mpidr_affinity(0x8000_0101), 0x0000_0101);
        assert_eq!(mpidr_affinity(0x01_8000_0203), 0x0100_0203);
    }
}
