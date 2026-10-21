//! The artist's assets (docs/ui-concepts/art), decoded at build time by
//! scripts/art_embed.py and embedded here: app icons at 16..96 px (light and dark), the
//! Files file types, the wallpapers and the boot splash. Anything missing (no python3 at
//! build time, a new app without an icon) falls back to the shapes the kernel draws itself.

use alloc::vec::Vec;

#[repr(C, align(4))]
struct Aligned<T: ?Sized>(T);

/// An icon: `size` x `size` ARGB pixels (alpha in the top byte, not premultiplied).
pub struct IconArt {
    pub name: &'static str,
    pub size: u16,
    pub dark: bool,
    data: &'static [u8],
}

impl IconArt {
    pub fn pixels(&self) -> &'static [u32] {
        // SAFETY: the blob is 4-byte aligned (Aligned) and its length a multiple of 4
        // (size * size * 4, from art_embed.py); u32 has no invalid bit patterns.
        unsafe {
            core::slice::from_raw_parts(self.data.as_ptr() as *const u32, self.data.len() / 4)
        }
    }
}

/// A full-screen image (wallpaper, splash): RGB, raw or run-length coded.
pub struct ImageArt {
    pub name: &'static str,
    pub w: u16,
    pub h: u16,
    rle: bool,
    data: &'static [u8],
}

impl ImageArt {
    /// Decode straight into rows of `stride` pixels (no allocation: the boot splash runs
    /// this early). `out` must hold h rows.
    pub fn decode_rows(&self, out: &mut [u32], stride: usize) {
        let w = self.w as usize;
        let n = w * self.h as usize;
        let px = |c: &[u8; 3]| (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
        let mut put = |i: usize, c: u32| {
            if let Some(p) = out.get_mut(i / w * stride + i % w) {
                *p = c;
            }
        };
        if self.rle {
            let mut i = 0;
            for run in self.data.as_chunks::<4>().0 {
                let c = px(&[run[1], run[2], run[3]]);
                for _ in 0..run[0] {
                    if i < n {
                        put(i, c);
                    }
                    i += 1;
                }
            }
        } else {
            for (i, c) in self.data.as_chunks::<3>().0.iter().take(n).enumerate() {
                put(i, px(c));
            }
        }
    }

    /// Decode into `out` (w * h pixels, 0x00RRGGBB).
    pub fn decode(&self, out: &mut [u32]) {
        self.decode_rows(out, self.w as usize);
    }

    /// Decoded, scaled to cover `w` x `h` (centred, the overflow cropped).
    pub fn scaled(&self, w: usize, h: usize) -> Vec<u32> {
        let (sw, sh) = (self.w as usize, self.h as usize);
        let mut src = alloc::vec![0u32; sw * sh];
        self.decode(&mut src);
        if (sw, sh) == (w, h) {
            return src;
        }
        scale_cover(&src, sw, sh, w, h)
    }
}

include!(concat!(env!("OUT_DIR"), "/art/art.rs"));

/// The icon `name` at exactly `size` px in this theme (dark falls back to light).
pub fn icon(name: &str, size: i32, dark: bool) -> Option<&'static IconArt> {
    let find = |d: bool| {
        ICONS
            .iter()
            .find(|i| i.name == name && i.size as i32 == size && i.dark == d)
    };
    if dark {
        find(true).or_else(|| find(false))
    } else {
        find(false)
    }
}

/// The smallest embedded size of `name` at least `size` (else the largest), to scale from.
pub fn icon_near(name: &str, size: i32, dark: bool) -> Option<&'static IconArt> {
    let of = |d: bool| ICONS.iter().filter(move |i| i.name == name && i.dark == d);
    let pick = |d: bool| {
        of(d)
            .filter(|i| i.size as i32 >= size)
            .min_by_key(|i| i.size)
            .or_else(|| of(d).max_by_key(|i| i.size))
    };
    if dark {
        pick(true).or_else(|| pick(false))
    } else {
        pick(false)
    }
}

