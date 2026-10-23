//! The commands that talk to another machine: `ping` (ICMP echo through a Linux-style ping
//! socket), `get` (HTTP/1.1 GET through rt::web), `lookup` (the kernel's resolver),
//! `connect` and `listen` (stdin to a socket, the socket to stdout).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use cmdkit::args::{Parsed, number};
use cmdkit::net as kit;
use rt::net::{self, POLLERR, POLLHUP, POLLIN, PollFd, SockAddr};
use rt::{eprintln, print, println, sys, web};

// ---- ping -----------------------------------------------------------------------------------

fn ms3(ns: u64) -> String {
    format!("{}.{:03}", ns / 1_000_000, ns % 1_000_000 / 1000)
}

fn csum(b: &[u8]) -> u16 {
    let mut s: u32 = 0;
    for c in b.chunks(2) {
        s += u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]) as u32;
    }
    while s > 0xffff {
        s = (s & 0xffff) + (s >> 16);
    }
    !(s as u16)
}

/// `network ping [--count N] [--interval MS] [--size BYTES] [--timeout MS] [-4|-6] HOST`.
/// Defaults: 4 pings, 1000 ms apart, 56 data bytes, 1000 ms timeout. IPv4 unless --ipv6 or
/// an IPv6 address (so plain `network ping localhost` stays IPv4, as ping does on Linux).
pub fn ping(p: &Parsed, host: &str, want: net::Want) -> i32 {
    let num = |long: &str, dflt: u64| -> Result<u64, i32> {
        match p.value(long) {
            None => Ok(dflt),
            Some(v) => number("network ping", long, v).map_err(|e| {
                eprintln!("{e}");
                2
            }),
        }
    };
    let (count, interval, size, wait) = match (
        num("count", 4),
        num("interval", 1000),
        num("size", 56),
        num("timeout", 1000),
    ) {
        (Ok(c), Ok(i), Ok(s), Ok(w)) => (c as u32, i, s.min(1400) as usize, w),
        _ => return 2,
    };
    let pick = if want == net::Want::Any {
        net::Want::V4
    } else {
        want
    };
    let found = net::lookup_all(host, 0, pick).or_else(|e| {
        if want == net::Want::Any {
            net::lookup_all(host, 0, net::Want::Any)
        } else {
            Err(e)
        }
    });
    let dst = match found {
        Ok(v) => v[0],
        Err(e) => {
            eprintln!("network ping: {e}");
            return 2;
        }
    };
    let v6 = dst.ip.is_v6();
    let (fam, proto) = if v6 {
        (net::AF_INET6, net::IPPROTO_ICMPV6)
    } else {
        (net::AF_INET, net::IPPROTO_ICMP)
    };
    let fd = match net::socket_family(fam, net::SOCK_DGRAM, proto) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("network ping: can't open a ping socket: {e}");
            return 1;
        }
    };
    let _ = net::set_recv_timeout(fd, wait);
    let name = format!("{}", dst.ip);
    let (req, rep) = if v6 { (128u8, 129u8) } else { (8, 0) };
    println!(
        "PING {name} {size}({}) bytes of data.",
        size + if v6 { 48 } else { 28 }
    );
    let t_start = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let (mut sent, mut got) = (0u32, 0u32);
    let (mut min, mut max, mut sum) = (u64::MAX, 0u64, 0u64);
    let mut pkt = alloc::vec![0u8; 8 + size];
    let mut buf = alloc::vec![0u8; 8 + size + 64];
    for seq in 1..=count {
        pkt[0] = req; // echo request
        pkt[1] = 0;
        pkt[2..4].fill(0);
        pkt[4..6].fill(0); // identifier: the kernel sets it
        pkt[6..8].copy_from_slice(&(seq as u16).to_be_bytes());
        let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
        for (k, x) in pkt[8..].iter_mut().enumerate() {
            *x = if k < 8 { t0.to_be_bytes()[k] } else { k as u8 };
        }
        let c = csum(&pkt);
        pkt[2..4].copy_from_slice(&c.to_be_bytes());
        sent += 1;
        if let Err(e) = net::sendto(fd, &pkt, &dst) {
            println!("network ping: couldn't send: {e}");
            continue;
        }
        loop {
            match net::recvfrom(fd, &mut buf) {
                Ok((n, from)) if n >= 8 && buf[0] == rep && buf[6..8] == pkt[6..8] => {
                    let rtt = sys::clock_ns(sys::CLOCK_MONOTONIC) - t0;
                    println!(
                        "{} bytes from {}: icmp_seq={seq} time={} ms",
                        n,
                        from.ip,
                        ms3(rtt)
                    );
                    got += 1;
                    min = min.min(rtt);
                    max = max.max(rtt);
                    sum += rtt;
                    break;
                }
                Ok(_) => continue, // an older reply or something else: keep waiting
                Err(e) if e.0 == 11 => {
                    println!("Request timeout for icmp_seq {seq}");
                    break;
                }
                Err(e) => {
                    println!("network ping: couldn't receive: {e}");
                    break;
                }
            }
        }
        if seq < count {
            let spent = (sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000;
            sys::sleep_ms(interval.saturating_sub(spent));
        }
    }
    let total_ms = (sys::clock_ns(sys::CLOCK_MONOTONIC) - t_start) / 1_000_000;
    println!("--- {name} ping statistics ---");
    println!(
        "{sent} packets transmitted, {got} received, {}% packet loss, time {total_ms}ms",
        ((sent - got) * 100).checked_div(sent).unwrap_or(0)
    );
    if got > 0 {
        println!(
            "rtt min/avg/max = {}/{}/{} ms",
            ms3(min),
            ms3(sum / got as u64),
            ms3(max)
        );
    }
    let _ = sys::close(fd);
    if got > 0 { 0 } else { 1 }
}

