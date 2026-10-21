//! The 1.0 wallpaper, "dawn pasture" (SPEC: Wallpaper), drawn once at boot, plus the pre-blurred
//! copies glass panels sample (SPEC: Glass). Integer maths only (the target is soft-float).

use crate::gfx::{Color, DrawTarget, blend, rgb};

use super::buffer::Buffer;
use super::paint::Painter;
use super::theme::*;

pub struct Wallpaper {
    /// W: the sharp wallpaper.
    pub sharp: Buffer,
    /// Wb (box blur r24 x3) with milk blended over at 0.55: the menu bar / dock / toast glass.
    pub glass: Buffer,
    /// Wb with milk at 0.7: launcher and control centre glass.
    pub glass_strong: Buffer,
}

impl Wallpaper {
    pub fn new() -> Option<Wallpaper> {
        let (w, h) = (SCREEN_W as usize, SCREEN_H as usize);
        let mut sharp = Buffer::new(w, h, 0)?;
        draw(&mut sharp);
        let mut glass = Buffer::new(w, h, 0)?;
        copy(&sharp, &mut glass);
        for _ in 0..BLUR_PASSES {
            blur_rows(&mut glass, BLUR_RADIUS);
            blur_cols(&mut glass, BLUR_RADIUS);
        }
        let mut glass_strong = Buffer::new(w, h, 0)?;
        copy(&glass, &mut glass_strong);
        tint(&mut glass, MILK, GLASS_A);
        tint(&mut glass_strong, MILK, GLASS_STRONG_A);
        Some(Wallpaper {
            sharp,
            glass,
            glass_strong,
        })
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
        let hw_in = if inner2 > 0 && r16 > 8 { inner2.isqrt() } else { -1 };
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
    css_circle(b, 700, 40, 240, MILK, a(280));
    css_circle(b, 740, 80, 160, HONEY, 255);
    css_circle(b, -120, 500, 1040, rgb(0xCF, 0xE0, 0xC4), 255);
    css_circle(b, 380, 590, 1100, rgb(0xB9, 0xD3, 0xB0), 255);
    css_circle(b, 760, 650, 700, rgb(0xA5, 0xC7, 0x9F), 255);
    // The tiny cow.
    let mut p = Painter::new(b);
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
        *t = (((sr + win / 2) / win) << 16) | (((sg + win / 2) / win) << 8) | ((sb + win / 2) / win);
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
