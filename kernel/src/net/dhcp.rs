//! DHCP client (RFC 2131): DISCOVER → OFFER → REQUEST → ACK, renewal at T1, and back to
//! the start when the lease runs out or the server NAKs. Runs from the net thread's timer
//! pass; nothing blocks. With qemu user networking the server is slirp (10.0.2.2) and the
//! lease is 10.0.2.15/24, router 10.0.2.2, DNS 10.0.2.3.

use super::Stack;
use super::udp;
use super::wire::{Ipv4, SockAddr, be32, ip_at, put16, put32};
use crate::timer;

pub const SERVER_PORT: u16 = 67;
pub const CLIENT_PORT: u16 = 68;

const S: u64 = 1_000_000_000;
const MAGIC: u32 = 0x6382_5363;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DhcpState {
    Off,
    Selecting,
    Requesting,
    Bound,
    Renewing,
}

pub struct Dhcp {
    pub state: DhcpState,
    xid: u32,
    offered: Ipv4,
    pub server: Ipv4,
    next_tx: u64,
    tries: u32,
    pub lease_s: u32,
    pub bound_at: u64,
    t1: u64,
    expires: u64,
    /// Number of times a lease was obtained (selftests wait for this to change).
    pub binds: u32,
}

impl Dhcp {
    pub const fn new() -> Dhcp {
        Dhcp {
            state: DhcpState::Off,
            xid: 0,
            offered: Ipv4::UNSPECIFIED,
            server: Ipv4::UNSPECIFIED,
            next_tx: 0,
            tries: 0,
            lease_s: 0,
            bound_at: 0,
            t1: 0,
            expires: 0,
            binds: 0,
        }
    }

    /// When the timer pass next has work for us.
    pub fn next_deadline(&self) -> Option<u64> {
        match self.state {
            DhcpState::Off => None,
            DhcpState::Bound => Some(self.t1),
            DhcpState::Renewing => Some(self.next_tx.min(self.expires)),
            _ => Some(self.next_tx),
        }
    }
}

fn new_xid() -> u32 {
    (timer::counter() as u32).wrapping_mul(2_654_435_761) ^ 0x6d69_6c6b
}

/// Start (or restart) address acquisition on eth0.
pub fn start(st: &mut Stack) {
    let d = &mut st.dhcp;
    d.state = DhcpState::Selecting;
    d.xid = new_xid();
    d.tries = 0;
    d.next_tx = st.now;
    tick(st);
}

/// Retransmission backoff: 1, 2, 4, 8, 8, ... seconds (RFC 2131 suggests 4, 8, 16, ..;
/// shorter so a late server is picked up quickly at boot).
fn backoff(tries: u32) -> u64 {
    S << tries.min(3)
}

pub fn tick(st: &mut Stack) {
    let now = st.now;
    match st.dhcp.state {
        DhcpState::Off => {}
        DhcpState::Selecting | DhcpState::Requesting if now >= st.dhcp.next_tx => {
            if st.dhcp.state == DhcpState::Requesting && st.dhcp.tries >= 4 {
                st.dhcp.state = DhcpState::Selecting;
                st.dhcp.tries = 0;
            }
            let req = st.dhcp.state == DhcpState::Requesting;
            send(st, if req { REQUEST } else { DISCOVER });
            let d = &mut st.dhcp;
            d.next_tx = now + backoff(d.tries);
            d.tries += 1;
        }
        DhcpState::Bound if now >= st.dhcp.t1 => {
            st.dhcp.state = DhcpState::Renewing;
            st.dhcp.tries = 0;
            st.dhcp.next_tx = now;
            tick(st);
        }
        DhcpState::Renewing => {
            if now >= st.dhcp.expires {
                // Lease gone: drop the address and start over.
                st.iface.eth0_mut().cfg = Default::default();
                start(st);
            } else if now >= st.dhcp.next_tx {
                send(st, REQUEST);
                st.dhcp.next_tx = now + 10 * S;
            }
        }
        _ => {}
    }
}

