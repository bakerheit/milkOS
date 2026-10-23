//! JPEG: baseline and progressive Huffman-coded pictures, 8 bits a sample, grey (1 component),
//! YCbCr or RGB (3) and CMYK or YCCK (4, Adobe), any sampling factors (the 2x1, 1x2 and 2x2 cases
//! with libjpeg's "fancy" triangle upsampling, the rest by repeating samples), restart markers,
//! and the EXIF orientation (the picture comes out the way the camera meant it).
//!
//! Not decoded (an [`ImgError::Unsupported`]): arithmetic coding, lossless and hierarchical JPEG,
//! 12-bit samples. Colour profiles are ignored. A file whose entropy-coded data stops early still
//! gives a picture (the rest stays grey), as browsers show it; damaged headers, tables or Huffman
//! codes are an error. Work and memory are bounded by the frame size, which is checked against
//! [`MAX_PIXELS`] before anything is allocated.

use alloc::vec;
use alloc::vec::Vec;

use crate::{Image, ImgError, MAX_PIXELS};

/// Zigzag position to natural (row-major) index in the 8x8 block.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Progressive scans a file may have before we call it hostile (encoders write about ten).
const MAX_SCANS: usize = 256;

const FAST_BITS: u32 = 9;

/// A Huffman table: a 9-bit lookup for the short codes, canonical limits for the rest.
#[derive(Clone)]
struct Huff {
    /// `len << 8 | symbol` for codes of up to FAST_BITS bits (0: not a short code).
    fast: [u16; 1 << FAST_BITS],
    /// Largest code of each length (-1: none), and where its symbols start.
    maxcode: [i32; 17],
    first_code: [i32; 17],
    first_index: [i32; 17],
    vals: [u8; 256],
}

impl Huff {
    fn new(counts: &[u8; 16], syms: &[u8]) -> Result<Huff, ImgError> {
        let mut h = Huff {
            fast: [0; 1 << FAST_BITS],
            maxcode: [-1; 17],
            first_code: [0; 17],
            first_index: [0; 17],
            vals: [0; 256],
        };
        h.vals[..syms.len()].copy_from_slice(syms);
        let mut code: i32 = 0;
        let mut k: i32 = 0;
        for len in 1..=16usize {
            let n = counts[len - 1] as i32;
            h.first_code[len] = code;
            h.first_index[len] = k;
            if n > 0 {
                if code + n > 1 << len {
                    return Err(ImgError::Corrupt("A JPEG Huffman table is impossible."));
                }
                for i in 0..n {
                    let c = (code + i) as usize;
                    if len as u32 <= FAST_BITS {
                        let shift = FAST_BITS - len as u32;
                        let sym = h.vals[(k + i) as usize] as u16;
                        for j in 0..(1usize << shift) {
                            h.fast[(c << shift) | j] = (len as u16) << 8 | sym;
                        }
                    }
                }
                h.maxcode[len] = code + n - 1;
            }
            code = (code + n) << 1;
            k += n;
        }
        Ok(h)
    }
}

/// The entropy-coded bits of a scan: byte stuffing (FF 00) removed, stopping at a marker.
/// Past the end (or a marker) it reads zeros, so a short file still terminates.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    /// A marker (the byte after FF) stopped the reader; `pos` points at its FF.
    marker: Option<u8>,
    /// Zero bytes made up after the data ran out.
    padded: usize,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], pos: usize) -> Bits<'a> {
        Bits {
            d,
            pos,
            acc: 0,
            n: 0,
            marker: None,
            padded: 0,
        }
    }

    fn fill(&mut self) {
        while self.n <= 56 {
            let mut byte = 0u8;
            if self.marker.is_none() {
                match self.d.get(self.pos) {
                    None => self.padded += 1,
                    Some(&0xFF) => match self.d.get(self.pos + 1) {
                        Some(0) => {
                            byte = 0xFF;
                            self.pos += 2;
                        }
                        Some(0xFF) => {
                            // Fill bytes before a marker.
                            self.pos += 1;
                            continue;
                        }
                        Some(&m) => self.marker = Some(m),
                        None => {
                            self.pos += 1;
                            self.padded += 1;
                        }
                    },
                    Some(&b) => {
                        byte = b;
                        self.pos += 1;
                    }
                }
            } else {
                self.padded += 1;
            }
            self.acc |= (byte as u64) << (56 - self.n);
            self.n += 8;
        }
    }

    /// The data is used up: everything from here on would be made-up zeros.
    fn exhausted(&self) -> bool {
        // Eight bytes of padding cover what fill() reads ahead.
        self.padded > 8
    }

    fn bits(&mut self, k: u32) -> u32 {
        if k == 0 {
            return 0;
        }
        if self.n < k {
            self.fill();
        }
        let v = (self.acc >> (64 - k)) as u32;
        self.acc <<= k;
        self.n -= k;
        v
    }

    fn bit(&mut self) -> bool {
        self.bits(1) == 1
    }

    fn decode(&mut self, h: &Huff) -> Result<u8, ImgError> {
        if self.n < 16 {
            self.fill();
        }
        let look = (self.acc >> (64 - FAST_BITS)) as usize;
        let f = h.fast[look];
        if f != 0 {
            let len = (f >> 8) as u32;
            self.acc <<= len;
            self.n -= len;
            return Ok(f as u8);
        }
        for len in FAST_BITS as usize + 1..=16 {
            let code = (self.acc >> (64 - len)) as i32;
            if code <= h.maxcode[len] {
                self.acc <<= len;
                self.n -= len as u32;
                let i = h.first_index[len] + code - h.first_code[len];
                return Ok(h.vals[(i as usize) & 255]);
            }
        }
        Err(ImgError::Corrupt("The JPEG picture data is damaged."))
    }

    /// `s` bits as a signed coefficient (JPEG's EXTEND).
    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s) as i32;
        if v < 1 << (s - 1) {
            v - (1 << s) + 1
        } else {
            v
        }
    }

    /// Drop the rest of the current byte and step over a restart marker. False if the next
    /// marker isn't a restart (the scan is over).
    fn restart(&mut self) -> bool {
        self.acc = 0;
        self.n = 0;
        if self.marker.is_none() {
            // Skip to the next marker (normally right here).
            while self.pos + 1 < self.d.len() {
                if self.d[self.pos] == 0xFF
                    && self.d[self.pos + 1] != 0
                    && self.d[self.pos + 1] != 0xFF
                {
                    self.marker = Some(self.d[self.pos + 1]);
                    break;
                }
                self.pos += 1;
            }
        }
        match self.marker {
            Some(m) if (0xD0..=0xD7).contains(&m) => {
                self.marker = None;
                self.pos += 2;
                self.padded = 0;
                true
            }
            _ => false,
        }
    }
}

struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks across and down, padded to whole MCUs.
    bw: usize,
    bh: usize,
    /// Samples across and down that are part of the picture.
    cw: usize,
    ch: usize,
    /// The samples: bw * 8 by bh * 8.
    plane: Vec<u8>,
    /// Progressive only: every block's coefficients (zigzag-ordered positions, natural index).
    coefs: Vec<i16>,
    dc_pred: i32,
    dc_tbl: usize,
    ac_tbl: usize,
}

struct Frame {
    w: usize,
    h: usize,
    progressive: bool,
    comps: Vec<Comp>,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
}

#[derive(Default)]
struct Meta {
    /// The APP14 Adobe transform, when there is one.
    adobe: Option<u8>,
    jfif: bool,
    /// EXIF orientation, 1 to 8.
    orientation: u8,
}

fn be16(b: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize)
}

/// The EXIF orientation in an APP1 segment's data, if it has one.
fn exif_orientation(seg: &[u8]) -> Option<u8> {
    let t = seg.strip_prefix(b"Exif\0\0")?;
    let le = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |i: usize| -> Option<usize> {
        let b = t.get(i..i + 2)?;
        Some(if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        } as usize)
    };
    let u32_at = |i: usize| -> Option<usize> {
        let b = t.get(i..i + 4)?;
        let a = [b[0], b[1], b[2], b[3]];
        Some(if le {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        } as usize)
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)?;
    let n = u16_at(ifd)?;
    for i in 0..n.min(512) {
        let e = ifd.checked_add(2 + 12 * i)?;
        if u16_at(e)? == 0x0112 && u16_at(e + 2)? == 3 {
            let o = u16_at(e + 8)?;
            return (1..=8).contains(&o).then_some(o as u8);
        }
    }
    None
}

/// The marker at `pos` (after any fill bytes): (marker, position after it).
fn next_marker(d: &[u8], mut pos: usize) -> Option<(u8, usize)> {
    // Garbage between segments is skipped, the way libjpeg does.
    loop {
        while *d.get(pos)? != 0xFF {
            pos += 1;
        }
        while *d.get(pos)? == 0xFF {
            pos += 1;
        }
        let m = *d.get(pos)?;
        pos += 1;
        if m != 0 {
            return Some((m, pos));
        }
    }
}

/// Width and height as shown (after the EXIF orientation), without decoding.
pub fn probe(d: &[u8]) -> Result<(usize, usize), ImgError> {
    let mut d2 = Decoder::new(d)?;
    loop {
        let Some((m, pos)) = next_marker(d, d2.pos) else {
            return Err(ImgError::Truncated);
        };
        d2.pos = pos;
        match m {
            0xD9 | 0xDA => return Err(ImgError::Truncated),
            0xD0..=0xD7 | 0x01 => continue,
            _ => {}
        }
        let seg = d2.segment()?;
        match m {
            0xC0..=0xCF if m != 0xC4 && m != 0xC8 && m != 0xCC => {
                let h = be16(seg, 1).ok_or(ImgError::Truncated)?;
                let w = be16(seg, 3).ok_or(ImgError::Truncated)?;
                if w == 0 || h == 0 {
                    return Err(ImgError::Unsupported(
                        "JPEGs whose height comes after the picture can't be opened.",
                    ));
                }
                return Ok(if d2.meta.orientation >= 5 {
                    (h, w)
                } else {
                    (w, h)
                });
            }
            0xE1 => {
                if let Some(o) = exif_orientation(seg) {
                    d2.meta.orientation = o;
                }
            }
            _ => {}
        }
    }
}

pub fn decode(d: &[u8]) -> Result<Image, ImgError> {
    decode_oriented(d, true)
}

