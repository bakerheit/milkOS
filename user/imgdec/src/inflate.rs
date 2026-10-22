//! DEFLATE (RFC 1951) and the zlib wrapper (RFC 1950), after the structure of zlib's `puff.c`:
//! small, bit-at-a-time Huffman decoding, strict about everything that can't be valid. The
//! output is capped by the caller, so a hostile stream can't make us allocate without limit.

use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InflateError {
    /// The data ended in the middle of the stream.
    Truncated,
    /// Not a valid DEFLATE stream (the text says what was wrong).
    Corrupt(&'static str),
    /// The result would be bigger than the limit.
    TooBig,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    cnt: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Result<u32, InflateError> {
        while self.cnt < n {
            let b = *self.data.get(self.pos).ok_or(InflateError::Truncated)?;
            self.pos += 1;
            self.buf |= (b as u32) << self.cnt;
            self.cnt += 8;
        }
        let v = self.buf & ((1u32 << n) - 1);
        self.buf >>= n;
        self.cnt -= n;
        Ok(v)
    }
}

const MAX_BITS: usize = 15;

struct Huff {
    count: [u16; MAX_BITS + 1],
    symbol: [u16; 288],
}

impl Huff {
    /// A canonical code from code lengths. Over-subscribed sets are errors; incomplete ones are
    /// allowed (a code that was never assigned fails when it is read).
    fn new(lengths: &[u8]) -> Result<Huff, InflateError> {
        let mut h = Huff {
            count: [0; MAX_BITS + 1],
            symbol: [0; 288],
        };
        for &l in lengths {
            h.count[l as usize] += 1;
        }
        let mut left: i32 = 1;
        for &c in &h.count[1..] {
            left <<= 1;
            left -= c as i32;
            if left < 0 {
                return Err(InflateError::Corrupt("over-subscribed Huffman code"));
            }
        }
        let mut offs = [0u16; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            offs[len + 1] = offs[len] + h.count[len];
        }
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                h.symbol[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(h)
    }

    fn decode(&self, b: &mut Bits) -> Result<u16, InflateError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..=MAX_BITS {
            code |= b.need(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::Corrupt("bad Huffman code"))
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

fn codes(
    b: &mut Bits,
    out: &mut Vec<u8>,
    limit: usize,
    lit: &Huff,
    dist: &Huff,
) -> Result<(), InflateError> {
    loop {
        let sym = lit.decode(b)? as usize;
        if sym < 256 {
            if out.len() >= limit {
                return Err(InflateError::TooBig);
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let s = sym - 257;
            if s >= 29 {
                return Err(InflateError::Corrupt("bad length symbol"));
            }
            let len = LEN_BASE[s] as usize + b.need(LEN_EXTRA[s] as u32)? as usize;
            let ds = dist.decode(b)? as usize;
            if ds >= 30 {
                return Err(InflateError::Corrupt("bad distance symbol"));
            }
            let d = DIST_BASE[ds] as usize + b.need(DIST_EXTRA[ds] as u32)? as usize;
            if d > out.len() {
                return Err(InflateError::Corrupt("distance too far back"));
            }
            if out.len() + len > limit {
                return Err(InflateError::TooBig);
            }
            for _ in 0..len {
                let c = out[out.len() - d];
                out.push(c);
            }
        }
    }
}

fn fixed_tables() -> (Huff, Huff) {
    let mut l = [0u8; 288];
    for (i, v) in l.iter_mut().enumerate() {
        *v = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    // Both tables are complete, so these can't fail.
    (Huff::new(&l).unwrap(), Huff::new(&[5u8; 30]).unwrap())
}

fn dynamic_tables(b: &mut Bits) -> Result<(Huff, Huff), InflateError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let nlen = b.need(5)? as usize + 257;
    let ndist = b.need(5)? as usize + 1;
    let ncode = b.need(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(InflateError::Corrupt("too many length or distance codes"));
    }
    let mut cl = [0u8; 19];
    for &o in ORDER.iter().take(ncode) {
        cl[o] = b.need(3)? as u8;
    }
    let clh = Huff::new(&cl)?;
    let mut lengths = [0u8; 286 + 30];
    let mut i = 0;
    while i < nlen + ndist {
        let sym = clh.decode(b)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
        } else {
            let (prev, rep) = match sym {
                16 => {
                    if i == 0 {
                        return Err(InflateError::Corrupt("repeat with nothing before it"));
                    }
                    (lengths[i - 1], 3 + b.need(2)? as usize)
                }
                17 => (0, 3 + b.need(3)? as usize),
                _ => (0, 11 + b.need(7)? as usize),
            };
            if i + rep > nlen + ndist {
                return Err(InflateError::Corrupt("code lengths run past the end"));
            }
            for _ in 0..rep {
                lengths[i] = prev;
                i += 1;
            }
        }
    }
    if lengths[256] == 0 {
        return Err(InflateError::Corrupt("no end-of-block code"));
    }
    Ok((
        Huff::new(&lengths[..nlen])?,
        Huff::new(&lengths[nlen..nlen + ndist])?,
    ))
}

/// Inflate a raw DEFLATE stream. Returns the data and how many input bytes the stream used
/// (rounded up to a whole byte). `limit` caps the output.
pub fn inflate(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), InflateError> {
    let mut b = Bits {
        data,
        pos: 0,
        buf: 0,
        cnt: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = b.need(1)?;
        match b.need(2)? {
            0 => {
                b.buf = 0;
                b.cnt = 0; // drop the bits up to the byte boundary
                let h = data.get(b.pos..b.pos + 4).ok_or(InflateError::Truncated)?;
                let len = u16::from_le_bytes([h[0], h[1]]) as usize;
                if u16::from_le_bytes([h[2], h[3]]) != !(len as u16) {
                    return Err(InflateError::Corrupt("stored block length check failed"));
                }
                b.pos += 4;
                let body = data
                    .get(b.pos..b.pos + len)
                    .ok_or(InflateError::Truncated)?;
                if out.len() + len > limit {
                    return Err(InflateError::TooBig);
                }
                out.extend_from_slice(body);
                b.pos += len;
            }
            1 => {
                let (l, d) = fixed_tables();
                codes(&mut b, &mut out, limit, &l, &d)?;
            }
            2 => {
                let (l, d) = dynamic_tables(&mut b)?;
                codes(&mut b, &mut out, limit, &l, &d)?;
            }
            _ => return Err(InflateError::Corrupt("bad block type")),
        }
        if last == 1 {
            return Ok((out, b.pos));
        }
    }
}

/// Adler-32 (RFC 1950).
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

/// Inflate a zlib stream (2-byte header, DEFLATE, Adler-32), checking the header and checksum.
pub fn zlib_decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, InflateError> {
    if data.len() < 2 {
        return Err(InflateError::Truncated);
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (cmf as u32 * 256 + flg as u32) % 31 != 0 {
        return Err(InflateError::Corrupt("not a zlib stream"));
    }
    if flg & 0x20 != 0 {
        return Err(InflateError::Corrupt("zlib preset dictionary"));
    }
    let (out, used) = inflate(&data[2..], limit)?;
    let at = 2 + used;
    let sum = data.get(at..at + 4).ok_or(InflateError::Truncated)?;
    if u32::from_be_bytes([sum[0], sum[1], sum[2], sum[3]]) != adler32(&out) {
        return Err(InflateError::Corrupt("checksum mismatch"));
    }
    Ok(out)
}

/// CRC-32 (the PNG / zlib one).
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for (n, e) in table.iter_mut().enumerate() {
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
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &x in *p {
            c = table[((c ^ x as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    !c
}
