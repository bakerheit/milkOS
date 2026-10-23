//! Reading and writing TLS's wire encoding (RFC 8446 3): big-endian integers and vectors
//! with 1-, 2- or 3-byte length prefixes. [`Reader`] never panics: every read is checked and
//! returns None when the input is short.

use alloc::vec::Vec;

pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let s = self.buf.get(self.pos..).unwrap_or(&[]);
        self.pos = self.buf.len();
        s
    }

    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u24(&mut self) -> Option<u32> {
        let b = self.take(3)?;
        Some(u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }

    pub fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// `opaque x<0..2^8-1>`.
    pub fn vec8(&mut self) -> Option<&'a [u8]> {
        let n = self.u8()? as usize;
        self.take(n)
    }

    /// `opaque x<0..2^16-1>`.
    pub fn vec16(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }

    /// `opaque x<0..2^24-1>`.
    pub fn vec24(&mut self) -> Option<&'a [u8]> {
        let n = self.u24()? as usize;
        self.take(n)
    }
}

pub fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}

pub fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn put_u24(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes()[1..]);
}

/// Write `f`'s output with a length prefix of `width` bytes (1, 2 or 3) in front.
pub fn with_len(out: &mut Vec<u8>, width: usize, f: impl FnOnce(&mut Vec<u8>)) {
    let at = out.len();
    out.extend_from_slice(&[0, 0, 0][..width]);
    f(out);
    let n = out.len() - at - width;
    let be = (n as u32).to_be_bytes();
    out[at..at + width].copy_from_slice(&be[4 - width..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_never_reads_past_the_end() {
        let mut r = Reader::new(&[0x00, 0x03, 1, 2]);
        assert_eq!(r.vec16(), None);
        let mut r = Reader::new(&[0x00, 0x02, 1, 2, 9]);
        assert_eq!(r.vec16(), Some(&[1u8, 2][..]));
        assert_eq!(r.u16(), None);
        assert_eq!(r.u8(), Some(9));
        assert!(r.is_empty());
        assert_eq!(r.take(usize::MAX), None);
        let mut r = Reader::new(&[0xff, 0xff, 0xff]);
        assert_eq!(r.vec24(), None);
    }

    #[test]
    fn with_len_prefixes() {
        let mut v = Vec::new();
        with_len(&mut v, 3, |o| {
            with_len(o, 1, |o| o.extend_from_slice(b"ab"));
        });
        assert_eq!(v, [0, 0, 3, 2, b'a', b'b']);
    }
}
