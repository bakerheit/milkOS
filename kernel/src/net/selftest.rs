//! Boot self-test of the network stack (thread context, after `net::init`), with
//! `opt/milkos/selftest=net` or `full` (kernel/src/selftest.rs; the default boot only waits for
//! DHCP, [`wait_dhcp`]).
//!
//! Always (no host services needed): DHCP on eth0 (3 s timeout, boot goes on without it),
//! ping the gateway (10.0.2.2, answered by qemu's slirp itself), UDP and TCP over loopback,
//! connection refused, an HTTP GET to our own server on :80, the DNS policy checks.
//!
//! With a test config passed by `scripts/net-test.sh` as fw_cfg file `opt/milkos/nettest`
//! (`echo=IP:PORT echo6=[IPV6]:PORT dns=IP:PORT name=NAME expect=IP refused=IP:PORT khttpd=PORT`): TCP
//! echo round trips against the host's echo servers (IPv4, and IPv6 to fec0::2, which slirp
//! maps to the Mac's ::1), a DNS lookup against the host's test DNS server, and a
//! refused connection to a host port nobody listens on. Only addresses from that config are
//! contacted (in practice the Mac's loopback, seen as 10.0.2.2).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::errno::{self, *};
use super::socket::{self, Family, POLLIN, POLLOUT, SockType, SocketId};
use super::wire::{Ipv4, Ipv6, SockAddr, checksum, put16};
use super::{dns, services};
use crate::fs::errno::{Errno, Result};
use crate::{fwcfg, kprintln, sched, timer};

const MS: u64 = 1_000_000;

#[derive(Default)]
struct Config {
    echo: Option<SockAddr>,
    echo6: Option<SockAddr>,
    dns: Option<SockAddr>,
    name: Option<String>,
    expect: Option<Ipv4>,
    refused: Option<SockAddr>,
    khttpd: Option<u16>,
    /// `lossy3=1`: also the 1-in-3 loss test (slow: tens of seconds of RTO backoff).
    heavy_loss: bool,
}

fn parse_addr(s: &str) -> Option<SockAddr> {
    let (ip, port) = s.split_once(':')?;
    Some(SockAddr::new(Ipv4::parse(ip)?, port.parse().ok()?))
}

/// `[IPV6]:PORT`.
fn parse_addr6(s: &str) -> Option<SockAddr> {
    let (ip, port) = s.strip_prefix('[')?.split_once("]:")?;
    Some(SockAddr::new(Ipv6::parse(ip)?, port.parse().ok()?))
}

fn read_config() -> Config {
    let mut c = Config::default();
    if !fwcfg::available() {
        return c;
    }
    let Some(f) = fwcfg::find_file("opt/milkos/nettest") else {
        return c;
    };
    let mut buf = vec![0u8; (f.size as usize).min(512)];
    if !fwcfg::read_file(f, &mut buf) {
        return c;
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    for kv in text.split_whitespace() {
        match kv.split_once('=') {
            Some(("echo", v)) => c.echo = parse_addr(v),
            Some(("echo6", v)) => c.echo6 = parse_addr6(v),
            Some(("dns", v)) => c.dns = parse_addr(v),
            Some(("name", v)) => c.name = Some(String::from(v)),
            Some(("expect", v)) => c.expect = Ipv4::parse(v),
            Some(("refused", v)) => c.refused = parse_addr(v),
            Some(("khttpd", v)) => c.khttpd = v.parse().ok(),
            Some(("lossy3", v)) => c.heavy_loss = v == "1",
            _ => {}
        }
    }
    c
}

fn wait_for(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let end = timer::now_ns() + timeout_ms * MS;
    loop {
        if cond() {
            return true;
        }
        if timer::now_ns() >= end {
            return false;
        }
        sched::sleep_ms(5);
    }
}

fn e(x: Errno) -> &'static str {
    errno::name(x)
}

fn es(x: Errno) -> String {
    String::from(errno::name(x))
}

/// Blocking connect with a timeout.
fn connect(addr: SockAddr, timeout_ms: u64) -> Result<SocketId> {
    connect_fam(Family::V4, addr, timeout_ms)
}

/// [`connect`] from a socket of family `fam`.
fn connect_fam(fam: Family, addr: SockAddr, timeout_ms: u64) -> Result<SocketId> {
    let s = socket::socket_family(fam, SockType::Stream)?;
    match socket::connect(s, addr) {
        Ok(()) => return Ok(s),
        Err(x) if x == EINPROGRESS => {}
        Err(x) => {
            let _ = socket::close(s);
            return Err(x);
        }
    }
    let r = socket::wait(s, POLLOUT, timeout_ms * MS);
    let res = if r == 0 {
        Err(ETIMEDOUT)
    } else {
        socket::connect(s, addr)
    };
    match res {
        Ok(()) => Ok(s),
        Err(x) => {
            let _ = socket::close(s);
            Err(x)
        }
    }
}

fn pattern(i: usize) -> u8 {
    (i.wrapping_mul(31) ^ (i >> 8)) as u8
}

/// Push `total` pattern bytes through `tx` while reading them back on `rx` (same or other
/// socket), checking every byte. Returns elapsed ns.
fn pump(
    tx: SocketId,
    rx: SocketId,
    total: usize,
    timeout_ms: u64,
) -> core::result::Result<u64, String> {
    let t0 = timer::now_ns();
    let (mut sent, mut got) = (0usize, 0usize);
    let mut buf = [0u8; 2048];
    while got < total {
        if timer::now_ns() - t0 > timeout_ms * MS {
            return Err(alloc::format!("timeout after {sent} sent / {got} received"));
        }
        if sent < total {
            let n = (total - sent).min(buf.len());
            for (k, b) in buf[..n].iter_mut().enumerate() {
                *b = pattern(sent + k);
            }
            match socket::send(tx, &buf[..n]) {
                Ok(m) => sent += m,
                Err(x) if x == EAGAIN => {}
                Err(x) => return Err(alloc::format!("send: {}", e(x))),
            }
        }
        match socket::recv(rx, &mut buf) {
            Ok(0) => return Err(alloc::format!("EOF after {got} bytes")),
            Ok(n) => {
                for (k, &b) in buf[..n].iter().enumerate() {
                    if b != pattern(got + k) {
                        return Err(alloc::format!("byte {} differs", got + k));
                    }
                }
                got += n;
                continue;
            }
            Err(x) if x == EAGAIN => {}
            Err(x) => return Err(alloc::format!("recv: {}", e(x))),
        }
        let want = if sent < total {
            POLLIN | POLLOUT
        } else {
            POLLIN
        };
        socket::wait(rx, want, 50 * MS);
        if sent < total {
            socket::wait(tx, POLLOUT, 0);
        }
    }
    Ok(timer::now_ns() - t0)
}

fn kib_per_s(bytes: usize, ns: u64) -> u64 {
    (bytes as u64 * 1_000_000_000 / ns.max(1)) / 1024
}

