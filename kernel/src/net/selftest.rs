//! Boot self-test of the network stack (thread context, after `net::init`).
//!
//! Always (no host services needed): DHCP on eth0 (3 s timeout, boot goes on without it),
//! ping the gateway (10.0.2.2, answered by qemu's slirp itself), UDP and TCP over loopback,
//! connection refused, an HTTP GET to our own server on :80, the DNS policy checks.
//!
//! With a test config passed by `scripts/net-test.sh` as fw_cfg file `opt/milkos/nettest`
//! (`echo=IP:PORT dns=IP:PORT name=NAME expect=IP refused=IP:PORT`): a TCP echo round trip
//! against the host's echo server, a DNS lookup against the host's test DNS server, and a
//! refused connection to a host port nobody listens on. Only addresses from that config are
//! contacted (in practice the Mac's loopback, seen as 10.0.2.2).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::errno::{self, *};
use super::socket::{self, POLLIN, POLLOUT, SockType, SocketId};
use super::wire::{Ipv4, SockAddr};
use super::{dns, services};
use crate::fs::errno::{Errno, Result};
use crate::{fwcfg, kprintln, sched, timer};

const MS: u64 = 1_000_000;

#[derive(Default)]
struct Config {
    echo: Option<SockAddr>,
    dns: Option<SockAddr>,
    name: Option<String>,
    expect: Option<Ipv4>,
    refused: Option<SockAddr>,
}

fn parse_addr(s: &str) -> Option<SockAddr> {
    let (ip, port) = s.split_once(':')?;
    Some(SockAddr::new(Ipv4::parse(ip)?, port.parse().ok()?))
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
            Some(("dns", v)) => c.dns = parse_addr(v),
            Some(("name", v)) => c.name = Some(String::from(v)),
            Some(("expect", v)) => c.expect = Ipv4::parse(v),
            Some(("refused", v)) => c.refused = parse_addr(v),
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
    let s = socket::socket(SockType::Stream)?;
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

/// Drop rate of the lossy-loopback test (every Nth packet). 3 passes too, just slower.
const LOSSY_EVERY: u32 = 7;

/// TCP over a lossy loopback (every Nth packet dropped: data, ACKs, SYN and FIN alike).
fn test_lossy_tcp() -> core::result::Result<(), String> {
    use super::iface::LO_DROP_EVERY;
    let before = super::with(|st| st.stats.tcp_retrans_segs);
    LO_DROP_EVERY.store(LOSSY_EVERY, core::sync::atomic::Ordering::Relaxed);
    let r = (|| {
        let l = socket::socket(SockType::Stream).map_err(es)?;
        socket::bind(l, SockAddr::new(Ipv4::LOCALHOST, 5002)).map_err(es)?;
        socket::listen(l, 4).map_err(es)?;
        let c = connect(SockAddr::new(Ipv4::LOCALHOST, 5002), 10_000)
            .map_err(|x| alloc::format!("connect: {}", e(x)))?;
        if socket::wait(l, POLLIN, 10_000 * MS) == 0 {
            return Err(String::from("accept: nothing queued"));
        }
        let (a, _) = socket::accept(l).map_err(es)?;
        let total = 64 * 1024;
        let r = pump(c, a, total, 60_000);
        socket::shutdown(c, socket::Shutdown::Write).map_err(es)?;
        socket::wait(a, POLLIN, 10_000 * MS);
        let mut b = [0u8; 8];
        let eof = socket::recv(a, &mut b);
        let _ = socket::close(a);
        let _ = socket::close(c);
        let _ = socket::close(l);
        let ns = r?;
        if eof != Ok(0) {
            return Err(alloc::format!("no EOF after FIN: {eof:?}"));
        }
        Ok((total, ns))
    })();
    LO_DROP_EVERY.store(0, core::sync::atomic::Ordering::Relaxed);
    let (total, ns) = r?;
    let (after, dropped) =
        super::with(|st| (st.stats.tcp_retrans_segs, st.iface.lo_stats.tx_dropped));
    kprintln!(
        "[net] tcp lossy loopback: {} KiB intact in {} ms with every {}th packet dropped ({} dropped, {} retransmissions)",
        total / 1024,
        ns / MS,
        LOSSY_EVERY,
        dropped,
        after - before
    );
    if after == before {
        return Err(String::from("no retransmissions?"));
    }
    Ok(())
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
    let s = connect(addr, 3000).map_err(|x| alloc::format!("connect {addr}: {}", e(x)))?;
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

    // 1. DHCP.
    let mut have_ip = false;
    if super::has_device() {
        let t0 = timer::now_ns();
        have_ip = wait_for(3000, || super::config().configured());
        let c = super::config();
        if have_ip {
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
        } else {
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

    // 3. Loopback.
    check("udp loopback", test_loopback_udp());
    check("tcp loopback", test_loopback_tcp());
    check("tcp lossy", test_lossy_tcp());
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
    check(
        "http self",
        http_get(SockAddr::new(Ipv4::LOCALHOST, 80), "/").and_then(|b| {
            let t = String::from_utf8_lossy(&b);
            if t.starts_with("HTTP/1.0 200 OK") && t.contains("Hello from milkOS") {
                kprintln!("[net] http GET 127.0.0.1:80/: 200 OK, {} bytes", b.len());
                Ok(())
            } else {
                Err(alloc::format!("bad reply: {:?}", t.lines().next()))
            }
        }),
    );

    // 4. DNS policy (no packets for any of these).
    check(
        "dns policy",
        match (
            dns::resolve("localhost", 100),
            dns::resolve("example.com", 100),
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
