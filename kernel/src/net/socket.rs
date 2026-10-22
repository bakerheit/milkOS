//! The kernel socket API: what the syscall layer wraps as socket(2), bind(2), ...
//!
//! IPv4 only (`AF_INET`), `SOCK_STREAM` (TCP) and `SOCK_DGRAM` (UDP). Every call is
//! non-blocking: where Linux would sleep it returns `EAGAIN` (or `EINPROGRESS` for
//! connect), and [`poll`] says when to retry. Blocking `read`/`accept`/`connect` = loop on
//! [`poll`] and sleep until [`super::set_wake_hook`] fires for that socket. Errors are Linux
//! errno values (`fs::errno::Errno`), so the syscall layer returns `-(e.0)` unchanged.
//!
//! Sockets live in a fixed table of [`MAX_SOCKETS`] slots. Each socket owns a receive and
//! a send buffer ([`super::ring::Ring`], PMM frames), [`BUF_SIZE`] bytes to start with;
//! TCP grows them as a connection needs (auto-sizing, up to [`MAX_BUF`]) unless the user
//! fixed a size with SO_RCVBUF / SO_SNDBUF. A [`SocketId`] is slot + generation, so a
//! stale id (after close) gives `EBADF`. Connections in TIME_WAIT don't hold a slot
//! (`tcp::TimeWait`).
//!
//! Full list in `docs/networking.md`.

use alloc::vec::Vec;

use super::errno::*;
use super::ring::Ring;
use super::tcp::{self, Tcb, TcpState};
use super::udp;
use super::wire::{IpAddr, Ipv4, Ipv6, SockAddr};
use super::{Stack, with};
use crate::fs::errno::{Errno, Result};

pub const MAX_SOCKETS: usize = 32;
/// Receive and send buffer size a socket starts with (each).
pub const BUF_SIZE: usize = 64 * 1024;
/// Largest a TCP buffer grows to (auto-sizing or SO_RCVBUF/SO_SNDBUF).
pub const MAX_BUF: usize = 4 << 20;
/// Smallest SO_RCVBUF/SO_SNDBUF we take.
pub const MIN_BUF: usize = 4096;
/// First ephemeral port (Linux's default range is 32768-60999; we use IANA's).
const EPHEMERAL_LO: u16 = 49152;

/// Linux `poll(2)` bits.
pub const POLLIN: u32 = 0x001;
pub const POLLPRI: u32 = 0x002;
pub const POLLOUT: u32 = 0x004;
pub const POLLERR: u32 = 0x008;
pub const POLLHUP: u32 = 0x010;
pub const POLLNVAL: u32 = 0x020;
pub const POLLRDNORM: u32 = 0x040;
pub const POLLWRNORM: u32 = 0x100;
pub const POLLRDHUP: u32 = 0x2000;

/// Linux address family / type numbers, for the syscall layer.
pub const AF_INET: u32 = 2;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_DGRAM: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SockType {
    Stream,
    Dgram,
}

impl SockType {
    /// From Linux `type` (flags such as SOCK_NONBLOCK / SOCK_CLOEXEC masked off first).
    pub fn from_linux(ty: u32) -> Option<SockType> {
        match ty & 0xf {
            SOCK_STREAM => Some(SockType::Stream),
            SOCK_DGRAM => Some(SockType::Dgram),
            _ => None,
        }
    }
}

/// Address family of a socket.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    V4,
    V6,
}

pub const AF_INET6: u32 = 10;

/// `shutdown(2)` `how`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shutdown {
    Read,
    Write,
    Both,
}

/// Opaque socket handle: slot index (low 16 bits) + generation (high 16).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SocketId(u32);

impl SocketId {
    pub const INVALID: SocketId = SocketId(u32::MAX);
    pub fn raw(self) -> u32 {
        self.0
    }
    pub fn from_raw(v: u32) -> SocketId {
        SocketId(v)
    }
    fn new(slot: usize, gen_: u16) -> SocketId {
        SocketId((gen_ as u32) << 16 | slot as u32)
    }
    pub fn slot(self) -> usize {
        (self.0 & 0xffff) as usize
    }
    fn gen_(self) -> u16 {
        (self.0 >> 16) as u16
    }
}