fn test_loopback_tcp() -> core::result::Result<(), String> {
    let l = socket::socket(SockType::Stream).map_err(es)?;
    socket::bind(l, SockAddr::new(Ipv4::LOCALHOST, 5000)).map_err(es)?;
    socket::listen(l, 4).map_err(es)?;
    let c = connect(SockAddr::new(Ipv4::LOCALHOST, 5000), 1000)
        .map_err(|x| alloc::format!("connect: {}", e(x)))?;
    if socket::wait(l, POLLIN, 1000 * MS) == 0 {
        return Err("accept: nothing queued".into());
    }
    let (a, peer) = socket::accept(l).map_err(|x| alloc::format!("accept: {}", e(x)))?;
    let total = 256 * 1024;
    let r = pump(c, a, total, 5000);
    let r2 = r.and_then(|ns| pump(a, c, 64 * 1024, 5000).map(|ns2| (ns, ns2)));
    let (ns, _) = r2?;
    // Orderly close both ways: c FINs, a sees EOF, a closes.
    socket::shutdown(c, socket::Shutdown::Write).map_err(es)?;
    socket::wait(a, POLLIN, 1000 * MS);
    let mut b = [0u8; 8];
    let eof = socket::recv(a, &mut b);
    let _ = socket::close(a);
    socket::wait(c, POLLIN, 1000 * MS);
    let eof2 = socket::recv(c, &mut b);
    let _ = socket::close(c);
    let _ = socket::close(l);
    if eof != Ok(0) || eof2 != Ok(0) {
        return Err(alloc::format!("close: eof {:?} / {:?}", eof, eof2));
    }
    kprintln!(
        "[net] tcp loopback: {} KiB from :{} through 127.0.0.1:5000 in {} ms ({} KiB/s), 64 KiB back, FIN both ways ok",
        total / 1024,
        peer.port,
        ns / MS,
        kib_per_s(total, ns)
    );
    Ok(())
}

/// One ICMPv6 echo round trip: Ok(rtt ns).
fn ping6(dst: Ipv6, seq: u16, timeout_ms: u64) -> core::result::Result<u64, String> {
    let t0 = timer::now_ns();
    super::with(|st| super::ipv6::ping(st, dst, 0x6d6b, seq, b"milkOS ping6"))
        .map_err(|x| alloc::format!("send: {}", e(x)))?;
    let got = wait_for(timeout_ms, || {
        super::with(|st| {
            st.ip6.ping_replies.iter().any(|&(from, id, sq, _, _)| {
                id == 0x6d6b && sq == seq && (from == dst || dst.is_multicast())
            })
        })
    });
    if got {
        Ok(timer::now_ns() - t0)
    } else {
        Err(alloc::format!("no echo reply from {dst}"))
    }
}

fn test_ipv6() -> core::result::Result<(), String> {
    // Link-local after DAD (0.5 s), then SLAAC from slirp's RA (it answers our RS).
    let ll = wait_for(2000, || {
        super::with(|st| st.iface.eth0().v6.link_local().is_some())
    });
    let slaac = wait_for(4000, || {
        super::with(|st| {
            st.iface
                .eth0()
                .v6
                .addr(super::ipv6::Origin::Slaac)
                .is_some()
        })
    });
    let (lla, ga, router) = super::with(|st| {
        let v = &st.iface.eth0().v6;
        (
            v.link_local(),
            v.addr(super::ipv6::Origin::Slaac),
            v.router.map(|r| r.0),
        )
    });
    if !ll {
        return Err(String::from("no link-local address on eth0"));
    }
    let rtt_lo = ping6(Ipv6::LOOPBACK, 1, 500)?;
    let rtt_ll = ping6(lla.unwrap(), 2, 500)?;
    kprintln!(
        "[net] ipv6: eth0 {} (link-local), ping ::1 {} us, own link-local {} us",
        lla.unwrap(),
        rtt_lo / 1000,
        rtt_ll / 1000
    );
    if !slaac {
        return Err(String::from(
            "no SLAAC address (no router advertisement from slirp?)",
        ));
    }
    // slirp's host: <prefix>::2 (as 10.0.2.2 for IPv4).
    let ga = ga.unwrap();
    let mut host = ga;
    host.0[8..].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 2]);
    let rtt = ping6(host, 3, 2000)?;
    kprintln!(
        "[net] ipv6: eth0 {ga}/64 (SLAAC, router {}), ping {host} {}.{:03} ms",
        router.unwrap_or_default(),
        rtt / MS,
        rtt % MS / 1000
    );
    Ok(())
}

/// IPv6 extension headers, on crafted packets from ::1 to ::1 carrying an echo request:
/// hop-by-hop + destination options (accepted), an atomic fragment (accepted), a routing
/// header with segments left (refused, never routed), a lone first fragment (held for
/// reassembly, not answered), an unknown option whose action bits say "discard"
/// (dropped).
fn test_ipv6_ext() -> core::result::Result<(), String> {
    use super::wire::{finish, pseudo_sum6, sum};
    // ext: (next header value of each header, its bytes without the next-header byte).
    let packet = |seq: u16, ext: &[(u8, &[u8])]| -> Vec<u8> {
        let mut icmp = alloc::vec![128u8, 0, 0, 0, 0x6d, 0x6c, (seq >> 8) as u8, seq as u8];
        let c = finish(sum(
            &icmp,
            pseudo_sum6(Ipv6::LOOPBACK, Ipv6::LOOPBACK, 58, icmp.len()),
        ));
        put16(&mut icmp, 2, c);
        let mut body = Vec::new();
        for (k, (_, bytes)) in ext.iter().enumerate() {
            let next = ext.get(k + 1).map_or(58, |x| x.0);
            body.push(next);
            body.extend_from_slice(bytes);
        }
        body.extend_from_slice(&icmp);
        let mut p = alloc::vec![0u8; 40];
        p[0] = 0x60;
        put16(&mut p, 4, body.len() as u16);
        p[6] = ext.first().map_or(58, |x| x.0);
        p[7] = 64;
        p[8..24].copy_from_slice(&Ipv6::LOOPBACK.0);
        p[24..40].copy_from_slice(&Ipv6::LOOPBACK.0);
        p.extend_from_slice(&body);
        p
    };
    // An 8-byte options header: hdr-ext-len 0, then 6 bytes of options.
    let padn: &[u8] = &[0, 1, 4, 0, 0, 0, 0];
    let unknown_discard: &[u8] = &[0, 0x41, 4, 0, 0, 0, 0]; // action 01: discard silently
    let routing_live: &[u8] = &[0, 0, 1, 0, 0, 0, 0]; // type 0, segments left 1
    let atomic: &[u8] = &[0, 0, 0, 0, 0, 0, 1]; // fragment header, offset 0, no M
    let real_frag: &[u8] = &[0, 0, 1, 0, 0, 0, 2]; // M set
    let replies = |seq: u16| {
        wait_for(200, || {
            super::with(|st| {
                st.ip6
                    .ping_replies
                    .iter()
                    .any(|&(_, id, sq, _, _)| id == 0x6d6c && sq == seq)
            })
        })
    };
    let before = super::with(|st| st.ip6.stats);
    let held0 = super::with(|st| st.frags.stats6.reqds);
    let inject =
        |p: Vec<u8>| super::with(|st| super::ipv6::input(st, None, &p, super::wire::Mac::ZERO));
    inject(packet(1, &[(0, padn), (60, padn)]));
    let ok1 = replies(1);
    inject(packet(2, &[(44, atomic)]));
    let ok2 = replies(2);
    inject(packet(3, &[(43, routing_live)]));
    let ok3 = !replies(3);
    inject(packet(4, &[(44, real_frag)]));
    let ok4 = !replies(4);
    inject(packet(5, &[(60, unknown_discard)]));
    let ok5 = !replies(5);
    let after = super::with(|st| st.ip6.stats);
    let held = super::with(|st| st.frags.stats6.reqds) - held0;
    kprintln!(
        "[net] ipv6 extension headers: hop-by-hop + dst options ok, atomic fragment ok, routing header with segments left refused ({}), lone fragment held ({}), discard-option dropped ({})",
        after.routing_refused - before.routing_refused,
        held,
        after.in_discards - before.in_discards
    );
    if !(ok1 && ok2 && ok3 && ok4 && ok5)
        || after.routing_refused != before.routing_refused + 1
        || held != 1
        || after.in_discards != before.in_discards + 1
    {
        return Err(alloc::format!("results {ok1} {ok2} {ok3} {ok4} {ok5}"));
    }
    Ok(())
}

