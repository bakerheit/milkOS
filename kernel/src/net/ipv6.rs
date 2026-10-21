//! IPv6 (RFC 8200): addresses on every link, NDP (RFC 4861), SLAAC (RFC 4862), ICMPv6
//! (RFC 4443), extension headers. See docs/networking.md, "IPv6".
//!
//! * **Addresses.**
//!   * Every link gets a link-local `fe80::<EUI-64 of its MAC>/64`.
//!   * eth0 gets a SLAAC address from the router advertisement (slirp: `fec0::/64`, from
//!     its router `fe80::2`).
//!   * eth1, which has no router, also gets a unique-local address of its own,
//!     [`VM_LINK_PREFIX`]`::<EUI-64>/64`, so two VMs can talk without scope ids.
//!   * `::1` is loopback.
//!   * New addresses go through duplicate address detection (one NS from `::`,
//!     [`DAD_NS`] of quiet) before they are used.
//! * **NDP.** A neighbour cache per link, filled from NS/NA/RA source and target
//!   link-layer options and by our own NS to the solicited-node group. Packets waiting
//!   for an answer are parked (up to 16, 3 s). RS go out at start (3 tries, 4 s apart).
//!   An RA sets the default router (eth0), on-link prefixes and the MTU. NDP messages
//!   must come with hop limit 255 (RFC 4861 7.1).
//! * **ICMPv6.** Echo request/reply (also for `ping -6` and the self-test), destination
//!   unreachable (port, for closed UDP ports), parameter problem (bad or unknown extension
//!   headers). There are no errors about errors or about multicast.
//! * **Extension headers.**
//!   * Hop-by-hop options (first only) and destination options are parsed. Unknown
//!     options are handled by their action bits.
//!   * Routing headers: **no source routing, ever**. With segments left > 0 the packet
//!     is dropped with a parameter problem (RFC 5095 for type 0); with 0 the header is
//!     skipped.
//!   * Fragment headers: an atomic fragment (offset 0, no M) is accepted, other
//!     fragments are dropped and counted (no IPv6 reassembly).

use alloc::vec::Vec;

use super::Stack;
use super::iface::{ETH_HDR, Iface, MTU};
use super::socket::Family;
use super::wire::{
    IpAddr, Ipv6, Mac, SockAddr, be16, be32, finish, pseudo_sum6, put16, put32, sum,
};
use crate::fs::errno::Result;
use crate::kprintln;
use crate::net::errno::*;

pub const ETHERTYPE_IPV6: u16 = 0x86dd;
pub const HDR: usize = 40;
pub const NH_HOP: u8 = 0;
pub const NH_TCP: u8 = 6;
pub const NH_UDP: u8 = 17;
pub const NH_ROUTING: u8 = 43;
pub const NH_FRAG: u8 = 44;
pub const NH_ICMP6: u8 = 58;
pub const NH_NONE: u8 = 59;
pub const NH_DSTOPT: u8 = 60;

/// eth1's unique-local prefix (RFC 4193 fd00::/8): "milk" VM link, subnet 1.
pub const VM_LINK_PREFIX: [u8; 8] = [0xfd, 0x6d, 0x69, 0x6c, 0x6b, 0x00, 0x00, 0x01];
const MS: u64 = 1_000_000;
const S: u64 = 1_000_000_000;
/// Duplicate address detection: one NS, then this long without an answer.
pub const DAD_NS: u64 = 500 * MS;
const RS_INTERVAL: u64 = 4 * S;
const RS_TRIES: u8 = 3;
const ND_SLOTS: usize = 32;
const ND_RETRY: u64 = S;
const ND_TRIES: u8 = 3;
const ND_REACHABLE: u64 = 600 * S;
const PENDING_MAX: usize = 16;
const PENDING_NS: u64 = 3 * S;

/// Interface index for scope ids and /proc (Linux numbering: lo 1, eth0 2, eth1 3).
pub fn ifindex(link: Option<usize>) -> u32 {
    link.map_or(1, |k| k as u32 + 2)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    LinkLocal,
    Slaac,
    VmLink,
}

#[derive(Clone, Copy, Debug)]
pub struct Addr6 {
    pub addr: Ipv6,
    pub plen: u8,
    pub tentative: bool,
    /// DAD ends (tentative) / the address expires (SLAAC valid lifetime; u64::MAX: never).
    pub dad_until: u64,
    pub valid_until: u64,
    pub origin: Origin,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NdState {
    Incomplete,
    Reachable,
}

#[derive(Clone, Copy)]
struct NdEntry {
    ip: Ipv6,
    mac: Mac,
    state: NdState,
    updated: u64,
    last_req: u64,
    tries: u8,
    router: bool,
}

/// IPv6 state of one link.
pub struct Link6 {
    pub addrs: Vec<Addr6>,
    /// On-link prefixes from RAs: (prefix, length, valid until).
    pub prefixes: Vec<(Ipv6, u8, u64)>,
    /// Default router: (its link-local address, valid until).
    pub router: Option<(Ipv6, u64)>,
    pub mtu: u32,
    nd: Vec<NdEntry>,
    rs_sent: u8,
    rs_next: u64,
}

impl Link6 {
    pub const fn new() -> Link6 {
        Link6 {
            addrs: Vec::new(),
            prefixes: Vec::new(),
            router: None,
            mtu: MTU as u32,
            nd: Vec::new(),
            rs_sent: 0,
            rs_next: 0,
        }
    }

