//! ARP (RFC 826) for IPv4 over Ethernet: a small fixed-size cache plus request rate limiting.

use super::wire::{Ipv4, Mac};

pub const ARP_SLOTS: usize = 32;
/// Resolved entries are forgotten after this long (re-resolved on next use).
const REACHABLE_NS: u64 = 10 * 60 * 1_000_000_000;
/// Minimum gap between requests for the same address.
pub const RETRY_NS: u64 = 1_000_000_000;
/// Requests sent before an incomplete entry is given up.
const MAX_TRIES: u8 = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArpState {
    Incomplete,
    Reachable,
}

#[derive(Clone, Copy)]
pub struct ArpEntry {
    pub ip: Ipv4,
    pub mac: Mac,
    pub state: ArpState,
    pub updated_ns: u64,
    last_req_ns: u64,
    tries: u8,
}

pub struct ArpCache {
    pub entries: [Option<ArpEntry>; ARP_SLOTS],
}

impl ArpCache {
    pub const fn new() -> ArpCache {
        ArpCache {
            entries: [None; ARP_SLOTS],
        }
    }

    fn find(&mut self, ip: Ipv4) -> Option<&mut ArpEntry> {
        self.entries.iter_mut().flatten().find(|e| e.ip == ip)
    }

    pub fn lookup(&self, ip: Ipv4) -> Option<Mac> {
        self.entries
            .iter()
            .flatten()
            .find(|e| e.ip == ip && e.state == ArpState::Reachable)
            .map(|e| e.mac)
    }

    pub fn has(&self, ip: Ipv4) -> bool {
        self.entries.iter().flatten().any(|e| e.ip == ip)
    }

    /// Slot for a new entry: a free one, else the oldest.
    fn victim(&mut self) -> &mut Option<ArpEntry> {
        let mut best = 0;
        let mut best_t = u64::MAX;
        for (i, e) in self.entries.iter().enumerate() {
            match e {
                None => {
                    best = i;
                    break;
                }
                Some(e) if e.updated_ns < best_t => {
                    best = i;
                    best_t = e.updated_ns;
                }
                _ => {}
            }
        }
        &mut self.entries[best]
    }

    /// Learn (or refresh) `ip -> mac`.
    pub fn insert(&mut self, ip: Ipv4, mac: Mac, now: u64) {
        if let Some(e) = self.find(ip) {
            e.mac = mac;
            e.state = ArpState::Reachable;
            e.updated_ns = now;
            e.tries = 0;
            return;
        }
        *self.victim() = Some(ArpEntry {
            ip,
            mac,
            state: ArpState::Reachable,
            updated_ns: now,
            last_req_ns: now,
            tries: 0,
        });
    }

    /// We need `ip` resolved: should a request go out now? Creates an incomplete entry.
    pub fn want(&mut self, ip: Ipv4, now: u64) -> bool {
        if let Some(e) = self.find(ip) {
            if e.state == ArpState::Reachable {
                return false;
            }
            if now.saturating_sub(e.last_req_ns) < RETRY_NS {
                return false;
            }
            e.last_req_ns = now;
            e.tries = e.tries.saturating_add(1);
            return true;
        }
        *self.victim() = Some(ArpEntry {
            ip,
            mac: Mac::ZERO,
            state: ArpState::Incomplete,
            updated_ns: now,
            last_req_ns: now,
            tries: 1,
        });
        true
    }

    /// Drop stale entries; returns addresses whose resolution failed (their queued packets
    /// should be dropped).
    pub fn expire(&mut self, now: u64, mut failed: impl FnMut(Ipv4)) {
        for slot in self.entries.iter_mut() {
            let Some(e) = slot else { continue };
            let dead = match e.state {
                ArpState::Reachable => now.saturating_sub(e.updated_ns) > REACHABLE_NS,
                ArpState::Incomplete => {
                    e.tries >= MAX_TRIES && now.saturating_sub(e.last_req_ns) >= RETRY_NS
                }
            };
            if dead {
                if e.state == ArpState::Incomplete {
                    failed(e.ip);
                }
                *slot = None;
            }
        }
    }

    /// Incomplete entries that are due for another request.
    pub fn due(&self, now: u64, out: &mut [Ipv4; ARP_SLOTS]) -> usize {
        let mut n = 0;
        for e in self.entries.iter().flatten() {
            if e.state == ArpState::Incomplete
                && e.tries < MAX_TRIES
                && now.saturating_sub(e.last_req_ns) >= RETRY_NS
            {
                out[n] = e.ip;
                n += 1;
            }
        }
        n
    }
}

pub const OP_REQUEST: u16 = 1;
pub const OP_REPLY: u16 = 2;

/// Write an ARP packet (28 bytes) for Ethernet/IPv4.
pub fn build(buf: &mut [u8], op: u16, sha: Mac, spa: Ipv4, tha: Mac, tpa: Ipv4) -> usize {
    buf[0..2].copy_from_slice(&1u16.to_be_bytes()); // HTYPE Ethernet
    buf[2..4].copy_from_slice(&0x0800u16.to_be_bytes()); // PTYPE IPv4
    buf[4] = 6;
    buf[5] = 4;
    buf[6..8].copy_from_slice(&op.to_be_bytes());
    buf[8..14].copy_from_slice(&sha.0);
    buf[14..18].copy_from_slice(&spa.0);
    buf[18..24].copy_from_slice(&tha.0);
    buf[24..28].copy_from_slice(&tpa.0);
    28
}
