//! GIF (87a and 89a): LZW, global and local palettes, transparency, interlacing, and every frame
//! of an animation with its delay, disposal and the NETSCAPE loop count. [`decode`] gives the
//! first frame; [`crate::anim::Animation`] plays the rest.
//!
//! Damaged picture data is shown the way browsers show it: what decoded is kept and the rest of
//! the frame stays transparent. A frame bigger than [`MAX_PIXELS`] is refused; frames that reach
//! past the canvas are clipped to it.

use alloc::vec::Vec;

use crate::{Image, ImgError, MAX_PIXELS};

/// Frames we look at before calling the file hostile.
pub const MAX_FRAMES: usize = 10_000;

/// How a frame leaves the canvas before the next one is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dispose {
    /// Leave it (GIF disposal 0 and 1).
    Keep,
    /// Clear the frame's rectangle to transparent (2).
    Background,
    /// Put back what was there before the frame (3).
    Previous,
}

/// One frame's header: where its data is in the file.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub interlace: bool,
    /// Its own palette: (offset in the file, entries).
    lct: Option<(usize, usize)>,
    pub transparent: Option<u8>,
    pub dispose: Dispose,
    /// Already adjusted the way browsers do (0 or 10 ms play as 100 ms).
    pub delay_ms: u32,
    min_code: u8,
    /// Offset of the first data sub-block's length byte.
    data: usize,
}

/// A parsed GIF: the canvas, the palette and every frame's header (no pixels yet).
#[derive(Clone, Debug)]
pub struct Gif {
    pub w: usize,
    pub h: usize,
    gct: Option<(usize, usize)>,
    pub frames: Vec<FrameInfo>,
    /// Times to play the whole animation; 0 = for ever.
    pub plays: u32,
}

fn le16(d: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes([*d.get(i)?, *d.get(i + 1)?]) as usize)
}

/// Step over data sub-blocks starting at `pos`; returns the position after the terminator
/// (or the end of the file).
fn skip_blocks(d: &[u8], mut pos: usize) -> usize {
    while let Some(&n) = d.get(pos) {
        pos += 1;
        if n == 0 {
            return pos;
        }
        pos += n as usize;
    }
    d.len()
}

