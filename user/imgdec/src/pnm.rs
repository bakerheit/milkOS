//! Netpbm: PBM, PGM, PPM, in the text (P1, P2, P3) and binary (P4, P5, P6) forms, maxval up to
//! 65535. Comments (`#` to the end of the line) are allowed in the header (and, in the text
//! forms, between numbers). Greys and bits come out as grey RGB, always opaque.

use crate::{Image, ImgError, MAX_PIXELS, argb};

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cur<'_> {
    fn skip_space(&mut self) {
        while let Some(&c) = self.b.get(self.i) {
            if c == b'#' {
                while self.b.get(self.i).is_some_and(|&c| c != b'\n') {
                    self.i += 1;
                }
            } else if c.is_ascii_whitespace() {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn number(&mut self) -> Result<u32, ImgError> {
        self.skip_space();
        let start = self.i;
        let mut v: u32 = 0;
        while let Some(&c) = self.b.get(self.i) {
            if !c.is_ascii_digit() {
                break;
            }
            v = v
                .checked_mul(10)
                .and_then(|v| v.checked_add((c - b'0') as u32))
                .ok_or(ImgError::Corrupt("A number in the file is too big."))?;
            self.i += 1;
        }
        if self.i == start {
            return Err(if self.i >= self.b.len() {
                ImgError::Truncated
            } else {
                ImgError::Corrupt("The file has something other than a number in its data.")
            });
        }
        Ok(v)
    }
}

pub fn decode(b: &[u8]) -> Result<Image, ImgError> {
    if b.len() < 2 || b[0] != b'P' || !(b'1'..=b'6').contains(&b[1]) {
        return Err(ImgError::Unrecognised("This isn't a PPM, PGM or PBM file."));
    }
    let kind = b[1];
    let mut c = Cur { b, i: 2 };
    let (w, h) = (c.number()? as usize, c.number()? as usize);
    if w == 0 || h == 0 {
        return Err(ImgError::Corrupt("The picture has no size."));
    }
    if w.checked_mul(h).is_none_or(|n| n > MAX_PIXELS) {
        return Err(ImgError::TooBig);
    }
    let bitmap = kind == b'1' || kind == b'4';
    let maxval = if bitmap { 1 } else { c.number()? };
    if maxval == 0 || maxval > 65535 {
        return Err(ImgError::Corrupt(
            "The maximum sample value is out of range.",
        ));
    }
    let scale = |v: u32| -> u8 { ((v * 255 + maxval / 2) / maxval) as u8 };
    let mut img = Image::new(w, h)?;
    let n = w * h;
    match kind {
        b'1'..=b'3' => {
            let chans = if kind == b'3' { 3 } else { 1 };
            for p in img.px.iter_mut() {
                let mut s = [0u32; 3];
                for v in s.iter_mut().take(chans) {
                    if bitmap {
                        // The bits may run together ("0110") or be spaced.
                        c.skip_space();
                        let d = c.b.get(c.i).copied().ok_or(ImgError::Truncated)?;
                        if d != b'0' && d != b'1' {
                            return Err(ImgError::Corrupt(
                                "A bitmap has a character other than 0 or 1.",
                            ));
                        }
                        c.i += 1;
                        *v = (d - b'0') as u32;
                    } else {
                        *v = c.number()?;
                        if *v > maxval {
                            return Err(ImgError::Corrupt(
                                "A sample is bigger than the maximum value.",
                            ));
                        }
                    }
                }
                *p = if bitmap {
                    // PBM: 1 is black.
                    let g = if s[0] == 1 { 0 } else { 255 };
                    argb(255, g, g, g)
                } else if chans == 1 {
                    let g = scale(s[0]);
                    argb(255, g, g, g)
                } else {
                    argb(255, scale(s[0]), scale(s[1]), scale(s[2]))
                };
            }
        }
        _ => {
            // Exactly one whitespace byte after the header, then the raw data.
            let start = c.i + 1;
            if c.b.get(c.i).is_none_or(|x| !x.is_ascii_whitespace()) {
                return Err(if c.i >= c.b.len() {
                    ImgError::Truncated
                } else {
                    ImgError::Corrupt("The header isn't followed by a blank.")
                });
            }
            let data = &b[start..];
            if kind == b'4' {
                let rb = w.div_ceil(8);
                if data.len() < rb * h {
                    return Err(ImgError::Truncated);
                }
                for y in 0..h {
                    for x in 0..w {
                        let bit = (data[y * rb + x / 8] >> (7 - x % 8)) & 1;
                        let g = if bit == 1 { 0 } else { 255 };
                        img.px[y * w + x] = argb(255, g, g, g);
                    }
                }
            } else {
                let chans = if kind == b'6' { 3 } else { 1 };
                let wide = maxval > 255;
                let per = chans * if wide { 2 } else { 1 };
                if data.len() < n * per {
                    return Err(ImgError::Truncated);
                }
                let get = |i: usize| -> u32 {
                    if wide {
                        (data[2 * i] as u32) << 8 | data[2 * i + 1] as u32
                    } else {
                        data[i] as u32
                    }
                };
                for (i, p) in img.px.iter_mut().enumerate() {
                    *p = if chans == 1 {
                        let g = scale(get(i).min(maxval));
                        argb(255, g, g, g)
                    } else {
                        argb(
                            255,
                            scale(get(3 * i).min(maxval)),
                            scale(get(3 * i + 1).min(maxval)),
                            scale(get(3 * i + 2).min(maxval)),
                        )
                    };
                }
            }
        }
    }
    Ok(img)
}
