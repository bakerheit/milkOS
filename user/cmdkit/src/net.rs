//! What `network` reads, in words: /proc/net/ifaddr (milkOS: one line per link, `key=value`
//! fields, `inet6=` once per IPv6 address), /proc/net/dev (counters), /proc/net/{tcp,udp,
//! tcp6,udp6} (sockets), /proc/net/snmp{,6} (protocol counters), /proc/net/internet, and
//! `network lookup`'s answers.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use filekit::words::size;

/// One link from /proc/net/ifaddr.
#[derive(Debug, Clone)]
pub struct Link {
    pub name: String,
    pub kv: Vec<(String, String)>,
}

impl Link {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.kv
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
    }
    pub fn all(&self, k: &str) -> Vec<&str> {
        self.kv
            .iter()
            .filter(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
            .collect()
    }
    /// `a.b.c.d/len`, from inet= and netmask=.
    pub fn ipv4(&self) -> Option<String> {
        let a = self.get("inet")?;
        let bits: u32 = self
            .get("netmask")
            .unwrap_or("0.0.0.0")
            .split('.')
            .map(|o| o.parse::<u8>().unwrap_or(0).count_ones())
            .sum();
        Some(format!("{a}/{bits}"))
    }
    pub fn up(&self) -> bool {
        self.get("flags").unwrap_or("").contains("RUNNING")
    }
}

pub fn links(ifaddr: &str) -> Vec<Link> {
    ifaddr
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let name = String::from(w.next()?);
            let kv = w
                .filter_map(|x| x.split_once('='))
                .map(|(a, b)| (String::from(a), String::from(b)))
                .collect();
            Some(Link { name, kv })
        })
        .collect()
}

/// A link's counters from /proc/net/dev.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counters {
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
    pub tx_dropped: u64,
}

pub fn counters(dev: &str, name: &str) -> Option<Counters> {
    for line in dev.lines().skip(2) {
        let Some((n, rest)) = line.split_once(':') else {
            continue;
        };
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
        return Some(Counters {
            rx_bytes: f[0],
            rx_packets: f[1],
            rx_errors: f[2],
            rx_dropped: f[3],
            tx_bytes: f[8],
            tx_packets: f[9],
            tx_errors: f[10],
            tx_dropped: f[11],
        });
    }
    None
}

/// The machine's names and Milk Drop visibility, for the top of `network status`.
pub struct Names<'a> {
    pub hostname: &'a str,
    /// The .local (Milk Drop) name, without `.local`.
    pub local: Option<&'a str>,
    pub visible: bool,
    /// /proc/net/internet, as read (empty if the kernel has none).
    pub internet: &'a str,
}

/// What /proc/net/internet says: (on?, "on (DNS 10.0.2.3 from DHCP)" / "off (offline mode)").
pub fn internet(proc: &str) -> (bool, String) {
    let get = |k: &str| {
        proc.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .map(str::trim)
    };
    match get("internet") {
        Some("on") => {
            let words = match (get("dns"), get("dns_from")) {
                (Some(d), Some("dhcp")) if d != "-" => {
                    format!("on (DNS {} from DHCP)", d.strip_suffix(":53").unwrap_or(d))
                }
                (Some(d), Some("test")) if d != "-" => format!("on (DNS {d}, test override)"),
                _ => String::from("on (no DNS server yet)"),
            };
            (true, words)
        }
        Some(_) => (false, String::from("off (offline mode)")),
        None => (false, String::from("off (this kernel doesn't say)")),
    }
}

