//! VP8L, lossless WebP (RFC 9649 section 3): the four transforms (predictor, cross colour,
//! subtract green, colour indexing), the colour cache, meta Huffman codes and LZ77 copies with
//! the 2-D distance map. Also used for the lossless-compressed alpha of lossy WebP.

use alloc::vec;
use alloc::vec::Vec;

use crate::{Image, ImgError, MAX_PIXELS};

fn corrupt() -> ImgError {
    ImgError::Corrupt("The WebP picture data is damaged.")
}

/// LSB-first bits. Past the end it gives zeros and remembers that it did.
pub(super) struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    over: usize,
}

impl<'a> Bits<'a> {
    pub(super) fn new(d: &'a [u8]) -> Bits<'a> {
        Bits {
            d,
            pos: 0,
            acc: 0,
            n: 0,
            over: 0,
        }
    }

    fn fill(&mut self) {
        while self.n <= 56 {
            let b = match self.d.get(self.pos) {
                Some(&b) => b,
                None => {
                    self.over += 1;
                    0
                }
            };
            self.pos += 1;
            self.acc |= (b as u64) << self.n;
            self.n += 8;
        }
    }

    fn peek(&mut self, k: u32) -> u32 {
        if self.n < k {
            self.fill();
        }
        (self.acc & ((1u64 << k) - 1)) as u32
    }

    fn skip(&mut self, k: u32) {
        self.acc >>= k;
        self.n -= k;
    }

    pub(super) fn read(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        let v = self.peek(k);
        self.skip(k);
        v
    }

    /// Read past the end of the data (more than the look-ahead).
    fn eos(&self) -> bool {
        self.over > 8
    }
}

const LOOKUP_BITS: u32 = 8;

/// A canonical Huffman code read with the first bit as the code's most significant one.
struct Huff {
    /// For each 8-bit peek: (length << 16 | symbol), length 0 = longer code.
    fast: Vec<u32>,
    count: [u16; 16],
    symbols: Vec<u16>,
    /// A code with a single symbol: reading it takes no bits.
    single: Option<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Result<Huff, ImgError> {
        let mut count = [0u16; 16];
        let mut nonzero = 0;
        let mut last = 0;
        for (s, &l) in lengths.iter().enumerate() {
            if l > 15 {
                return Err(corrupt());
            }
            if l > 0 {
                count[l as usize] += 1;
                nonzero += 1;
                last = s;
            }
        }
        if nonzero == 0 {
            return Err(corrupt());
        }
        if nonzero == 1 {
            return Ok(Huff {
                fast: Vec::new(),
                count,
                symbols: Vec::new(),
                single: Some(last as u16),
            });
        }
        // Complete codes only.
        let mut left: i64 = 1;
        for &c in &count[1..] {
            left = (left << 1) - c as i64;
            if left < 0 {
                return Err(corrupt());
            }
        }
        if left != 0 {
            return Err(corrupt());
        }
        let mut offs = [0u16; 17];
        for len in 1..16 {
            offs[len + 1] = offs[len] + count[len];
        }
        let mut symbols = vec![0u16; nonzero];
        let mut next = offs;
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                symbols[next[l as usize] as usize] = s as u16;
                next[l as usize] += 1;
            }
        }
        // The fast table: walk the codes in canonical order; index = code bits reversed.
        let mut fast = vec![0u32; 1 << LOOKUP_BITS];
        let mut code = 0u32;
        let mut k = 0usize;
        for len in 1..=15u32 {
            for _ in 0..count[len as usize] {
                if len <= LOOKUP_BITS {
                    let rev = code.reverse_bits() >> (32 - len);
                    let mut i = rev as usize;
                    while i < fast.len() {
                        fast[i] = len << 16 | symbols[k] as u32;
                        i += 1 << len;
                    }
                }
                code += 1;
                k += 1;
            }
            code <<= 1;
        }
        Ok(Huff {
            fast,
            count,
            symbols,
            single: None,
        })
    }

    fn read(&self, b: &mut Bits) -> Result<u16, ImgError> {
        if let Some(s) = self.single {
            return Ok(s);
        }
        let look = b.peek(15);
        let f = self.fast[(look & ((1 << LOOKUP_BITS) - 1)) as usize];
        if f != 0 {
            b.skip(f >> 16);
            return Ok(f as u16);
        }
        // Bit by bit (puff style).
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= ((look >> (len - 1)) & 1) as i32;
            let c = self.count[len] as i32;
            if code - c < first {
                b.skip(len as u32);
                return self
                    .symbols
                    .get((index + code - first) as usize)
                    .copied()
                    .ok_or_else(corrupt);
            }
            index += c;
            first = (first + c) << 1;
            code <<= 1;
        }
        Err(corrupt())
    }
}

