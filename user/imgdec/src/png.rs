//! PNG: every colour type and bit depth, tRNS transparency, Adam7 interlace, chunk CRCs and the
//! zlib checksum checked. Gamma, ICC and the other ancillary chunks are ignored.

use alloc::vec::Vec;

use crate::inflate::{InflateError, crc32, zlib_decompress};
use crate::{Image, ImgError, MAX_PIXELS, argb, to8};

const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

struct Header {
    w: usize,
    h: usize,
    depth: u32,
    ctype: u8,
    interlace: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.ctype {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }

    fn bits_per_pixel(&self) -> usize {
        self.channels() * self.depth as usize
    }

    /// Bytes in one scanline of `w` pixels (without the filter byte).
    fn row_bytes(&self, w: usize) -> usize {
        (w * self.bits_per_pixel()).div_ceil(8)
    }
}

/// The Adam7 passes: (x start, y start, x step, y step).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn map_inflate(e: InflateError) -> ImgError {
    match e {
        InflateError::Truncated => ImgError::Truncated,
        InflateError::TooBig => ImgError::Corrupt("The picture data is longer than the image."),
        InflateError::Corrupt(_) => ImgError::Corrupt("The picture data is damaged."),
    }
}

pub fn decode(file: &[u8]) -> Result<Image, ImgError> {
    if !file.starts_with(&SIG) {
        return Err(ImgError::Unrecognised("This isn't a PNG file."));
    }
    let mut pos = 8;
    let mut hdr: Option<Header> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut seen_idat = false;
    let mut first = true;
    loop {
        let Some(head) = file.get(pos..pos + 8) else {
            return Err(ImgError::Truncated);
        };
        let len = be32(head) as usize;
        let kind: [u8; 4] = [head[4], head[5], head[6], head[7]];
        if len > 0x7FFF_FFFF {
            return Err(ImgError::Corrupt("A PNG chunk has an impossible length."));
        }
        let body = file
            .get(pos + 8..pos + 8 + len)
            .ok_or(ImgError::Truncated)?;
        let crc = file
            .get(pos + 8 + len..pos + 12 + len)
            .ok_or(ImgError::Truncated)?;
        if be32(crc) != crc32(&[&kind, body]) {
            return Err(ImgError::Corrupt("A PNG chunk is damaged (bad checksum)."));
        }
        pos += 12 + len;
        if first && &kind != b"IHDR" {
            return Err(ImgError::Corrupt("The PNG doesn't start with a header."));
        }
        first = false;
        match &kind {
            b"IHDR" => {
                if hdr.is_some() || len != 13 {
                    return Err(ImgError::Corrupt("The PNG header is wrong."));
                }
                hdr = Some(parse_header(body)?);
            }
            b"PLTE" => {
                if len % 3 != 0 || len == 0 || len > 768 || seen_idat {
                    return Err(ImgError::Corrupt("The PNG palette is wrong."));
                }
                palette = body.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
            }
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => {
                seen_idat = true;
                if idat.len() + len > MAX_PIXELS * 9 {
                    return Err(ImgError::TooBig);
                }
                idat.extend_from_slice(body);
            }
            b"IEND" => break,
            _ => {}
        }
    }
    let h = hdr.ok_or(ImgError::Corrupt("The PNG has no header."))?;
    if !seen_idat {
        return Err(ImgError::Corrupt("The PNG has no picture data."));
    }
    if h.ctype == 3 && palette.is_empty() {
        return Err(ImgError::Corrupt("The PNG needs a palette and has none."));
    }

    // How much data the image needs, before asking for it.
    let passes: Vec<(usize, usize, usize, usize)> = if h.interlace {
        ADAM7.to_vec()
    } else {
        alloc::vec![(0, 0, 1, 1)]
    };
    let mut want = 0usize;
    for &(x0, y0, dx, dy) in &passes {
        let (pw, ph) = pass_size(&h, x0, y0, dx, dy);
        if pw > 0 && ph > 0 {
            want += ph * (1 + h.row_bytes(pw));
        }
    }
    let raw = zlib_decompress(&idat, want).map_err(map_inflate)?;
    if raw.len() != want {
        return Err(ImgError::Truncated);
    }
    drop(idat);

    let mut img = Image::new(h.w, h.h)?;
    let bpp = h.bits_per_pixel().div_ceil(8).max(1);
    let mut at = 0;
    for &(x0, y0, dx, dy) in &passes {
        let (pw, ph) = pass_size(&h, x0, y0, dx, dy);
        if pw == 0 || ph == 0 {
            continue;
        }
        let rb = h.row_bytes(pw);
        let mut prev = alloc::vec![0u8; rb];
        let mut cur = alloc::vec![0u8; rb];
        for j in 0..ph {
            let ft = raw[at];
            cur.copy_from_slice(&raw[at + 1..at + 1 + rb]);
            at += 1 + rb;
            unfilter(ft, &mut cur, &prev, bpp)?;
            for i in 0..pw {
                let p = pixel(&h, &cur, i, &palette, &trns)?;
                img.px[(y0 + j * dy) * h.w + x0 + i * dx] = p;
            }
            core::mem::swap(&mut prev, &mut cur);
        }
    }
    Ok(img)
}

