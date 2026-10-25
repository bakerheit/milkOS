//! The 3.5-inch SPI colour display on the Pi's 40-pin header (docs/raspberry-pi.md, "3.5-inch SPI
//! display"): everything about it that can be decided from bytes alone, so the kernel's driver
//! (`kernel/src/rpi/lcd.rs`, SPI0 and GPIO) stays thin and the host tests can run the very same code
//! against a model of the panel.
//!
//! The kits sold for this are a 480x320 TFT with a resistive touch chip. Three controllers are
//! supported, picked by a boot option ([`Controller`]):
//!
//! * `ili9486`: the common "Waveshare 3.5 inch RPi LCD (A)"-style kit. The controller runs on its
//!   16-bit parallel bus and the SPI is turned into that bus by shift registers, so every command and
//!   every parameter goes out as a 16-bit word (high byte 0, then the value), with D/C low for the
//!   command and high for the parameters. Pixels are plain RGB565, two bytes each, high byte first.
//! * `ili9488`: a controller wired to SPI directly (8-bit commands) that only takes 18-bit pixels
//!   over SPI: three bytes per pixel.
//! * `st7796`: the same wiring with RGB565 pixels.
//!
//! The panel is a 320x480 portrait one; [`Rotation`] turns it into the 480x320 landscape the kit is
//! used in (the default) or any other quarter turn, through the memory access control register.
//! None of this knows about hardware: the driver speaks to a [`Bus`] and the pixels come from a
//! [`PixelSource`]. Written from the controllers' public datasheets and the way such kits are known
//! to behave; nothing copied from any driver.

/// The native (portrait) size of the panel.
pub const NATIVE_W: usize = 320;
pub const NATIVE_H: usize = 480;
/// Pixels in the panel, in any rotation.
pub const PIXELS: usize = NATIVE_W * NATIVE_H;

// Commands every one of the controllers has (MIPI display command set).
pub const SWRESET: u8 = 0x01;
pub const SLPOUT: u8 = 0x11;
pub const NORON: u8 = 0x13;
pub const INVOFF: u8 = 0x20;
pub const INVON: u8 = 0x21;
pub const DISPON: u8 = 0x29;
pub const CASET: u8 = 0x2a;
pub const PASET: u8 = 0x2b;
pub const RAMWR: u8 = 0x2c;
pub const MADCTL: u8 = 0x36;
pub const COLMOD: u8 = 0x3a;

// Memory access control bits.
pub const MADCTL_MY: u8 = 0x80;
pub const MADCTL_MX: u8 = 0x40;
pub const MADCTL_MV: u8 = 0x20;
pub const MADCTL_BGR: u8 = 0x08;

// ---- rectangles -------------------------------------------------------------------------------

/// A rectangle of pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub const fn new(x: usize, y: usize, w: usize, h: usize) -> Rect {
        Rect { x, y, w, h }
    }
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub const fn x1(&self) -> usize {
        self.x + self.w
    }
    pub const fn y1(&self) -> usize {
        self.y + self.h
    }
    pub const fn area(&self) -> usize {
        self.w * self.h
    }
    /// The smallest rectangle holding both (an empty one does not count).
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect {
            x,
            y,
            w: self.x1().max(o.x1()) - x,
            h: self.y1().max(o.y1()) - y,
        }
    }
    /// The overlap (empty if none).
    pub fn intersect(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = (self.x1().min(o.x1()), self.y1().min(o.y1()));
        if x1 <= x || y1 <= y {
            Rect::default()
        } else {
            Rect {
                x,
                y,
                w: x1 - x,
                h: y1 - y,
            }
        }
    }
    /// Overlapping, or sharing an edge.
    pub fn touches(&self, o: &Rect) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.x <= o.x1()
            && o.x <= self.x1()
            && self.y <= o.y1()
            && o.y <= self.y1()
    }
    /// Cut to a `w` x `h` area at the origin.
    pub fn clip(&self, w: usize, h: usize) -> Rect {
        self.intersect(&Rect::new(0, 0, w, h))
    }
}

