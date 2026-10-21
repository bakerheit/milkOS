//! The network stack: Ethernet, ARP, IPv4, ICMP, UDP, TCP, DHCP and a DNS resolver, over
//! the virtio-net driver. See `docs/networking.md`.
//!
//! Layout:
//! - [`wire`]: addresses, field access, checksums.
//! - [`iface`]: Ethernet framing, ARP resolution, IPv4 output + routing, loopback (`lo`).
//! - [`arp`], [`icmp`], [`udp`], [`tcp`], [`dhcp`]: the protocols.
//! - [`socket`]: the kernel socket API (what the syscall layer wraps).
//! - [`dns`]: a stub resolver (thread context, blocking with a timeout).
//! - [`procnet`]: `/proc/net/*`. [`services`]: the in-kernel HTTP demo server.
//!
//! Concurrency: all state is one [`Stack`] behind one `SpinLock` (`NET`), held only for
//! short, non-blocking work. A kernel thread, "net" (High priority), drives it: it sleeps
//! on a wait queue until the virtio-net IRQ (which only acks and wakes it) or the next
//! timer (TCP retransmission / TIME_WAIT, ARP, DHCP), then receives, runs timers and the
//! in-kernel services. Socket API calls run in the caller's thread under the same lock.
//! Socket buffers come from a pool allocated once at boot; frames are built straight into
//! device buffers.

#![allow(dead_code)]

pub mod arp;
pub mod dhcp;
pub mod dns;
pub mod errno;
pub mod frag;
pub mod hostname;
pub mod icmp;
pub mod iface;
pub mod ipv6;
pub mod mld;
pub mod pmtu;
pub mod procnet;
pub mod ring;
pub mod selftest;
pub mod services;
pub mod socket;
pub mod sockfile;
pub mod tcp;
pub mod udp;
pub mod wire;

use alloc::boxed::Box;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::sched::{self, WaitQueue};
use crate::sync::{FnSlot, SpinLock};
use crate::virtio::net::VirtioNet;
use crate::{kprintln, timer};
use iface::{ETH_HDR, ETHERTYPE_ARP, ETHERTYPE_IPV4, IP_HDR, Iface};
use socket::SocketId;
use wire::{Ipv4, Mac, be16, checksum, ip_at};

/// Protocol counters (`/proc/net/snmp`).
#[derive(Default, Clone, Copy, Debug)]
pub struct Stats {
    pub ip_in_receives: u64,
    pub ip_in_hdr_errors: u64,
    pub ip_in_addr_errors: u64,
    pub ip_in_unknown_protos: u64,
    pub ip_in_delivers: u64,
    pub ip_out_requests: u64,
    pub ip_out_no_routes: u64,
    pub ip_reasm_fails: u64,
    pub icmp_in_msgs: u64,
    pub icmp_in_errors: u64,
    pub icmp_in_echos: u64,
    pub icmp_in_echo_reps: u64,
    pub icmp_out_msgs: u64,
    pub icmp_out_echos: u64,
    pub icmp_out_echo_reps: u64,
    pub icmp_out_dest_unreachs: u64,
    pub tcp_active_opens: u64,
    pub tcp_passive_opens: u64,
    pub tcp_attempt_fails: u64,
    pub tcp_estab_resets: u64,
    pub tcp_in_segs: u64,
    pub tcp_out_segs: u64,
    pub tcp_retrans_segs: u64,
    pub tcp_in_errs: u64,
    pub tcp_out_rsts: u64,
    pub udp_in_datagrams: u64,
    pub udp_no_ports: u64,
    pub udp_in_errors: u64,
    pub udp_out_datagrams: u64,
    pub udp_rcvbuf_errors: u64,
}

