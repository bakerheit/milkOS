//! Multicast Listener Discovery v2 (RFC 3810), the host side, and the group membership it
//! reports. It decides which IPv6 multicast a link takes at all.
//!
//! * A link listens to all-nodes (`ff02::1`, never reported), the solicited-node group of
//!   each of its addresses (tentative ones too, so DAD works), and the groups sockets
//!   joined with IPV6_JOIN_GROUP (`ff02::fb` for mDNS and Milk Drop discovery on eth1).
//!   Anything else addressed to a multicast group is dropped, at the Ethernet filter
//!   (`33:33:<group>`) and again at IPv6 input.
//! * When the set changes, a state-change report (CHANGE_TO_EXCLUDE {} to join,
//!   CHANGE_TO_INCLUDE {} to leave) goes to `ff02::16`, twice, 1 s apart (the robustness
//!   variable); from the link-local address, hop limit 1, with a router alert
//!   (RFC 2711) in a hop-by-hop header. Reports wait for the link-local address to finish
//!   DAD.
//! * A query (type 130, MLDv1 or v2, from a link-local address with hop limit 1) is
//!   answered with one current-state report (MODE_IS_EXCLUDE {} per group) after a random
//!   delay within its maximum response time (capped at 1 s).
//!
//! No source filtering (every group is "exclude nothing"), no MLDv1 compatibility mode:
//! our links have no multicast router; this keeps snooping switches and any future router
//! honest about what we listen to.

use alloc::vec::Vec;

use super::Stack;
use super::ipv6::{self, NH_HOP, NH_ICMP6};
use super::wire::{Ipv6, Mac, be16, finish, pseudo_sum6, put16};

