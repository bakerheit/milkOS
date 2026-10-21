//! The compositor's drawing kit: a [`Painter`] is a clip rectangle plus a translation over a pixel
//! buffer, and every primitive here clips to it first and then works a row span at a time.
//!
//! Why not just `gfx::DrawTarget`: compositing only redraws damaged rectangles, so every primitive
//! must be cheap when it is mostly clipped away. Anti-aliased rounded corners come from
//! precomputed coverage tables (one per radius, 8x8 supersampled, built once and cached), so a
//! rounded rect costs one table lookup per corner pixel instead of 16 point-in-shape tests, and
//! straight spans are plain fills / copies.
//!
//! Coordinates passed to a painter are *logical*; logical (0, 0) is buffer pixel (`ox`, `oy`).

#![allow(clippy::too_many_arguments)]

use alloc::boxed::Box;
use alloc::vec;

use crate::font::{self, GLYPH_H, GLYPH_W};
use crate::gfx::{Color, DrawTarget, Rect, blend};
use crate::sync::SpinLock;

use super::buffer::Buffer;

// ---- corner coverage tables --------------------------------------------------------------

const MAX_R: usize = 128;
static CORNERS: SpinLock<[Option<&'static [u8]>; MAX_R + 1]> = SpinLock::new([None; MAX_R + 1]);

/// Coverage (0..=255) of the pixels of a top-left corner box of radius `r`: `r * r` entries,
/// row-major, pixel (i, j) at `j * r + i`, the circle's centre at (r, r).
pub fn corner(r: i32) -> &'static [u8] {
    let r = (r.max(0) as usize).min(MAX_R);
    let mut c = CORNERS.lock();
    if let Some(t) = c[r] {
        return t;
    }
    let mut t = vec![0u8; r * r];
    let r16 = 16 * r as i64;
    for j in 0..r {
        for i in 0..r {
            let mut n = 0u32;
            for sy in 0..8 {
                for sx in 0..8 {
                    let dx = r16 - (16 * i as i64 + 2 * sx + 1);
                    let dy = r16 - (16 * j as i64 + 2 * sy + 1);
                    if dx * dx + dy * dy <= r16 * r16 {
                        n += 1;
                    }
                }
            }
            t[j * r + i] = ((n * 255 + 32) / 64) as u8;
        }
    }
    let t: &'static [u8] = Box::leak(t.into_boxed_slice());
    c[r] = Some(t);
    t
}

/// Coverage of pixel (i, j) of a `w` x `h` rounded rect with corner radius `r` (table `t`).
#[inline]
fn rr_cov(t: &[u8], r: i32, w: i32, h: i32, i: i32, j: i32) -> u32 {
    if i < 0 || j < 0 || i >= w || j >= h {
        return 0;
    }
    let ci = if i < r {
        i
    } else if i >= w - r {
        w - 1 - i
    } else {
        return 255;
    };
    let cj = if j < r {
        j
    } else if j >= h - r {
        h - 1 - j
    } else {
        return 255;
    };
    t[(cj * r + ci) as usize] as u32
}

#[inline]
fn mul_a(cov: u32, alpha: u8) -> u8 {
    ((cov * alpha as u32 + 127) / 255) as u8
}

/// Intersection of two rects (may be empty).
pub fn intersect(a: &Rect, b: &Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
}

/// Does `outer` contain all of `inner`?
pub fn contains(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

pub fn grow(r: &Rect, n: i32) -> Rect {
    Rect::new(r.x - n, r.y - n, r.w + 2 * n, r.h + 2 * n)
}

pub fn hit(r: &Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h
}

// ---- the painter ---------------------------------------------------------------------------

pub struct Painter<'a> {
    px: &'a mut [u32],
    stride: usize,
    /// Buffer coordinates, inside the buffer.
    clip: Rect,
    ox: i32,
    oy: i32,
}

