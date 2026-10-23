//! VP8 key frames, lossy WebP (RFC 6386): the boolean entropy decoder, segments, the token
//! partitions, intra prediction (16x16, 4x4 and chroma modes), the inverse DCT and WHT, the
//! normal and simple loop filters, and YUV 4:2:0 to RGB with libwebp's "fancy" upsampling.
//! Inter frames never occur in WebP and are refused.

use alloc::vec;
use alloc::vec::Vec;

use super::vp8_tables::{AC_Q, COEF_PROBS, COEF_UPDATE_PROBS, DC_Q, KF_BMODE_PROBS};
use crate::{ImgError, MAX_PIXELS};

fn corrupt() -> ImgError {
    ImgError::Corrupt("The WebP picture data is damaged.")
}

/// The boolean decoder (RFC 6386 section 7). Past the end it reads zeros.
struct Bool<'a> {
    d: &'a [u8],
    pos: usize,
    value: u32,
    range: u32,
    bits: i32,
    over: usize,
}

impl<'a> Bool<'a> {
    fn new(d: &'a [u8]) -> Bool<'a> {
        let mut b = Bool {
            d,
            pos: 0,
            value: 0,
            range: 255,
            bits: -8,
            over: 0,
        };
        b.load();
        b.load();
        b.bits = 0;
        b
    }

    fn load(&mut self) {
        let byte = match self.d.get(self.pos) {
            Some(&v) => v,
            None => {
                self.over += 1;
                0
            }
        };
        self.pos += 1;
        self.value = (self.value << 8) | byte as u32;
    }

    fn get(&mut self, prob: u8) -> bool {
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        let big = split << 8;
        let bit = if self.value >= big {
            self.range -= split;
            self.value -= big;
            true
        } else {
            self.range = split;
            false
        };
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.bits += 1;
            if self.bits == 8 {
                self.bits = 0;
                self.value &= 0xFFFF;
                self.load_low();
            }
        }
        bit
    }

    /// The next byte goes into the low eight bits of the 16-bit window.
    fn load_low(&mut self) {
        let byte = match self.d.get(self.pos) {
            Some(&v) => v,
            None => {
                self.over += 1;
                0
            }
        };
        self.pos += 1;
        self.value |= byte as u32;
    }

    fn lit(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.get(128) as u32;
        }
        v
    }

    fn signed(&mut self, n: u32) -> i32 {
        let v = self.lit(n) as i32;
        if self.get(128) { -v } else { v }
    }

    fn flag_signed(&mut self, n: u32) -> i32 {
        if self.get(128) { self.signed(n) } else { 0 }
    }

    fn eos(&self) -> bool {
        self.over > 4
    }
}

// Sub-block modes, in the RFC's order.
const B_DC: u8 = 0;
const B_TM: u8 = 1;
const B_VE: u8 = 2;
const B_HE: u8 = 3;
const B_LD: u8 = 4;
const B_RD: u8 = 5;
const B_VR: u8 = 6;
const B_VL: u8 = 7;
const B_HD: u8 = 8;
const B_HU: u8 = 9;

/// RFC 6386 bmode_tree: positive entries index the tree, the rest are -mode.
const BMODE_TREE: [i8; 18] = [
    -(B_DC as i8),
    2,
    -(B_TM as i8),
    4,
    -(B_VE as i8),
    6,
    8,
    12,
    -(B_HE as i8),
    10,
    -(B_RD as i8),
    -(B_VR as i8),
    -(B_LD as i8),
    14,
    -(B_VL as i8),
    16,
    -(B_HD as i8),
    -(B_HU as i8),
];

const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const BANDS: [usize; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];
const CAT3: [u8; 3] = [173, 148, 140];
const CAT4: [u8; 4] = [176, 155, 140, 135];
const CAT5: [u8; 5] = [180, 157, 141, 134, 130];
const CAT6: [u8; 11] = [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];

type Probs = [[[[u8; 11]; 3]; 8]; 4];

