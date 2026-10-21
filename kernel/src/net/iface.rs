//! The interface layer: Ethernet framing, ARP resolution, IPv4 output and routing, and the
//! loopback queue.
//!
//! Up to two Ethernet interfaces and `lo`: `eth0` (the first virtio-net device: slirp,
//! DHCP) and `eth1` (a second one, for VM-to-VM links: a link-local 169.254/16 address
//! from its MAC, and link-scope multicast). Routing: 127/8 and our own addresses go to
//! `lo`; broadcasts and multicast out of the link they belong to (multicast only on a
//! link-local link); addresses on a link's subnet (169.254/16 on eth1) go out directly;
//! everything else goes to the default gateway (eth0's, from DHCP). With one NIC this is
//! exactly the old single-interface behaviour.
//!
//! Nothing here allocates after [`Iface::new`]: frames are built straight into device tx
//! buffers, packets waiting for ARP sit in a small fixed pool, and loopback packets in a
//! fixed ring.

use alloc::vec;
use core::sync::atomic::{AtomicU32, Ordering};

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
const LO_SLOTS: usize = 256;

/// Test knob: drop 1 in N loopback packets, pseudo-randomly (0 = off). The self-test uses it to prove
/// TCP recovers from loss; slirp never loses anything.
pub static LO_DROP_EVERY: AtomicU32 = AtomicU32::new(0);
/// Test knob (the self-test's path MTU test): loopback behaves like a path whose MTU is
/// this (0 = off). A bigger packet is not delivered; it is kept in `lo_too_big` for the
/// stack to answer like a router would (ICMP fragmentation needed / ICMPv6 packet too big).
pub static LO_MTU: AtomicU32 = AtomicU32::new(0);

/// IPv4 configuration of a link (eth0: DHCP; eth1: link-local). `addr` unspecified = not
/// configured.
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
    link: u8,
    nexthop: Ipv4,
    len: usize,
    since: u64,
}

/// Most Ethernet interfaces: eth0 (slirp, DHCP) and eth1 (VM-to-VM, link-local).
pub const MAX_LINKS: usize = 2;
pub const LINK_NAMES: [&str; MAX_LINKS] = ["eth0", "eth1"];

/// Gap between link-local ARP probes, and how many go out before the address is taken.
const LL_PROBE_NS: u64 = 200_000_000;
const LL_PROBES: u8 = 2;

/// IPv4 link-local autoconfiguration (RFC 3927, simplified): a candidate in 169.254/16
/// derived from the MAC, probed with ARP, taken if nobody answers, re-picked on conflict.
#[derive(Clone, Copy, Default, Debug)]
pub struct LinkLocal {
    pub on: bool,
    pub cand: Ipv4,
    probes: u8,
    next: u64,
    pub conflicts: u32,
}

/// One Ethernet interface: a virtio-net device, its MAC, IPv4 config and ARP cache.
pub struct Link {
    pub dev: Option<VirtioNet>,
    /// virtio-mmio slot (interfaces are named by slot, highest first, which is the qemu
    /// command-line order: the first `-device virtio-net-device` is eth0).
    pub slot: usize,
    pub mac: Mac,
    pub cfg: IpConfig,
    pub arp: ArpCache,
    /// eth frames we dropped before they reached the device (ARP failures, full pool).
    pub extra_drops: u64,
    pub ll: LinkLocal,
    /// IPv6: addresses, neighbour cache, router.
    pub v6: super::ipv6::Link6,
}

impl Link {
    const fn empty() -> Link {
        Link {
            dev: None,
            slot: 0,
            mac: Mac::ZERO,
            cfg: IpConfig {
                addr: Ipv4::UNSPECIFIED,
                mask: Ipv4::UNSPECIFIED,
                gateway: None,
                dns: None,
            },
            arp: ArpCache::new(),
            extra_drops: 0,
            ll: LinkLocal {
                on: false,
                cand: Ipv4::UNSPECIFIED,
                probes: 0,
                next: 0,
                conflicts: 0,
            },
            v6: super::ipv6::Link6::new(),
        }
    }

    pub fn up(&self) -> bool {
        self.dev.is_some()
    }