/// `network status`: the names, then one block per link (only `only` if given). Err: there
/// is no link called that.
pub fn status_text(
    links: &[Link],
    dev: &str,
    names: &Names,
    only: Option<&str>,
) -> Result<String, String> {
    let mut o = String::new();
    if only.is_none() {
        let _ = writeln!(o, "hostname     {}", names.hostname);
        match names.local {
            Some(n) => {
                let _ = writeln!(o, "local name   {n}.local (the Milk Drop name)");
            }
            None => o.push_str("local name   - (no Milk Drop identity yet)\n"),
        }
        let _ = writeln!(
            o,
            "milk drop    {}",
            if names.visible {
                "visible on the VM link"
            } else {
                "hidden (no announcements, no .local answers)"
            }
        );
        let _ = writeln!(o, "internet     {}", internet(names.internet).1);
    }
    let mut shown = 0;
    for l in links {
        if only.is_some_and(|n| n != l.name) {
            continue;
        }
        if shown > 0 || only.is_none() {
            o.push('\n');
        }
        shown += 1;
        link_block(&mut o, l, dev);
    }
    if shown == 0
        && let Some(n) = only
    {
        let known: Vec<&str> = links.iter().map(|l| l.name.as_str()).collect();
        return Err(format!(
            "there's no network link called {n} (there are: {})",
            known.join(", ")
        ));
    }
    Ok(o)
}

fn link_block(o: &mut String, l: &Link, dev: &str) {
    let _ = writeln!(
        o,
        "{:<12} {}  {}  mtu {}",
        l.name,
        if l.up() { "up" } else { "down" },
        l.get("ether").unwrap_or("-"),
        l.get("mtu").unwrap_or("?")
    );
    let _ = writeln!(o, "  flags      {}", l.get("flags").unwrap_or("-"));
    let mut v4 = match l.ipv4() {
        Some(a) => format!("  ipv4       {a}"),
        None if l.get("ipv4") == Some("off") => String::from("  ipv4       off (IPv6 only)"),
        None => String::from("  ipv4       -"),
    };
    if let Some(m) = l.get("netmask").filter(|_| l.get("inet").is_some()) {
        let _ = write!(v4, "  netmask {m}");
    }
    if let Some(b) = l.get("broadcast") {
        let _ = write!(v4, "  broadcast {b}");
    }
    if let Some(d) = l.get("dhcp") {
        let _ = write!(v4, "  dhcp {d}");
    }
    if let Some(ll) = l.get("linklocal") {
        let _ = write!(v4, "  link-local {ll}");
    }
    let _ = writeln!(o, "{v4}");
    if l.get("gateway").is_some() || l.get("dns").is_some() {
        let _ = writeln!(
            o,
            "  gateway    {}  dns {}",
            l.get("gateway").unwrap_or("-"),
            l.get("dns").unwrap_or("-")
        );
    }
    for a in l.all("inet6") {
        let (addr, scope, tentative) = v6_parts(a);
        let _ = writeln!(
            o,
            "  ipv6       {addr}  {scope}{}",
            if tentative { "  (tentative)" } else { "" }
        );
    }
    if let Some(r) = l.get("router6") {
        let _ = writeln!(o, "  router6    {r}");
    }
    if let Some(c) = counters(dev, &l.name) {
        let _ = writeln!(
            o,
            "  received   {} packets, {} bytes ({}), {} errors, {} dropped",
            c.rx_packets,
            c.rx_bytes,
            size(c.rx_bytes),
            c.rx_errors,
            c.rx_dropped
        );
        let _ = writeln!(
            o,
            "  sent       {} packets, {} bytes ({}), {} errors, {} dropped",
            c.tx_packets,
            c.tx_bytes,
            size(c.tx_bytes),
            c.tx_errors,
            c.tx_dropped
        );
    }
}

/// `fe80::1/64,link,tentative` -> ("fe80::1/64", "link", true). `::1` is `host`.
pub fn v6_parts(v: &str) -> (&str, &'static str, bool) {
    let mut parts = v.split(',');
    let addr = parts.next().unwrap_or(v);
    let mut scope = "global";
    let mut tentative = false;
    for f in parts {
        match f {
            "link" => scope = "link",
            "host" => scope = "host",
            "tentative" => tentative = true,
            _ => {}
        }
    }
    if addr.split('/').next() == Some("::1") {
        scope = "host";
    }
    (addr, scope, tentative)
}

