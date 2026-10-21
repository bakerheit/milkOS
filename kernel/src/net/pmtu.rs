//! Path MTU discovery (RFC 1191 for IPv4, RFC 8201 for IPv6), both families in one cache.
//!
//! * Everything starts at the link MTU (1500). IPv4 TCP goes out with DF set; IPv6 is never
//!   fragmented by routers anyway.
//! * An ICMP "fragmentation needed" (type 3 code 4, with the next-hop MTU) or an ICMPv6
//!   "packet too big" (type 2) about a packet we sent lowers the path MTU to that
//!   destination: never below 552 (IPv4, as Linux's `min_pmtu`) or 1280 (IPv6, the
//!   minimum link MTU), never up. For TCP the quoted segment must belong to a connection
//!   and its sequence number must be in flight (RFC 5927), else the message is ignored.
//! * TCP drops its MSS to fit and resends what is in flight at once, without treating the
//!   loss as congestion. UDP fragments at the path MTU (both families).
//! * Entries expire after 10 minutes (RFC 1191's suggestion), then the path is tried at the
//!   link MTU again. At most [`SLOTS`] destinations are remembered (oldest replaced).

use alloc::vec::Vec;

use super::iface::MTU;
use super::wire::IpAddr;

pub const SLOTS: usize = 64;
pub const EXPIRE: u64 = 600_000_000_000;
pub const MIN4: usize = 552;
pub const MIN6: usize = 1280;

#[derive(Clone, Copy)]
pub struct Entry {
    pub dst: IpAddr,
    pub mtu: usize,
    pub until: u64,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct PmtuStats {
    /// Messages that lowered a path MTU.
    pub learned: u64,
    /// Messages ignored (no matching connection, sequence not in flight, not lower).
    pub ignored: u64,
}

pub struct Cache {
    pub entries: Vec<Entry>,
    pub stats: PmtuStats,
}

impl Cache {
    pub const fn new() -> Cache {
        Cache {
            entries: Vec::new(),
            stats: PmtuStats {
                learned: 0,
                ignored: 0,
            },
        }
    }

    /// The path MTU to `dst` (the link MTU unless we learned less).
    pub fn mtu(&self, dst: IpAddr, now: u64) -> usize {
        self.entries
            .iter()
            .find(|e| e.dst == dst && e.until > now)
            .map_or(MTU, |e| e.mtu)
    }

    /// A "too big" said the path to `dst` takes `mtu`. Returns the new path MTU if it
    /// went down.
    pub fn learn(&mut self, dst: IpAddr, mtu: usize, now: u64) -> Option<usize> {
        let floor = if dst.is_v6() { MIN6 } else { MIN4 };
        let mtu = mtu.clamp(floor, MTU);
        if mtu >= self.mtu(dst, now) {
            self.stats.ignored += 1;
            return None;
        }
        self.stats.learned += 1;
        self.entries.retain(|e| e.dst != dst && e.until > now);
        if self.entries.len() >= SLOTS
            && let Some(k) = (0..self.entries.len()).min_by_key(|&k| self.entries[k].until)
        {
            self.entries.swap_remove(k);
        }
        self.entries.push(Entry {
            dst,
            mtu,
            until: now + EXPIRE,
        });
        Some(mtu)
    }

    pub fn expire(&mut self, now: u64) {
        self.entries.retain(|e| e.until > now);
    }

    /// Forget everything (the self-test, after it played with loopback's MTU).
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The largest TCP payload per segment for a path MTU (IP + TCP headers, no options).
pub fn mss_for(dst: IpAddr, mtu: usize) -> u16 {
    (mtu - if dst.is_v6() { 60 } else { 40 }) as u16
}
