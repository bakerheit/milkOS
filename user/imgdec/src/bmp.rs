//! BMP (Windows bitmap): 1, 4, 8 bit palettes, 16 bit (555 or masks), 24 and 32 bit, bottom-up
//! or top-down, with the old 12-byte header and the 40/52/56/108/124-byte ones. RLE compression
//! (and the JPEG/PNG-in-BMP kinds) is reported as unsupported.

use alloc::vec::Vec;

use crate::{Image, ImgError, MAX_PIXELS, argb};

fn u16le(b: &[u8], at: usize) -> Result<u32, ImgError> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]) as u32)
        .ok_or(ImgError::Truncated)
}

fn u32le(b: &[u8], at: usize) -> Result<u32, ImgError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(ImgError::Truncated)
}

/// A colour channel taken from a mask: (shift, bits).
fn channel(mask: u32) -> (u32, u32) {
    if mask == 0 {
        return (0, 0);
    }
    let shift = mask.trailing_zeros();
    (shift, (mask >> shift).trailing_ones())
}

fn extract(v: u32, mask: u32) -> u8 {
    let (shift, bits) = channel(mask);
    if bits == 0 {
        return 255;
    }
    let x = (v & mask) >> shift;
    if bits >= 8 {
        (x >> (bits - 8)) as u8
    } else {
        (x * 255 / ((1 << bits) - 1)) as u8
    }
}

pub fn decode(b: &[u8]) -> Result<Image, ImgError> {
    if !b.starts_with(b"BM") {
        return Err(ImgError::Unrecognised("This isn't a BMP file."));
    }
    let data_off = u32le(b, 10)? as usize;
    let hsize = u32le(b, 14)? as usize;
    let (w, hraw, bpp, comp, clrused, core) = if hsize == 12 {
        (
            u16le(b, 18)? as i64,
            u16le(b, 20)? as i64,
            u16le(b, 24)?,
            0,
            0,
            true,
        )
    } else if hsize >= 40 {
        (
            u32le(b, 18)? as i32 as i64,
            u32le(b, 22)? as i32 as i64,
            u16le(b, 28)?,
            u32le(b, 30)?,
            u32le(b, 46)?,
            false,
        )
    } else {
        return Err(ImgError::Corrupt("The BMP header has an odd size."));
    };
    if w <= 0 || hraw == 0 {
        return Err(ImgError::Corrupt("The BMP has no size."));
    }
    let top_down = hraw < 0;
    let (w, h) = (w as usize, hraw.unsigned_abs() as usize);
    if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err(ImgError::TooBig);
    }
    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(ImgError::Unsupported(
            "The BMP has a colour depth that isn't supported.",
        ));
    }
    match comp {
        0 | 3 | 6 => {}
        1 | 2 => {
            return Err(ImgError::Unsupported(
                "RLE-compressed BMP files can't be opened yet.",
            ));
        }
        _ => {
            return Err(ImgError::Unsupported(
                "This kind of BMP compression isn't supported.",
            ));
        }
    }
    // Channel masks (16 and 32 bit).
    let (mut rm, mut gm, mut bm, mut am) = match bpp {
        16 => (0x7C00u32, 0x03E0, 0x001F, 0),
        32 => (0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0),
        _ => (0, 0, 0, 0),
    };
    if (comp == 3 || comp == 6) && (bpp == 16 || bpp == 32) {
        if hsize >= 108 || hsize == 52 || hsize == 56 {
            rm = u32le(b, 54)?;
            gm = u32le(b, 58)?;
            bm = u32le(b, 62)?;
            if hsize >= 56 {
                am = u32le(b, 66)?;
            }
        } else {
            rm = u32le(b, 54)?;
            gm = u32le(b, 58)?;
            bm = u32le(b, 62)?;
        }
    } else if bpp == 32 && hsize >= 108 && comp == 0 {
        // A BI_RGB 32-bit image with a V4/V5 header: alpha only if a mask says so.
        am = u32le(b, 66)?;
    }
    // Palette.
    let mut palette: Vec<u32> = Vec::new();
    if bpp <= 8 {
        let n = if clrused != 0 {
            clrused as usize
        } else {
            1usize << bpp
        };
        let n = n.min(1 << bpp);
        let entry = if core { 3 } else { 4 };
        let at = 14 + hsize;
        let raw = b.get(at..at + n * entry).ok_or(ImgError::Truncated)?;
        palette = raw
            .chunks(entry)
            .map(|c| argb(255, c[2], c[1], c[0]))
            .collect();
    }
    let stride = (w * bpp as usize).div_ceil(32) * 4;
    let pixels = b.get(data_off..).ok_or(ImgError::Truncated)?;
    if pixels.len() < stride * h {
        return Err(ImgError::Truncated);
    }
    let mut img = Image::new(w, h)?;
    for y in 0..h {
        let src_y = if top_down { y } else { h - 1 - y };
        let row = &pixels[src_y * stride..(src_y + 1) * stride];
        for x in 0..w {
            img.px[y * w + x] = match bpp {
                1 | 4 | 8 => {
                    let per = 8 / bpp as usize;
                    let byte = row[x / per] as u32;
                    let shift = 8 - bpp * (1 + (x % per) as u32);
                    let i = ((byte >> shift) & ((1 << bpp) - 1)) as usize;
                    *palette
                        .get(i)
                        .ok_or(ImgError::Corrupt("A BMP pixel points past the palette."))?
                }
                16 => {
                    let v = u16::from_le_bytes([row[2 * x], row[2 * x + 1]]) as u32;
                    argb(
                        if am != 0 { extract(v, am) } else { 255 },
                        extract(v, rm),
                        extract(v, gm),
                        extract(v, bm),
                    )
                }
                24 => argb(255, row[3 * x + 2], row[3 * x + 1], row[3 * x]),
                _ => {
                    let v = u32::from_le_bytes([
                        row[4 * x],
                        row[4 * x + 1],
                        row[4 * x + 2],
                        row[4 * x + 3],
                    ]);
                    argb(
                        if am != 0 { extract(v, am) } else { 255 },
                        extract(v, rm),
                        extract(v, gm),
                        extract(v, bm),
                    )
                }
            };
        }
    }
    Ok(img)
}