// ---- get, lookup ----------------------------------------------------------------------------

/// `network get [--headers] [--save FILE [--replace]] URL`: the body to stdout (or FILE), a
/// summary line ("network get: 200 OK, 1234 bytes from 10.0.2.2:80 in 5 ms") to stderr.
/// HTTP/1.1 through rt::web: redirects followed (each one said on stderr), gzip / deflate
/// undone; https:// over TLS 1.3 (rt::tls, the certificate checked).
pub fn get(p: &Parsed, url: &str, want: net::Want) -> i32 {
    let u = match web::Url::parse(url) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("network get: {url}: {e}");
            return 2;
        }
    };
    let save = p.value("save");
    if let Some(f) = save
        && !p.has("replace")
        && sys::stat_path(f, false).is_ok()
    {
        eprintln!("network get: {f} already exists (add --replace to overwrite it)");
        return 1;
    }
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let mut n = web::RtNet::new(want);
    let o = web::Options {
        user_agent: alloc::format!("{} (network get)", web::USER_AGENT),
        accept: String::from("*/*"),
        ..web::Options::default()
    };
    let r = match web::get(&mut n, &u, &o) {
        Ok(r) => r,
        Err(f) => {
            for r in &f.redirects {
                eprintln!("network get: redirected to {r}");
            }
            eprintln!("network get: {}", f.error);
            return match f.error {
                web::Error::Url(_) => 2,
                _ => 1,
            };
        }
    };
    let ms = (sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000;
    for r in &r.redirects {
        eprintln!("network get: redirected to {r}");
    }
    if p.has("headers") {
        let _ = sys::write_all(1, r.head_text().as_bytes());
        println!();
    }
    let body = &r.body;
    match save {
        Some(f) => {
            let w = sys::open_mode(
                f,
                sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
                0o644,
            )
            .and_then(|fd| {
                let r = sys::write_all(fd, body);
                let _ = sys::close(fd);
                r
            });
            if let Err(e) = w {
                eprintln!("network get: {}", filekit::words::say(f, e.0));
                return 1;
            }
        }
        None => {
            let _ = sys::write_all(1, body);
            if !body.ends_with(b"\n") && !body.is_empty() {
                println!();
            }
        }
    }
    let from = n
        .peer
        .map(|a| format!("{a}"))
        .unwrap_or_else(|| r.url.host_port());
    eprintln!(
        "network get: {} {}, {} bytes{} from {from} in {ms} ms{}",
        r.status,
        r.reason,
        body.len(),
        r.encoding
            .map(|e| format!(" ({e}, {} on the wire)", r.wire_bytes))
            .unwrap_or_default(),
        save.map(|f| format!(", saved to {f}")).unwrap_or_default()
    );
    if (200..300).contains(&r.status) { 0 } else { 1 }
}

/// `network lookup NAME [-4|-6]`: every address, IPv4 first (the kernel's resolver: DNS
/// with the internet on; offline only numeric addresses, localhost, NAME.local and .test).
pub fn lookup(name: &str, want: net::Want) -> i32 {
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    match net::resolve_all(name, want) {
        Ok(v) => {
            let ms = (sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000;
            let text: Vec<String> = v.iter().map(|a| format!("{}", a.ip)).collect();
            let six: Vec<bool> = v.iter().map(|a| a.ip.is_v6()).collect();
            print!("{}", kit::lookup_text(name, &text, &six));
            eprintln!(
                "network lookup: {} address{} in {ms} ms",
                v.len(),
                if v.len() == 1 { "" } else { "es" }
            );
            0
        }
        Err(e) => {
            eprintln!("network lookup: {e}");
            1
        }
    }
}

// ---- connect, listen ------------------------------------------------------------------------

/// `*:PORT` (IPv4), `[::]:PORT (IPv6 only)` or `[::]:PORT (IPv6 and IPv4)`.
fn listen_name(want: net::Want, port: u16) -> String {
    match want {
        net::Want::V4 => format!("*:{port}"),
        net::Want::V6 => format!("[::]:{port} (IPv6 only)"),
        net::Want::Any => format!("[::]:{port} (IPv6 and IPv4)"),
    }
}

fn fail(cmd: &str, what: &str, e: rt::Errno) -> i32 {
    eprintln!("network {cmd}: {what}: {e}");
    1
}

/// `network connect HOST PORT [--udp]`: every address of HOST is tried in turn (IPv6 first,
/// as getaddrinfo orders them); UDP sends to the first.
pub fn connect(host: &str, port: &str, udp: bool, want: net::Want) -> i32 {
    let port = match kit::port(port) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("network connect: {e}");
            return 2;
        }
    };
    let ty = if udp {
        net::SOCK_DGRAM
    } else {
        net::SOCK_STREAM
    };
    let addrs = match net::lookup_all(host, port, want) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("network connect: {e}");
            return 2;
        }
    };
    let conn = if udp {
        match net::socket_for(&addrs[0], ty) {
            Ok(s) => s,
            Err(e) => return fail("connect", "can't open a socket", e),
        }
    } else {
        match net::connect_any(&addrs, ty) {
            Ok((s, _)) => s,
            Err(e) => {
                eprintln!("network connect: {e}");
                return 1;
            }
        }
    };
    relay("connect", conn, udp, udp.then_some(addrs[0]))
}

