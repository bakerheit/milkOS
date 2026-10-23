//! Compressed HTTP bodies: gzip (RFC 1952, every member checked against its CRC-32 and
//! length) and "deflate" (RFC 1950 zlib as the standard says; a raw RFC 1951 stream as some
//! servers send). The output is capped, so a small hostile body can't inflate without limit.

use alloc::vec::Vec;

use crate::inflate::{self, InflateError};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GzipError {
    /// Not gzip (bad magic, method or flags).
    NotGzip,
    Truncated,
    Corrupt(&'static str),
    /// CRC-32 or ISIZE didn't match.
    Checksum,
    /// The result would be bigger than the limit.
    TooBig,
}

impl GzipError {
    pub fn message(self) -> &'static str {
        match self {
            GzipError::NotGzip => "the gzip body isn't gzip",
            GzipError::Truncated => "the compressed body is cut off",
            GzipError::Corrupt(m) => m,
            GzipError::Checksum => "the compressed body's checksum is wrong",
            GzipError::TooBig => "the page is too big once uncompressed",
        }
    }
}

impl From<InflateError> for GzipError {
    fn from(e: InflateError) -> GzipError {
        match e {
            InflateError::Truncated => GzipError::Truncated,
            InflateError::Corrupt(m) => GzipError::Corrupt(m),
            InflateError::TooBig => GzipError::TooBig,
        }
    }
}

const FHCRC: u8 = 2;
const FEXTRA: u8 = 4;
const FNAME: u8 = 8;
const FCOMMENT: u8 = 16;

/// One gzip member at `data`: the data and the bytes it used.
fn member(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), GzipError> {
    if data.len() < 10 {
        return Err(GzipError::Truncated);
    }
    if data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 || data[3] & 0xe0 != 0 {
        return Err(GzipError::NotGzip);
    }
    let flg = data[3];
    let mut k = 10;
    if flg & FEXTRA != 0 {
        let x = data.get(k..k + 2).ok_or(GzipError::Truncated)?;
        k += 2 + u16::from_le_bytes([x[0], x[1]]) as usize;
    }
    for f in [FNAME, FCOMMENT] {
        if flg & f != 0 {
            let z = data
                .get(k..)
                .and_then(|r| r.iter().position(|&b| b == 0))
                .ok_or(GzipError::Truncated)?;
            k += z + 1;
        }
    }
    if flg & FHCRC != 0 {
        k += 2;
    }
    let body = data.get(k..).ok_or(GzipError::Truncated)?;
    let (out, used) = inflate::inflate(body, limit)?;
    let t = k + used;
    let tail = data.get(t..t + 8).ok_or(GzipError::Truncated)?;
    let crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    let isize = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
    if crc != inflate::crc32(&[&out]) || isize != out.len() as u32 {
        return Err(GzipError::Checksum);
    }
    Ok((out, t + 8))
}

/// Decompress a gzip body (all its members, as `cat a.gz b.gz` makes), at most `limit` bytes.
pub fn gunzip(data: &[u8], limit: usize) -> Result<Vec<u8>, GzipError> {
    let (mut out, mut k) = member(data, limit)?;
    while data.len() >= k + 2 && data[k] == 0x1f && data[k + 1] == 0x8b {
        let (more, used) = member(&data[k..], limit - out.len())?;
        out.extend_from_slice(&more);
        k += used;
    }
    Ok(out)
}

/// HTTP `deflate`: zlib, or failing that, a raw deflate stream.
pub fn inflate_http(data: &[u8], limit: usize) -> Result<Vec<u8>, GzipError> {
    match inflate::zlib_decompress(data, limit) {
        Ok(v) => Ok(v),
        Err(InflateError::TooBig) => Err(GzipError::TooBig),
        Err(_) => Ok(inflate::inflate(data, limit)?.0),
    }
}
