//! `/proc/net/{dev,arp,route,tcp,udp,snmp}` and the IPv6 ones (`tcp6`, `udp6`, `if_inet6`,
//! `snmp6`, `ndisc`) in the Linux formats (close enough for
//! `cat`, `netstat`-style parsers and humans). Each writer takes any `fmt::Write`, so the
//! HTTP demo can serve the same text from a fixed buffer.

use alloc::string::String;
use core::fmt::{self, Write};

use super::Stack;
use super::arp::ArpState;
use super::iface::LINK_NAMES;
use super::ipv6;
use super::socket::{Family, Sock, SockType};
use super::tcp::TcpState;
use super::wire::{IpAddr, Ipv4};
use super::with;
use crate::fs::procfs;
use crate::virtio::net::DevStats;

/// An address in /proc's hex: IPv4 as one native-endian u32, IPv6 as four of them (so
/// `::1` is `00000000000000000000000001000000`, as Linux prints it on little-endian).
struct Hex(IpAddr, bool);

impl fmt::Display for Hex {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if !self.1 {
            return write!(f, "{:08X}", self.0.v4().unwrap_or_default().proc_hex());
        }
        let b = self.0.to_v6_mapped().0;
        for c in b.chunks(4) {
            write!(f, "{:08X}", u32::from_le_bytes([c[0], c[1], c[2], c[3]]))?;
        }
        Ok(())
    }
}

/// Does socket `s` belong in the IPv6 (`v6`) or IPv4 file?
fn in_file(s: &Sock, v6: bool) -> bool {
    (s.family == Family::V6) == v6
}

fn dev_line(w: &mut dyn Write, name: &str, s: &DevStats, extra_drop: u64) -> fmt::Result {
    writeln!(
        w,
        "{:>6}: {:>8} {:>7} {:>4} {:>4} {:>4} {:>5} {:>10} {:>9} {:>8} {:>7} {:>4} {:>4} {:>4} {:>5} {:>7} {:>10}",
        name,
        s.rx_bytes,
        s.rx_packets,
        s.rx_errors,
        s.rx_dropped,
        0,
        0,
        0,
        0,
        s.tx_bytes,
        s.tx_packets,
        s.tx_errors,
        s.tx_dropped + extra_drop,
        0,
        0,
        0,
        0
    )
}

pub fn dev(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(
        w,
        "Inter-|   Receive                                                |  Transmit"
    )?;
    writeln!(
        w,
        " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed"
    )?;
    dev_line(w, "lo", &st.iface.lo_stats, 0)?;
    for (k, l) in st.iface.links.iter().enumerate() {
        if let Some(d) = l.dev.as_ref() {
            dev_line(w, LINK_NAMES[k], &d.stats, l.extra_drops)?;
        }
    }
    Ok(())
}

pub fn arp(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(
        w,
        "IP address       HW type     Flags       HW address            Mask     Device"
    )?;
    for (k, l) in st.iface.links.iter().enumerate() {
        for e in l.arp.entries.iter().flatten() {
            let flags = if e.state == ArpState::Reachable {
                0x2
            } else {
                0x0
            };
            let mut ip = String::new();
            let _ = write!(ip, "{}", e.ip);
            writeln!(
                w,
                "{:<16} 0x1         {:<#11x} {}     *        {}",
                ip, flags, e.mac, LINK_NAMES[k]
            )?;
        }
    }
    Ok(())
}

