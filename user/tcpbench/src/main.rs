//! tcpbench: TCP throughput.
//!
//!   tcpbench send HOST:PORT MIB     connect, send MIB MiB, half-close, wait for the peer
//!                                   to close (so the time covers delivery)
//!   tcpbench recv HOST:PORT         connect, read until the peer closes
//!   tcpbench sink PORT              listen, take one connection, read until it closes
//!   tcpbench source PORT MIB        listen, take one connection, send MIB MiB, close
//!
//! Prints one line: `tcpbench: send 32 MiB in 1234 ms = 25.9 MiB/s (local -> peer)`.
//! HOST as in rt::net::lookup (numbers, localhost, NAME.local). scripts/net-bench.sh
//! drives it.

#![no_std]
#![no_main]

extern crate alloc;

use rt::net::{self, SockAddr};
use rt::{eprintln, println, sys};

rt::main!(main);

const CHUNK: usize = 64 * 1024;

fn usage() -> i32 {
    eprintln!("usage: tcpbench send HOST:PORT MIB | recv HOST:PORT | sink PORT | source PORT MIB");
    2
}

fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

fn report(what: &str, bytes: u64, t0: u64, peer: SockAddr) {
    let ms = (now_ms() - t0).max(1);
    let kib_s = bytes * 1000 / 1024 / ms;
    println!(
        "tcpbench: {what} {} MiB in {ms} ms = {}.{} MiB/s ({peer})",
        bytes >> 20,
        kib_s / 1024,
        kib_s % 1024 * 10 / 1024
    );
}

fn send_all(fd: i32, mib: u64) -> Result<u64, rt::Errno> {
    let mut buf = alloc::vec![0u8; CHUNK];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (i * 7 + 13) as u8;
    }
    let total = mib << 20;
    let mut sent = 0u64;
    while sent < total {
        let n = ((total - sent) as usize).min(CHUNK);
        let k = net::send(fd, &buf[..n])?;
        sent += k as u64;
    }
    Ok(sent)
}

fn recv_all(fd: i32) -> Result<u64, rt::Errno> {
    let mut buf = alloc::vec![0u8; CHUNK];
    let mut got = 0u64;
    loop {
        match net::recv(fd, &mut buf) {
            Ok(0) => return Ok(got),
            Ok(n) => got += n as u64,
            Err(e) if e.0 == 4 => {}
            Err(e) => return Err(e),
        }
    }
}

fn listen_one(port: u16) -> Result<(i32, SockAddr), rt::Errno> {
    let l = net::socket(net::SOCK_STREAM)?;
    let _ = net::set_reuseaddr(l);
    net::bind(l, &SockAddr { ip: [0; 4], port })?;
    net::listen(l, 1)?;
    let r = net::accept(l);
    let _ = sys::close(l);
    r
}

fn main(args: &rt::Args) -> i32 {
    let a: alloc::vec::Vec<&str> = args.argv.iter().skip(1).copied().collect();
    let fail = |what: &str, e: alloc::string::String| {
        eprintln!("tcpbench: {what}: {e}");
        1
    };
    match a.as_slice() {
        ["send" | "recv", hp, rest @ ..] => {
            let addr = match net::lookup(hp, 5001) {
                Ok(x) => x,
                Err(e) => return fail("address", e),
            };
            let fd = match net::socket(net::SOCK_STREAM) {
                Ok(f) => f,
                Err(e) => return fail("socket", alloc::format!("{e}")),
            };
            if let Err(e) = net::connect(fd, &addr) {
                return fail(&alloc::format!("connect {addr}"), alloc::format!("{e}"));
            }
            let t0 = now_ms();
            if a[0] == "send" {
                let Some(mib) = rest.first().and_then(|m| m.parse().ok()) else {
                    return usage();
                };
                let sent = match send_all(fd, mib) {
                    Ok(s) => s,
                    Err(e) => return fail("send", alloc::format!("{e}")),
                };
                let _ = net::shutdown(fd, 1);
                // The peer closes once it has read everything.
                let _ = recv_all(fd);
                report("send", sent, t0, addr);
            } else {
                match recv_all(fd) {
                    Ok(got) => report("recv", got, t0, addr),
                    Err(e) => return fail("recv", alloc::format!("{e}")),
                }
            }
            let _ = sys::close(fd);
            0
        }
        ["sink", port] => {
            let Ok(port) = port.parse() else {
                return usage();
            };
            let (fd, peer) = match listen_one(port) {
                Ok(x) => x,
                Err(e) => return fail("listen", alloc::format!("{e}")),
            };
            let t0 = now_ms();
            match recv_all(fd) {
                Ok(got) => report("sink", got, t0, peer),
                Err(e) => return fail("recv", alloc::format!("{e}")),
            }
            let _ = sys::close(fd);
            0
        }
        ["source", port, mib] => {
            let (Ok(port), Ok(mib)) = (port.parse(), mib.parse()) else {
                return usage();
            };
            let (fd, peer) = match listen_one(port) {
                Ok(x) => x,
                Err(e) => return fail("listen", alloc::format!("{e}")),
            };
            let t0 = now_ms();
            match send_all(fd, mib) {
                Ok(s) => report("source", s, t0, peer),
                Err(e) => return fail("send", alloc::format!("{e}")),
            }
            let _ = sys::close(fd);
            0
        }
        _ => usage(),
    }
}
