//! Scaling for zoom: area average when an axis shrinks, nearest pixel when it grows. Averaging is
//! weighted by alpha, so a transparent pixel doesn't tint its neighbours.

use alloc::vec::Vec;

use crate::{Image, ImgError};

pub fn scaled(src: &Image, w: usize, h: usize) -> Result<Image, ImgError> {
    let mut out = Image::new(w, h)?;
    // Source span of destination column / row: [from, to), at least one pixel.
    let span = |d: usize, dn: usize, sn: usize| -> (usize, usize) {
        let from = d * sn / dn;
        let to = ((d + 1) * sn).div_ceil(dn).max(from + 1).min(sn);
        (from, to)
    };
    let cols: Vec<(usize, usize)> = (0..w).map(|x| span(x, w, src.w)).collect();
    for y in 0..h {
        let (y0, y1) = span(y, h, src.h);
        for (x, &(x0, x1)) in cols.iter().enumerate() {
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.px[y * w + x] = if n == 1 {
                src.px[y0 * src.w + x0]
            } else {
                let (mut a, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64);
                for sy in y0..y1 {
                    for &p in &src.px[sy * src.w + x0..sy * src.w + x1] {
                        let pa = (p >> 24) as u64;
                        a += pa;
                        r += pa * ((p >> 16) & 0xFF) as u64;
                        g += pa * ((p >> 8) & 0xFF) as u64;
                        b += pa * (p & 0xFF) as u64;
                    }
                }
                if a == 0 {
                    0
                } else {
                    let avg = |v: u64| ((v + a / 2) / a) as u32;
                    (((a + n / 2) / n) as u32) << 24 | avg(r) << 16 | avg(g) << 8 | avg(b)
                }
            };
        }
    }
    Ok(out)
}