pub fn route(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(
        w,
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT"
    )?;
    for (k, l) in st.iface.links.iter().enumerate() {
        let c = l.cfg;
        if !l.up() || !c.configured() {
            continue;
        }
        let name = LINK_NAMES[k];
        if let Some(gw) = c.gateway {
            // RTF_UP | RTF_GATEWAY
            writeln!(
                w,
                "{name}\t{:08X}\t{:08X}\t0003\t0\t0\t0\t{:08X}\t0\t0\t0",
                0,
                gw.proc_hex(),
                0
            )?;
        }
        let net = Ipv4::from_u32(c.addr.to_u32() & c.mask.to_u32());
        writeln!(
            w,
            "{name}\t{:08X}\t{:08X}\t0001\t0\t0\t0\t{:08X}\t0\t0\t0",
            net.proc_hex(),
            0,
            c.mask.proc_hex()
        )?;
        if l.multicast() {
            // 224.0.0.0/4 out of the link-local link (link-scope multicast, discovery).
            writeln!(
                w,
                "{name}\t{:08X}\t{:08X}\t0001\t0\t0\t0\t{:08X}\t0\t0\t0",
                Ipv4::new(224, 0, 0, 0).proc_hex(),
                0,
                Ipv4::new(240, 0, 0, 0).proc_hex()
            )?;
        }
    }
    Ok(())
}

/// `/proc/net/tcp` (Linux columns) plus, for humans, a trailing comment column with the
/// state name, retransmit count and RTO.
pub fn tcp(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    tcp_file(st, w, false)
}

/// `/proc/net/tcp6`: the AF_INET6 sockets (IPv4 peers of dual-stack ones as ::ffff:a.b.c.d).
pub fn tcp6(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    tcp_file(st, w, true)
}

fn tcp_file(st: &Stack, w: &mut dyn Write, v6: bool) -> fmt::Result {
    let pad = if v6 { 24 } else { 0 };
    writeln!(
        w,
        "  sl  {:<w1$}local_address {:<w1$}rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode",
        "",
        "",
        w1 = pad
    )?;
    let now = st.now;
    for (n, (_, s)) in st
        .socks
        .iter()
        .filter(|(_, s)| {
            s.ty == SockType::Stream && s.tcb.state != TcpState::Closed && in_file(s, v6)
        })
        .enumerate()
    {
        let t = &s.tcb;
        let (tr, when) = match (t.timer, t.deadline) {
            (Some(d), _) => (1, d.saturating_sub(now)),
            (None, Some(d)) if t.state == TcpState::TimeWait => (3, d.saturating_sub(now)),
            _ => (0, 0),
        };
        let when_jiffies = when / 10_000_000; // 100 Hz
        let txq = if t.state == TcpState::Listen {
            0
        } else {
            s.tx.len()
        };
        let rxq = if t.state == TcpState::Listen {
            t.queued().len()
        } else {
            s.rx.len()
        };
        writeln!(
            w,
            "{:>4}: {}:{:04X} {}:{:04X} {:02X} {:08X}:{:08X} {:02X}:{:08X} {:08X} {:>5} {:>8} {} # {} rexmit {} rto {}ms",
            n,
            Hex(s.local.ip, v6),
            s.local.port,
            Hex(s.remote.ip, v6),
            s.remote.port,
            t.state.linux_code(),
            txq,
            rxq,
            tr,
            when_jiffies,
            t.retransmits,
            0,
            0,
            0,
            t.state.name(),
            t.retransmits,
            t.rto / 1_000_000
        )?;
    }
    // Connections in TIME_WAIT (kept without a socket, tcp::TwTable), by address family.
    let base = st
        .socks
        .iter()
        .filter(|(_, s)| s.ty == SockType::Stream && in_file(s, v6))
        .count();
    for (n, e) in st.tw.entries.iter().filter(|e| e.v6_sock == v6).enumerate() {
        writeln!(
            w,
            "{:>4}: {}:{:04X} {}:{:04X} 06 00000000:00000000 03:{:08X} 00000000 {:>5} {:>8} 0 # TIME_WAIT",
            base + n,
            Hex(e.local.ip, v6),
            e.local.port,
            Hex(e.remote.ip, v6),
            e.remote.port,
            e.deadline.saturating_sub(now) / 10_000_000,
            0,
            0
        )?;
    }
    Ok(())
}

pub fn udp(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    udp_file(st, w, false)
}

pub fn udp6(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    udp_file(st, w, true)
}