pub struct Stack {
    pub iface: Iface,
    pub socks: socket::Table,
    /// TCP connections in TIME_WAIT (no socket slot of their own).
    pub tw: tcp::TwTable,
    /// IP datagrams being reassembled.
    pub frags: frag::Reasm,
    /// Path MTUs learned from ICMP (both families).
    pub pmtu: pmtu::Cache,
    /// IPv6 (per-link state is in the links).
    pub ip6: ipv6::State6,
    pub dhcp: dhcp::Dhcp,
    pub ping: icmp::PingTable,
    pub stats: Stats,
    /// Socket slots whose readiness may have changed since the last wake-up pass.
    pub wake_mask: u64,
    /// Wake every waiter (non-socket events, e.g. a ping reply).
    pub wake_any: bool,
    /// Time of the current processing pass (set on entry).
    pub now: u64,
    /// The link the IPv6 segment being processed came in on, when its addresses are
    /// link-scoped (TCP answers and new connections need it). None otherwise.
    pub rx_link: Option<usize>,
}

static NET: SpinLock<Option<Box<Stack>>> = SpinLock::new(None);
/// `fn(SocketId)`, called (thread context, stack lock *not* held) when a socket may have
/// become readable/writable/errored. See [`set_wake_hook`].
static WAKE_HOOK: FnSlot = FnSlot::new();
/// Bumped on every readiness change; a cheap "did anything happen" for pollers.
static READY_GEN: AtomicU64 = AtomicU64::new(0);
/// Woken on every readiness change ([`socket::wait`] sleeps here).
pub(crate) static READY_WQ: WaitQueue = WaitQueue::new();

/// The net thread sleeps here; the virtio-net IRQ and early timers wake it.
static NET_WQ: WaitQueue = WaitQueue::new();
static IRQ_PENDING: AtomicBool = AtomicBool::new(false);
static KICK: AtomicBool = AtomicBool::new(false);
/// When the net thread plans to wake up next (now_ns time).
static PLANNED: AtomicU64 = AtomicU64::new(0);
static THREAD_UP: AtomicBool = AtomicBool::new(false);

pub type WakeHook = fn(SocketId);

/// Install the readiness hook (the process layer can use it to wake its own wait queues;
/// [`socket::wait`] already covers kernel-side blocking).
pub fn set_wake_hook(h: WakeHook) {
    WAKE_HOOK.set(h);
}

/// Counter bumped whenever any socket's readiness may have changed.
pub fn ready_generation() -> u64 {
    READY_GEN.load(Ordering::Acquire)
}

fn new_stack() -> Box<Stack> {
    Box::new(Stack {
        iface: Iface::new(),
        socks: socket::Table::new(),
        tw: tcp::TwTable::new(),
        frags: frag::Reasm::new(),
        pmtu: pmtu::Cache::new(),
        ip6: ipv6::State6::new(),
        dhcp: dhcp::Dhcp::new(),
        ping: icmp::PingTable::new(),
        stats: Stats::default(),
        wake_mask: 0,
        wake_any: false,
        now: timer::now_ns(),
        rx_link: None,
    })
}

/// Run `f` on the stack (creating it on first use). Afterwards: deliver loopback traffic,
/// fire wake-ups, and kick the net thread if `f` armed a timer earlier than its planned
/// wake-up. Thread context only; never blocks (the lock is a spinlock held briefly).
pub fn with<R>(f: impl FnOnce(&mut Stack) -> R) -> R {
    let (r, woken, next) = {
        let mut g = NET.lock();
        let st = g.get_or_insert_with(new_stack);
        st.now = timer::now_ns();
        let r = f(st);
        ipv6::flush_ns(st);
        st.poll_loopback();
        let woken = st.take_woken();
        (r, woken, st.next_deadline())
    };
    deliver_wakeups(woken);
    if next < PLANNED.load(Ordering::Acquire) {
        KICK.store(true, Ordering::Release);
        NET_WQ.wake_all();
    }
    r
}

struct Woken {
    any: bool,
    n: usize,
    ids: [SocketId; socket::MAX_SOCKETS],
}