pub fn image(name: &str) -> Option<&'static ImageArt> {
    IMAGES.iter().find(|i| i.name == name)
}

/// Area-average an ARGB square down (or nearest up) to `d` x `d`, alpha-weighted so edges
/// stay clean.
pub fn scale_icon(src: &[u32], s: usize, d: usize) -> Vec<u32> {
    let mut out = alloc::vec![0u32; d * d];
    for dy in 0..d {
        let (y0, y1) = (dy * s / d, ((dy + 1) * s / d).max(dy * s / d + 1));
        for dx in 0..d {
            let (x0, x1) = (dx * s / d, ((dx + 1) * s / d).max(dx * s / d + 1));
            let (mut a, mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for y in y0..y1.min(s) {
                for x in x0..x1.min(s) {
                    let q = src[y * s + x];
                    let qa = q >> 24;
                    a += qa;
                    r += (q >> 16 & 0xff) * qa;
                    g += (q >> 8 & 0xff) * qa;
                    b += (q & 0xff) * qa;
                    n += 1;
                }
            }
            if let (Some(r), Some(g), Some(b)) =
                (r.checked_div(a), g.checked_div(a), b.checked_div(a))
            {
                out[dy * d + dx] = (a / n.max(1)) << 24 | r << 16 | g << 8 | b;
            }
        }
    }
    out
}

/// Scale an RGB image to cover `dw` x `dh` (aspect kept, centred, the rest cropped):
/// bilinear when growing, an area average when shrinking.
pub fn scale_cover(src: &[u32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u32> {
    // The scale (16.16) and the source window that maps onto the destination.
    let k = ((dw << 16) / sw).max((dh << 16) / sh).max(1);
    let (vw, vh) = ((dw << 16) / k, (dh << 16) / k); // source pixels shown
    let (ox, oy) = ((sw - vw.min(sw)) / 2, (sh - vh.min(sh)) / 2);
    let mut out = alloc::vec![0u32; dw * dh];
    let ch = |c: u32, s: u32| (c >> s) & 0xff;
    if k >= 1 << 16 {
        // Growing: bilinear.
        for y in 0..dh {
            let fy = (y << 16) * vh / dh.max(1);
            let sy = oy + (fy >> 16).min(vh.saturating_sub(1));
            let ty = (fy & 0xffff) as u32 >> 8;
            let sy1 = (sy + 1).min(sh - 1);
            for x in 0..dw {
                let fx = (x << 16) * vw / dw.max(1);
                let sx = ox + (fx >> 16).min(vw.saturating_sub(1));
                let tx = (fx & 0xffff) as u32 >> 8;
                let sx1 = (sx + 1).min(sw - 1);
                let (p00, p10) = (src[sy * sw + sx], src[sy * sw + sx1]);
                let (p01, p11) = (src[sy1 * sw + sx], src[sy1 * sw + sx1]);
                let mut c = 0;
                for s in [16, 8, 0] {
                    let top = ch(p00, s) * (256 - tx) + ch(p10, s) * tx;
                    let bot = ch(p01, s) * (256 - tx) + ch(p11, s) * tx;
                    c |= ((top * (256 - ty) + bot * ty) >> 16) << s;
                }
                out[y * dw + x] = c;
            }
        }
    } else {
        // Shrinking: average the source pixels each destination pixel covers.
        for y in 0..dh {
            let y0 = oy + y * vh / dh;
            let y1 = (oy + (y + 1) * vh / dh).max(y0 + 1).min(sh);
            for x in 0..dw {
                let x0 = ox + x * vw / dw;
                let x1 = (ox + (x + 1) * vw / dw).max(x0 + 1).min(sw);
                let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
                for yy in y0..y1 {
                    for &q in &src[yy * sw + x0..yy * sw + x1] {
                        r += ch(q, 16);
                        g += ch(q, 8);
                        b += ch(q, 0);
                        n += 1;
                    }
                }
                let n = n.max(1);
                out[y * dw + x] = (r / n) << 16 | (g / n) << 8 | (b / n);
            }
        }
    }
    out
}
