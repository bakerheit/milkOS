//! Pixels: a [`Surface`] (an offscreen 0x00RRGGBB picture with a clip rectangle) with the
//! drawing the Browser needs, painting a [`DisplayList`] into one at a scroll offset, and
//! [`scale_into`] for zoom (nearest pixel for whole-number zooms, so text stays crisp, else
//! bilinear).

use alloc::vec;
use alloc::vec::Vec;

use imgdec::Image;

use crate::engine::{Color, DisplayList, Font, Item, Rect};
use crate::font;

/// `src` over `dst` at `a` (0..=255).
pub fn blend(dst: Color, src: Color, a: u32) -> Color {
    let ia = 255 - a;
    let ch = |s: u32| {
        let (d, x) = ((dst >> s) & 0xFF, (src >> s) & 0xFF);
        ((x * a + d * ia + 127) / 255) << s
    };
    ch(16) | ch(8) | ch(0)
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
}

pub struct Surface {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
    pub clip: Rect,
    /// Added to every coordinate drawn (the page's scroll).
    pub dx: i32,
    pub dy: i32,
}

impl Surface {
    /// The most pixels a surface may have.
    pub const MAX_PIXELS: usize = 4096 * 4096;

    pub fn new(w: i32, h: i32, c: Color) -> Surface {
        let (w, h) = (w.max(1), h.max(1));
        let n = (w as usize * h as usize).min(Self::MAX_PIXELS);
        let h = (n / w as usize) as i32;
        Surface {
            w,
            h,
            px: vec![c; n],
            clip: Rect::new(0, 0, w, h),
            dx: 0,
            dy: 0,
        }
    }

    /// Make it `w` x `h` (contents undefined afterwards).
    pub fn resize(&mut self, w: i32, h: i32) {
        if (w, h) != (self.w, self.h) {
            *self = Surface::new(w, h, 0);
        }
        self.clip = Rect::new(0, 0, self.w, self.h);
        self.dx = 0;
        self.dy = 0;
    }

    pub fn clear(&mut self, c: Color) {
        self.px.fill(c);
    }

    /// The visible part of `r` (after the offset), in surface pixels.
    fn vis(&self, r: Rect) -> Rect {
        intersect(Rect::new(r.x + self.dx, r.y + self.dy, r.w, r.h), self.clip)
    }

    pub fn fill(&mut self, r: Rect, c: Color) {
        let v = self.vis(r);
        for y in v.y..v.y + v.h {
            let row = (y * self.w) as usize;
            self.px[row + v.x as usize..row + (v.x + v.w) as usize].fill(c);
        }
    }

    pub fn fill_alpha(&mut self, r: Rect, c: Color, a: u32) {
        let v = self.vis(r);
        for y in v.y..v.y + v.h {
            let row = (y * self.w) as usize;
            for p in &mut self.px[row + v.x as usize..row + (v.x + v.w) as usize] {
                *p = blend(*p, c, a);
            }
        }
    }

    fn put(&mut self, x: i32, y: i32, c: Color, a: u32) {
        let (x, y) = (x + self.dx, y + self.dy);
        if self.clip.contains(x, y) {
            let p = &mut self.px[(y * self.w + x) as usize];
            *p = if a >= 255 { c } else { blend(*p, c, a) };
        }
    }