/// Decode; `orient` applies the EXIF orientation (false: the pixels as stored).
pub fn decode_oriented(d: &[u8], orient: bool) -> Result<Image, ImgError> {
    let mut dec = Decoder::new(d)?;
    dec.run()?;
    let img = dec.finish()?;
    Ok(if orient {
        apply_orientation(img, dec.meta.orientation)
    } else {
        img
    })
}

struct Decoder<'a> {
    d: &'a [u8],
    pos: usize,
    qt: [[u16; 64]; 4],
    qt_set: [bool; 4],
    dc: [Option<Huff>; 4],
    ac: [Option<Huff>; 4],
    restart: usize,
    frame: Option<Frame>,
    meta: Meta,
    scans: usize,
    eobrun: u32,
}

impl<'a> Decoder<'a> {
    fn new(d: &'a [u8]) -> Result<Decoder<'a>, ImgError> {
        if !d.starts_with(&[0xFF, 0xD8]) {
            return Err(ImgError::Unrecognised("This isn't a JPEG file."));
        }
        Ok(Decoder {
            d,
            pos: 2,
            qt: [[1; 64]; 4],
            qt_set: [false; 4],
            dc: [None, None, None, None],
            ac: [None, None, None, None],
            restart: 0,
            frame: None,
            meta: Meta {
                orientation: 1,
                ..Meta::default()
            },
            scans: 0,
            eobrun: 0,
        })
    }

    /// The segment at `pos` (after its marker): its data without the length; `pos` moves past.
    fn segment(&mut self) -> Result<&'a [u8], ImgError> {
        let len = be16(self.d, self.pos).ok_or(ImgError::Truncated)?;
        if len < 2 {
            return Err(ImgError::Corrupt(
                "A JPEG segment has an impossible length.",
            ));
        }
        let seg = self
            .d
            .get(self.pos + 2..self.pos + len)
            .ok_or(ImgError::Truncated)?;
        self.pos += len;
        Ok(seg)
    }

    fn run(&mut self) -> Result<(), ImgError> {
        loop {
            let Some((m, pos)) = next_marker(self.d, self.pos) else {
                // Out of data: fine once a scan was read (the picture is what there is).
                return if self.scans > 0 {
                    Ok(())
                } else {
                    Err(ImgError::Truncated)
                };
            };
            self.pos = pos;
            match m {
                0xD9 => {
                    return if self.scans > 0 {
                        Ok(())
                    } else {
                        Err(ImgError::Truncated)
                    };
                }
                0xD8 | 0xD0..=0xD7 | 0x01 => continue,
                _ => {}
            }
            let seg = self.segment()?;
            match m {
                0xC0..=0xC2 => {
                    if self.frame.is_some() {
                        return Err(ImgError::Corrupt("The JPEG has two frames."));
                    }
                    self.frame = Some(read_frame(seg, m == 0xC2)?);
                }
                0xC3 | 0xC5..=0xC7 => {
                    return Err(ImgError::Unsupported(
                        "Lossless and hierarchical JPEGs can't be opened.",
                    ));
                }
                0xC9..=0xCB | 0xCD..=0xCF => {
                    return Err(ImgError::Unsupported(
                        "JPEGs with arithmetic coding can't be opened.",
                    ));
                }
                0xC4 => self.read_dht(seg)?,
                0xDB => self.read_dqt(seg)?,
                0xDD => {
                    self.restart = be16(seg, 0).ok_or(ImgError::Truncated)?;
                }
                0xDA => {
                    self.scans += 1;
                    if self.scans > MAX_SCANS {
                        return Err(ImgError::Corrupt("The JPEG has too many scans."));
                    }
                    self.scan(seg)?;
                }
                0xE0 => self.meta.jfif |= seg.starts_with(b"JFIF\0"),
                0xE1 => {
                    if let Some(o) = exif_orientation(seg) {
                        self.meta.orientation = o;
                    }
                }
                0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => {
                    self.meta.adobe = Some(seg[11]);
                }
                _ => {} // COM, other APPn, DNL, ...
            }
        }
    }

    fn read_dqt(&mut self, mut s: &[u8]) -> Result<(), ImgError> {
        while !s.is_empty() {
            let pq = s[0] >> 4;
            let tq = (s[0] & 15) as usize;
            if pq > 1 || tq > 3 {
                return Err(ImgError::Corrupt("A JPEG quantisation table is damaged."));
            }
            let n = if pq == 0 { 64 } else { 128 };
            let t = s.get(1..1 + n).ok_or(ImgError::Truncated)?;
            for k in 0..64 {
                let v = if pq == 0 {
                    t[k] as u16
                } else {
                    u16::from_be_bytes([t[2 * k], t[2 * k + 1]])
                };
                self.qt[tq][ZIGZAG[k] as usize] = v;
            }
            self.qt_set[tq] = true;
            s = &s[1 + n..];
        }
        Ok(())
    }

