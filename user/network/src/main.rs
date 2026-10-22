//! network: one command for the network (it replaced ifconfig, netstat, ping, nc, netctl and
//! fetch; docs/networking.md).
//!
//!   network [status] [LINK] [--keys]   links, addresses, counters, the names, Milk Drop
//!   network connections [-t] [-u] [-l] [-4|-6]   sockets       network counters
//!   network ping HOST                  network get URL [--save FILE]
//!   network connect HOST PORT          network listen PORT
//!   network name [--local] [NAME]      network hide | show     network help
//!
//! Reads /proc/net/* (kernel), uname (hostname) and /System/Settings/milk/drop (the .local
//! name, visibility). `status`, `connections` and `counters` give msh a table when asked
//! (MILK_TABLE=1, docs/shell.md). Exit 0 on success, 1 on a refused or failed change, 2 on
//! bad usage. Parsing and words: user/cmdkit.

#![no_std]
#![no_main]

extern crate alloc;

mod talk;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use cmdkit::args::{self, Opt, flag, valued};
use cmdkit::net::{self as kit, Names};
use milkdrop::{audit, ident};
use rt::io::Out;
use rt::table::{Table, Ty, Val};
use rt::{eprintln, print, println, sys};

rt::main!(main);

const HELP: &str = "\
usage: network COMMAND ...    the network: links, connections, names and Milk Drop

  status [LINK]           links and addresses, the hostname, Milk Drop (--keys: for programs)
  connections             open sockets: --tcp (-t) --udp (-u) --listening (-l) --ipv4 --ipv6
  counters                protocol counters (IP, ICMP, TCP, UDP, IPv6)
  ping HOST               echo requests: --count N (-c) --interval MS --size BYTES --timeout MS
  get URL                 an http:// page to the screen, or --save FILE (--headers too)
  connect HOST PORT       send what you type to HOST PORT and show what comes back (--udp)
  listen PORT             wait for one connection on PORT, then the same as connect (--udp)
  name [NAME]             show or set the hostname (root); --local: the .local (Milk Drop) name
  hide                    Milk Drop off: no announcements, no .local answers (administrators)
  show                    Milk Drop back on: visible on the VM link again (administrators)
  help                    this list

HOST is an address (10.0.2.2, fe80::1%eth1), localhost, or NAME.local; --ipv4 (-4) and
--ipv6 (-6) pick a family. ^D on an empty line ends what connect and listen send. With no
COMMAND, network shows the status.
";

const HOSTNAME_FILE: &str = "/System/Settings/milk/net/hostname";

const FAMILY: [Opt; 2] = [flag("ipv4", Some('4')), flag("ipv6", Some('6'))];

pub(crate) fn text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn hostname() -> String {
    sys::uname()
        .map(|u| u[1].clone())
        .unwrap_or_else(|_| String::from("milk"))
}