/// IPv6 reassembly on crafted fragments from ::1 to ::1 carrying a 2000-byte echo
/// request: two fragments last-first (reassembled and answered), an overlapping pair
/// (the datagram is poisoned), and a first fragment without the upper-layer header (a
/// destination options header in it, the ICMPv6 header only in the second: refused, RFC
/// 7112).
fn test_fragments6() -> core::result::Result<(), String> {
    use super::wire::{finish, pseudo_sum6, sum};
    let lo = Ipv6::LOOPBACK;
    let echo = |seq: u16| -> Vec<u8> {
        let mut m = alloc::vec![128u8, 0, 0, 0, 0x6d, 0x6c, (seq >> 8) as u8, seq as u8];
        m.extend((0..2000).map(|k| k as u8));
        let c = finish(sum(&m, pseudo_sum6(lo, lo, 58, m.len())));
        put16(&mut m, 2, c);
        m
    };
    // An IPv6 packet with a fragment header: id, byte offset, more, next, data.
    let frag = |id: u32, off: usize, more: bool, next: u8, data: &[u8]| -> Vec<u8> {
        let mut p = alloc::vec![0u8; 48];
        p[0] = 0x60;
        put16(&mut p, 4, (8 + data.len()) as u16);
        p[6] = 44;
        p[7] = 64;
        p[8..24].copy_from_slice(&lo.0);
        p[24..40].copy_from_slice(&lo.0);
        p[40] = next;
        put16(&mut p, 42, off as u16 | more as u16);
        p[44..48].copy_from_slice(&id.to_be_bytes());
        p.extend_from_slice(data);
        p
    };
    let inject =
        |p: Vec<u8>| super::with(|st| super::ipv6::input(st, None, &p, super::wire::Mac::ZERO));
    let replied = |seq: u16| {
        wait_for(200, || {
            super::with(|st| {
                st.ip6
                    .ping_replies
                    .iter()
                    .any(|&(_, id, sq, _, _)| id == 0x6d6c && sq == seq)
            })
        })
    };
    let before = super::with(|st| st.frags.stats6);
    // 1. Last first.
    let m = echo(21);
    inject(frag(0x6d00_0001, 1232, false, 58, &m[1232..]));
    inject(frag(0x6d00_0001, 0, true, 58, &m[..1232]));
    let ok1 = replied(21);
    // 2. Overlap: the second fragment starts 8 bytes inside the first.
    let m = echo(22);
    inject(frag(0x6d00_0002, 0, true, 58, &m[..1232]));
    inject(frag(0x6d00_0002, 1224, false, 58, &m[1224..]));
    let ok2 = !replied(22);
    // 3. The header chain doesn't fit the first fragment: a destination options header
    // (8 bytes) is all it holds, the ICMPv6 header comes in the second.
    let m = echo(23);
    let first = [58u8, 0, 1, 4, 0, 0, 0, 0]; // dst options: next 58, PadN
    let ok3_seen = {
        inject(frag(0x6d00_0003, 0, true, 60, &first));
        inject(frag(0x6d00_0003, 8, false, 60, &m));
        !replied(23)
    };
    let after = super::with(|st| st.frags.stats6);
    kprintln!(
        "[net] ipv6 fragments: 2000-byte echo in 2 fragments last-first answered, overlap poisoned ({}), header chain split refused, {} reassembled",
        after.overlaps - before.overlaps,
        after.oks - before.oks
    );
    if !(ok1 && ok2 && ok3_seen)
        || after.overlaps != before.overlaps + 1
        || after.oks != before.oks + 1
    {
        return Err(alloc::format!("results {ok1} {ok2} {ok3_seen}"));
    }
    Ok(())
}

/// A v6-only boot: no IPv4 address on eth0 or eth1 after DHCP would have finished, IPv4
/// to the slirp gateway is refused at once (ENETUNREACH), loopback IPv4 still works.
fn test_v4_off() -> core::result::Result<(), String> {
    sched::sleep_ms(1500); // DHCP would have answered by now (~10 ms on slirp)
    let (a0, a1) = super::with(|st| (st.iface.links[0].cfg.addr, st.iface.links[1].cfg.addr));
    let gw = connect(SockAddr::new(Ipv4::new(10, 0, 2, 2), 80), 500);
    let lo = {
        let a = socket::socket(SockType::Dgram).map_err(es)?;
        let r = socket::sendto(a, b"x", SockAddr::new(Ipv4::LOCALHOST, 9));
        let _ = socket::close(a);
        r
    };
    kprintln!(
        "[net] v6-only: eth0 {a0}, eth1 {a1} (no IPv4), connect 10.0.2.2: {}, IPv4 loopback ok",
        match gw {
            Err(x) => e(x),
            Ok(_) => "connected?!",
        }
    );
    if !a0.is_unspecified()
        || !a1.is_unspecified()
        || gw.map(|_| ()) != Err(ENETUNREACH)
        || lo.is_err()
    {
        return Err(String::from("IPv4 is not off"));
    }
    Ok(())
}

