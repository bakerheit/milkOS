//! Software 2D graphics: pixel buffers and drawing primitives.
//!
//! Everything draws into a [`DrawTarget`], which is just "a rectangle of 32-bit XRGB pixels with a
//! stride". The screen framebuffer is a [`Surface`]; so is any off-screen back buffer or window
//! backing store a window server wants later. Surfaces blit onto each other with [`DrawTarget::blit`].
//!
//! Pixels are `0x00RRGGBB` (DRM XRGB8888, little endian in memory = B, G, R, X bytes).
//!
//! The bulk loops use volatile 64-bit writes where alignment allows. That keeps them fast, stops
//! LLVM from turning them into `memset`/`memcpy` calls, and is safe while the MMU is off (memory is
//! then Device memory and unaligned accesses fault).
#![allow(clippy::too_many_arguments)] // x, y, w, h, colour, alpha: drawing calls are like that
#![allow(dead_code)] // A drawing API for later users (window server); not all of it is used yet.

use core::ptr::{read_volatile, write_volatile};

/// A pixel: `0x00RRGGBB`.
pub type Color = u32;

/// Build a colour from 8-bit channels.
pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// Blend `src` over `dst` with `alpha` (0 = all dst, 255 = all src). Exact to within 1 LSB.
#[inline]
pub fn blend(dst: Color, src: Color, alpha: u8) -> Color {
    let a = alpha as u32;
    let ia = 255 - a;
    // Red+blue and green are blended in two lanes each. Max lane value is 255*255 + 0x80 < 65536.
    let rb = (src & 0x00FF_00FF) * a + (dst & 0x00FF_00FF) * ia + 0x0080_0080;
    let rb = ((rb + ((rb >> 8) & 0x00FF_00FF)) >> 8) & 0x00FF_00FF;
    let g = (src & 0x0000_FF00) * a + (dst & 0x0000_FF00) * ia + 0x0000_8000;
    let g = ((g + ((g >> 8) & 0x0000_FF00)) >> 8) & 0x0000_FF00;
    rb | g
}

/// Clip a rectangle to a `tw` x `th` target. Returns (x, y, w, h) in target space.
fn clip(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    tw: usize,
    th: usize,
) -> Option<(usize, usize, usize, usize)> {
    let x0 = (x as i64).max(0);
    let y0 = (y as i64).max(0);
    let x1 = (x as i64 + w as i64).min(tw as i64);
    let y1 = (y as i64 + h as i64).min(th as i64);
    if x1 <= x0 || y1 <= y0 {
        None
    } else {
        Some((
            x0 as usize,
            y0 as usize,
            (x1 - x0) as usize,
            (y1 - y0) as usize,
        ))
    }
}

/// Fill `n` pixels at `dst` with `c`, using 64-bit stores when aligned.
///
/// # Safety
/// `dst..dst+n` must be valid for writes.
unsafe fn fill_words(mut dst: *mut u32, mut n: usize, c: Color) {
    unsafe {
        if n > 0 && (dst as usize) & 7 != 0 {
            write_volatile(dst, c);
            dst = dst.add(1);
            n -= 1;
        }
        let wide = ((c as u64) << 32) | c as u64;
        let mut d64 = dst as *mut u64;
        for _ in 0..n / 2 {
            write_volatile(d64, wide);
            d64 = d64.add(1);
        }
        if n & 1 != 0 {
            write_volatile(d64 as *mut u32, c);
        }
    }
}

/// Copy `n` pixels forwards (so `dst <= src` overlap is fine), 64 bits at a time when both
/// pointers are 8-byte aligned.
///
/// # Safety
/// Both ranges must be valid.
unsafe fn copy_words(mut dst: *mut u32, mut src: *const u32, mut n: usize) {
    unsafe {
        if (dst as usize | src as usize) & 7 == 0 {
            let mut d = dst as *mut u64;
            let mut s = src as *const u64;
            for _ in 0..n / 2 {
                write_volatile(d, read_volatile(s));
                d = d.add(1);
                s = s.add(1);
            }
            if n & 1 != 0 {
                write_volatile(d as *mut u32, read_volatile(s as *const u32));
            }
        } else {
            while n > 0 {
                write_volatile(dst, read_volatile(src));
                dst = dst.add(1);
                src = src.add(1);
                n -= 1;
            }
        }
    }
}