fn main(args: &rt::Args) -> i32 {
    let a = &args.argv[1..];
    let (cmd, rest) = match a.first() {
        None => ("status", &a[..0]),
        Some(&c) if c.starts_with('-') && c != "--help" && c != "-h" => ("status", a),
        Some(&c) => (c, &a[1..]),
    };
    if matches!(cmd, "help" | "--help" | "-h") || rest.contains(&"--help") {
        let mut o = Out::stdout();
        o.write(HELP.as_bytes());
        o.flush();
        return 0;
    }
    let table = rt::table::requested(args);
    let spec: Vec<Opt> = match cmd {
        "status" => alloc::vec![flag("keys", Some('k'))],
        "connections" => {
            let mut v = alloc::vec![
                flag("tcp", Some('t')),
                flag("udp", Some('u')),
                flag("listening", Some('l')),
            ];
            v.extend_from_slice(&FAMILY);
            v
        }
        "counters" | "hide" | "show" => Vec::new(),
        "ping" => {
            let mut v = alloc::vec![
                valued("count", Some('c')),
                valued("interval", Some('i')),
                valued("size", Some('s')),
                valued("timeout", Some('W')),
            ];
            v.extend_from_slice(&FAMILY);
            v
        }
        "get" => {
            let mut v = alloc::vec![
                valued("save", Some('o')),
                flag("headers", Some('i')),
                flag("replace", None),
            ];
            v.extend_from_slice(&FAMILY);
            v
        }
        "connect" | "listen" => {
            let mut v = alloc::vec![flag("udp", Some('u'))];
            v.extend_from_slice(&FAMILY);
            v
        }
        "name" => alloc::vec![flag("local", None)],
        _ => {
            eprintln!("network: there's no network {cmd} (try network help)");
            return 2;
        }
    };
    let what = format!("network {cmd}");
    let p = match args::parse(rest, &spec) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{}", e.words(&what));
            return 2;
        }
    };
    if p.has("ipv4") && p.has("ipv6") {
        eprintln!("{what}: pick one of --ipv4 and --ipv6");
        return 2;
    }
    let want = if p.has("ipv4") {
        rt::net::Want::V4
    } else if p.has("ipv6") {
        rt::net::Want::V6
    } else {
        rt::net::Want::Any
    };
    let usage = |u: &str| {
        eprintln!("usage: network {u}   (network help lists them all)");
        2
    };
    match (cmd, p.rest.as_slice()) {
        ("status", []) => status(None, p.has("keys"), table),
        ("status", [l]) if !p.has("keys") => status(Some(l), false, table),
        ("status", _) => usage("status [LINK] | status --keys"),
        ("connections", []) => connections(&p, table),
        ("connections", _) => usage("connections [--tcp] [--udp] [--listening] [--ipv4|--ipv6]"),
        ("counters", []) => counters(table),
        ("counters", _) => usage("counters"),
        ("ping", [host]) => talk::ping(&p, host, want),
        ("ping", _) => usage("ping [--count N] [--interval MS] [--size BYTES] [--timeout MS] HOST"),
        ("get", [url]) => talk::get(&p, url, want),
        ("get", _) => usage("get URL [--save FILE] [--replace] [--headers]"),
        ("connect", [host, port]) => talk::connect(host, port, p.has("udp"), want),
        ("connect", _) => usage("connect HOST PORT [--udp]"),
        ("listen", [port]) => talk::listen(port, p.has("udp"), want),
        ("listen", _) => usage("listen PORT [--udp]"),
        ("name", []) if p.has("local") => show_local_name(),
        ("name", []) => {
            println!("{}", hostname());
            0
        }
        ("name", [n]) if p.has("local") => set_local_name(n.trim_end_matches(".local")),
        ("name", [n]) => set_hostname(n),
        ("name", _) => usage("name [NAME]   or   name --local [NAME]"),
        ("hide", []) => set_visible(false),
        ("show", []) => set_visible(true),
        ("hide", _) | ("show", _) => usage(cmd),
        _ => usage(cmd),
    }
}

// ---- status ---------------------------------------------------------------------------------