#[derive(Clone, Copy, Default)]
struct Quant {
    y1: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

#[derive(Clone, Copy, Default)]
struct FilterInfo {
    limit: i32,
    ilevel: i32,
    hev: i32,
    inner: bool,
}

struct Header {
    w: usize,
    h: usize,
    first_part: usize,
}

/// (width, height) of a VP8 key frame, from its first ten bytes.
pub fn header(d: &[u8]) -> Result<(usize, usize), ImgError> {
    let h = frame_header(d)?;
    Ok((h.w, h.h))
}

fn frame_header(d: &[u8]) -> Result<Header, ImgError> {
    if d.len() < 10 {
        return Err(ImgError::Truncated);
    }
    let bits = d[0] as u32 | (d[1] as u32) << 8 | (d[2] as u32) << 16;
    if bits & 1 != 0 {
        return Err(ImgError::Corrupt("The WebP frame isn't a key frame."));
    }
    if (bits >> 1) & 7 > 3 {
        return Err(ImgError::Unsupported(
            "This lossy WebP uses an unknown VP8 profile.",
        ));
    }
    if d[3..6] != [0x9D, 0x01, 0x2A] {
        return Err(corrupt());
    }
    let w = (u16::from_le_bytes([d[6], d[7]]) & 0x3FFF) as usize;
    let h = (u16::from_le_bytes([d[8], d[9]]) & 0x3FFF) as usize;
    if w == 0 || h == 0 {
        return Err(ImgError::Corrupt("The image has no size."));
    }
    Ok(Header {
        w,
        h,
        first_part: (bits >> 5) as usize,
    })
}

/// A decoded key frame: Y at full size, U and V at half, padded to whole macroblocks.
pub struct Yuv {
    pub w: usize,
    pub h: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub ystride: usize,
    pub uvstride: usize,
}

pub fn decode(d: &[u8]) -> Result<Yuv, ImgError> {
    let hd = frame_header(d)?;
    let (w, h) = (hd.w, hd.h);
    if w * h > MAX_PIXELS {
        return Err(ImgError::TooBig);
    }
    let body = &d[10..];
    let first = body.get(..hd.first_part).ok_or(ImgError::Truncated)?;
    let mut br = Bool::new(first);
    let _colour_space = br.get(128);
    let _clamp = br.get(128);
    // Segments.
    let use_segment = br.get(128);
    let mut update_map = false;
    let mut absolute = false;
    let mut seg_q = [0i32; 4];
    let mut seg_lf = [0i32; 4];
    let mut seg_probs = [255u8; 3];
    if use_segment {
        update_map = br.get(128);
        if br.get(128) {
            absolute = br.get(128);
            for q in seg_q.iter_mut() {
                *q = br.flag_signed(7);
            }
            for l in seg_lf.iter_mut() {
                *l = br.flag_signed(6);
            }
        }
        if update_map {
            for p in seg_probs.iter_mut() {
                *p = if br.get(128) { br.lit(8) as u8 } else { 255 };
            }
        }
    }
    // The loop filter.
    let simple = br.get(128);
    let level = br.lit(6) as i32;
    let sharpness = br.lit(3) as i32;
    let mut ref_delta = [0i32; 4];
    let mut mode_delta = [0i32; 4];
    let use_lf_delta = br.get(128);
    if use_lf_delta && br.get(128) {
        for r in ref_delta.iter_mut() {
            if br.get(128) {
                *r = br.signed(6);
            }
        }
        for m in mode_delta.iter_mut() {
            if br.get(128) {
                *m = br.signed(6);
            }
        }
    }
    // Token partitions.
    let nparts = 1usize << br.lit(2);
    let rest = &body[hd.first_part..];
    let sizes_len = 3 * (nparts - 1);
    if rest.len() < sizes_len {
        return Err(ImgError::Truncated);
    }
    let mut parts: Vec<Bool> = Vec::with_capacity(nparts);
    let mut start = sizes_len;
    for p in 0..nparts {
        let size = if p + 1 < nparts {
            let s = &rest[3 * p..3 * p + 3];
            (s[0] as usize | (s[1] as usize) << 8 | (s[2] as usize) << 16).min(rest.len() - start)
        } else {
            rest.len() - start
        };
        parts.push(Bool::new(&rest[start..start + size]));
        start += size;
    }
    // Quantisers.
    let base_q = br.lit(7) as i32;
    let dy1_dc = br.flag_signed(4);
    let dy2_dc = br.flag_signed(4);
    let dy2_ac = br.flag_signed(4);
    let duv_dc = br.flag_signed(4);
    let duv_ac = br.flag_signed(4);
    let mut quant = [Quant::default(); 4];
    for (s, q) in quant.iter_mut().enumerate() {
        let base = if use_segment {
            seg_q[s] + if absolute { 0 } else { base_q }
        } else {
            base_q
        };
        let at = |t: &[u16; 128], v: i32, max: i32| t[v.clamp(0, max) as usize] as i32;
        q.y1 = [at(&DC_Q, base + dy1_dc, 127), at(&AC_Q, base, 127)];
        q.y2 = [
            at(&DC_Q, base + dy2_dc, 127) * 2,
            ((at(&AC_Q, base + dy2_ac, 127) * 101581) >> 16).max(8),
        ];
        q.uv = [at(&DC_Q, base + duv_dc, 117), at(&AC_Q, base + duv_ac, 127)];
    }
    let _refresh = br.get(128);
    let mut probs: Probs = COEF_PROBS;
    for t in 0..4 {
        for b in 0..8 {
            for c in 0..3 {
                for p in 0..11 {
                    if br.get(COEF_UPDATE_PROBS[t][b][c][p]) {
                        probs[t][b][c][p] = br.lit(8) as u8;
                    }
                }
            }
        }
    }
    let skip_prob = if br.get(128) {
        Some(br.lit(8) as u8)
    } else {
        None
    };

    // Filter strengths per segment and per "has 4x4 modes".
    let mut fstr = [[FilterInfo::default(); 2]; 4];
    for (s, fs) in fstr.iter_mut().enumerate() {
        let mut base = level;
        if use_segment {
            base = seg_lf[s] + if absolute { 0 } else { level };
        }
        for (i4, f) in fs.iter_mut().enumerate() {
            let mut lv = base;
            if use_lf_delta {
                lv += ref_delta[0];
                if i4 == 1 {
                    lv += mode_delta[0];
                }
            }
            let lv = lv.clamp(0, 63);
            if lv > 0 {
                let mut il = lv;
                if sharpness > 0 {
                    il >>= if sharpness > 4 { 2 } else { 1 };
                    il = il.min(9 - sharpness);
                }
                let il = il.max(1);
                *f = FilterInfo {
                    limit: 2 * lv + il,
                    ilevel: il,
                    hev: if lv >= 40 {
                        2
                    } else if lv >= 15 {
                        1
                    } else {
                        0
                    },
                    inner: i4 == 1,
                };
            }
        }
    }
    let filter_type = if level == 0 {
        0
    } else if simple {
        1
    } else {
        2
    };

    let (mbw, mbh) = (w.div_ceil(16), h.div_ceil(16));
    let ystride = mbw * 16;
    let uvstride = mbw * 8;
    let mut yp = vec![0u8; ystride * mbh * 16];
    let mut up = vec![0u8; uvstride * mbh * 8];
    let mut vp = vec![0u8; uvstride * mbh * 8];
    let mut finfo = vec![FilterInfo::default(); mbw * mbh];

    // Contexts carried between macroblocks.
    let mut intra_top = vec![B_DC; 4 * mbw];
    let mut nz_top = vec![0u8; mbw]; // bits 0-3 luma, 4-5 u, 6-7 v (per column of blocks)
    let mut nz_dc_top = vec![false; mbw];

    let mut coeffs = [0i16; 384];
    for mby in 0..mbh {
        let mut intra_left = [B_DC; 4];
        let mut nz_left = 0u8;
        let mut nz_dc_left = false;
        let tp = &mut parts[mby & (nparts - 1)];
        for mbx in 0..mbw {
            if br.eos() {
                return Err(ImgError::Truncated);
            }
            // Modes (first partition).
            let segment = if update_map {
                if !br.get(seg_probs[0]) {
                    br.get(seg_probs[1]) as usize
                } else {
                    br.get(seg_probs[2]) as usize + 2
                }
            } else {
                0
            };
            let skip = skip_prob.is_some_and(|p| br.get(p));
            let i4x4 = !br.get(145);
            let mut ymode = 0u8;
            let mut bmodes = [0u8; 16];
            if !i4x4 {
                ymode = if br.get(156) {
                    if br.get(128) { B_TM } else { B_HE }
                } else if br.get(163) {
                    B_VE
                } else {
                    B_DC
                };
                intra_top[4 * mbx..4 * mbx + 4].fill(ymode);
                intra_left = [ymode; 4];
            } else {
                for y in 0..4 {
                    let mut left = intra_left[y];
                    for x in 0..4 {
                        let top = intra_top[4 * mbx + x];
                        let p = &KF_BMODE_PROBS[top as usize][left as usize];
                        let mut i = 0usize;
                        let m = loop {
                            let n = BMODE_TREE[i + br.get(p[i >> 1]) as usize];
                            if n <= 0 {
                                break (-n) as u8;
                            }
                            i = n as usize;
                        };
                        bmodes[4 * y + x] = m;
                        intra_top[4 * mbx + x] = m;
                        left = m;
                    }
                    intra_left[y] = left;
                }
            }
            let uvmode = if !br.get(142) {
                B_DC
            } else if !br.get(114) {
                B_VE
            } else if br.get(183) {
                B_TM
            } else {
                B_HE
            };

            // Residuals (token partition).
            coeffs.fill(0);
            let q = &quant[segment];
            let mut has_coeffs = false;
            if !skip {
                let mut first = 0;
                let mut ac_type = 3;
                if !i4x4 {
                    let mut dc = [0i16; 16];
                    let ctx = nz_dc_top[mbx] as usize + nz_dc_left as usize;
                    let nz = coeff_block(tp, &probs[1], ctx, q.y2, 0, &mut dc);
                    nz_dc_top[mbx] = nz > 0;
                    nz_dc_left = nz > 0;
                    wht(&dc, &mut coeffs);
                    first = 1;
                    ac_type = 0;
                }
                let mut tnz = nz_top[mbx] & 0x0F;
                let lnz = nz_left & 0x0F;
                let mut new_t = 0u8;
                let mut new_l = 0u8;
                for y in 0..4 {
                    let mut l = (lnz >> y) & 1;
                    for x in 0..4 {
                        let t = (tnz >> x) & 1;
                        let ctx = (l + t) as usize;
                        let blk: &mut [i16; 16] = (&mut coeffs
                            [(4 * y + x) * 16..(4 * y + x) * 16 + 16])
                            .try_into()
                            .map_err(|_| corrupt())?;
                        let nz = coeff_block(tp, &probs[ac_type], ctx, q.y1, first, blk);
                        let nzb = (nz > first) as u8;
                        // libwebp's test for "this macroblock has coefficients".
                        has_coeffs |= nz > 1 || blk[0] != 0;
                        l = nzb;
                        tnz = (tnz & !(1 << x)) | nzb << x;
                    }
                    new_l |= l << y;
                }
                new_t |= tnz & 0x0F;
                // Chroma: U then V, 2x2 blocks each.
                for ch in 0..2 {
                    let shift = 4 + 2 * ch;
                    let mut tnz = (nz_top[mbx] >> shift) & 3;
                    let lnz = (nz_left >> shift) & 3;
                    let mut nl = 0u8;
                    for y in 0..2 {
                        let mut l = (lnz >> y) & 1;
                        for x in 0..2 {
                            let t = (tnz >> x) & 1;
                            let ctx = (l + t) as usize;
                            let base = 256 + ch * 64 + (2 * y + x) * 16;
                            let blk: &mut [i16; 16] = (&mut coeffs[base..base + 16])
                                .try_into()
                                .map_err(|_| corrupt())?;
                            let nz = coeff_block(tp, &probs[2], ctx, q.uv, 0, blk);
                            let nzb = (nz > 0) as u8;
                            has_coeffs |= nz > 1 || blk[0] != 0;
                            l = nzb;
                            tnz = (tnz & !(1 << x)) | nzb << x;
                        }
                        nl |= l << y;
                    }
                    new_t |= tnz << shift;
                    new_l |= nl << shift;
                }
                nz_top[mbx] = new_t;
                nz_left = new_l;
            } else {
                nz_top[mbx] = 0;
                nz_left = 0;
                if !i4x4 {
                    nz_dc_top[mbx] = false;
                    nz_dc_left = false;
                }
                // A skipped 16x16 macroblock still needs its (zero) DC spread: nothing to do.
            }
            if filter_type > 0 {
                let mut f = fstr[segment][i4x4 as usize];
                f.inner |= has_coeffs;
                finfo[mby * mbw + mbx] = f;
            }

            // Prediction and reconstruction, in a bordered work area like libwebp's.
            reconstruct(
                &mut yp, &mut up, &mut vp, ystride, uvstride, mbx, mby, mbw, i4x4, ymode, &bmodes,
                uvmode, &coeffs,
            );
        }
    }
    if filter_type > 0 {
        for mby in 0..mbh {
            for mbx in 0..mbw {
                let f = finfo[mby * mbw + mbx];
                if f.limit == 0 {
                    continue;
                }
                filter_mb(
                    &mut yp,
                    &mut up,
                    &mut vp,
                    ystride,
                    uvstride,
                    mbx,
                    mby,
                    f,
                    filter_type == 1,
                );
            }
        }
    }
    Ok(Yuv {
        w,
        h,
        y: yp,
        u: up,
        v: vp,
        ystride,
        uvstride,
    })
}

/// One block's tokens (RFC 6386 section 13): returns one past the last position read (0 =
/// none), the way libwebp's GetCoeffs does.
fn coeff_block(
    br: &mut Bool,
    probs: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    dq: [i32; 2],
    first: usize,
    out: &mut [i16; 16],
) -> usize {
    let mut n = first;
    let mut p = &probs[BANDS[n]][ctx];
    while n < 16 {
        if !br.get(p[0]) {
            return n; // end of block
        }
        while !br.get(p[1]) {
            n += 1;
            if n == 16 {
                return 16;
            }
            p = &probs[BANDS[n]][0];
        }
        let v: i32;
        let next_ctx;
        if !br.get(p[2]) {
            v = 1;
            next_ctx = 1;
        } else {
            v = large_value(br, p);
            next_ctx = 2;
        }
        let s = if br.get(128) { -v } else { v };
        out[ZIGZAG[n]] = (s * dq[(n > 0) as usize]).clamp(-32768, 32767) as i16;
        n += 1;
        if n < 16 {
            p = &probs[BANDS[n]][next_ctx];
        }
    }
    16
}

fn large_value(br: &mut Bool, p: &[u8; 11]) -> i32 {
    if !br.get(p[3]) {
        if !br.get(p[4]) {
            2
        } else {
            3 + br.get(p[5]) as i32
        }
    } else if !br.get(p[6]) {
        if !br.get(p[7]) {
            5 + br.get(159) as i32
        } else {
            7 + 2 * br.get(165) as i32 + br.get(145) as i32
        }
    } else {
        let b1 = br.get(p[8]) as usize;
        let b0 = br.get(p[9 + b1]) as usize;
        let cat = 2 * b1 + b0;
        let tab: &[u8] = match cat {
            0 => &CAT3,
            1 => &CAT4,
            2 => &CAT5,
            _ => &CAT6,
        };
        let mut v = 0i32;
        for &pr in tab {
            v = v + v + br.get(pr) as i32;
        }
        v + 3 + (8 << cat)
    }
}

/// The inverse Walsh-Hadamard transform: the 16 luma DCs into each block's coefficient 0.
fn wht(inp: &[i16; 16], out: &mut [i16; 384]) {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let a0 = inp[i] as i32 + inp[12 + i] as i32;
        let a1 = inp[4 + i] as i32 + inp[8 + i] as i32;
        let a2 = inp[4 + i] as i32 - inp[8 + i] as i32;
        let a3 = inp[i] as i32 - inp[12 + i] as i32;
        t[i] = a0 + a1;
        t[8 + i] = a0 - a1;
        t[4 + i] = a3 + a2;
        t[12 + i] = a3 - a2;
    }
    for i in 0..4 {
        let dc = t[4 * i] + 3;
        let a0 = dc + t[4 * i + 3];
        let a1 = t[4 * i + 1] + t[4 * i + 2];
        let a2 = t[4 * i + 1] - t[4 * i + 2];
        let a3 = dc - t[4 * i + 3];
        let o = 64 * i;
        out[o] = ((a0 + a1) >> 3) as i16;
        out[o + 16] = ((a3 + a2) >> 3) as i16;
        out[o + 32] = ((a0 - a1) >> 3) as i16;
        out[o + 48] = ((a3 - a2) >> 3) as i16;
    }
}

