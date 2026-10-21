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

/// An IPv6 address, network byte order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ipv6(pub [u8; 16]);

impl Ipv6 {
    pub const UNSPECIFIED: Ipv6 = Ipv6([0; 16]);
    pub const LOOPBACK: Ipv6 = Ipv6([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    /// ff02::1 (all nodes on the link), ff02::2 (all routers), ff02::fb (mDNS).
    pub const ALL_NODES: Ipv6 = Ipv6([0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    pub const ALL_ROUTERS: Ipv6 = Ipv6([0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
    pub const MDNS: Ipv6 = Ipv6([0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfb]);

    pub fn is_unspecified(self) -> bool {
        self.0 == [0; 16]
    }
    pub fn is_loopback(self) -> bool {
        self == Ipv6::LOOPBACK
    }
    pub fn is_multicast(self) -> bool {
        self.0[0] == 0xff
    }
    /// fe80::/10.
    pub fn is_link_local(self) -> bool {
        self.0[0] == 0xfe && self.0[1] & 0xc0 == 0x80
    }
    /// ff02::/16: link-scope multicast.
    pub fn is_link_multicast(self) -> bool {
        self.0[0] == 0xff && self.0[1] & 0x0f == 2
    }
    /// An IPv4-mapped address (::ffff:a.b.c.d): the IPv4 address.
    pub fn to_v4(self) -> Option<Ipv4> {
        (self.0[..10] == [0; 10] && self.0[10] == 0xff && self.0[11] == 0xff)
            .then(|| Ipv4([self.0[12], self.0[13], self.0[14], self.0[15]]))
    }
    pub fn mapped(v4: Ipv4) -> Ipv6 {
        let mut a = [0u8; 16];
        a[10] = 0xff;
        a[11] = 0xff;
        a[12..].copy_from_slice(&v4.0);
        Ipv6(a)
    }
    /// The solicited-node multicast address of this one (ff02::1:ffXX:XXXX).
    pub fn solicited_node(self) -> Ipv6 {
        let mut a = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff, 0, 0, 0];
        a[13..].copy_from_slice(&self.0[13..]);
        Ipv6(a)
    }
    /// Ethernet address for a multicast group (RFC 2464: 33:33 + the last 32 bits).
    pub fn multicast_mac(self) -> Mac {
        Mac([0x33, 0x33, self.0[12], self.0[13], self.0[14], self.0[15]])
    }
    /// `prefix` (its first 8 bytes) + the modified EUI-64 interface id of `mac`.
    pub fn from_eui64(prefix: &[u8; 8], mac: Mac) -> Ipv6 {
        let m = mac.0;
        let mut a = [0u8; 16];
        a[..8].copy_from_slice(prefix);
        a[8..].copy_from_slice(&[m[0] ^ 2, m[1], m[2], 0xff, 0xfe, m[3], m[4], m[5]]);
        Ipv6(a)
    }
    /// The first `plen` bits are the same.
    pub fn same_prefix(self, other: Ipv6, plen: u8) -> bool {
        let full = (plen / 8) as usize;
        if self.0[..full] != other.0[..full] {
            return false;
        }
        let rest = plen % 8;
        rest == 0 || (self.0[full] ^ other.0[full]) >> (8 - rest) == 0
    }
    fn segs(self) -> [u16; 8] {
        core::array::from_fn(|i| u16::from_be_bytes([self.0[2 * i], self.0[2 * i + 1]]))
    }
    /// Parse RFC 4291 text (`fe80::1`, `fec0::2`, `::1`, `::ffff:10.0.2.2`).
    pub fn parse(s: &str) -> Option<Ipv6> {
        let (head, tail) = match s.split_once("::") {
            Some((h, t)) => (h, Some(t)),
            None => (s, None),
        };
        let mut words: alloc::vec::Vec<u16> = alloc::vec::Vec::new();
        let parse_part = |p: &str, out: &mut alloc::vec::Vec<u16>| -> Option<()> {
            if p.is_empty() {
                return Some(());
            }
            let parts: alloc::vec::Vec<&str> = p.split(':').collect();
            for (k, w) in parts.iter().enumerate() {
                if k == parts.len() - 1 && w.contains('.') {
                    let v4 = Ipv4::parse(w)?;
                    out.push(u16::from_be_bytes([v4.0[0], v4.0[1]]));
                    out.push(u16::from_be_bytes([v4.0[2], v4.0[3]]));
                } else {
                    if w.is_empty() || w.len() > 4 {
                        return None;
                    }
                    out.push(u16::from_str_radix(w, 16).ok()?);
                }
            }
            Some(())
        };
        parse_part(head, &mut words)?;
        let mut tail_words = alloc::vec::Vec::new();
        if let Some(t) = tail {
            parse_part(t, &mut tail_words)?;
            if words.len() + tail_words.len() > 7 {
                return None;
            }
            words.resize(8 - tail_words.len(), 0);
            words.extend(tail_words);
        }
        if words.len() != 8 {
            return None;
        }
        let mut a = [0u8; 16];
        for (i, w) in words.iter().enumerate() {
            a[2 * i..2 * i + 2].copy_from_slice(&w.to_be_bytes());
        }
        Some(Ipv6(a))
    }
}

impl fmt::Display for Ipv6 {
    /// RFC 5952: lowercase, leading zeros dropped, the longest run of 2+ zero words as `::`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(v4) = self.to_v4() {
            return write!(f, "::ffff:{v4}");
        }
        let w = self.segs();
        let (mut best, mut best_len, mut cur, mut cur_len) = (8, 0, 0, 0);
        for (i, &x) in w.iter().enumerate() {
            if x == 0 {
                if cur_len == 0 {
                    cur = i;
                }
                cur_len += 1;
                if cur_len > best_len {
                    best = cur;
                    best_len = cur_len;
                }
            } else {
                cur_len = 0;
            }
        }
        if best_len < 2 {
            best = 8;
        }
        let mut i = 0;
        while i < 8 {
            if i == best {
                f.write_str("::")?;
                i += best_len;
                continue;
            }
            if i > 0 && i != best + best_len {
                f.write_str(":")?;
            }
            write!(f, "{:x}", w[i])?;
            i += 1;
        }
        Ok(())
    }
}

impl fmt::Debug for Ipv6 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Running sum of the IPv6 pseudo-header (RFC 8200 8.1).
pub fn pseudo_sum6(src: Ipv6, dst: Ipv6, next: u8, len: usize) -> u32 {
    let mut acc = sum(&src.0, 0);
    acc = sum(&dst.0, acc);
    acc + (len as u32 >> 16) + (len as u32 & 0xffff) + next as u32
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