/// MLD and multicast membership on eth0: a UDP datagram to the group ff02::1:3 (as if
/// from a neighbour) is dropped until a socket joins the group, delivered while it is
/// joined, dropped again after it leaves; the joins and leaves go out as MLDv2 reports.
fn test_mld() -> core::result::Result<(), String> {
    use super::wire::{finish, pseudo_sum6, sum};
    let g = Ipv6([0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 3]);
    let from = Ipv6([0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x99]);
    let pkt = |seq: u8| -> Vec<u8> {
        let mut u = alloc::vec![0u8, 99, 0x13, 0xb0, 0, 9, 0, 0, seq]; // 99 -> 5040
        let c = finish(sum(&u, pseudo_sum6(from, g, 17, u.len())));
        put16(&mut u, 6, if c == 0 { 0xffff } else { c });
        let mut p = alloc::vec![0u8; 40];
        p[0] = 0x60;
        put16(&mut p, 4, u.len() as u16);
        p[6] = 17;
        p[7] = 1;
        p[8..24].copy_from_slice(&from.0);
        p[24..40].copy_from_slice(&g.0);
        p.extend_from_slice(&u);
        p
    };
    let s = socket::socket_family(Family::V6, SockType::Dgram).map_err(es)?;
    socket::bind(s, SockAddr::new(Ipv6::UNSPECIFIED, 5040)).map_err(es)?;
    let inject = |seq: u8| {
        super::with(|st| super::ipv6::input(st, Some(0), &pkt(seq), super::wire::Mac::ZERO))
    };
    let got = |seq: u8| {
        socket::wait(s, POLLIN, 100 * MS);
        let mut b = [0u8; 8];
        matches!(socket::recvfrom(s, &mut b), Ok((1, _)) if b[0] == seq)
    };
    let reports0 = super::with(|st| st.ip6.mld.reports_out);
    inject(1);
    let before = got(1);
    socket::group6(s, g, Some(0), true).map_err(es)?;
    let member = super::with(|st| super::mld::member(&st.iface.links[0].v6, g));
    inject(2);
    let during = got(2);
    let reported = wait_for(1500, || super::with(|st| st.ip6.mld.reports_out > reports0));
    socket::group6(s, g, Some(0), false).map_err(es)?;
    inject(3);
    let after = got(3);
    let _ = socket::close(s);
    let reports = if wait_for(3000, || {
        super::with(|st| st.ip6.mld.reports_out >= reports0 + 2)
    }) {
        super::with(|st| st.ip6.mld.reports_out - reports0)
    } else {
        0
    };
    kprintln!(
        "[net] mld: ff02::1:3 on eth0 dropped before the join, delivered while joined, dropped after leaving; {reports} MLDv2 reports"
    );
    if before || !member || !during || after || !reported {
        return Err(alloc::format!(
            "before {before} member {member} during {during} after {after} reported {reported}"
        ));
    }
    Ok(())
}

/// Path MTU discovery over loopback, with loopback playing a path of a smaller MTU
/// (`iface::LO_MTU`): too-big packets are answered by ICMPv6 "packet too big" / ICMP
/// "fragmentation needed" as a router would. IPv6 at 1280: a TCP connection drops its MSS
/// to 1220 mid-stream and the bytes still arrive intact, then a 4000-byte UDP datagram
/// goes out in fragments of the path's size and comes back whole. IPv4 at 1000 the same
/// way (MSS 960). The cache is cleared afterwards.
fn test_pmtu() -> core::result::Result<(), String> {
    use core::sync::atomic::Ordering;
    let set_mtu = |m: u32| super::iface::LO_MTU.store(m, Ordering::Relaxed);
    let run =
        |fam: Family, lo: SockAddr, port: u16| -> core::result::Result<(u16, u32, usize), String> {
            let l = socket::socket_family(fam, SockType::Stream).map_err(es)?;
            let at = SockAddr::new(lo.ip, port);
            socket::bind(l, at).map_err(es)?;
            socket::listen(l, 2).map_err(es)?;
            let c =
                connect_fam(fam, at, 1000).map_err(|x| alloc::format!("connect {at}: {}", e(x)));
            let c = match c {
                Ok(c) => c,
                Err(m) => {
                    let _ = socket::close(l);
                    return Err(m);
                }
            };
            socket::wait(l, POLLIN, 1000 * MS);
            let (a, _) = socket::accept(l).map_err(|x| alloc::format!("accept: {}", e(x)))?;
            let r = pump(c, a, 64 * 1024, 10_000);
            let (mss, drops) = super::with(|st| {
                st.socks
                    .get(c.slot())
                    .map_or((0, 0), |s| (s.tcb.mss, s.tcb.pmtu_drops))
            });
            let mtu = super::with(|st| st.pmtu.mtu(lo.ip, st.now));
            for s in [c, a, l] {
                let _ = socket::close(s);
            }
            r?;
            // A big UDP datagram after the path MTU is known: fragments of that size.
            let u = socket::socket_family(fam, SockType::Dgram).map_err(es)?;
            let v = socket::socket_family(fam, SockType::Dgram).map_err(es)?;
            socket::bind(v, SockAddr::new(lo.ip, port + 1)).map_err(es)?;
            let big: Vec<u8> = (0..4000).map(|k| (k * 7) as u8).collect();
            let sent = socket::sendto(u, &big, SockAddr::new(lo.ip, port + 1));
            socket::wait(v, POLLIN, 500 * MS);
            let mut buf = alloc::vec![0u8; 4096];
            let got = socket::recvfrom(v, &mut buf);
            let _ = socket::close(u);
            let _ = socket::close(v);
            sent.map_err(|x| alloc::format!("udp send: {}", e(x)))?;
            let (n, _) = got.map_err(|x| alloc::format!("udp: {}", e(x)))?;
            if buf[..n] != big[..] {
                return Err(alloc::format!("udp: {n} bytes back, not the 4000 sent"));
            }
            Ok((mss, drops, mtu))
        };
    set_mtu(1280);
    let r6 = run(Family::V6, SockAddr::new(Ipv6::LOOPBACK, 0), 5030);
    set_mtu(1000);
    let r4 = run(Family::V4, SockAddr::new(Ipv4::LOCALHOST, 0), 5032);
    set_mtu(0);
    let learned = super::with(|st| {
        let n = st.pmtu.stats.learned;
        st.pmtu.clear();
        n
    });
    let (mss6, d6, m6) = r6.map_err(|m| alloc::format!("IPv6: {m}"))?;
    let (mss4, d4, m4) = r4.map_err(|m| alloc::format!("IPv4: {m}"))?;
    kprintln!(
        "[net] path mtu: IPv6 path 1280 -> tcp mss {mss6} ({d6} drop), 4000-byte udp in fragments ok; IPv4 path 1000 -> tcp mss {mss4} ({d4} drop), udp ok; {learned} learned"
    );
    if (m6, mss6, m4, mss4) != (1280, 1220, 1000, 960) || d6 == 0 || d4 == 0 {
        return Err(alloc::format!(
            "IPv6 mtu {m6} mss {mss6} drops {d6}, IPv4 mtu {m4} mss {mss4} drops {d4}"
        ));
    }
    Ok(())
}

/// What a connection negotiated, and its recovery counters: (wscale, sack, ts,
/// sack/recovery retransmissions, total retransmissions).
fn tcb_info(id: SocketId) -> (u8, bool, bool, u32, u32, u32, u32) {
    super::with(|st| {
        st.socks
            .get(id.slot())
            .map(|s| {
                let t = &s.tcb;
                (
                    t.rcv_wscale,
                    t.sack_ok,
                    t.ts_ok,
                    t.sack_retrans,
                    t.retransmits,
                    t.timeouts,
                    t.max_rto_ms,
                )
            })
            .unwrap_or_default()
    })
}