fn mul1(a: i32) -> i32 {
    ((a * 20091) >> 16) + a
}

fn mul2(a: i32) -> i32 {
    (a * 35468) >> 16
}

/// Add the inverse DCT of `c` to the 4x4 block at `dst` (row stride `s`).
fn idct_add(c: &[i16], dst: &mut [u8], at: usize, s: usize) {
    if c.iter().all(|&v| v == 0) {
        return;
    }
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (i0, i4, i8, i12) = (
            c[i] as i32,
            c[4 + i] as i32,
            c[8 + i] as i32,
            c[12 + i] as i32,
        );
        let a = i0 + i8;
        let b = i0 - i8;
        let cc = mul2(i4) - mul1(i12);
        let d = mul1(i4) + mul2(i12);
        t[4 * i] = a + d;
        t[4 * i + 1] = b + cc;
        t[4 * i + 2] = b - cc;
        t[4 * i + 3] = a - d;
    }
    for i in 0..4 {
        let dc = t[i] + 4;
        let a = dc + t[8 + i];
        let b = dc - t[8 + i];
        let cc = mul2(t[4 + i]) - mul1(t[12 + i]);
        let d = mul1(t[4 + i]) + mul2(t[12 + i]);
        let row = at + i * s;
        for (x, v) in [a + d, b + cc, b - cc, a - d].into_iter().enumerate() {
            let p = &mut dst[row + x];
            *p = (*p as i32 + (v >> 3)).clamp(0, 255) as u8;
        }
    }
}