impl<'a> Painter<'a> {
    /// Paint on all of `buf`, logical = buffer coordinates.
    pub fn new(buf: &'a mut Buffer) -> Painter<'a> {
        let (w, h, stride) = (buf.w(), buf.h(), buf.stride());
        Painter {
            px: buf.pixels_mut(),
            stride,
            clip: Rect::new(0, 0, w, h),
            ox: 0,
            oy: 0,
        }
    }

    /// A painter on the same pixels, clipped further to `r` (logical).
    pub fn clipped(&mut self, r: Rect) -> Painter<'_> {
        let b = intersect(
            &self.clip,
            &Rect::new(r.x + self.ox, r.y + self.oy, r.w, r.h),
        );
        Painter {
            px: &mut *self.px,
            stride: self.stride,
            clip: b,
            ox: self.ox,
            oy: self.oy,
        }
    }

    /// A painter whose logical (0, 0) is this painter's (`r.x`, `r.y`), clipped to `r`.
    pub fn sub(&mut self, r: Rect) -> Painter<'_> {
        let mut p = self.clipped(r);
        p.ox += r.x;
        p.oy += r.y;
        p
    }

    /// The clip rectangle in logical coordinates.
    pub fn clip(&self) -> Rect {
        Rect::new(
            self.clip.x - self.ox,
            self.clip.y - self.oy,
            self.clip.w,
            self.clip.h,
        )
    }

    /// Logical rect -> clipped buffer rect (None if nothing visible).
    #[inline]
    fn buf_rect(&self, x: i32, y: i32, w: i32, h: i32) -> Option<Rect> {
        let r = intersect(&self.clip, &Rect::new(x + self.ox, y + self.oy, w, h));
        (!r.is_empty()).then_some(r)
    }

    #[inline]
    fn span(&mut self, by: i32, x0: i32, x1: i32) -> &mut [u32] {
        let base = by as usize * self.stride;
        &mut self.px[base + x0 as usize..base + x1 as usize]
    }

    // ---- fills ------------------------------------------------------------------------------

    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        for by in b.y..b.y + b.h {
            self.span(by, b.x, b.x + b.w).fill(c);
        }
    }

    pub fn fill_a(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color, alpha: u8) {
        if alpha == 255 {
            return self.fill(x, y, w, h, c);
        }
        if alpha == 0 {
            return;
        }
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        for by in b.y..b.y + b.h {
            for p in self.span(by, b.x, b.x + b.w) {
                *p = blend(*p, c, alpha);
            }
        }
    }

    /// Filled anti-aliased rounded rect, blended at `alpha`.
    pub fn rrect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Color, alpha: u8) {
        let r = r.clamp(0, w.min(h) / 2);
        if r == 0 {
            return self.fill_a(x, y, w, h, c, alpha);
        }
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        let t = corner(r);
        let (rx, ry) = (x + self.ox, y + self.oy); // rect origin, buffer coords
        for by in b.y..b.y + b.h {
            let j = by - ry;
            let corner_row = if j < r {
                Some(j)
            } else if j >= h - r {
                Some(h - 1 - j)
            } else {
                None
            };
            let (x0, x1) = (b.x, b.x + b.w);
            match corner_row {
                None => {
                    let s = self.span(by, x0, x1);
                    if alpha == 255 {
                        s.fill(c);
                    } else {
                        for p in s {
                            *p = blend(*p, c, alpha);
                        }
                    }
                }
                Some(cj) => {
                    let trow = &t[(cj * r) as usize..(cj * r + r) as usize];
                    let s = self.span(by, x0, x1);
                    for (k, p) in s.iter_mut().enumerate() {
                        let i = x0 + k as i32 - rx;
                        let cov = if i < r {
                            trow[i as usize] as u32
                        } else if i >= w - r {
                            trow[(w - 1 - i) as usize] as u32
                        } else {
                            255
                        };
                        let a = mul_a(cov, alpha);
                        if a == 255 {
                            *p = c;
                        } else if a != 0 {
                            *p = blend(*p, c, a);
                        }
                    }
                }
            }
        }
    }

    pub fn rrect_r(&mut self, r: Rect, radius: i32, c: Color, alpha: u8) {
        self.rrect(r.x, r.y, r.w, r.h, radius, c, alpha);
    }

    /// Rounded rect outline `th` pixels thick, drawn inside the bounds.
    pub fn rrect_stroke(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        r: i32,
        th: i32,
        c: Color,
        alpha: u8,
    ) {
        if w <= 0 || h <= 0 || th <= 0 {
            return;
        }
        let r = r.clamp(0, w.min(h) / 2);
        let th = th.min(w.min(h) / 2).max(1);
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        let to = corner(r);
        let ri = (r - th).max(0);
        let ti = corner(ri);
        let (wi, hi) = (w - 2 * th, h - 2 * th);
        let (rx, ry) = (x + self.ox, y + self.oy);
        let band = r.max(th);
        for by in b.y..b.y + b.h {
            let j = by - ry;
            let edge_row = j < band || j >= h - band;
            let (x0, x1) = (b.x, b.x + b.w);
            if !edge_row {
                // Only the left and right `th` columns.
                for i in [0..th, w - th..w] {
                    let s0 = (rx + i.start).max(x0);
                    let s1 = (rx + i.end).min(x1);
                    if s0 < s1 {
                        for p in self.span(by, s0, s1) {
                            *p = if alpha == 255 { c } else { blend(*p, c, alpha) };
                        }
                    }
                }
                continue;
            }
            let full_row = j < th || j >= h - th;
            for bx in x0..x1 {
                let i = bx - rx;
                let in_corner_cols = i < band || i >= w - band;
                let cov = if !in_corner_cols {
                    if full_row { 255 } else { 0 }
                } else {
                    let o = rr_cov(to, r, w, h, i, j);
                    let inn = if wi > 0 && hi > 0 {
                        rr_cov(ti, ri, wi, hi, i - th, j - th)
                    } else {
                        0
                    };
                    o.saturating_sub(inn)
                };
                if cov == 0 {
                    continue;
                }
                let a = mul_a(cov, alpha);
                let base = by as usize * self.stride + bx as usize;
                let p = &mut self.px[base];
                *p = if a == 255 { c } else { blend(*p, c, a) };
            }
        }
    }

    /// Filled anti-aliased circle of diameter `d` with its bounding box at (x, y).
    pub fn circle(&mut self, x: i32, y: i32, d: i32, c: Color, alpha: u8) {
        self.rrect(x, y, d, d, d / 2, c, alpha);
    }

    /// Circle outline `th` thick.
    pub fn ring(&mut self, x: i32, y: i32, d: i32, th: i32, c: Color, alpha: u8) {
        self.rrect_stroke(x, y, d, d, d / 2, th, c, alpha);
    }

    /// The stacked-rect shadow of a rounded rect (layers from `theme::SHADOW_*`).
    pub fn shadow(&mut self, r: Rect, radius: i32, layers: &[(i32, i32, u8)]) {
        for &(g, dy, a) in layers {
            self.rrect(
                r.x - g,
                r.y - g + dy,
                r.w + 2 * g,
                r.h + 2 * g,
                radius + g,
                super::theme::ESPRESSO,
                a,
            );
        }
    }

    /// A window's shadow (SPEC: Shadows, window), drawn only where the window itself won't
    /// cover it: around its opaque core, so most of the area costs nothing.
    pub fn shadow_window(&mut self, r: Rect) {
        let core = Rect::new(
            r.x,
            r.y + super::theme::R_WINDOW,
            r.w,
            r.h - 2 * super::theme::R_WINDOW,
        );
        let c = self.clip();
        let (cr, cb) = (c.x + c.w, c.y + c.h);
        let (kr, kb) = (core.x + core.w, core.y + core.h);
        let parts = [
            Rect::new(c.x, c.y, c.w, core.y - c.y),
            Rect::new(c.x, kb, c.w, cb - kb),
            Rect::new(c.x, core.y, core.x - c.x, core.h),
            Rect::new(kr, core.y, cr - kr, core.h),
        ];
        for part in parts {
            let part = intersect(&part, &c);
            if !part.is_empty() {
                self.clipped(part)
                    .shadow(r, super::theme::R_WINDOW, &super::theme::SHADOW_WINDOW);
            }
        }
    }

    /// Blend an ARGB sprite (alpha in the top byte) with its top-left at (x, y).
    pub fn sprite(&mut self, x: i32, y: i32, src: &[u32], w: usize, h: usize) {
        let Some(b) = self.buf_rect(x, y, w as i32, h as i32) else {
            return;
        };
        let (dx0, dy0) = (x + self.ox, y + self.oy);
        for by in b.y..b.y + b.h {
            let srow = &src[(by - dy0) as usize * w..][..w];
            for bx in b.x..b.x + b.w {
                let q = srow[(bx - dx0) as usize];
                let a = (q >> 24) as u8;
                if a != 0 {
                    let p = &mut self.px[by as usize * self.stride + bx as usize];
                    *p = blend(*p, q & 0x00FF_FFFF, a);
                }
            }
        }
    }

    /// Scale all of `src` (nearest neighbour) into the logical rect `dst`, through a
    /// rounded mask of radius `r`, blended at `alpha`. For window animations.
    pub fn blit_scaled_rrect(&mut self, dst: Rect, src: &Buffer, r: i32, alpha: u8) {
        let Some(b) = self.buf_rect(dst.x, dst.y, dst.w, dst.h) else {
            return;
        };
        let r = r.clamp(0, dst.w.min(dst.h) / 2);
        let t = corner(r);
        let (dx0, dy0) = (dst.x + self.ox, dst.y + self.oy);
        let (sw, sh) = (src.w(), src.h());
        for by in b.y..b.y + b.h {
            let j = by - dy0;
            let sy = (j * sh / dst.h).min(sh - 1);
            let srow = src.row(sy as usize);
            let base = by as usize * self.stride;
            for bx in b.x..b.x + b.w {
                let i = bx - dx0;
                let cov = rr_cov(t, r, dst.w, dst.h, i, j);
                let a = mul_a(cov, alpha);
                if a == 0 {
                    continue;
                }
                let q = srow[((i * sw / dst.w).min(sw - 1)) as usize];
                let p = &mut self.px[base + bx as usize];
                *p = if a == 255 { q } else { blend(*p, q, a) };
            }
        }
    }

    /// Nearest-neighbour scale all of `src` into the logical rect `dst` (tiny previews).
    pub fn blit_scaled(&mut self, dst: Rect, src: &Buffer) {
        let Some(b) = self.buf_rect(dst.x, dst.y, dst.w, dst.h) else {
            return;
        };
        let (dx0, dy0) = (dst.x + self.ox, dst.y + self.oy);
        let (sw, sh) = (src.w(), src.h());
        for by in b.y..b.y + b.h {
            let sy = ((by - dy0) * sh / dst.h).min(sh - 1);
            let srow = src.row(sy as usize);
            let base = by as usize * self.stride;
            for bx in b.x..b.x + b.w {
                let sx = ((bx - dx0) * sw / dst.w).min(sw - 1);
                self.px[base + bx as usize] = srow[sx as usize];
            }
        }
    }

    // ---- blits ------------------------------------------------------------------------------

    /// Copy the `w` x `h` block at (sx, sy) of `src` to logical (x, y).
    pub fn blit(&mut self, x: i32, y: i32, src: &Buffer, sx: i32, sy: i32, w: i32, h: i32) {
        let w = w.min(src.w() - sx);
        let h = h.min(src.h() - sy);
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        let (dx0, dy0) = (x + self.ox, y + self.oy);
        for by in b.y..b.y + b.h {
            let srow = src.row((sy + by - dy0) as usize);
            let s0 = (sx + b.x - dx0) as usize;
            let s = &srow[s0..s0 + b.w as usize];
            self.span(by, b.x, b.x + b.w).copy_from_slice(s);
        }
    }

    /// Copy a `w` x `h` block of raw pixels (`stride` per row) to logical (x, y).
    pub fn blit_raw(&mut self, x: i32, y: i32, src: &[u32], stride: usize, w: i32, h: i32) {
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        let (dx0, dy0) = (x + self.ox, y + self.oy);
        for by in b.y..b.y + b.h {
            let s0 = (by - dy0) as usize * stride + (b.x - dx0) as usize;
            let s = &src[s0..s0 + b.w as usize];
            self.span(by, b.x, b.x + b.w).copy_from_slice(s);
        }
    }

    /// Copy the same rectangle from a buffer laid out like the one we paint on (a screen-sized
    /// wallpaper under the back buffer): source pixel = destination buffer pixel.
    pub fn blit_same(&mut self, r: Rect, src: &Buffer) {
        self.blit(r.x, r.y, src, r.x + self.ox, r.y + self.oy, r.w, r.h);
    }

    /// Blit the `w` x `h` block at (sx, sy) of `src` to (x, y) through a rounded-rect mask of
    /// radius `r`, blended at `alpha`.
    pub fn blit_rrect(
        &mut self,
        x: i32,
        y: i32,
        src: &Buffer,
        sx: i32,
        sy: i32,
        w: i32,
        h: i32,
        r: i32,
        alpha: u8,
    ) {
        let w = w.min(src.w() - sx);
        let h = h.min(src.h() - sy);
        let r = r.clamp(0, w.min(h) / 2);
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        let t = corner(r);
        let (dx0, dy0) = (x + self.ox, y + self.oy);
        for by in b.y..b.y + b.h {
            let j = by - dy0;
            let srow = src.row((sy + j) as usize);
            let s0 = (sx + b.x - dx0) as usize;
            let s = &srow[s0..s0 + b.w as usize];
            let corner_row = if j < r {
                Some(j)
            } else if j >= h - r {
                Some(h - 1 - j)
            } else {
                None
            };
            let (x0, x1) = (b.x, b.x + b.w);
            match (corner_row, alpha) {
                (None, 255) => self.span(by, x0, x1).copy_from_slice(s),
                (None, a) => {
                    for (p, &q) in self.span(by, x0, x1).iter_mut().zip(s) {
                        *p = blend(*p, q, a);
                    }
                }
                (Some(cj), _) => {
                    let trow = &t[(cj * r) as usize..(cj * r + r) as usize];
                    for (k, (p, &q)) in self.span(by, x0, x1).iter_mut().zip(s).enumerate() {
                        let i = x0 + k as i32 - dx0;
                        let cov = if i < r {
                            trow[i as usize] as u32
                        } else if i >= w - r {
                            trow[(w - 1 - i) as usize] as u32
                        } else {
                            255
                        };
                        let a = mul_a(cov, alpha);
                        if a == 255 {
                            *p = q;
                        } else if a != 0 {
                            *p = blend(*p, q, a);
                        }
                    }
                }
            }
        }
    }

    // ---- text -------------------------------------------------------------------------------

    /// Draw `s` at (x, y), `scale`x, blended at `alpha`. Returns the x after the last glyph.
    pub fn text_a(&mut self, x: i32, y: i32, s: &str, c: Color, alpha: u8, scale: i32) -> i32 {
        let gw = GLYPH_W as i32 * scale;
        let gh = GLYPH_H as i32 * scale;
        let clip = self.clip();
        let mut cx = x;
        for ch in s.chars() {
            if cx >= clip.x + clip.w {
                break;
            }
            if cx + gw > clip.x && y + gh > clip.y && y < clip.y + clip.h && ch != ' ' {
                self.glyph(cx, y, ch, c, alpha, scale);
            }
            cx += gw;
        }
        x + font::str_width(s, scale)
    }

    fn glyph(&mut self, x: i32, y: i32, ch: char, c: Color, alpha: u8, scale: i32) {
        let g = font::glyph(ch);
        let cl = self.clip;
        let (bx, by) = (x + self.ox, y + self.oy);
        for (row, &bits) in g.iter().enumerate() {
            if bits == 0 {
                continue;
            }
            let py0 = by + row as i32 * scale;
            for sy in 0..scale {
                let py = py0 + sy;
                if py < cl.y || py >= cl.y + cl.h {
                    continue;
                }
                let base = py as usize * self.stride;
                for col in 0..GLYPH_W as i32 {
                    if bits & (0x80 >> col) == 0 {
                        continue;
                    }
                    let px0 = bx + col * scale;
                    for sx in 0..scale {
                        let px = px0 + sx;
                        if px < cl.x || px >= cl.x + cl.w {
                            continue;
                        }
                        let p = &mut self.px[base + px as usize];
                        *p = if alpha == 255 { c } else { blend(*p, c, alpha) };
                    }
                }
            }
        }
    }

    pub fn text(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.text_a(x, y, s, c, 255, 1)
    }

    /// Faux bold (SPEC): the glyphs twice, the second copy 1px to the right.
    pub fn bold(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.text_a(x, y, s, c, 255, 1);
        self.text_a(x + 1, y, s, c, 255, 1)
    }

    /// Scaled text, optionally faux bold. 2x and 4x are drawn smooth (fontx: Scale2x +
    /// anti-aliasing from the Unifont bitmaps); other sizes are pixel-doubled.
    pub fn text_x(&mut self, x: i32, y: i32, s: &str, c: Color, scale: i32, bold: bool) -> i32 {
        if super::fontx::smooth_scale(scale) {
            return self.text_smooth(x, y, s, c, scale, bold);
        }
        let end = self.text_a(x, y, s, c, 255, scale);
        if bold {
            self.text_a(x + scale.max(1), y, s, c, 255, scale);
        }
        end
    }

    fn text_smooth(&mut self, x: i32, y: i32, s: &str, c: Color, scale: i32, bold: bool) -> i32 {
        let (gw, gh) = (super::fontx::width(scale), GLYPH_H as i32 * scale);
        let clip = self.clip();
        let mut cx = x;
        for ch in s.chars() {
            let adv = GLYPH_W as i32 * scale;
            if ch != ' '
                && cx + gw > clip.x
                && cx < clip.x + clip.w
                && y + gh > clip.y
                && y < clip.y + clip.h
            {
                let m = super::fontx::glyph(ch, scale, bold);
                let Some(b) = self.buf_rect(cx, y, gw, gh) else {
                    cx += adv;
                    continue;
                };
                let (bx0, by0) = (cx + self.ox, y + self.oy);
                for by in b.y..b.y + b.h {
                    let mrow = &m[((by - by0) * gw) as usize..][..gw as usize];
                    let base = by as usize * self.stride;
                    for bx in b.x..b.x + b.w {
                        let a = mrow[(bx - bx0) as usize];
                        if a != 0 {
                            let p = &mut self.px[base + bx as usize];
                            *p = if a == 255 { c } else { blend(*p, c, a) };
                        }
                    }
                }
            }
            cx += adv;
        }
        x + font::str_width(s, scale)
    }

    /// Text centred in `[x, x+w)`.
    pub fn text_center(&mut self, x: i32, w: i32, y: i32, s: &str, c: Color, bold: bool) {
        let tx = x + (w - font::str_width(s, 1)) / 2;
        if bold {
            self.bold(tx, y, s, c);
        } else {
            self.text(tx, y, s, c);
        }
    }

    /// Text right-aligned so it ends at `xr`.
    pub fn text_right(&mut self, xr: i32, y: i32, s: &str, c: Color, scale: i32, bold: bool) {
        let tx = xr - font::str_width(s, scale) - if bold { scale } else { 0 };
        self.text_x(tx, y, s, c, scale, bold);
    }
}

/// Box-filter downscale of all of `src` into all of `dst` (used for Spaces thumbnails).
pub fn downscale(src: &Buffer, dst: &mut Buffer) {
    let (sw, sh) = (src.w() as usize, src.h() as usize);
    let (dw, dh) = (dst.w() as usize, dst.h() as usize);
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return;
    }
    let dstride = dst.stride();
    let out = dst.pixels_mut();
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1);
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for y in y0..y1 {
                for &p in &src.row(y)[x0..x1] {
                    r += (p >> 16) & 0xFF;
                    g += (p >> 8) & 0xFF;
                    b += p & 0xFF;
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            out[dy * dstride + dx] = ((r / n) << 16) | ((g / n) << 8) | (b / n);
        }
    }
}
