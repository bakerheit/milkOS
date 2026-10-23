//! Image decoders for Preview and the Browser: PNG, JPEG, GIF, WebP, PPM/PGM/PBM (P1 to P6) and
//! BMP, and scaling for zoom. Pixels come out as straight-alpha `0xAARRGGBB`. No unsafe, no
//! dependencies (the inflate, Huffman, LZW, VP8 and VP8L decoders are all here). Everything that
//! comes from a file is checked: sizes before allocating, lengths against the data, checksums
//! where the format has them, so a damaged or hostile file gives an [`ImgError`], never a panic,
//! an unbounded allocation or unbounded work (the host tests fuzz every decoder).
//!
//! # API (stable: the Browser builds on it)
//!
//! - [`sniff`]`(bytes) -> Format` from the first bytes; [`Format::name`] for a status line.
//! - [`probe`]`(bytes) -> Result<Info, ImgError>`: width and height (after JPEG's EXIF
//!   orientation) and whether it animates, from the headers only. For `<img>` intrinsic sizes.
//! - [`decode`]`(bytes) -> Result<Image, ImgError>`: the picture (an animation's first frame).
//! - [`anim::Animation`]`::new(Vec<u8>)`: every frame of a GIF or animated WebP, composited, one
//!   at a time with its delay (`next_frame`, `rewind`, `should_loop`); a still picture is one frame.
//! - [`Image`]: `w`, `h`, `px` (row-major ARGB), `scaled`, `rotated`, `fit_size`, `to_png`,
//!   [`Image::flatten_pixel`].
//!
//! What is decoded: PNG (every colour type and depth, tRNS, Adam7); JPEG (baseline and
//! progressive, grey/YCbCr/RGB/CMYK, any subsampling, restart markers, EXIF orientation); GIF
//! (LZW, transparency, interlace, animation with disposal and loop count); WebP (lossy VP8 with
//! ALPH alpha, lossless VP8L, animation); PNM; BMP. Not supported (and said so, not silently
//! wrong): arithmetic-coded, lossless and 12-bit JPEG, RLE-compressed BMP, PDF (only its text,
//! see [`pdf`]), colour management (gamma and ICC profiles are ignored).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

pub mod anim;
pub mod bmp;
pub mod gif;
pub mod inflate;
pub mod jpeg;
pub mod pdf;
pub mod png;
pub mod pnm;
mod scale;
pub mod webp;

// The window system's screenshot encoder, shared as it is (fixed Huffman deflate, LZ77).
#[allow(dead_code)]
#[path = "../../../kernel/src/wm/png.rs"]
mod kernel_png;

/// The most pixels an image may have (width x height): 8 million, so at most 32 MB decoded.
pub const MAX_PIXELS: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImgError {
    /// Not a format we know (the text says what it looks like, when we can tell).
    Unrecognised(&'static str),
    /// The file ends before the image does.
    Truncated,
    /// The file is damaged or breaks the format's rules.
    Corrupt(&'static str),
    /// A valid file with a feature we don't decode.
    Unsupported(&'static str),
    /// More than [`MAX_PIXELS`] (or more data than that would need).
    TooBig,
}

impl ImgError {
    /// A short sentence for a status line.
    pub fn message(&self) -> &'static str {
        match self {
            ImgError::Unrecognised(s) => s,
            ImgError::Truncated => "The file is cut short.",
            ImgError::Corrupt(s) => s,
            ImgError::Unsupported(s) => s,
            ImgError::TooBig => "The image is too big to open (over 8 megapixels).",
        }
    }
}

/// A decoded picture: `w` x `h` pixels, row by row, `0xAARRGGBB` (straight alpha).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

impl Image {
    /// A transparent black `w` x `h` image, or `TooBig`.
    pub fn new(w: usize, h: usize) -> Result<Image, ImgError> {
        if w == 0 || h == 0 {
            return Err(ImgError::Corrupt("The image has no size."));
        }
        match w.checked_mul(h) {
            Some(n) if n <= MAX_PIXELS => Ok(Image {
                w,
                h,
                px: alloc::vec![0; n],
            }),
            _ => Err(ImgError::TooBig),
        }
    }

    /// The same picture at `w` x `h`: averaged when it gets smaller, nearest pixel when it gets
    /// bigger (so zooming in shows the pixels).
    pub fn scaled(&self, w: usize, h: usize) -> Result<Image, ImgError> {
        scale::scaled(self, w, h)
    }

    /// Turned a quarter clockwise (`cw`) or anticlockwise.
    pub fn rotated(&self, cw: bool) -> Image {
        let (w, h) = (self.w, self.h);
        let mut px = alloc::vec![0u32; w * h];
        for y in 0..h {
            for x in 0..w {
                // The new image is h wide and w tall.
                let (nx, ny) = if cw { (h - 1 - y, x) } else { (y, w - 1 - x) };
                px[ny * h + nx] = self.px[y * w + x];
            }
        }
        Image { w: h, h: w, px }
    }

    /// The picture as a PNG file (8-bit RGB: transparent areas are flattened onto white).
    pub fn to_png(&self) -> Vec<u8> {
        let flat: Vec<u32> = self
            .px
            .iter()
            .map(|&p| Image::flatten_pixel(p, 0xFF_FFFF))
            .collect();
        kernel_png::encode(&flat, self.w, self.h, self.w)
    }

    /// The largest size that fits `max_w` x `max_h` with the same shape (never below 1 x 1).
    pub fn fit_size(&self, max_w: usize, max_h: usize) -> (usize, usize) {
        let (w, h) = (self.w as u64, self.h as u64);
        let (mw, mh) = (max_w.max(1) as u64, max_h.max(1) as u64);
        // Compare w/h with mw/mh without division.
        if w * mh >= h * mw {
            (mw as usize, ((h * mw / w) as usize).max(1))
        } else {
            (((w * mh / h) as usize).max(1), mh as usize)
        }
    }

    /// `px` over `background` (straight alpha to opaque), for drawing on a window.
    pub fn flatten_pixel(px: u32, background: u32) -> u32 {
        let a = px >> 24;
        if a == 255 {
            return px & 0x00FF_FFFF;
        }
        let ch = |s: u32| {
            let (x, d) = ((px >> s) & 0xFF, (background >> s) & 0xFF);
            ((x * a + d * (255 - a) + 127) / 255) << s
        };
        ch(16) | ch(8) | ch(0)
    }
}

/// What a file looks like, from its first bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Png,
    Pnm,
    Bmp,
    Pdf,
    Jpeg,
    Gif,
    WebP,
    Unknown,
}

impl Format {
    pub fn name(&self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Pnm => "PNM",
            Format::Bmp => "BMP",
            Format::Pdf => "PDF",
            Format::Jpeg => "JPEG",
            Format::Gif => "GIF",
            Format::WebP => "WebP",
            Format::Unknown => "unknown",
        }
    }
}