fn udp_file(st: &Stack, w: &mut dyn Write, v6: bool) -> fmt::Result {
    let pad = if v6 { 24 } else { 0 };
    writeln!(
        w,
        "  sl  {:<w1$}local_address {:<w1$}rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops",
        "",
        "",
        w1 = pad
    )?;
    for (n, (_, s)) in st
        .socks
        .iter()
        .filter(|(_, s)| s.ty == SockType::Dgram && s.owns_port && !s.icmp && in_file(s, v6))
        .enumerate()
    {
        writeln!(
            w,
            "{:>5}: {}:{:04X} {}:{:04X} {:02X} {:08X}:{:08X} 00:00000000 00000000 {:>5} {:>8} {} 2 0000000000000000 {}",
            n,
            Hex(s.local.ip, v6),
            s.local.port,
            Hex(s.remote.ip, v6),
            s.remote.port,
            if s.udp_connected { 1 } else { 7 },
            0,
            s.rx.len(),
            0,
            0,
            0,
            s.rx_drops
        )?;
    }
    Ok(())
}

pub fn snmp(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    let s = &st.stats;
    writeln!(
        w,
        "Ip: InReceives InHdrErrors InAddrErrors InUnknownProtos InDelivers OutRequests OutNoRoutes ReasmFails ReasmReqds ReasmOKs ReasmTimeout ReasmOverlaps FragCreates"
    )?;
    let f = &st.frags.stats;
    writeln!(
        w,
        "Ip: {} {} {} {} {} {} {} {} {} {} {} {} {}",
        s.ip_in_receives,
        s.ip_in_hdr_errors,
        s.ip_in_addr_errors,
        s.ip_in_unknown_protos,
        s.ip_in_delivers,
        s.ip_out_requests,
        s.ip_out_no_routes,
        s.ip_reasm_fails,
        f.reqds,
        f.oks,
        f.timeouts,
        f.overlaps,
        f.creates
    )?;
    writeln!(
        w,
        "Icmp: InMsgs InErrors InEchos InEchoReps OutMsgs OutEchos OutEchoReps OutDestUnreachs"
    )?;
    writeln!(
        w,
        "Icmp: {} {} {} {} {} {} {} {}",
        s.icmp_in_msgs,
        s.icmp_in_errors,
        s.icmp_in_echos,
        s.icmp_in_echo_reps,
        s.icmp_out_msgs,
        s.icmp_out_echos,
        s.icmp_out_echo_reps,
        s.icmp_out_dest_unreachs
    )?;
    writeln!(
        w,
        "Tcp: ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts"
    )?;
    let estab = st
        .socks
        .iter()
        .filter(|(_, k)| {
            k.ty == SockType::Stream
                && matches!(k.tcb.state, TcpState::Established | TcpState::CloseWait)
        })
        .count();
    writeln!(
        w,
        "Tcp: {} {} {} {} {} {} {} {} {} {}",
        s.tcp_active_opens,
        s.tcp_passive_opens,
        s.tcp_attempt_fails,
        s.tcp_estab_resets,
        estab,
        s.tcp_in_segs,
        s.tcp_out_segs,
        s.tcp_retrans_segs,
        s.tcp_in_errs,
        s.tcp_out_rsts
    )?;
    writeln!(
        w,
        "Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors"
    )?;
    writeln!(
        w,
        "Udp: {} {} {} {} {}",
        s.udp_in_datagrams,
        s.udp_no_ports,
        s.udp_in_errors,
        s.udp_out_datagrams,
        s.udp_rcvbuf_errors
    )
}