/// TCP over a lossy loopback (every `every`th packet dropped: data, ACKs, SYN and FIN
/// alike): the bytes must arrive intact, with window scaling, SACK and timestamps on.
fn test_lossy_tcp(every: u32, total: usize, port: u16) -> core::result::Result<(), String> {
    use super::iface::LO_DROP_EVERY;
    let before = super::with(|st| (st.stats.tcp_retrans_segs, st.iface.lo_stats.tx_dropped));
    LO_DROP_EVERY.store(every, core::sync::atomic::Ordering::Relaxed);
    let r = (|| {
        let l = socket::socket(SockType::Stream).map_err(es)?;
        socket::bind(l, SockAddr::new(Ipv4::LOCALHOST, port)).map_err(es)?;
        socket::listen(l, 4).map_err(es)?;
        let c = connect(SockAddr::new(Ipv4::LOCALHOST, port), 30_000)
            .map_err(|x| alloc::format!("connect: {}", e(x)))?;
        if socket::wait(l, POLLIN, 30_000 * MS) == 0 {
            return Err(String::from("accept: nothing queued"));
        }
        let (a, _) = socket::accept(l).map_err(es)?;
        let r = pump(c, a, total, 120_000);
        if r.is_err() {
            super::with(|st| {
                for id in [c, a] {
                    if let Some(k) = st.socks.get(id.slot()) {
                        kprintln!(
                            "[net] debug: {}",
                            super::tcp::debug_line(&k.tcb, k.tx.len(), st.now)
                        );
                    }
                }
            });
        }
        let info = tcb_info(c);
        socket::shutdown(c, socket::Shutdown::Write).map_err(es)?;
        socket::wait(a, POLLIN, 30_000 * MS);
        let mut b = [0u8; 8];
        let eof = socket::recv(a, &mut b);
        let _ = socket::close(a);
        let _ = socket::close(c);
        let _ = socket::close(l);
        let ns = r?;
        if eof != Ok(0) {
            return Err(alloc::format!("no EOF after FIN: {eof:?}"));
        }
        Ok((ns, info))
    })();
    LO_DROP_EVERY.store(0, core::sync::atomic::Ordering::Relaxed);
    let (ns, (ws, sack, ts, sack_rx, rexmit, timeouts, max_rto)) = r?;
    let (after, dropped) =
        super::with(|st| (st.stats.tcp_retrans_segs, st.iface.lo_stats.tx_dropped));
    kprintln!(
        "[net] tcp lossy loopback 1/{every}: {} KiB intact in {} ms ({} packets dropped, {} retransmissions, {} by SACK recovery, {timeouts} timeouts, longest RTO {max_rto} ms; wscale {ws} sack {} ts {})",
        total / 1024,
        ns / MS,
        dropped - before.1,
        after - before.0,
        sack_rx,
        if sack { "on" } else { "off" },
        if ts { "on" } else { "off" }
    );
    if after == before.0 || rexmit == 0 {
        return Err(String::from("no retransmissions?"));
    }
    if ws == 0 || !sack || !ts {
        return Err(String::from(
            "window scaling / SACK / timestamps not negotiated",
        ));
    }
    if sack_rx == 0 {
        return Err(String::from(
            "loss recovery never retransmitted from the scoreboard",
        ));
    }
    Ok(())
}

/// TIME_WAIT: the side that closes first parks the pair in the TIME_WAIT table (no socket
/// slot), and a new connection from the same local port reuses it.
fn test_time_wait() -> core::result::Result<(), String> {
    let l = socket::socket(SockType::Stream).map_err(es)?;
    socket::bind(l, SockAddr::new(Ipv4::LOCALHOST, 5004)).map_err(es)?;
    socket::listen(l, 4).map_err(es)?;
    let once = |port: u16| -> core::result::Result<SockAddr, String> {
        let c = socket::socket(SockType::Stream).map_err(es)?;
        socket::bind(c, SockAddr::new(Ipv4::LOCALHOST, port)).map_err(es)?;
        match socket::connect(c, SockAddr::new(Ipv4::LOCALHOST, 5004)) {
            Err(x) if x == EINPROGRESS => {}
            Ok(()) => {}
            Err(x) => return Err(alloc::format!("connect: {}", e(x))),
        }
        socket::wait(c, POLLOUT, 2000 * MS);
        socket::connect(c, SockAddr::new(Ipv4::LOCALHOST, 5004))
            .map_err(|x| alloc::format!("connect: {}", e(x)))?;
        socket::wait(l, POLLIN, 2000 * MS);
        let (a, _) = socket::accept(l).map_err(es)?;
        let local = socket::local_addr(c).map_err(es)?;
        // The client closes first (it ends up in TIME_WAIT), then the server.
        let _ = socket::close(c);
        socket::wait(a, POLLIN, 2000 * MS);
        let _ = socket::close(a);
        wait_for(2000, || {
            super::with(|st| {
                st.tw
                    .find(local, SockAddr::new(Ipv4::LOCALHOST, 5004))
                    .is_some()
            })
        });
        Ok(local)
    };
    let r = (|| {
        let used = socket::count();
        let first = once(45678)?;
        let parked = super::with(|st| {
            st.tw
                .find(first, SockAddr::new(Ipv4::LOCALHOST, 5004))
                .is_some()
        });
        if !parked {
            return Err(String::from("closed connection not in the TIME_WAIT table"));
        }
        if socket::count() > used {
            return Err(String::from("TIME_WAIT still holds a socket slot"));
        }
        // Same 4-tuple again: allowed, the TIME_WAIT entry is reused.
        once(45678)?;
        Ok(())
    })();
    let _ = socket::close(l);
    r?;
    kprintln!("[net] tcp TIME_WAIT: parked without a socket (60 s), same pair reused ok");
    Ok(())
}