    fn read_dht(&mut self, mut s: &[u8]) -> Result<(), ImgError> {
        while !s.is_empty() {
            let tc = s[0] >> 4;
            let th = (s[0] & 15) as usize;
            if tc > 1 || th > 3 {
                return Err(ImgError::Corrupt("A JPEG Huffman table is damaged."));
            }
            let counts: [u8; 16] = s
                .get(1..17)
                .ok_or(ImgError::Truncated)?
                .try_into()
                .map_err(|_| ImgError::Truncated)?;
            let total: usize = counts.iter().map(|&c| c as usize).sum();
            if total > 256 {
                return Err(ImgError::Corrupt("A JPEG Huffman table is damaged."));
            }
            let syms = s.get(17..17 + total).ok_or(ImgError::Truncated)?;
            let h = Huff::new(&counts, syms)?;
            if tc == 0 {
                self.dc[th] = Some(h);
            } else {
                self.ac[th] = Some(h);
            }
            s = &s[17 + total..];
        }
        Ok(())
    }

    fn scan(&mut self, seg: &[u8]) -> Result<(), ImgError> {
        let Some(frame) = self.frame.as_mut() else {
            return Err(ImgError::Corrupt(
                "The JPEG picture data comes before its header.",
            ));
        };
        let ns = *seg.first().ok_or(ImgError::Truncated)? as usize;
        if ns == 0 || ns > 4 || seg.len() < 1 + 2 * ns + 3 {
            return Err(ImgError::Corrupt("A JPEG scan header is damaged."));
        }
        let mut sel = Vec::with_capacity(ns);
        for i in 0..ns {
            let id = seg[1 + 2 * i];
            let t = seg[2 + 2 * i];
            let ci = frame
                .comps
                .iter()
                .position(|c| c.id == id)
                .ok_or(ImgError::Corrupt(
                    "A JPEG scan names a component that isn't there.",
                ))?;
            if sel.contains(&ci) {
                return Err(ImgError::Corrupt("A JPEG scan header is damaged."));
            }
            let (td, ta) = ((t >> 4) as usize, (t & 15) as usize);
            if td > 3 || ta > 3 {
                return Err(ImgError::Corrupt("A JPEG scan header is damaged."));
            }
            frame.comps[ci].dc_tbl = td;
            frame.comps[ci].ac_tbl = ta;
            sel.push(ci);
        }
        let p = 1 + 2 * ns;
        let (ss, se, ah, al) = (
            seg[p] as usize,
            seg[p + 1] as usize,
            (seg[p + 2] >> 4) as u32,
            (seg[p + 2] & 15) as u32,
        );
        let progressive = frame.progressive;
        if progressive {
            let bad = se > 63
                || ss > se
                || (ss == 0 && se != 0)
                || (ss > 0 && ns != 1)
                || ah > 13
                || al > 13;
            if bad {
                return Err(ImgError::Corrupt("A JPEG scan header is damaged."));
            }
        }
        // Check the tables this scan needs before decoding anything.
        for &ci in &sel {
            let c = &frame.comps[ci];
            let need_dc = !progressive || (ss == 0 && ah == 0);
            let need_ac = !progressive || ss > 0;
            if (need_dc && self.dc[c.dc_tbl].is_none()) || (need_ac && self.ac[c.ac_tbl].is_none())
            {
                return Err(ImgError::Corrupt("A JPEG Huffman table is missing."));
            }
            if !progressive && !self.qt_set[c.tq] {
                return Err(ImgError::Corrupt("A JPEG quantisation table is missing."));
            }
        }
        for c in frame.comps.iter_mut() {
            c.dc_pred = 0;
        }
        self.eobrun = 0;
        let mut bits = Bits::new(self.d, self.pos);
        let (mcux, mcuy) = if ns == 1 {
            let c = &frame.comps[sel[0]];
            (c.cw.div_ceil(8), c.ch.div_ceil(8))
        } else {
            (frame.mcux, frame.mcuy)
        };
        let ri = self.restart;
        let mut todo = ri;
        let mut blk = [0i16; 64];
        'mcus: for my in 0..mcuy {
            for mx in 0..mcux {
                if ri != 0 {
                    if todo == 0 {
                        if !bits.restart() {
                            break 'mcus;
                        }
                        for c in frame.comps.iter_mut() {
                            c.dc_pred = 0;
                        }
                        self.eobrun = 0;
                        todo = ri;
                    }
                    todo -= 1;
                }
                if bits.exhausted() {
                    break 'mcus;
                }
                for &ci in &sel {
                    let (bh, bv) = if ns == 1 {
                        (1, 1)
                    } else {
                        (frame.comps[ci].h, frame.comps[ci].v)
                    };
                    for by in 0..bv {
                        for bx in 0..bh {
                            let (x, y) = (mx * bh + bx, my * bv + by);
                            let c = &mut frame.comps[ci];
                            if progressive {
                                let i = (y * c.bw + x) * 64;
                                let Some(co) = c.coefs.get_mut(i..i + 64) else {
                                    continue;
                                };
                                let co: &mut [i16; 64] =
                                    co.try_into().map_err(|_| ImgError::TooBig)?;
                                if ss == 0 {
                                    if ah == 0 {
                                        let dc = self.dc[c.dc_tbl]
                                            .as_ref()
                                            .ok_or(ImgError::Truncated)?;
                                        let s = bits.decode(dc)? as u32;
                                        if s > 15 {
                                            return Err(ImgError::Corrupt(
                                                "The JPEG picture data is damaged.",
                                            ));
                                        }
                                        c.dc_pred = c.dc_pred.wrapping_add(bits.receive_extend(s));
                                        co[0] = (c.dc_pred.wrapping_shl(al)) as i16;
                                    } else if bits.bit() {
                                        co[0] |= (1 << al) as i16;
                                    }
                                } else {
                                    let ac =
                                        self.ac[c.ac_tbl].as_ref().ok_or(ImgError::Truncated)?;
                                    if ah == 0 {
                                        ac_first(&mut bits, ac, co, ss, se, al, &mut self.eobrun)?;
                                    } else {
                                        ac_refine(&mut bits, ac, co, ss, se, al, &mut self.eobrun)?;
                                    }
                                }
                            } else {
                                if x >= c.bw || y >= c.bh {
                                    continue;
                                }
                                blk.fill(0);
                                let dc = self.dc[c.dc_tbl].as_ref().ok_or(ImgError::Truncated)?;
                                let ac = self.ac[c.ac_tbl].as_ref().ok_or(ImgError::Truncated)?;
                                baseline_block(&mut bits, dc, ac, &mut blk, &mut c.dc_pred)?;
                                let stride = c.bw * 8;
                                idct_block(
                                    &blk,
                                    &self.qt[c.tq],
                                    &mut c.plane[(y * 8) * stride + x * 8..],
                                    stride,
                                );
                            }
                        }
                    }
                }
            }
        }
        // Carry on at the next marker after the scan's data.
        let mut p = bits.pos;
        loop {
            match (self.d.get(p), self.d.get(p + 1)) {
                (Some(0xFF), Some(&m)) if m != 0 && m != 0xFF && !(0xD0..=0xD7).contains(&m) => {
                    break;
                }
                (Some(_), Some(_)) => p += 1,
                _ => {
                    p = self.d.len();
                    break;
                }
            }
        }
        self.pos = p;
        Ok(())
    }

    /// The picture from what has been decoded.
    fn finish(&mut self) -> Result<Image, ImgError> {
        let Some(f) = self.frame.as_mut() else {
            return Err(ImgError::Corrupt("The JPEG has no picture header."));
        };
        if f.progressive {
            for c in f.comps.iter_mut() {
                let stride = c.bw * 8;
                let q = &self.qt[c.tq];
                let mut blk = [0i16; 64];
                for by in 0..c.bh {
                    for bx in 0..c.bw {
                        let i = (by * c.bw + bx) * 64;
                        blk.copy_from_slice(&c.coefs[i..i + 64]);
                        idct_block(&blk, q, &mut c.plane[(by * 8) * stride + bx * 8..], stride);
                    }
                }
                c.coefs = Vec::new();
            }
        }
        convert(f, &self.meta)
    }
}