/// A rectangle of pixels that can be drawn on. Implement `width`/`height`/`stride`/`pixels`/
/// `pixels_mut` and every primitive comes for free. The trait is object safe, so a window server
/// can hold `&mut dyn DrawTarget`.
pub trait DrawTarget {
    fn width(&self) -> usize;
    fn height(&self) -> usize;
    /// Pixels (not bytes) between the starts of two rows.
    fn stride(&self) -> usize;
    fn pixels(&self) -> &[u32];
    fn pixels_mut(&mut self) -> &mut [u32];

    /// Read a pixel; out of bounds reads as 0.
    fn get_pixel(&self, x: i32, y: i32) -> Color {
        if x < 0 || y < 0 || x as usize >= self.width() || y as usize >= self.height() {
            return 0;
        }
        self.pixels()[y as usize * self.stride() + x as usize]
    }

    /// Write a pixel; out of bounds is ignored.
    fn put_pixel(&mut self, x: i32, y: i32, c: Color) {
        if x < 0 || y < 0 || x as usize >= self.width() || y as usize >= self.height() {
            return;
        }
        let stride = self.stride();
        self.pixels_mut()[y as usize * stride + x as usize] = c;
    }

    /// Blend a pixel over what is there.
    fn blend_pixel(&mut self, x: i32, y: i32, c: Color, alpha: u8) {
        if x < 0 || y < 0 || x as usize >= self.width() || y as usize >= self.height() {
            return;
        }
        let stride = self.stride();
        let p = &mut self.pixels_mut()[y as usize * stride + x as usize];
        *p = blend(*p, c, alpha);
    }

    /// Fill the whole target.
    fn clear(&mut self, c: Color) {
        let (w, h) = (self.width() as i32, self.height() as i32);
        self.fill_rect(0, 0, w, h, c);
    }

