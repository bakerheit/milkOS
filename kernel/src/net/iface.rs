//! The interface layer: Ethernet framing, ARP resolution, IPv4 output and routing, and the
//! loopback queue.
//!
//! There is one Ethernet interface (`eth0`, the virtio-net device, if any) and `lo`.
//! Routing is three rules: 127/8 and our own address go to `lo`; addresses on our subnet
//! (and broadcasts) go out directly; everything else goes to the default gateway.
//!
//! Nothing here allocates after [`Iface::new`]: frames are built straight into device tx
//! buffers, packets waiting for ARP sit in a small fixed pool, and loopback packets in a
//! fixed ring.

use alloc::vec;

use super::arp::{self, ArpCache};
use super::errno::*;
use super::wire::{Ipv4, Mac, checksum, put16};
use crate::fs::errno::Result;
use crate::virtio::net::{DevStats, VirtioNet};

pub const ETH_HDR: usize = 14;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const MTU: usize = 1500;
pub const IP_HDR: usize = 20;

pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

/// Packets waiting for an ARP answer.
const PENDING: usize = 8;
/// How long a packet may wait for ARP.
const PENDING_NS: u64 = 3_000_000_000;
/// Loopback ring size (packets).
const LO_SLOTS: usize = 32;

/// IPv4 configuration of eth0 (from DHCP). `addr` unspecified = not configured.
#[derive(Clone, Copy, Default, Debug)]
pub struct IpConfig {
    pub addr: Ipv4,
    pub mask: Ipv4,
    pub gateway: Option<Ipv4>,
    pub dns: Option<Ipv4>,
}

impl IpConfig {
    pub fn configured(&self) -> bool {
        !self.addr.is_unspecified()
    }
    pub fn subnet_broadcast(&self) -> Ipv4 {
        Ipv4::from_u32(self.addr.to_u32() | !self.mask.to_u32())
    }
    pub fn is_local(&self, ip: Ipv4) -> bool {
        self.configured() && ip == self.addr
    }
}

#[derive(Clone, Copy)]
struct Pending {
    used: bool,
    nexthop: Ipv4,
    len: usize,
    since: u64,
}

pub struct Iface {
    pub dev: Option<VirtioNet>,
    pub mac: Mac,
    pub cfg: IpConfig,
    pub arp: ArpCache,
    pub lo_stats: DevStats,
    /// Stats of eth0 frames we dropped before they reached the device (ARP failures).
    pub eth_extra_drops: u64,
    ip_id: u16,
    pending: [Pending; PENDING],
    /// PENDING * MTU bytes.
    pending_buf: *mut u8,
    /// Loopback ring: LO_SLOTS * MTU bytes, lengths, head, count.
    lo_buf: *mut u8,
    lo_len: [u16; LO_SLOTS],
    lo_head: usize,
    lo_count: usize,
}

// SAFETY: the raw buffers are leaked heap memory owned by this Iface, accessed under the
// stack lock only.
unsafe impl Send for Iface {}

/// Leaked zeroed heap buffer (allocated in place, never on the stack).
fn leak(bytes: usize) -> *mut u8 {
    vec![0u8; bytes].leak().as_mut_ptr()
}

/// Where an outgoing packet goes.
enum Route {
    Loopback,
    Ether(Ipv4),
    Broadcast,
}

impl Iface {
    pub fn new() -> Iface {
        Iface {
            dev: None,
            mac: Mac::ZERO,
            cfg: IpConfig::default(),
            arp: ArpCache::new(),
            lo_stats: DevStats::default(),
            eth_extra_drops: 0,
            ip_id: 1,
            pending: [Pending {
                used: false,
                nexthop: Ipv4::UNSPECIFIED,
                len: 0,
                since: 0,
            }; PENDING],
            pending_buf: leak(PENDING * MTU),
            lo_buf: leak(LO_SLOTS * MTU),
            lo_len: [0; LO_SLOTS],
            lo_head: 0,
            lo_count: 0,
        }
    }

    /// Source address we use to reach `dst`.
    pub fn src_for(&self, dst: Ipv4) -> Ipv4 {
        if dst.is_loopback() {
            dst
        } else {
            self.cfg.addr
        }
    }