/// `network listen PORT [--udp]`: both families on [::] (-4 IPv4 only, -6 IPv6 only); accept
/// one TCP connection, or answer the first UDP sender.
pub fn listen(port: &str, udp: bool, want: net::Want) -> i32 {
    let port = match kit::port(port) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("network listen: {e}");
            return 2;
        }
    };
    let ty = if udp {
        net::SOCK_DGRAM
    } else {
        net::SOCK_STREAM
    };
    let (s, me) = if want == net::Want::V4 {
        (net::socket(ty), SockAddr::v4([0; 4], port))
    } else {
        (net::socket6(ty), SockAddr::any6(port))
    };
    let s = match s {
        Ok(s) => s,
        Err(e) => return fail("listen", "can't open a socket", e),
    };
    if want == net::Want::V6
        && let Err(e) = net::set_v6only(s, true)
    {
        return fail("listen", "IPv6 only", e);
    }
    let _ = net::set_reuseaddr(s);
    if let Err(e) = net::bind(s, &me) {
        return fail("listen", &format!("port {port}"), e);
    }
    let conn = if udp {
        eprintln!(
            "network listen: listening on udp {}",
            listen_name(want, port)
        );
        s
    } else {
        if let Err(e) = net::listen(s, 1) {
            return fail("listen", &format!("port {port}"), e);
        }
        eprintln!(
            "network listen: listening on tcp {}",
            listen_name(want, port)
        );
        match net::accept(s) {
            Ok((c, from)) => {
                eprintln!("network listen: connection from {from}");
                let _ = sys::close(s);
                c
            }
            Err(e) => return fail("listen", "accept", e),
        }
    };
    relay("listen", conn, udp, None)
}

/// Copy stdin to `conn` and `conn` to stdout until the peer closes (TCP) or stdin ends.
/// ^D on an empty line ends the input (TCP: half-close, then wait for the peer to finish).
fn relay(cmd: &str, conn: i32, udp: bool, mut peer: Option<SockAddr>) -> i32 {
    let mut stdin_open = true;
    let mut buf = [0u8; 4096];
    loop {
        let mut fds = [
            PollFd {
                fd: if stdin_open { 0 } else { -1 },
                events: POLLIN,
                revents: 0,
            },
            PollFd {
                fd: conn,
                events: POLLIN,
                revents: 0,
            },
        ];
        if let Err(e) = net::poll(&mut fds, None) {
            return fail(cmd, "poll", e);
        }
        if fds[1].revents & (POLLIN | POLLERR | POLLHUP) != 0 {
            let r = if udp {
                net::recvfrom(conn, &mut buf).map(|(n, from)| {
                    if peer.is_none() {
                        eprintln!("network {cmd}: datagram from {from}");
                        peer = Some(from);
                    }
                    n
                })
            } else {
                net::recv(conn, &mut buf)
            };
            match r {
                Ok(0) if !udp => return 0, // peer closed
                Ok(n) => {
                    let _ = sys::write_all(1, &buf[..n]);
                }
                Err(e) if e.0 == 11 => {}
                Err(e) => return fail(cmd, "receive", e),
            }
        }
        // A pipe at its end says POLLHUP, not POLLIN: read it then too (and get the 0).
        if stdin_open && fds[0].revents & (POLLIN | POLLHUP | POLLERR) != 0 {
            match sys::read(0, &mut buf) {
                Ok(0) | Err(_) => {
                    stdin_open = false;
                    if udp {
                        return 0;
                    }
                    let _ = net::shutdown(conn, 1); // FIN; keep reading the answer
                }
                Ok(n) => {
                    let r = match (udp, peer) {
                        (true, Some(p)) => net::sendto(conn, &buf[..n], &p).map(|_| ()),
                        (true, None) => {
                            eprintln!(
                                "network {cmd}: nobody to send to yet (waiting for a datagram)"
                            );
                            Ok(())
                        }
                        (false, _) => net::send(conn, &buf[..n]).map(|_| ()),
                    };
                    if let Err(e) = r {
                        return fail(cmd, "send", e);
                    }
                }
            }
        }
    }
}
