//! Image decoders for Preview: PNG, PPM/PGM/PBM (P1 to P6) and BMP, and scaling for zoom. Pixels
//! come out as straight-alpha `0xAARRGGBB`. No unsafe, no dependencies; the inflate is in
//! [`inflate`]. Everything that comes from a file is checked: sizes before allocating, lengths
//! against the data, checksums where the format has them, so a damaged or hostile file gives an
//! [`ImgError`], never a panic or an unbounded allocation.
//!
//! Not supported (and said so, not silently wrong): JPEG, GIF, PDF and the like (unrecognised),
//! RLE-compressed BMP, PNG colour management (gamma and ICC profiles are ignored).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

pub mod bmp;
pub mod inflate;
pub mod png;
pub mod pnm;
mod scale;

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
    Unknown,
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
        Format::Jpeg => Err(ImgError::Unrecognised("JPEG pictures can't be opened yet.")),
        Format::Gif => Err(ImgError::Unrecognised("GIF pictures can't be opened yet.")),
        Format::Unknown => Err(ImgError::Unrecognised(
            "This isn't a picture Preview knows (PNG, PPM and BMP work).",
        )),
    }
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
