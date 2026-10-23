//! A strict DER reader (X.690) for certificates: definite, minimally encoded lengths,
//! single-byte tags, and helpers for the few universal types X.509 uses. Every function
//! returns None on malformed input; nothing here can panic.

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// Context-specific constructed tag `[n]` (EXPLICIT, or IMPLICIT of a constructed type).
pub const fn ctx_cons(n: u8) -> u8 {
    0xa0 | n
}

/// Context-specific primitive tag `[n]` (IMPLICIT of a primitive type).
pub const fn ctx_prim(n: u8) -> u8 {
    0x80 | n
}

/// Largest object we accept (certificates are a few KiB; this bounds the arithmetic).
const MAX_LEN: usize = 1 << 24;

#[derive(Clone)]
pub struct Der<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Der<'a> {
    pub fn new(buf: &'a [u8]) -> Der<'a> {
        Der { buf, pos: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// The next TLV: (tag, contents, the whole encoding).
    pub fn any(&mut self) -> Option<(u8, &'a [u8], &'a [u8])> {
        let start = self.pos;
        let tag = *self.buf.get(start)?;
        if tag & 0x1f == 0x1f {
            return None; // high tag numbers: not used by X.509
        }
        let first = *self.buf.get(start + 1)? as usize;
        let (len, hdr) = if first < 0x80 {
            (first, 2)
        } else {
            let n = first & 0x7f;
            if n == 0 || n > 3 {
                return None; // indefinite length, or absurdly long
            }
            let bytes = self.buf.get(start + 2..start + 2 + n)?;
            if bytes[0] == 0 {
                return None; // not minimal
            }
            let mut len = 0usize;
            for &b in bytes {
                len = (len << 8) | b as usize;
            }
            if len < 0x80 {
                return None; // the short form would do
            }
            (len, 2 + n)
        };
        if len > MAX_LEN {
            return None;
        }
        let end = start.checked_add(hdr)?.checked_add(len)?;
        let value = self.buf.get(start + hdr..end)?;
        let whole = self.buf.get(start..end)?;
        self.pos = end;
        Some((tag, value, whole))
    }

    /// The contents of the next TLV, which must have tag `tag`.
    pub fn read(&mut self, tag: u8) -> Option<&'a [u8]> {
        let save = self.pos;
        match self.any() {
            Some((t, v, _)) if t == tag => Some(v),
            _ => {
                self.pos = save;
                None
            }
        }
    }

    /// The whole encoding of the next TLV, which must have tag `tag`.
    pub fn read_tlv(&mut self, tag: u8) -> Option<&'a [u8]> {
        let save = self.pos;
        match self.any() {
            Some((t, _, w)) if t == tag => Some(w),
            _ => {
                self.pos = save;
                None
            }
        }
    }

    /// The next TLV if it has tag `tag`, else nothing consumed. Outer None = malformed.
    pub fn optional(&mut self, tag: u8) -> Option<Option<&'a [u8]>> {
        if self.peek_tag() != Some(tag) {
            return Some(None);
        }
        Some(Some(self.read(tag)?))
    }

    /// A nested reader over the contents of a SEQUENCE.
    pub fn sequence(&mut self) -> Option<Der<'a>> {
        Some(Der::new(self.read(SEQUENCE)?))
    }

    /// Succeeds only if everything was read.
    pub fn end(&self) -> Option<()> {
        self.is_empty().then_some(())
    }
}

/// An INTEGER's magnitude, without the sign-padding zero. None if negative or not minimal.
pub fn positive_integer(v: &[u8]) -> Option<&[u8]> {
    match v {
        [] => None,
        [0] => Some(v),
        [0, b, ..] if *b < 0x80 => None,
        [0, rest @ ..] => Some(rest),
        [b, ..] if *b >= 0x80 => None,
        _ => Some(v),
    }
}

/// A small non-negative INTEGER (path lengths, versions, salt lengths).
pub fn small_uint(v: &[u8]) -> Option<u64> {
    let m = positive_integer(v)?;
    if m.len() > 8 {
        return None;
    }
    Some(m.iter().fold(0u64, |a, &b| (a << 8) | b as u64))
}

pub fn boolean(v: &[u8]) -> Option<bool> {
    match v {
        [0x00] => Some(false),
        [0xff] => Some(true),
        _ => None,
    }
}

/// A BIT STRING holding whole bytes (keys, signatures): the unused-bits count must be 0.
pub fn bit_string_bytes(v: &[u8]) -> Option<&[u8]> {
    match v {
        [0, rest @ ..] => Some(rest),
        _ => None,
    }
}