/// The rectangles of the screen that changed and have not been sent to the panel yet. At most
/// four are kept: a fifth is merged into whichever rectangle it enlarges least, and rectangles
/// whose union wastes little (a quarter, plus a few hundred pixels: a window command costs about
/// as much on the wire) are merged at once.
#[derive(Clone, Copy, Debug)]
pub struct Dirty {
    r: [Rect; 4],
    n: usize,
}

impl Default for Dirty {
    fn default() -> Self {
        Self::new()
    }
}

impl Dirty {
    pub const fn new() -> Dirty {
        Dirty {
            r: [Rect::new(0, 0, 0, 0); 4],
            n: 0,
        }
    }
    pub const fn is_empty(&self) -> bool {
        self.n == 0
    }
    pub const fn len(&self) -> usize {
        self.n
    }
    pub fn clear(&mut self) {
        self.n = 0;
    }
    /// Pixels that would be sent now.
    pub fn area(&self) -> usize {
        self.r[..self.n].iter().map(Rect::area).sum()
    }
    /// Note that `r` changed.
    pub fn add(&mut self, r: Rect) {
        let mut r = r;
        if r.is_empty() {
            return;
        }
        loop {
            let hit = (0..self.n).find(|&i| {
                let a = self.r[i];
                a.union(&r).area() <= (a.area() + r.area()) * 5 / 4 + 256
            });
            match hit {
                Some(i) => {
                    r = self.r[i].union(&r);
                    self.n -= 1;
                    self.r[i] = self.r[self.n];
                }
                None => break,
            }
        }
        if self.n < self.r.len() {
            self.r[self.n] = r;
            self.n += 1;
            return;
        }
        // Full: merge into the rectangle it enlarges least.
        let mut best = 0;
        let mut waste = usize::MAX;
        for i in 0..self.n {
            let w = self.r[i].union(&r).area() - self.r[i].area();
            if w < waste {
                waste = w;
                best = i;
            }
        }
        self.r[best] = self.r[best].union(&r);
    }
    /// Take one rectangle to send (the last added first), or `None`.
    pub fn pop(&mut self) -> Option<Rect> {
        if self.n == 0 {
            return None;
        }
        self.n -= 1;
        Some(self.r[self.n])
    }
}

// ---- the controller ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Controller {
    Ili9486,
    Ili9488,
    St7796,
}

impl Controller {
    /// `ili9486`, `ili9488` or `st7796` (any case).
    pub fn parse(s: &[u8]) -> Option<Controller> {
        let eq = |name: &str| s.eq_ignore_ascii_case(name.as_bytes());
        if eq("ili9486") {
            Some(Controller::Ili9486)
        } else if eq("ili9488") {
            Some(Controller::Ili9488)
        } else if eq("st7796") {
            Some(Controller::St7796)
        } else {
            None
        }
    }
    pub const fn name(self) -> &'static str {
        match self {
            Controller::Ili9486 => "ili9486",
            Controller::Ili9488 => "ili9488",
            Controller::St7796 => "st7796",
        }
    }
    /// Commands and their parameters go out as 16-bit words (the shift-register kits).
    pub const fn word16(self) -> bool {
        matches!(self, Controller::Ili9486)
    }
    /// Bytes per pixel on the wire.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Controller::Ili9488 => 3,
            _ => 2,
        }
    }
    /// Does the usual module show correct colours only with the display inversion on?
    const fn inverted(self) -> bool {
        matches!(self, Controller::St7796)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rotation {
    /// Portrait, 320x480 (the panel's own orientation).
    Deg0,
    /// Landscape 480x320 (how the kit is used: the default).
    Deg90,
    Deg180,
    Deg270,
}

impl Rotation {
    /// `0`, `90`, `180` or `270`.
    pub fn parse(s: &[u8]) -> Option<Rotation> {
        match s {
            b"0" => Some(Rotation::Deg0),
            b"90" => Some(Rotation::Deg90),
            b"180" => Some(Rotation::Deg180),
            b"270" => Some(Rotation::Deg270),
            _ => None,
        }
    }
    pub const fn degrees(self) -> u32 {
        match self {
            Rotation::Deg0 => 0,
            Rotation::Deg90 => 90,
            Rotation::Deg180 => 180,
            Rotation::Deg270 => 270,
        }
    }
    pub const fn landscape(self) -> bool {
        matches!(self, Rotation::Deg90 | Rotation::Deg270)
    }
}

/// What the boot options say about the display.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    pub controller: Controller,
    pub rotation: Rotation,
    /// The glass has its colour filters in B, G, R order (the kit's own does): sets the BGR bit.
    pub bgr: bool,
    /// Display inversion; `None` = the controller's usual setting.
    pub invert: Option<bool>,
    /// The memory access control byte as it is, instead of the one computed from the rotation.
    pub madctl: Option<u8>,
}