fn deliver_wakeups(w: Woken) {
    if w.n == 0 && !w.any {
        return;
    }
    READY_GEN.fetch_add(1, Ordering::AcqRel);
    READY_WQ.wake_all();
    crate::poll::notify(); // poll / select / epoll waiters (kernel/src/poll.rs)
    // The in-kernel services live on the net thread: let it look too (loopback traffic
    // never raises an IRQ). It stops once a pass changes nothing.
    KICK.store(true, Ordering::Release);
    NET_WQ.wake_all();
    if let Some(h) = WAKE_HOOK.get::<WakeHook>() {
        for &id in &w.ids[..w.n] {
            h(id);
        }
    }
}

/// Called by the virtio-net driver at attach time (thread context, boot). `slot` orders
/// the interfaces (see [`Iface::attach`]).
pub fn attach_device(dev: VirtioNet, slot: usize) {
    let mac = Mac(dev.mac());
    with(|st| {
        if st.iface.attach(dev, slot, mac).is_none() {
            kprintln!("[net] slot {slot}: no free interface");
        }
    });
}

/// Is there an Ethernet interface?
pub fn has_device() -> bool {
    crate::virtio::net::present()
}

/// virtio-net interrupt (already acknowledged by the driver): wake the net thread.
/// IRQ context: no locks, no allocation.
pub fn on_device_irq() {
    IRQ_PENDING.store(true, Ordering::Release);
    NET_WQ.wake_all();
}

/// The net thread: receive, run timers, run the services, sleep until the next timer or
/// interrupt.
fn net_thread() {
    let mut http: Option<services::Http> = None;
    loop {
        services::adopt(&mut http);
        with(|st| {
            st.poll_rx();
            let now = st.now;
            st.iface.tick(now);
            dhcp::tick(st);
            tcp::tick(st);
            ipv6::tick(st);
            st.frags.expire(now);
            st.pmtu.expire(now);
            st.stats.ip_reasm_fails = st.frags.stats.fails;
        });
        if let Some(h) = http.as_mut() {
            services::run(h);
        }
        let (next, now) = with(|st| (st.next_deadline(), st.now));
        PLANNED.store(next, Ordering::Release);
        NET_WQ.wait_until_timeout(
            || IRQ_PENDING.swap(false, Ordering::AcqRel) | KICK.swap(false, Ordering::AcqRel),
            next.saturating_sub(now).max(1_000_000),
        );
        PLANNED.store(0, Ordering::Release);
    }
}

/// Bring the stack up: create it, add `/proc/net`, start DHCP on eth0 (if there is one)
/// and the net thread. Call after `sched::init`.
pub fn init() {
    hostname::load();
    let v6only = boot_v6only();
    let have_dev = with(|st| {
        let now = st.now;
        if v6only {
            st.iface.v4_off = true;
            for l in st.iface.links.iter_mut() {
                l.ll.on = false;
            }
        }
        st.iface.ll_start(now);
        for (k, l) in st.iface.links.iter().enumerate().filter(|(_, l)| l.up()) {
            kprintln!(
                "[net] {}: virtio slot {}, mac {}{}",
                iface::LINK_NAMES[k],
                l.slot,
                l.mac,
                if v6only {
                    ", IPv6 only"
                } else if l.ll.on {
                    ", link-local"
                } else {
                    ", DHCP"
                }
            );
        }
        ipv6::start(st);
        if st.iface.eth0().up() {
            if !v6only {
                dhcp::start(st);
            }
            true
        } else {
            false
        }
    });
    if v6only {
        kprintln!("[net] v6-only boot: no IPv4 on eth0 or eth1 (loopback keeps 127.0.0.1)");
    }
    let eth1 = with(|st| st.iface.links[1].up());
    procnet::init();
    if !THREAD_UP.swap(true, Ordering::AcqRel) {
        drop(sched::spawn_prio("net", sched::Priority::High, net_thread));
    }
    kprintln!(
        "[net] stack up: lo{}, {} sockets, buffers {} KiB auto-sized up to {} MiB, net thread started",
        match (have_dev, eth1, v6only) {
            (true, true, false) => " + eth0 (DHCP started) + eth1 (link-local)",
            (true, false, false) => " + eth0 (DHCP started)",
            (true, true, true) => " + eth0 + eth1 (IPv6 only)",
            (true, false, true) => " + eth0 (IPv6 only)",
            _ => " only (no NIC)",
        },
        socket::MAX_SOCKETS,
        socket::BUF_SIZE / 1024,
        socket::MAX_BUF >> 20
    );
}

