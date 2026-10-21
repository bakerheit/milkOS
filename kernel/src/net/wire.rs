//! Wire-format helpers: addresses, big-endian field access, the Internet checksum.

use core::fmt;

/// An IPv4 address, stored in network byte order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ipv4(pub [u8; 4]);

impl Ipv4 {
    pub const UNSPECIFIED: Ipv4 = Ipv4([0, 0, 0, 0]);
    pub const BROADCAST: Ipv4 = Ipv4([255, 255, 255, 255]);
    pub const LOCALHOST: Ipv4 = Ipv4([127, 0, 0, 1]);

    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Ipv4 {
        Ipv4([a, b, c, d])
    }
    /// As a number (10.0.2.15 -> 0x0a00020f).
    pub const fn to_u32(self) -> u32 {
        u32::from_be_bytes(self.0)
    }
    pub const fn from_u32(v: u32) -> Ipv4 {
        Ipv4(v.to_be_bytes())
    }
    pub fn is_unspecified(self) -> bool {
        self.0 == [0; 4]
    }
    pub fn is_broadcast(self) -> bool {
        self.0 == [255; 4]
    }
    pub fn is_loopback(self) -> bool {
        self.0[0] == 127
    }
    pub fn is_multicast(self) -> bool {
        self.0[0] & 0xf0 == 0xe0
    }
    /// Same network under `mask`.
    pub fn same_net(self, other: Ipv4, mask: Ipv4) -> bool {
        self.to_u32() & mask.to_u32() == other.to_u32() & mask.to_u32()
    }
    /// The u32 Linux prints in `/proc/net/*`: the network-order bytes read as a
    /// little-endian word (10.0.2.2 -> 0202000A).
    pub fn proc_hex(self) -> u32 {
        u32::from_le_bytes(self.0)
    }
    /// Parse dotted-quad text.
    pub fn parse(s: &str) -> Option<Ipv4> {
        let mut out = [0u8; 4];
        let mut it = s.split('.');
        for b in out.iter_mut() {
            let part = it.next()?;
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            *b = part.parse().ok()?;
        }
        if it.next().is_some() {
            return None;
        }
        Some(Ipv4(out))
    }
}

impl fmt::Display for Ipv4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

impl fmt::Debug for Ipv4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// An IPv4 address + port (`struct sockaddr_in` minus the family).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct SockAddr {
    pub ip: Ipv4,
    pub port: u16,
}

impl SockAddr {
    pub const fn new(ip: Ipv4, port: u16) -> SockAddr {
        SockAddr { ip, port }
    }
    pub const ANY: SockAddr = SockAddr {
        ip: Ipv4::UNSPECIFIED,
        port: 0,
    };
}

impl fmt::Display for SockAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.ip, self.port)
    }
}

impl fmt::Debug for SockAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// An Ethernet MAC address.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Mac(pub [u8; 6]);

impl Mac {
    pub const BROADCAST: Mac = Mac([0xff; 6]);
    pub const ZERO: Mac = Mac([0; 6]);
}

impl fmt::Display for Mac {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            m[0], m[1], m[2], m[3], m[4], m[5]
        )
    }
}

// ---- field access ----------------------------------------------------------------------

#[inline]
pub fn be16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}
#[inline]
pub fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}
#[inline]
pub fn put16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_be_bytes());
}
#[inline]
pub fn put32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_be_bytes());
}
#[inline]
pub fn ip_at(b: &[u8], off: usize) -> Ipv4 {
    Ipv4([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

// ---- Internet checksum (RFC 1071) ------------------------------------------------------

/// Add `data` to a running ones-complement sum (not yet folded).
pub fn sum(data: &[u8], mut acc: u32) -> u32 {
    let (pairs, rest) = data.as_chunks::<2>();
    for c in pairs {
        acc += u16::from_be_bytes(*c) as u32;
    }
    if let [last] = rest {
        acc += (*last as u32) << 8;
    }
    // Fold early so long buffers cannot overflow.
    (acc & 0xffff) + (acc >> 16)
}

/// Fold and complement a running sum into the checksum field value.
pub fn finish(mut acc: u32) -> u16 {
    while acc > 0xffff {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    !(acc as u16)
}

pub fn checksum(data: &[u8]) -> u16 {
    finish(sum(data, 0))
}

/// Running sum of the TCP/UDP pseudo-header.
pub fn pseudo_sum(src: Ipv4, dst: Ipv4, proto: u8, len: usize) -> u32 {
    let mut acc = sum(&src.0, 0);
    acc = sum(&dst.0, acc);
    acc + proto as u32 + len as u32
}

/// `core::fmt::Write` into a fixed byte buffer; output past the end is cut off silently.
/// For formatting without the heap (IRQ context).
pub struct BufWriter<'a> {
    buf: &'a mut [u8],
    pub len: usize,
}

impl<'a> BufWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        BufWriter { buf, len: 0 }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl fmt::Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}