impl Gif {
    pub fn parse(d: &[u8]) -> Result<Gif, ImgError> {
        if !(d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a")) {
            return Err(ImgError::Unrecognised("This isn't a GIF file."));
        }
        if d.len() < 13 {
            return Err(ImgError::Truncated);
        }
        let (mut w, mut h) = (le16(d, 6).unwrap_or(0), le16(d, 8).unwrap_or(0));
        let flags = d[10];
        let mut pos = 13;
        let gct = if flags & 0x80 != 0 {
            let n = 2usize << (flags & 7);
            if d.len() < pos + 3 * n {
                return Err(ImgError::Truncated);
            }
            let g = (pos, n);
            pos += 3 * n;
            Some(g)
        } else {
            None
        };
        let mut frames = Vec::new();
        let mut plays = 1;
        // The graphic control extension that applies to the next image.
        let (mut transparent, mut dispose, mut delay) = (None, Dispose::Keep, 0u32);
        while let Some(&b) = d.get(pos) {
            pos += 1;
            match b {
                0x3B => break,
                0x21 => {
                    let Some(&label) = d.get(pos) else { break };
                    pos += 1;
                    match label {
                        0xF9 => {
                            if let (Some(&4), Some(g)) = (d.get(pos), d.get(pos + 1..pos + 5)) {
                                dispose = match (g[0] >> 2) & 7 {
                                    2 => Dispose::Background,
                                    3 => Dispose::Previous,
                                    _ => Dispose::Keep,
                                };
                                delay = u16::from_le_bytes([g[1], g[2]]) as u32 * 10;
                                transparent = (g[0] & 1 != 0).then_some(g[3]);
                            }
                        }
                        0xFF => {
                            // NETSCAPE2.0 (or ANIMEXTS1.0): sub-block 1 is the loop count.
                            if d.get(pos) == Some(&11)
                                && let Some(id) = d.get(pos + 1..pos + 12)
                                && (id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0")
                                && let Some(s) = d.get(pos + 12..pos + 16)
                                && s[0] == 3
                                && s[1] == 1
                            {
                                let n = u16::from_le_bytes([s[2], s[3]]) as u32;
                                plays = if n == 0 { 0 } else { n.saturating_add(1) };
                            }
                        }
                        _ => {}
                    }
                    pos = skip_blocks(d, pos);
                }
                0x2C => {
                    let Some(id) = d.get(pos..pos + 9) else { break };
                    let fx = u16::from_le_bytes([id[0], id[1]]) as usize;
                    let fy = u16::from_le_bytes([id[2], id[3]]) as usize;
                    let fw = u16::from_le_bytes([id[4], id[5]]) as usize;
                    let fh = u16::from_le_bytes([id[6], id[7]]) as usize;
                    let fl = id[8];
                    pos += 9;
                    let lct = if fl & 0x80 != 0 {
                        let n = 2usize << (fl & 7);
                        let l = (pos, n);
                        pos += 3 * n;
                        Some(l)
                    } else {
                        None
                    };
                    let Some(&min_code) = d.get(pos) else { break };
                    pos += 1;
                    let data = pos;
                    pos = skip_blocks(d, pos);
                    if fw.checked_mul(fh).is_none_or(|n| n > MAX_PIXELS) {
                        return Err(ImgError::TooBig);
                    }
                    frames.push(FrameInfo {
                        x: fx,
                        y: fy,
                        w: fw,
                        h: fh,
                        interlace: fl & 0x40 != 0,
                        lct: lct.filter(|&(o, n)| o + 3 * n <= d.len()),
                        transparent,
                        dispose,
                        delay_ms: if delay <= 10 { 100 } else { delay },
                        min_code,
                        data,
                    });
                    if frames.len() >= MAX_FRAMES {
                        break;
                    }
                    (transparent, dispose, delay) = (None, Dispose::Keep, 0);
                }
                // Anything else: the file is damaged from here on; keep what we have.
                _ => break,
            }
        }
        if frames.is_empty() {
            return Err(if pos >= d.len() {
                ImgError::Truncated
            } else {
                ImgError::Corrupt("The GIF has no picture in it.")
            });
        }
        // A zero-sized screen: browsers use the first frame's extent.
        if w == 0 || h == 0 {
            let f = &frames[0];
            w = f.x + f.w;
            h = f.y + f.h;
        }
        if w == 0 || h == 0 {
            return Err(ImgError::Corrupt("The GIF has no size."));
        }
        if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
            return Err(ImgError::TooBig);
        }
        Ok(Gif {
            w,
            h,
            gct,
            frames,
            plays,
        })
    }

    /// Draw frame `i` over `canvas` (`w` x `h`): opaque pixels replace, transparent ones are
    /// skipped. Damaged LZW data ends the frame early, quietly.
    pub fn draw_frame(&self, d: &[u8], i: usize, canvas: &mut Image) -> Result<(), ImgError> {
        let f = self
            .frames
            .get(i)
            .ok_or(ImgError::Corrupt("No such GIF frame."))?;
        let pal = f.lct.or(self.gct);
        let colour = |idx: u8| -> u32 {
            match pal {
                Some((o, n)) if (idx as usize) < n => {
                    let p = o + 3 * idx as usize;
                    0xFF00_0000 | (d[p] as u32) << 16 | (d[p + 1] as u32) << 8 | d[p + 2] as u32
                }
                Some(_) => 0xFF00_0000,
                // No palette at all: a grey ramp.
                None => 0xFF00_0000 | ((idx as u32) * 0x01_0101),
            }
        };
        if f.w == 0 || f.h == 0 {
            return Ok(());
        }
        let (cw, ch) = (canvas.w, canvas.h);
        // Interlaced rows come in four passes.
        const PASSES: [(usize, usize); 4] = [(0, 8), (4, 8), (2, 4), (1, 2)];
        let (mut x, mut y, mut pass) = (0usize, 0usize, 0usize);
        if f.interlace {
            y = PASSES[0].0;
        }
        let total = f.w * f.h;
        let mut done = 0usize;
        lzw(d, f.data, f.min_code, |idx| {
            if done >= total {
                return false;
            }
            done += 1;
            let (px, py) = (f.x + x, f.y + y);
            if Some(idx) != f.transparent && px < cw && py < ch {
                canvas.px[py * cw + px] = colour(idx);
            }
            x += 1;
            if x == f.w {
                x = 0;
                if f.interlace {
                    y += PASSES[pass].1;
                    while y >= f.h && pass < 3 {
                        pass += 1;
                        y = PASSES[pass].0;
                    }
                } else {
                    y += 1;
                }
                if y >= f.h {
                    return false;
                }
            }
            true
        });
        Ok(())
    }
}

