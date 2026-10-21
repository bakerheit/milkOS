//! A small PNG encoder for screenshots: 8-bit RGB, no interlace, filter type 0 per row,
//! one zlib stream compressed with a single fixed-Huffman deflate block and a greedy LZ77
//! matcher (a 3-byte hash head per position, plus "same spot on the row above", which is
//! what UI pixels repeat most). CRC-32 and Adler-32 per the specs. Desktop screenshots
//! (flat fills, text) come out at a few percent of the raw size.

use alloc::vec;
use alloc::vec::Vec;

// ---- checksums -------------------------------------------------------------------------

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (n, e) in t.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *e = c;
    }
    t
}

/// CRC-32 (ISO 3309 / PNG), over the concatenation of `parts`.
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let t = crc_table();
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &b in *p {
            c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

// ---- deflate (fixed Huffman) -------------------------------------------------------------

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// `count` bits of `v`, least significant first (deflate's bit order).
    fn put(&mut self, v: u32, count: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// A Huffman code: most significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        let mut r = 0;
        for i in 0..len {
            r |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.put(r, len);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn lit(b: &mut Bits, v: u32) {
    match v {
        0..=143 => b.code(0x30 + v, 8),
        144..=255 => b.code(0x190 + v - 144, 9),
        256..=279 => b.code(v - 256, 7),
        _ => b.code(0xC0 + v - 280, 8),
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn put_match(b: &mut Bits, len: usize, dist: usize) {
    let li = LEN_BASE
        .iter()
        .rposition(|&x| x as usize <= len)
        .unwrap_or(0);
    lit(b, 257 + li as u32);
    b.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
    let di = DIST_BASE
        .iter()
        .rposition(|&x| x as usize <= dist)
        .unwrap_or(0);
    b.code(di as u32, 5);
    b.put(
        (dist - DIST_BASE[di] as usize) as u32,
        DIST_EXTRA[di] as u32,
    );
}

const WINDOW: usize = 32768;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;

fn match_len(d: &[u8], a: usize, b: usize) -> usize {
    let max = MAX_MATCH.min(d.len() - b);
    let mut n = 0;
    while n < max && d[a + n] == d[b + n] {
        n += 1;
    }
    n
}

/// Raw deflate of `d`: one final fixed-Huffman block. `row` is the scanline length in
/// bytes, tried as a match distance at every position.
pub fn deflate(d: &[u8], row: usize) -> Vec<u8> {
    let mut b = Bits {
        out: Vec::with_capacity(d.len() / 8 + 64),
        acc: 0,
        n: 0,
    };
    b.put(1, 1); // BFINAL
    b.put(1, 2); // BTYPE = 01, fixed Huffman
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let hash = |i: usize| -> usize {
        let v = (d[i] as u32) | (d[i + 1] as u32) << 8 | (d[i + 2] as u32) << 16;
        (v.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
    };
    let mut i = 0;
    while i < d.len() {
        let (mut best_len, mut best_dist) = (0, 0);
        if i + 3 <= d.len() {
            let h = hash(i);
            let cand = head[h];
            head[h] = i;
            for c in [cand, i.wrapping_sub(row), i.wrapping_sub(1)] {
                if c < i && i - c <= WINDOW {
                    let l = match_len(d, c, i);
                    if l > best_len {
                        best_len = l;
                        best_dist = i - c;
                    }
                }
            }
        }
        if best_len >= 3 {
            put_match(&mut b, best_len, best_dist);
            // Keep the hash table fresh for a few positions inside the match.
            for k in i + 1..(i + best_len).min(d.len().saturating_sub(2)).min(i + 4) {
                head[hash(k)] = k;
            }
            i += best_len;
        } else {
            lit(&mut b, d[i] as u32);
            i += 1;
        }
    }
    lit(&mut b, 256); // end of block
    b.finish()
}

// ---- PNG ---------------------------------------------------------------------------------

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Encode `w` x `h` XRGB pixels (`stride` pixels per row) as a PNG file.
pub fn encode(px: &[u32], w: usize, h: usize, stride: usize) -> Vec<u8> {
    // Scanlines: a filter byte (0, none) then RGB.
    let row = 1 + 3 * w;
    let mut raw = Vec::with_capacity(row * h);
    for y in 0..h {
        raw.push(0);
        for &p in &px[y * stride..y * stride + w] {
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
    }
    let mut z = vec![0x78, 0x01]; // zlib: deflate, 32K window, no dictionary
    z.extend_from_slice(&deflate(&raw, row));
    z.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour, deflate, filter 0, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