const CODE_LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

fn read_code(b: &mut Bits, alphabet: usize) -> Result<Huff, ImgError> {
    let mut lengths = vec![0u8; alphabet];
    if b.read(1) == 1 {
        // Simple: one or two symbols.
        let n = b.read(1) + 1;
        let first_bits = if b.read(1) == 0 { 1 } else { 8 };
        let s0 = b.read(first_bits) as usize;
        *lengths.get_mut(s0).ok_or_else(corrupt)? = 1;
        if n == 2 {
            let s1 = b.read(8) as usize;
            *lengths.get_mut(s1).ok_or_else(corrupt)? = 1;
        }
    } else {
        let mut cl = [0u8; 19];
        let n = b.read(4) as usize + 4;
        for &i in CODE_LENGTH_ORDER.iter().take(n) {
            cl[i] = b.read(3) as u8;
        }
        let clh = Huff::new(&cl)?;
        let mut max = alphabet;
        if b.read(1) == 1 {
            let nbits = 2 + 2 * b.read(3);
            max = 2 + b.read(nbits) as usize;
            if max > alphabet {
                return Err(corrupt());
            }
        }
        let mut sym = 0;
        let mut prev = 8u8;
        while sym < alphabet {
            if max == 0 {
                break;
            }
            max -= 1;
            if b.eos() {
                return Err(ImgError::Truncated);
            }
            let c = clh.read(b)?;
            if c < 16 {
                lengths[sym] = c as u8;
                sym += 1;
                if c != 0 {
                    prev = c as u8;
                }
            } else {
                let (extra, offset, val) = match c {
                    16 => (2, 3, prev),
                    17 => (3, 3, 0),
                    _ => (7, 11, 0),
                };
                let rep = b.read(extra) as usize + offset;
                if sym + rep > alphabet {
                    return Err(corrupt());
                }
                lengths[sym..sym + rep].fill(val);
                sym += rep;
            }
        }
    }
    Huff::new(&lengths)
}

/// The five codes for one group: green/length/cache, red, blue, alpha, distance.
struct Group([Huff; 5]);

fn read_group(b: &mut Bits, cache_size: usize) -> Result<Group, ImgError> {
    Ok(Group([
        read_code(b, 256 + 24 + cache_size)?,
        read_code(b, 256)?,
        read_code(b, 256)?,
        read_code(b, 256)?,
        read_code(b, 40)?,
    ]))
}

enum Transform {
    Predictor {
        bits: u32,
        data: Vec<u32>,
        w: usize,
    },
    Color {
        bits: u32,
        data: Vec<u32>,
        w: usize,
    },
    SubtractGreen,
    /// Palette; `bits` pixels are packed per byte when the palette is small; `w` is the width
    /// of the picture before packing.
    Index {
        palette: Vec<u32>,
        bits: u32,
        w: usize,
    },
}

const DIST_MAP: [(i8, i8); 120] = [
    (0, 1),
    (1, 0),
    (1, 1),
    (-1, 1),
    (0, 2),
    (2, 0),
    (1, 2),
    (-1, 2),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (0, 3),
    (3, 0),
    (1, 3),
    (-1, 3),
    (3, 1),
    (-3, 1),
    (2, 3),
    (-2, 3),
    (3, 2),
    (-3, 2),
    (0, 4),
    (4, 0),
    (1, 4),
    (-1, 4),
    (4, 1),
    (-4, 1),
    (3, 3),
    (-3, 3),
    (2, 4),
    (-2, 4),
    (4, 2),
    (-4, 2),
    (0, 5),
    (3, 4),
    (-3, 4),
    (4, 3),
    (-4, 3),
    (5, 0),
    (1, 5),
    (-1, 5),
    (5, 1),
    (-5, 1),
    (2, 5),
    (-2, 5),
    (5, 2),
    (-5, 2),
    (4, 4),
    (-4, 4),
    (3, 5),
    (-3, 5),
    (5, 3),
    (-5, 3),
    (0, 6),
    (6, 0),
    (1, 6),
    (-1, 6),
    (6, 1),
    (-6, 1),
    (2, 6),
    (-2, 6),
    (6, 2),
    (-6, 2),
    (4, 5),
    (-4, 5),
    (5, 4),
    (-5, 4),
    (3, 6),
    (-3, 6),
    (6, 3),
    (-6, 3),
    (0, 7),
    (7, 0),
    (1, 7),
    (-1, 7),
    (5, 5),
    (-5, 5),
    (7, 1),
    (-7, 1),
    (4, 6),
    (-4, 6),
    (6, 4),
    (-6, 4),
    (2, 7),
    (-2, 7),
    (7, 2),
    (-7, 2),
    (3, 7),
    (-3, 7),
    (7, 3),
    (-7, 3),
    (5, 6),
    (-5, 6),
    (6, 5),
    (-6, 5),
    (8, 0),
    (4, 7),
    (-4, 7),
    (7, 4),
    (-7, 4),
    (8, 1),
    (8, 2),
    (6, 6),
    (-6, 6),
    (8, 3),
    (5, 7),
    (-5, 7),
    (7, 5),
    (-7, 5),
    (8, 4),
    (6, 7),
    (-6, 7),
    (7, 6),
    (-7, 6),
    (8, 5),
    (7, 7),
    (-7, 7),
    (8, 6),
    (8, 7),
];