/// LZW-decode the sub-blocks at `pos`, calling `out` for each index until it returns false,
/// the end code, the end of the data or a bad code.
fn lzw(d: &[u8], pos: usize, min_code: u8, mut out: impl FnMut(u8) -> bool) {
    if !(1..=11).contains(&min_code) {
        return;
    }
    // On the heap: 20 KB is a lot for a small user stack.
    let mut prefix = alloc::vec![0u16; 4096];
    let mut suffix = alloc::vec![0u8; 4096];
    let mut first = alloc::vec![0u8; 4096];
    let mut stack = alloc::vec![0u8; 4096];
    let clear = 1u16 << min_code;
    let end = clear + 1;
    for c in 0..clear {
        suffix[c as usize] = c as u8;
        first[c as usize] = c as u8;
    }
    let mut next = clear + 2;
    let mut size = min_code as u32 + 1;
    let mut prev: Option<u16> = None;
    // The bit reader over the sub-blocks.
    let (mut p, mut left) = (pos, 0usize);
    let (mut acc, mut nbits) = (0u32, 0u32);
    loop {
        while nbits < size {
            if left == 0 {
                let Some(&n) = d.get(p) else { return };
                p += 1;
                if n == 0 {
                    return;
                }
                left = n as usize;
            }
            let Some(&b) = d.get(p) else { return };
            p += 1;
            left -= 1;
            acc |= (b as u32) << nbits;
            nbits += 8;
        }
        let code = (acc & ((1 << size) - 1)) as u16;
        acc >>= size;
        nbits -= size;
        if code == clear {
            next = clear + 2;
            size = min_code as u32 + 1;
            prev = None;
            continue;
        }
        if code == end {
            return;
        }
        let Some(pv) = prev else {
            if code >= clear {
                return;
            }
            if !out(code as u8) {
                return;
            }
            prev = Some(code);
            continue;
        };
        let (start, firstc) = if code < next {
            (code, first[code as usize])
        } else if code == next {
            (pv, first[pv as usize])
        } else {
            return; // a code from the future: damaged
        };
        // Unwind the string for `start` (then the extra char for code == next).
        let mut n = 0;
        let mut c = start;
        loop {
            stack[n] = suffix[c as usize];
            n += 1;
            if c < clear || n >= stack.len() {
                break;
            }
            c = prefix[c as usize];
        }
        for i in (0..n).rev() {
            if !out(stack[i]) {
                return;
            }
        }
        if code == next && !out(firstc) {
            return;
        }
        if next < 4096 {
            prefix[next as usize] = pv;
            suffix[next as usize] = firstc;
            first[next as usize] = first[pv as usize];
            next += 1;
            if next == 1 << size && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
}

/// The first frame on a transparent canvas.
pub fn decode(d: &[u8]) -> Result<Image, ImgError> {
    let g = Gif::parse(d)?;
    let mut img = Image::new(g.w, g.h)?;
    g.draw_frame(d, 0, &mut img)?;
    Ok(img)
}

/// The canvas size, without decoding.
pub fn probe(d: &[u8]) -> Result<(usize, usize), ImgError> {
    let g = Gif::parse(d)?;
    Ok((g.w, g.h))
}
