//! `.local` names: a small mDNS (RFC 6762) resolver and the packet code the responder
//! (the Milk Drop daemon, `drop daemon`) uses. Real DNS wire format on UDP 5353 and the
//! group 224.0.0.251, so it speaks to any mDNS host on the link; milkOS only sends
//! multicast out of eth1 (the VM-to-VM link), never eth0.
//!
//! Resolution policy ([`crate::net::lookup`]): dotted quads and `localhost` locally,
//! `NAME.local` by mDNS, anything else is refused (no internet names from user space).
//! Queries are "legacy unicast" (an ephemeral source port, RFC 6762 6.7): the responder
//! answers straight back to us, with our query id.

use alloc::string::String;
use alloc::vec::Vec;

use crate::net::{self, SockAddr};
use crate::sys;

pub const PORT: u16 = 5353;
pub const GROUP: [u8; 4] = [224, 0, 0, 251];
pub const TYPE_A: u16 = 1;
pub const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;

pub fn group() -> SockAddr {
    SockAddr {
        ip: GROUP,
        port: PORT,
    }
}

fn be16(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

/// `bob.local` -> labels.
pub fn encode_name(name: &str, out: &mut Vec<u8>) {
    for l in name.trim_end_matches('.').split('.') {
        let l = &l.as_bytes()[..l.len().min(63)];
        out.push(l.len() as u8);
        out.extend_from_slice(l);
    }
    out.push(0);
}

/// A name at `off` (following compression pointers): (name, offset after it).
fn read_name(p: &[u8], mut off: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut end = None;
    for _ in 0..32 {
        let len = *p.get(off)? as usize;
        if len == 0 {
            return Some((name, end.unwrap_or(off + 1)));
        }
        if len & 0xc0 == 0xc0 {
            let ptr = (be16(p, off)? & 0x3fff) as usize;
            end.get_or_insert(off + 2);
            off = ptr;
            continue;
        }
        let label = p.get(off + 1..off + 1 + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&String::from_utf8_lossy(label));
        off += 1 + len;
    }
    None // a pointer loop
}

/// A query for `name`'s A record.
pub fn query_packet(id: u16, name: &str) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0, 0, 0]); // flags 0, qd 1
    encode_name(name, &mut p);
    p.extend_from_slice(&TYPE_A.to_be_bytes());
    p.extend_from_slice(&CLASS_IN.to_be_bytes());
    p
}

/// A query's id and questions (name, type), or None for anything that isn't one.
pub fn parse_query(p: &[u8]) -> Option<(u16, Vec<(String, u16)>)> {
    let id = be16(p, 0)?;
    let flags = be16(p, 2)?;
    if flags & 0x8000 != 0 || flags & 0x7800 != 0 {
        return None; // a response, or not a standard query
    }
    let qd = be16(p, 4)? as usize;
    let mut off = 12;
    let mut qs = Vec::new();
    for _ in 0..qd.min(8) {
        let (n, o) = read_name(p, off)?;
        let ty = be16(p, o)?;
        off = o + 4;
        qs.push((n, ty));
    }
    Some((id, qs))
}

/// The answer to a query for `name` (which we own) at `addr`: the question echoed (for
/// legacy unicast resolvers), one A record, TTL `ttl` seconds.
pub fn answer_packet(id: u16, name: &str, addr: [u8; 4], ttl: u32, unicast: bool) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0x8400u16.to_be_bytes()); // response, authoritative
    p.extend_from_slice(&[0, if unicast { 1 } else { 0 }, 0, 1, 0, 0, 0, 0]);
    if unicast {
        encode_name(name, &mut p);
        p.extend_from_slice(&TYPE_A.to_be_bytes());
        p.extend_from_slice(&CLASS_IN.to_be_bytes());
        p.extend_from_slice(&[0xc0, 12]); // the name above
    } else {
        encode_name(name, &mut p);
    }
    p.extend_from_slice(&TYPE_A.to_be_bytes());
    // Multicast answers set the cache-flush bit (we are the only owner of the name).
    let class = if unicast { CLASS_IN } else { CLASS_IN | 0x8000 };
    p.extend_from_slice(&class.to_be_bytes());
    p.extend_from_slice(&ttl.to_be_bytes());
    p.extend_from_slice(&4u16.to_be_bytes());
    p.extend_from_slice(&addr);
    p
}