/// ff02::16, all MLDv2-capable routers.
pub const ALL_MLDV2_ROUTERS: Ipv6 = Ipv6([0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);
const ROBUSTNESS: u8 = 2;
const UNSOLICITED_INTERVAL: u64 = 1_000_000_000;
const MAX_QUERY_DELAY: u64 = 1_000_000_000;
const TO_EXCLUDE: u8 = 4;
const TO_INCLUDE: u8 = 3;
const IS_EXCLUDE: u8 = 2;

/// A state change still being reported.
#[derive(Clone, Copy)]
struct Change {
    group: Ipv6,
    join: bool,
    left: u8,
    next: u64,
}

/// One link's memberships and MLD state.
pub struct LinkMld {
    /// Groups sockets joined, with how many joined each.
    joined: Vec<(Ipv6, u16)>,
    /// What the last reports said we listen to (all-nodes excluded).
    reported: Vec<Ipv6>,
    changes: Vec<Change>,
    /// A query asked: answer at this time.
    query_due: Option<u64>,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct MldStats {
    pub queries_in: u64,
    pub reports_out: u64,
}

impl LinkMld {
    pub const fn new() -> LinkMld {
        LinkMld {
            joined: Vec::new(),
            reported: Vec::new(),
            changes: Vec::new(),
            query_due: None,
        }
    }

    pub fn join(&mut self, g: Ipv6) {
        match self.joined.iter_mut().find(|(a, _)| *a == g) {
            Some((_, n)) => *n += 1,
            None => self.joined.push((g, 1)),
        }
    }

    pub fn leave(&mut self, g: Ipv6) {
        if let Some(k) = self.joined.iter().position(|(a, _)| *a == g) {
            self.joined[k].1 -= 1;
            if self.joined[k].1 == 0 {
                self.joined.swap_remove(k);
            }
        }
    }

    /// Socket joins (group, users), for /proc/net/igmp6.
    pub fn joined(&self) -> &[(Ipv6, u16)] {
        &self.joined
    }
}

/// Every group link `k` listens to, all-nodes first.
pub fn groups(l6: &ipv6::Link6) -> Vec<Ipv6> {
    let mut g = Vec::from([Ipv6::ALL_NODES]);
    for a in &l6.addrs {
        let s = a.addr.solicited_node();
        if !g.contains(&s) {
            g.push(s);
        }
    }
    for &(j, _) in l6.mld.joined.iter() {
        if !g.contains(&j) {
            g.push(j);
        }
    }
    g
}

/// Does the link take IPv6 multicast to `g`?
pub fn member(l6: &ipv6::Link6, g: Ipv6) -> bool {
    g == Ipv6::ALL_NODES
        || l6.addrs.iter().any(|a| a.addr.solicited_node() == g)
        || l6.mld.joined.iter().any(|&(j, _)| j == g)
}

/// Does the link take Ethernet frames to the IPv6 multicast MAC `m` (`33:33:...`)?
pub fn member_mac(l6: &ipv6::Link6, m: Mac) -> bool {
    groups(l6).iter().any(|g| g.multicast_mac() == m)
}

/// An MLDv2 report on link `k` with these records (record type, group).
fn send_report(st: &mut Stack, k: usize, records: &[(u8, Ipv6)]) {
    let Some(src) = st.iface.links[k].v6.link_local() else {
        return;
    };
    if records.is_empty() {
        return;
    }
    st.ip6.mld.reports_out += 1;
    let n = records.len().min(60); // (1280 - 40 - 8 - 8) / 20
    let _ = ipv6::send_raw(st, src, ALL_MLDV2_ROUTERS, Some(k), NH_HOP, 1, |b, s, d| {
        // Hop-by-hop: next = ICMPv6, length 0 (8 bytes), router alert (MLD), PadN.
        b[..8].copy_from_slice(&[NH_ICMP6, 0, 5, 2, 0, 0, 1, 0]);
        let m = &mut b[8..];
        let len = 8 + 20 * n;
        m[..8].copy_from_slice(&[143, 0, 0, 0, 0, 0, 0, 0]);
        put16(m, 6, n as u16);
        for (i, &(ty, g)) in records.iter().take(n).enumerate() {
            let r = &mut m[8 + 20 * i..8 + 20 * (i + 1)];
            r[..4].copy_from_slice(&[ty, 0, 0, 0]);
            r[4..].copy_from_slice(&g.0);
        }
        let c = finish(super::wire::sum(
            &m[..len],
            pseudo_sum6(s, d, NH_ICMP6, len),
        ));
        put16(m, 2, c);
        8 + len
    });
}

/// A query arrived on link `k` (`p`: the ICMPv6 message, hop limit and source checked).
pub fn query(st: &mut Stack, k: usize, p: &[u8]) {
    if p.len() < 24 {
        return;
    }
    st.ip6.mld.queries_in += 1;
    // Maximum response: v1 in ms; v2 a code (exact below 32768 ms).
    let max_ms = be16(p, 4) as u64;
    let max = (max_ms * 1_000_000).min(MAX_QUERY_DELAY);
    let jitter = st.now.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40; // cheap, not secret
    let due = st.now + if max == 0 { 0 } else { jitter % max };
    let l = &mut st.iface.links[k].v6.mld;
    l.query_due = Some(l.query_due.map_or(due, |d| d.min(due)));
}

/// Report changes and answer queries (from `ipv6::tick`).
pub fn tick(st: &mut Stack, k: usize) {
    let now = st.now;
    if st.iface.links[k].v6.link_local().is_none() {
        return; // reports need the link-local source; they wait for DAD
    }
    // What changed since the last report: state changes, ROBUSTNESS times each.
    let now_groups: Vec<Ipv6> = groups(&st.iface.links[k].v6)
        .into_iter()
        .filter(|&g| g != Ipv6::ALL_NODES)
        .collect();
    {
        let l = &mut st.iface.links[k].v6.mld;
        for &g in now_groups.iter().filter(|g| !l.reported.contains(g)) {
            l.changes.retain(|c| c.group != g);
            l.changes.push(Change {
                group: g,
                join: true,
                left: ROBUSTNESS,
                next: now,
            });
        }
        for &g in l
            .reported
            .clone()
            .iter()
            .filter(|g| !now_groups.contains(g))
        {
            l.changes.retain(|c| c.group != g);
            l.changes.push(Change {
                group: g,
                join: false,
                left: ROBUSTNESS,
                next: now,
            });
        }
        l.reported = now_groups.clone();
    }
    let due: Vec<(u8, Ipv6)> = {
        let l = &mut st.iface.links[k].v6.mld;
        let mut v = Vec::new();
        for c in l.changes.iter_mut().filter(|c| c.next <= now) {
            v.push((if c.join { TO_EXCLUDE } else { TO_INCLUDE }, c.group));
            c.left -= 1;
            c.next = now + UNSOLICITED_INTERVAL;
        }
        l.changes.retain(|c| c.left > 0);
        v
    };
    send_report(st, k, &due);
    let l = &mut st.iface.links[k].v6.mld;
    if l.query_due.is_some_and(|d| d <= now) {
        l.query_due = None;
        let recs: Vec<(u8, Ipv6)> = now_groups.iter().map(|&g| (IS_EXCLUDE, g)).collect();
        send_report(st, k, &recs);
    }
}

/// The next MLD timer on link `k`, if any.
pub fn next_deadline(l6: &ipv6::Link6, now: u64) -> Option<u64> {
    let l = &l6.mld;
    let pending = groups(l6).len() - 1 != l.reported.len();
    l.changes
        .iter()
        .map(|c| c.next)
        .chain(l.query_due)
        .chain(pending.then_some(now + 50_000_000))
        .min()
}