fn status(only: Option<&str>, keys: bool, table: bool) -> i32 {
    let links = kit::links(&text("/proc/net/ifaddr"));
    if links.is_empty() {
        eprintln!("network status: the kernel isn't showing any network links (/proc/net/ifaddr)");
        return 1;
    }
    let dev = text("/proc/net/dev");
    let host = hostname();
    let local = ident::load().map(|id| id.name);
    let names = Names {
        hostname: &host,
        local: local.as_deref(),
        visible: ident::visible(),
    };
    if keys {
        print!("{}", kit::keys_text(&links, &names));
        return 0;
    }
    if table {
        let mut t = Table::new(&[
            ("link", Ty::Str),
            ("state", Ty::Str),
            ("mac", Ty::Str),
            ("mtu", Ty::Int),
            ("ipv4", Ty::Str),
            ("ipv6", Ty::Str),
            ("gateway", Ty::Str),
            ("dns", Ty::Str),
            ("dhcp", Ty::Str),
            ("flags", Ty::Str),
            ("received", Ty::Size),
            ("received_packets", Ty::Int),
            ("received_errors", Ty::Int),
            ("received_dropped", Ty::Int),
            ("sent", Ty::Size),
            ("sent_packets", Ty::Int),
            ("sent_errors", Ty::Int),
            ("sent_dropped", Ty::Int),
        ]);
        let s = |v: Option<&str>| v.map_or(Val::Nil, Val::str);
        let mut shown = 0;
        for l in links.iter().filter(|l| only.is_none_or(|n| n == l.name)) {
            shown += 1;
            let c = kit::counters(&dev, &l.name);
            let n = |f: fn(&kit::Counters) -> u64| {
                c.as_ref().map_or(Val::Nil, |c| Val::Int(f(c) as i64))
            };
            let v6: Vec<&str> = l.all("inet6").iter().map(|a| kit::v6_parts(a).0).collect();
            t.push(alloc::vec![
                Val::str(&l.name),
                Val::str(if l.up() { "up" } else { "down" }),
                s(l.get("ether")),
                l.get("mtu")
                    .and_then(|m| m.parse().ok())
                    .map_or(Val::Nil, Val::Int),
                match l.ipv4() {
                    Some(a) => Val::Str(a),
                    None => s(l.get("ipv4")),
                },
                if v6.is_empty() {
                    Val::Nil
                } else {
                    Val::Str(v6.join(" "))
                },
                s(l.get("gateway")),
                s(l.get("dns")),
                s(l.get("dhcp")),
                s(l.get("flags")),
                n(|c| c.rx_bytes),
                n(|c| c.rx_packets),
                n(|c| c.rx_errors),
                n(|c| c.rx_dropped),
                n(|c| c.tx_bytes),
                n(|c| c.tx_packets),
                n(|c| c.tx_errors),
                n(|c| c.tx_dropped),
            ]);
        }
        if shown == 0 {
            eprintln!(
                "network status: there's no network link called {}",
                only.unwrap_or("?")
            );
            return 1;
        }
        let mut o = Out::stdout();
        t.write(&mut o);
        o.flush();
        return 0;
    }
    match kit::status_text(&links, &dev, &names, only) {
        Ok(t) => {
            print!("{t}");
            0
        }
        Err(e) => {
            eprintln!("network status: {e}");
            1
        }
    }
}

// ---- connections, counters ------------------------------------------------------------------

fn connections(p: &args::Parsed, table: bool) -> i32 {
    let w = kit::Which {
        tcp: p.has("tcp"),
        udp: p.has("udp"),
        listening: p.has("listening"),
        only4: p.has("ipv4"),
        only6: p.has("ipv6"),
    };
    let mut v = Vec::new();
    for (proto, path) in [
        ("tcp", "/proc/net/tcp"),
        ("tcp6", "/proc/net/tcp6"),
        ("udp", "/proc/net/udp"),
        ("udp6", "/proc/net/udp6"),
    ] {
        if w.wants(proto) {
            v.extend(
                kit::conns(proto, &text(path))
                    .into_iter()
                    .filter(|c| !w.listening || c.listening),
            );
        }
    }
    if table {
        let mut t = Table::new(&[
            ("proto", Ty::Str),
            ("local", Ty::Str),
            ("remote", Ty::Str),
            ("state", Ty::Str),
            ("received_queue", Ty::Int),
            ("send_queue", Ty::Int),
        ]);
        for c in &v {
            t.push(alloc::vec![
                Val::str(c.proto),
                Val::str(&c.local),
                Val::str(&c.remote),
                if c.state.is_empty() {
                    Val::Nil
                } else {
                    Val::str(c.state)
                },
                Val::Int(c.recv_q as i64),
                Val::Int(c.send_q as i64),
            ]);
        }
        let mut o = Out::stdout();
        t.write(&mut o);
        o.flush();
        return 0;
    }
    print!("{}", kit::conns_text(&v));
    0
}