/// The A record for `want` in a response with id `id`.
pub fn parse_answer(p: &[u8], id: u16, want: &str) -> Option<[u8; 4]> {
    if be16(p, 0)? != id || be16(p, 2)? & 0x8000 == 0 {
        return None;
    }
    let (qd, an) = (be16(p, 4)? as usize, be16(p, 6)? as usize);
    let mut off = 12;
    for _ in 0..qd {
        off = read_name(p, off)?.1 + 4;
    }
    for _ in 0..an {
        let (n, o) = read_name(p, off)?;
        let ty = be16(p, o)?;
        let len = be16(p, o + 8)? as usize;
        let data = p.get(o + 10..o + 10 + len)?;
        if ty == TYPE_A && len == 4 && n.eq_ignore_ascii_case(want.trim_end_matches('.')) {
            return Some([data[0], data[1], data[2], data[3]]);
        }
        off = o + 10 + len;
    }
    None
}

/// Is this a name we resolve by mDNS?
pub fn is_local(host: &str) -> bool {
    let h = host.trim_end_matches('.');
    h.len() > 6 && h[h.len() - 6..].eq_ignore_ascii_case(".local")
}

/// Resolve `name` (`bob.local`) by asking the link: one query, one retry halfway through
/// `timeout_ms`.
pub fn resolve(name: &str, timeout_ms: u64) -> Result<[u8; 4], String> {
    let fd = net::socket(net::SOCK_DGRAM).map_err(|e| alloc::format!("socket: {e}"))?;
    let id = (sys::clock_ns(sys::CLOCK_MONOTONIC) as u16) | 1;
    let q = query_packet(id, name);
    let r = (|| {
        net::sendto(fd, &q, &group()).map_err(|e| {
            alloc::format!("{name}: can't ask the link ({e}): .local needs eth1, the VM-to-VM link")
        })?;
        let start = sys::clock_ns(sys::CLOCK_MONOTONIC);
        let end = start + timeout_ms * 1_000_000;
        let mut resent = false;
        let mut buf = [0u8; 1500];
        loop {
            let now = sys::clock_ns(sys::CLOCK_MONOTONIC);
            if now >= end {
                return Err(alloc::format!("{name}: no answer on the link (is it up?)"));
            }
            if !resent && now - start >= (end - start) / 2 {
                let _ = net::sendto(fd, &q, &group());
                resent = true;
            }
            let _ = net::set_recv_timeout(fd, ((end - now) / 1_000_000).clamp(1, 100));
            let Ok((n, _from)) = net::recvfrom(fd, &mut buf) else {
                continue;
            };
            if let Some(a) = parse_answer(&buf[..n], id, name) {
                return Ok(a);
            }
        }
    })();
    let _ = sys::close(fd);
    r
}

/// Encode/decode round trip, a compressed answer, a foreign packet. (passed, total)
pub fn selftest() -> (usize, usize) {
    let mut ok = 0;
    let q = query_packet(0x1234, "bob.local");
    ok += parse_query(&q).is_some_and(|(id, qs)| {
        id == 0x1234 && qs.len() == 1 && qs[0].0 == "bob.local" && qs[0].1 == TYPE_A
    }) as usize;
    let a = answer_packet(0x1234, "bob.local", [169, 254, 11, 11], 10, true);
    ok += (parse_answer(&a, 0x1234, "BOB.local") == Some([169, 254, 11, 11])) as usize;
    ok += parse_answer(&a, 0x9999, "bob.local").is_none() as usize;
    ok += parse_query(&a).is_none() as usize;
    let m = answer_packet(0, "bob.local", [1, 2, 3, 4], 10, false);
    ok += (parse_answer(&m, 0, "bob.local") == Some([1, 2, 3, 4])) as usize;
    ok += (is_local("bob.local")
        && is_local("Bob.LOCAL.")
        && !is_local("local")
        && !is_local("x.test")) as usize;
    (ok, 6)
}
