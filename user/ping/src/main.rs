//! ping: ICMP echo through a Linux-style "ping socket" (SOCK_DGRAM + IPPROTO_ICMP; the
//! kernel fills in the identifier and checksum). No ^C yet, so it stops after -c replies.
//!   ping [-c COUNT] [-i INTERVAL_MS] [-s SIZE] [-W TIMEOUT_MS] HOST
//! Defaults: 4 pings, 1000 ms apart, 56 data bytes, 1000 ms timeout. HOST is an address.

#![no_std]
#![no_main]

extern crate alloc;

use rt::net;
use rt::{eprintln, println, sys};

rt::main!(main);

fn usage() -> i32 {
    eprintln!("usage: ping [-c COUNT] [-i INTERVAL_MS] [-s SIZE] [-W TIMEOUT_MS] HOST");
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
    let mut it = args.argv.iter().skip(1);
    while let Some(a) = it.next() {
        let mut num = || it.next().and_then(|v| v.parse::<u64>().ok());
        match *a {
            "-c" => count = num().unwrap_or(4) as u32,
            "-i" => interval = num().unwrap_or(1000),
            "-s" => size = num().unwrap_or(56).min(1400) as usize,
            "-W" => wait = num().unwrap_or(1000),
            h if !h.starts_with('-') => host = Some(h),
            _ => return usage(),
        }
    }
    let Some(host) = host else { return usage() };
    let dst = match net::lookup(host, 0) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ping: {e}");
            return 2;
        }
    };
    let fd = match net::socket_proto(net::SOCK_DGRAM, net::IPPROTO_ICMP) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("ping: socket: {e}");
            return 1;
        }
    };
    let _ = net::set_recv_timeout(fd, wait);
    let [a, b, c, d] = dst.ip;
    println!("PING {a}.{b}.{c}.{d} {size}({}) bytes of data.", size + 28);
    let t_start = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let (mut sent, mut got) = (0u32, 0u32);
    let (mut min, mut max, mut sum) = (u64::MAX, 0u64, 0u64);
    let mut pkt = alloc::vec![0u8; 8 + size];
    let mut buf = alloc::vec![0u8; 8 + size + 64];
    for seq in 1..=count {
        pkt[0] = 8; // echo request
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
                Ok((n, from)) if n >= 8 && buf[0] == 0 && buf[6..8] == pkt[6..8] => {
                    let rtt = sys::clock_ns(sys::CLOCK_MONOTONIC) - t0;
                    let [a, b, c, d] = from.ip;
                    println!(
                        "{} bytes from {a}.{b}.{c}.{d}: icmp_seq={seq} time={} ms",
                        n,
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
    println!("--- {a}.{b}.{c}.{d} ping statistics ---");
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
