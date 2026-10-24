//! DEFLATE (RFC 1951) and the zlib wrapper (RFC 1950), after the structure of zlib's `puff.c`:
//! small, bit-at-a-time Huffman decoding, strict about everything that can't be valid. The
//! output is capped by the caller, so a hostile stream can't make us allocate without limit.
//!
//! [`inflate`] and [`zlib_decompress`] take the whole stream. [`Inflater`] is the same decoder
//! made resumable: the compressed bytes are given as they arrive ([`Inflater::feed`]), the
//! decoded bytes are taken a piece at a time ([`Inflater::pump`]), and only the last 32 KiB of
//! output is kept (for back references), so a stream of any length is decoded in constant
//! memory. The web client uses it to hand a page to the Browser while it is still coming.

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

pub(crate) struct Bits<'a> {
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


// ---- a resumable decoder ----------------------------------------------------------------------

/// What [`Inflater::pump`] ran out of.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pump {
    /// The output budget of this call was used up; call again for more.
    More,
    /// All the input given so far is decoded; [`Inflater::feed`] more.
    NeedInput,
    /// The final block ended: [`Inflater::rest`] is what followed it.
    Done,
}

enum St {
    /// Before a block header.
    Block,
    /// Inside a stored block: bytes left.
    Stored(usize),
    /// Inside a Huffman-coded block.
    Codes(alloc::boxed::Box<(Huff, Huff)>),
    Done,
}

/// DEFLATE, a piece at a time. Feed compressed bytes as they arrive, pump decoded ones out.
///
/// Where the input ends in the middle of a symbol or a block header, the decoder goes back to
/// the start of that symbol and waits for more, so the pieces may be cut anywhere.
pub struct Inflater {
    inb: Vec<u8>,
    pos: usize,
    buf: u32,
    cnt: u32,
    st: St,
    last: bool,
    /// The newest output (at most about 64 KiB), for back references.
    win: Vec<u8>,
    /// The most output there may be, in all.
    limit: usize,
    out_total: usize,
}

const WINDOW: usize = 32 * 1024;

impl Inflater {
    /// A decoder whose output is at most `limit` bytes in all (more is [`InflateError::TooBig`]).
    pub fn new(limit: usize) -> Inflater {
        Inflater {
            inb: Vec::new(),
            pos: 0,
            buf: 0,
            cnt: 0,
            st: St::Block,
            last: false,
            win: Vec::new(),
            limit,
            out_total: 0,
        }
    }

    /// Compressed bytes that came.
    pub fn feed(&mut self, data: &[u8]) {
        if self.pos > 64 * 1024 && self.pos * 2 > self.inb.len() {
            self.inb.drain(..self.pos);
            self.pos = 0;
        }
        self.inb.extend_from_slice(data);
    }

    /// The bytes after the end of the stream (once [`Pump::Done`]).
    pub fn rest(&self) -> &[u8] {
        &self.inb[self.pos.min(self.inb.len())..]
    }

    /// Input bytes fed and not yet taken in.
    pub fn pending(&self) -> usize {
        self.inb.len() - self.pos
    }

    /// Bytes of output so far, in all.
    pub fn total_out(&self) -> usize {
        self.out_total
    }

    fn put(win: &mut Vec<u8>, out: &mut Vec<u8>, b: u8) {
        win.push(b);
        out.push(b);
    }

    fn trim(&mut self) {
        if self.win.len() > 2 * WINDOW {
            let cut = self.win.len() - WINDOW;
            self.win.drain(..cut);
        }
    }