    /// Fill a rectangle (clipped).
    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let Some((x, y, w, h)) = clip(x, y, w, h, self.width(), self.height()) else {
            return;
        };
        let stride = self.stride();
        let base = self.pixels_mut().as_mut_ptr();
        for row in y..y + h {
            // SAFETY: clip() keeps the span inside width x height, which the buffer covers.
            unsafe { fill_words(base.add(row * stride + x), w, c) };
        }
    }

    /// Alpha-blend a rectangle over what is there (clipped). `alpha` 0..=255.
    fn fill_rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color, alpha: u8) {
        if alpha == 255 {
            return self.fill_rect(x, y, w, h, c);
        }
        let Some((x, y, w, h)) = clip(x, y, w, h, self.width(), self.height()) else {
            return;
        };
        let stride = self.stride();
        let px = self.pixels_mut();
        for row in y..y + h {
            for p in &mut px[row * stride + x..row * stride + x + w] {
                *p = blend(*p, c, alpha);
            }
        }
    }

    /// Horizontal line of `len` pixels starting at (x, y).
    fn hline(&mut self, x: i32, y: i32, len: i32, c: Color) {
        self.fill_rect(x, y, len, 1, c);
    }

    /// Vertical line of `len` pixels starting at (x, y).
    fn vline(&mut self, x: i32, y: i32, len: i32, c: Color) {
        self.fill_rect(x, y, 1, len, c);
    }

    /// One-pixel rectangle outline.
    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        if w <= 0 || h <= 0 {
            return;
        }
        self.hline(x, y, w, c);
        self.hline(x, y + h - 1, w, c);
        self.vline(x, y, h, c);
        self.vline(x + w - 1, y, h, c);
    }

    /// Copy a `w` x `h` block from raw pixels (`src_stride` pixels per row) to (dx, dy).
    fn blit_raw(&mut self, dx: i32, dy: i32, src: &[u32], src_stride: usize, w: usize, h: usize) {
        let (mut sx, mut sy, mut dx, mut dy, mut w, mut h) =
            (0i32, 0i32, dx, dy, w as i32, h as i32);
        if dx < 0 {
            sx -= dx;
            w += dx;
            dx = 0;
        }
        if dy < 0 {
            sy -= dy;
            h += dy;
            dy = 0;
        }
        w = w.min(self.width() as i32 - dx);
        h = h.min(self.height() as i32 - dy);
        if w <= 0 || h <= 0 {
            return;
        }
        let stride = self.stride();
        let dst = self.pixels_mut().as_mut_ptr();
        for row in 0..h as usize {
            let s = &src[(sy as usize + row) * src_stride + sx as usize..][..w as usize];
            // SAFETY: the destination span was clipped above; the source span is bounds-checked.
            unsafe {
                copy_words(
                    dst.add((dy as usize + row) * stride + dx as usize),
                    s.as_ptr(),
                    w as usize,
                )
            };
        }
    }

    /// Copy the `w` x `h` block at (sx, sy) of `src` to (dx, dy) here. Clips on both sides.
    fn blit(&mut self, dx: i32, dy: i32, src: &dyn DrawTarget, sx: i32, sy: i32, w: i32, h: i32) {
        let (mut sx, mut sy, mut dx, mut dy, mut w, mut h) = (sx, sy, dx, dy, w, h);
        if sx < 0 {
            dx -= sx;
            w += sx;
            sx = 0;
        }
        if sy < 0 {
            dy -= sy;
            h += sy;
            sy = 0;
        }
        w = w.min(src.width() as i32 - sx);
        h = h.min(src.height() as i32 - sy);
        if w <= 0 || h <= 0 {
            return;
        }
        let off = sy as usize * src.stride() + sx as usize;
        self.blit_raw(
            dx,
            dy,
            &src.pixels()[off..],
            src.stride(),
            w as usize,
            h as usize,
        );
    }

    /// Like [`blit_raw`](Self::blit_raw) but the source is ARGB: the top byte of each pixel is its
    /// alpha (0 = transparent, 255 = opaque). For cursors, icons and window shadows.
    fn blit_argb(&mut self, dx: i32, dy: i32, src: &[u32], src_stride: usize, w: usize, h: usize) {
        for row in 0..h {
            for col in 0..w {
                let p = src[row * src_stride + col];
                let a = (p >> 24) as u8;
                if a != 0 {
                    self.blend_pixel(dx + col as i32, dy + row as i32, p & 0x00FF_FFFF, a);
                }
            }
        }
    }

    /// Filled rounded rectangle with anti-aliased corners of radius `r`, blended with `alpha`.
    fn fill_rounded_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: Color, alpha: u8) {
        rounded_rect_impl(self, x, y, w, h, r, None, c, alpha);
    }

    /// Rounded rectangle outline of `thickness` pixels (drawn inside the bounds), blended with `alpha`.
    fn rounded_rect(
        &mut self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        r: i32,
        thickness: i32,
        c: Color,
        alpha: u8,
    ) {
        rounded_rect_impl(self, x, y, w, h, r, Some(thickness.max(1)), c, alpha);
    }

    /// Move the contents of the rectangle up by `dy` pixels (towards y = 0) and fill the freed
    /// strip at the bottom with `fill`. Used for console scrolling.
    fn scroll_up(&mut self, x: i32, y: i32, w: i32, h: i32, dy: i32, fill: Color) {
        let Some((x, y, w, h)) = clip(x, y, w, h, self.width(), self.height()) else {
            return;
        };
        let dy = (dy.max(0) as usize).min(h);
        let stride = self.stride();
        let base = self.pixels_mut().as_mut_ptr();
        for row in y..y + h - dy {
            // SAFETY: both rows are inside the clipped rectangle; dst row < src row so a forward
            // copy is safe even though the ranges are in the same buffer.
            unsafe {
                copy_words(
                    base.add(row * stride + x),
                    base.add((row + dy) * stride + x),
                    w,
                )
            };
        }
        self.fill_rect(x as i32, (y + h - dy) as i32, w as i32, dy as i32, fill);
    }
}

/// Is the sample point (coordinates in eighths of a pixel, `px2`, `py2` relative to the rect origin) inside the
/// w x h rounded rectangle shrunk by `inset` on every side, whose corners have radius `r - inset`?
fn rr_inside(px2: i32, py2: i32, w: i32, h: i32, r: i32, inset: i32) -> bool {
    let (l, t, rt, b) = (8 * inset, 8 * inset, 8 * (w - inset), 8 * (h - inset));
    if px2 < l || py2 < t || px2 >= rt || py2 >= b {
        return false;
    }
    let rad = 8 * (r - inset).max(0);
    // Distance from the nearest corner circle centre, if we are in a corner zone.
    let dx = if px2 < l + rad {
        l + rad - px2
    } else if px2 >= rt - rad {
        px2 - (rt - rad)
    } else {
        0
    };
    let dy = if py2 < t + rad {
        t + rad - py2
    } else if py2 >= b - rad {
        py2 - (b - rad)
    } else {
        0
    };
    dx * dx + dy * dy <= rad * rad
}

