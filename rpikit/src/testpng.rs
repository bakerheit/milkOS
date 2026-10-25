//! Test support: read the repository's own PNG screenshots (8-bit RGB / RGBA, not interlaced) and
//! write PNGs, with no crates: a small inflate, the five scanline filters, and an encoder that
//! stores its data uncompressed.

use std::vec::Vec;

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bit: u32,
    cnt: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Option<u32> {
        while self.cnt < n {
            let b = *self.d.get(self.pos)? as u32;
            self.pos += 1;
            self.bit |= b << self.cnt;
            self.cnt += 8;
        }
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.cnt -= n;
        Some(v)
    }
}

struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Huff {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + count[i];
        }
        let mut symbol = std::vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Huff { count, symbol }
    }
    fn decode(&self, b: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.need(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return self.symbol.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Inflate a zlib stream.
pub fn inflate(z: &[u8]) -> Option<Vec<u8>> {
    let mut b = Bits {
        d: z.get(2..)?,
        pos: 0,
        bit: 0,
        cnt: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = b.need(1)?;
        match b.need(2)? {
            0 => {
                b.bit = 0;
                b.cnt = 0;
                let len = u16::from_le_bytes([*b.d.get(b.pos)?, *b.d.get(b.pos + 1)?]) as usize;
                out.extend_from_slice(b.d.get(b.pos + 4..b.pos + 4 + len)?);
                b.pos += 4 + len;
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 {
                    let mut l = [8u8; 288];
                    l[144..256].fill(9);
                    l[256..280].fill(7);
                    (Huff::new(&l), Huff::new(&[5u8; 30]))
                } else {
                    let nlen = b.need(5)? as usize + 257;
                    let ndist = b.need(5)? as usize + 1;
                    let ncode = b.need(4)? as usize + 4;
                    const ORDER: [usize; 19] = [
                        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                    ];
                    let mut cl = [0u8; 19];
                    for &o in ORDER.iter().take(ncode) {
                        cl[o] = b.need(3)? as u8;
                    }
                    let ch = Huff::new(&cl);
                    let mut lens = Vec::new();
                    while lens.len() < nlen + ndist {
                        let s = ch.decode(&mut b)?;
                        match s {
                            0..=15 => lens.push(s as u8),
                            16 => {
                                let p = *lens.last()?;
                                for _ in 0..3 + b.need(2)? {
                                    lens.push(p);
                                }
                            }
                            17 => {
                                let n = 3 + b.need(3)? as usize;
                                lens.resize(lens.len() + n, 0);
                            }
                            _ => {
                                let n = 11 + b.need(7)? as usize;
                                lens.resize(lens.len() + n, 0);
                            }
                        }
                    }
                    (
                        Huff::new(&lens[..nlen]),
                        Huff::new(&lens[nlen..nlen + ndist]),
                    )
                };
                loop {
                    let s = lit.decode(&mut b)? as usize;
                    if s < 256 {
                        out.push(s as u8);
                    } else if s == 256 {
                        break;
                    } else {
                        let s = s - 257;
                        let len = *LBASE.get(s)? as usize + b.need(LEXT[s] as u32)? as usize;
                        let d = dist.decode(&mut b)? as usize;
                        let off = *DBASE.get(d)? as usize + b.need(DEXT[d] as u32)? as usize;
                        if off > out.len() {
                            return None;
                        }
                        for _ in 0..len {
                            out.push(out[out.len() - off]);
                        }
                    }
                }
            }
            _ => return None,
        }
        if last == 1 {
            return Some(out);
        }
    }
}

/// Decode an 8-bit RGB or RGBA PNG: (width, height, `0x00RRGGBB` pixels).
pub fn decode(png: &[u8]) -> Option<(usize, usize, Vec<u32>)> {
    if png.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let (mut i, mut w, mut h, mut bpp) = (8, 0, 0, 0);
    let mut idat = Vec::new();
    while i + 8 <= png.len() {
        let n = u32::from_be_bytes(png[i..i + 4].try_into().ok()?) as usize;
        let kind = &png[i + 4..i + 8];
        let body = png.get(i + 8..i + 8 + n)?;
        match kind {
            b"IHDR" => {
                w = u32::from_be_bytes(body[0..4].try_into().ok()?) as usize;
                h = u32::from_be_bytes(body[4..8].try_into().ok()?) as usize;
                if body[8] != 8 || body[12] != 0 {
                    return None;
                }
                bpp = match body[9] {
                    2 => 3,
                    6 => 4,
                    _ => return None,
                };
            }
            b"IDAT" => idat.extend_from_slice(body),
            _ => {}
        }
        i += 12 + n;
    }
    let raw = inflate(&idat)?;
    let stride = w * bpp;
    let mut cur = std::vec![0u8; stride];
    let mut prev = std::vec![0u8; stride];
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = raw.get(y * (stride + 1)..(y + 1) * (stride + 1))?;
        let f = row[0];
        for x in 0..stride {
            let a = if x >= bpp { cur[x - bpp] as i32 } else { 0 };
            let b = prev[x] as i32;
            let c = if x >= bpp { prev[x - bpp] as i32 } else { 0 };
            let add = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => return None,
            };
            cur[x] = row[1 + x].wrapping_add(add as u8);
        }
        for x in 0..w {
            out.push(
                (cur[x * bpp] as u32) << 16
                    | (cur[x * bpp + 1] as u32) << 8
                    | cur[x * bpp + 2] as u32,
            );
        }
        core::mem::swap(&mut cur, &mut prev);
    }
    Some((w, h, out))
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// Encode `0x00RRGGBB` pixels as an RGB PNG (stored, not compressed).
pub fn encode(w: usize, h: usize, px: &[u32]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(h * (w * 3 + 1));
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let p = px[y * w + x];
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let mut z = std::vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = std::vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(body);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}