impl Config {
    pub const fn new(controller: Controller) -> Config {
        Config {
            controller,
            rotation: Rotation::Deg90,
            bgr: true,
            invert: None,
            madctl: None,
        }
    }
    /// Visible width and height.
    pub const fn width(&self) -> usize {
        if self.rotation.landscape() {
            NATIVE_H
        } else {
            NATIVE_W
        }
    }
    pub const fn height(&self) -> usize {
        if self.rotation.landscape() {
            NATIVE_W
        } else {
            NATIVE_H
        }
    }
    /// The memory access control byte: the rotation (the mirroring that makes each quarter turn a
    /// true rotation of the kit's glass; 0x40, 0x20, 0x80, 0xe0 plus the BGR bit) or the override.
    pub const fn madctl(&self) -> u8 {
        if let Some(v) = self.madctl {
            return v;
        }
        let base = match self.rotation {
            Rotation::Deg0 => MADCTL_MX,
            Rotation::Deg90 => MADCTL_MV,
            Rotation::Deg180 => MADCTL_MY,
            Rotation::Deg270 => MADCTL_MV | MADCTL_MX | MADCTL_MY,
        };
        base | if self.bgr { MADCTL_BGR } else { 0 }
    }
    pub const fn inverted(&self) -> bool {
        match self.invert {
            Some(v) => v,
            None => self.controller.inverted(),
        }
    }
}

// ---- the bus -----------------------------------------------------------------------------------

/// What the driver needs from the hardware: SPI bytes with the D/C line set, the reset line and
/// time. The kernel implements it on SPI0 and GPIO; the host tests on a model of the panel.
pub trait Bus {
    /// Clock `bytes` out with D/C low (`data == false`: a command) or high. False if the transfer
    /// failed or timed out (the driver then gives up).
    fn write(&mut self, data: bool, bytes: &[u8]) -> bool;
    /// Drive the reset line.
    fn reset_pin(&mut self, high: bool);
    fn delay_ms(&mut self, ms: u32);
}

/// Where pixels come from when a rectangle is drawn.
pub trait PixelSource {
    /// Fill `out` with the pixels (`0x00RRGGBB`) of row `y`, from column `x`.
    fn row(&mut self, y: usize, x: usize, out: &mut [u32]);
}

/// One colour everywhere.
pub struct Solid(pub u32);

impl PixelSource for Solid {
    fn row(&mut self, _y: usize, _x: usize, out: &mut [u32]) {
        out.fill(self.0);
    }
}

/// A `width` x `height` array of pixels (stride = width) as a source.
pub struct Pixels<'a> {
    pub px: &'a [u32],
    pub width: usize,
}

impl PixelSource for Pixels<'_> {
    fn row(&mut self, y: usize, x: usize, out: &mut [u32]) {
        let base = y * self.width + x;
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.px.get(base + i).copied().unwrap_or(0);
        }
    }
}

// ---- pixels on the wire -------------------------------------------------------------------------

/// `0x00RRGGBB` to RGB565.
pub const fn rgb565(c: u32) -> u16 {
    (((c >> 8) & 0xf800) | ((c >> 5) & 0x07e0) | ((c >> 3) & 0x001f)) as u16
}

