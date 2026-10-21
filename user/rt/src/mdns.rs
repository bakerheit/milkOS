//! `.local` names: a small mDNS (RFC 6762) resolver and the packet code the responder
//! (the Milk Drop daemon, `drop daemon`) uses. Real DNS wire format on UDP 5353, over IPv4
//! (group 224.0.0.251) and IPv6 (group ff02::fb), A and AAAA records, so it speaks to any
//! mDNS host on the link; milkOS only sends multicast out of eth1 (the VM-to-VM link),
//! never eth0.
//!
//! Resolution policy ([`crate::net::lookup_all`]): numeric addresses and `localhost`
//! locally, `NAME.local` by mDNS, anything else is refused (no internet names from user
//! space). Queries are "legacy unicast" (an ephemeral source port, RFC 6762 6.7): the
//! responder answers straight back to us, with our query id.

use alloc::string::String;
use alloc::vec::Vec;

use crate::net::{self, Ip, SockAddr, Want};
use crate::sys;

pub const PORT: u16 = 5353;
pub const GROUP: [u8; 4] = [224, 0, 0, 251];
/// ff02::fb.
pub const GROUP6: [u8; 16] = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xfb];
pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
pub const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
/// eth1's interface index: where the IPv6 group is.
const ETH1: u32 = 3;

pub fn group() -> SockAddr {
    SockAddr::v4(GROUP, PORT)
}

/// [ff02::fb%eth1]:5353.
pub fn group6() -> SockAddr {
    SockAddr {
        ip: Ip::V6(GROUP6),
        port: PORT,
        scope: ETH1,
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

/// A query for `name`'s records of `types` (A, AAAA: one question each).
pub fn query_packet(id: u16, name: &str, types: &[u16]) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, types.len() as u8, 0, 0, 0, 0, 0, 0]); // flags 0
    for (k, ty) in types.iter().enumerate() {
        if k == 0 {
            encode_name(name, &mut p);
        } else {
            p.extend_from_slice(&[0xc0, 12]); // the first question's name
        }
        p.extend_from_slice(&ty.to_be_bytes());
        p.extend_from_slice(&CLASS_IN.to_be_bytes());
    }
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

/// The answer to the questions `qtypes` about `name` (which we own, at `addrs`): every A
/// (IPv4) and AAAA (IPv6) record they ask for (ANY: all), TTL `ttl` seconds, with the
/// question echoed for a legacy unicast resolver. None if no question has an answer.
pub fn answer_packet(
    id: u16,
    name: &str,
    qtypes: &[u16],
    addrs: &[Ip],
    ttl: u32,
    unicast: bool,
) -> Option<Vec<u8>> {
    let asked = |ty: u16| qtypes.iter().any(|&q| q == ty || q == TYPE_ANY);
    let recs: Vec<&Ip> = addrs
        .iter()
        .filter(|a| asked(if a.is_v6() { TYPE_AAAA } else { TYPE_A }))
        .collect();
    if recs.is_empty() {
        return None;
    }
    let mut p = Vec::new();
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&0x8400u16.to_be_bytes()); // response, authoritative
    let qd = if unicast { qtypes.len().min(8) } else { 0 };
    p.extend_from_slice(&(qd as u16).to_be_bytes());
    p.extend_from_slice(&(recs.len() as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0]);
    encode_name(name, &mut p);
    if unicast {
        for (k, q) in qtypes.iter().take(qd).enumerate() {
            if k > 0 {
                p.extend_from_slice(&[0xc0, 12]);
            }
            p.extend_from_slice(&q.to_be_bytes());
            p.extend_from_slice(&CLASS_IN.to_be_bytes());
        }
    } else {
        // No question: the name above belongs to the first record.
    }
    // Multicast answers set the cache-flush bit (we are the only owner of the name).
    let class = if unicast { CLASS_IN } else { CLASS_IN | 0x8000 };
    for (k, a) in recs.iter().enumerate() {
        if unicast || k > 0 {
            p.extend_from_slice(&[0xc0, 12]); // the name at offset 12
        }
        let (ty, data): (u16, Vec<u8>) = match a {
            Ip::V4(b) => (TYPE_A, b.to_vec()),
            Ip::V6(b) => (TYPE_AAAA, b.to_vec()),
        };
        p.extend_from_slice(&ty.to_be_bytes());
        p.extend_from_slice(&class.to_be_bytes());
        p.extend_from_slice(&ttl.to_be_bytes());
        p.extend_from_slice(&(data.len() as u16).to_be_bytes());
        p.extend_from_slice(&data);
    }
    Some(p)
}

/// The A and AAAA records for `want` in a response with id `id`, in packet order.
pub fn parse_answers(p: &[u8], id: u16, want: &str) -> Vec<Ip> {
    let mut out = Vec::new();
    let _ = (|| -> Option<()> {
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
            if n.eq_ignore_ascii_case(want.trim_end_matches('.')) {
                match (ty, len) {
                    (TYPE_A, 4) => out.push(Ip::V4(data.try_into().ok()?)),
                    (TYPE_AAAA, 16) => out.push(Ip::V6(data.try_into().ok()?)),
                    _ => {}
                }
            }
            off = o + 10 + len;
        }
        Some(())
    })();
    out
}

/// Is this a name we resolve by mDNS?
pub fn is_local(host: &str) -> bool {
    let h = host.trim_end_matches('.');
    h.len() > 6 && h[h.len() - 6..].eq_ignore_ascii_case(".local")
}