    /// A rounded rectangle (radius `r`) with soft corners.
    pub fn rrect(&mut self, rc: Rect, r: i32, c: Color) {
        let r = r.min(rc.w / 2).min(rc.h / 2).max(0);
        if r == 0 {
            return self.fill(rc, c);
        }
        self.fill(Rect::new(rc.x, rc.y + r, rc.w, rc.h - 2 * r), c);
        self.fill(Rect::new(rc.x + r, rc.y, rc.w - 2 * r, r), c);
        self.fill(Rect::new(rc.x + r, rc.y + rc.h - r, rc.w - 2 * r, r), c);
        for (cx, cy, x0, y0) in [
            (rc.x + r, rc.y + r, rc.x, rc.y),
            (rc.x + rc.w - r - 1, rc.y + r, rc.x + rc.w - r, rc.y),
            (rc.x + r, rc.y + rc.h - r - 1, rc.x, rc.y + rc.h - r),
            (
                rc.x + rc.w - r - 1,
                rc.y + rc.h - r - 1,
                rc.x + rc.w - r,
                rc.y + rc.h - r,
            ),
        ] {
            for y in y0..y0 + r {
                for x in x0..x0 + r {
                    let mut n = 0;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let dx = (x - cx) * 4 + sx - 2 + 1;
                            let dy = (y - cy) * 4 + sy - 2 + 1;
                            if dx * dx + dy * dy <= 16 * r * r {
                                n += 1;
                            }
                        }
                    }
                    if n > 0 {
                        self.put(x, y, c, n * 255 / 16);
                    }
                }
            }
        }
    }

    /// A filled circle.
    pub fn disc(&mut self, cx: i32, cy: i32, r: i32, c: Color) {
        self.rrect(Rect::new(cx - r, cy - r, 2 * r + 1, 2 * r + 1), r, c);
    }

    /// A `width`-pixel frame just inside `r`.
    pub fn border(&mut self, r: Rect, width: i32, c: Color) {
        let t = width.max(1).min(r.w / 2 + 1).min(r.h / 2 + 1);
        self.fill(Rect::new(r.x, r.y, r.w, t), c);
        self.fill(Rect::new(r.x, r.y + r.h - t, r.w, t), c);
        self.fill(Rect::new(r.x, r.y + t, t, r.h - 2 * t), c);
        self.fill(Rect::new(r.x + r.w - t, r.y + t, t, r.h - 2 * t), c);
    }

    /// Text with its top left at (x, y); returns the x where it ends.
    pub fn text(&mut self, x: i32, y: i32, s: &str, f: Font, c: Color) -> i32 {
        let sc = font::scale(f);
        let step = font::CELL_W * sc;
        // Nothing of it can show: skip the glyph loop.
        if y + self.dy >= self.clip.bottom() || y + self.dy + font::CELL_H * sc <= self.clip.y {
            return x + font::width(s, f);
        }
        let mut cx = x;
        for g in font::glyphs(s) {
            if cx + self.dx >= self.clip.x + self.clip.w {
                cx += step;
                continue;
            }
            if cx + step + self.dx > self.clip.x {
                for (row, bits) in g.iter().enumerate() {
                    if *bits == 0 {
                        continue;
                    }
                    for col in 0..8 {
                        if bits & (0x80 >> col) == 0 {
                            continue;
                        }
                        // Italic: a slant of one pixel every four rows.
                        let slant = if f.italic {
                            (15 - row as i32) / 4 - 1
                        } else {
                            0
                        };
                        for sy in 0..sc {
                            for sx in 0..sc {
                                let px = cx + col * sc + sx + slant;
                                let py = y + row as i32 * sc + sy;
                                self.put(px, py, c, 255);
                                if f.bold {
                                    self.put(px + 1, py, c, 255);
                                }
                            }
                        }
                    }
                }
            }
            cx += step;
        }
        cx
    }

    /// `img` (straight alpha ARGB) scaled into `r` (nearest pixel), blended over what is there.
    pub fn image(&mut self, r: Rect, img: &Image) {
        if img.w == 0 || img.h == 0 || r.w <= 0 || r.h <= 0 {
            return;
        }
        let v = self.vis(r);
        if v.w <= 0 || v.h <= 0 {
            return;
        }
        let (ox, oy) = (r.x + self.dx, r.y + self.dy);
        let cols: Vec<usize> = (v.x..v.x + v.w)
            .map(|x| (((x - ox) as i64 * img.w as i64 / r.w as i64) as usize).min(img.w - 1))
            .collect();
        for y in v.y..v.y + v.h {
            let sy = (((y - oy) as i64 * img.h as i64 / r.h as i64) as usize).min(img.h - 1);
            let src = &img.px[sy * img.w..sy * img.w + img.w];
            let row = (y * self.w) as usize;
            for (k, &sx) in cols.iter().enumerate() {
                let p = src[sx];
                let a = p >> 24;
                if a == 0 {
                    continue;
                }
                let d = &mut self.px[row + v.x as usize + k];
                *d = if a == 255 {
                    p & 0xFF_FFFF
                } else {
                    blend(*d, p & 0xFF_FFFF, a)
                };
            }
        }
    }
}

/// What [`paint_list`] needs from the app besides the list.
pub trait Painter {
    /// A loaded image.
    fn image(&self, url: &str) -> Option<&Image>;
    /// Draw form control `id` (the app knows its state, focus and caret).
    fn control(&mut self, s: &mut Surface, rect: Rect, id: usize);
}

