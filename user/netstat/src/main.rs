//! netstat: sockets from /proc/net/{tcp,udp}, or protocol counters.
//!   netstat [-t] [-u] [-l] [-s]
//! -t TCP only, -u UDP only, -l listening only, -s counters (/proc/net/snmp).
//! Addresses are numeric (there is no resolver in user space).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use rt::{eprintln, println, sys};

rt::main!(main);

fn text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// "0F02000A:0050" -> "10.0.2.15:80" ("*" for 0.0.0.0 / port 0).
fn addr(hex: &str) -> String {
    let Some((ip, port)) = hex.split_once(':') else {
        return String::from(hex);
    };
    let ip = u32::from_str_radix(ip, 16).unwrap_or(0).to_le_bytes();
    let port = u16::from_str_radix(port, 16).unwrap_or(0);
    let host = if ip == [0; 4] {
        String::from("*")
    } else {
        alloc::format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
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
    for a in args.argv.iter().skip(1) {
        for c in a.trim_start_matches('-').chars() {
            match c {
                't' => t = true,
                'u' => u = true,
                'l' => l = true,
                's' => s = true,
                'a' | 'n' => {}
                _ => {
                    eprintln!("usage: netstat [-t] [-u] [-l] [-s]");
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
        return 0;
    }
    if !t && !u {
        t = true;
        u = true;
    }
    println!(
        "{:<5} {:>6} {:>6} {:<22} {:<22} State",
        "Proto", "Recv-Q", "Send-Q", "Local Address", "Foreign Address"
    );
    for (proto, path) in [("tcp", "/proc/net/tcp"), ("udp", "/proc/net/udp")] {
        if (proto == "tcp" && !t) || (proto == "udp" && !u) {
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
            let state = if proto == "tcp" { tcp_state(f[3]) } else { "" };
            let listening = state == "LISTEN" || (proto == "udp" && f[2].ends_with(":0000"));
            if l && !listening {
                continue;
            }
            println!(
                "{:<5} {:>6} {:>6} {:<22} {:<22} {}",
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