/// RGB565 back to `0x00RRGGBB` (the low bits filled by replicating the high ones).
pub const fn rgb565_to_xrgb(v: u16) -> u32 {
    let (r, g, b) = (
        ((v >> 11) & 0x1f) as u32,
        ((v >> 5) & 0x3f) as u32,
        (v & 0x1f) as u32,
    );
    let (r, g, b) = (
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    );
    (r << 16) | (g << 8) | b
}

/// Pixels as the controller takes them: RGB565 high byte first (two bytes each), or for the
/// 18-bit controllers R, G, B with the six colour bits in the top of each byte (three bytes
/// each). Returns the number of bytes written; `out` must hold `px.len() * bytes_per_pixel`.
pub fn pack(c: Controller, px: &[u32], out: &mut [u8]) -> usize {
    match c.bytes_per_pixel() {
        3 => {
            for (p, o) in px.iter().zip(out.chunks_exact_mut(3)) {
                o[0] = (*p >> 16) as u8 & 0xfc;
                o[1] = (*p >> 8) as u8 & 0xfc;
                o[2] = *p as u8 & 0xfc;
            }
            px.len() * 3
        }
        _ => {
            for (p, o) in px.iter().zip(out.chunks_exact_mut(2)) {
                o.copy_from_slice(&rgb565(*p).to_be_bytes());
            }
            px.len() * 2
        }
    }
}

// ---- commands -----------------------------------------------------------------------------------

/// Send command `cmd` with its parameters, in the controller's framing.
pub fn command<B: Bus>(cfg: &Config, bus: &mut B, cmd: u8, params: &[u8]) -> bool {
    if cfg.controller.word16() {
        if !bus.write(false, &[0, cmd]) {
            return false;
        }
        if params.is_empty() {
            return true;
        }
        let mut buf = [0u8; 64];
        if params.len() * 2 > buf.len() {
            return false;
        }
        for (i, p) in params.iter().enumerate() {
            buf[2 * i + 1] = *p;
        }
        bus.write(true, &buf[..params.len() * 2])
    } else {
        bus.write(false, &[cmd]) && (params.is_empty() || bus.write(true, params))
    }
}

struct Entry {
    cmd: u8,
    params: &'static [u8],
    /// Milliseconds to wait afterwards.
    wait: u16,
}

const fn e(cmd: u8, params: &'static [u8], wait: u16) -> Entry {
    Entry { cmd, params, wait }
}

// The set-up that comes before the memory access control byte, the inversion and the display on.
const ILI9486_SETUP: &[Entry] = &[
    e(SWRESET, &[], 150),
    e(SLPOUT, &[], 150),
    e(COLMOD, &[0x55], 0),
    // Power control 3 (normal mode), VCOM.
    e(0xc2, &[0x44], 0),
    e(0xc5, &[0x00, 0x00, 0x00, 0x00], 0),
    // Positive and negative gamma.
    e(
        0xe0,
        &[
            0x0f, 0x1f, 0x1c, 0x0c, 0x0f, 0x08, 0x48, 0x98, 0x37, 0x0a, 0x13, 0x04, 0x11, 0x0d,
            0x00,
        ],
        0,
    ),
    e(
        0xe1,
        &[
            0x0f, 0x32, 0x2e, 0x0b, 0x0d, 0x05, 0x47, 0x75, 0x37, 0x06, 0x10, 0x03, 0x24, 0x20,
            0x00,
        ],
        0,
    ),
];

const ILI9488_SETUP: &[Entry] = &[
    e(SWRESET, &[], 150),
    e(SLPOUT, &[], 150),
    // Gamma, power control 1 and 2, VCOM, frame rate, inversion type, display function.
    e(
        0xe0,
        &[
            0x00, 0x03, 0x09, 0x08, 0x16, 0x0a, 0x3f, 0x78, 0x4c, 0x09, 0x0a, 0x08, 0x16, 0x1a,
            0x0f,
        ],
        0,
    ),
    e(
        0xe1,
        &[
            0x00, 0x16, 0x19, 0x03, 0x0f, 0x05, 0x32, 0x45, 0x46, 0x04, 0x0e, 0x0d, 0x35, 0x37,
            0x0f,
        ],
        0,
    ),
    e(0xc0, &[0x17, 0x15], 0),
    e(0xc1, &[0x41], 0),
    e(0xc5, &[0x00, 0x12, 0x80], 0),
    e(COLMOD, &[0x66], 0),
    e(0xb1, &[0xa0], 0),
    e(0xb4, &[0x02], 0),
    e(0xb6, &[0x02, 0x02], 0),
];

