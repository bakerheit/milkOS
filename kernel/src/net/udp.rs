//! UDP (RFC 768) over IPv4 and IPv6. Datagrams wait in the socket's receive ring as
//! records: `[len: u16][family: u8 (4/6)][0][src port: u16][src address: 16][payload]`.

use super::errno::*;
use super::iface::{MTU, PROTO_UDP};
use super::ipv6;
use super::socket::{self, Sock, SockType};
use super::wire::{IpAddr, Ipv4, Ipv6, SockAddr, be16, finish, l4_sum, pseudo_sum, put16, sum};
use super::{Stack, dhcp, icmp};
use crate::fs::errno::Result;

pub const HDR: usize = 8;
/// Largest payload without IP fragmentation (1500 - 20 - 8).
pub const MAX_PAYLOAD: usize = MTU - 20 - HDR;
/// Largest datagram payload at all (65535 - 20 - 8): bigger ones go out as IP fragments.
pub const MAX_DATAGRAM: usize = 65535 - 20 - HDR;
/// Over IPv6 we don't fragment: one packet (1500 - 40 - 8).
pub const MAX_PAYLOAD6: usize = MTU - 40 - HDR;
const REC_HDR: usize = 22;

fn finish_udp(c: u16) -> u16 {
    if c == 0 { 0xffff } else { c } // 0 means "no checksum"
}

/// Send one datagram from `src` to `dst` (no socket involved; DHCP uses this directly).
pub fn send_raw(st: &mut Stack, src: SockAddr, dst: SockAddr, data: &[u8]) -> Result<()> {
    send_scoped(st, src, dst, None, data)
}

/// [`send_raw`] with a link for a scoped (link-local / multicast) IPv6 destination.
pub fn send_scoped(
    st: &mut Stack,
    src: SockAddr,
    dst: SockAddr,
    scope: Option<usize>,
    data: &[u8],
) -> Result<()> {
    match dst.ip {
        IpAddr::V6(d6) => {
            if data.len() > MAX_PAYLOAD6 {
                return Err(EMSGSIZE);
            }
            let s6 = src.ip.v6().unwrap_or(Ipv6::UNSPECIFIED);
            st.stats.udp_out_datagrams += 1;
            ipv6::send(st, s6, d6, scope, ipv6::NH_UDP, |b, s, d| {
                let n = HDR + data.len();
                put16(b, 0, src.port);
                put16(b, 2, dst.port);
                put16(b, 4, n as u16);
                put16(b, 6, 0);
                b[HDR..n].copy_from_slice(data);
                let c = finish(sum(&b[..n], l4_sum(s.into(), d.into(), PROTO_UDP, n)));
                put16(b, 6, finish_udp(c));
                n
            })
        }
        IpAddr::V4(d4) => send_v4(st, src.ip.v4().unwrap_or(Ipv4::UNSPECIFIED), src.port, d4, dst.port, data),
    }
}

fn send_v4(st: &mut Stack, src: Ipv4, sport: u16, dst: Ipv4, dport: u16, data: &[u8]) -> Result<()> {
    if data.len() > MAX_DATAGRAM {
        return Err(EMSGSIZE);
    }
    let src_ip = if src.is_unspecified() && !dst.is_broadcast() {
        st.iface.src_for(dst)
    } else {
        src
    };
    st.stats.udp_out_datagrams += 1;
    if data.len() > MAX_PAYLOAD {
        // Too big for one packet: the whole datagram, then IP fragments of it.
        let n = HDR + data.len();
        let mut d = alloc::vec![0u8; n];
        put16(&mut d, 0, sport);
        put16(&mut d, 2, dport);
        put16(&mut d, 4, n as u16);
        d[HDR..].copy_from_slice(data);
        let c = finish(sum(&d, pseudo_sum(src_ip, dst, PROTO_UDP, n)));
        put16(&mut d, 6, finish_udp(c));
        return st.send_ip_fragments(src_ip, dst, PROTO_UDP, &d);
    }
    st.send_ip(src_ip, dst, PROTO_UDP, |b| {
        let n = HDR + data.len();
        put16(b, 0, sport);
        put16(b, 2, dport);
        put16(b, 4, n as u16);
        put16(b, 6, 0);
        b[HDR..n].copy_from_slice(data);
        let c = finish(sum(&b[..n], pseudo_sum(src_ip, dst, PROTO_UDP, n)));
        put16(b, 6, finish_udp(c));
        n
    })
}

/// sendto on UDP socket slot `i`.
pub fn sendto(st: &mut Stack, i: usize, data: &[u8], to: SockAddr) -> Result<usize> {
    if to.port == 0 {
        return Err(EINVAL);
    }
    let s = st.socks.get(i).ok_or(EBADF)?;
    if s.shut_wr {
        return Err(EPIPE);
    }
    socket::family_ok(s, to.ip)?;
    if data.len() > if to.ip.is_v6() { MAX_PAYLOAD6 } else { MAX_DATAGRAM } {
        return Err(EMSGSIZE);
    }
    if !s.owns_port {
        let p = st.socks.ephemeral(SockType::Dgram)?;
        let s = st.socks.get(i).unwrap();
        s.local.port = p;
        s.owns_port = true;
    }
    let s = st.socks.get(i).unwrap();
    let (local, scope) = (s.local, s.scope);
    // A dual-stack socket's wildcard local address fits either family.
    let src = if local.ip.is_unspecified() || local.ip.is_v6() != to.ip.is_v6() {
        SockAddr {
            ip: if to.ip.is_v6() { IpAddr::ANY6 } else { IpAddr::ANY4 },
            port: local.port,
        }
    } else {
        local
    };
    send_scoped(st, src, to, scope, data)?;
    Ok(data.len())
}