/// IP fragments: a 20000-byte UDP datagram over loopback goes out as 14 fragments and comes
/// back whole; crafted fragments check the overlap, tiny-fragment, out-of-order and timeout
/// rules.
fn test_fragments() -> core::result::Result<(), String> {
    let before = super::with(|st| st.frags.stats);
    let a = socket::socket(SockType::Dgram).map_err(es)?;
    let b = socket::socket(SockType::Dgram).map_err(es)?;
    socket::bind(b, SockAddr::new(Ipv4::LOCALHOST, 5005)).map_err(es)?;
    let big: Vec<u8> = (0..20000).map(|k| pattern(k * 3)).collect();
    let r = (|| {
        socket::sendto(a, &big, SockAddr::new(Ipv4::LOCALHOST, 5005)).map_err(es)?;
        socket::wait(b, POLLIN, 1000 * MS);
        let mut buf = alloc::vec![0u8; 32768];
        let (n, _) = socket::recvfrom(b, &mut buf).map_err(|x| alloc::format!("recv: {}", e(x)))?;
        if buf[..n] != big[..] {
            return Err(alloc::format!("got {n} bytes, not the 20000 sent"));
        }
        // Crafted fragments, UDP from 127.0.0.1:7 to :5005.
        let frag = |id: u16, off: usize, more: bool, proto: u8, payload: &[u8]| -> Vec<u8> {
            let mut p = alloc::vec![0u8; 20 + payload.len()];
            p[0] = 0x45;
            put16(&mut p, 2, (20 + payload.len()) as u16);
            put16(&mut p, 4, id);
            put16(&mut p, 6, (off / 8) as u16 | if more { 0x2000 } else { 0 });
            p[8] = 64;
            p[9] = proto;
            p[12..16].copy_from_slice(&[127, 0, 0, 1]);
            p[16..20].copy_from_slice(&[127, 0, 0, 1]);
            let c = checksum(&p[..20]);
            put16(&mut p, 10, c);
            p[20..].copy_from_slice(payload);
            p
        };
        // A UDP datagram of 8 + 24 bytes, checksum 0 (none).
        let mut dg = alloc::vec![0u8; 32];
        put16(&mut dg, 0, 7);
        put16(&mut dg, 2, 5005);
        put16(&mut dg, 4, 32);
        for (k, x) in dg[8..].iter_mut().enumerate() {
            *x = b'a' + k as u8;
        }
        // Out of order (last fragment first): delivered.
        super::with(|st| {
            st.inject(&frag(0x7001, 16, false, 17, &dg[16..]));
            st.inject(&frag(0x7001, 0, true, 17, &dg[..16]));
        });
        socket::wait(b, POLLIN, 500 * MS);
        let (n, _) = socket::recvfrom(b, &mut buf)
            .map_err(|_| String::from("out-of-order fragments not reassembled"))?;
        if buf[..n] != dg[8..] {
            return Err(String::from("out-of-order reassembly: wrong bytes"));
        }
        // Overlap (bytes 8..24 sent twice, differently): the datagram is dropped.
        let mut evil = dg.clone();
        evil[20] = b'X';
        super::with(|st| {
            st.inject(&frag(0x7002, 0, true, 17, &dg[..16]));
            st.inject(&frag(0x7002, 8, true, 17, &evil[8..24]));
            st.inject(&frag(0x7002, 24, false, 17, &dg[24..]));
        });
        if socket::wait(b, POLLIN, 200 * MS) != 0 {
            return Err(String::from("an overlapping datagram was delivered"));
        }
        // TCP fragment at offset 8 (RFC 1858): refused.
        super::with(|st| st.inject(&frag(0x7003, 8, false, 6, &[0u8; 24])));
        // A lone first fragment times out.
        super::with(|st| {
            st.inject(&frag(0x7004, 0, true, 17, &dg[..16]));
            let later = st.now + super::frag::TIMEOUT + 1;
            st.frags.expire(later);
        });
        Ok(())
    })();
    let _ = socket::close(a);
    let _ = socket::close(b);
    r?;
    let after = super::with(|st| st.frags.stats);
    let (oks, overlaps, timeouts, creates) = (
        after.oks - before.oks,
        after.overlaps - before.overlaps,
        after.timeouts - before.timeouts,
        after.creates - before.creates,
    );
    let fails = after.fails - before.fails;
    kprintln!(
        "[net] ip fragments: 20000-byte UDP datagram as {creates} fragments, reassembled; out of order ok; overlap dropped; tiny TCP fragment refused; timeout ok ({oks} reassembled, {overlaps} overlap, {timeouts} timed out, {fails} failed)"
    );
    if oks != 2 || overlaps != 1 || timeouts != 1 || creates != 14 || fails < 3 {
        return Err(String::from("fragment counters don't add up"));
    }
    Ok(())
}

/// The dual-stack socket layer over loopback: an AF_INET6 wildcard listener takes both a
/// ::1 client and a 127.0.0.1 one (peer seen as IPv4); IPV6_V6ONLY lets an AF_INET listener
/// share the port, a dual-stack one may not; UDP over ::1; an AF_INET socket refuses ::1.
fn test_dual_stack() -> core::result::Result<(), String> {
    let lo6 = Ipv6::LOOPBACK;
    let l = socket::socket_family(Family::V6, SockType::Stream).map_err(es)?;
    socket::bind(l, SockAddr::new(Ipv6::UNSPECIFIED, 5010)).map_err(es)?;
    socket::listen(l, 4).map_err(es)?;
    let accept = |what: &str| {
        if socket::wait(l, POLLIN, 1000 * MS) == 0 {
            return Err(alloc::format!("{what}: nothing to accept"));
        }
        socket::accept(l).map_err(|x| alloc::format!("{what} accept: {}", e(x)))
    };
    // v6 client to [::1]:5010.
    let c6 = connect_fam(Family::V6, SockAddr::new(lo6, 5010), 1000)
        .map_err(|x| alloc::format!("connect [::1]:5010: {}", e(x)))?;
    let (a6, p6) = accept("v6")?;
    let total = 128 * 1024;
    let ns = pump(c6, a6, total, 5000)?;
    pump(a6, c6, 16 * 1024, 5000)?;
    let mss6 = tcb_mss(a6);
    let _ = socket::close(c6);
    let _ = socket::close(a6);
    // v4 client to the same dual-stack listener.
    let c4 = connect(SockAddr::new(Ipv4::LOCALHOST, 5010), 1000)
        .map_err(|x| alloc::format!("connect 127.0.0.1:5010: {}", e(x)))?;
    let (a4, p4) = accept("v4")?;
    pump(c4, a4, 8 * 1024, 5000)?;
    let _ = socket::close(c4);
    let _ = socket::close(a4);
    let _ = socket::close(l);
    if p6.ip != lo6.into() || p4.ip != Ipv4::LOCALHOST.into() {
        return Err(alloc::format!("peers {p6} / {p4}"));
    }
    if mss6 != super::tcp::OUR_MSS6 {
        return Err(alloc::format!("v6 mss {mss6}"));
    }
    // IPV6_V6ONLY: a v6-only [::]:5011 and 0.0.0.0:5011 coexist; dual-stack [::]:5012 can't
    // join a 0.0.0.0:5012.
    let v6o = socket::socket_family(Family::V6, SockType::Stream).map_err(es)?;
    socket::set_v6only(v6o, true).map_err(es)?;
    socket::bind(v6o, SockAddr::new(Ipv6::UNSPECIFIED, 5011)).map_err(es)?;
    let v4l = socket::socket(SockType::Stream).map_err(es)?;
    let shared = socket::bind(v4l, SockAddr::new(Ipv4::UNSPECIFIED, 5011));
    let v4b = socket::socket(SockType::Stream).map_err(es)?;
    socket::bind(v4b, SockAddr::new(Ipv4::UNSPECIFIED, 5012)).map_err(es)?;
    let ds = socket::socket_family(Family::V6, SockType::Stream).map_err(es)?;
    let clash = socket::bind(ds, SockAddr::new(Ipv6::UNSPECIFIED, 5012));
    // A v6-only socket can't reach IPv4; an AF_INET one can't reach IPv6.
    let v6o_c = socket::socket_family(Family::V6, SockType::Stream).map_err(es)?;
    socket::set_v6only(v6o_c, true).map_err(es)?;
    let to4 = socket::connect(v6o_c, SockAddr::new(Ipv4::LOCALHOST, 5011));
    let v4c = socket::socket(SockType::Stream).map_err(es)?;
    let to6 = socket::connect(v4c, SockAddr::new(lo6, 5011));
    for s in [v6o, v4l, v4b, ds, v6o_c, v4c] {
        let _ = socket::close(s);
    }
    if shared.is_err() || clash != Err(EADDRINUSE) || to4.is_ok() || to6 != Err(EAFNOSUPPORT) {
        return Err(alloc::format!(
            "v6only: share {shared:?}, clash {clash:?}, v6only->v4 {to4:?}, v4->v6 {to6:?}"
        ));
    }
    // UDP over ::1.
    let ua = socket::socket_family(Family::V6, SockType::Dgram).map_err(es)?;
    let ub = socket::socket_family(Family::V6, SockType::Dgram).map_err(es)?;
    socket::bind(ub, SockAddr::new(lo6, 5013)).map_err(es)?;
    socket::sendto(ua, b"milkOS udp6", SockAddr::new(lo6, 5013)).map_err(es)?;
    socket::wait(ub, POLLIN, 500 * MS);
    let mut buf = [0u8; 32];
    let got = socket::recvfrom(ub, &mut buf);
    let _ = socket::close(ua);
    let _ = socket::close(ub);
    let (n, from) = got.map_err(|x| alloc::format!("udp6: {}", e(x)))?;
    if &buf[..n] != b"milkOS udp6" || from.ip != lo6.into() {
        return Err(alloc::format!("udp6 got {n} bytes from {from}"));
    }
    kprintln!(
        "[net] dual-stack: {} KiB [::1]:{} -> [::]:5010 in {} us (mss {mss6}), 127.0.0.1:{} accepted too, V6ONLY ok, udp6 ok",
        total / 1024,
        p6.port,
        ns / 1000,
        p4.port
    );
    Ok(())
}