pub struct Sock {
    pub ty: SockType,
    /// AF_INET or AF_INET6. An AF_INET6 socket is dual-stack unless `v6only`: its IPv4
    /// peers show as ::ffff:a.b.c.d (the syscall layer maps them), internally they are
    /// plain IPv4.
    pub family: Family,
    pub v6only: bool,
    /// The link of a scoped (link-local) IPv6 address: sin6_scope_id, or the link a
    /// connection to one came in on.
    pub scope: Option<usize>,
    /// IPv6 multicast groups joined (IPV6_JOIN_GROUP): (group, link). Left on close.
    pub groups6: alloc::vec::Vec<(super::wire::Ipv6, usize)>,
    pub local: SockAddr,
    pub remote: SockAddr,
    /// This socket owns its local port (bind / autobind / listen). Accepted children share
    /// the listener's port and do not.
    pub owns_port: bool,
    /// A user handle exists (false: orphan finishing a close, or a child not yet accepted).
    pub user_open: bool,
    /// Pending error for SO_ERROR / the next call.
    pub error: Option<Errno>,
    pub rx: Ring,
    pub tx: Ring,
    pub shut_rd: bool,
    pub shut_wr: bool,
    /// UDP: `connect` set a default peer (and filters what we receive).
    pub udp_connected: bool,
    /// An ICMP "ping socket" (SOCK_DGRAM + IPPROTO_ICMP, as on Linux): datagrams are ICMP
    /// echo messages; the local "port" is the echo identifier.
    pub icmp: bool,
    pub rx_drops: u64,
    /// TCP_NODELAY: no Nagle (small segments go out at once).
    pub nodelay: bool,
    /// SO_RCVBUF / SO_SNDBUF fixed the size: no auto-sizing for that buffer.
    pub rcvbuf_locked: bool,
    pub sndbuf_locked: bool,
    pub tcb: Tcb,
}

pub struct Slot {
    pub gen_: u16,
    pub sock: Option<Sock>,
}

pub struct Table {
    pub slots: Vec<Slot>,
    next_port: u16,
}

impl Table {
    pub fn new() -> Table {
        let mut slots = Vec::with_capacity(MAX_SOCKETS);
        for _ in 0..MAX_SOCKETS {
            slots.push(Slot {
                gen_: 0,
                sock: None,
            });
        }
        Table {
            slots,
            next_port: EPHEMERAL_LO,
        }
    }

    /// Take a free slot and put a fresh socket in it (with fresh buffers). Returns the slot
    /// index, or None if every slot is taken or there is no buffer memory.
    pub fn alloc(&mut self, ty: SockType) -> Option<usize> {
        let i = self.slots.iter().position(|s| s.sock.is_none())?;
        let rx = Ring::alloc(BUF_SIZE)?;
        let tx = Ring::alloc(BUF_SIZE)?;
        let s = &mut self.slots[i];
        s.gen_ = s.gen_.wrapping_add(1);
        s.sock = Some(Sock {
            ty,
            family: Family::V4,
            v6only: false,
            scope: None,
            groups6: alloc::vec::Vec::new(),
            local: SockAddr::ANY,
            remote: SockAddr::ANY,
            owns_port: false,
            user_open: true,
            error: None,
            rx,
            tx,
            shut_rd: false,
            shut_wr: false,
            udp_connected: false,
            icmp: false,
            rx_drops: 0,
            nodelay: false,
            rcvbuf_locked: false,
            sndbuf_locked: false,
            tcb: Tcb::new(),
        });
        Some(i)
    }

    pub fn free(&mut self, i: usize) {
        self.slots[i].sock = None;
    }

    pub fn id_of(&self, i: usize) -> Option<SocketId> {
        let s = self.slots.get(i)?;
        s.sock.as_ref()?;
        Some(SocketId::new(i, s.gen_))
    }

    pub fn get(&mut self, i: usize) -> Option<&mut Sock> {
        self.slots.get_mut(i)?.sock.as_mut()
    }

