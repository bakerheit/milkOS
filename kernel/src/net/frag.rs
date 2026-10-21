//! IP fragment reassembly, IPv4 (RFC 791) and IPv6 (RFC 8200 4.5), with limits and a
//! strict overlap policy. One table and one set of limits for both families.
//!
//! * Datagrams are keyed by (source, destination, identification, protocol) for IPv4 and
//!   (source, destination, 32-bit identification) for IPv6. At most
//!   [`MAX_DATAGRAMS`] are in progress and [`MAX_BYTES`] buffered in all; past that the
//!   oldest is dropped. Each gives up after [`TIMEOUT`] (Linux: 30 s).
//! * A bitmap of 8-byte blocks says what has arrived. **Any overlap** with data already
//!   there (including an exact duplicate) poisons the whole datagram: it is dropped, and
//!   further fragments with that key are dropped until its timeout (RFC 5722 does this for
//!   IPv6; it defeats overlap tricks that paint different bytes over a header).
//! * A fragment with MF must carry a multiple of 8 bytes; the reassembled datagram must fit
//!   in 65535 bytes; two different "last" fragments (total lengths) poison it; so does data
//!   past the end the last fragment gave.
//! * RFC 1858 (tiny fragments): for TCP a fragment at offset 8 bytes, or a first fragment
//!   too short to hold the 20-byte TCP header, poisons the datagram.
//! * IPv6 (RFC 7112, RFC 8200): the first fragment must hold the whole header chain up to
//!   the upper-layer header, here at least 8 bytes of it (20 for TCP), or the datagram is
//!   poisoned.
//! * The reassembled IPv4 packet gets the first fragment's header (and options), MF and
//!   offset cleared, the total length set and a fresh checksum; it then goes through
//!   `ip_input` like any packet. An IPv6 one is the first fragment's unfragmentable part
//!   (fragment header removed, its next header put in its place), payload length set, and
//!   goes through `ipv6::input` again.

use alloc::vec;
use alloc::vec::Vec;

use super::iface::PROTO_TCP;
use super::wire::{Ipv4, Ipv6, be16, checksum, put16};

pub const MAX_DATAGRAMS: usize = 16;
pub const MAX_BYTES: usize = 1 << 20;
pub const TIMEOUT: u64 = 30_000_000_000;
const MAX_LEN: usize = 65535;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    V4 {
        src: Ipv4,
        dst: Ipv4,
        id: u16,
        proto: u8,
    },
    V6 {
        src: Ipv6,
        dst: Ipv6,
        id: u32,
    },
}

/// One fragment, family-neutral: where its payload goes, and the header to rebuild the
/// datagram with if it is the first.
struct Frag<'a> {
    key: Key,
    off: usize,
    more: bool,
    payload: &'a [u8],
    /// IPv4: the IP header. IPv6: the unfragmentable part, next header already patched.
    hdr: &'a [u8],
    /// Upper-layer protocol, when known (always for IPv4; for IPv6 from the first one).
    proto: Option<u8>,
    v6: bool,
}

struct Datagram {
    key: Key,
    /// First fragment's IP header (offset 0), once seen.
    hdr: Option<Vec<u8>>,
    /// Payload bytes, at their offsets.
    data: Vec<u8>,
    /// Bit per 8-byte block received.
    have: Vec<u64>,
    /// Payload length, once the last fragment came.
    total: Option<usize>,
    deadline: u64,
    started: u64,
    poisoned: bool,
}