const ST7796_SETUP: &[Entry] = &[
    e(SWRESET, &[], 150),
    e(SLPOUT, &[], 150),
    // Command set 2 enable (two steps), then the tuning, then disable again.
    e(0xf0, &[0xc3], 0),
    e(0xf0, &[0x96], 0),
    e(COLMOD, &[0x55], 0),
    e(0xb4, &[0x01], 0),
    e(0xb6, &[0x80, 0x02, 0x3b], 0),
    e(0xe8, &[0x40, 0x8a, 0x00, 0x00, 0x29, 0x19, 0xa5, 0x33], 0),
    e(0xc1, &[0x06], 0),
    e(0xc2, &[0xa7], 0),
    e(0xc5, &[0x18], 0),
    e(
        0xe0,
        &[
            0xf0, 0x09, 0x0b, 0x06, 0x04, 0x15, 0x2f, 0x54, 0x42, 0x3c, 0x17, 0x14, 0x18, 0x1b,
        ],
        0,
    ),
    e(
        0xe1,
        &[
            0xe0, 0x09, 0x0b, 0x06, 0x04, 0x03, 0x2b, 0x43, 0x42, 0x3b, 0x16, 0x14, 0x17, 0x1b,
        ],
        0,
    ),
    e(0xf0, &[0x3c], 0),
    e(0xf0, &[0x69], 0),
];

/// Reset the panel and bring it up: hardware reset pulse, then the controller's set-up, the
/// memory access control byte, the inversion, normal mode and display on. False if a transfer
/// failed (the panel is then not usable).
pub fn init<B: Bus>(cfg: &Config, bus: &mut B) -> bool {
    bus.reset_pin(true);
    bus.delay_ms(10);
    bus.reset_pin(false);
    bus.delay_ms(20);
    bus.reset_pin(true);
    bus.delay_ms(120);
    let setup = match cfg.controller {
        Controller::Ili9486 => ILI9486_SETUP,
        Controller::Ili9488 => ILI9488_SETUP,
        Controller::St7796 => ST7796_SETUP,
    };
    for s in setup {
        if !command(cfg, bus, s.cmd, s.params) {
            return false;
        }
        if s.wait != 0 {
            bus.delay_ms(s.wait as u32);
        }
    }
    command(cfg, bus, MADCTL, &[cfg.madctl()])
        && command(cfg, bus, if cfg.inverted() { INVON } else { INVOFF }, &[])
        && command(cfg, bus, NORON, &[])
        && command(cfg, bus, DISPON, &[])
        && {
            bus.delay_ms(50);
            true
        }
}

/// Open the memory window `r` (column and page address) and start a memory write: the pixel bytes
/// sent next, with D/C high, fill the window row by row. `r` must be inside the screen.
pub fn window<B: Bus>(cfg: &Config, bus: &mut B, r: Rect) -> bool {
    if r.is_empty() || r.x1() > cfg.width() || r.y1() > cfg.height() {
        return false;
    }
    let (x0, y0, x1, y1) = (
        r.x as u16,
        r.y as u16,
        (r.x1() - 1) as u16,
        (r.y1() - 1) as u16,
    );
    command(
        cfg,
        bus,
        CASET,
        &[(x0 >> 8) as u8, x0 as u8, (x1 >> 8) as u8, x1 as u8],
    ) && command(
        cfg,
        bus,
        PASET,
        &[(y0 >> 8) as u8, y0 as u8, (y1 >> 8) as u8, y1 as u8],
    ) && command(cfg, bus, RAMWR, &[])
}

/// Bytes of pixel data per write to the bus.
const CHUNK: usize = 2048;