    /// Is `ip` one of ours (any 127/8 address, or eth0's)?
    pub fn is_our_addr(&self, ip: Ipv4) -> bool {
        ip.is_loopback() || self.cfg.is_local(ip)
    }

    fn route(&self, dst: Ipv4) -> Result<Route> {
        if self.is_our_addr(dst) {
            return Ok(Route::Loopback);
        }
        if dst.is_broadcast() || (self.cfg.configured() && dst == self.cfg.subnet_broadcast()) {
            return Ok(Route::Broadcast);
        }
        if dst.is_multicast() || !self.cfg.configured() {
            return Err(ENETUNREACH);
        }
        if dst.same_net(self.cfg.addr, self.cfg.mask) {
            Ok(Route::Ether(dst))
        } else {
            self.cfg.gateway.map(Route::Ether).ok_or(ENETUNREACH)
        }
    }

    /// Send an IPv4 packet. `fill` writes the payload into the buffer it gets (up to
    /// `MTU - 20` bytes) and returns its length; it must compute any L4 checksum itself
    /// with `src`/`dst`. Packets to an unresolved next hop wait (briefly) for ARP; Ok means
    /// sent or queued, not delivered.
    pub fn send_ip(
        &mut self,
        now: u64,
        src: Ipv4,
        dst: Ipv4,
        proto: u8,
        fill: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<()> {
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        match self.route(dst)? {
            Route::Loopback => {
                if self.lo_count == LO_SLOTS {
                    self.lo_stats.tx_dropped += 1;
                    return Err(ENOBUFS);
                }
                let slot = (self.lo_head + self.lo_count) % LO_SLOTS;
                // SAFETY: slot is free (not between head and head+count); MTU bytes each.
                let buf =
                    unsafe { core::slice::from_raw_parts_mut(self.lo_buf.add(slot * MTU), MTU) };
                let n = build_ip(buf, src, dst, proto, id, fill);
                self.lo_len[slot] = n as u16;
                self.lo_count += 1;
                self.lo_stats.tx_packets += 1;
                self.lo_stats.tx_bytes += n as u64;
                self.lo_stats.rx_packets += 1;
                self.lo_stats.rx_bytes += n as u64;
                Ok(())
            }
            Route::Broadcast => self.send_frame(Mac::BROADCAST, src, dst, proto, id, fill),
            Route::Ether(hop) => {
                if let Some(mac) = self.arp.lookup(hop) {
                    return self.send_frame(mac, src, dst, proto, id, fill);
                }
                // Park the packet until ARP answers.
                let Some(k) = self.pending.iter().position(|p| !p.used) else {
                    self.eth_extra_drops += 1;
                    return Err(ENOBUFS);
                };
                // SAFETY: pending slot k is free; MTU bytes each.
                let buf =
                    unsafe { core::slice::from_raw_parts_mut(self.pending_buf.add(k * MTU), MTU) };
                let n = build_ip(buf, src, dst, proto, id, fill);
                self.pending[k] = Pending {
                    used: true,
                    nexthop: hop,
                    len: n,
                    since: now,
                };
                if self.arp.want(hop, now) {
                    self.send_arp(arp::OP_REQUEST, Mac::ZERO, hop);
                }
                Ok(())
            }
        }
    }

    fn send_frame(
        &mut self,
        dst_mac: Mac,
        src: Ipv4,
        dst: Ipv4,
        proto: u8,
        id: u16,
        fill: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<()> {
        let mac = self.mac;
        let dev = self.dev.as_mut().ok_or(ENETDOWN)?;
        let ok = dev.send_with(|f| {
            eth_header(f, dst_mac, mac, ETHERTYPE_IPV4);
            ETH_HDR + build_ip(&mut f[ETH_HDR..ETH_HDR + MTU], src, dst, proto, id, fill)
        });
        if ok { Ok(()) } else { Err(ENOBUFS) }
    }

    /// Send an ARP packet (request: `tha` zero, broadcast; reply: unicast to `tha`).
    pub fn send_arp(&mut self, op: u16, tha: Mac, tpa: Ipv4) {
        let mac = self.mac;
        let spa = self.cfg.addr;
        let Some(dev) = self.dev.as_mut() else { return };
        let dst = if op == arp::OP_REQUEST {
            Mac::BROADCAST
        } else {
            tha
        };
        dev.send_with(|f| {
            eth_header(f, dst, mac, ETHERTYPE_ARP);
            ETH_HDR + arp::build(&mut f[ETH_HDR..], op, mac, spa, tha, tpa)
        });
    }

    /// `ip` resolved to `mac`: send everything that was waiting for it.
    pub fn flush_pending(&mut self, ip: Ipv4, mac: Mac) {
        let me = self.mac;
        for k in 0..PENDING {
            let p = self.pending[k];
            if !p.used || p.nexthop != ip {
                continue;
            }
            self.pending[k].used = false;
            let Some(dev) = self.dev.as_mut() else {
                continue;
            };
            // SAFETY: slot k holds p.len bytes of a built packet.
            let pkt = unsafe { core::slice::from_raw_parts(self.pending_buf.add(k * MTU), p.len) };
            dev.send_with(|f| {
                eth_header(f, mac, me, ETHERTYPE_IPV4);
                f[ETH_HDR..ETH_HDR + pkt.len()].copy_from_slice(pkt);
                ETH_HDR + pkt.len()
            });
        }
    }

    /// Timers: ARP retries and expiry, stale parked packets.
    pub fn tick(&mut self, now: u64) {
        let mut due = [Ipv4::UNSPECIFIED; arp::ARP_SLOTS];
        let n = self.arp.due(now, &mut due);
        for &ip in &due[..n] {
            if self.arp.want(ip, now) {
                self.send_arp(arp::OP_REQUEST, Mac::ZERO, ip);
            }
        }
        let pending = &mut self.pending;
        let mut drops = 0;
        self.arp.expire(now, |ip| {
            for p in pending.iter_mut() {
                if p.used && p.nexthop == ip {
                    p.used = false;
                    drops += 1;
                }
            }
        });
        for p in self.pending.iter_mut() {
            if p.used && now.saturating_sub(p.since) > PENDING_NS {
                p.used = false;
                drops += 1;
            }
        }
        self.eth_extra_drops += drops;
    }

    /// ARP work outstanding (parked packets or unresolved entries)?
    pub fn busy(&self) -> bool {
        self.pending.iter().any(|p| p.used)
            || self
                .arp
                .entries
                .iter()
                .flatten()
                .any(|e| e.state == arp::ArpState::Incomplete)
    }

    /// Oldest loopback packet, if any. The slice stays valid until [`lo_pop`](Self::lo_pop)
    /// (new loopback packets go to other slots).
    pub fn lo_front<'a>(&self) -> Option<&'a [u8]> {
        if self.lo_count == 0 {
            return None;
        }
        let slot = self.lo_head;
        // SAFETY: slot is occupied and not reused before lo_pop.
        Some(unsafe {
            core::slice::from_raw_parts(self.lo_buf.add(slot * MTU), self.lo_len[slot] as usize)
        })
    }

    pub fn lo_pop(&mut self) {
        if self.lo_count > 0 {
            self.lo_head = (self.lo_head + 1) % LO_SLOTS;
            self.lo_count -= 1;
        }
    }
}

pub fn eth_header(f: &mut [u8], dst: Mac, src: Mac, ethertype: u16) {
    f[0..6].copy_from_slice(&dst.0);
    f[6..12].copy_from_slice(&src.0);
    put16(f, 12, ethertype);
}

/// Build an IPv4 header + payload in `buf` (at least MTU long); returns the packet length.
fn build_ip(
    buf: &mut [u8],
    src: Ipv4,
    dst: Ipv4,
    proto: u8,
    id: u16,
    fill: impl FnOnce(&mut [u8]) -> usize,
) -> usize {
    let n = fill(&mut buf[IP_HDR..MTU]).min(MTU - IP_HDR);
    let total = IP_HDR + n;
    buf[0] = 0x45;
    buf[1] = 0;
    put16(buf, 2, total as u16);
    put16(buf, 4, id);
    put16(buf, 6, 0x4000); // DF: we never fragment
    buf[8] = 64;
    buf[9] = proto;
    put16(buf, 10, 0);
    buf[12..16].copy_from_slice(&src.0);
    buf[16..20].copy_from_slice(&dst.0);
    let c = checksum(&buf[..IP_HDR]);
    put16(buf, 10, c);
    total
}