/// Process anything received right now (thread context; the net thread does this too).
pub fn poll() {
    with(|st| st.poll_rx());
}

impl Stack {
    /// Mark socket slot `i` as possibly changed readiness.
    pub fn wake(&mut self, slot: usize) {
        self.wake_mask |= 1 << slot;
    }

    /// When the net thread next has timer work (at most 1 s ahead).
    pub fn next_deadline(&self) -> u64 {
        let now = self.now;
        let mut d = now + 1_000_000_000;
        for (_, s) in self.socks.iter() {
            if s.ty == socket::SockType::Stream
                && let Some(t) = tcp::next_timer(&s.tcb)
            {
                d = d.min(t);
            }
        }
        if let Some(t) = self.tw.next_deadline() {
            d = d.min(t);
        }
        if let Some(t) = self.frags.next_deadline() {
            d = d.min(t);
        }
        if let Some(t) = ipv6::next_deadline(self) {
            d = d.min(t);
        }
        if let Some(t) = self.dhcp.next_deadline() {
            d = d.min(t);
        }
        if self.iface.busy() {
            d = d.min(now + 100_000_000);
        }
        d
    }

    fn take_woken(&mut self) -> Woken {
        let mut w = Woken {
            any: core::mem::take(&mut self.wake_any),
            n: 0,
            ids: [SocketId::INVALID; socket::MAX_SOCKETS],
        };
        let mut m = core::mem::take(&mut self.wake_mask);
        while m != 0 {
            let i = m.trailing_zeros() as usize;
            m &= m - 1;
            if let Some(id) = self.socks.id_of(i) {
                w.ids[w.n] = id;
                w.n += 1;
            }
        }
        w
    }

    fn poll_rx(&mut self) {
        for k in 0..iface::MAX_LINKS {
            for _ in 0..256 {
                let Some(dev) = self.iface.links[k].dev.as_mut() else {
                    break;
                };
                let Some(b) = dev.rx_pop() else { break };
                let f = b.frame();
                self.eth_input(k, f);
                if let Some(dev) = self.iface.links[k].dev.as_mut() {
                    dev.rx_done(b);
                }
            }
            if let Some(dev) = self.iface.links[k].dev.as_mut() {
                dev.rx_kick();
                dev.tx_reclaim();
            }
        }
    }

    /// Deliver queued loopback packets (bounded, they may generate more).
    fn poll_loopback(&mut self) {
        // Packets the emulated loopback path MTU refused: answer as a router would.
        let lo_mtu = iface::LO_MTU.load(core::sync::atomic::Ordering::Relaxed) as usize;
        for pkt in core::mem::take(&mut self.iface.lo_too_big) {
            if pkt.first().is_some_and(|b| b >> 4 == 6) {
                ipv6::send_error(self, None, 2, 0, lo_mtu as u32, &pkt);
            } else if pkt.len() >= 20 && be16(&pkt, 6) & 0x4000 != 0 {
                icmp::send_frag_needed(self, &pkt, lo_mtu as u16);
            }
        }
        for _ in 0..256 {
            let Some(pkt) = self.iface.lo_front() else {
                return;
            };
            if pkt.first().is_some_and(|b| b >> 4 == 6) {
                ipv6::input(self, None, pkt, Mac::ZERO);
            } else {
                self.ip_input(None, pkt, true);
            }
            self.iface.lo_pop();
        }
    }