/// Draw `r` (inside the screen) from `src`. No allocation: a row of pixels and a chunk of bytes
/// on the stack (about 4 KiB), so it works from a crash handler. False if the bus failed.
pub fn draw_rect<B: Bus, S: PixelSource>(cfg: &Config, bus: &mut B, r: Rect, src: &mut S) -> bool {
    let r = r.clip(cfg.width(), cfg.height());
    if r.is_empty() {
        return true;
    }
    if !window(cfg, bus, r) {
        return false;
    }
    let bpp = cfg.controller.bytes_per_pixel();
    let mut row = [0u32; NATIVE_H];
    let mut bytes = [0u8; CHUNK];
    let per = (CHUNK / (r.w * bpp)).max(1);
    let mut y = r.y;
    while y < r.y1() {
        let n = per.min(r.y1() - y);
        let mut len = 0;
        for yy in y..y + n {
            src.row(yy, r.x, &mut row[..r.w]);
            len += pack(cfg.controller, &row[..r.w], &mut bytes[len..]);
        }
        if !bus.write(true, &bytes[..len]) {
            return false;
        }
        y += n;
    }
    true
}

// ---- showing a bigger picture on the small screen -----------------------------------------------

/// How a picture of some other size ([`View::source`], the desktop) is shown on the panel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct View {
    sw: usize,
    sh: usize,
    lw: usize,
    lh: usize,
    /// Where the picture lies on the panel.
    dest: Rect,
    /// True: the whole picture, scaled down (a box filter). False: a panel-sized piece of it, 1:1,
    /// from `(px, py)`.
    scaled: bool,
    px: usize,
    py: usize,
}

impl View {
    /// The whole `sw` x `sh` picture on an `lw` x `lh` panel, scaled down to fit with the aspect
    /// ratio kept (black bars where it does not fill), or 1:1 if it is smaller.
    pub fn scaled(sw: usize, sh: usize, lw: usize, lh: usize) -> View {
        let (sw, sh) = (sw.max(1), sh.max(1));
        let (dw, dh) = if sw <= lw && sh <= lh {
            (sw, sh)
        } else if sw * lh >= sh * lw {
            (lw, (sh * lw / sw).max(1))
        } else {
            ((sw * lh / sh).max(1), lh)
        };
        View {
            sw,
            sh,
            lw,
            lh,
            dest: Rect::new((lw - dw) / 2, (lh - dh) / 2, dw, dh),
            scaled: true,
            px: 0,
            py: 0,
        }
    }
    /// A panel-sized piece of the picture at 1:1, starting at the top left.
    pub fn cropped(sw: usize, sh: usize, lw: usize, lh: usize) -> View {
        let (sw, sh) = (sw.max(1), sh.max(1));
        let (dw, dh) = (sw.min(lw), sh.min(lh));
        View {
            sw,
            sh,
            lw,
            lh,
            dest: Rect::new((lw - dw) / 2, (lh - dh) / 2, dw, dh),
            scaled: false,
            px: 0,
            py: 0,
        }
    }
    pub fn source(&self) -> (usize, usize) {
        (self.sw, self.sh)
    }
    pub fn panel(&self) -> (usize, usize) {
        (self.lw, self.lh)
    }
    pub fn dest(&self) -> Rect {
        self.dest
    }
    pub fn is_scaled(&self) -> bool {
        self.scaled
    }
    /// The top left of the piece shown (a cropped view; always 0 for a scaled one).
    pub fn origin(&self) -> (usize, usize) {
        (self.px, self.py)
    }

    /// The panel rectangle that has to be redrawn when source rectangle `r` changed.
    pub fn map_rect(&self, r: Rect) -> Rect {
        let r = r.clip(self.sw, self.sh);
        if r.is_empty() {
            return r;
        }
        let d = self.dest;
        if self.scaled {
            // One panel pixel more on each side: the box of a panel pixel can reach into `r`.
            let x0 = (r.x * d.w / self.sw).saturating_sub(1);
            let y0 = (r.y * d.h / self.sh).saturating_sub(1);
            let x1 = ((r.x1() * d.w).div_ceil(self.sw) + 1).min(d.w);
            let y1 = ((r.y1() * d.h).div_ceil(self.sh) + 1).min(d.h);
            Rect::new(
                d.x + x0,
                d.y + y0,
                x1.saturating_sub(x0),
                y1.saturating_sub(y0),
            )
        } else {
            let shown = Rect::new(self.px, self.py, d.w, d.h);
            let i = r.intersect(&shown);
            if i.is_empty() {
                return i;
            }
            Rect::new(d.x + i.x - self.px, d.y + i.y - self.py, i.w, i.h)
        }
    }