fn prefix_value(sym: u32, b: &mut Bits) -> usize {
    if sym < 4 {
        return sym as usize + 1;
    }
    let extra = (sym - 2) >> 1;
    let offset = (2 + (sym & 1)) << extra;
    (offset + b.read(extra)) as usize + 1
}

fn sub_size(n: usize, bits: u32) -> usize {
    n.div_ceil(1 << bits)
}

/// One image stream (RFC 9649 5): `level0` is the picture itself, which may have transforms
/// and meta codes; the others are the sub-images that describe them.
fn image_stream(b: &mut Bits, w: usize, h: usize, level0: bool) -> Result<Vec<u32>, ImgError> {
    if w == 0 || h == 0 || w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err(ImgError::TooBig);
    }
    let mut transforms: Vec<Transform> = Vec::new();
    let mut xsize = w;
    if level0 {
        let mut seen = 0u8;
        while b.read(1) == 1 {
            let t = b.read(2);
            if seen & (1 << t) != 0 {
                return Err(corrupt());
            }
            seen |= 1 << t;
            match t {
                0 | 1 => {
                    let bits = b.read(3) + 2;
                    let (sw, sh) = (sub_size(xsize, bits), sub_size(h, bits));
                    let data = image_stream(b, sw, sh, false)?;
                    transforms.push(if t == 0 {
                        Transform::Predictor {
                            bits,
                            data,
                            w: xsize,
                        }
                    } else {
                        Transform::Color {
                            bits,
                            data,
                            w: xsize,
                        }
                    });
                }
                2 => transforms.push(Transform::SubtractGreen),
                _ => {
                    let n = b.read(8) as usize + 1;
                    let mut pal = image_stream(b, n, 1, false)?;
                    for i in 1..n {
                        pal[i] = add_pixels(pal[i], pal[i - 1]);
                    }
                    let bits = match n {
                        0..=2 => 3,
                        3..=4 => 2,
                        5..=16 => 1,
                        _ => 0,
                    };
                    transforms.push(Transform::Index {
                        palette: pal,
                        bits,
                        w: xsize,
                    });
                    xsize = sub_size(xsize, bits);
                }
            }
        }
    }
    let cache_bits = if b.read(1) == 1 {
        let c = b.read(4);
        if !(1..=11).contains(&c) {
            return Err(corrupt());
        }
        c
    } else {
        0
    };
    let cache_size = if cache_bits > 0 {
        1usize << cache_bits
    } else {
        0
    };
    // Meta Huffman codes: an entropy image saying which group each tile uses.
    let mut meta: Option<(u32, usize, Vec<u32>)> = None;
    let mut ngroups = 1usize;
    if level0 && b.read(1) == 1 {
        let bits = b.read(3) + 2;
        let (ew, eh) = (sub_size(xsize, bits), sub_size(h, bits));
        let mut e = image_stream(b, ew, eh, false)?;
        for p in e.iter_mut() {
            *p = (*p >> 8) & 0xFFFF;
            ngroups = ngroups.max(*p as usize + 1);
        }
        meta = Some((bits, ew, e));
    }
    // Groups the entropy image never uses still have to be read; keep only the used ones.
    let used: Vec<bool> = match &meta {
        Some((_, _, e)) => {
            let mut u = vec![false; ngroups];
            for &g in e {
                u[g as usize] = true;
            }
            u
        }
        None => vec![true],
    };
    let mut groups: Vec<Option<Group>> = Vec::with_capacity(ngroups.min(4096));
    for &u in used.iter() {
        if b.eos() {
            return Err(ImgError::Truncated);
        }
        let g = read_group(b, cache_size)?;
        groups.push(if u { Some(g) } else { None });
    }
    let data = decode_pixels(b, xsize, h, &groups, meta.as_ref(), cache_bits)?;
    // Undo the transforms, last read first.
    let mut px = data;
    for t in transforms.iter().rev() {
        px = match t {
            Transform::SubtractGreen => {
                for p in px.iter_mut() {
                    let g = (*p >> 8) & 0xFF;
                    let r = ((*p >> 16) + g) & 0xFF;
                    let bl = ((*p & 0xFF) + g) & 0xFF;
                    *p = (*p & 0xFF00_FF00) | r << 16 | bl;
                }
                px
            }
            Transform::Predictor { bits, data, w } => {
                predict(&mut px, *w, h, *bits, data);
                px
            }
            Transform::Color { bits, data, w } => {
                cross_color(&mut px, *w, h, *bits, data);
                px
            }
            Transform::Index { palette, bits, w } => unpack_index(&px, *w, h, *bits, palette),
        };
    }
    Ok(px)
}