    /// A usable (not tentative) address of `origin`.
    pub fn addr(&self, origin: Origin) -> Option<Ipv6> {
        self.addrs
            .iter()
            .find(|a| a.origin == origin && !a.tentative)
            .map(|a| a.addr)
    }

    pub fn link_local(&self) -> Option<Ipv6> {
        self.addr(Origin::LinkLocal)
    }

    fn has(&self, ip: Ipv6, tentative_ok: bool) -> bool {
        self.addrs
            .iter()
            .any(|a| a.addr == ip && (tentative_ok || !a.tentative))
    }

    fn lookup(&self, ip: Ipv6) -> Option<Mac> {
        self.nd
            .iter()
            .find(|e| e.ip == ip && e.state == NdState::Reachable)
            .map(|e| e.mac)
    }

    fn learn(&mut self, ip: Ipv6, mac: Mac, now: u64, router: bool) {
        if let Some(e) = self.nd.iter_mut().find(|e| e.ip == ip) {
            e.mac = mac;
            e.state = NdState::Reachable;
            e.updated = now;
            e.tries = 0;
            e.router |= router;
            return;
        }
        if self.nd.len() >= ND_SLOTS
            && let Some(k) = (0..self.nd.len()).min_by_key(|&k| self.nd[k].updated)
        {
            self.nd.swap_remove(k);
        }
        self.nd.push(NdEntry {
            ip,
            mac,
            state: NdState::Reachable,
            updated: now,
            last_req: now,
            tries: 0,
            router,
        });
    }

    /// We need `ip` resolved: should an NS go out now?
    fn want(&mut self, ip: Ipv6, now: u64) -> bool {
        if let Some(e) = self.nd.iter_mut().find(|e| e.ip == ip) {
            if e.state == NdState::Reachable || now.saturating_sub(e.last_req) < ND_RETRY {
                return false;
            }
            e.last_req = now;
            e.tries = e.tries.saturating_add(1);
            return true;
        }
        if self.nd.len() >= ND_SLOTS {
            self.nd.remove(0);
        }
        self.nd.push(NdEntry {
            ip,
            mac: Mac::ZERO,
            state: NdState::Incomplete,
            updated: now,
            last_req: now,
            tries: 1,
            router: false,
        });
        true
    }