    /// The socket behind a user handle (must be user-open and of the right generation).
    fn lookup(&mut self, id: SocketId) -> Result<(usize, &mut Sock)> {
        let i = id.slot();
        let s = self.slots.get_mut(i).ok_or(EBADF)?;
        if s.gen_ != id.gen_() {
            return Err(EBADF);
        }
        match s.sock.as_mut() {
            Some(sock) if sock.user_open => Ok((i, sock)),
            _ => Err(EBADF),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &Sock)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.sock.as_ref().map(|k| (i, k)))
    }

    /// Is `port` taken for `ty` on an address overlapping `ip`?
    /// Is `port` taken for `ty` on an address overlapping `ip`? A dual-stack IPv6 wildcard
    /// (`[::]` without IPV6_V6ONLY) overlaps every IPv4 address too (as on Linux).
    pub fn port_in_use(&self, ty: SockType, ip: IpAddr, v6only: bool, port: u16) -> bool {
        let dual = |a: IpAddr, only: bool| a == IpAddr::ANY6 && !only;
        self.iter().any(|(_, s)| {
            if s.ty != ty || !s.owns_port || s.local.port != port {
                return false;
            }
            let (a, b) = (s.local.ip, ip);
            if a.is_v6() == b.is_v6() {
                a.is_unspecified() || b.is_unspecified() || a == b
            } else {
                // One IPv4, one IPv6: only a dual-stack wildcard covers both.
                dual(a, s.v6only) || dual(b, v6only)
            }
        })
    }

    /// Is the 4-tuple used by a TCP connection?
    pub fn tuple_in_use(&self, local: SockAddr, remote: SockAddr) -> bool {
        self.iter().any(|(_, s)| {
            s.ty == SockType::Stream
                && s.local == local
                && s.remote == remote
                && s.tcb.state != TcpState::Closed
        })
    }

    /// Pick a free ephemeral port.
    pub fn ephemeral(&mut self, ty: SockType) -> Result<u16> {
        for _ in 0..=(u16::MAX - EPHEMERAL_LO) {
            let p = self.next_port;
            self.next_port = if p == u16::MAX { EPHEMERAL_LO } else { p + 1 };
            if !self.iter().any(|(_, s)| s.ty == ty && s.local.port == p) {
                return Ok(p);
            }
        }
        Err(EADDRINUSE)
    }
}

// ---- helpers ------------------------------------------------------------------------------

fn check_local_ip(st: &Stack, ip: IpAddr) -> Result<()> {
    let ours = match ip {
        IpAddr::V4(a) => a.is_unspecified() || st.iface.is_our_addr(a),
        IpAddr::V6(a) => a.is_unspecified() || super::ipv6::is_ours(&st.iface, a),
    };
    if ours { Ok(()) } else { Err(EADDRNOTAVAIL) }
}

/// Our source address for `dst` (None: no route / no usable address).
pub fn source_for(st: &Stack, dst: IpAddr, scope: Option<usize>) -> Option<IpAddr> {
    match dst {
        IpAddr::V4(d) => Some(st.iface.src_for(d))
            .filter(|a| !a.is_unspecified())
            .map(IpAddr::V4),
        IpAddr::V6(d) => super::ipv6::src_for(&st.iface, d, scope).map(IpAddr::V6),
    }
}

/// Does socket `s` take traffic addressed to `dst` (its own address, its wildcard for
/// that family, a dual-stack wildcard for IPv4, broadcast/multicast of its family)?
pub fn takes(s: &Sock, dst: IpAddr) -> bool {
    let fam_ok = match (s.family, dst) {
        (Family::V4, IpAddr::V4(_)) => true,
        (Family::V4, IpAddr::V6(_)) => false,
        (Family::V6, IpAddr::V6(_)) => true,
        (Family::V6, IpAddr::V4(_)) => !s.v6only,
    };
    if !fam_ok {
        return false;
    }
    if s.local.ip.is_unspecified() {
        return true;
    }
    s.local.ip == dst
        || ((dst.is_broadcast() || dst.is_multicast()) && s.local.ip.is_v6() == dst.is_v6())
}

/// Can socket `s` talk to `to`? (An AF_INET socket can't reach IPv6; a v6-only socket
/// can't reach IPv4.)
pub fn family_ok(s: &Sock, to: IpAddr) -> Result<()> {
    match (s.family, to) {
        (Family::V4, IpAddr::V6(_)) => Err(EAFNOSUPPORT),
        (Family::V6, IpAddr::V4(_)) if s.v6only => Err(ENETUNREACH),
        _ => Ok(()),
    }
}