    fn eth_input(&mut self, k: usize, f: &[u8]) {
        if f.len() < ETH_HDR {
            return;
        }
        let dst = Mac([f[0], f[1], f[2], f[3], f[4], f[5]]);
        let l = &self.iface.links[k];
        let group = dst.0[0..3] == [0x01, 0x00, 0x5e] && l.multicast();
        // IPv6 multicast (33:33:*) only for groups the link is in (net::mld).
        let group6 = dst.0[0..2] == [0x33, 0x33] && mld::member_mac(&l.v6, dst);
        if dst != l.mac && dst != Mac::BROADCAST && !group && !group6 {
            return; // someone else's, or multicast for a group this link isn't in
        }
        let src = Mac([f[6], f[7], f[8], f[9], f[10], f[11]]);
        match be16(f, 12) {
            ETHERTYPE_ARP if !group6 => self.arp_input(k, &f[ETH_HDR..]),
            ETHERTYPE_IPV4 if !group6 => self.ip_input(Some(k), &f[ETH_HDR..], false),
            ipv6::ETHERTYPE_IPV6 => ipv6::input(self, Some(k), &f[ETH_HDR..], src),
            _ => {}
        }
    }

    fn arp_input(&mut self, k: usize, p: &[u8]) {
        if self.iface.v4_off {
            return;
        }
        if p.len() < 28 || be16(p, 0) != 1 || be16(p, 2) != 0x0800 || p[4] != 6 || p[5] != 4 {
            return;
        }
        let op = be16(p, 6);
        let sha = Mac([p[8], p[9], p[10], p[11], p[12], p[13]]);
        let spa = ip_at(p, 14);
        let tpa = ip_at(p, 24);
        let now = self.now;
        if self.iface.ll_arp_seen(k, sha, spa, tpa, now) {
            return; // a link-local conflict: we moved away from that address
        }
        let l = &mut self.iface.links[k];
        let for_us = l.cfg.is_local(tpa);
        // RFC 826 merge: refresh if known; learn if the packet is for us.
        if !spa.is_unspecified() && (for_us || l.arp.has(spa)) {
            l.arp.insert(spa, sha, now);
            self.iface.flush_pending(k, spa, sha);
        }
        if op == arp::OP_REQUEST && for_us {
            self.iface.send_arp(k, arp::OP_REPLY, sha, spa);
        }
    }

    /// An IPv4 packet from link `k` (None: loopback).
    fn ip_input(&mut self, link: Option<usize>, pkt: &[u8], from_lo: bool) {
        self.stats.ip_in_receives += 1;
        if self.iface.v4_off && link.is_some() {
            self.stats.ip_in_addr_errors += 1;
            return; // a v6-only boot takes no IPv4 from the wire
        }
        if pkt.len() < IP_HDR || pkt[0] >> 4 != 4 {
            self.stats.ip_in_hdr_errors += 1;
            return;
        }
        let ihl = (pkt[0] & 0x0f) as usize * 4;
        let total = be16(pkt, 2) as usize;
        if ihl < IP_HDR || total < ihl || total > pkt.len() || checksum(&pkt[..ihl]) != 0 {
            self.stats.ip_in_hdr_errors += 1;
            return;
        }
        if be16(pkt, 6) & 0x3fff != 0 {
            // A fragment (MF set or a non-zero offset): reassemble (net::frag).
            match self.frags.input(self.now, &pkt[..total], ihl, total) {
                frag::Outcome::Done(whole) => self.ip_input(link, &whole, from_lo),
                frag::Outcome::Pending | frag::Outcome::Dropped => {}
            }
            self.stats.ip_reasm_fails = self.frags.stats.fails;
            return;
        }
        let proto = pkt[9];
        let src = ip_at(pkt, 12);
        let dst = ip_at(pkt, 16);
        let l = &self.iface.links[link.unwrap_or(0)];
        let cfg = l.cfg;
        let accept = from_lo
            || self.iface.is_our_addr(dst)
            || dst.is_broadcast()
            || (cfg.configured() && dst == cfg.subnet_broadcast())
            || (l.multicast() && iface::is_link_multicast(dst))
            // Before DHCP finishes, take any unicast UDP on eth0 (the reply may be unicast).
            || (link == Some(0) && !cfg.configured() && proto == iface::PROTO_UDP);
        if !accept {
            self.stats.ip_in_addr_errors += 1;
            return;
        }
        let payload = &pkt[ihl..total];
        self.stats.ip_in_delivers += 1;
        match proto {
            iface::PROTO_ICMP => icmp::input(self, src, dst, payload, &pkt[..total]),
            iface::PROTO_TCP => tcp::input(self, src, dst, payload),
            iface::PROTO_UDP => udp::input(self, src, dst, payload, &pkt[..total]),
            _ => {
                self.stats.ip_in_unknown_protos += 1;
                if !dst.is_broadcast() && !dst.is_multicast() {
                    icmp::send_unreachable(self, 2, &pkt[..total]);
                }
            }
        }
    }

