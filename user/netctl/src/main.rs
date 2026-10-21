//! netctl: the network settings in one place (what the Settings > Network panel shows and
//! changes; docs/network-panel.md).
//!
//!   netctl [show] [-k]        interfaces, IPv4 and IPv6 addresses, DHCP, DNS, the
//!                             hostname, the .local name, Milk Drop visibility
//!                             (-k: stable key=value lines for programs)
//!   netctl hostname [NAME]    show / set the hostname (root; kept in /etc/milk/net)
//!   netctl name [NAME]        show / set the .local name (the Milk Drop name;
//!                             administrators, through the drop daemon)
//!   netctl visible [on|off]   show / set Milk Drop visibility: off = no discovery
//!                             announcements or answers, no .local answers (administrators)
//!
//! Reads /proc/net/ifaddr (kernel), uname (hostname) and /etc/milk/drop (name,
//! visibility). Exit 0 on success, 1 on a refused or failed change, 2 on bad usage.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use milkdrop::{audit, ident};
use rt::{eprintln, println, sys};

rt::main!(main);

const HOSTNAME_FILE: &str = "/etc/milk/net/hostname";

fn usage() -> i32 {
    eprintln!("usage: netctl [show] [-k] | hostname [NAME] | name [NAME] | visible [on|off]");
    2
}