impl Datagram {
    fn bytes(&self) -> usize {
        self.data.len()
    }
    fn block(&self, b: usize) -> bool {
        self.have
            .get(b / 64)
            .is_some_and(|w| w >> (b % 64) & 1 == 1)
    }
    fn set(&mut self, b: usize) {
        if self.have.len() <= b / 64 {
            self.have.resize(b / 64 + 1, 0);
        }
        self.have[b / 64] |= 1 << (b % 64);
    }
    fn complete(&self) -> bool {
        let Some(t) = self.total else { return false };
        self.hdr.is_some() && (0..t.div_ceil(8)).all(|b| self.block(b))
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct FragStats {
    pub reqds: u64,
    pub oks: u64,
    pub fails: u64,
    pub timeouts: u64,
    pub overlaps: u64,
    pub creates: u64,
}

pub struct Reasm {
    q: Vec<Datagram>,
    /// IPv4 counters (`/proc/net/snmp`).
    pub stats: FragStats,
    /// IPv6 counters (`/proc/net/snmp6`).
    pub stats6: FragStats,
}

impl FragStats {
    fn add_delta(&mut self, after: FragStats, before: FragStats) {
        self.reqds += after.reqds - before.reqds;
        self.oks += after.oks - before.oks;
        self.fails += after.fails - before.fails;
        self.timeouts += after.timeouts - before.timeouts;
        self.overlaps += after.overlaps - before.overlaps;
        self.creates += after.creates - before.creates;
    }
}

pub enum Outcome {
    /// Kept; waiting for more.
    Pending,
    /// The whole datagram, as one IP packet.
    Done(Vec<u8>),
    /// Refused (and counted).
    Dropped,
}

impl Reasm {
    pub fn new() -> Reasm {
        Reasm {
            q: Vec::new(),
            stats: FragStats::default(),
            stats6: FragStats::default(),
        }
    }

    fn buffered(&self) -> usize {
        self.q.iter().map(|d| d.bytes()).sum()
    }

    pub fn next_deadline(&self) -> Option<u64> {
        self.q.iter().map(|d| d.deadline).min()
    }

    /// Give up on datagrams past their timeout.
    pub fn expire(&mut self, now: u64) {
        let (mut t4, mut t6) = (0u64, 0u64);
        self.q.retain(|d| {
            let keep = d.deadline > now;
            if !keep && !d.poisoned {
                match d.key {
                    Key::V4 { .. } => t4 += 1,
                    Key::V6 { .. } => t6 += 1,
                }
            }
            keep
        });
        // Like Linux, ReasmFails counts timeouts too.
        self.stats.timeouts += t4;
        self.stats.fails += t4;
        self.stats6.timeouts += t6;
        self.stats6.fails += t6;
    }

    /// One IPv4 fragment (`pkt`: the whole IP packet, header `ihl` bytes long, `total` its
    /// length).
    pub fn input(&mut self, now: u64, pkt: &[u8], ihl: usize, total: usize) -> Outcome {
        let fl = be16(pkt, 6);
        let f = Frag {
            key: Key::V4 {
                src: Ipv4([pkt[12], pkt[13], pkt[14], pkt[15]]),
                dst: Ipv4([pkt[16], pkt[17], pkt[18], pkt[19]]),
                id: be16(pkt, 4),
                proto: pkt[9],
            },
            off: (fl & 0x1fff) as usize * 8,
            more: fl & 0x2000 != 0,
            payload: &pkt[ihl..total],
            hdr: &pkt[..ihl],
            proto: Some(pkt[9]),
            v6: false,
        };
        match self.add(now, f) {
            Outcome::Done(whole) => {
                let mut out = whole;
                let hl = ihl.min(out.len());
                let n = out.len();
                put16(&mut out, 2, n as u16);
                put16(&mut out, 6, 0);
                put16(&mut out, 10, 0);
                let c = checksum(&out[..hl]);
                put16(&mut out, 10, c);
                Outcome::Done(out)
            }
            o => o,
        }
    }

    /// One IPv6 fragment: `pkt` is the packet, `fh` the offset of its fragment header,
    /// `nh_at` the offset of the next-header byte that names it (6, or the first byte of
    /// the extension header before it). `upper` is the upper-layer protocol, if the
    /// caller could see it (first fragments).
    pub fn input6(
        &mut self,
        now: u64,
        pkt: &[u8],
        fh: usize,
        nh_at: usize,
        upper: Option<u8>,
    ) -> Outcome {
        if pkt.len() < fh + 8 {
            return Outcome::Dropped;
        }
        let fl = be16(pkt, fh + 2);
        let mut hdr = pkt[..fh].to_vec();
        hdr[nh_at] = pkt[fh];
        let f = Frag {
            key: Key::V6 {
                src: Ipv6(pkt[8..24].try_into().unwrap()),
                dst: Ipv6(pkt[24..40].try_into().unwrap()),
                id: u32::from_be_bytes(pkt[fh + 4..fh + 8].try_into().unwrap()),
            },
            off: (fl >> 3) as usize * 8,
            more: fl & 1 != 0,
            payload: &pkt[fh + 8..],
            hdr: &hdr,
            proto: upper,
            v6: true,
        };
        // Counted as IPv6: run it against the v4 counters, then move the difference.
        let before = self.stats;
        let r = self.add(now, f);
        let after = self.stats;
        self.stats = before;
        self.stats6.add_delta(after, before);
        match r {
            Outcome::Done(mut out) => {
                let plen = out.len() - 40;
                put16(&mut out, 4, plen as u16);
                Outcome::Done(out)
            }
            o => o,
        }
    }

    fn add(&mut self, now: u64, f: Frag) -> Outcome {
        self.stats.reqds += 1;
        let (key, off, more, payload) = (f.key, f.off, f.more, f.payload);
        // Bytes of header that count against the 65535 limit (IPv6: past the fixed 40).
        let ihl = if f.v6 { f.hdr.len() - 40 } else { f.hdr.len() };
        let k = match self.q.iter().position(|d| d.key == key) {
            Some(k) => k,
            None => {
                if self.q.len() >= MAX_DATAGRAMS {
                    self.drop_oldest();
                }
                self.q.push(Datagram {
                    key,
                    hdr: None,
                    data: Vec::new(),
                    have: Vec::new(),
                    total: None,
                    deadline: now + TIMEOUT,
                    started: now,
                    poisoned: false,
                });
                self.q.len() - 1
            }
        };
        if self.q[k].poisoned {
            self.stats.fails += 1;
            return Outcome::Dropped;
        }
        let bad = |why_overlap: bool, s: &mut Self, k: usize| {
            let d = &mut s.q[k];
            d.poisoned = true;
            d.data = Vec::new();
            d.have = Vec::new();
            d.hdr = None;
            s.stats.fails += 1;
            if why_overlap {
                s.stats.overlaps += 1;
            }
            Outcome::Dropped
        };
        let end = off + payload.len();
        if payload.is_empty() && more {
            return Outcome::Pending;
        }
        if end > MAX_LEN - ihl || (more && payload.len() % 8 != 0) {
            return bad(false, self, k);
        }
        // RFC 1858: tiny / overlapping-header fragments for TCP. IPv6 (RFC 7112): a first
        // fragment must hold the upper-layer header, which we take as 8 bytes (20 for TCP).
        if f.proto == Some(PROTO_TCP) && (off == 8 || (off == 0 && payload.len() < 20)) {
            return bad(false, self, k);
        }
        if f.v6 && off == 0 && (f.proto.is_none() || payload.len() < 8) {
            return bad(false, self, k);
        }
        {
            let d = &self.q[k];
            if let Some(t) = d.total
                && (end > t || (!more && end != t))
            {
                return bad(false, self, k);
            }
            if !more
                && (0..MAX_LEN.div_ceil(8))
                    .skip(end.div_ceil(8))
                    .any(|b| d.block(b))
            {
                return bad(false, self, k); // data already past the claimed end
            }
            if (off / 8..end.div_ceil(8)).any(|b| d.block(b)) {
                return bad(true, self, k);
            }
        }
        let mut k = k;
        if self.buffered() + payload.len() > MAX_BYTES {
            self.drop_oldest_except(k);
            k = self.q.iter().position(|d| d.key == key).unwrap();
            if self.buffered() + payload.len() > MAX_BYTES {
                return bad(false, self, k);
            }
        }
        let d = &mut self.q[k];
        if d.data.len() < end {
            d.data.resize(end, 0);
        }
        d.data[off..end].copy_from_slice(payload);
        for b in off / 8..end.div_ceil(8) {
            d.set(b);
        }
        if !more {
            d.total = Some(end);
        }
        if off == 0 {
            d.hdr = Some(f.hdr.to_vec());
        }
        if !d.complete() {
            return Outcome::Pending;
        }
        let d = self.q.swap_remove(k);
        let t = d.total.unwrap();
        let hdr = d.hdr.unwrap();
        let mut out = vec![0u8; hdr.len() + t];
        out[..hdr.len()].copy_from_slice(&hdr);
        out[hdr.len()..].copy_from_slice(&d.data[..t]);
        self.stats.oks += 1;
        let _ = d.started;
        Outcome::Done(out)
    }

    fn drop_oldest(&mut self) {
        self.drop_oldest_except(usize::MAX);
    }

    fn drop_oldest_except(&mut self, keep: usize) {
        if let Some((k, _)) = self
            .q
            .iter()
            .enumerate()
            .filter(|(k, _)| *k != keep)
            .min_by_key(|(_, d)| d.started)
        {
            self.q.swap_remove(k);
            self.stats.fails += 1;
        }
    }
}