fn tcb_mss(id: SocketId) -> u16 {
    super::with(|st| st.socks.get(id.slot()).map_or(0, |s| s.tcb.mss))
}

fn test_loopback_udp() -> core::result::Result<(), String> {
    let a = socket::socket(SockType::Dgram).map_err(es)?;
    let b = socket::socket(SockType::Dgram).map_err(es)?;
    socket::bind(b, SockAddr::new(Ipv4::LOCALHOST, 5001)).map_err(es)?;
    let msg = b"milkOS udp over lo";
    socket::sendto(a, msg, SockAddr::new(Ipv4::LOCALHOST, 5001)).map_err(es)?;
    socket::wait(b, POLLIN, 500 * MS);
    let mut buf = [0u8; 64];
    let (n, from) =
        socket::recvfrom(b, &mut buf).map_err(|x| alloc::format!("recvfrom: {}", e(x)))?;
    let from_a = socket::local_addr(a).map_err(es)?;
    // Reply to the sender's ephemeral port.
    socket::sendto(b, b"pong", from).map_err(es)?;
    socket::wait(a, POLLIN, 500 * MS);
    let (n2, _) =
        socket::recvfrom(a, &mut buf[32..]).map_err(|x| alloc::format!("reply: {}", e(x)))?;
    let _ = socket::close(a);
    let _ = socket::close(b);
    if &buf[..n] != msg || from.port != from_a.port || &buf[32..32 + n2] != b"pong" {
        return Err("payload/address mismatch".into());
    }
    kprintln!(
        "[net] udp loopback: {} bytes {} -> 127.0.0.1:5001 and back ok",
        n,
        from
    );
    Ok(())
}

fn http_get(addr: SockAddr, path: &str) -> core::result::Result<Vec<u8>, String> {
    let s = connect(addr, 2000).map_err(|x| alloc::format!("connect: {}", e(x)))?;
    let req = alloc::format!("GET {path} HTTP/1.0\r\nHost: milkos\r\n\r\n");
    let r = (|| {
        let mut off = 0;
        while off < req.len() {
            match socket::send(s, &req.as_bytes()[off..]) {
                Ok(n) => off += n,
                Err(x) if x == EAGAIN => {
                    socket::wait(s, POLLOUT, 100 * MS);
                }
                Err(x) => return Err(alloc::format!("send: {}", e(x))),
            }
        }
        let mut out = Vec::new();
        let t0 = timer::now_ns();
        let mut buf = [0u8; 1024];
        loop {
            match socket::recv(s, &mut buf) {
                Ok(0) => return Ok(out),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(x) if x == EAGAIN => {
                    if timer::now_ns() - t0 > 3000 * MS {
                        return Err("timeout".into());
                    }
                    socket::wait(s, POLLIN, 100 * MS);
                }
                Err(x) => return Err(alloc::format!("recv: {}", e(x))),
            }
        }
    })();
    let _ = socket::close(s);
    r
}

fn test_echo(addr: SockAddr) -> core::result::Result<(), String> {
    let fam = if addr.ip.is_v6() {
        Family::V6
    } else {
        Family::V4
    };
    let s = connect_fam(fam, addr, 3000).map_err(|x| alloc::format!("connect {addr}: {}", e(x)))?;
    let total = 512 * 1024;
    let r = pump(s, s, total, 20_000);
    let rexmit = super::with(|st| {
        st.socks.get(s.slot()).map_or((0, 0, 0), |k| {
            (k.tcb.retransmits, k.tcb.srtt / 1000, k.tcb.mss)
        })
    });
    let _ = socket::close(s);
    let ns = r?;
    kprintln!(
        "[net] tcp echo via {}: {} KiB round trip in {} ms ({} KiB/s), mss {}, srtt {} us, {} retransmits",
        addr,
        total / 1024,
        ns / MS,
        kib_per_s(total, ns),
        rexmit.2,
        rexmit.1,
        rexmit.0
    );
    Ok(())
}

fn dump_proc() {
    for f in ["dev", "route", "arp", "tcp", "snmp"] {
        let path = alloc::format!("/proc/net/{f}");
        match crate::fs::read_to_string(&path) {
            Ok(t) => {
                kprintln!("[net] {path}:");
                for line in t.lines() {
                    kprintln!("    {line}");
                }
            }
            Err(x) => kprintln!("[net] {path}: {x:?}"),
        }
    }
}

/// Every boot (self-test or not, so the network is up when the shell is): wait up to 3 s for
/// DHCP on eth0 and print the lease. Nothing to wait for without a NIC or in a v6-only boot.
/// True if eth0 has an address.
pub fn wait_dhcp() -> bool {
    if super::v4_off() || !super::has_device() {
        return false;
    }
    let t0 = timer::now_ns();
    if !wait_for(3000, || super::config().configured()) {
        kprintln!("[net] dhcp: no answer in 3 s, eth0 unconfigured (DHCP keeps trying)");
        return false;
    }
    let c = super::config();
    let (server, lease) = super::with(|st| (st.dhcp.server, st.dhcp.lease_s));
    kprintln!(
        "[net] dhcp: {} mask {} gw {} dns {} from {} lease {} s ({} ms)",
        c.addr,
        c.mask,
        c.gateway.unwrap_or_default(),
        c.dns.unwrap_or_default(),
        server,
        lease,
        (timer::now_ns() - t0) / MS
    );
    true
}

/// Every boot without the network self-test: DHCP ([`wait_dhcp`]), then up to 2 s for eth0's
/// IPv6 link-local address and 2 s more for its SLAAC one, so the shell doesn't come up while
/// IPv6 autoconfiguration is still printing its lines (the self-test waited for both too).
pub fn wait_up() {
    wait_dhcp();
    if !super::has_device() {
        return;
    }
    let v6 = |o: Option<super::ipv6::Origin>| {
        super::with(|st| {
            let v = &st.iface.eth0().v6;
            match o {
                None => v.link_local().is_some(),
                Some(o) => v.addr(o).is_some(),
            }
        })
    };
    if wait_for(2000, || v6(None)) {
        wait_for(2000, || v6(Some(super::ipv6::Origin::Slaac)));
    }
}

