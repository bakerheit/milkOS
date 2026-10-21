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
pub mod icmp;
pub mod iface;
pub mod procnet;
pub mod ring;
pub mod selftest;
pub mod services;
pub mod socket;
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
    pub dhcp: dhcp::Dhcp,
    pub ping: icmp::PingTable,
    pub stats: Stats,
    /// Socket slots whose readiness may have changed since the last wake-up pass.
    pub wake_mask: u64,
    /// Wake every waiter (non-socket events, e.g. a ping reply).
    pub wake_any: bool,
    /// Time of the current processing pass (set on entry).
    pub now: u64,
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
        dhcp: dhcp::Dhcp::new(),
        ping: icmp::PingTable::new(),
        stats: Stats::default(),
        wake_mask: 0,
        wake_any: false,
        now: timer::now_ns(),
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
    if let Some(h) = WAKE_HOOK.get::<WakeHook>() {
        for &id in &w.ids[..w.n] {
            h(id);
        }
    }
}

/// Called by the virtio-net driver at attach time (thread context, boot).
pub fn attach_device(dev: VirtioNet) {
    let mac = Mac(dev.mac());
    with(|st| {
        st.iface.mac = mac;
        st.iface.dev = Some(dev);
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
fn net_thread(mut http: services::Http) {
    loop {
        with(|st| {
            st.poll_rx();
            let now = st.now;
            st.iface.tick(now);
            dhcp::tick(st);
            tcp::tick(st);
        });
        services::run(&mut http);
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
/// and the net thread (which also runs the HTTP demo on :80). Call after `sched::init`.
pub fn init() {
    let have_dev = with(|st| {
        if st.iface.dev.is_some() {
            dhcp::start(st);
            true
        } else {
            false
        }
    });
    procnet::init();
    if !THREAD_UP.swap(true, Ordering::AcqRel) {
        // Listen on :80 before the thread runs, so the port is open when init returns.
        let http = services::start();
        drop(sched::spawn_prio("net", sched::Priority::High, move || {
            net_thread(http)
        }));
    }
    kprintln!(
        "[net] stack up: lo{}, {} sockets x 2 x {} KiB buffers, net thread started",
        if have_dev {
            " + eth0 (DHCP started)"
        } else {
            " only (no NIC)"
        },
        socket::MAX_SOCKETS,
        socket::BUF_SIZE / 1024
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
            if s.ty == socket::SockType::Stream {
                for t in [s.tcb.timer, s.tcb.deadline].into_iter().flatten() {
                    d = d.min(t);
                }
            }
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
        for _ in 0..256 {
            let Some(dev) = self.iface.dev.as_mut() else {
                return;
            };
            let Some(b) = dev.rx_pop() else { break };
            let f = b.frame();
            self.eth_input(f);
            if let Some(dev) = self.iface.dev.as_mut() {
                dev.rx_done(b);
            }
        }
        if let Some(dev) = self.iface.dev.as_mut() {
            dev.rx_kick();
            dev.tx_reclaim();
        }
    }

    /// Deliver queued loopback packets (bounded, they may generate more).
    fn poll_loopback(&mut self) {
        for _ in 0..256 {
            let Some(pkt) = self.iface.lo_front() else {
                return;
            };
            self.ip_input(pkt, true);
            self.iface.lo_pop();
        }
    }

    fn eth_input(&mut self, f: &[u8]) {
        if f.len() < ETH_HDR {
            return;
        }
        let dst = Mac([f[0], f[1], f[2], f[3], f[4], f[5]]);
        if dst != self.iface.mac && dst != Mac::BROADCAST {
            return; // multicast or someone else's
        }
        match be16(f, 12) {
            ETHERTYPE_ARP => self.arp_input(&f[ETH_HDR..]),
            ETHERTYPE_IPV4 => self.ip_input(&f[ETH_HDR..], false),
            _ => {}
        }
    }

    fn arp_input(&mut self, p: &[u8]) {
        if p.len() < 28 || be16(p, 0) != 1 || be16(p, 2) != 0x0800 || p[4] != 6 || p[5] != 4 {
            return;
        }
        let op = be16(p, 6);
        let sha = Mac([p[8], p[9], p[10], p[11], p[12], p[13]]);
        let spa = ip_at(p, 14);
        let tpa = ip_at(p, 24);
        let for_us = self.iface.cfg.is_local(tpa);
        // RFC 826 merge: refresh if known; learn if the packet is for us.
        if !spa.is_unspecified() && (for_us || self.iface.arp.has(spa)) {
            self.iface.arp.insert(spa, sha, self.now);
            self.iface.flush_pending(spa, sha);
        }
        if op == arp::OP_REQUEST && for_us {
            self.iface.send_arp(arp::OP_REPLY, sha, spa);
        }
    }

    fn ip_input(&mut self, pkt: &[u8], from_lo: bool) {
        self.stats.ip_in_receives += 1;
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
            // MF set or non-zero offset: we do not reassemble.
            self.stats.ip_reasm_fails += 1;
            return;
        }
        let proto = pkt[9];
        let src = ip_at(pkt, 12);
        let dst = ip_at(pkt, 16);
        let cfg = self.iface.cfg;
        let accept = from_lo
            || cfg.is_local(dst)
            || dst.is_broadcast()
            || (cfg.configured() && dst == cfg.subnet_broadcast())
            // Before DHCP finishes, take any unicast UDP (the DHCP reply may be unicast).
            || (!cfg.configured() && proto == iface::PROTO_UDP);
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
                if !dst.is_broadcast() {
                    icmp::send_unreachable(self, 2, &pkt[..total]);
                }
            }
        }
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

/// Current IPv4 configuration of eth0.
pub fn config() -> iface::IpConfig {
    with(|st| st.iface.cfg)
}

/// eth0's MAC, if there is a NIC.
pub fn mac() -> Option<Mac> {
    with(|st| st.iface.dev.as_ref().map(|_| st.iface.mac))
}