/// What [`probe`] can tell from the headers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
    pub format: Format,
    /// The size it is shown at (a JPEG's EXIF orientation applied).
    pub w: usize,
    pub h: usize,
    /// More than one frame (GIF, animated WebP).
    pub animated: bool,
}

pub fn sniff(b: &[u8]) -> Format {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Format::Png
    } else if b.len() >= 2 && b[0] == b'P' && (b'1'..=b'6').contains(&b[1]) {
        Format::Pnm
    } else if b.starts_with(b"BM") {
        Format::Bmp
    } else if b.starts_with(b"%PDF-") {
        Format::Pdf
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Format::Jpeg
    } else if b.starts_with(b"GIF8") {
        Format::Gif
    } else if b.len() >= 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        Format::WebP
    } else {
        Format::Unknown
    }
}

/// Decode a file by what it is.
pub fn decode(b: &[u8]) -> Result<Image, ImgError> {
    match sniff(b) {
        Format::Png => png::decode(b),
        Format::Pnm => pnm::decode(b),
        Format::Bmp => bmp::decode(b),
        Format::Pdf => Err(ImgError::Unrecognised(
            "This is a PDF. Preview can't show PDFs yet.",
        )),
        Format::Jpeg => jpeg::decode(b),
        Format::Gif => gif::decode(b),
        Format::WebP => webp::decode(b),
        Format::Unknown => Err(ImgError::Unrecognised(
            "This isn't a picture Preview knows (PNG, JPEG, GIF, WebP, PPM and BMP work).",
        )),
    }
}

/// Size and kind from the headers, without decoding the pixels.
pub fn probe(b: &[u8]) -> Result<Info, ImgError> {
    let format = sniff(b);
    let (w, h, animated) = match format {
        Format::Png => {
            if b.len() < 24 || &b[12..16] != b"IHDR" {
                return Err(ImgError::Truncated);
            }
            let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize;
            (be(16), be(20), false)
        }
        Format::Jpeg => {
            let (w, h) = jpeg::probe(b)?;
            (w, h, false)
        }
        Format::Gif => {
            let g = gif::Gif::parse(b)?;
            (g.w, g.h, g.frames.len() > 1)
        }
        Format::WebP => {
            let p = webp::WebP::parse(b)?;
            (p.w, p.h, p.frames.len() > 1)
        }
        Format::Bmp | Format::Pnm => {
            let i = decode(b)?;
            (i.w, i.h, false)
        }
        Format::Pdf | Format::Unknown => {
            return Err(decode(b).err().unwrap_or(ImgError::Truncated));
        }
    };
    if w == 0 || h == 0 {
        return Err(ImgError::Corrupt("The image has no size."));
    }
    Ok(Info {
        format,
        w,
        h,
        animated,
    })
}

pub(crate) fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Scale a `bits`-bit sample (up to 16) to 0..=255.
pub(crate) fn to8(v: u32, bits: u32) -> u8 {
    match bits {
        8 => v as u8,
        16 => (v >> 8) as u8,
        _ => (v * 255 / ((1u32 << bits) - 1)) as u8,
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_media;