fn send(st: &mut Stack, kind: u8) {
    let d = &st.dhcp;
    let renewing = d.state == DhcpState::Renewing;
    let mac = st.iface.eth0().mac;
    let ciaddr = if renewing {
        st.iface.eth0_mut().cfg.addr
    } else {
        Ipv4::UNSPECIFIED
    };
    let mut m = [0u8; 300];
    m[0] = 1; // BOOTREQUEST
    m[1] = 1; // Ethernet
    m[2] = 6;
    put32(&mut m, 4, d.xid);
    put16(&mut m, 10, 0x8000); // ask for broadcast replies (we have no address yet)
    m[12..16].copy_from_slice(&ciaddr.0);
    m[28..34].copy_from_slice(&mac.0);
    put32(&mut m, 236, MAGIC);
    let mut k = 240;
    let mut opt = |m: &mut [u8; 300], code: u8, val: &[u8]| {
        m[k] = code;
        m[k + 1] = val.len() as u8;
        m[k + 2..k + 2 + val.len()].copy_from_slice(val);
        k += 2 + val.len();
    };
    opt(&mut m, 53, &[kind]);
    let mut cid = [1u8; 7];
    cid[1..].copy_from_slice(&mac.0);
    opt(&mut m, 61, &cid);
    if kind == REQUEST && !renewing {
        opt(&mut m, 50, &d.offered.0);
        opt(&mut m, 54, &d.server.0);
    }
    opt(&mut m, 12, b"milkos");
    opt(&mut m, 55, &[1, 3, 6, 15, 51, 54, 58]);
    m[k] = 255;
    let src = SockAddr::new(ciaddr, CLIENT_PORT);
    let dst = SockAddr::new(Ipv4::BROADCAST, SERVER_PORT);
    let _ = udp::send_raw(st, src, dst, &m);
}

struct Reply {
    kind: u8,
    yiaddr: Ipv4,
    mask: Option<Ipv4>,
    router: Option<Ipv4>,
    dns: Option<Ipv4>,
    lease: Option<u32>,
    t1: Option<u32>,
    server: Option<Ipv4>,
}

fn parse(m: &[u8], xid: u32, mac: &[u8; 6]) -> Option<Reply> {
    if m.len() < 240 || m[0] != 2 || be32(m, 4) != xid || &m[28..34] != mac {
        return None;
    }
    if be32(m, 236) != MAGIC {
        return None;
    }
    let mut r = Reply {
        kind: 0,
        yiaddr: ip_at(m, 16),
        mask: None,
        router: None,
        dns: None,
        lease: None,
        t1: None,
        server: None,
    };
    let mut k = 240;
    while k < m.len() {
        let code = m[k];
        if code == 255 {
            break;
        }
        if code == 0 {
            k += 1;
            continue;
        }
        let len = *m.get(k + 1)? as usize;
        let v = m.get(k + 2..k + 2 + len)?;
        match (code, len) {
            (53, 1) => r.kind = v[0],
            (1, 4) => r.mask = Some(ip_at(v, 0)),
            (3, l) if l >= 4 => r.router = Some(ip_at(v, 0)),
            (6, l) if l >= 4 => r.dns = Some(ip_at(v, 0)),
            (51, 4) => r.lease = Some(be32(v, 0)),
            (58, 4) => r.t1 = Some(be32(v, 0)),
            (54, 4) => r.server = Some(ip_at(v, 0)),
            _ => {}
        }
        k += 2 + len;
    }
    Some(r)
}

/// A datagram for UDP port 68.
pub fn input(st: &mut Stack, m: &[u8]) {
    let mac = st.iface.eth0().mac.0;
    let Some(r) = parse(m, st.dhcp.xid, &mac) else {
        return;
    };
    let now = st.now;
    match (st.dhcp.state, r.kind) {
        (DhcpState::Selecting, OFFER) => {
            let Some(server) = r.server else { return };
            let d = &mut st.dhcp;
            d.offered = r.yiaddr;
            d.server = server;
            d.state = DhcpState::Requesting;
            d.tries = 0;
            d.next_tx = now;
            tick(st);
        }
        (DhcpState::Requesting | DhcpState::Renewing, ACK) => {
            let lease = r.lease.unwrap_or(3600).max(60);
            let cfg = &mut st.iface.eth0_mut().cfg;
            cfg.addr = r.yiaddr;
            cfg.mask = r.mask.unwrap_or(Ipv4::new(255, 255, 255, 0));
            cfg.gateway = r.router;
            cfg.dns = r.dns;
            let d = &mut st.dhcp;
            if let Some(s) = r.server {
                d.server = s;
            }
            d.state = DhcpState::Bound;
            d.lease_s = lease;
            d.bound_at = now;
            d.t1 = now + r.t1.unwrap_or(lease / 2) as u64 * S;
            d.expires = now + lease as u64 * S;
            d.binds += 1;
            // Announce ourselves so the gateway (and we) have each other in ARP caches.
            if let Some(gw) = r.router
                && st.iface.eth0_mut().arp.want(gw, now)
            {
                st.iface
                    .send_arp(0, super::arp::OP_REQUEST, super::wire::Mac::ZERO, gw);
            }
        }
        (DhcpState::Requesting | DhcpState::Renewing, NAK) => {
            st.iface.eth0_mut().cfg = Default::default();
            start(st);
        }
        _ => {}
    }
}