/// milkOS-specific: one line per interface, `key=value` fields (what `ifconfig` reads;
/// Linux gets these from ioctls instead).
pub fn ifaddr(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(
        w,
        "lo flags=UP,LOOPBACK,RUNNING mtu={} inet=127.0.0.1 netmask=255.0.0.0 inet6=::1/128",
        super::iface::MTU
    )?;
    for (k, l) in st.iface.links.iter().enumerate() {
        let Some(d) = l.dev.as_ref() else { continue };
        let c = l.cfg;
        write!(
            w,
            "{} flags=UP,BROADCAST{}{} mtu={} ether={}",
            LINK_NAMES[k],
            if l.ll.on { ",MULTICAST" } else { "" },
            if d.link_up() { ",RUNNING" } else { "" },
            super::iface::MTU,
            l.mac
        )?;
        if c.configured() {
            write!(
                w,
                " inet={} netmask={} broadcast={}",
                c.addr,
                c.mask,
                c.subnet_broadcast()
            )?;
        }
        if let Some(g) = c.gateway {
            write!(w, " gateway={g}")?;
        }
        if let Some(n) = c.dns {
            write!(w, " dns={n}")?;
        }
        // Every IPv6 address: inet6=ADDR/LEN,SCOPE[,tentative] (tentative while DAD runs).
        for a in &l.v6.addrs {
            write!(
                w,
                " inet6={}/{},{}{}",
                a.addr,
                a.plen,
                if a.addr.is_link_local() {
                    "link"
                } else {
                    "global"
                },
                if a.tentative { ",tentative" } else { "" }
            )?;
        }
        if let Some((r, _)) = l.v6.router {
            write!(w, " router6={r}")?;
        }
        if k == 0 {
            writeln!(w, " dhcp={:?}", st.dhcp.state)?;
        } else if l.ll.on {
            let state = if c.configured() { "bound" } else { "probing" };
            writeln!(w, " linklocal={state} conflicts={}", l.ll.conflicts)?;
        } else {
            writeln!(w)?;
        }
    }
    Ok(())
}

/// `/proc/net/if_inet6`: address, ifindex, prefix length, scope, flags, name (all hex but
/// the name), one line per address.
pub fn if_inet6(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(
        w,
        "{} 01 80 10 80       lo",
        Hex32(super::wire::Ipv6::LOOPBACK)
    )?;
    for (k, l) in st.iface.links.iter().enumerate() {
        for a in &l.v6.addrs {
            let scope = if a.addr.is_link_local() { 0x20 } else { 0x00 };
            // IFA_F_TENTATIVE 0x40, IFA_F_PERMANENT 0x80 (link-local and the VM-link ULA).
            let mut flags = if a.tentative { 0x40 } else { 0 };
            if a.origin != ipv6::Origin::Slaac {
                flags |= 0x80;
            }
            writeln!(
                w,
                "{} {:02x} {:02x} {:02x} {:02x} {:>8}",
                Hex32(a.addr),
                ipv6::ifindex(Some(k)),
                a.plen,
                scope,
                flags,
                LINK_NAMES[k]
            )?;
        }
    }
    Ok(())
}

/// 32 lowercase hex digits, byte order as on the wire (the if_inet6 format).
struct Hex32(super::wire::Ipv6);

