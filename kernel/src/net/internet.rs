//! Internet on or off: which policy this boot runs under (docs/networking.md, "Internet").
//!
//! The launcher says, in fw_cfg file `opt/milkos/net` (space-separated words):
//! - `internet=on`: online. `scripts/run.sh` and the release `milkOS.command` give slirp no
//!   `restrict=on` and pass this. Names resolve through the DHCP-provided DNS server (slirp's
//!   10.0.2.3, which asks the Mac's resolver), the Browser visits host names, `network
//!   lookup` / `network get` work for real names.
//! - nothing (or `internet=off`): offline, today's policy exactly. Every test boots like this
//!   (`restrict=on`, or its own local-only netdev); the resolver answers only `.test` names
//!   from a server set by the self-test, the Browser only visits IP literals and localhost.
//! - `dns=A.B.C.D:PORT` (test only, with `internet=on`): use this server instead of the DHCP
//!   one, so the online paths can be tested against a server on the Mac's 127.0.0.1 (seen as
//!   10.0.2.2:PORT). It must be on eth0's subnet or loopback; the DHCP server is then never
//!   used, not even as a fallback.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use super::Stack;
use super::wire::{IpAddr, Ipv4, SockAddr};
use crate::sync::SpinLock;

static ONLINE: AtomicBool = AtomicBool::new(false);
/// The test-only DNS override (`dns=` with `internet=on`).
static DNS_OVERRIDE: SpinLock<Option<SockAddr>> = SpinLock::new(None);

/// Read `opt/milkos/net` (at net init).
pub fn init() {
    let Some(f) = crate::fwcfg::find_file("opt/milkos/net") else {
        return;
    };
    let mut buf = alloc::vec![0u8; (f.size as usize).min(256)];
    if !crate::fwcfg::read_file(f, &mut buf) {
        return;
    }
    let text = alloc::string::String::from_utf8_lossy(&buf).into_owned();
    let mut online = false;
    let mut dns = None;
    for w in text.trim_matches(char::from(0)).split_whitespace() {
        match w.split_once('=') {
            Some(("internet", "on")) => online = true,
            Some(("internet", _)) => online = false,
            Some(("dns", v)) => {
                dns = v.split_once(':').and_then(|(ip, port)| {
                    Some(SockAddr::new(Ipv4::parse(ip)?, port.parse().ok()?))
                })
            }
            _ => {}
        }
    }
    ONLINE.store(online, Ordering::Release);
    if online {
        *DNS_OVERRIDE.lock() = dns;
    }
    crate::kprintln!(
        "[net] internet: {}",
        match (online, dns) {
            (true, Some(d)) =>
                alloc::format!("on, DNS {d} (test override; the DHCP server is not used)"),
            (true, None) => alloc::string::String::from("on, DNS from DHCP"),
            (false, _) => alloc::string::String::from("off (offline mode)"),
        }
    );
}

/// Is the internet on for this boot?
pub fn online() -> bool {
    ONLINE.load(Ordering::Acquire)
}

/// Where the DNS server came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DnsFrom {
    Dhcp,
    Test,
}

/// May the resolver send to `addr`? Loopback, or on eth0's subnet (not eth0 itself).
pub fn server_ok(addr: SockAddr) -> bool {
    let cfg = super::config();
    addr.port != 0
        && (addr.ip.is_loopback()
            || (cfg.configured()
                && addr
                    .ip
                    .v4()
                    .is_some_and(|a| a.same_net(cfg.addr, cfg.mask) && a != cfg.addr)))
}

/// The DNS server to use online: the test override, else DHCP's (port 53). None offline,
/// or before DHCP has one.
pub fn dns_server() -> Option<(SockAddr, DnsFrom)> {
    if !online() {
        return None;
    }
    if let Some(d) = *DNS_OVERRIDE.lock() {
        return server_ok(d).then_some((d, DnsFrom::Test));
    }
    let dns = super::config().dns?;
    let a = SockAddr::new(dns, 53);
    server_ok(a).then_some((a, DnsFrom::Dhcp))
}

/// One line for people: "on (DNS 10.0.2.3 from DHCP)" / "off (offline mode)".
pub fn summary() -> alloc::string::String {
    if !online() {
        return alloc::string::String::from("off (offline mode)");
    }
    match dns_server() {
        Some((a, DnsFrom::Dhcp)) => alloc::format!("on (DNS {} from DHCP)", a.ip),
        Some((a, DnsFrom::Test)) => alloc::format!("on (DNS {a}, test override)"),
        None => alloc::string::String::from("on (no DNS server yet)"),
    }
}

/// `/proc/net/internet`: `internet=on|off`, `dns=ADDR:PORT|-`, `dns_from=dhcp|test|-`, and
/// the resolver cache (`cache=N`, `cache_hits=`, `cache_misses=`).
pub fn proc_file(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(w, "internet={}", if online() { "on" } else { "off" })?;
    // Not dns_server(): that reads the stack, whose lock our caller holds.
    let cfg_dns = st.iface.eth0().cfg.dns;
    let over = *DNS_OVERRIDE.lock();
    let (dns, from) = match (online(), over, cfg_dns) {
        (false, _, _) => (None, "-"),
        (true, Some(d), _) => (Some(d), "test"),
        (true, None, Some(d)) => (Some(SockAddr::new(d, 53)), "dhcp"),
        (true, None, None) => (None, "-"),
    };
    match dns {
        Some(d) => writeln!(w, "dns={d}")?,
        None => writeln!(w, "dns=-")?,
    }
    writeln!(w, "dns_from={from}")?;
    let (n, hits, misses) = super::dns::cache_stats();
    writeln!(w, "cache={n}\ncache_hits={hits}\ncache_misses={misses}")
}

/// Is `ip` a numeric address the offline Browser may visit? (Everything numeric is.)
pub fn literal(host: &str) -> Option<IpAddr> {
    if host.eq_ignore_ascii_case("localhost") {
        return Some(IpAddr::V4(Ipv4::LOCALHOST));
    }
    if let Some(a) = Ipv4::parse(host) {
        return Some(IpAddr::V4(a));
    }
    super::wire::Ipv6::parse(host).map(IpAddr::V6)
}