/// Coverage 0..=16 of pixel (i, j) (rect-relative) by the inset rounded rect, 4x4 samples.
fn rr_coverage(i: i32, j: i32, w: i32, h: i32, r: i32, inset: i32) -> u32 {
    let mut n = 0;
    for sy in 0..4 {
        for sx in 0..4 {
            // Sample at the centres of a 4x4 grid inside the pixel, in eighths of a pixel.
            if rr_inside(8 * i + 2 * sx + 1, 8 * j + 2 * sy + 1, w, h, r, inset) {
                n += 1;
            }
        }
    }
    n
}

/// Shared by filled (`stroke` = None) and outlined rounded rectangles. Straight edges and the
/// interior go through span fills; only the corner boxes are sampled per pixel.
fn rounded_rect_impl<T: DrawTarget + ?Sized>(
    t: &mut T,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: i32,
    stroke: Option<i32>,
    c: Color,
    alpha: u8,
) {
    if w <= 0 || h <= 0 || alpha == 0 {
        return;
    }
    let half = w.min(h) / 2;
    let r = r.clamp(0, half);
    let st = stroke.map(|s| s.min(half.max(1)));
    // Corner boxes are k x k; for strokes thicker than the radius the ring spills past the arc.
    let k = r.max(st.unwrap_or(0)).min(half);
    let pix = |t: &mut T, i: i32, j: i32| {
        let mut cov = rr_coverage(i, j, w, h, r, 0);
        if let Some(s) = st {
            cov -= rr_coverage(i, j, w, h, r, s).min(cov);
        }
        if cov > 0 {
            t.blend_pixel(x + i, y + j, c, (cov * alpha as u32 / 16) as u8);
        }
    };
    // Corner boxes, mirrored by looping over all four.
    for j in 0..k {
        for i in 0..k {
            pix(t, i, j);
            pix(t, w - 1 - i, j);
            pix(t, i, h - 1 - j);
            pix(t, w - 1 - i, h - 1 - j);
        }
    }
    // Straight parts between the corner boxes.
    let (mw, mh) = (w - 2 * k, h - 2 * k);
    match st {
        None => {
            t.fill_rect_alpha(x + k, y, mw, k, c, alpha); // top
            t.fill_rect_alpha(x + k, y + h - k, mw, k, c, alpha); // bottom
            t.fill_rect_alpha(x, y + k, w, mh, c, alpha); // middle, full width
        }
        Some(s) => {
            t.fill_rect_alpha(x + k, y, mw, s, c, alpha);
            t.fill_rect_alpha(x + k, y + h - s, mw, s, c, alpha);
            t.fill_rect_alpha(x, y + k, s, mh, c, alpha);
            t.fill_rect_alpha(x + w - s, y + k, s, mh, c, alpha);
        }
    }
}

/// An axis-aligned rectangle in pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// True if the two rectangles share at least one pixel.
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.x < o.x + o.w
            && o.x < self.x + self.w
            && self.y < o.y + o.h
            && o.y < self.y + self.h
    }

    /// Smallest rectangle containing both.
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x0 = self.x.min(o.x);
        let y0 = self.y.min(o.y);
        let x1 = (self.x + self.w).max(o.x + o.w);
        let y1 = (self.y + self.h).max(o.y + o.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Clip to 0..w, 0..h. May be empty.
    pub fn clipped(&self, w: usize, h: usize) -> Rect {
        let x0 = self.x.max(0);
        let y0 = self.y.max(0);
        let x1 = (self.x + self.w).min(w as i32);
        let y1 = (self.y + self.h).min(h as i32);
        Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }
}

/// A small fixed-capacity list of damaged rectangles. Draw into a back buffer, call
/// [`add`](Self::add) for each region you touched, then [`flush`](Self::flush) to copy only
/// those regions to the screen. Overlapping rectangles are merged into their union; if the list
/// fills up, everything collapses into one bounding rectangle.
pub struct DirtyList<const N: usize> {
    rects: [Rect; N],
    len: usize,
}

