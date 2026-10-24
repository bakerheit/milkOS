//! Compressed HTTP bodies: gzip (RFC 1952, every member checked against its CRC-32 and
//! length) and "deflate" (RFC 1950 zlib as the standard says; a raw RFC 1951 stream as some
//! servers send). The output is capped, so a small hostile body can't inflate without limit.
//!
//! [`gunzip`] and [`inflate_http`] take the whole body. [`StreamDecoder`] decodes the same codings
//! (and none) as the body arrives, in pieces cut anywhere, in constant memory: the web client
//! uses it when the Browser wants a page while it is still coming.

use alloc::vec::Vec;

use crate::inflate::{self, InflateError, Inflater, Pump};

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


// ---- decoding as it arrives ------------------------------------------------------------------

enum Coding {
    /// No coding: the bytes are the body.
    Plain,
    /// Reading a gzip member's header (bytes are held until it is whole).
    GzHead,
    /// Inside a gzip member: the CRC-32 and the length so far.
    GzBody { crc: u32, len: u32 },
    /// After the deflate data: the trailer's eight bytes.
    GzTail { crc: u32, len: u32 },
    /// The first two bytes decide: a zlib header, or a raw stream.
    DeflateStart,
    ZlibBody { adler: u32 },
    ZlibTail { adler: u32 },
    RawBody,
    /// The stream ended and was checked (another gzip member may still follow).
    Over,
}

/// A response body decoded piece by piece. See the module comment.
pub struct StreamDecoder {
    coding: Coding,
    inf: Inflater,
    /// Bytes given and not yet used: a header or trailer waiting to be whole.
    pend: Vec<u8>,
    /// Decoded bytes in all, and the most there may be.
    total: usize,
    limit: usize,
    members: u32,
}

/// The longest a gzip header is looked for (its optional name and comment included).
const MAX_GZIP_HEAD: usize = 4096;

impl StreamDecoder {
    /// `encoding`: the Content-Encoding header's value ("" for none). `limit`: the most decoded
    /// bytes there may be in all (more is [`GzipError::TooBig`]). The caller refuses codings it
    /// does not know before this (here they count as none).
    pub fn new(encoding: &str, limit: usize) -> StreamDecoder {
        let coding = match encoding.trim().to_ascii_lowercase().as_str() {
            "gzip" | "x-gzip" => Coding::GzHead,
            "deflate" => Coding::DeflateStart,
            _ => Coding::Plain,
        };
        StreamDecoder {
            coding,
            inf: Inflater::new(limit),
            pend: Vec::new(),
            total: 0,
            limit,
            members: 0,
        }
    }

