//! ifconfig: show network interfaces (read-only; addresses come from DHCP).
//!   ifconfig [IFACE]
//! Reads /proc/net/ifaddr (milkOS: one `key=value` line per interface) and /proc/net/dev.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::{eprintln, println, sys};

rt::main!(main);

fn text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// rx/tx counters of `name` from /proc/net/dev: (rx bytes, rx pkts, rx errs, rx drop, tx ...).
fn counters(dev: &str, name: &str) -> Option<[u64; 8]> {
    for line in dev.lines().skip(2) {
        let (n, rest) = line.split_once(':')?;
        if n.trim() != name {
            continue;
        }
        let f: Vec<u64> = rest
            .split_whitespace()
            .map(|x| x.parse().unwrap_or(0))
            .collect();
        if f.len() < 12 {
            return None;
        }
        return Some([f[0], f[1], f[2], f[3], f[8], f[9], f[10], f[11]]);
    }
    None
}

fn human(b: u64) -> String {
    if b < 1024 {
        alloc::format!("{b} B")
    } else if b < 1024 * 1024 {
        alloc::format!("{}.{} KiB", b / 1024, b % 1024 * 10 / 1024)
    } else {
        alloc::format!("{}.{} MiB", b >> 20, ((b & 0xfffff) * 10) >> 20)
    }
}

fn main(args: &rt::Args) -> i32 {
    let only = args.get(1);
    let ifaddr = text("/proc/net/ifaddr");
    let dev = text("/proc/net/dev");
    if ifaddr.is_empty() {
        eprintln!("ifconfig: /proc/net/ifaddr: not available");
        return 1;
    }
    let mut shown = 0;
    for line in ifaddr.lines() {
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { continue };
        if only.is_some_and(|o| o != name) {
            continue;
        }
        shown += 1;
        let kv: Vec<(&str, &str)> = words.filter_map(|w| w.split_once('=')).collect();
        let get = |k: &str| kv.iter().find(|(a, _)| *a == k).map(|(_, v)| *v);
        println!(
            "{name}: flags=<{}>  mtu {}",
            get("flags").unwrap_or(""),
            get("mtu").unwrap_or("?")
        );
        if let Some(a) = get("inet") {
            let mut l = alloc::format!(
                "        inet {a}  netmask {}",
                get("netmask").unwrap_or("?")
            );
            if let Some(b) = get("broadcast") {
                l.push_str(&alloc::format!("  broadcast {b}"));
            }
            println!("{l}");
        }
        if let Some(m) = get("ether") {
            println!("        ether {m}");
        }
        if get("gateway").is_some() || get("dns").is_some() || get("dhcp").is_some() {
            println!(
                "        gateway {}  dns {}  dhcp {}",
                get("gateway").unwrap_or("-"),
                get("dns").unwrap_or("-"),
                get("dhcp").unwrap_or("-")
            );
        }
        if let Some(c) = counters(&dev, name) {
            println!(
                "        RX packets {}  bytes {} ({})  errors {}  dropped {}",
                c[1],
                c[0],
                human(c[0]),
                c[2],
                c[3]
            );
            println!(
                "        TX packets {}  bytes {} ({})  errors {}  dropped {}",
                c[5],
                c[4],
                human(c[4]),
                c[6],
                c[7]
            );
        }
        println!();
    }
    if shown == 0 {
        eprintln!("ifconfig: {}: no such interface", only.unwrap_or("?"));
        return 1;
    }
    0
}