impl<const N: usize> DirtyList<N> {
    pub const fn new() -> Self {
        DirtyList {
            rects: [Rect::new(0, 0, 0, 0); N],
            len: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The current damaged rectangles.
    pub fn rects(&self) -> &[Rect] {
        &self.rects[..self.len]
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Mark a region as damaged.
    pub fn add(&mut self, r: Rect) {
        if r.is_empty() {
            return;
        }
        let mut r = r;
        // Absorb everything that overlaps, repeating because the union may now reach more rects.
        loop {
            let mut merged = false;
            let mut i = 0;
            while i < self.len {
                if self.rects[i].intersects(&r) {
                    r = r.union(&self.rects[i]);
                    self.len -= 1;
                    self.rects[i] = self.rects[self.len];
                    merged = true;
                } else {
                    i += 1;
                }
            }
            if !merged {
                break;
            }
        }
        if self.len == N {
            // Out of room: collapse to one bounding rectangle.
            for o in &self.rects[..self.len] {
                r = r.union(o);
            }
            self.len = 0;
        }
        self.rects[self.len] = r;
        self.len += 1;
    }

    /// Copy every damaged region from `back` to `front` (same coordinates), then forget them.
    pub fn flush(&mut self, front: &mut dyn DrawTarget, back: &dyn DrawTarget) {
        let (w, h) = (
            front.width().min(back.width()),
            front.height().min(back.height()),
        );
        for r in self.rects() {
            let r = r.clipped(w, h);
            if !r.is_empty() {
                front.blit(r.x, r.y, back, r.x, r.y, r.w, r.h);
            }
        }
        self.len = 0;
    }
}

impl<const N: usize> Default for DirtyList<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`DrawTarget`] backed by a slice of pixels: the screen framebuffer, or a back buffer.
pub struct Surface<'a> {
    buf: &'a mut [u32],
    width: usize,
    height: usize,
    stride: usize,
}

impl<'a> Surface<'a> {
    /// Wrap a pixel slice. `buf` must hold at least `stride * height` pixels (and `stride >= width`).
    pub fn new(buf: &'a mut [u32], width: usize, height: usize, stride: usize) -> Self {
        assert!(stride >= width && buf.len() >= stride * height);
        Surface {
            buf,
            width,
            height,
            stride,
        }
    }
}

impl Surface<'static> {
    /// Wrap raw memory, e.g. the screen framebuffer.
    ///
    /// # Safety
    /// `ptr` must be 4-byte aligned, valid for `stride * height` pixels for `'static`, and nothing
    /// else may access it through a Rust reference while this exists.
    pub unsafe fn from_raw(ptr: *mut u32, width: usize, height: usize, stride: usize) -> Self {
        // SAFETY: forwarded to the caller.
        Surface {
            buf: unsafe { core::slice::from_raw_parts_mut(ptr, stride * height) },
            width,
            height,
            stride,
        }
    }
}

impl DrawTarget for Surface<'_> {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
    fn stride(&self) -> usize {
        self.stride
    }
    fn pixels(&self) -> &[u32] {
        self.buf
    }
    fn pixels_mut(&mut self) -> &mut [u32] {
        self.buf
    }
}

/// Fill an anti-aliased shape. `inside(x, y)` is asked about points in quarter-pixel units
/// relative to (`ox`, `oy`) (so pixel (px, py) covers x in `4*px..4*px+4`); each pixel is covered
/// by 4x4 sample points. The area drawn is `bx..bx+bw`, `by..by+bh` in target pixels.
pub fn fill_aa(
    t: &mut dyn DrawTarget,
    (bx, by, bw, bh): (i32, i32, i32, i32),
    (ox, oy): (i32, i32),
    c: Color,
    max_alpha: u8,
    inside: &dyn Fn(i32, i32) -> bool,
) {
    for py in by..by + bh {
        for px in bx..bx + bw {
            let mut hits = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    if inside((px - ox) * 4 + sx, (py - oy) * 4 + sy) {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                t.blend_pixel(px, py, c, (hits * max_alpha as u32 / 16) as u8);
            }
        }
    }
}
