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

/// The embedded icon name among `names` (first match), as a static string.
pub fn icon_named(names: &[&str]) -> Option<&'static str> {
    names
        .iter()
        .find_map(|n| ICONS.iter().find(|i| i.name == *n).map(|i| i.name))
}

pub fn image(name: &str) -> Option<&'static ImageArt> {
    IMAGES.iter().find(|i| i.name == name)
}

/// Per output pixel along one axis (`s` source px to `d` output px): the first source pixel
/// and the weights (1/256) of the ones it reads. Shrinking: exact area coverage (a box over
/// the fractional source interval). Growing: bilinear (the two nearest, by distance).
fn axis_weights(s: usize, d: usize) -> Vec<(usize, Vec<u32>)> {
    let mut out = Vec::with_capacity(d);
    for o in 0..d {
        if d <= s {
            // [o*s/d, (o+1)*s/d) in 1/256 source px.
            let (a, z) = ((o * s * 256 / d) as u64, ((o + 1) * s * 256 / d) as u64);
            let first = (a / 256) as usize;
            let mut w = Vec::new();
            let mut c = first;
            while ((c as u64) * 256) < z && c < s {
                let ca = a.max(c as u64 * 256);
                let cz = z.min((c as u64 + 1) * 256);
                w.push((cz - ca) as u32);
                c += 1;
            }
            out.push((first, w));
        } else {
            // The output centre, in 1/256 source px, less half a pixel.
            let c = ((2 * o + 1) * s * 128 / d) as i64 - 128;
            let c = c.clamp(0, (s as i64 - 1) * 256);
            let (i, f) = ((c / 256) as usize, (c % 256) as u32);
            if i + 1 < s && f > 0 {
                out.push((i, alloc::vec![256 - f, f]));
            } else {
                out.push((i, alloc::vec![256]));
            }
        }
    }
    out
}

/// Resample an ARGB square to `d` x `d`: area-averaged when shrinking, bilinear when growing,
/// alpha-weighted so edges stay clean. Every source pixel counts with its exact fractional
/// share, so fractional sizes (the magnified dock) don't shimmer as the size changes.
pub fn scale_icon(src: &[u32], s: usize, d: usize) -> Vec<u32> {
    let mut out = alloc::vec![0u32; d * d];
    let wts = axis_weights(s, d);
    for (dy, (y0, wy)) in wts.iter().enumerate() {
        for (dx, (x0, wx)) in wts.iter().enumerate() {
            let (mut a, mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for (j, &ky) in wy.iter().enumerate() {
                let row = &src[(y0 + j) * s..(y0 + j + 1) * s];
                for (i, &kx) in wx.iter().enumerate() {
                    let k = (ky * kx) as u64;
                    let q = row[x0 + i];
                    let qa = (q >> 24) as u64 * k;
                    a += qa;
                    r += (q >> 16 & 0xff) as u64 * qa;
                    g += (q >> 8 & 0xff) as u64 * qa;
                    b += (q & 0xff) as u64 * qa;
                    n += k;
                }
            }
            if let (Some(r), Some(g), Some(b)) =
                (r.checked_div(a), g.checked_div(a), b.checked_div(a))
            {
                let (r, g, b) = (r as u32, g as u32, b as u32);
                let al = ((a + n / 2) / n.max(1)).min(255) as u32;
                out[dy * d + dx] = al << 24 | r << 16 | g << 8 | b;
            }
        }
    }
    out
}

/// Scale an RGB image (`sw` x `sh`, rows `ss` pixels apart) to cover `dw` x `dh` (aspect
/// kept, centred, the rest cropped) into `out` (rows `os` pixels apart): bilinear when
/// growing, an area average when shrinking. No allocation.
#[allow(clippy::too_many_arguments)]
pub fn scale_cover(
    src: &[u32],
    ss: usize,
    sw: usize,
    sh: usize,
    out: &mut [u32],
    os: usize,
    dw: usize,
    dh: usize,
) {
    // The scale (16.16) and the source window that maps onto the destination.
    let k = ((dw << 16) / sw).max((dh << 16) / sh).max(1);
    let (vw, vh) = ((dw << 16) / k, (dh << 16) / k); // source pixels shown
    let (ox, oy) = ((sw - vw.min(sw)) / 2, (sh - vh.min(sh)) / 2);
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
                let (p00, p10) = (src[sy * ss + sx], src[sy * ss + sx1]);
                let (p01, p11) = (src[sy1 * ss + sx], src[sy1 * ss + sx1]);
                let mut c = 0;
                for s in [16, 8, 0] {
                    let top = ch(p00, s) * (256 - tx) + ch(p10, s) * tx;
                    let bot = ch(p01, s) * (256 - tx) + ch(p11, s) * tx;
                    c |= ((top * (256 - ty) + bot * ty) >> 16) << s;
                }
                out[y * os + x] = c;
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
                    for &q in &src[yy * ss + x0..yy * ss + x1] {
                        r += ch(q, 16);
                        g += ch(q, 8);
                        b += ch(q, 0);
                        n += 1;
                    }
                }
                let n = n.max(1);
                out[y * os + x] = (r / n) << 16 | (g / n) << 8 | (b / n);
            }
        }
    }
}
