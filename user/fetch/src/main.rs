//! fetch: HTTP/1.0 GET over the kernel's TCP stack.
//!   fetch [-i] [http://]HOST[:PORT][/PATH]     (-i: print the response headers too)
//! The body goes to stdout; a summary line ("fetch: 200 OK, 1234 bytes in 5 ms") follows.
//! Hosts: addresses, `localhost`, or `NAME.local` (mDNS on eth1, `rt::net::lookup`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::net;
use rt::sys;
use rt::{eprintln, println};

rt::main!(main);

fn main(args: &rt::Args) -> i32 {
    let mut headers = false;
    let mut url = None;
    for a in args.argv.iter().skip(1) {
        match *a {
            "-i" => headers = true,
            u => url = Some(u),
        }
    }
    let Some(url) = url else {
        eprintln!("usage: fetch [-i] [http://]HOST[:PORT][/PATH]");
        return 2;
    };
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (hostport, path) = match rest.find('/') {
        Some(k) => (&rest[..k], &rest[k..]),
        None => (rest, "/"),
    };
    let addr = match net::lookup(hostport, 80) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("fetch: {e}");
            return 2;
        }
    };
    let t0 = sys::clock_ns(1);
    let fd = match net::socket(net::SOCK_STREAM) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("fetch: socket: {e}");
            return 1;
        }
    };
    if let Err(e) = net::connect(fd, &addr) {
        eprintln!("fetch: connect {addr}: {e}");
        return 1;
    }
    let req = alloc::format!(
        "GET {path} HTTP/1.0\r\nHost: {hostport}\r\nUser-Agent: milkOS-fetch\r\nConnection: close\r\n\r\n"
    );
    if let Err(e) = net::send(fd, req.as_bytes()) {
        eprintln!("fetch: send: {e}");
        return 1;
    }
    let mut resp = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match net::recv(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => resp.extend_from_slice(&buf[..n]),
            Err(e) => {
                eprintln!("fetch: recv: {e}");
                return 1;
            }
        }
    }
    let _ = sys::close(fd);
    let ms = (sys::clock_ns(1) - t0) / 1_000_000;
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n");
    let (head, body) = match split {
        Some(k) => (&resp[..k], &resp[k + 4..]),
        None => (&resp[..], &resp[resp.len()..]),
    };
    let status = head
        .split(|&b| b == b'\n')
        .next()
        .and_then(|l| core::str::from_utf8(l).ok())
        .unwrap_or("")
        .trim_end();
    if headers {
        let _ = sys::write_all(1, head);
        println!();
        println!();
    }
    let _ = sys::write_all(1, body);
    if !body.ends_with(b"\n") && !body.is_empty() {
        println!();
    }
    let code = status.split(' ').nth(1).unwrap_or("?");
    let reason: Vec<&str> = status.split(' ').skip(2).collect();
    eprintln!(
        "fetch: {} {}, {} bytes from {} in {} ms",
        code,
        reason.join(" "),
        body.len(),
        addr,
        ms
    );
    if code.starts_with('2') { 0 } else { 1 }
}