fn autobind(st: &mut Stack, i: usize) -> Result<()> {
    let s = st.socks.get(i).ok_or(EBADF)?;
    if s.owns_port {
        return Ok(());
    }
    let ty = s.ty;
    let port = st.socks.ephemeral(ty)?;
    let s = st.socks.get(i).ok_or(EBADF)?;
    s.local.port = port;
    s.owns_port = true;
    Ok(())
}

// ---- the API ------------------------------------------------------------------------------

/// socket(AF_INET, ty, 0).
pub fn socket(ty: SockType) -> Result<SocketId> {
    socket_family(Family::V4, ty)
}

/// socket(AF_INET or AF_INET6, ty, 0). AF_INET6 sockets start dual-stack (IPV6_V6ONLY
/// off, as Linux's default) with the wildcard `[::]`.
pub fn socket_family(family: Family, ty: SockType) -> Result<SocketId> {
    with(|st| {
        let i = st.socks.alloc(ty).ok_or(ENOBUFS)?;
        let s = st.socks.get(i).unwrap();
        s.family = family;
        if family == Family::V6 {
            s.local.ip = IpAddr::ANY6;
        }
        Ok(st.socks.id_of(i).unwrap())
    })
}

/// IPV6_V6ONLY (AF_INET6 sockets only, before bind).
pub fn set_v6only(id: SocketId, on: bool) -> Result<()> {
    with(|st| {
        let (_, s) = st.socks.lookup(id)?;
        if s.family != Family::V6 {
            return Err(ENOPROTOOPT);
        }
        if s.owns_port {
            return Err(EINVAL);
        }
        s.v6only = on;
        Ok(())
    })
}

pub fn v6only(id: SocketId) -> Result<bool> {
    with(|st| st.socks.lookup(id).map(|(_, s)| s.v6only))
}

/// The socket's family (for the syscall layer's address conversions).
pub fn family(id: SocketId) -> Result<Family> {
    with(|st| st.socks.lookup(id).map(|(_, s)| s.family))
}

/// IPV6_JOIN_GROUP / IPV6_LEAVE_GROUP (UDP, AF_INET6): listen to IPv6 multicast `group`
/// on `link` (None: the link it routes to; `ff02::fb` is eth1's). The link reports the
/// membership with MLD (net::mld) and from then on takes that group's traffic.
pub fn group6(
    id: SocketId,
    group: super::wire::Ipv6,
    link: Option<usize>,
    join: bool,
) -> Result<()> {
    with(|st| {
        let k = link
            .or_else(|| super::ipv6::mcast_link(&st.iface, group))
            .ok_or(ENODEV)?;
        if k >= st.iface.links.len() || !st.iface.links[k].up() {
            return Err(ENODEV);
        }
        let (_, s) = st.socks.lookup(id)?;
        if s.ty != SockType::Dgram || s.family != Family::V6 || !group.is_multicast() {
            return Err(EINVAL);
        }
        let have = s.groups6.iter().position(|&m| m == (group, k));
        match (join, have) {
            (true, Some(_)) => return Err(EADDRINUSE),
            (true, None) => {
                if s.groups6.len() >= 20 {
                    return Err(ENOBUFS);
                }
                s.groups6.push((group, k));
                st.iface.links[k].v6.mld.join(group);
            }
            (false, Some(p)) => {
                s.groups6.swap_remove(p);
                st.iface.links[k].v6.mld.leave(group);
            }
            (false, None) => return Err(EADDRNOTAVAIL),
        }
        // Report the change now rather than on the next stack tick, so the join is on the
        // wire before anything the caller sends to the group right after this call.
        super::mld::tick(st, k);
        Ok(())
    })
}

/// The link a scoped (link-local) IPv6 socket is bound to, if any.
pub fn scope(id: SocketId) -> Result<Option<usize>> {
    with(|st| st.socks.lookup(id).map(|(_, s)| s.scope))
}

/// Set the link for a scoped IPv6 address (sin6_scope_id; 0 = none).
pub fn set_scope(id: SocketId, scope: Option<usize>) -> Result<()> {
    with(|st| {
        let (_, s) = st.socks.lookup(id)?;
        if scope.is_some() {
            s.scope = scope;
        }
        Ok(())
    })
}