fn text(path: &str) -> String {
    sys::read_file(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn hostname() -> String {
    sys::uname()
        .map(|u| u[1].clone())
        .unwrap_or_else(|_| String::from("milk"))
}

/// One interface from /proc/net/ifaddr.
struct Iface {
    name: String,
    kv: Vec<(String, String)>,
}

impl Iface {
    fn get(&self, k: &str) -> Option<&str> {
        self.kv
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
    }
    fn all(&self, k: &str) -> Vec<&str> {
        self.kv
            .iter()
            .filter(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
            .collect()
    }
    /// `a.b.c.d/len`, from inet= and netmask=.
    fn ipv4(&self) -> Option<String> {
        let a = self.get("inet")?;
        let bits: u32 = self
            .get("netmask")
            .unwrap_or("0.0.0.0")
            .split('.')
            .map(|o| o.parse::<u8>().unwrap_or(0).count_ones())
            .sum();
        Some(alloc::format!("{a}/{bits}"))
    }
}

fn interfaces() -> Vec<Iface> {
    text("/proc/net/ifaddr")
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let name = String::from(w.next()?);
            let kv = w
                .filter_map(|x| x.split_once('='))
                .map(|(a, b)| (String::from(a), String::from(b)))
                .collect();
            Some(Iface { name, kv })
        })
        .collect()
}

fn local_name() -> Option<String> {
    ident::load().map(|id| id.name)
}

fn show(keys: bool) -> i32 {
    let ifs = interfaces();
    if ifs.is_empty() {
        eprintln!("netctl: /proc/net/ifaddr: not available");
        return 1;
    }
    let host = hostname();
    let name = local_name();
    let visible = ident::visible();
    if keys {
        println!("hostname={host}");
        println!("local_name={}", name.as_deref().unwrap_or(""));
        println!(
            "local_fqdn={}",
            name.as_ref()
                .map(|n| alloc::format!("{n}.local"))
                .unwrap_or_default()
        );
        println!("drop_visible={}", if visible { "on" } else { "off" });
        for i in &ifs {
            let p = alloc::format!("iface.{}", i.name);
            println!("{p}.flags={}", i.get("flags").unwrap_or(""));
            println!("{p}.mtu={}", i.get("mtu").unwrap_or(""));
            for (k, key) in [
                ("ether", "mac"),
                ("gateway", "gateway"),
                ("dns", "dns"),
                ("dhcp", "dhcp"),
                ("linklocal", "linklocal"),
                ("router6", "router6"),
            ] {
                if let Some(v) = i.get(k) {
                    println!("{p}.{key}={v}");
                }
            }
            if let Some(a) = i.ipv4() {
                println!("{p}.ipv4={a}");
            } else if i.get("ipv4") == Some("off") {
                println!("{p}.ipv4=off");
            }
            let v6 = i.all("inet6");
            if !v6.is_empty() {
                println!("{p}.ipv6={}", v6.join(" "));
            }
        }
        return 0;
    }
    println!("hostname     {host}");
    match &name {
        Some(n) => println!("local name   {n}.local (the Milk Drop name)"),
        None => println!("local name   - (no Milk Drop identity yet)"),
    }
    println!(
        "milk drop    {}",
        if visible {
            "visible on the VM link"
        } else {
            "hidden (no announcements, no .local answers)"
        }
    );
    for i in &ifs {
        println!();
        let flags = i.get("flags").unwrap_or("");
        let state = if flags.contains("RUNNING") {
            "up"
        } else {
            "down"
        };
        println!(
            "{:<12} {state}  {}  mtu {}",
            i.name,
            i.get("ether").unwrap_or("-"),
            i.get("mtu").unwrap_or("?")
        );
        let mut v4 = match i.ipv4() {
            Some(a) => alloc::format!("  ipv4       {a}"),
            None if i.get("ipv4") == Some("off") => String::from("  ipv4       off (IPv6 only)"),
            None => String::from("  ipv4       -"),
        };
        if let Some(d) = i.get("dhcp") {
            v4.push_str(&alloc::format!("  dhcp {d}"));
        }
        if let Some(l) = i.get("linklocal") {
            v4.push_str(&alloc::format!("  link-local {l}"));
        }
        println!("{v4}");
        if i.get("gateway").is_some() || i.get("dns").is_some() {
            println!(
                "  gateway    {}  dns {}",
                i.get("gateway").unwrap_or("-"),
                i.get("dns").unwrap_or("-")
            );
        }
        for a in i.all("inet6") {
            let mut parts = a.split(',');
            let addr = parts.next().unwrap_or(a);
            let rest: Vec<&str> = parts.collect();
            println!("  ipv6       {addr}  {}", rest.join(" "));
        }
        if let Some(r) = i.get("router6") {
            println!("  router6    {r}");
        }
    }
    0
}

fn set_hostname(name: &str) -> i32 {
    let old = hostname();
    if let Err(e) = sys::sethostname(name) {
        eprintln!(
            "netctl: hostname {name}: {e}{}",
            if e.0 == 1 {
                " (root only)"
            } else {
                " (1-63 letters, digits and '-')"
            }
        );
        return 1;
    }
    // Kept for the next boot (persistent with a Milk volume), and /etc/hostname to match.
    let _ = sys::mkdir("/etc/milk", 0o755);
    let _ = sys::mkdir("/etc/milk/net", 0o755);
    let line = alloc::format!("{name}\n");
    if let Err(e) = ident::write_atomic(HOSTNAME_FILE, line.as_bytes(), 0o644) {
        eprintln!("netctl: {HOSTNAME_FILE}: {e} (the name is set until reboot)");
    }
    let _ = ident::write_atomic("/etc/hostname", line.as_bytes(), 0o644);
    audit(
        "net.hostname",
        &alloc::format!("old={old} new={name} uid={}", sys::geteuid()),
    );
    println!("netctl: hostname is now {name} (was {old})");
    0
}

fn set_name(name: &str) -> i32 {
    let Some(mut id) = ident::load() else {
        eprintln!("netctl: no Milk Drop identity on this machine yet (is the drop service up?)");
        return 1;
    };
    let old = id.name.clone();
    match ident::set_name(&mut id, name) {
        Ok(()) => {
            println!("netctl: this machine is now {name}.local (was {old}.local)");
            0
        }
        Err(e) => {
            eprintln!("netctl: name: {e}");
            1
        }
    }
}

fn set_visible(on: bool) -> i32 {
    match ident::set_visible(on) {
        Ok(()) => {
            println!(
                "netctl: Milk Drop is now {} on the VM link",
                if on { "visible" } else { "hidden" }
            );
            0
        }
        Err(e) => {
            eprintln!("netctl: visible: {e}");
            1
        }
    }
}

fn main(args: &rt::Args) -> i32 {
    let a: Vec<&str> = args.argv.iter().skip(1).copied().collect();
    match a.as_slice() {
        [] | ["show"] => show(false),
        ["-k"] | ["show", "-k"] => show(true),
        ["hostname"] => {
            println!("{}", hostname());
            0
        }
        ["hostname", n] => set_hostname(n),
        ["name"] => match local_name() {
            Some(n) => {
                println!("{n}.local");
                0
            }
            None => {
                eprintln!("netctl: no Milk Drop identity on this machine yet");
                1
            }
        },
        ["name", n] => set_name(n.trim_end_matches(".local")),
        ["visible"] => {
            println!("{}", if ident::visible() { "on" } else { "off" });
            0
        }
        ["visible", v @ ("on" | "off")] => set_visible(*v == "on"),
        _ => usage(),
    }
}
