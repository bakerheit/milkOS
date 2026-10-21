//! `/proc/net/{dev,arp,route,tcp,udp,snmp}` in the Linux formats (close enough for
//! `cat`, `netstat`-style parsers and humans). Each writer takes any `fmt::Write`, so the
//! HTTP demo can serve the same text from a fixed buffer.

use alloc::string::String;
use core::fmt::{self, Write};

use super::Stack;
use super::arp::ArpState;
use super::iface::LINK_NAMES;
use super::socket::SockType;
use super::tcp::TcpState;
use super::wire::Ipv4;
use super::with;
use crate::fs::procfs;
use crate::virtio::net::DevStats;

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
    writeln!(
        w,
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode"
    )?;
    let now = st.now;
    for (n, (_, s)) in st
        .socks
        .iter()
        .filter(|(_, s)| s.ty == SockType::Stream && s.tcb.state != TcpState::Closed)
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
            "{:>4}: {:08X}:{:04X} {:08X}:{:04X} {:02X} {:08X}:{:08X} {:02X}:{:08X} {:08X} {:>5} {:>8} {} # {} rexmit {} rto {}ms",
            n,
            s.local.ip.proc_hex(),
            s.local.port,
            s.remote.ip.proc_hex(),
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
    Ok(())
}

pub fn udp(st: &Stack, w: &mut dyn Write) -> fmt::Result {
    writeln!(
        w,
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops"
    )?;
    for (n, (_, s)) in st
        .socks
        .iter()
        .filter(|(_, s)| s.ty == SockType::Dgram && s.owns_port)
        .enumerate()
    {
        writeln!(
            w,
            "{:>5}: {:08X}:{:04X} {:08X}:{:04X} {:02X} {:08X}:{:08X} 00:00000000 00000000 {:>5} {:>8} {} 2 0000000000000000 {}",
            n,
            s.local.ip.proc_hex(),
            s.local.port,
            s.remote.ip.proc_hex(),
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
        "Ip: InReceives InHdrErrors InAddrErrors InUnknownProtos InDelivers OutRequests OutNoRoutes ReasmFails"
    )?;
    writeln!(
        w,
        "Ip: {} {} {} {} {} {} {} {}",
        s.ip_in_receives,
        s.ip_in_hdr_errors,
        s.ip_in_addr_errors,
        s.ip_in_unknown_protos,
        s.ip_in_delivers,
        s.ip_out_requests,
        s.ip_out_no_routes,
        s.ip_reasm_fails
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
        "lo flags=UP,LOOPBACK,RUNNING mtu={} inet=127.0.0.1 netmask=255.0.0.0",
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

type Gen = fn(&Stack, &mut dyn Write) -> fmt::Result;

pub const FILES: &[(&str, Gen)] = &[
    ("dev", dev),
    ("arp", arp),
    ("route", route),
    ("tcp", tcp),
    ("udp", udp),
    ("snmp", snmp),
    ("ifaddr", ifaddr),
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