/// Resolve `name` (`bob.local`) by asking the link, over IPv6 (ff02::fb) and IPv4
/// (224.0.0.251) as `want` allows, for its AAAA and A records: one query each, one retry
/// halfway through `timeout_ms`. The addresses come back IPv6 first. Once one answer is
/// in, the other family gets 100 ms more.
pub fn resolve(name: &str, timeout_ms: u64, want: Want) -> Result<Vec<Ip>, String> {
    let types: &[u16] = match want {
        Want::Any => &[TYPE_AAAA, TYPE_A],
        Want::V4 => &[TYPE_A],
        Want::V6 => &[TYPE_AAAA],
    };
    let id = (sys::clock_ns(sys::CLOCK_MONOTONIC) as u16) | 1;
    let q = query_packet(id, name, types);
    // (socket, where its queries go)
    let mut socks: Vec<(i32, SockAddr)> = Vec::new();
    if want != Want::V4
        && let Ok(fd) = net::socket6(net::SOCK_DGRAM)
    {
        let _ = net::set_v6only(fd, true);
        socks.push((fd, group6()));
    }
    if want != Want::V6
        && let Ok(fd) = net::socket(net::SOCK_DGRAM)
    {
        socks.push((fd, group()));
    }
    let send_all = |socks: &[(i32, SockAddr)]| {
        let mut last = None;
        let mut any = false;
        for (fd, g) in socks {
            match net::sendto(*fd, &q, g) {
                Ok(_) => any = true,
                Err(e) => last = Some(e),
            }
        }
        if any { Ok(()) } else { Err(last) }
    };
    let r = (|| {
        send_all(&socks).map_err(|e| match e {
            Some(e) => alloc::format!(
                "{name}: can't ask the link ({e}): .local needs eth1, the VM-to-VM link"
            ),
            None => alloc::format!("{name}: no socket to ask the link with"),
        })?;
        let start = sys::clock_ns(sys::CLOCK_MONOTONIC);
        let mut end = start + timeout_ms * 1_000_000;
        let mut resent = false;
        let mut got: Vec<Ip> = Vec::new();
        let mut buf = [0u8; 1500];
        let complete = |got: &[Ip]| match want {
            Want::Any => got.iter().any(|a| a.is_v6()) && got.iter().any(|a| !a.is_v6()),
            _ => !got.is_empty(),
        };
        loop {
            let now = sys::clock_ns(sys::CLOCK_MONOTONIC);
            if now >= end || complete(&got) {
                if got.is_empty() {
                    return Err(alloc::format!("{name}: no answer on the link (is it up?)"));
                }
                got.sort_by_key(|a| !a.is_v6());
                return Ok(got);
            }
            if !resent && now - start >= (end - start) / 2 {
                let _ = send_all(&socks);
                resent = true;
            }
            let mut pf: Vec<net::PollFd> = socks
                .iter()
                .map(|(fd, _)| net::PollFd {
                    fd: *fd,
                    events: net::POLLIN,
                    revents: 0,
                })
                .collect();
            let wait = ((end - now) / 1_000_000).clamp(1, 100);
            if !matches!(net::poll(&mut pf, Some(wait)), Ok(1..)) {
                continue;
            }
            for (k, (fd, _)) in socks.iter().enumerate() {
                if pf[k].revents & net::POLLIN == 0 {
                    continue;
                }
                let Ok((n, _from)) = net::recvfrom(*fd, &mut buf) else {
                    continue;
                };
                let had = !got.is_empty();
                for a in parse_answers(&buf[..n], id, name) {
                    let ok = match want {
                        Want::Any => true,
                        Want::V4 => !a.is_v6(),
                        Want::V6 => a.is_v6(),
                    };
                    if ok && !got.contains(&a) {
                        got.push(a);
                    }
                }
                if !had && !got.is_empty() {
                    end = end.min(sys::clock_ns(sys::CLOCK_MONOTONIC) + 100_000_000);
                }
            }
        }
    })();
    for (fd, _) in socks {
        let _ = sys::close(fd);
    }
    r
}

/// Encode/decode round trips (A, AAAA, both), a compressed answer, a foreign packet.
/// (passed, total)
pub fn selftest() -> (usize, usize) {
    let mut ok = 0;
    let v4 = Ip::V4([169, 254, 11, 11]);
    let v6 = Ip::V6([
        0xfd, 0x6d, 0x69, 0x6c, 0x6b, 0, 0, 1, 0x50, 0x54, 0, 0xff, 0xfe, 0x12, 0x34, 0x57,
    ]);
    let q = query_packet(0x1234, "bob.local", &[TYPE_AAAA, TYPE_A]);
    ok += parse_query(&q).is_some_and(|(id, qs)| {
        id == 0x1234
            && qs.len() == 2
            && qs[0] == (String::from("bob.local"), TYPE_AAAA)
            && qs[1] == (String::from("bob.local"), TYPE_A)
    }) as usize;
    let a = answer_packet(0x1234, "bob.local", &[TYPE_A], &[v4, v6], 10, true).unwrap();
    ok += (parse_answers(&a, 0x1234, "BOB.local") == [v4]) as usize;
    ok += parse_answers(&a, 0x9999, "bob.local").is_empty() as usize;
    ok += parse_query(&a).is_none() as usize;
    let both = answer_packet(7, "bob.local", &[TYPE_AAAA, TYPE_A], &[v4, v6], 10, true).unwrap();
    ok += (parse_answers(&both, 7, "bob.local") == [v4, v6]) as usize;
    let m = answer_packet(0, "bob.local", &[TYPE_ANY], &[v6], 10, false).unwrap();
    ok += (parse_answers(&m, 0, "bob.local") == [v6]) as usize;
    ok += answer_packet(0, "bob.local", &[TYPE_AAAA], &[v4], 10, false).is_none() as usize;
    ok += (is_local("bob.local")
        && is_local("Bob.LOCAL.")
        && !is_local("local")
        && !is_local("x.test")) as usize;
    (ok, 8)
}