/// `network status --keys`: stable `key=value` lines for programs (the Settings > Network
/// contract, docs/network-panel.md).
pub fn keys_text(links: &[Link], names: &Names) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "hostname={}", names.hostname);
    let _ = writeln!(o, "local_name={}", names.local.unwrap_or(""));
    let _ = writeln!(
        o,
        "local_fqdn={}",
        names
            .local
            .map(|n| format!("{n}.local"))
            .unwrap_or_default()
    );
    let _ = writeln!(
        o,
        "drop_visible={}",
        if names.visible { "on" } else { "off" }
    );
    let _ = writeln!(
        o,
        "internet={}",
        if internet(names.internet).0 {
            "on"
        } else {
            "off"
        }
    );
    for i in links {
        let p = format!("iface.{}", i.name);
        let _ = writeln!(o, "{p}.flags={}", i.get("flags").unwrap_or(""));
        let _ = writeln!(o, "{p}.mtu={}", i.get("mtu").unwrap_or(""));
        for (k, key) in [
            ("ether", "mac"),
            ("gateway", "gateway"),
            ("dns", "dns"),
            ("dhcp", "dhcp"),
            ("linklocal", "linklocal"),
            ("router6", "router6"),
        ] {
            if let Some(v) = i.get(k) {
                let _ = writeln!(o, "{p}.{key}={v}");
            }
        }
        if let Some(a) = i.ipv4() {
            let _ = writeln!(o, "{p}.ipv4={a}");
        } else if i.get("ipv4") == Some("off") {
            let _ = writeln!(o, "{p}.ipv4=off");
        }
        let v6 = i.all("inet6");
        if !v6.is_empty() {
            let _ = writeln!(o, "{p}.ipv6={}", v6.join(" "));
        }
    }
    o
}

// ---- sockets ---------------------------------------------------------------------------

/// IPv6 per RFC 5952 (lowercase, the longest run of 2+ zero words as `::`, IPv4-mapped as
/// `::ffff:a.b.c.d`), the same as rt::net::Ip.
pub fn v6_text(b: &[u8; 16]) -> String {
    if b[..10] == [0; 10] && b[10] == 0xff && b[11] == 0xff {
        return format!("::ffff:{}.{}.{}.{}", b[12], b[13], b[14], b[15]);
    }
    let mut w = [0u16; 8];
    for (i, x) in w.iter_mut().enumerate() {
        *x = u16::from_be_bytes([b[2 * i], b[2 * i + 1]]);
    }
    let (mut best, mut best_len, mut cur, mut cur_len) = (8, 0, 0, 0);
    for (i, &x) in w.iter().enumerate() {
        if x == 0 {
            if cur_len == 0 {
                cur = i;
            }
            cur_len += 1;
            if cur_len > best_len {
                best = cur;
                best_len = cur_len;
            }
        } else {
            cur_len = 0;
        }
    }
    if best_len < 2 {
        best = 8;
    }
    let mut o = String::new();
    let mut i = 0;
    while i < 8 {
        if i == best {
            o.push_str("::");
            i += best_len;
            continue;
        }
        if i > 0 && i != best + best_len {
            o.push(':');
        }
        let _ = write!(o, "{:x}", w[i]);
        i += 1;
    }
    o
}

/// "0F02000A:0050" -> "10.0.2.15:80" ("*" for 0.0.0.0, ":*" for port 0); the 32-digit IPv6
/// form (four native-endian words) -> "[fe80::1]:80" ("[::]" for the wildcard).
pub fn proc_addr(hex: &str) -> String {
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
        format!("[{}]", v6_text(&b))
    } else {
        let a = u32::from_str_radix(ip, 16).unwrap_or(0).to_le_bytes();
        if a == [0; 4] {
            String::from("*")
        } else {
            format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
        }
    };
    if port == 0 {
        format!("{host}:*")
    } else {
        format!("{host}:{port}")
    }
}

pub fn tcp_state(code: &str) -> &'static str {
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