/// Size of the next queued datagram (0 if none).
pub fn next_len(s: &Sock) -> usize {
    let mut h = [0u8; 2];
    if s.rx.peek_at(0, &mut h) == 2 {
        u16::from_be_bytes(h) as usize
    } else {
        0
    }
}

pub fn recvfrom(s: &mut Sock, buf: &mut [u8]) -> Result<(usize, SockAddr)> {
    let mut h = [0u8; REC_HDR];
    if s.rx.peek_at(0, &mut h) < REC_HDR {
        if let Some(e) = s.error.take() {
            return Err(e);
        }
        if s.shut_rd {
            return Ok((0, SockAddr::ANY));
        }
        return Err(EAGAIN);
    }
    let len = be16(&h, 0) as usize;
    let ip = if h[2] == 6 {
        IpAddr::V6(Ipv6(h[6..22].try_into().unwrap()))
    } else {
        IpAddr::V4(Ipv4([h[6], h[7], h[8], h[9]]))
    };
    let from = SockAddr::new(ip, be16(&h, 4));
    s.rx.consume(REC_HDR);
    let m = len.min(buf.len());
    let n = s.rx.pop(&mut buf[..m]);
    s.rx.consume(len - n); // truncated datagram: the rest is gone
    Ok((n, from))
}

pub fn input(st: &mut Stack, src: Ipv4, dst: Ipv4, p: &[u8], ip_pkt: &[u8]) {
    deliver(st, src.into(), dst.into(), p, ip_pkt, None);
}

pub fn input6(st: &mut Stack, src: Ipv6, dst: Ipv6, p: &[u8], ip_pkt: &[u8], link: Option<usize>) {
    deliver(st, IpAddr::V6(src), IpAddr::V6(dst), p, ip_pkt, link);
}

fn deliver(st: &mut Stack, src: IpAddr, dst: IpAddr, p: &[u8], ip_pkt: &[u8], link: Option<usize>) {
    if p.len() < HDR {
        st.stats.udp_in_errors += 1;
        return;
    }
    let len = be16(p, 4) as usize;
    if len < HDR || len > p.len() {
        st.stats.udp_in_errors += 1;
        return;
    }
    let p = &p[..len];
    let csum = be16(p, 6);
    // IPv6 requires the checksum (RFC 8200 8.1); IPv4 may leave it 0.
    if (csum != 0 || dst.is_v6()) && finish(sum(p, l4_sum(src, dst, PROTO_UDP, len))) != 0 {
        st.stats.udp_in_errors += 1;
        return;
    }
    let sport = be16(p, 0);
    let dport = be16(p, 2);
    let data = &p[HDR..];
    if !dst.is_v6() && dport == dhcp::CLIENT_PORT && sport == dhcp::SERVER_PORT {
        st.stats.udp_in_datagrams += 1;
        dhcp::input(st, data);
        return;
    }
    let from = SockAddr::new(src, sport);
    let mut target = None;
    for (i, s) in st.socks.iter() {
        if s.ty != SockType::Dgram || s.icmp || !s.owns_port || s.local.port != dport {
            continue;
        }
        if !socket::takes(s, dst) {
            continue;
        }
        if s.udp_connected && s.remote != from {
            continue;
        }
        target = Some(i);
        if !s.local.ip.is_unspecified() {
            break; // exact address beats the wildcard
        }
    }
    let Some(i) = target else {
        st.stats.udp_no_ports += 1;
        if !dst.is_broadcast() && !dst.is_multicast() {
            if dst.is_v6() {
                ipv6::send_error(st, link, 1, 4, 0, ip_pkt);
            } else {
                icmp::send_unreachable(st, 3, ip_pkt);
            }
        }
        return;
    };
    st.stats.udp_in_datagrams += 1;
    if !queue(st, i, from, data) {
        st.stats.udp_rcvbuf_errors += 1;
    }
}

/// Queue one datagram record for socket slot `i` (UDP, or ICMP ping sockets). False if it
/// did not fit (dropped).
pub fn queue(st: &mut Stack, i: usize, from: SockAddr, data: &[u8]) -> bool {
    let s = st.socks.get(i).unwrap();
    if s.shut_rd || s.rx.free() < REC_HDR + data.len() {
        s.rx_drops += 1;
        return false;
    }
    let mut h = [0u8; REC_HDR];
    put16(&mut h, 0, data.len() as u16);
    put16(&mut h, 4, from.port);
    match from.ip {
        IpAddr::V4(a) => {
            h[2] = 4;
            h[6..10].copy_from_slice(&a.0);
        }
        IpAddr::V6(a) => {
            h[2] = 6;
            h[6..22].copy_from_slice(&a.0);
        }
    }
    s.rx.push(&h);
    s.rx.push(data);
    st.wake(i);
    true
}
