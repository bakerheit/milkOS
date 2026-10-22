//! Wallpapers: "dawn pasture" (the 1.0 one, SPEC: Wallpaper), "night pasture" (the dark
//! theme's: moon, stars, the cow asleep) and plain "oat", each drawn once when chosen, plus
//! the pre-blurred copy glass panels sample (SPEC: Glass), tinted with the theme's milk.
//! The stronger glass (launcher, control centre) blends a little more milk over that copy
//! when its panel is rendered ([`Wallpaper::glass`]), instead of keeping a second tinted
//! full-screen copy; the panels redrawn every frame (dock, menu bar) use the copy as is.
//! Integer maths only (the target is soft-float).

use crate::gfx::{Color, DrawTarget, Rect, blend, rgb};
use crate::pmm::Use;

use super::buffer::Buffer;
use super::paint::Painter;
use super::theme::*;

pub struct Wallpaper {
    /// W: the sharp wallpaper.
    pub sharp: Buffer,
    /// Wb (box blur r24 x3) with milk blended over at 0.55: the menu bar / dock / toast glass.
    glass: Buffer,
    /// The milk blended over Wb (`theme::tc(MILK)` when this wallpaper was made).
    milk: Color,
}

/// Milk over the 0.55 glass that makes it the 0.7 one: 1 - (1 - 0.7) / (1 - 0.55), rounded
/// (within one level of blending 0.7 over Wb directly).
const STRONG_OVER_GLASS: u8 = ((255 * (GLASS_STRONG_A as u32 - GLASS_A as u32)
    + (255 - GLASS_A as u32) / 2)
    / (255 - GLASS_A as u32)) as u8;

/// How much milk a glass panel has over the blur.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glass {
    /// 0.55: the menu bar, dock, toasts, the lock screen.
    Normal,
    /// 0.7: the launcher and control centre.
    Strong,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Dawn,
    Night,
    Oat,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Dawn, Kind::Night, Kind::Oat];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Dawn => "Dawn pasture",
            Kind::Night => "Night pasture",
            Kind::Oat => "Oat",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Dawn => "dawn",
            Kind::Night => "night",
            Kind::Oat => "oat",
        }
    }
}

/// Draw wallpaper `kind` full screen into `b` (also used for the Settings previews).
pub fn paint(b: &mut Buffer, kind: Kind) {
    // The artist's wallpaper (crate::art), scaled to cover this size; the drawn one if
    // there is none. Decoded straight into `b`, or (another size) into frames freed right
    // after: a full-screen temporary on the kernel heap would stay with the heap for good.
    if let Some(a) = crate::art::image(&alloc::format!("wallpaper-{}", kind.key())) {
        let (w, h, stride) = (b.w() as usize, b.h() as usize, b.stride());
        let (sw, sh) = (a.w as usize, a.h as usize);
        if (sw, sh) == (w, h) {
            a.decode_rows(b.pixels_mut(), stride);
            return;
        }
        if let Some(mut src) = Buffer::new(Use::Wallpaper, sw, sh, 0) {
            let ss = src.stride();
            a.decode_rows(src.pixels_mut(), ss);
            crate::art::scale_cover(src.pixels(), ss, sw, sh, b.pixels_mut(), stride, w, h);
            return;
        }
    }
    match kind {
        Kind::Dawn => draw(b),
        Kind::Night => draw_night(b),
        Kind::Oat => draw_oat(b),
    }
}

impl Wallpaper {
    /// Wallpaper `kind`, with glass tinted for the current theme (`theme::tc(MILK)`).
    pub fn new(kind: Kind) -> Option<Wallpaper> {
        let (w, h) = (screen_w() as usize, screen_h() as usize);
        let mut sharp = Buffer::new(Use::Wallpaper, w, h, 0)?;
        paint(&mut sharp, kind);
        let mut glass = Buffer::new(Use::Wallpaper, w, h, 0)?;
        copy(&sharp, &mut glass);
        for _ in 0..BLUR_PASSES {
            blur_rows(&mut glass, BLUR_RADIUS);
            blur_cols(&mut glass, BLUR_RADIUS);
        }
        let milk = tc(MILK);
        tint(&mut glass, milk, GLASS_A);
        Some(Wallpaper { sharp, glass, milk })
    }