/// One socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conn {
    /// tcp, tcp6, udp, udp6.
    pub proto: &'static str,
    pub recv_q: u64,
    pub send_q: u64,
    pub local: String,
    pub remote: String,
    /// TCP's state ("" for UDP).
    pub state: &'static str,
    pub listening: bool,
}

/// The sockets in one /proc/net/{tcp,udp,tcp6,udp6} file.
pub fn conns(proto: &'static str, text: &str) -> Vec<Conn> {
    let tcp = proto.starts_with("tcp");
    let mut v = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let (txq, rxq) = f[4].split_once(':').unwrap_or(("0", "0"));
        let state = if tcp { tcp_state(f[3]) } else { "" };
        v.push(Conn {
            proto,
            recv_q: u64::from_str_radix(rxq, 16).unwrap_or(0),
            send_q: u64::from_str_radix(txq, 16).unwrap_or(0),
            local: proc_addr(f[1]),
            remote: proc_addr(f[2]),
            state,
            listening: state == "LISTEN" || (!tcp && f[2].ends_with(":0000")),
        });
    }
    v
}

/// Which sockets `network connections` shows.
#[derive(Debug, Clone, Copy, Default)]
pub struct Which {
    pub tcp: bool,
    pub udp: bool,
    pub listening: bool,
    pub only4: bool,
    pub only6: bool,
}

impl Which {
    /// Read `proto`'s file at all?
    pub fn wants(&self, proto: &str) -> bool {
        let tcp = proto.starts_with("tcp");
        let v6 = proto.ends_with('6');
        let (t, u) = if !self.tcp && !self.udp {
            (true, true)
        } else {
            (self.tcp, self.udp)
        };
        !((tcp && !t) || (!tcp && !u) || (v6 && self.only4) || (!v6 && self.only6))
    }
}

/// The column heading and one line per socket, netstat's layout.
pub fn conns_text(v: &[Conn]) -> String {
    let mut o = String::new();
    let _ = writeln!(
        o,
        "{:<5} {:>6} {:>6} {:<30} {:<30} State",
        "Proto", "Recv-Q", "Send-Q", "Local Address", "Foreign Address"
    );
    for c in v {
        let _ = writeln!(
            o,
            "{:<5} {:>6} {:>6} {:<30} {:<30} {}",
            c.proto, c.recv_q, c.send_q, c.local, c.remote, c.state
        );
    }
    o
}

/// `network counters`: /proc/net/snmp (pairs of `Proto: names` / `Proto: values` lines) and
/// snmp6 (`Name value` per line), one counter a line.
pub fn counters_text(snmp: &str, snmp6: &str) -> String {
    let mut o = String::new();
    let lines: Vec<&str> = snmp.lines().collect();
    for pair in lines.chunks(2) {
        let [names, vals] = pair else { continue };
        let (proto, names) = names.split_once(':').unwrap_or(("?", names));
        let vals = vals.split_once(':').map_or(*vals, |(_, v)| v);
        let _ = writeln!(o, "{proto}:");
        for (n, v) in names.split_whitespace().zip(vals.split_whitespace()) {
            let _ = writeln!(o, "    {v:>10} {n}");
        }
    }
    if !snmp6.is_empty() {
        o.push_str("Ip6:\n");
        for line in snmp6.lines() {
            if let Some((n, v)) = line.split_once(' ') {
                let _ = writeln!(o, "    {:>10} {n}", v.trim());
            }
        }
    }
    o
}

// ---- addresses -------------------------------------------------------------------------

/// `network lookup`'s answer: one line per address, the family first.
pub fn lookup_text(host: &str, addrs: &[String], v6: &[bool]) -> String {
    let mut o = String::new();
    for (a, six) in addrs.iter().zip(v6) {
        let _ = writeln!(
            o,
            "{host} has {} address {a}",
            if *six { "IPv6" } else { "IPv4" }
        );
    }
    o
}