fn read_frame(s: &[u8], progressive: bool) -> Result<Frame, ImgError> {
    if s.len() < 6 {
        return Err(ImgError::Truncated);
    }
    if s[0] != 8 {
        return Err(ImgError::Unsupported(
            "Only 8-bit JPEGs can be opened (this one has 12-bit samples).",
        ));
    }
    let h = be16(s, 1).ok_or(ImgError::Truncated)?;
    let w = be16(s, 3).ok_or(ImgError::Truncated)?;
    let n = s[5] as usize;
    if w == 0 || h == 0 {
        return Err(ImgError::Unsupported(
            "JPEGs whose height comes after the picture can't be opened.",
        ));
    }
    if w.checked_mul(h).is_none_or(|p| p > MAX_PIXELS) {
        return Err(ImgError::TooBig);
    }
    if !(n == 1 || n == 3 || n == 4) || s.len() < 6 + 3 * n {
        return Err(ImgError::Unsupported(
            "Only grey, colour and CMYK JPEGs can be opened.",
        ));
    }
    let mut comps = Vec::with_capacity(n);
    for i in 0..n {
        let c = &s[6 + 3 * i..9 + 3 * i];
        let (hs, vs) = ((c[1] >> 4) as usize, (c[1] & 15) as usize);
        if !(1..=4).contains(&hs) || !(1..=4).contains(&vs) || c[2] > 3 {
            return Err(ImgError::Corrupt("A JPEG component is damaged."));
        }
        comps.push(Comp {
            id: c[0],
            h: hs,
            v: vs,
            tq: c[2] as usize,
            bw: 0,
            bh: 0,
            cw: 0,
            ch: 0,
            plane: Vec::new(),
            coefs: Vec::new(),
            dc_pred: 0,
            dc_tbl: 0,
            ac_tbl: 0,
        });
    }
    let hmax = comps.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = comps.iter().map(|c| c.v).max().unwrap_or(1);
    if n == 1 {
        // A single component is never interleaved: its own size decides the blocks.
        comps[0].h = 1;
        comps[0].v = 1;
    }
    let (hmax, vmax) = if n == 1 { (1, 1) } else { (hmax, vmax) };
    let mcux = w.div_ceil(8 * hmax);
    let mcuy = h.div_ceil(8 * vmax);
    let mut coef_total = 0usize;
    for c in comps.iter_mut() {
        c.bw = mcux * c.h;
        c.bh = mcuy * c.v;
        c.cw = (w * c.h).div_ceil(hmax);
        c.ch = (h * c.v).div_ceil(vmax);
        coef_total += c.bw * c.bh * 64;
    }
    // Planes (and, progressive, coefficients at two bytes each) are at most about this much.
    if coef_total > 4 * MAX_PIXELS + (1 << 20) {
        return Err(ImgError::TooBig);
    }
    for c in comps.iter_mut() {
        c.plane = vec![128u8; c.bw * c.bh * 64];
        if progressive {
            c.coefs = vec![0i16; c.bw * c.bh * 64];
        }
    }
    Ok(Frame {
        w,
        h,
        progressive,
        comps,
        hmax,
        vmax,
        mcux,
        mcuy,
    })
}

