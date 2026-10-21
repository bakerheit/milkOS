//! nc: copy stdin to a socket and the socket to stdout.
//!   nc [-4|-6] [-u] HOST PORT    connect (TCP), or send to (UDP); every address of HOST
//!                                is tried in turn (IPv6 first, as getaddrinfo orders them)
//!   nc -l [-4|-6] [-u] PORT      listen (both families on [::], -4 IPv4 only, -6 IPv6
//!                                only): accept one TCP connection, or answer the first UDP
//!                                sender
//! ^D on an empty line ends the input (TCP: half-close, then wait for the peer to finish).
//! Waits on stdin and the socket together with ppoll.

#![no_std]
#![no_main]

extern crate alloc;

use rt::net::{self, POLLERR, POLLHUP, POLLIN, PollFd, SockAddr};
use rt::{eprintln, sys};

rt::main!(main);

fn usage() -> i32 {
    eprintln!("usage: nc [-4|-6] [-u] HOST PORT | nc -l [-4|-6] [-u] PORT");
    2
}

/// `*:PORT` (IPv4), `[::]:PORT (IPv6 only)` or `[::]:PORT (IPv6 and IPv4)`.
fn listen_name(want: net::Want, port: u16) -> alloc::string::String {
    match want {
        net::Want::V4 => alloc::format!("*:{port}"),
        net::Want::V6 => alloc::format!("[::]:{port} (IPv6 only)"),
        net::Want::Any => alloc::format!("[::]:{port} (IPv6 and IPv4)"),
    }
}

fn main(args: &rt::Args) -> i32 {
    let (mut listen, mut udp) = (false, false);
    let mut want = net::Want::Any;
    let mut pos = alloc::vec::Vec::new();
    for a in args.argv.iter().skip(1) {
        match *a {
            "-l" => listen = true,
            "-u" => udp = true,
            "-4" => want = net::Want::V4,
            "-6" => want = net::Want::V6,
            "-lu" | "-ul" => {
                listen = true;
                udp = true;
            }
            p if !p.starts_with('-') => pos.push(p),
            _ => return usage(),
        }
    }
    let ty = if udp {
        net::SOCK_DGRAM
    } else {
        net::SOCK_STREAM
    };
    let fail = |what: &str, e: rt::Errno| {
        eprintln!("nc: {what}: {e}");
        1
    };
    let mut peer: Option<SockAddr> = None;
    let conn = if listen {
        let [port] = pos[..] else { return usage() };
        let Ok(port) = port.parse::<u16>() else {
            return usage();
        };
        let (s, me) = if want == net::Want::V4 {
            (net::socket(ty), SockAddr::v4([0; 4], port))
        } else {
            (net::socket6(ty), SockAddr::any6(port))
        };
        let s = match s {
            Ok(s) => s,
            Err(e) => return fail("socket", e),
        };
        if want == net::Want::V6
            && let Err(e) = net::set_v6only(s, true)
        {
            return fail("IPV6_V6ONLY", e);
        }
        let _ = net::set_reuseaddr(s);
        if let Err(e) = net::bind(s, &me) {
            return fail("bind", e);
        }
        if udp {
            eprintln!("nc: listening on udp {}", listen_name(want, port));
            s
        } else {
            if let Err(e) = net::listen(s, 1) {
                return fail("listen", e);
            }
            eprintln!("nc: listening on tcp {}", listen_name(want, port));
            match net::accept(s) {
                Ok((c, from)) => {
                    eprintln!("nc: connection from {from}");
                    let _ = sys::close(s);
                    c
                }
                Err(e) => return fail("accept", e),
            }
        }
    } else {
        let [host, port] = pos[..] else {
            return usage();
        };
        let Ok(port) = port.parse::<u16>() else {
            return usage();
        };
        let addrs = match net::lookup_all(host, port, want) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("nc: {e}");
                return 2;
            }
        };
        if udp {
            peer = Some(addrs[0]);
            match net::socket_for(&addrs[0], ty) {
                Ok(s) => s,
                Err(e) => return fail("socket", e),
            }
        } else {
            match net::connect_any(&addrs, ty) {
                Ok((s, _)) => s,
                Err(e) => {
                    eprintln!("nc: {e}");
                    return 1;
                }
            }
        }
    };

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
            return fail("poll", e);
        }
        if fds[1].revents & (POLLIN | POLLERR | POLLHUP) != 0 {
            let r = if udp {
                net::recvfrom(conn, &mut buf).map(|(n, from)| {
                    if peer.is_none() {
                        eprintln!("nc: datagram from {from}");
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
                Err(e) => return fail("recv", e),
            }
        }
        if stdin_open && fds[0].revents & POLLIN != 0 {
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
                            eprintln!("nc: no peer yet (waiting for a datagram)");
                            Ok(())
                        }
                        (false, _) => net::send(conn, &buf[..n]).map(|_| ()),
                    };
                    if let Err(e) = r {
                        return fail("send", e);
                    }
                }
            }
        }
    }
}