    /// Decode what the input so far allows, appending to `out`, until at least `budget` bytes
    /// were appended by this call (it may go a little over: a match copies whole) or the input
    /// runs out. Errors are final.
    pub fn pump(&mut self, out: &mut Vec<u8>, budget: usize) -> Result<Pump, InflateError> {
        let start = out.len();
        loop {
            if out.len() - start >= budget {
                return Ok(Pump::More);
            }
            match &mut self.st {
                St::Done => return Ok(Pump::Done),
                St::Block => {
                    let mut b = Bits { data: &self.inb, pos: self.pos, buf: self.buf, cnt: self.cnt };
                    let (p0, b0, c0) = (self.pos, self.buf, self.cnt);
                    let r: Result<(bool, St), InflateError> = (|| {
                        let last = b.need(1)? == 1;
                        let st = match b.need(2)? {
                            0 => {
                                b.buf = 0;
                                b.cnt = 0;
                                let h = b.data.get(b.pos..b.pos + 4).ok_or(InflateError::Truncated)?;
                                let len = u16::from_le_bytes([h[0], h[1]]) as usize;
                                if u16::from_le_bytes([h[2], h[3]]) != !(len as u16) {
                                    return Err(InflateError::Corrupt("stored block length check failed"));
                                }
                                b.pos += 4;
                                St::Stored(len)
                            }
                            1 => St::Codes(alloc::boxed::Box::new(fixed_tables())),
                            2 => St::Codes(alloc::boxed::Box::new(dynamic_tables(&mut b)?)),
                            _ => return Err(InflateError::Corrupt("bad block type")),
                        };
                        Ok((last, st))
                    })();
                    match r {
                        Ok((last, st)) => {
                            self.pos = b.pos;
                            self.buf = b.buf;
                            self.cnt = b.cnt;
                            self.last = last;
                            self.st = st;
                        }
                        Err(InflateError::Truncated) => {
                            self.pos = p0;
                            self.buf = b0;
                            self.cnt = c0;
                            return Ok(Pump::NeedInput);
                        }
                        Err(e) => return Err(e),
                    }
                }
                St::Stored(left) => {
                    let avail = self.inb.len() - self.pos;
                    if *left == 0 {
                        self.st = if self.last { St::Done } else { St::Block };
                        continue;
                    }
                    if avail == 0 {
                        return Ok(Pump::NeedInput);
                    }
                    let take = (*left).min(avail).min(budget.saturating_sub(out.len() - start).max(1));
                    if self.out_total + take > self.limit {
                        return Err(InflateError::TooBig);
                    }
                    let chunk = &self.inb[self.pos..self.pos + take];
                    out.extend_from_slice(chunk);
                    self.win.extend_from_slice(chunk);
                    self.pos += take;
                    self.out_total += take;
                    *left -= take;
                    self.trim();
                }
                St::Codes(tabs) => {
                    let (lit, dist) = (&tabs.0, &tabs.1);
                    let mut b = Bits { data: &self.inb, pos: self.pos, buf: self.buf, cnt: self.cnt };
                    let mut ended = false;
                    let mut starved = false;
                    while out.len() - start < budget {
                        let (p0, b0, c0) = (b.pos, b.buf, b.cnt);
                        let step: Result<bool, InflateError> = (|| {
                            let sym = lit.decode(&mut b)? as usize;
                            if sym < 256 {
                                if self.out_total >= self.limit {
                                    return Err(InflateError::TooBig);
                                }
                                Inflater::put(&mut self.win, out, sym as u8);
                                self.out_total += 1;
                                Ok(false)
                            } else if sym == 256 {
                                Ok(true)
                            } else {
                                let s = sym - 257;
                                if s >= 29 {
                                    return Err(InflateError::Corrupt("bad length symbol"));
                                }
                                let len = LEN_BASE[s] as usize + b.need(LEN_EXTRA[s] as u32)? as usize;
                                let ds = dist.decode(&mut b)? as usize;
                                if ds >= 30 {
                                    return Err(InflateError::Corrupt("bad distance symbol"));
                                }
                                let d = DIST_BASE[ds] as usize + b.need(DIST_EXTRA[ds] as u32)? as usize;
                                if d > self.win.len() {
                                    return Err(InflateError::Corrupt("distance too far back"));
                                }
                                if self.out_total + len > self.limit {
                                    return Err(InflateError::TooBig);
                                }
                                for _ in 0..len {
                                    let c = self.win[self.win.len() - d];
                                    Inflater::put(&mut self.win, out, c);
                                }
                                self.out_total += len;
                                Ok(false)
                            }
                        })();
                        match step {
                            Ok(false) => {}
                            Ok(true) => {
                                ended = true;
                                break;
                            }
                            Err(InflateError::Truncated) => {
                                b.pos = p0;
                                b.buf = b0;
                                b.cnt = c0;
                                starved = true;
                                break;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    self.pos = b.pos;
                    self.buf = b.buf;
                    self.cnt = b.cnt;
                    self.trim();
                    if ended {
                        self.st = if self.last { St::Done } else { St::Block };
                    } else if starved {
                        return Ok(Pump::NeedInput);
                    }
                }
            }
        }
    }
}

/// CRC-32 continued: `crc` is 0 at the start and what the last call returned after that.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, e) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    let mut c = !crc;
    for &x in data {
        c = table[((c ^ x as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// Adler-32 continued: `adler` is 1 at the start.
pub fn adler32_update(adler: u32, data: &[u8]) -> u32 {
    let (mut a, mut b) = (adler & 0xFFFF, adler >> 16);
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