    /// Where a point of the panel is in the picture (clamped to it): for touch.
    pub fn to_source(&self, lx: i32, ly: i32) -> (i32, i32) {
        let d = self.dest;
        let x = (lx - d.x as i32).clamp(0, d.w as i32 - 1) as usize;
        let y = (ly - d.y as i32).clamp(0, d.h as i32 - 1) as usize;
        if self.scaled {
            // The middle of the panel pixel's box.
            (
                (((2 * x + 1) * self.sw) / (2 * d.w)).min(self.sw - 1) as i32,
                (((2 * y + 1) * self.sh) / (2 * d.h)).min(self.sh - 1) as i32,
            )
        } else {
            ((self.px + x) as i32, (self.py + y) as i32)
        }
    }

    /// Fill `out` with the panel pixels of row `ly` from column `lx0`, taken from `src` (`stride`
    /// pixels per row). Outside the picture: black.
    pub fn render_row(&self, src: &[u32], stride: usize, ly: usize, lx0: usize, out: &mut [u32]) {
        let d = self.dest;
        if ly < d.y || ly >= d.y1() {
            out.fill(0);
            return;
        }
        let get = |x: usize, y: usize| src.get(y * stride + x).copied().unwrap_or(0);
        let dy = ly - d.y;
        if !self.scaled {
            let sy = self.py + dy;
            for (i, o) in out.iter_mut().enumerate() {
                let lx = lx0 + i;
                *o = if lx >= d.x && lx < d.x1() {
                    get(self.px + lx - d.x, sy)
                } else {
                    0
                };
            }
            return;
        }
        let y0 = dy * self.sh / d.h;
        let y1 = ((dy + 1) * self.sh / d.h).clamp(y0 + 1, self.sh);
        for (i, o) in out.iter_mut().enumerate() {
            let lx = lx0 + i;
            if lx < d.x || lx >= d.x1() {
                *o = 0;
                continue;
            }
            let dx = lx - d.x;
            let x0 = dx * self.sw / d.w;
            let x1 = ((dx + 1) * self.sw / d.w).clamp(x0 + 1, self.sw);
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = get(x, y);
                    r += (p >> 16) & 0xff;
                    g += (p >> 8) & 0xff;
                    b += p & 0xff;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            *o = ((r + n / 2) / n) << 16 | ((g + n / 2) / n) << 8 | ((b + n / 2) / n);
        }
    }

    /// A cropped view follows a finger held near the edge of the panel: when `(lx, ly)` is within
    /// `margin` pixels of an edge the piece moves `step` pixels towards that edge. True if it moved.
    pub fn pan_towards(&mut self, lx: i32, ly: i32, margin: i32, step: usize) -> bool {
        if self.scaled {
            return false;
        }
        let (maxx, maxy) = (self.sw - self.dest.w, self.sh - self.dest.h);
        let (ox, oy) = (self.px, self.py);
        if lx < margin {
            self.px = self.px.saturating_sub(step);
        } else if lx >= self.lw as i32 - margin {
            self.px = (self.px + step).min(maxx);
        }
        if ly < margin {
            self.py = self.py.saturating_sub(step);
        } else if ly >= self.lh as i32 - margin {
            self.py = (self.py + step).min(maxy);
        }
        (ox, oy) != (self.px, self.py)
    }
}

/// A [`PixelSource`] that shows `src` (stride `stride`) through a [`View`].
pub struct ViewSource<'a> {
    pub view: View,
    pub src: &'a [u32],
    pub stride: usize,
}

impl PixelSource for ViewSource<'_> {
    fn row(&mut self, y: usize, x: usize, out: &mut [u32]) {
        self.view.render_row(self.src, self.stride, y, x, out);
    }
}