fn pass_size(h: &Header, x0: usize, y0: usize, dx: usize, dy: usize) -> (usize, usize) {
    (
        (h.w + dx - 1).saturating_sub(x0) / dx,
        (h.h + dy - 1).saturating_sub(y0) / dy,
    )
}

fn parse_header(b: &[u8]) -> Result<Header, ImgError> {
    let (w, h) = (be32(&b[0..4]) as usize, be32(&b[4..8]) as usize);
    let (depth, ctype, comp, filt, il) = (b[8] as u32, b[9], b[10], b[11], b[12]);
    if w == 0 || h == 0 || w > 0x7FFF_FFFF || h > 0x7FFF_FFFF {
        return Err(ImgError::Corrupt("The PNG has no size."));
    }
    if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err(ImgError::TooBig);
    }
    let ok = match ctype {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    if !ok {
        return Err(ImgError::Corrupt(
            "The PNG has a colour type or depth that isn't allowed.",
        ));
    }
    if comp != 0 || filt != 0 || il > 1 {
        return Err(ImgError::Unsupported(
            "The PNG uses a method this reader doesn't know.",
        ));
    }
    Ok(Header {
        w,
        h,
        depth,
        ctype,
        interlace: il == 1,
    })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i32, b as i32, c as i32);
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn unfilter(ft: u8, cur: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), ImgError> {
    match ft {
        0 => {}
        1 => {
            for i in bpp..cur.len() {
                cur[i] = cur[i].wrapping_add(cur[i - bpp]);
            }
        }
        2 => {
            for i in 0..cur.len() {
                cur[i] = cur[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..cur.len() {
                let a = if i >= bpp { cur[i - bpp] as u16 } else { 0 };
                cur[i] = cur[i].wrapping_add(((a + prev[i] as u16) / 2) as u8);
            }
        }
        4 => {
            for i in 0..cur.len() {
                let (a, c) = if i >= bpp {
                    (cur[i - bpp], prev[i - bpp])
                } else {
                    (0, 0)
                };
                cur[i] = cur[i].wrapping_add(paeth(a, prev[i], c));
            }
        }
        _ => return Err(ImgError::Corrupt("A PNG row has an unknown filter.")),
    }
    Ok(())
}

/// Sample `i` (counting samples, not pixels) of a scanline, raw, at the image's depth.
fn sample(row: &[u8], i: usize, depth: u32) -> u32 {
    match depth {
        8 => row[i] as u32,
        16 => (row[2 * i] as u32) << 8 | row[2 * i + 1] as u32,
        d => {
            let per = 8 / d as usize;
            let byte = row[i / per] as u32;
            let shift = 8 - d * (1 + (i % per) as u32);
            (byte >> shift) & ((1 << d) - 1)
        }
    }
}

fn pixel(
    h: &Header,
    row: &[u8],
    x: usize,
    palette: &[[u8; 3]],
    trns: &[u8],
) -> Result<u32, ImgError> {
    let d = h.depth;
    let key = |n: usize| -> Option<u32> {
        (trns.len() >= 2 * n)
            .then(|| u16::from_be_bytes([trns[2 * (n - 1)], trns[2 * (n - 1) + 1]]) as u32)
    };
    Ok(match h.ctype {
        0 => {
            let v = sample(row, x, d);
            let g = to8(v, d);
            let a = if trns.len() >= 2 && key(1) == Some(v) {
                0
            } else {
                255
            };
            argb(a, g, g, g)
        }
        2 => {
            let (r, g, b) = (
                sample(row, 3 * x, d),
                sample(row, 3 * x + 1, d),
                sample(row, 3 * x + 2, d),
            );
            let a = if trns.len() >= 6
                && u16::from_be_bytes([trns[0], trns[1]]) as u32 == r
                && u16::from_be_bytes([trns[2], trns[3]]) as u32 == g
                && u16::from_be_bytes([trns[4], trns[5]]) as u32 == b
            {
                0
            } else {
                255
            };
            argb(a, to8(r, d), to8(g, d), to8(b, d))
        }
        3 => {
            let i = sample(row, x, d) as usize;
            let c = palette
                .get(i)
                .ok_or(ImgError::Corrupt("A PNG pixel points past the palette."))?;
            argb(trns.get(i).copied().unwrap_or(255), c[0], c[1], c[2])
        }
        4 => {
            let g = to8(sample(row, 2 * x, d), d);
            argb(to8(sample(row, 2 * x + 1, d), d), g, g, g)
        }
        _ => argb(
            to8(sample(row, 4 * x + 3, d), d),
            to8(sample(row, 4 * x, d), d),
            to8(sample(row, 4 * x + 1, d), d),
            to8(sample(row, 4 * x + 2, d), d),
        ),
    })
}