const BPS: usize = 32;

fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((a as u32 + 2 * b as u32 + c as u32 + 2) >> 2) as u8
}

fn avg2(a: u8, b: u8) -> u8 {
    ((a as u32 + b as u32 + 1) >> 1) as u8
}

/// Predict the 4x4 block at `o` in the work buffer `b` (stride BPS) with mode `m`.
fn pred4(b: &mut [u8], o: usize, m: u8) {
    let top = |i: isize| b[(o as isize - BPS as isize + i) as usize];
    let left = |j: usize| b[o + j * BPS - 1];
    let (x_, a, bb, c, d, e, f, g, h) = (
        top(-1),
        top(0),
        top(1),
        top(2),
        top(3),
        top(4),
        top(5),
        top(6),
        top(7),
    );
    let (i, j, k, l) = (left(0), left(1), left(2), left(3));
    let mut out = [[0u8; 4]; 4]; // out[y][x]
    match m {
        B_DC => {
            let mut dc = 4u32;
            for n in 0..4 {
                dc += top(n) as u32 + left(n as usize) as u32;
            }
            out = [[(dc >> 3) as u8; 4]; 4];
        }
        B_TM => {
            for (y, row) in out.iter_mut().enumerate() {
                for (xx, v) in row.iter_mut().enumerate() {
                    *v = (top(xx as isize) as i32 + left(y) as i32 - x_ as i32).clamp(0, 255) as u8;
                }
            }
        }
        B_VE => {
            let vals = [
                avg3(x_, a, bb),
                avg3(a, bb, c),
                avg3(bb, c, d),
                avg3(c, d, e),
            ];
            out = [vals; 4];
        }
        B_HE => {
            out[0] = [avg3(x_, i, j); 4];
            out[1] = [avg3(i, j, k); 4];
            out[2] = [avg3(j, k, l); 4];
            out[3] = [avg3(k, l, l); 4];
        }
        B_RD => {
            let s = |yy: usize, xx: usize, v: u8, o: &mut [[u8; 4]; 4]| o[yy][xx] = v;
            s(3, 0, avg3(j, k, l), &mut out);
            let v = avg3(i, j, k);
            s(3, 1, v, &mut out);
            s(2, 0, v, &mut out);
            let v = avg3(x_, i, j);
            s(3, 2, v, &mut out);
            s(2, 1, v, &mut out);
            s(1, 0, v, &mut out);
            let v = avg3(a, x_, i);
            s(3, 3, v, &mut out);
            s(2, 2, v, &mut out);
            s(1, 1, v, &mut out);
            s(0, 0, v, &mut out);
            let v = avg3(bb, a, x_);
            s(2, 3, v, &mut out);
            s(1, 2, v, &mut out);
            s(0, 1, v, &mut out);
            let v = avg3(c, bb, a);
            s(1, 3, v, &mut out);
            s(0, 2, v, &mut out);
            s(0, 3, avg3(d, c, bb), &mut out);
        }
        B_LD => {
            let v = [
                avg3(a, bb, c),
                avg3(bb, c, d),
                avg3(c, d, e),
                avg3(d, e, f),
                avg3(e, f, g),
                avg3(f, g, h),
                avg3(g, h, h),
            ];
            for (y, row) in out.iter_mut().enumerate() {
                for (xx, p) in row.iter_mut().enumerate() {
                    *p = v[xx + y];
                }
            }
        }
        B_VR => {
            out[0][0] = avg2(x_, a);
            out[2][1] = out[0][0];
            out[0][1] = avg2(a, bb);
            out[2][2] = out[0][1];
            out[0][2] = avg2(bb, c);
            out[2][3] = out[0][2];
            out[0][3] = avg2(c, d);
            out[3][0] = avg3(k, j, i);
            out[2][0] = avg3(j, i, x_);
            out[1][0] = avg3(i, x_, a);
            out[3][1] = out[1][0];
            out[1][1] = avg3(x_, a, bb);
            out[3][2] = out[1][1];
            out[1][2] = avg3(a, bb, c);
            out[3][3] = out[1][2];
            out[1][3] = avg3(bb, c, d);
        }
        B_VL => {
            out[0][0] = avg2(a, bb);
            out[0][1] = avg2(bb, c);
            out[2][0] = out[0][1];
            out[0][2] = avg2(c, d);
            out[2][1] = out[0][2];
            out[0][3] = avg2(d, e);
            out[2][2] = out[0][3];
            out[1][0] = avg3(a, bb, c);
            out[1][1] = avg3(bb, c, d);
            out[3][0] = out[1][1];
            out[1][2] = avg3(c, d, e);
            out[3][1] = out[1][2];
            out[1][3] = avg3(d, e, f);
            out[3][2] = out[1][3];
            out[2][3] = avg3(e, f, g);
            out[3][3] = avg3(f, g, h);
        }
        B_HD => {
            out[0][0] = avg2(i, x_);
            out[1][2] = out[0][0];
            out[1][0] = avg2(j, i);
            out[2][2] = out[1][0];
            out[2][0] = avg2(k, j);
            out[3][2] = out[2][0];
            out[3][0] = avg2(l, k);
            out[0][3] = avg3(a, bb, c);
            out[0][2] = avg3(x_, a, bb);
            out[0][1] = avg3(i, x_, a);
            out[1][3] = out[0][1];
            out[1][1] = avg3(j, i, x_);
            out[2][3] = out[1][1];
            out[2][1] = avg3(k, j, i);
            out[3][3] = out[2][1];
            out[3][1] = avg3(l, k, j);
        }
        _ => {
            // B_HU
            out[0][0] = avg2(i, j);
            out[0][2] = avg2(j, k);
            out[1][0] = out[0][2];
            out[1][2] = avg2(k, l);
            out[2][0] = out[1][2];
            out[0][1] = avg3(i, j, k);
            out[0][3] = avg3(j, k, l);
            out[1][1] = out[0][3];
            out[1][3] = avg3(k, l, l);
            out[2][1] = out[1][3];
            out[2][3] = l;
            out[2][2] = l;
            out[3] = [l; 4];
        }
    }
    for (y, row) in out.iter().enumerate() {
        b[o + y * BPS..o + y * BPS + 4].copy_from_slice(row);
    }
}