/// socket(AF_INET, SOCK_DGRAM, IPPROTO_ICMP): a Linux-style ping socket. `sendto` takes
/// an ICMP echo request (8-byte header + payload; we fill in the identifier and checksum),
/// `recvfrom` returns the echo replies addressed to this socket (ICMP header + payload,
/// no IP header). Unprivileged, like Linux with `net.ipv4.ping_group_range` open.
pub fn socket_icmp() -> Result<SocketId> {
    with(|st| {
        let i = st.socks.alloc(SockType::Dgram).ok_or(ENOBUFS)?;
        st.socks.get(i).unwrap().icmp = true;
        Ok(st.socks.id_of(i).unwrap())
    })
}

/// socket(AF_INET6, SOCK_DGRAM, IPPROTO_ICMPV6): the same for ICMPv6 echo (type 128 in,
/// 129 back). IPv6 only.
pub fn socket_icmp6() -> Result<SocketId> {
    with(|st| {
        let i = st.socks.alloc(SockType::Dgram).ok_or(ENOBUFS)?;
        let s = st.socks.get(i).unwrap();
        s.icmp = true;
        s.family = Family::V6;
        s.v6only = true;
        s.local.ip = IpAddr::ANY6;
        Ok(st.socks.id_of(i).unwrap())
    })
}

/// bind(2). Port 0 picks an ephemeral port. The address must be unspecified, loopback or
/// eth0's (`EADDRNOTAVAIL`). Bound already: `EINVAL`. Taken: `EADDRINUSE` (connections in
/// TIME_WAIT or closing in the background never block a bind, as with SO_REUSEADDR).
pub fn bind(id: SocketId, addr: SockAddr) -> Result<()> {
    with(|st| {
        check_local_ip(st, addr.ip)?;
        let (i, s) = st.socks.lookup(id)?;
        if s.owns_port || s.tcb.state != TcpState::Closed {
            return Err(EINVAL);
        }
        family_ok(s, addr.ip).map_err(|_| EAFNOSUPPORT)?;
        let (ty, v6only) = (s.ty, s.v6only);
        let port = if addr.port == 0 {
            st.socks.ephemeral(ty)?
        } else if st.socks.port_in_use(ty, addr.ip, v6only, addr.port) {
            return Err(EADDRINUSE);
        } else {
            addr.port
        };
        let s = st.socks.get(i).unwrap();
        s.local = SockAddr::new(addr.ip, port);
        s.owns_port = true;
        Ok(())
    })
}

/// listen(2). TCP only (`EOPNOTSUPP`). Unbound sockets get an ephemeral port. The backlog
/// is clamped to 1..=16 (connections in the handshake count against it).
pub fn listen(id: SocketId, backlog: usize) -> Result<()> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        if s.ty != SockType::Stream {
            return Err(EOPNOTSUPP);
        }
        match s.tcb.state {
            TcpState::Closed => {}
            TcpState::Listen => {
                s.tcb.backlog = backlog.clamp(1, tcp::MAX_BACKLOG) as u8;
                return Ok(());
            }
            _ => return Err(EINVAL),
        }
        autobind(st, i)?;
        let s = st.socks.get(i).unwrap();
        s.tcb.state = TcpState::Listen;
        s.tcb.backlog = backlog.clamp(1, tcp::MAX_BACKLOG) as u8;
        Ok(())
    })
}

/// accept(2): an established connection from the queue, with the peer's address.
/// `EAGAIN` if none is ready, `EINVAL` if not listening.
pub fn accept(id: SocketId) -> Result<(SocketId, SockAddr)> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        if s.tcb.state != TcpState::Listen {
            return Err(EINVAL);
        }
        let child = tcp::accept_ready(st, i).ok_or(EAGAIN)?;
        let c = st.socks.get(child).unwrap();
        c.user_open = true;
        c.tcb.parent = None;
        let peer = c.remote;
        Ok((st.socks.id_of(child).unwrap(), peer))
    })
}