impl fmt::Display for Hex32 {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for b in self.0.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// `/proc/net/snmp6`: one `Name value` per line, as Linux.
pub fn snmp6(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    let s = &st.ip6.stats;
    let f = &st.frags.stats6;
    let rows: [(&str, u64); 25] = [
        ("Ip6InReceives", s.in_receives),
        ("Ip6InHdrErrors", s.in_hdr_errors),
        ("Ip6InAddrErrors", s.in_addr_errors),
        ("Ip6InUnknownProtos", s.in_unknown_protos),
        ("Ip6InDiscards", s.in_discards),
        ("Ip6InDelivers", s.in_delivers),
        ("Ip6OutRequests", s.out_requests),
        ("Ip6OutNoRoutes", s.out_no_routes),
        ("Ip6ReasmReqds", f.reqds),
        ("Ip6ReasmOKs", f.oks),
        ("Ip6ReasmFails", f.fails),
        ("Ip6ReasmTimeout", f.timeouts),
        ("Ip6ReasmOverlaps", f.overlaps),
        ("Ip6FragCreates", f.creates),
        ("Ip6FragDropped", s.frag_dropped),
        ("Ip6InSourceRouteRefused", s.routing_refused),
        ("Icmp6InMsgs", s.icmp_in_msgs),
        ("Icmp6OutMsgs", s.icmp_out_msgs),
        ("Icmp6InEchos", s.icmp_in_echos),
        ("Icmp6InEchoReplies", s.icmp_in_echo_reps),
        ("Icmp6OutEchoReplies", s.icmp_out_echo_reps),
        ("Icmp6OutErrors", s.icmp_out_errors),
        ("Icmp6InNeighborDiscovery", s.nd_in),
        ("Icmp6OutNeighborDiscovery", s.nd_out),
        ("Ip6DadConflicts", s.dad_conflicts),
    ];
    for (k, v) in rows {
        writeln!(w, "{k:<32}{v}")?;
    }
    Ok(())
}

/// `/proc/net/igmp6`: the IPv6 groups each link listens to (Linux columns: ifindex,
/// name, group, users, flags, timer), then the MLD counters as a comment.
pub fn igmp6(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    for (k, l) in st.iface.links.iter().enumerate() {
        if !l.up() {
            continue;
        }
        for g in super::mld::groups(&l.v6) {
            let users =
                l.v6.mld
                    .joined()
                    .iter()
                    .find(|(j, _)| *j == g)
                    .map_or(1, |&(_, n)| n);
            writeln!(
                w,
                "{:<4} {:<15} {} {:>5} {:08X} {:>5}",
                ipv6::ifindex(Some(k)),
                LINK_NAMES[k],
                Hex32(g),
                users,
                if g == super::wire::Ipv6::ALL_NODES {
                    0x0c
                } else {
                    0x04
                },
                0
            )?;
        }
    }
    writeln!(
        w,
        "# mld queries_in {} reports_out {}",
        st.ip6.mld.queries_in, st.ip6.mld.reports_out
    )
}

/// `/proc/net/hostname` (milkOS): the hostname (uname's nodename, net::hostname).
pub fn hostname(_st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(w, "{}", super::hostname::get())
}

/// `/proc/net/pmtu` (milkOS): the path MTUs learned from ICMP, `DST MTU EXPIRES_IN_S`,
/// then the counters.
pub fn pmtu(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    for e in &st.pmtu.entries {
        writeln!(
            w,
            "{} {} {}",
            e.dst,
            e.mtu,
            e.until.saturating_sub(st.now) / 1_000_000_000
        )?;
    }
    writeln!(
        w,
        "# learned {} ignored {}",
        st.pmtu.stats.learned, st.pmtu.stats.ignored
    )
}

/// `/proc/net/ndisc` (milkOS): the IPv6 neighbour cache, `ADDR MAC STATE [router] DEV`.
pub fn ndisc(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    for (k, l) in st.iface.links.iter().enumerate() {
        for (ip, mac, reach, router) in l.v6.neighbours() {
            writeln!(
                w,
                "{ip} lladdr {mac} {}{} dev {}",
                if reach { "REACHABLE" } else { "INCOMPLETE" },
                if router { " router" } else { "" },
                LINK_NAMES[k]
            )?;
        }
    }
    Ok(())
}

type Gen = fn(&Stack, &mut dyn Write) -> fmt::Result;

pub const FILES: &[(&str, Gen)] = &[
    ("dev", dev),
    ("arp", arp),
    ("route", route),
    ("tcp", tcp),
    ("udp", udp),
    ("snmp", snmp),
    ("ifaddr", ifaddr),
    ("tcp6", tcp6),
    ("udp6", udp6),
    ("if_inet6", if_inet6),
    ("snmp6", snmp6),
    ("ndisc", ndisc),
    ("pmtu", pmtu),
    ("igmp6", igmp6),
    ("hostname", hostname),
];

/// Render one of [`FILES`] into a String (thread context).
pub fn render(f: Gen) -> String {
    let mut s = String::new();
    with(|st| {
        let _ = f(st, &mut s);
    });
    s
}

/// Add `/proc/net/*`.
pub fn init() {
    let Some(root) = procfs::root() else { return };
    // Not network, but procfs is up by now and the CSPRNG has no init of its own after it.
    root.add_file("rng", crate::crypto::rng::status);
    let dir = root.add_dir("net");
    for &(name, f) in FILES {
        dir.add_file(name, move || render(f));
    }
}