/// Predict an n x n block (16 luma or 8 chroma) at `o` with a whole-block mode.
fn pred_big(b: &mut [u8], o: usize, n: usize, m: u8, has_top: bool, has_left: bool) {
    let top: Vec<u8> = (0..n).map(|x| b[o - BPS + x]).collect();
    let left: Vec<u8> = (0..n).map(|y| b[o + y * BPS - 1]).collect();
    let tl = b[o - BPS - 1] as i32;
    let shift = if n == 16 { 4 } else { 3 };
    for y in 0..n {
        for x in 0..n {
            b[o + y * BPS + x] = match m {
                B_VE => top[x],
                B_HE => left[y],
                B_TM => (top[x] as i32 + left[y] as i32 - tl).clamp(0, 255) as u8,
                _ => {
                    let st: u32 = top.iter().map(|&v| v as u32).sum();
                    let sl: u32 = left.iter().map(|&v| v as u32).sum();
                    match (has_top, has_left) {
                        (true, true) => ((st + sl + n as u32) >> (shift + 1)) as u8,
                        (true, false) => ((st + (n as u32 >> 1)) >> shift) as u8,
                        (false, true) => ((sl + (n as u32 >> 1)) >> shift) as u8,
                        (false, false) => 0x80,
                    }
                }
            };
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reconstruct(
    yp: &mut [u8],
    up: &mut [u8],
    vp: &mut [u8],
    ys: usize,
    uvs: usize,
    mbx: usize,
    mby: usize,
    mbw: usize,
    i4x4: bool,
    ymode: u8,
    bmodes: &[u8; 16],
    uvmode: u8,
    coeffs: &[i16; 384],
) {
    // Luma work area: row 0 is the row above, column 0 the column to the left, then 16x16
    // plus 4 more columns above for the top-right samples.
    let mut yb = [0u8; BPS * 17];
    let o = BPS + 1;
    let (x0, y0) = (mbx * 16, mby * 16);
    // The top row (with top-left and 4 top-right samples).
    if mby == 0 {
        yb[..21].fill(127);
    } else {
        let r = (y0 - 1) * ys;
        yb[0] = if mbx == 0 { 129 } else { yp[r + x0 - 1] };
        yb[1..17].copy_from_slice(&yp[r + x0..r + x0 + 16]);
        if mbx + 1 < mbw {
            yb[17..21].copy_from_slice(&yp[r + x0 + 16..r + x0 + 20]);
        } else {
            let v = yp[r + x0 + 15];
            yb[17..21].fill(v);
        }
    }
    for j in 0..16 {
        yb[o + j * BPS - 1] = if mbx == 0 {
            129
        } else {
            yp[(y0 + j) * ys + x0 - 1]
        };
    }
    if i4x4 {
        // The top-right samples repeat down the right edge for the 4x4 blocks there.
        let tr: [u8; 4] = [yb[17], yb[18], yb[19], yb[20]];
        for k in 1..4 {
            let at = o + (4 * k) * BPS - BPS + 16;
            yb[at..at + 4].copy_from_slice(&tr);
        }
        for n in 0..16 {
            let (bx, by) = (n % 4, n / 4);
            let at = o + by * 4 * BPS + bx * 4;
            pred4(&mut yb, at, bmodes[n]);
            idct_add(&coeffs[n * 16..n * 16 + 16], &mut yb, at, BPS);
        }
    } else {
        pred_big(&mut yb, o, 16, ymode, mby > 0, mbx > 0);
        for n in 0..16 {
            let (bx, by) = (n % 4, n / 4);
            idct_add(
                &coeffs[n * 16..n * 16 + 16],
                &mut yb,
                o + by * 4 * BPS + bx * 4,
                BPS,
            );
        }
    }
    for j in 0..16 {
        yp[(y0 + j) * ys + x0..(y0 + j) * ys + x0 + 16]
            .copy_from_slice(&yb[o + j * BPS..o + j * BPS + 16]);
    }
    // Chroma.
    let (cx, cy) = (mbx * 8, mby * 8);
    for (plane, ch) in [(up, 0usize), (vp, 1)] {
        let mut cb = [0u8; BPS * 9];
        if mby == 0 {
            cb[..9].fill(127);
        } else {
            let r = (cy - 1) * uvs;
            cb[0] = if mbx == 0 { 129 } else { plane[r + cx - 1] };
            cb[1..9].copy_from_slice(&plane[r + cx..r + cx + 8]);
        }
        for j in 0..8 {
            cb[o + j * BPS - 1] = if mbx == 0 {
                129
            } else {
                plane[(cy + j) * uvs + cx - 1]
            };
        }
        pred_big(&mut cb, o, 8, uvmode, mby > 0, mbx > 0);
        for n in 0..4 {
            let (bx, by) = (n % 2, n / 2);
            let base = 256 + ch * 64 + n * 16;
            idct_add(
                &coeffs[base..base + 16],
                &mut cb,
                o + by * 4 * BPS + bx * 4,
                BPS,
            );
        }
        for j in 0..8 {
            plane[(cy + j) * uvs + cx..(cy + j) * uvs + cx + 8]
                .copy_from_slice(&cb[o + j * BPS..o + j * BPS + 8]);
        }
    }
}

// ---- the loop filter (RFC 6386 section 15, as libwebp writes it) --------------------------

fn sclip1(v: i32) -> i32 {
    v.clamp(-128, 127)
}
fn sclip2(v: i32) -> i32 {
    v.clamp(-16, 15)
}
fn clip1(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// The pixel `k` steps (of `step`) from `i`.
#[inline(always)]
fn at(p: &[u8], i: usize, step: isize, k: isize) -> i32 {
    p[(i as isize + k * step) as usize] as i32
}

fn set(p: &mut [u8], i: usize, step: isize, k: isize, v: u8) {
    p[(i as isize + k * step) as usize] = v;
}

fn do_filter2(p: &mut [u8], i: usize, s: isize) {
    let (p1, p0, q0, q1) = (
        at(p, i, s, -2),
        at(p, i, s, -1),
        at(p, i, s, 0),
        at(p, i, s, 1),
    );
    let a = 3 * (q0 - p0) + sclip1(p1 - q1);
    let a1 = sclip2((a + 4) >> 3);
    let a2 = sclip2((a + 3) >> 3);
    set(p, i, s, -1, clip1(p0 + a2));
    set(p, i, s, 0, clip1(q0 - a1));
}

fn do_filter4(p: &mut [u8], i: usize, s: isize) {
    let (p1, p0, q0, q1) = (
        at(p, i, s, -2),
        at(p, i, s, -1),
        at(p, i, s, 0),
        at(p, i, s, 1),
    );
    let a = 3 * (q0 - p0);
    let a1 = sclip2((a + 4) >> 3);
    let a2 = sclip2((a + 3) >> 3);
    let a3 = (a1 + 1) >> 1;
    set(p, i, s, -2, clip1(p1 + a3));
    set(p, i, s, -1, clip1(p0 + a2));
    set(p, i, s, 0, clip1(q0 - a1));
    set(p, i, s, 1, clip1(q1 - a3));
}

fn do_filter6(p: &mut [u8], i: usize, s: isize) {
    let (p2, p1, p0) = (at(p, i, s, -3), at(p, i, s, -2), at(p, i, s, -1));
    let (q0, q1, q2) = (at(p, i, s, 0), at(p, i, s, 1), at(p, i, s, 2));
    let a = sclip1(3 * (q0 - p0) + sclip1(p1 - q1));
    let a1 = (27 * a + 63) >> 7;
    let a2 = (18 * a + 63) >> 7;
    let a3 = (9 * a + 63) >> 7;
    set(p, i, s, -3, clip1(p2 + a3));
    set(p, i, s, -2, clip1(p1 + a2));
    set(p, i, s, -1, clip1(p0 + a1));
    set(p, i, s, 0, clip1(q0 - a1));
    set(p, i, s, 1, clip1(q1 - a2));
    set(p, i, s, 2, clip1(q2 - a3));
}

fn hev(p: &[u8], i: usize, s: isize, t: i32) -> bool {
    let (p1, p0, q0, q1) = (
        at(p, i, s, -2),
        at(p, i, s, -1),
        at(p, i, s, 0),
        at(p, i, s, 1),
    );
    (p1 - p0).abs() > t || (q1 - q0).abs() > t
}

fn needs_filter(p: &[u8], i: usize, s: isize, t: i32) -> bool {
    let (p1, p0, q0, q1) = (
        at(p, i, s, -2),
        at(p, i, s, -1),
        at(p, i, s, 0),
        at(p, i, s, 1),
    );
    4 * (p0 - q0).abs() + (p1 - q1).abs() <= t
}

fn needs_filter2(p: &[u8], i: usize, s: isize, t: i32, it: i32) -> bool {
    let v = |k| at(p, i, s, k);
    let (p3, p2, p1, p0, q0, q1, q2, q3) = (v(-4), v(-3), v(-2), v(-1), v(0), v(1), v(2), v(3));
    if 4 * (p0 - q0).abs() + (p1 - q1).abs() > t {
        return false;
    }
    (p3 - p2).abs() <= it
        && (p2 - p1).abs() <= it
        && (p1 - p0).abs() <= it
        && (q3 - q2).abs() <= it
        && (q2 - q1).abs() <= it
        && (q1 - q0).abs() <= it
}

/// Filter `n` pixels across an edge: `hs` steps across it, `vs` along it.
#[allow(clippy::too_many_arguments)]
fn filter_loop(
    p: &mut [u8],
    mut i: usize,
    hs: isize,
    vs: usize,
    n: usize,
    thresh: i32,
    it: i32,
    hev_t: i32,
    mb_edge: bool,
) {
    let t2 = 2 * thresh + 1;
    for _ in 0..n {
        if needs_filter2(p, i, hs, t2, it) {
            if hev(p, i, hs, hev_t) {
                do_filter2(p, i, hs);
            } else if mb_edge {
                do_filter6(p, i, hs);
            } else {
                do_filter4(p, i, hs);
            }
        }
        i += vs;
    }
}

fn simple_loop(p: &mut [u8], mut i: usize, hs: isize, vs: usize, thresh: i32) {
    let t2 = 2 * thresh + 1;
    for _ in 0..16 {
        if needs_filter(p, i, hs, t2) {
            do_filter2(p, i, hs);
        }
        i += vs;
    }
}

#[allow(clippy::too_many_arguments)]
fn filter_mb(
    yp: &mut [u8],
    up: &mut [u8],
    vp: &mut [u8],
    ys: usize,
    uvs: usize,
    mbx: usize,
    mby: usize,
    f: FilterInfo,
    simple: bool,
) {
    let yo = mby * 16 * ys + mbx * 16;
    let co = mby * 8 * uvs + mbx * 8;
    let (lim, il, hv) = (f.limit, f.ilevel, f.hev);
    let (ysi, uvsi) = (ys as isize, uvs as isize);
    if simple {
        if mbx > 0 {
            simple_loop(yp, yo, 1, ys, lim + 4);
        }
        if f.inner {
            for k in 1..4 {
                simple_loop(yp, yo + 4 * k, 1, ys, lim);
            }
        }
        if mby > 0 {
            simple_loop(yp, yo, ysi, 1, lim + 4);
        }
        if f.inner {
            for k in 1..4 {
                simple_loop(yp, yo + 4 * k * ys, ysi, 1, lim);
            }
        }
        return;
    }
    if mbx > 0 {
        filter_loop(yp, yo, 1, ys, 16, lim + 4, il, hv, true);
        filter_loop(up, co, 1, uvs, 8, lim + 4, il, hv, true);
        filter_loop(vp, co, 1, uvs, 8, lim + 4, il, hv, true);
    }
    if f.inner {
        for k in 1..4 {
            filter_loop(yp, yo + 4 * k, 1, ys, 16, lim, il, hv, false);
        }
        filter_loop(up, co + 4, 1, uvs, 8, lim, il, hv, false);
        filter_loop(vp, co + 4, 1, uvs, 8, lim, il, hv, false);
    }
    if mby > 0 {
        filter_loop(yp, yo, ysi, 1, 16, lim + 4, il, hv, true);
        filter_loop(up, co, uvsi, 1, 8, lim + 4, il, hv, true);
        filter_loop(vp, co, uvsi, 1, 8, lim + 4, il, hv, true);
    }
    if f.inner {
        for k in 1..4 {
            filter_loop(yp, yo + 4 * k * ys, ysi, 1, 16, lim, il, hv, false);
        }
        filter_loop(up, co + 4 * uvs, uvsi, 1, 8, lim, il, hv, false);
        filter_loop(vp, co + 4 * uvs, uvsi, 1, 8, lim, il, hv, false);
    }
}

// ---- YUV to RGB ----------------------------------------------------------------------------

fn mult_hi(v: i32, c: i32) -> i32 {
    (v * c) >> 8
}

fn clip8(v: i32) -> u32 {
    if v & !16383 == 0 {
        (v >> 6) as u32
    } else if v < 0 {
        0
    } else {
        255
    }
}

fn yuv_rgb(y: u8, u: u8, v: u8) -> u32 {
    let (y, u, v) = (y as i32, u as i32, v as i32);
    let r = clip8(mult_hi(y, 19077) + mult_hi(v, 26149) - 14234);
    let g = clip8(mult_hi(y, 19077) - mult_hi(u, 6419) - mult_hi(v, 13320) + 8708);
    let b = clip8(mult_hi(y, 19077) + mult_hi(u, 33050) - 17685);
    0xFF00_0000 | r << 16 | g << 8 | b
}

/// One output row from luma row `yrow` and the two chroma rows to blend (libwebp's
/// UpsampleRgbLinePair, for one of its two rows): `near` weighs 3, `far` 1.
fn fancy_row(
    yrow: &[u8],
    near_u: &[u8],
    near_v: &[u8],
    far_u: &[u8],
    far_v: &[u8],
    out: &mut [u32],
) {
    let len = out.len();
    // Chroma blended vertically first: (3 * near + far), then horizontally like libwebp.
    let uv = |i: usize| -> (u32, u32) {
        (
            near_u[i] as u32 | (near_v[i] as u32) << 16,
            far_u[i] as u32 | (far_v[i] as u32) << 16,
        )
    };
    let emit =
        |o: &mut u32, y: u8, uvv: u32| *o = yuv_rgb(y, (uvv & 0xFF) as u8, (uvv >> 16) as u8);
    // libwebp names: "top" is the far row, "cur" the near one, when the output row is the
    // bottom of a pair; we pass near/far so one routine serves both rows.
    let (n0, f0) = uv(0);
    let mut l_uv = n0; // near (current) left sample
    let mut tl_uv = f0; // far (top) left sample
    emit(&mut out[0], yrow[0], (3 * l_uv + tl_uv + 0x0002_0002) >> 2);
    let last_pair = (len - 1) >> 1;
    for x in 1..=last_pair {
        let (n, f) = uv(x);
        let (t_uv, cur) = (f, n);
        let avg = tl_uv + t_uv + l_uv + cur + 0x0008_0008;
        let diag_12 = (avg + 2 * (t_uv + l_uv)) >> 3;
        let diag_03 = (avg + 2 * (tl_uv + cur)) >> 3;
        // The near row's two pixels (libwebp's bottom row formulas).
        let uv0 = (diag_03 + l_uv) >> 1;
        let uv1 = (diag_12 + cur) >> 1;
        emit(&mut out[2 * x - 1], yrow[2 * x - 1], uv0 & 0x00FF_00FF);
        if 2 * x < len {
            emit(&mut out[2 * x], yrow[2 * x], uv1 & 0x00FF_00FF);
        }
        tl_uv = t_uv;
        l_uv = cur;
    }
    if len & 1 == 0 {
        emit(
            &mut out[len - 1],
            yrow[len - 1],
            ((3 * l_uv + tl_uv + 0x0002_0002) >> 2) & 0x00FF_00FF,
        );
    }
}

/// The frame as 0xAARRGGBB (opaque), cropped to w x h.
pub fn to_argb(f: &Yuv) -> Vec<u32> {
    let (w, h) = (f.w, f.h);
    let mut px = vec![0u32; w * h];
    let cw = w.div_ceil(2);
    let urow = |r: usize| {
        (
            &f.u[r * f.uvstride..r * f.uvstride + cw],
            &f.v[r * f.uvstride..r * f.uvstride + cw],
        )
    };
    let ch = h.div_ceil(2);
    for y in 0..h {
        let yrow = &f.y[y * f.ystride..y * f.ystride + w];
        // Row y sits between chroma rows (y - 1) / 2 and (y + 1) / 2; the nearer one counts 3.
        let (near, far) = if y == 0 {
            (0, 0)
        } else if y % 2 == 1 {
            ((y - 1) / 2, y.div_ceil(2).min(ch - 1))
        } else {
            (y / 2, y / 2 - 1)
        };
        let (nu, nv) = urow(near);
        let (fu, fv) = urow(far);
        fancy_row(yrow, nu, nv, fu, fv, &mut px[y * w..(y + 1) * w]);
    }
    px
}
