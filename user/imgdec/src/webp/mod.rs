//! WebP: the RIFF container (simple and extended, VP8X), lossy pictures ([`vp8`]) with their
//! alpha (ALPH: raw or lossless-compressed, with the three prediction filters), lossless pictures
//! ([`vp8l`]), and animations (ANIM/ANMF frames with offsets, durations, blending and disposal;
//! played by [`crate::anim::Animation`]). ICC, EXIF and XMP chunks are ignored.

use alloc::vec::Vec;

use crate::{Image, ImgError, MAX_PIXELS};

pub mod vp8;
mod vp8_tables;
pub mod vp8l;

fn corrupt() -> ImgError {
    ImgError::Corrupt("The WebP file is damaged.")
}

fn le24(b: &[u8]) -> usize {
    b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16
}

/// The chunks in `d[start..end]`: (fourcc, data range). A chunk that runs past the end is cut
/// to what is there.
fn chunks(d: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut p = start;
    while p + 8 <= end {
        let id = [d[p], d[p + 1], d[p + 2], d[p + 3]];
        let size = u32::from_le_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]) as usize;
        let ds = p + 8;
        let de = ds.saturating_add(size).min(end);
        out.push((id, ds, de));
        p = ds.saturating_add(size).saturating_add(size & 1);
        if out.len() > 100_000 {
            break;
        }
    }
    out
}

/// One frame of an animation (or the only picture of a still file).
#[derive(Clone, Debug)]
pub struct FrameInfo {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub duration_ms: u32,
    /// Alpha-blend over the canvas (else replace the rectangle).
    pub blend: bool,
    /// Clear the rectangle to transparent before the next frame.
    pub dispose: bool,
    /// The frame's chunks (ALPH, VP8, VP8L) in the file.
    start: usize,
    end: usize,
}

#[derive(Clone, Debug)]
pub struct WebP {
    pub w: usize,
    pub h: usize,
    pub animated: bool,
    /// Times to play; 0 = for ever.
    pub plays: u32,
    pub frames: Vec<FrameInfo>,
}

impl WebP {
    pub fn parse(d: &[u8]) -> Result<WebP, ImgError> {
        if d.len() < 12 || &d[..4] != b"RIFF" || &d[8..12] != b"WEBP" {
            return Err(ImgError::Unrecognised("This isn't a WebP file."));
        }
        let riff = u32::from_le_bytes([d[4], d[5], d[6], d[7]]) as usize;
        let end = riff.saturating_add(8).min(d.len());
        let cs = chunks(d, 12, end);
        let Some(&(first, fs, fe)) = cs.first() else {
            return Err(ImgError::Truncated);
        };
        match &first {
            b"VP8 " | b"VP8L" => {
                let (w, h) = still_size(&first, &d[fs..fe])?;
                Ok(WebP {
                    w,
                    h,
                    animated: false,
                    plays: 1,
                    frames: alloc::vec![FrameInfo {
                        x: 0,
                        y: 0,
                        w,
                        h,
                        duration_ms: 0,
                        blend: false,
                        dispose: false,
                        start: 12,
                        end,
                    }],
                })
            }
            b"VP8X" => {
                let x = d
                    .get(fs..fe)
                    .filter(|x| x.len() >= 10)
                    .ok_or(ImgError::Truncated)?;
                let animated = x[0] & 0x02 != 0;
                let w = le24(&x[4..7]) + 1;
                let h = le24(&x[7..10]) + 1;
                if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
                    return Err(ImgError::TooBig);
                }
                let mut frames = Vec::new();
                let mut plays = 1;
                if animated {
                    for &(id, s, e) in &cs[1..] {
                        match &id {
                            b"ANIM" if e - s >= 6 => {
                                let n = u16::from_le_bytes([d[s + 4], d[s + 5]]) as u32;
                                plays = n;
                            }
                            b"ANMF" if e - s >= 16 => {
                                let f = &d[s..e];
                                let fw = le24(&f[6..9]) + 1;
                                let fh = le24(&f[9..12]) + 1;
                                if fw.checked_mul(fh).is_none_or(|n| n > MAX_PIXELS) {
                                    return Err(ImgError::TooBig);
                                }
                                frames.push(FrameInfo {
                                    x: le24(&f[0..3]) * 2,
                                    y: le24(&f[3..6]) * 2,
                                    w: fw,
                                    h: fh,
                                    duration_ms: le24(&f[12..15]) as u32,
                                    blend: f[15] & 0x02 == 0,
                                    dispose: f[15] & 0x01 != 0,
                                    start: s + 16,
                                    end: e,
                                });
                            }
                            _ => {}
                        }
                    }
                    if frames.is_empty() {
                        return Err(ImgError::Corrupt("The animated WebP has no frames."));
                    }
                } else {
                    frames.push(FrameInfo {
                        x: 0,
                        y: 0,
                        w,
                        h,
                        duration_ms: 0,
                        blend: false,
                        dispose: false,
                        start: fe + (fe - fs) % 2,
                        end,
                    });
                }
                Ok(WebP {
                    w,
                    h,
                    animated,
                    plays,
                    frames,
                })
            }
            _ => Err(corrupt()),
        }
    }

    /// Frame `i`'s own picture (not composited).
    pub fn frame_image(&self, d: &[u8], i: usize) -> Result<Image, ImgError> {
        let f = self.frames.get(i).ok_or_else(corrupt)?;
        let img = decode_chunks(d, f.start, f.end)?;
        if self.animated && (img.w != f.w || img.h != f.h) {
            return Err(corrupt());
        }
        Ok(img)
    }
}

