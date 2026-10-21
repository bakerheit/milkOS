//! netstat: sockets from /proc/net/{tcp,udp,tcp6,udp6}, or protocol counters.
//!   netstat [-t] [-u] [-l] [-s] [-4|-6]
//! -t TCP only, -u UDP only, -l listening only, -s counters (/proc/net/snmp and snmp6),
//! -4 / -6 one family only (AF_INET6 sockets are tcp6/udp6; a dual-stack one's IPv4 peers
//! show as ::ffff:a.b.c.d). Addresses are numeric (there is no resolver in user space).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use rt::net::Ip;
use rt::{eprintln, println, sys};

rt::main!(main);

fn text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// "0F02000A:0050" -> "10.0.2.15:80" ("*" for 0.0.0.0 / port 0); the 32-digit IPv6 form
/// (four native-endian words) -> "[fe80::1]:80" ("[::]" for the wildcard).
fn addr(hex: &str) -> String {
    let Some((ip, port)) = hex.split_once(':') else {
        return String::from(hex);
    };
    let port = u16::from_str_radix(port, 16).unwrap_or(0);
    let host = if ip.len() == 32 {
        let mut b = [0u8; 16];
        for k in 0..4 {
            let w = u32::from_str_radix(&ip[8 * k..8 * k + 8], 16).unwrap_or(0);
            b[4 * k..4 * k + 4].copy_from_slice(&w.to_le_bytes());
        }
        alloc::format!("[{}]", Ip::V6(b))
    } else {
        let ip = u32::from_str_radix(ip, 16).unwrap_or(0).to_le_bytes();
        if ip == [0; 4] {
            String::from("*")
        } else {
            alloc::format!("{}", Ip::V4(ip))
        }
    };
    if port == 0 {
        alloc::format!("{host}:*")
    } else {
        alloc::format!("{host}:{port}")
    }
}

fn tcp_state(code: &str) -> &'static str {
    match u8::from_str_radix(code, 16).unwrap_or(0) {
        1 => "ESTABLISHED",
        2 => "SYN_SENT",
        3 => "SYN_RECV",
        4 => "FIN_WAIT1",
        5 => "FIN_WAIT2",
        6 => "TIME_WAIT",
        7 => "CLOSE",
        8 => "CLOSE_WAIT",
        9 => "LAST_ACK",
        10 => "LISTEN",
        11 => "CLOSING",
        _ => "?",
    }
}

fn main(args: &rt::Args) -> i32 {
    let (mut t, mut u, mut l, mut s) = (false, false, false, false);
    let (mut only4, mut only6) = (false, false);
    for a in args.argv.iter().skip(1) {
        for c in a.trim_start_matches('-').chars() {
            match c {
                't' => t = true,
                'u' => u = true,
                'l' => l = true,
                's' => s = true,
                '4' => only4 = true,
                '6' => only6 = true,
                'a' | 'n' => {}
                _ => {
                    eprintln!("usage: netstat [-t] [-u] [-l] [-s] [-4|-6]");
                    return 2;
                }
            }
        }
    }
    if s {
        let snmp = text("/proc/net/snmp");
        let lines: alloc::vec::Vec<&str> = snmp.lines().collect();
        for pair in lines.chunks(2) {
            let [names, vals] = pair else { continue };
            let (proto, names) = names.split_once(':').unwrap_or(("?", names));
            let vals = vals.split_once(':').map_or(*vals, |(_, v)| v);
            println!("{proto}:");
            for (n, v) in names.split_whitespace().zip(vals.split_whitespace()) {
                println!("    {v:>10} {n}");
            }
        }
        // snmp6 is `Name value` per line.
        let snmp6 = text("/proc/net/snmp6");
        if !snmp6.is_empty() {
            println!("Ip6:");
            for line in snmp6.lines() {
                if let Some((n, v)) = line.split_once(' ') {
                    println!("    {:>10} {n}", v.trim());
                }
            }
        }
        return 0;
    }
    if !t && !u {
        t = true;
        u = true;
    }
    println!(
        "{:<5} {:>6} {:>6} {:<30} {:<30} State",
        "Proto", "Recv-Q", "Send-Q", "Local Address", "Foreign Address"
    );
    for (proto, path) in [
        ("tcp", "/proc/net/tcp"),
        ("tcp6", "/proc/net/tcp6"),
        ("udp", "/proc/net/udp"),
        ("udp6", "/proc/net/udp6"),
    ] {
        let tcp = proto.starts_with("tcp");
        let v6 = proto.ends_with('6');
        if (tcp && !t) || (!tcp && !u) || (v6 && only4) || (!v6 && only6) {
            continue;
        }
        for line in text(path).lines().skip(1) {
            let f: alloc::vec::Vec<&str> = line.split_whitespace().collect();
            if f.len() < 5 {
                continue;
            }
            let (txq, rxq) = f[4].split_once(':').unwrap_or(("0", "0"));
            let txq = usize::from_str_radix(txq, 16).unwrap_or(0);
            let rxq = usize::from_str_radix(rxq, 16).unwrap_or(0);
            let state = if tcp { tcp_state(f[3]) } else { "" };
            let listening = state == "LISTEN" || (!tcp && f[2].ends_with(":0000"));
            if l && !listening {
                continue;
            }
            println!(
                "{:<5} {:>6} {:>6} {:<30} {:<30} {}",
                proto,
                rxq,
                txq,
                addr(f[1]),
                addr(f[2]),
                state
            );
        }
    }
    0
}