    /// Does this link take IPv4 multicast (224.0.0.0/24, link scope)? Only link-local
    /// links: eth0 keeps its old behaviour (no multicast).
    pub fn multicast(&self) -> bool {
        self.ll.on && self.cfg.configured()
    }
}

/// The link-local candidate for `mac` after `conflicts` collisions: 169.254.X.Y with X, Y
/// from the last two MAC bytes (kept in 1..=254), then mixed on each conflict.
pub fn ll_candidate(mac: Mac, conflicts: u32) -> Ipv4 {
    let m = mac.0;
    let fix = |b: u32| {
        if (1..=254).contains(&b) {
            b
        } else {
            1 + b % 254
        }
    };
    let mut x = fix(m[4] as u32);
    let mut y = fix(m[5] as u32);
    if conflicts > 0 {
        let h = (m[3] as u32 ^ (m[4] as u32) << 8 ^ (m[5] as u32) << 16)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(conflicts.wrapping_mul(40_503));
        x = 1 + (h >> 8) % 254;
        y = 1 + (h >> 20) % 254;
    }
    Ipv4::new(169, 254, x as u8, y as u8)
}

pub fn is_link_local(ip: Ipv4) -> bool {
    ip.0[0] == 169 && ip.0[1] == 254
}

/// Link-scope multicast (224.0.0.0/24): never routed, what discovery uses.
pub fn is_link_multicast(ip: Ipv4) -> bool {
    ip.0[0] == 224 && ip.0[1] == 0 && ip.0[2] == 0
}

/// Ethernet address for an IPv4 multicast group (RFC 1112: 01:00:5e + low 23 bits).
pub fn multicast_mac(ip: Ipv4) -> Mac {
    Mac([0x01, 0x00, 0x5e, ip.0[1] & 0x7f, ip.0[2], ip.0[3]])
}

pub struct Iface {
    pub links: [Link; MAX_LINKS],
    pub lo_stats: DevStats,
    ip_id: u16,
    pending: [Pending; PENDING],
    /// PENDING * MTU bytes.
    pending_buf: *mut u8,
    /// Loopback ring: LO_SLOTS * MTU bytes, lengths, head, count.
    lo_buf: *mut u8,
    lo_len: [u16; LO_SLOTS],
    lo_head: usize,
    lo_count: usize,
    lo_seen: u32,
    /// Packets too big for [`LO_MTU`] (their first bytes), waiting for an ICMP answer.
    pub lo_too_big: alloc::vec::Vec<alloc::vec::Vec<u8>>,
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
    /// Link, next hop.
    Ether(usize, Ipv4),
    /// Link, destination MAC (broadcast or multicast).
    Flood(usize, Mac),
}

impl Iface {
    pub fn new() -> Iface {
        Iface {
            links: [Link::empty(), Link::empty()],
            lo_stats: DevStats::default(),
            ip_id: 1,
            pending: [Pending {
                used: false,
                link: 0,
                nexthop: Ipv4::UNSPECIFIED,
                len: 0,
                since: 0,
            }; PENDING],
            pending_buf: leak(PENDING * MTU),
            lo_buf: leak(LO_SLOTS * MTU),
            lo_len: [0; LO_SLOTS],
            lo_head: 0,
            lo_count: 0,
            lo_seen: 0,
            lo_too_big: alloc::vec::Vec::new(),
        }
    }

    /// eth0 (the first NIC; DHCP runs here).
    pub fn eth0(&self) -> &Link {
        &self.links[0]
    }
    pub fn eth0_mut(&mut self) -> &mut Link {
        &mut self.links[0]
    }