fn counters(table: bool) -> i32 {
    let (snmp, snmp6) = (text("/proc/net/snmp"), text("/proc/net/snmp6"));
    if snmp.is_empty() && snmp6.is_empty() {
        eprintln!("network counters: the kernel isn't showing any (/proc/net/snmp)");
        return 1;
    }
    if !table {
        print!("{}", kit::counters_text(&snmp, &snmp6));
        return 0;
    }
    let mut t = Table::new(&[("proto", Ty::Str), ("name", Ty::Str), ("value", Ty::Int)]);
    let lines: Vec<&str> = snmp.lines().collect();
    for pair in lines.chunks(2) {
        let [names, vals] = pair else { continue };
        let (proto, names) = names.split_once(':').unwrap_or(("?", names));
        let vals = vals.split_once(':').map_or(*vals, |(_, v)| v);
        for (n, v) in names.split_whitespace().zip(vals.split_whitespace()) {
            t.push(alloc::vec![
                Val::str(proto),
                Val::str(n),
                Val::Int(v.parse().unwrap_or(0))
            ]);
        }
    }
    for line in snmp6.lines() {
        if let Some((n, v)) = line.split_once(' ') {
            t.push(alloc::vec![
                Val::str("Ip6"),
                Val::str(n),
                Val::Int(v.trim().parse().unwrap_or(0))
            ]);
        }
    }
    let mut o = Out::stdout();
    t.write(&mut o);
    o.flush();
    0
}

// ---- names, Milk Drop -----------------------------------------------------------------------

fn set_hostname(name: &str) -> i32 {
    let old = hostname();
    if let Err(e) = sys::sethostname(name) {
        if e.0 == 1 {
            eprintln!(
                "network name: only root can change the hostname (admin network name {name})"
            );
        } else {
            eprintln!("network name: {name} can't be a hostname (1-63 letters, digits and '-')");
        }
        return 1;
    }
    // Kept for the next boot (persistent with a Milk volume), and /System/Settings/hostname to match.
    let _ = sys::mkdir("/System/Settings/milk", 0o755);
    let _ = sys::mkdir("/System/Settings/milk/net", 0o755);
    let line = format!("{name}\n");
    if let Err(e) = ident::write_atomic(HOSTNAME_FILE, line.as_bytes(), 0o644) {
        eprintln!("network name: {HOSTNAME_FILE}: {e} (the name is set until reboot)");
    }
    let _ = ident::write_atomic("/System/Settings/hostname", line.as_bytes(), 0o644);
    audit(
        "net.hostname",
        &format!("old={old} new={name} uid={}", sys::geteuid()),
    );
    println!("network: the hostname is now {name} (was {old})");
    0
}

fn show_local_name() -> i32 {
    match ident::load() {
        Some(id) => {
            println!("{}.local", id.name);
            0
        }
        None => {
            eprintln!("network name: this machine has no Milk Drop identity yet");
            1
        }
    }
}

fn set_local_name(name: &str) -> i32 {
    let Some(mut id) = ident::load() else {
        eprintln!(
            "network name: this machine has no Milk Drop identity yet (is the drop service up?)"
        );
        return 1;
    };
    let old = id.name.clone();
    match ident::set_name(&mut id, name) {
        Ok(()) => {
            println!("network: this machine is now {name}.local (was {old}.local)");
            0
        }
        Err(e) => {
            eprintln!("network name: {e}");
            1
        }
    }
}

fn set_visible(on: bool) -> i32 {
    match ident::set_visible(on) {
        Ok(()) => {
            println!(
                "network: Milk Drop is now {} on the VM link",
                if on { "visible" } else { "hidden" }
            );
            0
        }
        Err(e) => {
            eprintln!("network {}: {e}", if on { "show" } else { "hide" });
            1
        }
    }
}