/// A BIT STRING as named bits (KeyUsage): bit 0 is the most significant bit of the first
/// byte. Returns the first 16 bits, bit i at (1 << i).
pub fn bit_string_flags(v: &[u8]) -> Option<u16> {
    let (&unused, bytes) = v.split_first()?;
    if unused > 7 || (bytes.is_empty() && unused != 0) {
        return None;
    }
    if let Some(&last) = bytes.last()
        && last & ((1u16 << unused) - 1) as u8 != 0
    {
        return None; // the unused bits must be zero
    }
    let mut flags = 0u16;
    for (i, &b) in bytes.iter().take(2).enumerate() {
        for bit in 0..8 {
            if b & (0x80 >> bit) != 0 {
                flags |= 1 << (i * 8 + bit);
            }
        }
    }
    Some(flags)
}

/// UTCTime (YYMMDDHHMMSSZ) or GeneralizedTime (YYYYMMDDHHMMSSZ) as Unix seconds, in the
/// strict forms RFC 5280 4.1.2.5 requires.
pub fn time(tag: u8, v: &[u8]) -> Option<i64> {
    let digits = |s: &[u8]| -> Option<u32> {
        let mut n = 0u32;
        for &c in s {
            if !c.is_ascii_digit() {
                return None;
            }
            n = n * 10 + (c - b'0') as u32;
        }
        Some(n)
    };
    let (year, rest) = match (tag, v.len()) {
        (UTC_TIME, 13) => {
            let yy = digits(&v[..2])?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &v[2..])
        }
        (GENERALIZED_TIME, 15) => (digits(&v[..4])?, &v[4..]),
        _ => return None,
    };
    if rest.len() != 11 || rest[10] != b'Z' {
        return None;
    }
    let month = digits(&rest[0..2])?;
    let day = digits(&rest[2..4])?;
    let hour = digits(&rest[4..6])?;
    let min = digits(&rest[6..8])?;
    let sec = digits(&rest[8..10])?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let days = days_from_civil(year as i64, month, day);
    Some(days * 86400 + (hour * 3600 + min * 60 + sec) as i64)
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_strict() {
        assert!(Der::new(&[0x04, 0x01, 0xaa]).any().is_some());
        assert!(Der::new(&[0x04, 0x81, 0x01, 0xaa]).any().is_none()); // long form for 1
        assert!(Der::new(&[0x04, 0x82, 0x00, 0x81]).any().is_none()); // leading zero
        assert!(Der::new(&[0x30, 0x80, 0, 0]).any().is_none()); // indefinite
        assert!(Der::new(&[0x04, 0x02, 0xaa]).any().is_none()); // short
        assert!(Der::new(&[0x1f, 0x01, 0x00]).any().is_none()); // high tag
        assert!(Der::new(&[0x04, 0x84, 0xff, 0xff, 0xff, 0xff]).any().is_none());
        let mut long = alloc::vec![0x04, 0x81, 0x80];
        long.extend_from_slice(&[7; 0x80]);
        assert_eq!(Der::new(&long).any().unwrap().1.len(), 0x80);
    }

    #[test]
    fn integers() {
        assert_eq!(positive_integer(&[0x00]), Some(&[0u8][..]));
        assert_eq!(positive_integer(&[0x00, 0x80]), Some(&[0x80u8][..]));
        assert_eq!(positive_integer(&[0x00, 0x7f]), None);
        assert_eq!(positive_integer(&[0x80]), None);
        assert_eq!(positive_integer(&[]), None);
        assert_eq!(small_uint(&[0x01, 0x00]), Some(256));
        assert_eq!(small_uint(&[0x01; 9]), None);
    }

    #[test]
    fn times() {
        assert_eq!(time(UTC_TIME, b"700101000000Z"), Some(0));
        assert_eq!(time(UTC_TIME, b"491231235959Z"), Some(2524607999));
        assert_eq!(time(GENERALIZED_TIME, b"20500101000000Z"), Some(2524608000));
        assert_eq!(time(UTC_TIME, b"240229120000Z"), Some(1709208000));
        assert_eq!(time(UTC_TIME, b"230229120000Z"), None);
        assert_eq!(time(UTC_TIME, b"231301000000Z"), None);
        assert_eq!(time(UTC_TIME, b"230101000000"), None);
        assert_eq!(time(UTC_TIME, b"2301010000Z"), None);
        assert_eq!(time(UTC_TIME, b"230101000000+0100"), None);
        assert_eq!(time(GENERALIZED_TIME, b"20230101000000.5Z"), None);
        assert_eq!(time(UTC_TIME, b"23010100006aZ"), None);
        assert_eq!(time(GENERALIZED_TIME, b"99991231235959Z"), Some(253402300799));
    }

    #[test]
    fn key_usage_bits() {
        // digitalSignature (bit 0) and keyCertSign (bit 5): 0b1000_0100, 2 unused bits.
        assert_eq!(bit_string_flags(&[2, 0x84]), Some(1 | (1 << 5)));
        assert_eq!(bit_string_flags(&[2, 0x85]), None);
        assert_eq!(bit_string_flags(&[8, 0x00]), None);
        assert_eq!(bit_string_flags(&[0]), Some(0));
        assert_eq!(bit_string_flags(&[7, 0x80, 0x80]), Some(1 | (1 << 8)));
    }
}