fn decode_pixels(
    b: &mut Bits,
    w: usize,
    h: usize,
    groups: &[Option<Group>],
    meta: Option<&(u32, usize, Vec<u32>)>,
    cache_bits: u32,
) -> Result<Vec<u32>, ImgError> {
    let total = w * h;
    let mut px: Vec<u32> = Vec::with_capacity(total);
    let mut cache = vec![0u32; if cache_bits > 0 { 1 << cache_bits } else { 0 }];
    let hash_shift = 32 - cache_bits;
    let insert = |cache: &mut [u32], p: u32| {
        if !cache.is_empty() {
            cache[(p.wrapping_mul(0x1e35a7bd) >> hash_shift) as usize] = p;
        }
    };
    let group_at = |i: usize| -> Result<&Group, ImgError> {
        let g = match meta {
            Some((bits, ew, e)) => {
                let (x, y) = (i % w, i / w);
                e[(y >> bits) * ew + (x >> bits)] as usize
            }
            None => 0,
        };
        groups.get(g).and_then(|g| g.as_ref()).ok_or_else(corrupt)
    };
    while px.len() < total {
        if b.eos() {
            return Err(ImgError::Truncated);
        }
        let g = group_at(px.len())?;
        let code = g.0[0].read(b)? as usize;
        if code < 256 {
            let r = g.0[1].read(b)? as u32;
            let bl = g.0[2].read(b)? as u32;
            let a = g.0[3].read(b)? as u32;
            let p = a << 24 | r << 16 | (code as u32) << 8 | bl;
            px.push(p);
            insert(&mut cache, p);
        } else if code < 256 + 24 {
            let len = prefix_value(code as u32 - 256, b);
            let dsym = g.0[4].read(b)? as u32;
            let dcode = prefix_value(dsym, b);
            let dist = if dcode > 120 {
                dcode - 120
            } else {
                let (dx, dy) = DIST_MAP[dcode - 1];
                let d = dy as isize * w as isize + dx as isize;
                d.max(1) as usize
            };
            if dist > px.len() || px.len() + len > total {
                return Err(corrupt());
            }
            for _ in 0..len {
                let p = px[px.len() - dist];
                px.push(p);
                insert(&mut cache, p);
            }
        } else {
            let key = code - 256 - 24;
            let p = *cache.get(key).ok_or_else(corrupt)?;
            px.push(p);
            insert(&mut cache, p);
        }
    }
    Ok(px)
}

fn add_pixels(a: u32, b: u32) -> u32 {
    let ag = (a & 0xFF00_FF00).wrapping_add(b & 0xFF00_FF00) & 0xFF00_FF00;
    let rb = (a & 0x00FF_00FF).wrapping_add(b & 0x00FF_00FF) & 0x00FF_00FF;
    ag | rb
}

fn avg2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xFEFE_FEFE) >> 1).wrapping_add(a & b)
}

fn ch(p: u32, s: u32) -> i32 {
    ((p >> s) & 0xFF) as i32
}

fn select(t: u32, l: u32, tl: u32) -> u32 {
    let mut d = 0;
    for s in [24, 16, 8, 0] {
        d += (ch(l, s) - ch(tl, s)).abs() - (ch(t, s) - ch(tl, s)).abs();
    }
    if d <= 0 { t } else { l }
}

fn clamp_full(a: u32, b: u32, c: u32) -> u32 {
    let mut out = 0;
    for s in [24, 16, 8, 0] {
        out |= ((ch(a, s) + ch(b, s) - ch(c, s)).clamp(0, 255) as u32) << s;
    }
    out
}