    /// Feed a raw IP packet in as if it had arrived on loopback (the self-test's crafted
    /// fragments).
    pub fn inject(&mut self, pkt: &[u8]) {
        self.ip_input(None, pkt, true);
    }

    /// Send `payload` (a whole L4 datagram, too big for one packet) as IP fragments of
    /// 1480 bytes (DF clear). Ok means every fragment was sent or queued.
    pub fn send_ip_fragments(
        &mut self,
        src: Ipv4,
        dst: Ipv4,
        proto: u8,
        payload: &[u8],
    ) -> crate::fs::errno::Result<()> {
        // Fragments sized for the path MTU (1480 bytes of payload on a 1500 path).
        let mtu = self.pmtu.mtu(dst.into(), self.now);
        let chunk = (mtu - IP_HDR) & !7;
        let id = self.iface.next_ip_id();
        self.stats.ip_out_requests += 1;
        for (k, part) in payload.chunks(chunk).enumerate() {
            let f = iface::Frag {
                id,
                off8: (k * chunk / 8) as u16,
                more: (k + 1) * chunk < payload.len(),
            };
            self.frags.stats.creates += 1;
            self.iface
                .send_ip_ex(self.now, src, dst, proto, Some(f), |b| {
                    b[..part.len()].copy_from_slice(part);
                    part.len()
                })?;
        }
        Ok(())
    }

    /// Send an IPv4 packet from `src` (unspecified = pick) to `dst`. Counts it.
    pub fn send_ip(
        &mut self,
        src: Ipv4,
        dst: Ipv4,
        proto: u8,
        fill: impl FnOnce(&mut [u8]) -> usize,
    ) -> crate::fs::errno::Result<()> {
        let src = if src.is_unspecified() && !dst.is_broadcast() {
            self.iface.src_for(dst)
        } else {
            src
        };
        self.stats.ip_out_requests += 1;
        let r = self.iface.send_ip(self.now, src, dst, proto, fill);
        if r == Err(errno::ENETUNREACH) {
            self.stats.ip_out_no_routes += 1;
        }
        r
    }
}

/// Is this a v6-only boot? (fw_cfg file `opt/milkos/net` containing `v6only=1`; see
/// `scripts/run.sh` MILK_V6ONLY=1.)
fn boot_v6only() -> bool {
    let Some(f) = crate::fwcfg::find_file("opt/milkos/net") else {
        return false;
    };
    let mut buf = alloc::vec![0u8; (f.size as usize).min(256)];
    if !crate::fwcfg::read_file(f, &mut buf) {
        return false;
    }
    alloc::string::String::from_utf8_lossy(&buf)
        .split_whitespace()
        .any(|w| w == "v6only=1")
}

/// Is IPv4 off on the links (a v6-only boot)?
pub fn v4_off() -> bool {
    with(|st| st.iface.v4_off)
}

/// Current IPv4 configuration of eth0.
pub fn config() -> iface::IpConfig {
    with(|st| st.iface.eth0().cfg)
}

/// eth0's MAC, if there is a NIC.
pub fn mac() -> Option<Mac> {
    with(|st| st.iface.eth0().up().then_some(st.iface.eth0().mac))
}

/// A link's (name, MAC, IPv4 config), if that interface exists (0 = eth0, 1 = eth1).
pub fn link(k: usize) -> Option<(&'static str, Mac, iface::IpConfig)> {
    with(|st| {
        let l = st.iface.links.get(k)?;
        l.up().then_some((iface::LINK_NAMES[k], l.mac, l.cfg))
    })
}