/// connect(2). TCP: the first call sends the SYN and returns `EINPROGRESS`; while the
/// handshake runs, `EALREADY`; once established, the next call returns Ok (later ones
/// `EISCONN`); if it failed, the error (`ECONNREFUSED`, `ETIMEDOUT`, ...). Watch for
/// `POLLOUT` (connected) or `POLLERR` (failed; read it with [`take_error`]).
/// UDP: sets the default peer, Ok at once. An unspecified address means 127.0.0.1.
pub fn connect(id: SocketId, addr: SockAddr) -> Result<()> {
    with(|st| {
        let addr = match addr.ip {
            IpAddr::V4(a) if a.is_unspecified() => SockAddr::new(Ipv4::LOCALHOST, addr.port),
            IpAddr::V6(a) if a.is_unspecified() => SockAddr::new(Ipv6::LOOPBACK, addr.port),
            _ => addr,
        };
        let (i, s) = st.socks.lookup(id)?;
        family_ok(s, addr.ip)?;
        let scope = s.scope;
        if s.ty == SockType::Dgram {
            if addr.port == 0 {
                return Err(EINVAL);
            }
            s.remote = addr;
            s.udp_connected = true;
            if s.local.ip.is_unspecified() || s.local.ip.is_v6() != addr.ip.is_v6() {
                // Linux fixes the local address at connect.
                let src = source_for(st, addr.ip, scope).ok_or(ENETUNREACH)?;
                st.socks.get(i).unwrap().local.ip = src;
            }
            return autobind(st, i);
        }
        if let Some(e) = s.error.take() {
            s.tcb.connect_pending = false;
            return Err(e);
        }
        match s.tcb.state {
            TcpState::Closed => {}
            TcpState::SynSent | TcpState::SynReceived => return Err(EALREADY),
            TcpState::Listen => return Err(EISCONN),
            _ => {
                if s.tcb.connect_pending {
                    s.tcb.connect_pending = false;
                    return Ok(());
                }
                return Err(EISCONN);
            }
        }
        if addr.port == 0 {
            return Err(ECONNREFUSED);
        }
        if s.tcb.used {
            return Err(EISCONN); // it was connected and has closed: not reusable (Linux)
        }
        // Fresh TCB (a failed attempt may have left state behind).
        s.tcb = Tcb::new();
        s.rx.clear();
        s.tx.clear();
        let src = source_for(st, addr.ip, scope).ok_or(ENETUNREACH)?;
        let s = st.socks.get(i).unwrap();
        if s.local.ip.is_unspecified() || s.local.ip.is_v6() != addr.ip.is_v6() {
            s.local.ip = src;
        }
        autobind(st, i)?;
        let local = st.socks.get(i).unwrap().local;
        if st.socks.tuple_in_use(local, addr) {
            return Err(EADDRINUSE);
        }
        tcp::connect(st, i, addr);
        Err(EINPROGRESS)
    })
}

/// send(2) without flags. TCP: queues what fits in the send buffer (short count possible),
/// `EAGAIN` if it is full or the handshake is still running, `EPIPE` after shutdown/close
/// of the write side, `ENOTCONN` if never connected. UDP: needs `connect` (`EDESTADDRREQ`).
pub fn send(id: SocketId, buf: &[u8]) -> Result<usize> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        match s.ty {
            SockType::Dgram => {
                if !s.udp_connected {
                    return Err(EDESTADDRREQ);
                }
                let to = s.remote;
                udp::sendto(st, i, buf, to)
            }
            SockType::Stream => tcp::send(st, i, buf),
        }
    })
}

/// sendto(2). UDP: one datagram (max 1472 bytes: `EMSGSIZE`, we do not fragment). TCP:
/// the address is ignored (as Linux does for connected sockets).
pub fn sendto(id: SocketId, buf: &[u8], to: SockAddr) -> Result<usize> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        match s.ty {
            SockType::Dgram if s.icmp => super::icmp::sendto(st, i, buf, to),
            SockType::Dgram => udp::sendto(st, i, buf, to),
            SockType::Stream => tcp::send(st, i, buf),
        }
    })
}

/// recv(2) without flags. TCP: Ok(0) = end of stream (peer closed, or shut down for
/// reading). `EAGAIN` if nothing is there yet. A reset connection gives `ECONNRESET` once.
/// UDP: one datagram, truncated to `buf` (the rest is discarded, as on Linux).
pub fn recv(id: SocketId, buf: &mut [u8]) -> Result<usize> {
    recvfrom(id, buf).map(|(n, _)| n)
}