fn baseline_block(
    bits: &mut Bits,
    dc: &Huff,
    ac: &Huff,
    blk: &mut [i16; 64],
    pred: &mut i32,
) -> Result<(), ImgError> {
    let s = bits.decode(dc)? as u32;
    if s > 15 {
        return Err(ImgError::Corrupt("The JPEG picture data is damaged."));
    }
    *pred = pred.wrapping_add(bits.receive_extend(s));
    blk[0] = *pred as i16;
    let mut k = 1;
    while k < 64 {
        let rs = bits.decode(ac)?;
        let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
        if s == 0 {
            if r != 15 {
                break; // end of block
            }
            k += 16;
            continue;
        }
        k += r;
        if k > 63 {
            return Err(ImgError::Corrupt("The JPEG picture data is damaged."));
        }
        blk[ZIGZAG[k] as usize] = bits.receive_extend(s) as i16;
        k += 1;
    }
    Ok(())
}

fn ac_first(
    bits: &mut Bits,
    ac: &Huff,
    co: &mut [i16; 64],
    ss: usize,
    se: usize,
    al: u32,
    eobrun: &mut u32,
) -> Result<(), ImgError> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = ss;
    while k <= se {
        let rs = bits.decode(ac)?;
        let (r, s) = ((rs >> 4) as u32, (rs & 15) as u32);
        if s == 0 {
            if r < 15 {
                *eobrun = (1 << r) - 1 + bits.bits(r);
                break;
            }
            k += 16;
            continue;
        }
        k += r as usize;
        if k > 63 {
            return Err(ImgError::Corrupt("The JPEG picture data is damaged."));
        }
        co[ZIGZAG[k] as usize] = bits.receive_extend(s).wrapping_shl(al) as i16;
        k += 1;
    }
    Ok(())
}

fn ac_refine(
    bits: &mut Bits,
    ac: &Huff,
    co: &mut [i16; 64],
    ss: usize,
    se: usize,
    al: u32,
    eobrun: &mut u32,
) -> Result<(), ImgError> {
    let bit = (1i32 << al) as i16;
    let refine = |bits: &mut Bits, p: &mut i16| {
        if bits.bit() && *p & bit == 0 {
            *p = if *p >= 0 {
                p.wrapping_add(bit)
            } else {
                p.wrapping_sub(bit)
            };
        }
    };
    if *eobrun > 0 {
        *eobrun -= 1;
        for k in ss..=se {
            let p = &mut co[ZIGZAG[k] as usize];
            if *p != 0 {
                refine(bits, p);
            }
        }
        return Ok(());
    }
    let mut k = ss;
    while k <= se {
        let rs = bits.decode(ac)?;
        let (mut r, s) = ((rs >> 4) as u32, (rs & 15) as u32);
        let mut val: i16 = 0;
        if s == 0 {
            if r < 15 {
                *eobrun = (1 << r) - 1 + bits.bits(r);
                r = 64; // refine the rest of the band, then stop
            }
        } else {
            if s != 1 {
                return Err(ImgError::Corrupt("The JPEG picture data is damaged."));
            }
            val = if bits.bit() { bit } else { bit.wrapping_neg() };
        }
        while k <= se {
            let p = &mut co[ZIGZAG[k] as usize];
            k += 1;
            if *p != 0 {
                refine(bits, p);
            } else {
                if r == 0 {
                    *p = val;
                    break;
                }
                r -= 1;
            }
        }
    }
    Ok(())
}

