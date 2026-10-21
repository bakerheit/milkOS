//! UDP (RFC 768). Datagrams wait in the socket's receive ring as records:
//! `[len: u16][src ip: 4][src port: u16][payload]`.

use super::errno::*;
use super::iface::{MTU, PROTO_UDP};
use super::socket::{Sock, SockType};
use super::wire::{Ipv4, SockAddr, be16, finish, ip_at, pseudo_sum, put16, sum};
use super::{Stack, dhcp, icmp};
use crate::fs::errno::Result;

pub const HDR: usize = 8;
/// Largest payload without IP fragmentation (1500 - 20 - 8).
pub const MAX_PAYLOAD: usize = MTU - 20 - HDR;
const REC_HDR: usize = 8;

/// Send one datagram from `src` to `dst` (no socket involved; DHCP uses this directly).
pub fn send_raw(st: &mut Stack, src: SockAddr, dst: SockAddr, data: &[u8]) -> Result<()> {
    if data.len() > MAX_PAYLOAD {
        return Err(EMSGSIZE);
    }
    let src_ip = if src.ip.is_unspecified() && !dst.ip.is_broadcast() {
        st.iface.src_for(dst.ip)
    } else {
        src.ip
    };
    st.stats.udp_out_datagrams += 1;
    st.send_ip(src_ip, dst.ip, PROTO_UDP, |b| {
        let n = HDR + data.len();
        put16(b, 0, src.port);
        put16(b, 2, dst.port);
        put16(b, 4, n as u16);
        put16(b, 6, 0);
        b[HDR..n].copy_from_slice(data);
        let mut c = finish(sum(&b[..n], pseudo_sum(src_ip, dst.ip, PROTO_UDP, n)));
        if c == 0 {
            c = 0xffff; // 0 means "no checksum"
        }
        put16(b, 6, c);
        n
    })
}

/// sendto on UDP socket slot `i`.
pub fn sendto(st: &mut Stack, i: usize, data: &[u8], to: SockAddr) -> Result<usize> {
    if to.port == 0 {
        return Err(EINVAL);
    }
    if data.len() > MAX_PAYLOAD {
        return Err(EMSGSIZE);
    }
    let s = st.socks.get(i).ok_or(EBADF)?;
    if s.shut_wr {
        return Err(EPIPE);
    }
    if !s.owns_port {
        let p = st.socks.ephemeral(SockType::Dgram)?;
        let s = st.socks.get(i).unwrap();
        s.local.port = p;
        s.owns_port = true;
    }
    let local = st.socks.get(i).unwrap().local;
    send_raw(st, local, to, data)?;
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
    let from = SockAddr::new(ip_at(&h, 2), be16(&h, 6));
    s.rx.consume(REC_HDR);
    let m = len.min(buf.len());
    let n = s.rx.pop(&mut buf[..m]);
    s.rx.consume(len - n); // truncated datagram: the rest is gone
    Ok((n, from))
}

pub fn input(st: &mut Stack, src: Ipv4, dst: Ipv4, p: &[u8], ip_pkt: &[u8]) {
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
    if be16(p, 6) != 0 && finish(sum(p, pseudo_sum(src, dst, PROTO_UDP, len))) != 0 {
        st.stats.udp_in_errors += 1;
        return;
    }
    let sport = be16(p, 0);
    let dport = be16(p, 2);
    let data = &p[HDR..];
    if dport == dhcp::CLIENT_PORT && sport == dhcp::SERVER_PORT {
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
        if !s.local.ip.is_unspecified()
            && s.local.ip != dst
            && !dst.is_broadcast()
            && !dst.is_multicast()
        {
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
            icmp::send_unreachable(st, 3, ip_pkt);
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
    h[2..6].copy_from_slice(&from.ip.0);
    put16(&mut h, 6, from.port);
    s.rx.push(&h);
    s.rx.push(data);
    st.wake(i);
    true
}