/// recvfrom(2): like [`recv`], plus the sender (TCP: the peer).
pub fn recvfrom(id: SocketId, buf: &mut [u8]) -> Result<(usize, SockAddr)> {
    recvfrom_scoped(id, buf).map(|(n, a, _)| (n, a))
}

/// [`recvfrom`], plus the link a link-local IPv6 sender is on (its sin6_scope_id).
pub fn recvfrom_scoped(id: SocketId, buf: &mut [u8]) -> Result<(usize, SockAddr, Option<usize>)> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        match s.ty {
            SockType::Dgram => udp::recvfrom(s, buf),
            SockType::Stream => {
                let (peer, scope) = (s.remote, s.scope);
                tcp::recv(st, i, buf).map(|n| (n, peer, scope))
            }
        }
    })
}

/// shutdown(2). TCP write side: FIN after the queued data. Read side: further recv gives 0.
pub fn shutdown(id: SocketId, how: Shutdown) -> Result<()> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        if s.ty == SockType::Stream && !s.tcb.state.synchronized() {
            return Err(ENOTCONN);
        }
        if matches!(how, Shutdown::Read | Shutdown::Both) {
            s.shut_rd = true;
        }
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            s.shut_wr = true;
            if s.ty == SockType::Stream {
                tcp::shutdown_write(st, i);
            }
        }
        st.wake(i);
        Ok(())
    })
}

/// close(2). The handle is dead at once. TCP keeps going in the background: queued data is
/// still delivered, then FIN (unread received data instead aborts with RST, like Linux).
/// A listener resets its not-yet-accepted connections.
pub fn close(id: SocketId) -> Result<()> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        match s.ty {
            SockType::Dgram => {
                for (g, k) in core::mem::take(&mut s.groups6) {
                    st.iface.links[k].v6.mld.leave(g);
                }
                st.socks.free(i);
                Ok(())
            }
            SockType::Stream => {
                tcp::close(st, i);
                Ok(())
            }
        }
    })
}

/// poll(2) readiness, Linux bits (`POLLIN`, `POLLOUT`, `POLLERR`, `POLLHUP`, `POLLRDHUP`,
/// plus the RDNORM/WRNORM aliases). `POLLNVAL` for a bad handle.
pub fn poll(id: SocketId) -> u32 {
    with(|st| {
        let Ok((i, s)) = st.socks.lookup(id) else {
            return POLLNVAL;
        };
        match s.ty {
            SockType::Dgram => {
                let mut m = POLLOUT | POLLWRNORM;
                if !s.rx.is_empty() {
                    m |= POLLIN | POLLRDNORM;
                }
                if s.error.is_some() {
                    m |= POLLERR;
                }
                m
            }
            SockType::Stream => tcp::poll(st, i),
        }
    })
}

/// SO_RCVBUF / SO_SNDBUF: give the buffer at least `bytes` (clamped to
/// [`MIN_BUF`]..=[`MAX_BUF`]) and stop auto-sizing it. Buffers only grow (a smaller value
/// just fixes the current size).
pub fn set_buf(id: SocketId, receive: bool, bytes: usize) -> Result<()> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        let want = bytes.clamp(MIN_BUF, MAX_BUF);
        let ok = if receive {
            s.rcvbuf_locked = true;
            s.rx.grow(want)
        } else {
            s.sndbuf_locked = true;
            s.tx.grow(want)
        };
        if receive && ok {
            // A bigger window: tell the peer.
            tcp::output(st, i);
        }
        if ok { Ok(()) } else { Err(ENOBUFS) }
    })
}

/// SO_RCVBUF / SO_SNDBUF: the buffer's size now.
pub fn get_buf(id: SocketId, receive: bool) -> Result<usize> {
    with(|st| {
        let (_, s) = st.socks.lookup(id)?;
        Ok(if receive { s.rx.cap() } else { s.tx.cap() })
    })
}

/// TCP_NODELAY.
pub fn set_nodelay(id: SocketId, on: bool) -> Result<()> {
    with(|st| {
        let (i, s) = st.socks.lookup(id)?;
        s.nodelay = on;
        if on {
            tcp::output(st, i); // anything Nagle held back goes now
        }
        Ok(())
    })
}