/// Dequantise and inverse-DCT one block into `out` (8 rows of `stride`). The integer
/// separable IDCT with 12-bit constants (the one libjpeg calls "islow", as stb_image has it).
fn idct_block(blk: &[i16; 64], q: &[u16; 64], out: &mut [u8], stride: usize) {
    let mut v = [0i64; 64];
    // The constants times 4096, rounded the way C's (int)(x * 4096 + 0.5) does.
    const K0541: i64 = 2217; // 0.5411961
    const K1847: i64 = -7567; // -1.847759065
    const K0765: i64 = 3135; // 0.765366865
    const K1175: i64 = 4816; // 1.175875602
    const K0298: i64 = 1223; // 0.298631336
    const K2053: i64 = 8410; // 2.053119869
    const K3072: i64 = 12586; // 3.072711026
    const K1501: i64 = 6149; // 1.501321110
    const K0899: i64 = -3685; // -0.899976223
    const K2562: i64 = -10497; // -2.562915447
    const K1961: i64 = -8034; // -1.961570560
    const K0390: i64 = -1597; // -0.390180644
    #[inline(always)]
    fn idct1d(s: [i64; 8]) -> ([i64; 4], [i64; 4]) {
        let p2 = s[2];
        let p3 = s[6];
        let p1 = (p2 + p3) * K0541;
        let t2 = p1 + p3 * K1847;
        let t3 = p1 + p2 * K0765;
        let p2 = s[0];
        let p3 = s[4];
        let t0 = (p2 + p3) * 4096;
        let t1 = (p2 - p3) * 4096;
        let x0 = t0 + t3;
        let x3 = t0 - t3;
        let x1 = t1 + t2;
        let x2 = t1 - t2;
        let (t0, t1, t2, t3) = (s[7], s[5], s[3], s[1]);
        let p3 = t0 + t2;
        let p4 = t1 + t3;
        let p1 = t0 + t3;
        let p2 = t1 + t2;
        let p5 = (p3 + p4) * K1175;
        let t0 = t0 * K0298;
        let t1 = t1 * K2053;
        let t2 = t2 * K3072;
        let t3 = t3 * K1501;
        let p1 = p5 + p1 * K0899;
        let p2 = p5 + p2 * K2562;
        let p3 = p3 * K1961;
        let p4 = p4 * K0390;
        let t3 = t3 + p1 + p4;
        let t2 = t2 + p2 + p3;
        let t1 = t1 + p2 + p4;
        let t0 = t0 + p1 + p3;
        ([x0, x1, x2, x3], [t0, t1, t2, t3])
    }
    for i in 0..8 {
        let d = |r: usize| blk[r * 8 + i] as i64 * q[r * 8 + i] as i64;
        if (1..8).all(|r| blk[r * 8 + i] == 0) {
            let dc = d(0) * 4;
            for r in 0..8 {
                v[r * 8 + i] = dc;
            }
            continue;
        }
        let ([x0, x1, x2, x3], [t0, t1, t2, t3]) =
            idct1d([d(0), d(1), d(2), d(3), d(4), d(5), d(6), d(7)]);
        let (x0, x1, x2, x3) = (x0 + 512, x1 + 512, x2 + 512, x3 + 512);
        v[i] = (x0 + t3) >> 10;
        v[56 + i] = (x0 - t3) >> 10;
        v[8 + i] = (x1 + t2) >> 10;
        v[48 + i] = (x1 - t2) >> 10;
        v[16 + i] = (x2 + t1) >> 10;
        v[40 + i] = (x2 - t1) >> 10;
        v[24 + i] = (x3 + t0) >> 10;
        v[32 + i] = (x3 - t0) >> 10;
    }
    for r in 0..8 {
        let row: [i64; 8] = v[r * 8..r * 8 + 8].try_into().unwrap_or([0; 8]);
        let ([x0, x1, x2, x3], [t0, t1, t2, t3]) = idct1d(row);
        let b = 65536 + (128 << 17);
        let (x0, x1, x2, x3) = (x0 + b, x1 + b, x2 + b, x3 + b);
        let clamp = |x: i64| (x >> 17).clamp(0, 255) as u8;
        let Some(o) = out.get_mut(r * stride..r * stride + 8) else {
            return;
        };
        o[0] = clamp(x0 + t3);
        o[7] = clamp(x0 - t3);
        o[1] = clamp(x1 + t2);
        o[6] = clamp(x1 - t2);
        o[2] = clamp(x2 + t1);
        o[5] = clamp(x2 - t1);
        o[3] = clamp(x3 + t0);
        o[4] = clamp(x3 - t0);
    }
}

/// One output row (`y` of the picture) of component `c`, upsampled to `w` samples.
fn upsample_row(
    c: &Comp,
    hmax: usize,
    vmax: usize,
    y: usize,
    w: usize,
    out: &mut [u8],
    colsum: &mut [i32],
) {
    let stride = c.bw * 8;
    let (fx, fy) = (hmax / c.h, vmax / c.v);
    // libjpeg only uses its triangle filters when the component is more than 2 samples wide.
    let exact =
        hmax.is_multiple_of(c.h) && vmax.is_multiple_of(c.v) && (c.cw > 2 || (fx == 1 && fy == 1));
    let row = |r: usize| &c.plane[r.min(c.ch - 1) * stride..][..c.cw];
    if exact && fx == 1 && fy == 1 {
        out[..w].copy_from_slice(&row(y)[..w]);
    } else if exact && fx == 2 && fy == 1 {
        let s = row(y);
        let last = c.cw - 1;
        for (x, o) in out[..w].iter_mut().enumerate() {
            let i = x / 2;
            let v = s[i] as i32;
            *o = if x % 2 == 0 {
                if i == 0 {
                    v
                } else {
                    (v * 3 + s[i - 1] as i32 + 1) >> 2
                }
            } else if i == last {
                v
            } else {
                (v * 3 + s[i + 1] as i32 + 2) >> 2
            } as u8;
        }
    } else if exact && fy == 2 && (fx == 1 || fx == 2) {
        let r = y / 2;
        let near = row(r);
        let (far, bias) = if y % 2 == 0 {
            (row(r.saturating_sub(1)), 1)
        } else {
            (row(r + 1), 2)
        };
        if fx == 1 {
            for (x, o) in out[..w].iter_mut().enumerate() {
                *o = ((near[x] as i32 * 3 + far[x] as i32 + bias) >> 2) as u8;
            }
        } else {
            for i in 0..c.cw {
                colsum[i] = near[i] as i32 * 3 + far[i] as i32;
            }
            let last = c.cw - 1;
            for (x, o) in out[..w].iter_mut().enumerate() {
                let i = x / 2;
                let t = colsum[i];
                *o = if x % 2 == 0 {
                    if i == 0 {
                        (t * 4 + 8) >> 4
                    } else {
                        (t * 3 + colsum[i - 1] + 8) >> 4
                    }
                } else if i == last {
                    (t * 4 + 7) >> 4
                } else {
                    (t * 3 + colsum[i + 1] + 7) >> 4
                } as u8;
            }
        }
    } else {
        let s = row(y * c.v / vmax);
        for (x, o) in out[..w].iter_mut().enumerate() {
            *o = s[(x * c.h / hmax).min(c.cw - 1)];
        }
    }
}