/// Paint the part of `list` from page y = `scroll` down into `s` (which is cleared to the
/// page's background first). `marks` are find-in-page highlights (the `current` one
/// stronger).
pub fn paint_list(
    s: &mut Surface,
    list: &DisplayList,
    scroll: i32,
    p: &mut dyn Painter,
    marks: &[Rect],
    current: Option<usize>,
) {
    s.clear(list.background);
    s.dx = 0;
    s.dy = -scroll;
    let top = scroll;
    let bottom = scroll + s.h;
    let visible = |r: &Rect| r.y < bottom && r.y + r.h > top;
    for (k, m) in marks.iter().enumerate() {
        if visible(m) {
            let c = if Some(k) == current {
                0xEB_B6_50
            } else {
                0xF6_DF_A8
            };
            s.fill(Rect::new(m.x - 1, m.y, m.w + 2, m.h), c);
        }
    }
    for it in &list.items {
        match it {
            Item::Fill { rect, color } => {
                if visible(rect) {
                    s.fill(*rect, *color)
                }
            }
            Item::Border { rect, width, color } => {
                if visible(rect) {
                    s.border(*rect, *width, *color)
                }
            }
            Item::Text {
                x,
                y,
                width,
                text,
                font,
                color,
                underline,
            } => {
                let h = font::line_height(*font);
                if *y < bottom && y + h > top {
                    s.text(*x, *y + 2, text, *font, *color);
                    if *underline {
                        let uy = y + 2 + font::CELL_H * font::scale(*font) - 1;
                        s.fill(Rect::new(*x, uy, *width, 1), *color);
                    }
                }
            }
            Item::Image { rect, url, alt } => {
                if !visible(rect) {
                    continue;
                }
                match p.image(url) {
                    Some(img) => s.image(*rect, img),
                    None => {
                        // Not loaded (yet, or ever): a frame with the alt text.
                        s.fill(*rect, 0xF3_EC_DF);
                        s.border(*rect, 1, 0xE2_D5_BD);
                        let old = s.clip;
                        s.clip =
                            intersect(Rect::new(rect.x + s.dx, rect.y + s.dy, rect.w, rect.h), old);
                        s.text(
                            rect.x + 6,
                            rect.y + (rect.h - 16) / 2,
                            alt,
                            Font::BODY,
                            0x6B_58_46,
                        );
                        s.clip = old;
                    }
                }
            }
            Item::Control { rect, id } => {
                if visible(rect) {
                    p.control(s, *rect, *id)
                }
            }
        }
    }
    s.dy = 0;
}