pub fn nodelay(id: SocketId) -> Result<bool> {
    with(|st| st.socks.lookup(id).map(|(_, s)| s.nodelay))
}

/// SO_ERROR: take the pending error, if any.
pub fn take_error(id: SocketId) -> Result<Option<Errno>> {
    with(|st| {
        let (_, s) = st.socks.lookup(id)?;
        Ok(s.error.take())
    })
}

/// "tcp", "udp" or "icmp" (ping socket).
pub fn proto_name(id: SocketId) -> Result<&'static str> {
    with(|st| {
        st.socks.lookup(id).map(|(_, s)| match s.ty {
            SockType::Stream => "tcp",
            SockType::Dgram if s.icmp => "icmp",
            SockType::Dgram => "udp",
        })
    })
}

/// SO_TYPE.
pub fn sock_type(id: SocketId) -> Result<SockType> {
    with(|st| st.socks.lookup(id).map(|(_, s)| s.ty))
}

/// getsockname(2).
pub fn local_addr(id: SocketId) -> Result<SockAddr> {
    with(|st| st.socks.lookup(id).map(|(_, s)| s.local))
}

/// getpeername(2). `ENOTCONN` if there is no peer.
pub fn peer_addr(id: SocketId) -> Result<SockAddr> {
    with(|st| {
        let (_, s) = st.socks.lookup(id)?;
        let connected = match s.ty {
            SockType::Dgram => s.udp_connected,
            SockType::Stream => s.tcb.state.synchronized(),
        };
        if connected {
            Ok(s.remote)
        } else {
            Err(ENOTCONN)
        }
    })
}

/// Bytes [`send`] would accept right now (TCP: free send-buffer space; UDP: max datagram).
pub fn send_capacity(id: SocketId) -> Result<usize> {
    with(|st| {
        let (_, s) = st.socks.lookup(id)?;
        Ok(match s.ty {
            SockType::Dgram => udp::MAX_DATAGRAM,
            SockType::Stream if s.tcb.state.can_send() && !s.shut_wr => s.tx.free(),
            SockType::Stream => 0,
        })
    })
}

/// Bytes waiting to be read (FIONREAD / SIOCINQ; UDP: size of the next datagram).
pub fn pending_input(id: SocketId) -> Result<usize> {
    with(|st| {
        let (_, s) = st.socks.lookup(id)?;
        Ok(match s.ty {
            SockType::Dgram => udp::next_len(s),
            SockType::Stream => s.rx.len(),
        })
    })
}

/// Number of sockets in use (all states, background ones included).
pub fn count() -> usize {
    with(|st| st.socks.iter().count())
}

// ---- blocking helpers (thread context) ------------------------------------------------------

/// Sleep until `poll(id)` has any of `mask` (or POLLERR / POLLHUP / POLLNVAL), or
/// `timeout_ns` passes. Returns the bits that were set (0 = timed out). This is the
/// building block for blocking read/accept/connect: the stack wakes `READY_WQ` on every
/// readiness change.
///
/// A user thread also stops waiting when a signal is pending (0 is returned; the caller
/// checks [`interrupted`] and fails with `EINTR`), so Ctrl-C reaches a process blocked in
/// accept / recv / send / connect. Kernel threads are never interrupted.
pub fn wait(id: SocketId, mask: u32, timeout_ns: u64) -> u32 {
    let want = mask | POLLERR | POLLHUP | POLLNVAL;
    let mut r = 0;
    super::READY_WQ.wait_until_timeout(
        || {
            r = poll(id) & want;
            r != 0 || interrupted()
        },
        timeout_ns,
    );
    r
}

/// Does the calling (user) thread have a signal that should end a blocking socket call?
pub fn interrupted() -> bool {
    crate::proc::signal::interrupted()
}

/// Send one ICMP echo request to `dst` and wait for the reply (thread context).
pub fn ping(dst: Ipv4, seq: u16, timeout_ns: u64) -> Result<super::icmp::PingReply> {
    with(|st| super::icmp::send_ping(st, dst, seq))?;
    let mut got = None;
    super::READY_WQ.wait_until_timeout(
        || {
            got = with(|st| st.ping.take(dst, seq));
            got.is_some()
        },
        timeout_ns,
    );
    got.ok_or(ETIMEDOUT)
}