    /// Paint the glass under the screen rect `src` at (x, y) of `p`, at `strength`.
    pub fn glass(&self, p: &mut Painter, x: i32, y: i32, src: Rect, strength: Glass) {
        match strength {
            Glass::Normal => p.blit(x, y, &self.glass, src.x, src.y, src.w, src.h),
            Glass::Strong => p.blit_tinted(x, y, &self.glass, src, self.milk, STRONG_OVER_GLASS),
        }
    }
}

fn copy(src: &Buffer, dst: &mut Buffer) {
    let n = src.stride() * src.height();
    dst.pixels_mut()[..n].copy_from_slice(&src.pixels()[..n]);
}

fn tint(b: &mut Buffer, c: Color, a: u8) {
    for p in b.pixels_mut() {
        *p = blend(*p, c, a);
    }
}

fn lerp(a: Color, b: Color, t: u32, n: u32) -> Color {
    let ch = |s: u32| {
        let (x, y) = ((a >> s) & 0xFF, (b >> s) & 0xFF);
        ((x * (n - t) + y * t + n / 2) / n) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Anti-aliased filled circle, centre and radius in 1/16 px (pixel centres are at 16*i + 8).
fn disc(b: &mut Buffer, cx16: i64, cy16: i64, r16: i64, c: Color, alpha: u8) {
    let (w, h) = (b.w() as i64, b.h() as i64);
    let stride = b.stride();
    let px = b.pixels_mut();
    let y0 = ((cy16 - r16) / 16 - 1).max(0);
    let y1 = ((cy16 + r16) / 16 + 2).min(h);
    for y in y0..y1 {
        let dy = 16 * y + 8 - cy16;
        let outer2 = (r16 + 8) * (r16 + 8) - dy * dy;
        if outer2 <= 0 {
            continue;
        }
        let hw_out = outer2.isqrt();
        let inner2 = (r16 - 8) * (r16 - 8) - dy * dy;
        let hw_in = if inner2 > 0 && r16 > 8 {
            inner2.isqrt()
        } else {
            -1
        };
        let xa = ((cx16 - hw_out) / 16 - 1).max(0);
        let xb = ((cx16 + hw_out) / 16 + 2).min(w);
        let row = &mut px[y as usize * stride..y as usize * stride + w as usize];
        for x in xa..xb {
            let dx = 16 * x + 8 - cx16;
            let a = if dx.abs() < hw_in {
                alpha as i64
            } else {
                let d = (dx * dx + dy * dy).isqrt();
                let cov = (r16 + 8 - d).clamp(0, 16);
                cov * alpha as i64 / 16
            };
            if a > 0 {
                let p = &mut row[x as usize];
                *p = blend(*p, c, a as u8);
            }
        }
    }
}

/// CSS-style circle: `border-radius: 50%` on a `d` x `d` box at (x, y).
fn css_circle(b: &mut Buffer, x: i64, y: i64, d: i64, c: Color, alpha: u8) {
    disc(b, 16 * x + 8 * d, 16 * y + 8 * d, 8 * d, c, alpha);
}

/// The scenes are designed at 1024x768 (SPEC). Other display modes scale them uniformly by
/// the width; the sky keeps its distance from the top, the hills (and the cow) from the
/// bottom. At 1024x768 every mapping is the identity.
#[derive(Clone, Copy)]
struct Fit {
    w: i64,
    h: i64,
}

impl Fit {
    fn of(b: &Buffer) -> Fit {
        Fit {
            w: b.w() as i64,
            h: b.h() as i64,
        }
    }
    /// A length (or x).
    fn d(self, v: i64) -> i64 {
        v * self.w / 1024
    }
    /// A y measured from the top.
    fn top(self, y: i64) -> i64 {
        self.d(y)
    }
    /// A y measured from the bottom.
    fn bot(self, y: i64) -> i64 {
        self.h - self.d(768 - y)
    }
    /// A circle anchored to the top / the bottom.
    fn sky(self, b: &mut Buffer, x: i64, y: i64, d: i64, c: Color, alpha: u8) {
        css_circle(b, self.d(x), self.top(y), self.d(d), c, alpha);
    }
    fn hill(self, b: &mut Buffer, x: i64, y: i64, d: i64, c: Color, alpha: u8) {
        css_circle(b, self.d(x), self.bot(y), self.d(d), c, alpha);
    }
    /// How far the cow (drawn at its 1024x768 spot, (842, 636)) moves.
    fn cow(self) -> (i32, i32) {
        ((self.d(842) - 842) as i32, (self.bot(636) - 636) as i32)
    }
}

fn draw(b: &mut Buffer) {
    // Vertical gradient #FBE3C8 0% -> #F3C3B8 52% -> #C9B3D9 100%.
    let (top, mid, bot) = (
        rgb(0xFB, 0xE3, 0xC8),
        rgb(0xF3, 0xC3, 0xB8),
        rgb(0xC9, 0xB3, 0xD9),
    );
    let h = b.h() as u32;
    let split = h * 52 / 100;
    for y in 0..h {
        let c = if y < split {
            lerp(top, mid, y, split)
        } else {
            lerp(mid, bot, y - split, h - split)
        };
        b.fill_rect(0, y as i32, b.w(), 1, c);
    }
    // Sun halo, sun, three hills.
    let f = Fit::of(b);
    f.sky(b, 700, 40, 240, MILK, a(280));
    f.sky(b, 740, 80, 160, HONEY, 255);
    f.hill(b, -120, 500, 1040, rgb(0xCF, 0xE0, 0xC4), 255);
    f.hill(b, 380, 590, 1100, rgb(0xB9, 0xD3, 0xB0), 255);
    f.hill(b, 760, 650, 700, rgb(0xA5, 0xC7, 0x9F), 255);
    // The tiny cow.
    let (cx, cy) = f.cow();
    let (bw, bh) = (b.w(), b.h());
    let mut p = Painter::raw(b);
    let mut p = p.sub(Rect::new(cx, cy, bw, bh));
    p.rrect(842, 636, 56, 30, 8, MILK, 255);
    p.rrect(852, 642, 14, 10, 4, ESPRESSO, 255);
    p.rrect(874, 650, 12, 8, 4, ESPRESSO, 255);
    p.rrect(892, 630, 22, 22, 6, MILK, 255);
    p.circle(902, 638, 6, ESPRESSO, 255);
    p.rrect(890, 626, 6, 8, 3, ESPRESSO, 255);
    for lx in [846, 862, 878] {
        p.fill(lx, 664, 6, 14, ESPRESSO);
    }
}

/// Night pasture: a plum-to-navy sky, stars, a moon with a halo, dark hills, the cow asleep.
fn draw_night(b: &mut Buffer) {
    let (top, mid, bot) = (
        rgb(0x1B, 0x1A, 0x2E),
        rgb(0x2C, 0x24, 0x40),
        rgb(0x4B, 0x35, 0x52),
    );
    let h = b.h() as u32;
    let split = h * 55 / 100;
    for y in 0..h {
        let c = if y < split {
            lerp(top, mid, y, split)
        } else {
            lerp(mid, bot, y - split, h - split)
        };
        b.fill_rect(0, y as i32, b.w(), 1, c);
    }
    // Stars: a fixed pseudo-random sprinkle over the upper sky, a few brighter ones.
    let mut seed: u32 = 0x6d69_6c6b; // "milk"
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let f = Fit::of(b);
    for i in 0..140 {
        let x = (next() % screen_w() as u32) as i64;
        let y = f.top((next() % 470) as i64);
        let a = (60 + next() % 180) as u8;
        let big = i % 11 == 0;
        let r16 = if big { 20 } else { 9 };
        disc(b, 16 * x + 8, 16 * y + 8, r16, rgb(0xFF, 0xF6, 0xE4), a);
    }
    // Moon and halo where the sun is in the day.
    f.sky(b, 720, 60, 200, rgb(0xFF, 0xF3, 0xDA), a(90));
    f.sky(b, 760, 100, 120, rgb(0xF5, 0xEC, 0xD6), 255);
    f.sky(b, 790, 110, 30, rgb(0xE4, 0xD9, 0xC0), 255); // craters
    f.sky(b, 812, 160, 22, rgb(0xE4, 0xD9, 0xC0), 255);
    f.sky(b, 770, 150, 14, rgb(0xE8, 0xDE, 0xC6), 255);
    // Hills, night greens.
    f.hill(b, -120, 500, 1040, rgb(0x3F, 0x55, 0x45), 255);
    f.hill(b, 380, 590, 1100, rgb(0x35, 0x4B, 0x3C), 255);
    f.hill(b, 760, 650, 700, rgb(0x2C, 0x41, 0x34), 255);
    // The cow, lying down asleep: body on the grass, legs tucked, head resting, eyes shut.
    let (cx, cy) = f.cow();
    let (bw, bh) = (b.w(), b.h());
    let mut p = Painter::raw(b);
    let mut p = p.sub(Rect::new(cx, cy, bw, bh));
    let cow = rgb(0xE9, 0xE2, 0xD6);
    let spot = rgb(0x2A, 0x22, 0x1E);
    p.rrect(842, 650, 56, 26, 10, cow, 255);
    p.rrect(852, 655, 14, 9, 4, spot, 255);
    p.rrect(874, 662, 12, 7, 4, spot, 255);
    p.rrect(892, 656, 22, 20, 7, cow, 255); // head down low
    p.fill(900, 664, 6, 2, spot); // closed eye
    p.rrect(890, 652, 6, 7, 3, spot, 255);
    p.rrect(846, 672, 12, 5, 2, cow, 255); // tucked legs
    p.rrect(878, 672, 12, 5, 2, cow, 255);
    // z z z
    let z = rgb(0xF5, 0xEC, 0xD6);
    p.mono_a(918, 632, "z", z, 200, 1);
    p.mono_a(928, 618, "z", z, 170, 1);
    p.mono_a(940, 600, "Z", z, 140, 1);
}

/// Oat: a quiet cream-to-oat gradient with one soft milk glow.
fn draw_oat(b: &mut Buffer) {
    let (top, bot) = (rgb(0xFB, 0xF4, 0xE6), rgb(0xE6, 0xD9, 0xC2));
    let h = b.h() as u32;
    for y in 0..h {
        b.fill_rect(0, y as i32, b.w(), 1, lerp(top, bot, y, h));
    }
    let f = Fit::of(b);
    f.sky(b, 620, -220, 760, MILK, a(350));
}

/// One box-blur pass of radius `r` along a line of `n` pixels (`get`/`set` by index), edges
/// clamped. Channels are summed separately.
fn blur_line(line: &mut [u32], tmp: &mut [u32], r: usize) {
    let n = line.len();
    if n == 0 {
        return;
    }
    let win = (2 * r + 1) as u32;
    let at = |i: isize| line[i.clamp(0, n as isize - 1) as usize];
    let (mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32);
    for k in -(r as isize)..=(r as isize) {
        let p = at(k);
        sr += (p >> 16) & 0xFF;
        sg += (p >> 8) & 0xFF;
        sb += p & 0xFF;
    }
    for (i, t) in tmp.iter_mut().enumerate().take(n) {
        *t =
            (((sr + win / 2) / win) << 16) | (((sg + win / 2) / win) << 8) | ((sb + win / 2) / win);
        let add = at(i as isize + r as isize + 1);
        let sub = at(i as isize - r as isize);
        sr = sr + ((add >> 16) & 0xFF) - ((sub >> 16) & 0xFF);
        sg = sg + ((add >> 8) & 0xFF) - ((sub >> 8) & 0xFF);
        sb = sb + (add & 0xFF) - (sub & 0xFF);
    }
    line.copy_from_slice(&tmp[..n]);
}

fn blur_rows(b: &mut Buffer, r: usize) {
    let (w, h, stride) = (b.width(), b.height(), b.stride());
    let mut tmp = alloc::vec![0u32; w.max(h)];
    let px = b.pixels_mut();
    for y in 0..h {
        blur_line(&mut px[y * stride..y * stride + w], &mut tmp, r);
    }
}

fn blur_cols(b: &mut Buffer, r: usize) {
    let (w, h, stride) = (b.width(), b.height(), b.stride());
    let mut col = alloc::vec![0u32; h];
    let mut tmp = alloc::vec![0u32; h];
    let px = b.pixels_mut();
    for x in 0..w {
        for y in 0..h {
            col[y] = px[y * stride + x];
        }
        blur_line(&mut col, &mut tmp, r);
        for y in 0..h {
            px[y * stride + x] = col[y];
        }
    }
}