    /// Neighbour cache lines for /proc/net/ndisc: (ip, mac, reachable, router).
    pub fn neighbours(&self) -> impl Iterator<Item = (Ipv6, Mac, bool, bool)> + '_ {
        self.nd
            .iter()
            .map(|e| (e.ip, e.mac, e.state == NdState::Reachable, e.router))
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Stats6 {
    pub in_receives: u64,
    pub in_hdr_errors: u64,
    pub in_addr_errors: u64,
    pub in_unknown_protos: u64,
    pub in_discards: u64,
    pub in_delivers: u64,
    pub out_requests: u64,
    pub out_no_routes: u64,
    pub frag_dropped: u64,
    pub routing_refused: u64,
    pub icmp_in_msgs: u64,
    pub icmp_out_msgs: u64,
    pub icmp_in_echos: u64,
    pub icmp_in_echo_reps: u64,
    pub icmp_out_echo_reps: u64,
    pub icmp_out_errors: u64,
    pub nd_in: u64,
    pub nd_out: u64,
    pub dad_conflicts: u64,
}

struct Parked {
    link: usize,
    hop: Ipv6,
    pkt: Vec<u8>,
    since: u64,
}

/// Stack-wide IPv6 state (per-link state is in `Link::v6`).
pub struct State6 {
    pub stats: Stats6,
    parked: Vec<Parked>,
    /// Neighbours to solicit (queued by `send_on`).
    want_ns: Vec<(usize, Ipv6)>,
    /// Echo replies to our pings: (from, identifier, sequence, payload bytes, when).
    pub ping_replies: Vec<(Ipv6, u16, u16, usize, u64)>,
}

impl State6 {
    pub fn new() -> State6 {
        State6 {
            stats: Stats6::default(),
            parked: Vec::new(),
            want_ns: Vec::new(),
            ping_replies: Vec::new(),
        }
    }
}

// ---- addresses --------------------------------------------------------------------------------

fn add_tentative(st: &mut Stack, k: usize, addr: Ipv6, plen: u8, origin: Origin, valid: u64) {
    let now = st.now;
    let l6 = &mut st.iface.links[k].v6;
    if let Some(a) = l6.addrs.iter_mut().find(|a| a.addr == addr) {
        a.valid_until = valid; // a refreshed RA: just the lifetime
        return;
    }
    l6.addrs.push(Addr6 {
        addr,
        plen,
        tentative: true,
        dad_until: now + DAD_NS,
        valid_until: valid,
        origin,
    });
    send_ns(st, k, addr, Ipv6::UNSPECIFIED, false);
}

/// Bring IPv6 up on every link: link-local (and eth1's VM-link address) with DAD; the
/// router solicitations follow once the link-local address is usable.
pub fn start(st: &mut Stack) {
    for k in 0..st.iface.links.len() {
        let l = &st.iface.links[k];
        if !l.up() {
            continue;
        }
        let mac = l.mac;
        let ll = Ipv6::from_eui64(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0], mac);
        add_tentative(st, k, ll, 64, Origin::LinkLocal, u64::MAX);
        if k > 0 {
            let ula = Ipv6::from_eui64(&VM_LINK_PREFIX, mac);
            add_tentative(st, k, ula, 64, Origin::VmLink, u64::MAX);
        }
    }
}

/// Every address of ours, on any link (and `::1`).
pub fn is_ours(iface: &Iface, ip: Ipv6) -> bool {
    ip.is_loopback() || iface.links.iter().any(|l| l.up() && l.v6.has(ip, false))
}

/// The link that owns `ip`.
pub fn link_of(iface: &Iface, ip: Ipv6) -> Option<usize> {
    iface
        .links
        .iter()
        .position(|l| l.up() && l.v6.has(ip, false))
}

/// Which link a destination goes out of, and the next hop. `scope`: the link a scoped
/// (link-local / multicast) destination was given with (sin6_scope_id), if any.
fn route(iface: &Iface, dst: Ipv6, scope: Option<usize>) -> Option<(usize, Ipv6)> {
    let up = |k: usize| iface.links.get(k).is_some_and(|l| l.up());
    if dst.is_multicast() || dst.is_link_local() {
        if let Some(k) = scope.filter(|&k| up(k)) {
            return Some((k, dst));
        }
        if dst == Ipv6::MDNS && up(1) {
            return Some((1, dst)); // .local lives on the VM link
        }
        // A link-local neighbour we already know, else eth1 (the VM link), else eth0.
        if let Some(k) =
            (0..iface.links.len()).find(|&k| up(k) && iface.links[k].v6.lookup(dst).is_some())
        {
            return Some((k, dst));
        }
        return [1, 0].into_iter().find(|&k| up(k)).map(|k| (k, dst));
    }
    for (k, l) in iface.links.iter().enumerate() {
        if !l.up() {
            continue;
        }
        let on_link =
            l.v6.addrs
                .iter()
                .any(|a| a.origin != Origin::LinkLocal && dst.same_prefix(a.addr, a.plen))
                || l.v6.prefixes.iter().any(|&(p, n, _)| dst.same_prefix(p, n));
        if on_link {
            return Some((k, dst));
        }
    }
    iface
        .links
        .iter()
        .enumerate()
        .find_map(|(k, l)| l.v6.router.map(|(r, _)| (k, r)))
}

/// Our source address for `dst` (RFC 6724, the simple cases): ::1 for loopback, the
/// link-local for link-local or link-scope multicast, else an address on the same prefix,
/// else eth0's SLAAC address.
pub fn src_for(iface: &Iface, dst: Ipv6, scope: Option<usize>) -> Option<Ipv6> {
    if dst.is_loopback() || is_ours(iface, dst) {
        return Some(if dst.is_loopback() {
            Ipv6::LOOPBACK
        } else {
            dst
        });
    }
    let (k, _) = route(iface, dst, scope)?;
    let l6 = &iface.links[k].v6;
    if dst.is_link_local() || dst.is_link_multicast() {
        return l6.link_local();
    }
    l6.addrs
        .iter()
        .filter(|a| !a.tentative && a.origin != Origin::LinkLocal)
        .find(|a| dst.same_prefix(a.addr, a.plen))
        .or_else(|| {
            l6.addrs
                .iter()
                .find(|a| !a.tentative && a.origin != Origin::LinkLocal)
        })
        .map(|a| a.addr)
        .or_else(|| l6.link_local())
}

// ---- output -------------------------------------------------------------------------------------

fn build(buf: &mut [u8], src: Ipv6, dst: Ipv6, next: u8, hop: u8, payload_len: usize) -> usize {
    buf[0] = 0x60;
    buf[1] = 0;
    buf[2] = 0;
    buf[3] = 0;
    put16(buf, 4, payload_len as u16);
    buf[6] = next;
    buf[7] = hop;
    buf[8..24].copy_from_slice(&src.0);
    buf[24..40].copy_from_slice(&dst.0);
    HDR + payload_len
}

/// Send an IPv6 packet. `fill` writes the upper-layer payload (at most MTU - 40 bytes)
/// and returns its length; it gets the final source and destination for its checksum.
/// `src` unspecified = pick ([`src_for`]). Ok means sent or queued (waiting for NDP).
pub fn send(
    st: &mut Stack,
    src: Ipv6,
    dst: Ipv6,
    scope: Option<usize>,
    next: u8,
    fill: impl FnOnce(&mut [u8], Ipv6, Ipv6) -> usize,
) -> Result<()> {
    send_hop(st, src, dst, scope, next, 64, fill)
}

fn send_hop(
    st: &mut Stack,
    src: Ipv6,
    dst: Ipv6,
    scope: Option<usize>,
    next: u8,
    hop: u8,
    fill: impl FnOnce(&mut [u8], Ipv6, Ipv6) -> usize,
) -> Result<()> {
    let now = st.now;
    send_on(
        &mut st.iface,
        &mut st.ip6,
        now,
        src,
        dst,
        scope,
        next,
        hop,
        fill,
    )
}

/// The sending core, on the parts of the stack it needs (so TCP can call it while it
/// holds the socket table).
#[allow(clippy::too_many_arguments)]
pub fn send_on(
    iface: &mut Iface,
    ip6: &mut State6,
    now: u64,
    src: Ipv6,
    dst: Ipv6,
    scope: Option<usize>,
    next: u8,
    hop: u8,
    fill: impl FnOnce(&mut [u8], Ipv6, Ipv6) -> usize,
) -> Result<()> {
    ip6.stats.out_requests += 1;
    let src = if src.is_unspecified() {
        match src_for(iface, dst, scope) {
            Some(s) => s,
            None if next == NH_ICMP6 => Ipv6::UNSPECIFIED, // DAD probes, early RS
            None => {
                ip6.stats.out_no_routes += 1;
                return Err(EADDRNOTAVAIL);
            }
        }
    } else {
        src
    };
    if dst.is_loopback() || is_ours(iface, dst) {
        return iface.lo_enqueue(|b| {
            let n = fill(&mut b[HDR..MTU], src, dst).min(MTU - HDR);
            build(b, src, dst, next, hop, n)
        });
    }
    let Some((k, nexthop)) = route(iface, dst, scope) else {
        ip6.stats.out_no_routes += 1;
        return Err(ENETUNREACH);
    };
    let mac = if dst.is_multicast() {
        Some(dst.multicast_mac())
    } else {
        iface.links[k].v6.lookup(nexthop)
    };
    if let Some(mac) = mac {
        return iface.send_eth(k, mac, ETHERTYPE_IPV6, |b| {
            let n = fill(&mut b[HDR..MTU], src, dst).min(MTU - HDR);
            build(b, src, dst, next, hop, n)
        });
    }
    // Unresolved: park the packet and ask.
    let mut pkt = alloc::vec![0u8; MTU];
    let n = fill(&mut pkt[HDR..MTU], src, dst).min(MTU - HDR);
    let len = build(&mut pkt, src, dst, next, hop, n);
    pkt.truncate(len);
    if ip6.parked.len() >= PENDING_MAX {
        ip6.parked.remove(0);
    }
    ip6.parked.push(Parked {
        link: k,
        hop: nexthop,
        pkt,
        since: now,
    });
    if iface.links[k].v6.want(nexthop, now) {
        ip6.want_ns.push((k, nexthop));
    }
    Ok(())
}

/// NS for neighbours `send_on` found unresolved (sent from the net thread's tick or right
/// after, where the whole stack is at hand).
pub fn flush_ns(st: &mut Stack) {
    let want = core::mem::take(&mut st.ip6.want_ns);
    for (k, ip) in want {
        send_ns(st, k, ip, Ipv6::UNSPECIFIED, true);
    }
}

fn flush_parked(st: &mut Stack, k: usize, ip: Ipv6, mac: Mac) {
    let mut i = 0;
    while i < st.ip6.parked.len() {
        if st.ip6.parked[i].link == k && st.ip6.parked[i].hop == ip {
            let p = st.ip6.parked.swap_remove(i);
            let _ = st.iface.send_eth(k, mac, ETHERTYPE_IPV6, |b| {
                b[..p.pkt.len()].copy_from_slice(&p.pkt);
                p.pkt.len()
            });
        } else {
            i += 1;
        }
    }
}

/// An ICMPv6 message: `fill` writes the body after the 4-byte type/code/checksum header.
#[allow(clippy::too_many_arguments)]
pub(super) fn send_icmp(
    st: &mut Stack,
    src: Ipv6,
    dst: Ipv6,
    scope: Option<usize>,
    hop: u8,
    ty: u8,
    code: u8,
    fill: impl FnOnce(&mut [u8]) -> usize,
) -> Result<()> {
    st.ip6.stats.icmp_out_msgs += 1;
    send_hop(st, src, dst, scope, NH_ICMP6, hop, |b, s, d| {
        b[0] = ty;
        b[1] = code;
        put16(b, 2, 0);
        let n = 4 + fill(&mut b[4..]);
        let c = finish(sum(&b[..n], pseudo_sum6(s, d, NH_ICMP6, n)));
        put16(b, 2, c);
        n
    })
}

/// Neighbour solicitation for `target` on link `k`: to its solicited-node group, from
/// `src` (unspecified = our link-local, or `::` for DAD), with our MAC unless DAD.
fn send_ns(st: &mut Stack, k: usize, target: Ipv6, src: Ipv6, with_slla: bool) {
    let mac = st.iface.links[k].mac;
    let src = if src.is_unspecified() && with_slla {
        st.iface.links[k]
            .v6
            .link_local()
            .unwrap_or(Ipv6::UNSPECIFIED)
    } else {
        src
    };
    let with_slla = with_slla && !src.is_unspecified();
    st.ip6.stats.nd_out += 1;
    let _ = send_icmp(
        st,
        src,
        target.solicited_node(),
        Some(k),
        255,
        135,
        0,
        |b| {
            put32(b, 0, 0);
            b[4..20].copy_from_slice(&target.0);
            // Reserved (4) + target (16), then the 8-byte source link-layer option.
            if with_slla {
                b[20] = 1;
                b[21] = 1;
                b[22..28].copy_from_slice(&mac.0);
                28
            } else {
                20
            }
        },
    );
}

fn send_na(st: &mut Stack, k: usize, target: Ipv6, to: Ipv6, solicited: bool) {
    let mac = st.iface.links[k].mac;
    st.ip6.stats.nd_out += 1;
    let flags: u32 = if solicited { 0x6000_0000 } else { 0x2000_0000 }; // S, O
    let _ = send_icmp(st, target, to, Some(k), 255, 136, 0, |b| {
        put32(b, 0, flags);
        b[4..20].copy_from_slice(&target.0);
        b[20] = 2; // target link-layer address (8 bytes)
        b[21] = 1;
        b[22..28].copy_from_slice(&mac.0);
        28
    });
}

fn send_rs(st: &mut Stack, k: usize) {
    let mac = st.iface.links[k].mac;
    let src = st.iface.links[k]
        .v6
        .link_local()
        .unwrap_or(Ipv6::UNSPECIFIED);
    st.ip6.stats.nd_out += 1;
    let _ = send_icmp(st, src, Ipv6::ALL_ROUTERS, Some(k), 255, 133, 0, |b| {
        put32(b, 0, 0);
        if src.is_unspecified() {
            4
        } else {
            b[4] = 1;
            b[5] = 1;
            b[6..12].copy_from_slice(&mac.0);
            12
        }
    });
}

/// An ICMPv6 error about `pkt` (the offending packet, IPv6 header first) back to its
/// source: never about an ICMPv6 error, a multicast destination or an unspecified source.
pub fn send_error(st: &mut Stack, link: Option<usize>, ty: u8, code: u8, pointer: u32, pkt: &[u8]) {
    if pkt.len() < HDR {
        return;
    }
    let src = Ipv6(pkt[8..24].try_into().unwrap());
    let dst = Ipv6(pkt[24..40].try_into().unwrap());
    if dst.is_multicast() || src.is_unspecified() || src.is_multicast() {
        return;
    }
    let _ = &st.iface;
    if pkt[6] == NH_ICMP6 && pkt.len() > HDR && pkt[HDR] < 128 {
        return; // an error about an error
    }
    st.ip6.stats.icmp_out_errors += 1;
    let take = pkt.len().min(1280 - HDR - 8);
    let _ = send_icmp(st, dst, src, link, 64, ty, code, |b| {
        put32(b, 0, pointer);
        b[4..4 + take].copy_from_slice(&pkt[..take]);
        4 + take
    });
}

// ---- input -------------------------------------------------------------------------------------

/// An IPv6 packet from link `k` (None: loopback).
pub fn input(st: &mut Stack, link: Option<usize>, pkt: &[u8], src_mac: Mac) {
    st.ip6.stats.in_receives += 1;
    if pkt.len() < HDR || pkt[0] >> 4 != 6 {
        st.ip6.stats.in_hdr_errors += 1;
        return;
    }
    let plen = be16(pkt, 4) as usize;
    if HDR + plen > pkt.len() {
        st.ip6.stats.in_hdr_errors += 1;
        return;
    }
    let pkt = &pkt[..HDR + plen];
    let hop = pkt[7];
    let src = Ipv6(pkt[8..24].try_into().unwrap());
    let dst = Ipv6(pkt[24..40].try_into().unwrap());
    if src.is_multicast() {
        st.ip6.stats.in_addr_errors += 1;
        return;
    }
    // Ours: a unicast address (tentative ones only for NDP), a link-scope group, loopback.
    let ours = link.is_none()
        || is_ours(&st.iface, dst)
        || dst.is_link_multicast()
        || st.iface.links.iter().any(|l| l.v6.has(dst, true));
    if !ours {
        st.ip6.stats.in_addr_errors += 1;
        return;
    }
    // Extension headers.
    let mut next = pkt[6];
    let mut off = HDR;
    let mut first = true;
    loop {
        match next {
            NH_HOP | NH_DSTOPT => {
                if next == NH_HOP && !first {
                    st.ip6.stats.in_hdr_errors += 1;
                    send_error(st, link, 4, 1, off as u32, pkt);
                    return;
                }
                if off + 2 > pkt.len() {
                    st.ip6.stats.in_hdr_errors += 1;
                    return;
                }
                let len = (pkt[off + 1] as usize + 1) * 8;
                if off + len > pkt.len() {
                    st.ip6.stats.in_hdr_errors += 1;
                    return;
                }
                // Options: Pad1, PadN; anything else by its action bits.
                let mut o = off + 2;
                while o < off + len {
                    let t = pkt[o];
                    if t == 0 {
                        o += 1;
                        continue;
                    }
                    if o + 2 > off + len {
                        break;
                    }
                    let olen = pkt[o + 1] as usize;
                    if t != 1 {
                        match t >> 6 {
                            0 => {}
                            1 => {
                                st.ip6.stats.in_discards += 1;
                                return;
                            }
                            _ => {
                                st.ip6.stats.in_discards += 1;
                                if t >> 6 == 2 || !dst.is_multicast() {
                                    send_error(st, link, 4, 2, o as u32, pkt);
                                }
                                return;
                            }
                        }
                    }
                    o += 2 + olen;
                }
                next = pkt[off];
                off += len;
            }
            NH_ROUTING => {
                if off + 4 > pkt.len() {
                    st.ip6.stats.in_hdr_errors += 1;
                    return;
                }
                let segments_left = pkt[off + 3];
                if segments_left > 0 {
                    // No source routing, ever (RFC 5095 for type 0; we route for no one).
                    st.ip6.stats.routing_refused += 1;
                    send_error(st, link, 4, 0, (off + 2) as u32, pkt);
                    return;
                }
                let len = (pkt[off + 1] as usize + 1) * 8;
                next = pkt[off];
                off += len;
            }
            NH_FRAG => {
                if off + 8 > pkt.len() {
                    st.ip6.stats.in_hdr_errors += 1;
                    return;
                }
                let fo = be16(pkt, off + 2);
                if fo & 0xfff9 != 0 {
                    // A real fragment (offset or M set): no IPv6 reassembly.
                    st.ip6.stats.frag_dropped += 1;
                    return;
                }
                next = pkt[off]; // atomic fragment (RFC 6946): just skip the header
                off += 8;
            }
            _ => break,
        }
        first = false;
        if off > pkt.len() {
            st.ip6.stats.in_hdr_errors += 1;
            return;
        }
    }
    let payload = &pkt[off..];
    st.ip6.stats.in_delivers += 1;
    match next {
        NH_ICMP6 => icmp_input(st, link, src, dst, hop, payload, pkt, src_mac),
        NH_UDP => super::udp::input6(st, src, dst, payload, pkt, link),
        NH_TCP => super::tcp::input6(st, src, dst, payload, link),
        NH_NONE => {}
        _ => {
            st.ip6.stats.in_unknown_protos += 1;
            send_error(st, link, 4, 1, 6, pkt);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn icmp_input(
    st: &mut Stack,
    link: Option<usize>,
    src: Ipv6,
    dst: Ipv6,
    hop: u8,
    p: &[u8],
    pkt: &[u8],
    src_mac: Mac,
) {
    st.ip6.stats.icmp_in_msgs += 1;
    if p.len() < 4 || finish(sum(p, pseudo_sum6(src, dst, NH_ICMP6, p.len()))) != 0 {
        return;
    }
    let now = st.now;
    match p[0] {
        128 => {
            // Echo request: answer from the address it was sent to (or ours on the link).
            st.ip6.stats.icmp_in_echos += 1;
            st.ip6.stats.icmp_out_echo_reps += 1;
            let from = if dst.is_multicast() {
                Ipv6::UNSPECIFIED
            } else {
                dst
            };
            let body = &p[4..];
            let _ = send_icmp(st, from, src, link, 64, 129, 0, |b| {
                let n = body.len().min(b.len());
                b[..n].copy_from_slice(&body[..n]);
                n
            });
        }
        129 if p.len() >= 8 => {
            st.ip6.stats.icmp_in_echo_reps += 1;
            // A ping socket's reply (AF_INET6, IPPROTO_ICMPV6)?
            let id = be16(p, 4);
            let target = st.socks.iter().find(|(_, s)| {
                s.icmp
                    && s.family == Family::V6
                    && s.owns_port
                    && s.local.port == id
                    && (!s.udp_connected || s.remote.ip == IpAddr::V6(src))
            });
            if let Some((i, _)) = target {
                let scope = link.filter(|_| src.is_link_local());
                super::udp::queue(st, i, SockAddr::new(src, 0), scope, p);
                return;
            }
            let r = &mut st.ip6.ping_replies;
            if r.len() >= 32 {
                r.remove(0);
            }
            r.push((src, be16(p, 4), be16(p, 6), p.len() - 8, now));
            st.wake_any = true;
        }
        1..=4 => {}
        133..=137 if hop == 255 => {
            st.ip6.stats.nd_in += 1;
            if let Some(k) = link {
                nd_input(st, k, src, dst, p, src_mac);
            }
        }
        _ => {}
    }
    let _ = pkt;
}

/// A prefix information option: (prefix, length, flags, valid lifetime).
type PrefixInfo = (Ipv6, u8, u8, u32);

/// The options of an ND message from offset `o`: (source link-layer address, target
/// link-layer address, MTU, prefix infos).
fn nd_opts(p: &[u8], mut o: usize) -> (Option<Mac>, Option<Mac>, Option<u32>, Vec<PrefixInfo>) {
    let (mut slla, mut tlla, mut mtu, mut pfx) = (None, None, None, Vec::new());
    while o + 2 <= p.len() {
        let (t, len) = (p[o], p[o + 1] as usize * 8);
        if len == 0 || o + len > p.len() {
            break;
        }
        match (t, len) {
            (1, 8) => slla = Some(Mac(p[o + 2..o + 8].try_into().unwrap())),
            (2, 8) => tlla = Some(Mac(p[o + 2..o + 8].try_into().unwrap())),
            (5, 8) => mtu = Some(be32(p, o + 4)),
            (3, 32) => pfx.push((
                Ipv6(p[o + 16..o + 32].try_into().unwrap()),
                p[o + 2],
                p[o + 3],
                be32(p, o + 4),
            )),
            _ => {}
        }
        o += len;
    }
    (slla, tlla, mtu, pfx)
}

fn nd_input(st: &mut Stack, k: usize, src: Ipv6, dst: Ipv6, p: &[u8], src_mac: Mac) {
    let now = st.now;
    match p[0] {
        135 if p.len() >= 24 => {
            // Neighbour solicitation.
            let target = Ipv6(p[8..24].try_into().unwrap());
            let (slla, ..) = nd_opts(p, 24);
            let l6 = &mut st.iface.links[k].v6;
            let tentative = l6.addrs.iter().any(|a| a.addr == target && a.tentative);
            if tentative {
                if src.is_unspecified() && src_mac != st.iface.links[k].mac {
                    dad_failed(st, k, target, "another host probes it too");
                }
                return;
            }
            if !l6.has(target, false) {
                return;
            }
            if let Some(m) = slla
                && !src.is_unspecified()
            {
                l6.learn(src, m, now, false);
                flush_parked(st, k, src, m);
            }
            let (to, solicited) = if src.is_unspecified() {
                (Ipv6::ALL_NODES, false)
            } else {
                (src, true)
            };
            send_na(st, k, target, to, solicited);
        }
        136 if p.len() >= 24 => {
            // Neighbour advertisement.
            let target = Ipv6(p[8..24].try_into().unwrap());
            let (_, tlla, ..) = nd_opts(p, 24);
            let router = p[4] & 0x80 != 0;
            let l6 = &st.iface.links[k].v6;
            if l6.addrs.iter().any(|a| a.addr == target) && src_mac != st.iface.links[k].mac {
                dad_failed(st, k, target, "a neighbour advertises it");
                return;
            }
            if let Some(m) = tlla.or((!dst.is_multicast()).then_some(src_mac)) {
                st.iface.links[k].v6.learn(target, m, now, router);
                flush_parked(st, k, target, m);
            }
        }
        134 if p.len() >= 16 && src.is_link_local() => {
            // Router advertisement.
            let lifetime = be16(p, 6) as u64;
            let (slla, _, mtu, pfx) = nd_opts(p, 16);
            if let Some(m) = slla {
                st.iface.links[k].v6.learn(src, m, now, true);
                flush_parked(st, k, src, m);
            }
            let first = st.iface.links[k].v6.router.is_none() && lifetime > 0;
            {
                let l6 = &mut st.iface.links[k].v6;
                l6.router = (lifetime > 0).then_some((src, now + lifetime * S));
                if let Some(m) = mtu {
                    l6.mtu = m.clamp(1280, MTU as u32);
                }
                l6.rs_sent = RS_TRIES; // answered: no more solicitations
            }
            if first {
                kprintln!(
                    "[net] {}: IPv6 router {src} (lifetime {lifetime} s)",
                    super::iface::LINK_NAMES[k]
                );
            }
            let mac = st.iface.links[k].mac;
            for (prefix, plen, flags, valid) in pfx {
                let until = if valid == u32::MAX {
                    u64::MAX
                } else {
                    now + valid as u64 * S
                };
                if flags & 0x80 != 0 {
                    let l6 = &mut st.iface.links[k].v6;
                    l6.prefixes.retain(|&(p, n, _)| !(p == prefix && n == plen));
                    if valid > 0 {
                        l6.prefixes.push((prefix, plen, until));
                    }
                }
                // SLAAC: autonomous flag, a /64, not link-local.
                if flags & 0x40 != 0 && plen == 64 && !prefix.is_link_local() && valid > 0 {
                    let mut p8 = [0u8; 8];
                    p8.copy_from_slice(&prefix.0[..8]);
                    let addr = Ipv6::from_eui64(&p8, mac);
                    add_tentative(st, k, addr, 64, Origin::Slaac, until);
                }
            }
        }
        _ => {}
    }
}

fn dad_failed(st: &mut Stack, k: usize, addr: Ipv6, why: &str) {
    st.ip6.stats.dad_conflicts += 1;
    st.iface.links[k].v6.addrs.retain(|a| a.addr != addr);
    kprintln!(
        "[net] {}: IPv6 address {addr} is a duplicate ({why}); not using it",
        super::iface::LINK_NAMES[k]
    );
}

// ---- timers --------------------------------------------------------------------------------------

pub fn tick(st: &mut Stack) {
    let now = st.now;
    for k in 0..st.iface.links.len() {
        if !st.iface.links[k].up() {
            continue;
        }
        // DAD done: the address is ours.
        let mut ready = Vec::new();
        for a in st.iface.links[k].v6.addrs.iter_mut() {
            if a.tentative && now >= a.dad_until {
                a.tentative = false;
                ready.push(*a);
            }
        }
        for a in ready {
            kprintln!(
                "[net] {}: IPv6 {}/{} ({})",
                super::iface::LINK_NAMES[k],
                a.addr,
                a.plen,
                match a.origin {
                    Origin::LinkLocal => "link-local",
                    Origin::Slaac => "SLAAC",
                    Origin::VmLink => "VM link",
                }
            );
            if a.origin == Origin::LinkLocal {
                st.iface.links[k].v6.rs_next = now; // now we can ask for routers
            }
        }
        let l6 = &mut st.iface.links[k].v6;
        l6.addrs.retain(|a| a.valid_until > now);
        l6.prefixes.retain(|&(_, _, u)| u > now);
        if l6.router.is_some_and(|(_, u)| u <= now) {
            l6.router = None;
        }
        // Router solicitation (eth0 only: the VM link has no routers).
        if k == 0
            && l6.rs_sent < RS_TRIES
            && l6.rs_next != 0
            && now >= l6.rs_next
            && l6.router.is_none()
        {
            l6.rs_sent += 1;
            l6.rs_next = now + RS_INTERVAL;
            send_rs(st, k);
        }
        // Neighbour cache: retries and expiry.
        let mut ask = Vec::new();
        let l6 = &mut st.iface.links[k].v6;
        l6.nd.retain(|e| match e.state {
            NdState::Reachable => now.saturating_sub(e.updated) < ND_REACHABLE,
            NdState::Incomplete => e.tries < ND_TRIES || now.saturating_sub(e.last_req) < ND_RETRY,
        });
        for e in l6.nd.iter_mut() {
            if e.state == NdState::Incomplete && now.saturating_sub(e.last_req) >= ND_RETRY {
                e.last_req = now;
                e.tries += 1;
                ask.push(e.ip);
            }
        }
        for ip in ask {
            send_ns(st, k, ip, Ipv6::UNSPECIFIED, true);
        }
    }
    st.ip6
        .parked
        .retain(|p| now.saturating_sub(p.since) < PENDING_NS);
}

/// The next IPv6 timer (DAD, RS, ND retries), for the net thread.
pub fn next_deadline(st: &Stack) -> Option<u64> {
    let mut d: Option<u64> = None;
    let mut min = |t: u64| d = Some(d.map_or(t, |x: u64| x.min(t)));
    for l in st.iface.links.iter().filter(|l| l.up()) {
        for a in l.v6.addrs.iter().filter(|a| a.tentative) {
            min(a.dad_until);
        }
        if l.v6.rs_sent < RS_TRIES && l.v6.rs_next != 0 && l.v6.router.is_none() {
            min(l.v6.rs_next);
        }
        if l.v6.nd.iter().any(|e| e.state == NdState::Incomplete) {
            min(st.now + ND_RETRY / 4);
        }
    }
    if !st.ip6.parked.is_empty() {
        min(st.now + 100 * MS);
    }
    d
}

/// Send one echo request (kernel callers and the self-test): identifier `id`, seq `seq`.
pub fn ping(st: &mut Stack, dst: Ipv6, id: u16, seq: u16, payload: &[u8]) -> Result<()> {
    send_icmp(st, Ipv6::UNSPECIFIED, dst, None, 64, 128, 0, |b| {
        put16(b, 0, id);
        put16(b, 2, seq);
        let n = payload.len().min(b.len() - 4);
        b[4..4 + n].copy_from_slice(&payload[..n]);
        4 + n
    })
}

/// Bytes of Ethernet + IPv6 header, for MSS math.
pub const OVERHEAD: usize = ETH_HDR + HDR;