/// Run the network self-test. Prints `[net] selftest ok` or `[net] selftest FAILED (n)`.
pub fn run() {
    let cfg = read_config();
    let mut fails = 0u32;
    let mut check = |name: &str, r: core::result::Result<(), String>| {
        if let Err(m) = r {
            kprintln!("[net] FAIL {name}: {m}");
            fails += 1;
        }
    };

    // 1. DHCP (not in a v6-only boot: there IPv4 must stay off the wire).
    let mut have_ip = false;
    let v6only = super::v4_off();
    if v6only && super::has_device() {
        check("v6-only", test_v4_off());
    } else if super::has_device() {
        have_ip = wait_dhcp();
        if !have_ip {
            check(
                "dhcp",
                Err("no answer in 3 s, eth0 unconfigured (DHCP keeps trying)".into()),
            );
        }
    } else {
        kprintln!("[net] no NIC: loopback tests only");
    }

    // 2. Ping the gateway (slirp answers itself). With `restrict=on` slirp sends no router
    // option, so fall back to the DHCP server (the same 10.0.2.2).
    let gw = super::config()
        .gateway
        .or_else(|| Some(super::with(|st| st.dhcp.server)).filter(|s| !s.is_unspecified()));
    if have_ip && let Some(gw) = gw {
        let mut ok = 0;
        for seq in 1..=3u16 {
            match socket::ping(gw, seq, 1000 * MS) {
                Ok(r) => {
                    ok += 1;
                    kprintln!(
                        "[net] ping {}: {} bytes seq={} ttl={} time={}.{:03} ms",
                        r.from,
                        r.bytes,
                        r.seq,
                        r.ttl,
                        r.rtt_ns / MS,
                        r.rtt_ns % MS / 1000
                    );
                }
                Err(x) => kprintln!("[net] ping {gw} seq={seq}: {}", e(x)),
            }
        }
        check(
            "ping",
            if ok == 3 {
                Ok(())
            } else {
                Err(alloc::format!("{ok}/3 replies"))
            },
        );
    }

    // 2b. IPv6: link-local and SLAAC addresses, echo to ::1 and to slirp's host.
    if super::has_device() {
        check("ipv6", test_ipv6());
        check("ipv6 extension headers", test_ipv6_ext());
        check("mld", test_mld());
    }

    // 3. Loopback.
    check("udp loopback", test_loopback_udp());
    check("tcp loopback", test_loopback_tcp());
    check("dual-stack", test_dual_stack());
    check("tcp lossy 1/7", test_lossy_tcp(7, 256 * 1024, 5002));
    if cfg.heavy_loss {
        check("tcp lossy 1/3", test_lossy_tcp(3, 32 * 1024, 5003));
    }
    check("tcp time-wait", test_time_wait());
    check("ip fragments", test_fragments());
    check("ipv6 fragments", test_fragments6());
    check("path mtu", test_pmtu());
    check(
        "refused",
        match connect(SockAddr::new(Ipv4::LOCALHOST, 9), 1000) {
            Err(x) if x == ECONNREFUSED => {
                kprintln!("[net] connect 127.0.0.1:9: ECONNREFUSED (RST) ok");
                Ok(())
            }
            Ok(s) => {
                let _ = socket::close(s);
                Err("connected?!".into())
            }
            Err(x) => Err(alloc::format!("got {}", e(x))),
        },
    );
    // The kernel HTTP server only when asked (net-test.sh), else userland httpd owns :80.
    if let Some(port) = cfg.khttpd {
        check(
            "kernel httpd",
            services::enable(port)
                .map_err(|x| alloc::format!("enable :{port}: {}", e(x)))
                .and_then(|_| http_get(SockAddr::new(Ipv4::LOCALHOST, port), "/"))
                .and_then(|b| {
                    let t = String::from_utf8_lossy(&b);
                    if t.starts_with("HTTP/1.0 200 OK") && t.contains("Hello from milkOS") {
                        kprintln!(
                            "[net] http GET 127.0.0.1:{port}/: 200 OK, {} bytes",
                            b.len()
                        );
                        Ok(())
                    } else {
                        Err(alloc::format!("bad reply: {:?}", t.lines().next()))
                    }
                }),
        );
    }

    // IPv6 to the host: no IPv4 needed (a v6-only boot runs this too).
    if super::has_device()
        && let Some(addr) = cfg.echo6
    {
        check("tcp echo over IPv6", test_echo(addr));
    }

    // 4. DNS policy (no packets for any of these). Online, example.com would be a real
    // lookup, so only the offline rule is checked then (it must still refuse the name).
    let refused = if super::internet::online() {
        if dns::offline_allows("example.com") {
            Ok(Ipv4::LOCALHOST)
        } else {
            Err(EPERM)
        }
    } else {
        dns::resolve("example.com", 100)
    };
    check(
        "dns policy",
        match (
            dns::resolve("localhost", 100),
            refused,
            dns::resolve("10.0.2.2", 100),
        ) {
            (Ok(a), Err(b), Ok(c))
                if a == Ipv4::LOCALHOST && b == EPERM && c == Ipv4::new(10, 0, 2, 2) =>
            {
                kprintln!(
                    "[net] dns: localhost -> 127.0.0.1, example.com refused (EPERM, policy), literal ok"
                );
                Ok(())
            }
            r => Err(alloc::format!("{r:?}")),
        },
    );

    // 5. Host-side tests, only with the test config.
    if have_ip {
        if let (Some(server), Some(name)) = (cfg.dns, cfg.name.as_deref()) {
            check(
                "dns",
                dns::set_server(server)
                    .map_err(|x| alloc::format!("set_server: {}", e(x)))
                    .and_then(|_| {
                        let t0 = timer::now_ns();
                        let ip = dns::resolve(name, 2000)
                            .map_err(|x| alloc::format!("{name}: {}", e(x)))?;
                        kprintln!(
                            "[net] dns: {name} -> {ip} via {server} ({} ms)",
                            (timer::now_ns() - t0) / MS
                        );
                        match dns::resolve("nope.milk.test", 2000) {
                            Err(x) if x == ENOENT => {
                                kprintln!("[net] dns: nope.milk.test -> NXDOMAIN ok")
                            }
                            r => return Err(alloc::format!("nope.milk.test: {r:?}")),
                        }
                        match cfg.expect {
                            Some(want) if want != ip => Err(alloc::format!("expected {want}")),
                            _ => Ok(()),
                        }
                    }),
            );
        }
        if let Some(addr) = cfg.echo {
            check("tcp echo", test_echo(addr));
        }

        if let Some(addr) = cfg.refused {
            check(
                "host refused",
                match connect(addr, 3000) {
                    Err(x) if x == ECONNREFUSED => {
                        kprintln!("[net] connect {addr}: ECONNREFUSED ok");
                        Ok(())
                    }
                    Ok(s) => {
                        let _ = socket::close(s);
                        Err("connected?!".into())
                    }
                    Err(x) => Err(alloc::format!("got {}", e(x))),
                },
            );
        }
    }

    // Let closing connections finish so the listing is tidy, then show /proc/net.
    wait_for(300, || false);
    dump_proc();
    let _ = services::served();
    if fails == 0 {
        kprintln!("[net] selftest ok");
    } else {
        kprintln!("[net] selftest FAILED ({fails})");
    }
}