/// Pillow's and libjpeg's a * b / 255, rounded.
fn mul255(a: i32, b: i32) -> i32 {
    let t = a * b + 128;
    ((t >> 8) + t) >> 8
}

fn convert(f: &Frame, meta: &Meta) -> Result<Image, ImgError> {
    let (w, h) = (f.w, f.h);
    let mut img = Image::new(w, h)?;
    let n = f.comps.len();
    // libjpeg's choice of colour space.
    let ycc = match (n, meta.adobe) {
        (3, Some(t)) => t != 0,
        (3, None) => {
            let ids: Vec<u8> = f.comps.iter().map(|c| c.id).collect();
            meta.jfif || ids != b"RGB"
        }
        (4, Some(t)) => t == 2,
        _ => false,
    };
    let mut rows = vec![0u8; n * w];
    let mut colsum = vec![0i32; f.comps.iter().map(|c| c.cw).max().unwrap_or(1)];
    // Fixed-point YCbCr to RGB, libjpeg's tables (16 fraction bits).
    const ONE_HALF: i32 = 1 << 15;
    // FIX(1.40200), FIX(1.77200), FIX(0.71414), FIX(0.34414).
    let (cr_r, cb_b, cr_g, cb_g) = (91881, 116130, 46802, 22554);
    let ycc_rgb = |y: i32, cb: i32, cr: i32| -> (i32, i32, i32) {
        let (cb, cr) = (cb - 128, cr - 128);
        let r = y + ((cr_r * cr + ONE_HALF) >> 16);
        let g = y + ((-cb_g * cb - cr_g * cr + ONE_HALF) >> 16);
        let b = y + ((cb_b * cb + ONE_HALF) >> 16);
        (r.clamp(0, 255), g.clamp(0, 255), b.clamp(0, 255))
    };
    for y in 0..h {
        for (i, c) in f.comps.iter().enumerate() {
            upsample_row(
                c,
                f.hmax,
                f.vmax,
                y,
                w,
                &mut rows[i * w..(i + 1) * w],
                &mut colsum,
            );
        }
        let out = &mut img.px[y * w..(y + 1) * w];
        for (x, o) in out.iter_mut().enumerate() {
            let s = |i: usize| rows[i * w + x] as i32;
            let (r, g, b) = match n {
                1 => (s(0), s(0), s(0)),
                3 if ycc => ycc_rgb(s(0), s(1), s(2)),
                3 => (s(0), s(1), s(2)),
                _ => {
                    let (c, m, yy) = if ycc {
                        let (r, g, b) = ycc_rgb(s(0), s(1), s(2));
                        (255 - r, 255 - g, 255 - b)
                    } else {
                        (s(0), s(1), s(2))
                    };
                    let k = s(3);
                    if meta.adobe.is_some() {
                        // Adobe CMYK is stored inverted (255 = no ink).
                        (mul255(c, k), mul255(m, k), mul255(yy, k))
                    } else {
                        (
                            mul255(255 - c, 255 - k),
                            mul255(255 - m, 255 - k),
                            mul255(255 - yy, 255 - k),
                        )
                    }
                }
            };
            *o = 0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32;
        }
    }
    Ok(img)
}

/// The picture turned the way EXIF orientation `o` (1 to 8) says it should be shown.
pub fn apply_orientation(img: Image, o: u8) -> Image {
    let (w, h) = (img.w, img.h);
    let map: fn(usize, usize, usize, usize) -> (usize, usize) = match o {
        2 => |x, y, w, _| (w - 1 - x, y),
        3 => |x, y, w, h| (w - 1 - x, h - 1 - y),
        4 => |x, y, _, h| (x, h - 1 - y),
        5 => |x, y, _, _| (y, x),
        6 => |x, y, _, h| (h - 1 - y, x),
        7 => |x, y, w, h| (h - 1 - y, w - 1 - x),
        8 => |x, y, w, _| (y, w - 1 - x),
        _ => return img,
    };
    let (nw, nh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = map(x, y, w, h);
            px[ny * nw + nx] = img.px[y * w + x];
        }
    }
    Image { w: nw, h: nh, px }
}
