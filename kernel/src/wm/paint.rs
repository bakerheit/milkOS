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
use alloc::vec::Vec;

use crate::font::{self, GLYPH_H, GLYPH_W};
use crate::gfx::{Color, DrawTarget, Rect, blend};
use crate::sync::SpinLock;
use crate::sysfont::{Font, Weight};

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
    /// Run colours through the theme's token table (`theme::tc`). Off for the wallpaper.
    themed: bool,
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
            themed: true,
        }
    }

    /// Like [`new`](Self::new), but colours are used as given (no theme mapping).
    pub fn raw(buf: &'a mut Buffer) -> Painter<'a> {
        let mut p = Painter::new(buf);
        p.themed = false;
        p
    }

    #[inline]
    fn col(&self, c: Color) -> Color {
        if self.themed { super::theme::tc(c) } else { c }
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
            themed: self.themed,
        }
    }

    /// The same painter with theme mapping switched off (colours used as given): for
    /// previews of the other theme, wallpaper thumbnails and such.
    pub fn unthemed(&mut self) -> Painter<'_> {
        let mut p = self.clipped(self.clip());
        p.themed = false;
        p
    }

    /// A painter whose logical (0, 0) is this painter's (`r.x`, `r.y`), clipped to `r`.
    pub fn sub(&mut self, r: Rect) -> Painter<'_> {
        let mut p = self.clipped(r);
        p.ox += r.x;
        p.oy += r.y;
        p
    }

    /// Where this painter's logical (0, 0) is in its buffer.
    pub fn origin(&self) -> (i32, i32) {
        (self.ox, self.oy)
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
        let c = self.col(c);
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        for by in b.y..b.y + b.h {
            self.span(by, b.x, b.x + b.w).fill(c);
        }
    }

    pub fn fill_a(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color, alpha: u8) {
        let c = self.col(c);
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
        let c = self.col(c);
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
        let c = self.col(c);
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
                super::theme::INK,
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

    /// One row of a genie: source row `sy` of `src` stretched (nearest) across logical
    /// `x0..x1` at row `y`, blended at `alpha`; the window's rounded corners (`radius`,
    /// in source pixels) stay transparent.
    #[allow(clippy::too_many_arguments)]
    pub fn blit_span(
        &mut self,
        y: i32,
        x0: i32,
        x1: i32,
        src: &Buffer,
        sy: i32,
        radius: i32,
        alpha: u8,
    ) {
        let w = x1 - x0;
        if w <= 0 {
            return;
        }
        let Some(b) = self.buf_rect(x0, y, w, 1) else {
            return;
        };
        let (sw, sh) = (src.w(), src.h());
        let sy = sy.clamp(0, sh - 1);
        let srow = src.row(sy as usize);
        let dx0 = x0 + self.ox;
        let base = b.y as usize * self.stride;
        // How far in from each side the corner cuts at this source row.
        let inset = |r: i32, d: i32| -> i32 {
            if d >= r {
                return 0;
            }
            let dy = r - d;
            let mut i = 0;
            while i < r && (r - i) * (r - i) + dy * dy > r * r {
                i += 1;
            }
            i
        };
        let cut = inset(radius, sy).max(inset(radius, sh - 1 - sy));
        for bx in b.x..b.x + b.w {
            let i = bx - dx0;
            let sx = (i * sw / w).min(sw - 1);
            if sx < cut || sx >= sw - cut {
                continue;
            }
            let q = srow[sx as usize];
            let p = &mut self.px[base + bx as usize];
            *p = if alpha == 255 { q } else { blend(*p, q, alpha) };
        }
    }

    /// One row of a squeezed window (the genie), filtered so nothing is dropped: source rows
    /// `sy0..sy1` (16.16, fractional) are averaged (a box over the rows they cover; under one
    /// row, the two nearest rows blended), then the averaged row is resampled across logical
    /// `x0..x1` (1/256 px, fractional, at the row's centre) with a box filter. The ends are
    /// anti-aliased: `slope` is how far (1/256 px) the left and right edges move across this
    /// one row, and each end ramps over that width (at least a pixel), so a shallow edge fades
    /// over the pixels it crosses instead of stepping. The window's rounded corners (`radius`)
    /// count as empty. `acc` is scratch (one entry per source column).
    pub fn blit_span_filtered(
        &mut self,
        y: i32,
        x0: i64,
        x1: i64,
        src: &Buffer,
        sy0: i64,
        sy1: i64,
        slope: (i64, i64),
        radius: i32,
        alpha: u8,
        acc: &mut Vec<[u32; 4]>,
    ) {
        if x1 <= x0 {
            return;
        }
        let (wl, wr) = (slope.0.abs().max(256), slope.1.abs().max(256));
        let (px0, px1) = (
            ((x0 - wl / 2) >> 8) as i32,
            ((x1 + wr / 2 + 255) >> 8) as i32,
        );
        let Some(b) = self.buf_rect(px0, y, px1 - px0, 1) else {
            return;
        };
        let (sw, sh) = (src.w() as usize, src.h());
        // Close to 1:1 both ways: the plain nearest span is exact enough (and cheaper).
        let wide = x1 - x0;
        let rows = sy1 - sy0;
        if (wide - sw as i64 * 256).abs() * 100 < sw as i64 * 256
            && (rows - 65536).abs() * 100 < 65536
            && wl == 256
            && wr == 256
        {
            let (a, z) = (((x0 + 128) >> 8) as i32, ((x1 + 128) >> 8) as i32);
            return self.blit_span(y, a, z, src, (sy0 >> 16) as i32, radius, alpha);
        }
        // How far in from each side the rounded corner cuts at source row `r`.
        let cut = |r: i32| -> usize {
            let inset = |d: i32| -> i32 {
                if d >= radius {
                    return 0;
                }
                let dy = radius - d;
                let mut i = 0;
                while i < radius && (radius - i) * (radius - i) + dy * dy > radius * radius {
                    i += 1;
                }
                i
            };
            inset(r).max(inset(sh - 1 - r)) as usize
        };
        // 1. Vertical: weights (1/256) of the source rows this output row covers.
        acc.clear();
        acc.resize(sw, [0; 4]);
        let add_row = |r: i32, wr: u32, acc: &mut Vec<[u32; 4]>| {
            if wr == 0 || r < 0 || r >= sh {
                return;
            }
            let row = src.row(r as usize);
            let c = cut(r);
            for (x, q) in row.iter().enumerate().take(sw - c).skip(c) {
                let e = &mut acc[x];
                e[0] += (q >> 16 & 0xff) * wr;
                e[1] += (q >> 8 & 0xff) * wr;
                e[2] += (q & 0xff) * wr;
                e[3] += wr;
            }
        };
        let total_w: u32 = if rows < 65536 {
            // Under a row: the two nearest rows, blended by where the centre falls.
            let c = (sy0 + sy1) / 2 - 32768;
            let (r0, f) = ((c >> 16) as i32, ((c & 0xffff) >> 8) as u32);
            add_row(r0.max(0), 256 - f, acc);
            add_row((r0 + 1).min(sh - 1), f, acc);
            256
        } else {
            let mut t = 0;
            let mut r = (sy0 >> 16) as i32;
            while ((r as i64) << 16) < sy1 && r < sh {
                let a = sy0.max((r as i64) << 16);
                let z = sy1.min(((r + 1) as i64) << 16);
                let wr = ((z - a) >> 8) as u32;
                add_row(r, wr, acc);
                t += wr;
                r += 1;
            }
            t.max(1)
        };
        // 2. Horizontal: each output pixel averages the source columns under it. All the
        // per-pixel maths is multiplies (one reciprocal per pixel); the ramps only run at the
        // two ends.
        let dx0 = x0 + ((self.ox as i64) << 8);
        let dx1 = dx0 + wide;
        let base = b.y as usize * self.stride;
        let swf = sw as i64 * 256; // source width, 1/256 px
        // Source 1/256 px per output 1/256 px, 16.16.
        let scale = (swf << 16) / wide;
        // Within 2% of 1:1 across: one source column per pixel (nearest) is exact enough.
        let near = (scale - 65536).abs() < 65536 / 50;
        let (lin, rin) = (dx0 + wl / 2, dx1 - wr / 2);
        for bx in b.x..b.x + b.w {
            let (p0, p1) = ((bx as i64) << 8, ((bx + 1) as i64) << 8);
            // Edge ramps (0..256) at the pixel's centre.
            let c = p0 + 128;
            let lf = if c >= lin {
                256
            } else {
                ((c - (dx0 - wl / 2)) * 256 / wl).clamp(0, 256)
            };
            let rf = if c <= rin {
                256
            } else {
                ((dx1 + wr / 2 - c) * 256 / wr).clamp(0, 256)
            };
            let cov = (lf * rf) as u64 >> 8;
            if cov == 0 {
                continue;
            }
            // What the pixel shows: the span under it (clamped to the span at the ends).
            let a = p0.clamp(dx0, dx1 - 1);
            let z = p1.clamp(a + 1, dx1);
            // The source range under [a, z), 1/256 source px.
            let s0 = ((a - dx0) * scale) >> 16;
            let s1 = (((z - dx0) * scale) >> 16).max(s0 + 1);
            let (r, g, bl, w, cw) = if near {
                let e = acc[(((s0 + s1) >> 9) as usize).min(sw - 1)];
                (e[0] as u64, e[1] as u64, e[2] as u64, e[3] as u64, 1u64)
            } else {
                let (mut r, mut g, mut bl, mut w, mut cw) = (0u64, 0u64, 0u64, 0u64, 0u64);
                let mut c = (s0 >> 8) as usize;
                while ((c as i64) << 8) < s1 && c < sw {
                    let ca = s0.max((c as i64) << 8);
                    let cz = s1.min(((c + 1) as i64) << 8);
                    let wc = (cz - ca) as u64;
                    let e = acc[c];
                    r += e[0] as u64 * wc;
                    g += e[1] as u64 * wc;
                    bl += e[2] as u64 * wc;
                    w += e[3] as u64 * wc;
                    cw += wc;
                    c += 1;
                }
                (r, g, bl, w, cw)
            };
            if w == 0 {
                continue;
            }
            let inv = (1u64 << 40).div_ceil(w);
            let ch = |v: u64| ((v * inv) >> 40).min(255) as u32;
            let q = ch(r) << 16 | ch(g) << 8 | ch(bl);
            // Coverage: the span's share of the pixel x how much of it was window (not a
            // rounded corner), x the fade.
            let full = cw * total_w as u64;
            let k = if w >= full {
                (cov * alpha as u64) >> 8
            } else {
                cov * w * alpha as u64 / (256 * full)
            }
            .min(255) as u8;
            if k == 0 {
                continue;
            }
            let p = &mut self.px[base + bx as usize];
            *p = if k == 255 { q } else { blend(*p, q, k) };
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
        // A source block starting off the source's top or left (a panel hanging off the
        // screen, like a wide magnified dock): start where the source does.
        let (x, sx, w) = if sx < 0 {
            (x - sx, 0, w + sx)
        } else {
            (x, sx, w)
        };
        let (y, sy, h) = if sy < 0 {
            (y - sy, 0, h + sy)
        } else {
            (y, sy, h)
        };
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

    /// [`blit`](Self::blit) the `src` block of `src_buf` to (x, y) with `c` (as given, not
    /// themed) blended over every pixel at `alpha` (the stronger glass, from the normal one).
    pub fn blit_tinted(
        &mut self,
        x: i32,
        y: i32,
        src_buf: &Buffer,
        src: Rect,
        c: Color,
        alpha: u8,
    ) {
        let (sx, sy, w, h) = (src.x, src.y, src.w, src.h);
        let (x, sx, w) = if sx < 0 {
            (x - sx, 0, w + sx)
        } else {
            (x, sx, w)
        };
        let (y, sy, h) = if sy < 0 {
            (y - sy, 0, h + sy)
        } else {
            (y, sy, h)
        };
        let w = w.min(src_buf.w() - sx);
        let h = h.min(src_buf.h() - sy);
        let Some(b) = self.buf_rect(x, y, w, h) else {
            return;
        };
        let (dx0, dy0) = (x + self.ox, y + self.oy);
        for by in b.y..b.y + b.h {
            let srow = src_buf.row((sy + by - dy0) as usize);
            let s0 = (sx + b.x - dx0) as usize;
            let s = &srow[s0..s0 + b.w as usize];
            for (d, &q) in self.span(by, b.x, b.x + b.w).iter_mut().zip(s) {
                *d = blend(q, c, alpha);
            }
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
    //
    // UI text is the system font (crate::sysfont): proportional, anti-aliased, in the family
    // chosen in Settings. `y` is the top of a line box `16 * scale` pixels tall. Grids that
    // need fixed cells (Terminal, Notes' editor, icon glyphs) use the `mono` calls (Unifont).

    /// Draw `s` in `f` with the top of its line box at `y`, blended at `alpha`. Returns the
    /// end x.
    pub fn text_fa(&mut self, x: i32, y: i32, s: &str, c: Color, f: Font, alpha: u8) -> i32 {
        let c = self.col(c);
        let clip = self.clip();
        let lh = f.line_height();
        // Whole line outside the clip (vertically, with room for accents and descenders).
        if y + lh + lh / 4 <= clip.y || y - lh / 4 >= clip.y + clip.h {
            return x + f.width(s);
        }
        let base = y + f.baseline();
        let cl = self.clip;
        let (ox, oy) = (self.ox, self.oy);
        let stride = self.stride;
        let px = &mut *self.px;
        f.layout(x, s, |pen, g, mag| {
            let gx = pen + g.left * mag + ox;
            let gy = base - g.top * mag + oy;
            if gx >= cl.x + cl.w || gx + g.w * mag <= cl.x {
                return;
            }
            if gy >= cl.y + cl.h || gy + g.h * mag <= cl.y {
                return;
            }
            g.spans(mag, |sx, sy, len, cov| {
                let by = gy + sy;
                if by < cl.y || by >= cl.y + cl.h {
                    return;
                }
                let x0 = (gx + sx).max(cl.x);
                let x1 = (gx + sx + len).min(cl.x + cl.w);
                if x0 >= x1 {
                    return;
                }
                let a = if alpha == 255 {
                    cov
                } else {
                    ((cov as u32 * alpha as u32 + 127) / 255) as u8
                };
                let base = by as usize * stride;
                for p in &mut px[base + x0 as usize..base + x1 as usize] {
                    *p = if a == 255 { c } else { blend(*p, c, a) };
                }
            });
        })
    }

    /// Draw `s` in `f` (opaque). Returns the end x.
    pub fn text_f(&mut self, x: i32, y: i32, s: &str, c: Color, f: Font) -> i32 {
        self.text_fa(x, y, s, c, f, 255)
    }

    /// Body text in the UI font, optionally bold. Returns the end x.
    pub fn text_p(&mut self, x: i32, y: i32, s: &str, c: Color, bold: bool) -> i32 {
        self.text_f(x, y, s, c, Font::body(bold))
    }

    pub fn text(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.text_p(x, y, s, c, false)
    }

    pub fn bold(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.text_p(x, y, s, c, true)
    }

    /// UI text at `scale` (line boxes of `16 * scale`), optionally bold. Returns the end x.
    pub fn text_x(&mut self, x: i32, y: i32, s: &str, c: Color, scale: i32, bold: bool) -> i32 {
        self.text_f(x, y, s, c, Font::ui(scale, Weight::bold(bold)))
    }

    /// Body text centred in `[x, x+w)`.
    pub fn text_p_center(&mut self, x: i32, w: i32, y: i32, s: &str, c: Color, bold: bool) {
        self.text_center_f(x, w, y, s, c, Font::body(bold));
    }

    /// Body text centred in `[x, x+w)`.
    pub fn text_center(&mut self, x: i32, w: i32, y: i32, s: &str, c: Color, bold: bool) {
        self.text_center_f(x, w, y, s, c, Font::body(bold));
    }

    /// Text in `f` centred in `[x, x+w)`.
    pub fn text_center_f(&mut self, x: i32, w: i32, y: i32, s: &str, c: Color, f: Font) {
        let tx = x + (w - f.width(s)) / 2;
        self.text_f(tx, y, s, c, f);
    }

    /// Text right-aligned so it ends at `xr`.
    pub fn text_right(&mut self, xr: i32, y: i32, s: &str, c: Color, scale: i32, bold: bool) {
        let f = Font::ui(scale, Weight::bold(bold));
        self.text_f(xr - f.width(s), y, s, c, f);
    }

    /// Monospace Unifont cells (8x16 per char at `scale`), blended at `alpha`: for grids and
    /// icon glyphs, not UI text. Returns the x after the last cell.
    pub fn mono_a(&mut self, x: i32, y: i32, s: &str, c: Color, alpha: u8, scale: i32) -> i32 {
        let c = self.col(c);
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

    /// Monospace Unifont text at 1x (8 px a char). Returns the end x.
    pub fn mono(&mut self, x: i32, y: i32, s: &str, c: Color) -> i32 {
        self.mono_a(x, y, s, c, 255, 1)
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