    /// Decoded bytes so far.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Give `data` (the next bytes of the body); every decoded piece goes to `sink`, which says
    /// whether to go on (false stops: the result is Ok(false)). An error is final.
    pub fn feed(&mut self, data: &[u8], sink: &mut dyn FnMut(&[u8]) -> bool) -> Result<bool, GzipError> {
        if matches!(self.coding, Coding::Plain) {
            if self.total + data.len() > self.limit {
                return Err(GzipError::TooBig);
            }
            self.total += data.len();
            return Ok(data.is_empty() || sink(data));
        }
        self.pend.extend_from_slice(data);
        loop {
            match self.coding {
                Coding::Plain => return Ok(true),
                Coding::Over => {
                    // Another gzip member may follow; anything else after the end is ignored.
                    if self.pend.len() >= 2 && self.pend[0] == 0x1f && self.pend[1] == 0x8b && self.members > 0 {
                        self.coding = Coding::GzHead;
                        continue;
                    }
                    if self.pend.len() < 2 && self.members > 0 {
                        return Ok(true);
                    }
                    self.pend.clear();
                    return Ok(true);
                }
                Coding::GzHead => match gzip_head(&self.pend) {
                    Head::NeedMore => {
                        if self.pend.len() > MAX_GZIP_HEAD {
                            return Err(GzipError::Corrupt("the gzip header is too long"));
                        }
                        return Ok(true);
                    }
                    Head::Bad(e) => return Err(e),
                    Head::Ok(n) => {
                        self.pend.drain(..n);
                        self.inf = Inflater::new(self.limit.saturating_sub(self.total));
                        self.coding = Coding::GzBody { crc: 0, len: 0 };
                        self.members += 1;
                    }
                },
                Coding::DeflateStart => {
                    if self.pend.len() < 2 {
                        return Ok(true);
                    }
                    let (cmf, flg) = (self.pend[0], self.pend[1]);
                    let zlib = cmf & 0x0F == 8 && cmf >> 4 <= 7 && (cmf as u32 * 256 + flg as u32) % 31 == 0;
                    if zlib {
                        if flg & 0x20 != 0 {
                            return Err(GzipError::Corrupt("zlib preset dictionary"));
                        }
                        self.pend.drain(..2);
                        self.coding = Coding::ZlibBody { adler: 1 };
                    } else {
                        self.coding = Coding::RawBody;
                    }
                    self.inf = Inflater::new(self.limit);
                }
                Coding::GzBody { .. } | Coding::ZlibBody { .. } | Coding::RawBody => {
                    let input = core::mem::take(&mut self.pend);
                    self.inf.feed(&input);
                    let mut out: Vec<u8> = Vec::new();
                    loop {
                        out.clear();
                        let st = self.inf.pump(&mut out, 64 * 1024)?;
                        if !out.is_empty() {
                            self.total += out.len();
                            match &mut self.coding {
                                Coding::GzBody { crc, len } => {
                                    *crc = inflate::crc32_update(*crc, &out);
                                    *len = len.wrapping_add(out.len() as u32);
                                }
                                Coding::ZlibBody { adler } => *adler = inflate::adler32_update(*adler, &out),
                                _ => {}
                            }
                            if !sink(&out) {
                                return Ok(false);
                            }
                        }
                        match st {
                            Pump::More => continue,
                            Pump::NeedInput => return Ok(true),
                            Pump::Done => break,
                        }
                    }
                    // The deflate data is over: what follows is the trailer.
                    self.pend = self.inf.rest().to_vec();
                    self.coding = match self.coding {
                        Coding::GzBody { crc, len } => Coding::GzTail { crc, len },
                        Coding::ZlibBody { adler } => Coding::ZlibTail { adler },
                        _ => Coding::Over,
                    };
                }
                Coding::GzTail { crc, len } => {
                    if self.pend.len() < 8 {
                        return Ok(true);
                    }
                    let b = &self.pend;
                    let crc_got = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    let len_got = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
                    if crc != crc_got || len != len_got {
                        return Err(GzipError::Checksum);
                    }
                    self.pend.drain(..8);
                    self.coding = Coding::Over;
                }
                Coding::ZlibTail { adler } => {
                    if self.pend.len() < 4 {
                        return Ok(true);
                    }
                    let b = &self.pend;
                    if u32::from_be_bytes([b[0], b[1], b[2], b[3]]) != adler {
                        return Err(GzipError::Corrupt("checksum mismatch"));
                    }
                    self.pend.drain(..4);
                    self.coding = Coding::Over;
                }
            }
        }
    }

    /// The body ended: was the stream whole? (A cut-off one is [`GzipError::Truncated`].)
    pub fn finish(&self) -> Result<(), GzipError> {
        match self.coding {
            Coding::Plain => Ok(()),
            // (Whatever followed a finished stream was not another member.)
            Coding::Over => Ok(()),
            // (No input at all: an empty body.)
            Coding::DeflateStart | Coding::GzHead if self.pend.is_empty() && self.members == 0 && self.total == 0 => Ok(()),
            // A raw deflate stream that ended without its last block is cut off.
            _ => Err(GzipError::Truncated),
        }
    }
}

enum Head {
    NeedMore,
    Bad(GzipError),
    /// A header of this many bytes.
    Ok(usize),
}

/// A gzip member's header at the start of `data`.
fn gzip_head(data: &[u8]) -> Head {
    if data.len() < 10 {
        return if data.len() >= 2 && (data[0] != 0x1f || data[1] != 0x8b) {
            Head::Bad(GzipError::NotGzip)
        } else {
            Head::NeedMore
        };
    }
    if data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 || data[3] & 0xe0 != 0 {
        return Head::Bad(GzipError::NotGzip);
    }
    let flg = data[3];
    let mut k = 10;
    if flg & FEXTRA != 0 {
        let Some(x) = data.get(k..k + 2) else { return Head::NeedMore };
        k += 2 + u16::from_le_bytes([x[0], x[1]]) as usize;
        if k > data.len() {
            return Head::NeedMore;
        }
    }
    for f in [FNAME, FCOMMENT] {
        if flg & f != 0 {
            match data.get(k..).and_then(|r| r.iter().position(|&b| b == 0)) {
                Some(z) => k += z + 1,
                None => return Head::NeedMore,
            }
        }
    }
    if flg & FHCRC != 0 {
        k += 2;
    }
    if k > data.len() {
        return Head::NeedMore;
    }
    Head::Ok(k)
}
