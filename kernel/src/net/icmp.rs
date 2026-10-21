//! ICMP (RFC 792): answer echo requests, send our own pings, and emit destination
//! unreachable for closed UDP ports / unknown protocols.

use super::Stack;
use super::iface::PROTO_ICMP;
use super::wire::{Ipv4, be16, checksum, put16};
use crate::fs::errno::Result;
use crate::timer;

const ECHO_REPLY: u8 = 0;
const DEST_UNREACH: u8 = 3;
const ECHO_REQUEST: u8 = 8;

/// Identifier on our outgoing echo requests ("mk").
pub const PING_ID: u16 = 0x6d6b;
const PING_PAYLOAD: usize = 56;

#[derive(Clone, Copy, Debug)]
pub struct PingReply {
    pub from: Ipv4,
    pub seq: u16,
    pub ttl: u8,
    pub rtt_ns: u64,
    pub bytes: usize,
}

/// Recent echo replies to our pings (newest overwrite oldest).
pub struct PingTable {
    replies: [Option<PingReply>; 8],
    next: usize,
}

impl PingTable {
    pub const fn new() -> PingTable {
        PingTable {
            replies: [None; 8],
            next: 0,
        }
    }
    fn record(&mut self, r: PingReply) {
        self.replies[self.next] = Some(r);
        self.next = (self.next + 1) % self.replies.len();
    }
    /// Take the reply from `from` with sequence `seq`, if it arrived.
    pub fn take(&mut self, from: Ipv4, seq: u16) -> Option<PingReply> {
        for r in self.replies.iter_mut() {
            if let Some(p) = r
                && p.from == from
                && p.seq == seq
            {
                return r.take();
            }
        }
        None
    }
}

pub fn input(st: &mut Stack, src: Ipv4, dst: Ipv4, msg: &[u8], ip_pkt: &[u8]) {
    st.stats.icmp_in_msgs += 1;
    if msg.len() < 8 || checksum(msg) != 0 {
        st.stats.icmp_in_errors += 1;
        return;
    }
    match msg[0] {
        ECHO_REQUEST => {
            st.stats.icmp_in_echos += 1;
            if dst.is_broadcast() || msg.len() > super::iface::MTU - 20 {
                return;
            }
            st.stats.icmp_out_msgs += 1;
            st.stats.icmp_out_echo_reps += 1;
            let _ = st.send_ip(dst, src, PROTO_ICMP, |b| {
                b[..msg.len()].copy_from_slice(msg);
                b[0] = ECHO_REPLY;
                b[1] = 0;
                put16(b, 2, 0);
                let c = checksum(&b[..msg.len()]);
                put16(b, 2, c);
                msg.len()
            });
        }
        ECHO_REPLY => {
            st.stats.icmp_in_echo_reps += 1;
            if be16(msg, 4) != PING_ID {
                return;
            }
            let seq = be16(msg, 6);
            let sent = if msg.len() >= 16 {
                u64::from_be_bytes(msg[8..16].try_into().unwrap())
            } else {
                st.now
            };
            st.ping.record(PingReply {
                from: src,
                seq,
                ttl: ip_pkt[8],
                rtt_ns: st.now.saturating_sub(sent),
                bytes: msg.len(),
            });
            st.wake_any = true;
        }
        _ => {}
    }
}

/// Send one echo request to `dst` with sequence number `seq`. The reply shows up in
/// [`PingTable::take`] (see `net::socket::ping` for the blocking helper).
pub fn send_ping(st: &mut Stack, dst: Ipv4, seq: u16) -> Result<()> {
    st.stats.icmp_out_msgs += 1;
    st.stats.icmp_out_echos += 1;
    let now = timer::now_ns();
    st.send_ip(Ipv4::UNSPECIFIED, dst, PROTO_ICMP, |b| {
        let n = 8 + PING_PAYLOAD;
        b[0] = ECHO_REQUEST;
        b[1] = 0;
        put16(b, 2, 0);
        put16(b, 4, PING_ID);
        put16(b, 6, seq);
        b[8..16].copy_from_slice(&now.to_be_bytes());
        for (i, x) in b[16..n].iter_mut().enumerate() {
            *x = 0x10 + i as u8;
        }
        let c = checksum(&b[..n]);
        put16(b, 2, c);
        n
    })
}

/// Destination unreachable (`code` 2 = protocol, 3 = port) about `orig` (an IP packet we
/// received). Quotes the header + 8 bytes, as RFC 792 asks.
pub fn send_unreachable(st: &mut Stack, code: u8, orig: &[u8]) {
    let ihl = ((orig[0] & 0x0f) as usize) * 4;
    let quote = &orig[..(ihl + 8).min(orig.len())];
    let to = super::wire::ip_at(orig, 12);
    let from = super::wire::ip_at(orig, 16);
    if to.is_unspecified() || to.is_broadcast() {
        return;
    }
    st.stats.icmp_out_msgs += 1;
    st.stats.icmp_out_dest_unreachs += 1;
    let _ = st.send_ip(from, to, PROTO_ICMP, |b| {
        b[0] = DEST_UNREACH;
        b[1] = code;
        b[2..8].fill(0);
        b[8..8 + quote.len()].copy_from_slice(quote);
        let n = 8 + quote.len();
        let c = checksum(&b[..n]);
        put16(b, 2, c);
        n
    });
}