/// Copy `src` into `dst` (stride `dst_w`) at (`x`, `y`), scaled to `w` x `h`: nearest pixel
/// when the scale is a whole number (or 1), bilinear otherwise.
#[allow(clippy::too_many_arguments)]
pub fn scale_into(
    src: &Surface,
    dst: &mut [u32],
    dst_w: i32,
    dst_h: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) {
    if w <= 0 || h <= 0 || src.w <= 0 || src.h <= 0 {
        return;
    }
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(dst_w);
    let y1 = (y + h).min(dst_h);
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    if (w, h) == (src.w, src.h) {
        for yy in y0..y1 {
            let s = ((yy - y) * src.w + (x0 - x)) as usize;
            let d = (yy * dst_w + x0) as usize;
            let n = (x1 - x0) as usize;
            dst[d..d + n].copy_from_slice(&src.px[s..s + n]);
        }
        return;
    }
    let whole = w % src.w == 0 && h % src.h == 0;
    if whole {
        let (fx, fy) = (w / src.w, h / src.h);
        for yy in y0..y1 {
            let sy = ((yy - y) / fy).min(src.h - 1);
            for xx in x0..x1 {
                let sx = ((xx - x) / fx).min(src.w - 1);
                dst[(yy * dst_w + xx) as usize] = src.px[(sy * src.w + sx) as usize];
            }
        }
        return;
    }
    // Bilinear, in 8.8 fixed point.
    let map = |d: i32, dn: i32, sn: i32| -> (usize, usize, u32) {
        let f = ((2 * d + 1) as i64 * sn as i64 * 256 / (2 * dn as i64) - 128).max(0);
        let i = (f >> 8) as i32;
        let i0 = i.min(sn - 1) as usize;
        let i1 = (i + 1).min(sn - 1) as usize;
        (i0, i1, (f & 255) as u32)
    };
    let cols: Vec<(usize, usize, u32)> = (x0..x1).map(|xx| map(xx - x, w, src.w)).collect();
    for yy in y0..y1 {
        let (r0, r1, fy) = map(yy - y, h, src.h);
        let (a, b) = (
            &src.px[r0 * src.w as usize..][..src.w as usize],
            &src.px[r1 * src.w as usize..][..src.w as usize],
        );
        let drow = (yy * dst_w) as usize;
        for (k, &(c0, c1, fx)) in cols.iter().enumerate() {
            let top = blend(a[c0], a[c1], fx);
            let bot = blend(b[c0], b[c1], fx);
            dst[drow + x0 as usize + k] = blend(top, bot, fy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct P(Option<Image>, usize);
    impl Painter for P {
        fn image(&self, _: &str) -> Option<&Image> {
            self.0.as_ref()
        }
        fn control(&mut self, s: &mut Surface, r: Rect, _id: usize) {
            self.1 += 1;
            s.fill(r, 0x123456);
        }
    }

    #[test]
    fn fills_clip_and_text() {
        let mut s = Surface::new(40, 30, 0);
        s.fill(Rect::new(-10, -10, 20, 20), 0xFF0000);
        assert_eq!(s.px[0], 0xFF0000);
        assert_eq!(s.px[10], 0);
        s.fill(Rect::new(35, 25, 100, 100), 0x00FF00);
        assert_eq!(*s.px.last().unwrap(), 0x00FF00);
        let end = s.text(0, 10, "Hi", Font::BODY, 0xFFFFFF);
        assert_eq!(end, 16);
        assert!(s.px[(10 * 40)..(26 * 40)].contains(&0xFFFFFF));
        // Way off the surface: nothing happens, nothing panics.
        s.text(-1000, -1000, "x", Font::BODY, 1);
        s.text(i32::MAX / 4, 5, "x", Font::BODY, 1);
        s.rrect(Rect::new(5, 5, 20, 10), 50, 0x0000FF);
        s.disc(20, 15, 4, 0x00FFFF);
        s.border(Rect::new(0, 0, 40, 30), 2, 0xABCDEF);
        assert_eq!(s.px[0], 0xABCDEF);
    }

    #[test]
    fn images_blend_and_scale() {
        let mut img = Image::new(2, 1).unwrap();
        img.px = alloc::vec![0xFF_FF0000, 0x80_0000FF];
        let mut s = Surface::new(4, 2, 0xFFFFFF);
        s.image(Rect::new(0, 0, 4, 2), &img);
        assert_eq!(s.px[0], 0xFF0000);
        assert_eq!(s.px[1], 0xFF0000);
        let b = s.px[3];
        assert!((b & 0xFF) == 0xFF && (b >> 16) < 0x90, "{b:x}");
    }

    #[test]
    fn display_list_paint() {
        let mut d = crate::engine::parse(
            b"<p>hello</p><p><input name=a><img src=x.png alt=x></p>",
            "text/html",
            "http://x.test/",
        );
        let none = |_: &str| None;
        let list = d
            .layout(300, &crate::font::Metrics { image_size: &none })
            .clone();
        let mut s = Surface::new(300, 200, 0);
        let mut p = P(None, 0);
        paint_list(
            &mut s,
            &list,
            0,
            &mut p,
            &[Rect::new(20, 20, 10, 10)],
            Some(0),
        );
        assert_eq!(p.1, 1);
        assert!(s.px.contains(&0x123456));
        assert!(s.px.contains(&0xEBB650));
        // Scrolled past everything: just the background.
        paint_list(&mut s, &list, 10_000, &mut p, &[], None);
        assert!(s.px.iter().all(|&c| c == list.background));
    }

    #[test]
    fn scaling() {
        let mut src = Surface::new(2, 2, 0);
        src.px = alloc::vec![0x000000, 0xFFFFFF, 0xFFFFFF, 0x000000];
        let mut dst = alloc::vec![7u32; 16];
        scale_into(&src, &mut dst, 4, 4, 0, 0, 4, 4);
        assert_eq!(&dst[..4], &[0, 0, 0xFFFFFF, 0xFFFFFF]);
        let mut dst = alloc::vec![7u32; 9];
        scale_into(&src, &mut dst, 3, 3, 0, 0, 3, 3);
        assert!(dst.iter().all(|&c| c != 7));
        let mut same = alloc::vec![0u32; 4];
        scale_into(&src, &mut same, 2, 2, 0, 0, 2, 2);
        assert_eq!(same, src.px);
        // Off the destination: clipped.
        let mut d = alloc::vec![7u32; 4];
        scale_into(&src, &mut d, 2, 2, 1, 1, 3, 3);
        assert_eq!(d[0], 7);
        scale_into(&src, &mut d, 2, 2, -5, -5, 3, 3);
    }
}
