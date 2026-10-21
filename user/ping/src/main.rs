//! ping: ICMP echo through a Linux-style "ping socket" (SOCK_DGRAM + IPPROTO_ICMP, or
//! IPPROTO_ICMPV6 over IPv6; the kernel fills in the identifier and checksum). No ^C yet,
//! so it stops after -c replies.
//!   ping [-4|-6] [-c COUNT] [-i INTERVAL_MS] [-s SIZE] [-W TIMEOUT_MS] HOST
//! Defaults: 4 pings, 1000 ms apart, 56 data bytes, 1000 ms timeout. HOST as in
//! rt::net::lookup (addresses, `fe80::1%eth1`, localhost, NAME.local); IPv4 unless -6 or
//! an IPv6 address.

#![no_std]
#![no_main]

extern crate alloc;

use rt::net;
use rt::{eprintln, println, sys};

rt::main!(main);

fn usage() -> i32 {
    eprintln!("usage: ping [-4|-6] [-c COUNT] [-i INTERVAL_MS] [-s SIZE] [-W TIMEOUT_MS] HOST");
    2
}

fn ms3(ns: u64) -> alloc::string::String {
    alloc::format!("{}.{:03}", ns / 1_000_000, ns % 1_000_000 / 1000)
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

fn main(args: &rt::Args) -> i32 {
    let (mut count, mut interval, mut size, mut wait) = (4u32, 1000u64, 56usize, 1000u64);
    let mut host = None;
    let mut want = net::Want::Any;
    let mut it = args.argv.iter().skip(1);
    while let Some(a) = it.next() {
        let mut num = || it.next().and_then(|v| v.parse::<u64>().ok());
        match *a {
            "-c" => count = num().unwrap_or(4) as u32,
            "-i" => interval = num().unwrap_or(1000),
            "-s" => size = num().unwrap_or(56).min(1400) as usize,
            "-W" => wait = num().unwrap_or(1000),
            "-4" => want = net::Want::V4,
            "-6" => want = net::Want::V6,
            h if !h.starts_with('-') => host = Some(h),
            _ => return usage(),
        }
    }
    let Some(host) = host else { return usage() };
    // Plain `ping localhost` stays IPv4 (as ping does on Linux); -6 or a v6 address: IPv6.
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
            eprintln!("ping: {e}");
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
            eprintln!("ping: socket: {e}");
            return 1;
        }
    };
    let _ = net::set_recv_timeout(fd, wait);
    let name = alloc::format!("{}", dst.ip);
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
        if let Err(e) = net::sendto(fd, &pkt, &dst) {
            println!("ping: sendto: {e}");
            sent += 1;
            continue;
        }
        sent += 1;
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
                    println!("ping: recvfrom: {e}");
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