    /// Attach a device. Interfaces are kept ordered by virtio slot, highest first, so the
    /// names follow the qemu command line whatever order the slots were probed in. Returns
    /// the name it got, or None if every link is taken.
    pub fn attach(&mut self, dev: VirtioNet, slot: usize, mac: Mac) -> Option<&'static str> {
        let n = self.links.iter().filter(|l| l.up()).count();
        if n == MAX_LINKS {
            return None;
        }
        // Insert at its sorted position (links[..n] are up and sorted by slot, descending).
        let pos = self.links[..n]
            .iter()
            .position(|l| slot > l.slot)
            .unwrap_or(n);
        for k in (pos..n).rev() {
            self.links.swap(k, k + 1);
        }
        let l = &mut self.links[pos];
        *l = Link::empty();
        l.dev = Some(dev);
        l.slot = slot;
        l.mac = mac;
        // Every link but eth0 configures itself link-local (eth0 gets DHCP).
        for (k, l) in self.links.iter_mut().enumerate() {
            if l.up() {
                l.ll.on = k > 0;
            }
        }
        Some(LINK_NAMES[pos])
    }

    /// Start link-local autoconfiguration on every link that wants it.
    pub fn ll_start(&mut self, now: u64) {
        for l in self.links.iter_mut().filter(|l| l.up() && l.ll.on) {
            l.ll.cand = ll_candidate(l.mac, l.ll.conflicts);
            l.ll.probes = 0;
            l.ll.next = now;
            l.cfg = IpConfig::default();
        }
    }

    /// Which link owns `ip` as its address?
    pub fn link_of_addr(&self, ip: Ipv4) -> Option<usize> {
        self.links.iter().position(|l| l.up() && l.cfg.is_local(ip))
    }

    /// The link-local link (eth1), if configured.
    fn ll_link(&self) -> Option<usize> {
        self.links.iter().position(|l| l.up() && l.multicast())
    }

    /// Source address we use to reach `dst`.
    pub fn src_for(&self, dst: Ipv4) -> Ipv4 {
        if dst.is_loopback() {
            return dst;
        }
        if (is_link_local(dst) || dst.is_multicast())
            && let Some(k) = self.ll_link()
        {
            return self.links[k].cfg.addr;
        }
        for l in self.links.iter().filter(|l| l.up() && l.cfg.configured()) {
            if dst.same_net(l.cfg.addr, l.cfg.mask) {
                return l.cfg.addr;
            }
        }
        self.links[0].cfg.addr
    }

    /// Is `ip` one of ours (any 127/8 address, or a link's)?
    pub fn is_our_addr(&self, ip: Ipv4) -> bool {
        ip.is_loopback() || self.link_of_addr(ip).is_some()
    }

    fn route(&self, src: Ipv4, dst: Ipv4) -> Result<Route> {
        if self.is_our_addr(dst) {
            return Ok(Route::Loopback);
        }
        let by_src = self.link_of_addr(src);
        if dst.is_broadcast() {
            // Limited broadcast: out of the source's link, else eth0 (DHCP before an address).
            let k = by_src.unwrap_or(0);
            return if self.links[k].up() {
                Ok(Route::Flood(k, Mac::BROADCAST))
            } else {
                Err(ENETDOWN)
            };
        }
        for (k, l) in self.links.iter().enumerate() {
            if l.up() && l.cfg.configured() && dst == l.cfg.subnet_broadcast() {
                return Ok(Route::Flood(k, Mac::BROADCAST));
            }
        }
        if dst.is_multicast() {
            // Only link-local links carry multicast (eth0 never did).
            let k = by_src
                .filter(|&k| self.links[k].multicast())
                .or_else(|| self.ll_link())
                .ok_or(ENETUNREACH)?;
            return Ok(Route::Flood(k, multicast_mac(dst)));
        }
        for (k, l) in self.links.iter().enumerate() {
            if l.up() && l.cfg.configured() && dst.same_net(l.cfg.addr, l.cfg.mask) {
                return Ok(Route::Ether(k, dst));
            }
        }
        if is_link_local(dst)
            && let Some(k) = self.ll_link()
        {
            return Ok(Route::Ether(k, dst));
        }
        // Default route: the first link with a gateway (eth0 from DHCP).
        self.links
            .iter()
            .enumerate()
            .find_map(|(k, l)| {
                (l.up() && l.cfg.configured())
                    .then_some(l.cfg.gateway)
                    .flatten()
                    .map(|g| Route::Ether(k, g))
            })
            .ok_or(ENETUNREACH)
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
        self.send_ip_ex(now, src, dst, proto, None, fill)
    }

    /// A fresh IP identification (for the fragments of one datagram).
    pub fn next_ip_id(&mut self) -> u16 {
        let id = self.ip_id;
        self.ip_id = self.ip_id.wrapping_add(1);
        id
    }

    /// [`send_ip`](Self::send_ip), optionally as one fragment of a bigger datagram (its
    /// id, offset and more-fragments flag; DF is then clear).
    pub fn send_ip_ex(
        &mut self,
        now: u64,
        src: Ipv4,
        dst: Ipv4,
        proto: u8,
        frag: Option<Frag>,
        fill: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<()> {
        let id = match frag {
            Some(f) => f.id,
            None => self.next_ip_id(),
        };
        match self.route(src, dst)? {
            Route::Loopback => {
                self.lo_enqueue(|buf| build_ip(buf, src, dst, proto, id, frag, fill))
            }
            Route::Flood(k, mac) => self.send_frame(k, mac, src, dst, proto, id, frag, fill),
            Route::Ether(k, hop) => {
                if let Some(mac) = self.links[k].arp.lookup(hop) {
                    return self.send_frame(k, mac, src, dst, proto, id, frag, fill);
                }
                // Park the packet until ARP answers.
                let Some(slot) = self.pending.iter().position(|p| !p.used) else {
                    self.links[k].extra_drops += 1;
                    return Err(ENOBUFS);
                };
                // SAFETY: pending slot is free; MTU bytes each.
                let buf = unsafe {
                    core::slice::from_raw_parts_mut(self.pending_buf.add(slot * MTU), MTU)
                };
                let n = build_ip(buf, src, dst, proto, id, frag, fill);
                self.pending[slot] = Pending {
                    used: true,
                    link: k as u8,
                    nexthop: hop,
                    len: n,
                    since: now,
                };
                if self.links[k].arp.want(hop, now) {
                    self.send_arp(k, arp::OP_REQUEST, Mac::ZERO, hop);
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn send_frame(
        &mut self,
        k: usize,
        dst_mac: Mac,
        src: Ipv4,
        dst: Ipv4,
        proto: u8,
        id: u16,
        frag: Option<Frag>,
        fill: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<()> {
        let l = &mut self.links[k];
        let mac = l.mac;
        let dev = l.dev.as_mut().ok_or(ENETDOWN)?;
        let ok = dev.send_with(|f| {
            eth_header(f, dst_mac, mac, ETHERTYPE_IPV4);
            ETH_HDR
                + build_ip(
                    &mut f[ETH_HDR..ETH_HDR + MTU],
                    src,
                    dst,
                    proto,
                    id,
                    frag,
                    fill,
                )
        });
        if ok { Ok(()) } else { Err(ENOBUFS) }
    }

    /// Send an ARP packet on link `k` (request: `tha` zero, broadcast; reply: unicast to
    /// `tha`).
    pub fn send_arp(&mut self, k: usize, op: u16, tha: Mac, tpa: Ipv4) {
        let spa = self.links[k].cfg.addr;
        self.send_arp_from(k, op, spa, tha, tpa);
    }

    /// ARP with an explicit sender address (link-local probes use 0.0.0.0, announcements
    /// the new address).
    fn send_arp_from(&mut self, k: usize, op: u16, spa: Ipv4, tha: Mac, tpa: Ipv4) {
        let l = &mut self.links[k];
        let mac = l.mac;
        let Some(dev) = l.dev.as_mut() else { return };
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

    /// `ip` resolved to `mac` on link `k`: send everything that was waiting for it.
    pub fn flush_pending(&mut self, k: usize, ip: Ipv4, mac: Mac) {
        let me = self.links[k].mac;
        for slot in 0..PENDING {
            let p = self.pending[slot];
            if !p.used || p.link as usize != k || p.nexthop != ip {
                continue;
            }
            self.pending[slot].used = false;
            let Some(dev) = self.links[k].dev.as_mut() else {
                continue;
            };
            // SAFETY: the slot holds p.len bytes of a built packet.
            let pkt =
                unsafe { core::slice::from_raw_parts(self.pending_buf.add(slot * MTU), p.len) };
            dev.send_with(|f| {
                eth_header(f, mac, me, ETHERTYPE_IPV4);
                f[ETH_HDR..ETH_HDR + pkt.len()].copy_from_slice(pkt);
                ETH_HDR + pkt.len()
            });
        }
    }

    /// An ARP packet on link `k` mentions `spa` (from `sha`) / asks for `tpa`: link-local
    /// conflict detection. True if it was a conflict (the caller then ignores it).
    pub fn ll_arp_seen(&mut self, k: usize, sha: Mac, spa: Ipv4, tpa: Ipv4, now: u64) -> bool {
        let l = &mut self.links[k];
        if !l.ll.on || sha == l.mac {
            return false;
        }
        let probing = !l.cfg.configured() && !l.ll.cand.is_unspecified();
        let clash = if probing {
            spa == l.ll.cand || (spa.is_unspecified() && tpa == l.ll.cand)
        } else {
            l.cfg.configured() && spa == l.cfg.addr
        };
        if !clash {
            return false;
        }
        let old = if probing { l.ll.cand } else { l.cfg.addr };
        l.ll.conflicts += 1;
        l.ll.cand = ll_candidate(l.mac, l.ll.conflicts);
        l.ll.probes = 0;
        l.ll.next = now + LL_PROBE_NS;
        l.cfg = IpConfig::default();
        crate::kprintln!(
            "[net] {}: link-local {old} is taken by {sha}; trying {}",
            LINK_NAMES[k],
            l.ll.cand
        );
        true
    }

    /// Timers: link-local probing, ARP retries and expiry, stale parked packets.
    pub fn tick(&mut self, now: u64) {
        for k in 0..MAX_LINKS {
            let l = &self.links[k];
            if !l.up() || !l.ll.on || l.cfg.configured() || l.ll.cand.is_unspecified() {
                continue;
            }
            if now < l.ll.next {
                continue;
            }
            let cand = l.ll.cand;
            if l.ll.probes < LL_PROBES {
                self.send_arp_from(k, arp::OP_REQUEST, Ipv4::UNSPECIFIED, Mac::ZERO, cand);
                let l = &mut self.links[k];
                l.ll.probes += 1;
                l.ll.next = now + LL_PROBE_NS;
            } else {
                let l = &mut self.links[k];
                l.cfg = IpConfig {
                    addr: cand,
                    mask: Ipv4::new(255, 255, 0, 0),
                    gateway: None,
                    dns: None,
                };
                crate::kprintln!(
                    "[net] {}: link-local address {cand}/16 (from mac {})",
                    LINK_NAMES[k],
                    l.mac
                );
                // Announce it, so peers' ARP caches are fresh.
                self.send_arp_from(k, arp::OP_REQUEST, cand, Mac::ZERO, cand);
            }
        }
        let mut drops = [0u64; MAX_LINKS];
        for k in 0..MAX_LINKS {
            if !self.links[k].up() {
                continue;
            }
            let mut due = [Ipv4::UNSPECIFIED; arp::ARP_SLOTS];
            let n = self.links[k].arp.due(now, &mut due);
            for &ip in &due[..n] {
                if self.links[k].arp.want(ip, now) {
                    self.send_arp(k, arp::OP_REQUEST, Mac::ZERO, ip);
                }
            }
            let pending = &mut self.pending;
            self.links[k].arp.expire(now, |ip| {
                for p in pending.iter_mut() {
                    if p.used && p.link as usize == k && p.nexthop == ip {
                        p.used = false;
                        drops[k] += 1;
                    }
                }
            });
        }
        for p in self.pending.iter_mut() {
            if p.used && now.saturating_sub(p.since) > PENDING_NS {
                p.used = false;
                drops[p.link as usize] += 1;
            }
        }
        for (l, d) in self.links.iter_mut().zip(drops) {
            l.extra_drops += d;
        }
    }

    /// Timer work outstanding (parked packets, unresolved entries, link-local probing)?
    pub fn busy(&self) -> bool {
        self.pending.iter().any(|p| p.used)
            || self.links.iter().any(|l| {
                (l.ll.on && l.up() && !l.cfg.configured())
                    || l.arp
                        .entries
                        .iter()
                        .flatten()
                        .any(|e| e.state == arp::ArpState::Incomplete)
            })
    }

    /// Queue one packet (IPv4 or IPv6, built by `build` into an MTU-sized buffer) on the
    /// loopback ring. The lossy-loopback test knob applies.
    pub fn lo_enqueue(&mut self, build: impl FnOnce(&mut [u8]) -> usize) -> Result<()> {
        let every = LO_DROP_EVERY.load(Ordering::Relaxed);
        if every != 0 {
            // A fixed pseudo-random sequence (xorshift32, same seed every run), so the
            // rate is 1 in `every` but the losses don't line up with the traffic's own
            // period (dropping exactly every Nth packet can hit the same packet of a
            // repeating exchange for ever).
            let mut x = if self.lo_seen == 0 {
                0x6d69_6c6b
            } else {
                self.lo_seen
            };
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            self.lo_seen = x;
            if x.is_multiple_of(every) {
                self.lo_stats.tx_dropped += 1;
                return Ok(()); // "lost on the wire"
            }
        }
        if self.lo_count == LO_SLOTS {
            self.lo_stats.tx_dropped += 1;
            return Err(ENOBUFS);
        }
        let slot = (self.lo_head + self.lo_count) % LO_SLOTS;
        // SAFETY: slot is free (not between head and head+count); MTU bytes each.
        let buf = unsafe { core::slice::from_raw_parts_mut(self.lo_buf.add(slot * MTU), MTU) };
        let n = build(buf);
        let lo_mtu = LO_MTU.load(Ordering::Relaxed) as usize;
        if lo_mtu != 0 && n > lo_mtu {
            // "Too big for the next hop": keep enough of it to quote, deliver nothing.
            if self.lo_too_big.len() < 16 {
                self.lo_too_big.push(buf[..n.min(1180)].to_vec());
            }
            self.lo_stats.tx_dropped += 1;
            return Ok(());
        }
        self.lo_len[slot] = n as u16;
        self.lo_count += 1;
        self.lo_stats.tx_packets += 1;
        self.lo_stats.tx_bytes += n as u64;
        self.lo_stats.rx_packets += 1;
        self.lo_stats.rx_bytes += n as u64;
        Ok(())
    }

    /// Send one Ethernet frame on link `k`: `build` writes the payload (up to MTU bytes)
    /// after the header and returns its length. False if the device queue is full.
    pub fn send_eth(
        &mut self,
        k: usize,
        dst: Mac,
        ethertype: u16,
        build: impl FnOnce(&mut [u8]) -> usize,
    ) -> Result<()> {
        let l = &mut self.links[k];
        let mac = l.mac;
        let dev = l.dev.as_mut().ok_or(ENETDOWN)?;
        let ok = dev.send_with(|f| {
            eth_header(f, dst, mac, ethertype);
            ETH_HDR + build(&mut f[ETH_HDR..ETH_HDR + MTU])
        });
        if ok { Ok(()) } else { Err(ENOBUFS) }
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
/// One fragment's place in its datagram.
#[derive(Clone, Copy)]
pub struct Frag {
    pub id: u16,
    /// Offset in 8-byte units.
    pub off8: u16,
    pub more: bool,
}

#[allow(clippy::too_many_arguments)]
fn build_ip(
    buf: &mut [u8],
    src: Ipv4,
    dst: Ipv4,
    proto: u8,
    id: u16,
    frag: Option<Frag>,
    fill: impl FnOnce(&mut [u8]) -> usize,
) -> usize {
    let n = fill(&mut buf[IP_HDR..MTU]).min(MTU - IP_HDR);
    let total = IP_HDR + n;
    buf[0] = 0x45;
    buf[1] = 0;
    put16(buf, 2, total as u16);
    put16(buf, 4, id);
    // DF on everything we send whole (TCP never needs fragments; path MTU is the link's);
    // a fragment carries its offset and MF.
    let flags = match frag {
        Some(f) => (f.off8 & 0x1fff) | if f.more { 0x2000 } else { 0 },
        None => 0x4000,
    };
    put16(buf, 6, flags);
    buf[8] = 64;
    buf[9] = proto;
    put16(buf, 10, 0);
    buf[12..16].copy_from_slice(&src.0);
    buf[16..20].copy_from_slice(&dst.0);
    let c = checksum(&buf[..IP_HDR]);
    put16(buf, 10, c);
    total
}