fn clamp_half(a: u32, b: u32) -> u32 {
    let mut out = 0;
    for s in [24, 16, 8, 0] {
        let (x, y) = (ch(a, s), ch(b, s));
        out |= ((x + (x - y) / 2).clamp(0, 255) as u32) << s;
    }
    out
}

fn predict(px: &mut [u32], w: usize, h: usize, bits: u32, modes: &[u32]) {
    let tw = sub_size(w, bits);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let pred = if y == 0 {
                if x == 0 { 0xFF00_0000 } else { px[i - 1] }
            } else if x == 0 {
                px[i - w]
            } else {
                let mode = (modes[(y >> bits) * tw + (x >> bits)] >> 8) & 15;
                let (l, t, tl, tr) = (px[i - 1], px[i - w], px[i - w - 1], px[i - w + 1]);
                match mode {
                    1 => l,
                    2 => t,
                    3 => tr,
                    4 => tl,
                    5 => avg2(avg2(l, tr), t),
                    6 => avg2(l, tl),
                    7 => avg2(l, t),
                    8 => avg2(tl, t),
                    9 => avg2(t, tr),
                    10 => avg2(avg2(l, tl), avg2(t, tr)),
                    11 => select(t, l, tl),
                    12 => clamp_full(l, t, tl),
                    13 => clamp_half(avg2(l, t), tl),
                    _ => 0xFF00_0000,
                }
            };
            px[i] = add_pixels(px[i], pred);
        }
    }
}

fn delta(pred: u8, c: u8) -> i32 {
    ((pred as i8 as i32) * (c as i8 as i32)) >> 5
}

fn cross_color(px: &mut [u32], w: usize, h: usize, bits: u32, data: &[u32]) {
    let tw = sub_size(w, bits);
    for y in 0..h {
        for x in 0..w {
            let m = data[(y >> bits) * tw + (x >> bits)];
            let (g2r, g2b, r2b) = (m as u8, (m >> 8) as u8, (m >> 16) as u8);
            let p = &mut px[y * w + x];
            let g = (*p >> 8) as u8;
            let r = ((((*p >> 16) & 0xFF) as i32 + delta(g2r, g)) & 0xFF) as u32;
            let bl = ((((*p & 0xFF) as i32) + delta(g2b, g) + delta(r2b, r as u8)) & 0xFF) as u32;
            *p = (*p & 0xFF00_FF00) | r << 16 | bl;
        }
    }
}

fn unpack_index(px: &[u32], w: usize, h: usize, bits: u32, pal: &[u32]) -> Vec<u32> {
    let pw = sub_size(w, bits);
    let per = 1usize << bits;
    let bpp = 8 >> bits;
    let mask = (1u32 << bpp) - 1;
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let packed = (px[y * pw + x / per] >> 8) & 0xFF;
            let idx = (packed >> ((x % per) * bpp)) & mask;
            out.push(pal.get(idx as usize).copied().unwrap_or(0));
        }
    }
    out
}

/// A VP8L chunk's (width, height, has alpha) from its header.
pub fn header(d: &[u8]) -> Result<(usize, usize, bool), ImgError> {
    if d.len() < 5 {
        return Err(ImgError::Truncated);
    }
    if d[0] != 0x2F {
        return Err(corrupt());
    }
    let v = u32::from_le_bytes([d[1], d[2], d[3], d[4]]);
    let w = (v & 0x3FFF) as usize + 1;
    let h = ((v >> 14) & 0x3FFF) as usize + 1;
    let alpha = (v >> 28) & 1 == 1;
    if v >> 29 != 0 {
        return Err(ImgError::Unsupported(
            "This lossless WebP is a newer version.",
        ));
    }
    Ok((w, h, alpha))
}

/// Decode a VP8L chunk.
pub fn decode(d: &[u8]) -> Result<Image, ImgError> {
    let (w, h, _) = header(d)?;
    if w * h > MAX_PIXELS {
        return Err(ImgError::TooBig);
    }
    let mut b = Bits::new(&d[5..]);
    let px = image_stream(&mut b, w, h, true)?;
    Ok(Image { w, h, px })
}

/// The alpha plane of a lossy WebP, stored as a header-less VP8L stream (green = alpha).
pub fn decode_alpha(d: &[u8], w: usize, h: usize) -> Result<Vec<u8>, ImgError> {
    let mut b = Bits::new(d);
    let px = image_stream(&mut b, w, h, true)?;
    Ok(px.iter().map(|p| (p >> 8) as u8).collect())
}