fn still_size(id: &[u8; 4], c: &[u8]) -> Result<(usize, usize), ImgError> {
    if id == b"VP8L" {
        let (w, h, _) = vp8l::header(c)?;
        Ok((w, h))
    } else {
        vp8::header(c)
    }
}

/// Decode the picture in the chunks `d[start..end]`: VP8L, or VP8 with an optional ALPH.
fn decode_chunks(d: &[u8], start: usize, end: usize) -> Result<Image, ImgError> {
    let mut alph: Option<(usize, usize)> = None;
    for (id, s, e) in chunks(d, start, end) {
        match &id {
            b"ALPH" => alph = Some((s, e)),
            b"VP8L" => return vp8l::decode(&d[s..e]),
            b"VP8 " => {
                let f = vp8::decode(&d[s..e])?;
                let mut img = Image {
                    w: f.w,
                    h: f.h,
                    px: vp8::to_argb(&f),
                };
                if let Some((a, b)) = alph {
                    apply_alpha(&mut img, &d[a..b])?;
                }
                return Ok(img);
            }
            _ => {}
        }
    }
    Err(ImgError::Corrupt("The WebP file has no picture in it."))
}

fn apply_alpha(img: &mut Image, c: &[u8]) -> Result<(), ImgError> {
    let (w, h) = (img.w, img.h);
    let hdr = *c.first().ok_or(ImgError::Truncated)?;
    let method = hdr & 3;
    let filter = (hdr >> 2) & 3;
    let data = &c[1..];
    let mut a = match method {
        0 => {
            let raw = data.get(..w * h).ok_or(ImgError::Truncated)?;
            raw.to_vec()
        }
        1 => vp8l::decode_alpha(data, w, h)?,
        _ => return Err(corrupt()),
    };
    // Undo the prediction filter, row by row.
    for y in 0..h {
        let (done, rest) = a.split_at_mut(y * w);
        let row = &mut rest[..w];
        let prev = if y > 0 {
            Some(&done[(y - 1) * w..])
        } else {
            None
        };
        match (filter, prev) {
            (0, _) => {}
            (1, p) | (_, p @ None) => {
                let mut pred = p.map_or(0, |p| p[0]);
                for v in row.iter_mut() {
                    *v = v.wrapping_add(pred);
                    pred = *v;
                }
            }
            (2, Some(p)) => {
                for (v, &t) in row.iter_mut().zip(p) {
                    *v = v.wrapping_add(t);
                }
            }
            (_, Some(p)) => {
                let (mut left, mut tl) = (p[0], p[0]);
                for (v, &top) in row.iter_mut().zip(p) {
                    let g = left as i32 + top as i32 - tl as i32;
                    let pred = g.clamp(0, 255) as u8;
                    *v = v.wrapping_add(pred);
                    left = *v;
                    tl = top;
                }
            }
        }
    }
    for (p, &al) in img.px.iter_mut().zip(&a) {
        *p = (*p & 0x00FF_FFFF) | (al as u32) << 24;
    }
    Ok(())
}

/// The picture (an animation's first frame, composited on its canvas).
pub fn decode(d: &[u8]) -> Result<Image, ImgError> {
    let wp = WebP::parse(d)?;
    if !wp.animated {
        return wp.frame_image(d, 0);
    }
    let mut a = crate::anim::Animation::new(d.to_vec())?;
    match a.next_frame() {
        Some(Ok(f)) => Ok(f.image.clone()),
        Some(Err(e)) => Err(e),
        None => Err(corrupt()),
    }
}

pub fn probe(d: &[u8]) -> Result<(usize, usize), ImgError> {
    let wp = WebP::parse(d)?;
    Ok((wp.w, wp.h))
}