/// A port number, in words when it isn't one.
pub fn port(v: &str) -> Result<u16, String> {
    match v.parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(format!("{v} isn't a port number (1 to 65535)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IFADDR: &str = "lo flags=UP,LOOPBACK,RUNNING mtu=65536 inet=127.0.0.1 netmask=255.0.0.0 inet6=::1/128\n\
eth0 flags=UP,BROADCAST,MULTICAST,RUNNING mtu=1500 ether=52:54:00:12:34:56 inet=10.0.2.15 netmask=255.255.255.0 broadcast=10.0.2.255 gateway=10.0.2.2 dns=10.0.2.3 inet6=fe80::5054:ff:fe12:3456/64,link inet6=fec0::5054:ff:fe12:3456/64,global router6=fe80::2 dhcp=Bound\n\
eth1 flags=UP,BROADCAST mtu=1500 ether=52:54:00:12:34:57 inet6=fe80::1/64,link,tentative ipv4=off\n";

    const DEV: &str = "Inter-|   Receive  |  Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n\
  eth0:    2048      10    0    0    0     0          0         0     1024       5    0    0    0     0       0          0\n";

    fn names() -> Names<'static> {
        Names {
            hostname: "milk",
            local: Some("milk-1"),
            visible: true,
            internet: "internet=on\ndns=10.0.2.3:53\ndns_from=dhcp\ncache=0\n",
        }
    }

    #[test]
    fn status() {
        let ls = links(IFADDR);
        assert_eq!(ls.len(), 3);
        assert_eq!(ls[1].ipv4().as_deref(), Some("10.0.2.15/24"));
        assert!(ls[1].up() && !ls[2].up());
        let t = status_text(&ls, DEV, &names(), None).unwrap();
        assert!(t.starts_with("hostname     milk\nlocal name   milk-1.local (the Milk Drop name)\nmilk drop    visible on the VM link\ninternet     on (DNS 10.0.2.3 from DHCP)\n"));
        assert!(t.contains("\neth0         up  52:54:00:12:34:56  mtu 1500\n"));
        assert!(t.contains(
            "  ipv4       10.0.2.15/24  netmask 255.255.255.0  broadcast 10.0.2.255  dhcp Bound\n"
        ));
        assert!(t.contains("  gateway    10.0.2.2  dns 10.0.2.3\n"));
        assert!(t.contains("  ipv6       fe80::5054:ff:fe12:3456/64  link\n"));
        assert!(t.contains("  ipv6       fec0::5054:ff:fe12:3456/64  global\n"));
        assert!(t.contains("  ipv6       ::1/128  host\n"));
        assert!(t.contains("  ipv6       fe80::1/64  link  (tentative)\n"));
        assert!(t.contains("  ipv4       off (IPv6 only)\n"));
        assert!(t.contains("  received   10 packets, 2048 bytes (2.0 KB), 0 errors, 0 dropped\n"));
        assert!(t.contains("  sent       5 packets, 1024 bytes (1.0 KB), 0 errors, 0 dropped\n"));
        let one = status_text(&ls, DEV, &names(), Some("eth1")).unwrap();
        assert!(one.starts_with("eth1 "));
        assert!(!one.contains("hostname"));
        assert_eq!(
            status_text(&ls, DEV, &names(), Some("wlan0")).unwrap_err(),
            "there's no network link called wlan0 (there are: lo, eth0, eth1)"
        );
        let hidden = Names {
            hostname: "milk",
            local: None,
            visible: false,
            internet: "internet=off\ndns=-\ndns_from=-\n",
        };
        let t = status_text(&ls, DEV, &hidden, None).unwrap();
        assert!(t.contains("local name   - (no Milk Drop identity yet)\n"));
        assert!(t.contains("milk drop    hidden (no announcements, no .local answers)\n"));
        assert!(t.contains("internet     off (offline mode)\n"));
    }

    #[test]
    fn keys() {
        let k = keys_text(&links(IFADDR), &names());
        assert!(k.contains(
            "hostname=milk\nlocal_name=milk-1\nlocal_fqdn=milk-1.local\ndrop_visible=on\ninternet=on\n"
        ));
        assert!(k.contains("\niface.eth0.ipv4=10.0.2.15/24\n"));
        assert!(k.contains(
            "\niface.eth0.ipv6=fe80::5054:ff:fe12:3456/64,link fec0::5054:ff:fe12:3456/64,global\n"
        ));
        assert!(k.contains("\niface.eth0.dhcp=Bound\n"));
        assert!(k.contains("\niface.eth1.ipv4=off\n"));
        assert!(k.contains("\niface.eth0.mac=52:54:00:12:34:56\n"));
    }

    #[test]
    fn sockets() {
        assert_eq!(proc_addr("0F02000A:0050"), "10.0.2.15:80");
        assert_eq!(proc_addr("00000000:0000"), "*:*");
        assert_eq!(
            proc_addr("00000000000000000000000000000000:0050"),
            "[::]:80"
        );
        assert_eq!(
            proc_addr("00000000000000000000000001000000:1F90"),
            "[::1]:8080"
        );
        assert_eq!(
            proc_addr("0000000000000000FFFF00000F02000A:0050"),
            "[::ffff:10.0.2.15]:80"
        );
        assert_eq!(
            v6_text(&[0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
            "fe80::1"
        );
        let tcp = "  sl  local_address rem_address   st tx_queue:rx_queue\n   0: 00000000:0050 00000000:0000 0A 00000000:00000000\n   1: 0F02000A:C350 0202000A:0050 01 00000010:00000020\n";
        let v = conns("tcp", tcp);
        assert_eq!(v.len(), 2);
        assert!(v[0].listening && v[0].state == "LISTEN" && v[0].local == "*:80");
        assert_eq!((v[1].send_q, v[1].recv_q), (16, 32));
        assert_eq!(v[1].state, "ESTABLISHED");
        let udp = "  sl  local_address rem_address   st tx_queue:rx_queue\n   0: 00000000:0035 00000000:0000 07 00000000:00000000\n";
        let u = conns("udp", udp);
        assert!(u[0].listening && u[0].state.is_empty());
        let t = conns_text(&v);
        assert!(t.starts_with("Proto Recv-Q Send-Q Local Address"));
        assert!(t.contains("tcp        0      0 *:80"));
        let all = Which::default();
        assert!(all.wants("tcp") && all.wants("udp6"));
        let w = Which {
            tcp: true,
            only6: true,
            ..Which::default()
        };
        assert!(w.wants("tcp6") && !w.wants("tcp") && !w.wants("udp6"));
    }

    #[test]
    fn snmp() {
        let t = counters_text("Ip: Forwarding InReceives\nIp: 2 17\n", "Ip6InReceives 4\n");
        assert_eq!(
            t,
            "Ip:\n             2 Forwarding\n            17 InReceives\nIp6:\n             4 Ip6InReceives\n"
        );
    }

    #[test]
    fn urls() {
        assert_eq!(port("80"), Ok(80));
        assert_eq!(port("0").unwrap_err(), "0 isn't a port number (1 to 65535)");
        assert!(port("http").is_err());
        assert_eq!(v6_parts("::1/128"), ("::1/128", "host", false));
    }

    #[test]
    fn internet_words() {
        assert_eq!(
            internet("internet=on\ndns=10.0.2.3:53\ndns_from=dhcp\n"),
            (true, String::from("on (DNS 10.0.2.3 from DHCP)"))
        );
        assert_eq!(
            internet("internet=on\ndns=10.0.2.2:5353\ndns_from=test\n").1,
            "on (DNS 10.0.2.2:5353, test override)"
        );
        assert_eq!(
            internet("internet=on\ndns=-\ndns_from=-\n").1,
            "on (no DNS server yet)"
        );
        assert_eq!(
            internet("internet=off\n"),
            (false, String::from("off (offline mode)"))
        );
        assert!(!internet("").0);
        assert_eq!(
            lookup_text(
                "example.test",
                &[String::from("10.0.2.2"), String::from("fec0::2")],
                &[false, true]
            ),
            "example.test has IPv4 address 10.0.2.2\nexample.test has IPv6 address fec0::2\n"
        );
    }
}
