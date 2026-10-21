//! Socket syscalls (Linux aarch64 numbers) on top of the kernel socket API in `net::socket`.
//! Written by the net side; kept here because it needs the fd table and user copies.
//!
//! AF_UNIX sockets (and socketpair, sendmsg, recvmsg) are routed to proc/unixsys.rs.
//! AF_INET: SOCK_STREAM (TCP), SOCK_DGRAM (UDP, or ICMP echo with IPPROTO_ICMP, the
//! Linux "ping socket"), `struct sockaddr_in` (16 bytes). AF_INET6: the same over IPv6
//! (IPPROTO_ICMPV6 for ping), `struct sockaddr_in6` (28 bytes, sin6_scope_id = ifindex),
//! dual-stack unless IPV6_V6ONLY: IPv4 peers show as ::ffff:a.b.c.d. Also ppoll(2), for any
//! fd. Blocking
//! unless the fd is O_NONBLOCK (SOCK_NONBLOCK / fcntl) or the call passes MSG_DONTWAIT.
//! No signals exist yet, so a blocking call waits until the socket is ready.

use alloc::sync::Arc;

use super::current;
use super::errno::*;
use super::syscall::{copy_in, copy_out, file};
use crate::fs::OpenFile;
use crate::net::errno as nerr;
use crate::net::ipv6;
use crate::net::socket::{self, Family, POLLOUT, Shutdown, SockType, SocketId};
use crate::net::sockfile::{self, FOREVER};
use crate::net::wire::{IpAddr, Ipv4, Ipv6, SockAddr};

pub const SOCKET: u64 = 198;
pub const BIND: u64 = 200;
pub const LISTEN: u64 = 201;
pub const ACCEPT: u64 = 202;
pub const CONNECT: u64 = 203;
pub const GETSOCKNAME: u64 = 204;
pub const GETPEERNAME: u64 = 205;
pub const SENDTO: u64 = 206;
pub const RECVFROM: u64 = 207;
pub const SETSOCKOPT: u64 = 208;
pub const GETSOCKOPT: u64 = 209;
pub const SHUTDOWN: u64 = 210;
pub const ACCEPT4: u64 = 242;
pub const PPOLL: u64 = 73;

const SOCK_NONBLOCK: u32 = 0o4000;
const SOCK_CLOEXEC: u32 = 0o2000000;
const MSG_DONTWAIT: u32 = 0x40;
const SOL_SOCKET: u32 = 1;
const IPPROTO_TCP: u32 = 6;
const SO_TYPE: u32 = 3;
const SO_ERROR: u32 = 4;
const SO_RCVTIMEO: u32 = 20;
const SO_SNDBUF: u32 = 7;
const SO_RCVBUF: u32 = 8;
const TCP_NODELAY: u32 = 1;
const IPPROTO_IPV6: u32 = 41;
const IPV6_V6ONLY: u32 = 26;
const IPV6_JOIN_GROUP: u32 = 20;
const IPV6_LEAVE_GROUP: u32 = 21;
const SO_SNDTIMEO: u32 = 21;
const ENOPROTOOPT: Errno = Errno(92);
const MAX_IO: usize = 1 << 20;
/// milkOS: allocate a pseudo-terminal (crate::pty); the master fd is returned.
pub const MILK_OPENPTY: u64 = 0x1002;

/// Is `nr` one of ours?
pub fn handles(nr: u64) -> bool {
    use super::unixsys::{RECVMSG, SENDMSG, SOCKETPAIR};
    matches!(
        nr,
        SOCKET | SOCKETPAIR | BIND..=SHUTDOWN | SENDMSG | RECVMSG | ACCEPT4 | PPOLL | MILK_OPENPTY
    )
}

/// Is `addr` reachable with only the Local network capability (loopback / link-local, and
/// over IPv6 also the VM link's own prefix, which is eth1's equivalent of 169.254/16)?
fn is_local_addr(addr: &SockAddr) -> bool {
    match addr.ip {
        IpAddr::V4(a) => a.is_loopback() || a.0[0] == 169 && a.0[1] == 254 || a.is_unspecified(),
        IpAddr::V6(a) => {
            a.is_loopback()
                || a.is_link_local()
                || a.is_unspecified()
                || a.0[..8] == ipv6::VM_LINK_PREFIX
        }
    }
}

/// Deny if the calling process may not reach `addr` with its net capability.
fn net_allows(addr: &SockAddr, op: &str) -> KResult<()> {
    use super::caps::Net;
    match super::current_caps().net {
        Net::All => Ok(()),
        Net::Local if is_local_addr(addr) => Ok(()),
        Net::Local => Err(super::caps::deny(
            op,
            &alloc::format!("dst={} net=local-only", addr.ip),
        )),
        Net::None => Err(super::caps::deny(op, "net capability dropped")),
    }
}

pub fn dispatch(nr: u64, a: [u64; 6]) -> KResult<usize> {
    let fd = a[0] as i32 as i64;
    let (a1, a2, a3, a4, a5) = (
        a[1] as usize,
        a[2] as usize,
        a[3] as usize,
        a[4] as usize,
        a[5] as usize,
    );
    // AF_UNIX: its own module (proc/unix.rs); not gated by the net capability (paths by
    // the fs grants, abstract names by net, when connecting).
    if nr == MILK_OPENPTY {
        return sys_openpty(a[0] as usize, a[1] as u32);
    }
    if nr == SOCKET && a[0] as u32 == super::unix::AF_UNIX {
        return super::unixsys::socket(a[1] as u32, a[2] as u32);
    }
    if nr == super::unixsys::SOCKETPAIR {
        return super::unixsys::socketpair(a[0] as u32, a[1] as u32, a[2] as u32, a3);
    }
    if !matches!(nr, SOCKET | PPOLL)
        && let Ok(f) = file(fd)
        && let Some(s) = super::unix::of(&f)
    {
        return super::unixsys::call(nr, a, &f, s);
    }
    if matches!(nr, super::unixsys::SENDMSG | super::unixsys::RECVMSG) {
        return Err(Errno(95)); // EOPNOTSUPP: sendmsg/recvmsg are AF_UNIX only for now
    }
    match nr {
        SOCKET => {
            if super::current_caps().net == super::caps::Net::None {
                return Err(super::caps::deny("socket", "net capability dropped"));
            }
            sys_socket(a[0] as u32, a[1] as u32, a[2] as u32)
        }
        PPOLL => sys_ppoll(a[0] as usize, a1, a2),
        BIND => with_sock(fd, |s, _| socket::bind(s, read_addr(s, a1, a2)?).map(|_| 0)),
        LISTEN => with_sock(fd, |s, _| {
            let local = socket::local_addr(s).unwrap_or(SockAddr::ANY);
            let r = socket::listen(s, a1).map(|_| 0);
            let local = socket::local_addr(s).unwrap_or(local);
            net_event("net.listen", s, "local", local, &r);
            r
        }),
        ACCEPT => sys_accept(fd, a1, a2, 0),
        ACCEPT4 => sys_accept(fd, a1, a2, a3 as u32),
        CONNECT => with_sock(fd, |s, nb| {
            let addr = read_addr(s, a1, a2)?;
            net_allows(&addr, "connect")?;
            let r = connect(s, addr, nb);
            if !matches!(r, Err(e) if e == nerr::EALREADY || e == nerr::EISCONN) {
                net_event("net.connect", s, "dst", addr, &r);
            }
            r
        }),
        GETSOCKNAME => with_sock(fd, |s, _| {
            write_addr(s, socket::local_addr(s)?, socket::scope(s)?, a1, a2)
        }),
        GETPEERNAME => with_sock(fd, |s, _| {
            write_addr(s, socket::peer_addr(s)?, socket::scope(s)?, a1, a2)
        }),
        SENDTO => with_sock(fd, |s, nb| {
            let data = copy_in(a1, a2.min(MAX_IO))?;
            let to = if a4 != 0 {
                Some(read_addr(s, a4, a5)?)
            } else {
                None
            };
            if let Some(to) = &to {
                net_allows(to, "sendto")?;
            }
            if let Some(to) = to {
                // FNV-1a of the destination: audit each new one once.
                let key = to
                    .ip
                    .bytes()
                    .iter()
                    .chain(&to.port.to_be_bytes())
                    .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
                        (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
                    })
                    | 1;
                let f = file(fd)?;
                let sf = sockfile::of(&f)?;
                if sf
                    .audited_dst
                    .swap(key, core::sync::atomic::Ordering::Relaxed)
                    != key
                {
                    net_event("net.sendto", s, "dst", to, &Ok(0));
                }
            }
            sockfile::send(s, &data, to, nb || a3 as u32 & MSG_DONTWAIT != 0)
        }),
        RECVFROM => with_sock(fd, |s, nb| {
            let mut buf = alloc::vec![0u8; a2.min(MAX_IO)];
            let nb = nb || a3 as u32 & MSG_DONTWAIT != 0;
            let (n, from, scope) = sockfile::recv(s, &mut buf, nb, timeout_of(fd))?;
            copy_out(a1, &buf[..n])?;
            if a4 != 0 {
                write_addr(s, from, scope, a4, a5)?;
            }
            Ok(n)
        }),
        SETSOCKOPT => setsockopt(fd, a1 as u32, a2 as u32, a3, a4),
        GETSOCKOPT => with_sock(fd, |s, _| getsockopt(s, a1 as u32, a2 as u32, a3, a4)),
        SHUTDOWN => with_sock(fd, |s, _| {
            let how = match a1 {
                0 => Shutdown::Read,
                1 => Shutdown::Write,
                2 => Shutdown::Both,
                _ => return Err(EINVAL),
            };
            socket::shutdown(s, how).map(|_| 0)
        }),
        _ => Err(ENOSYS),
    }
}

/// Run `f` with the socket behind `fd` and its non-blocking flag.
fn with_sock(fd: i64, f: impl FnOnce(SocketId, bool) -> KResult<usize>) -> KResult<usize> {
    let file = file(fd)?;
    let s = sockfile::of(&file)?;
    let (id, nb) = (s.id, s.nonblocking());
    f(id, nb)
}

/// SO_RCVTIMEO of `fd` (FOREVER if unset).
fn timeout_of(fd: i64) -> u64 {
    file(fd)
        .ok()
        .and_then(|f| sockfile::of(&f).ok().map(|s| s.rcv_timeout()))
        .unwrap_or(FOREVER)
}

/// MILK_OPENPTY(&mut slave_number: u32, flags): a free pty's master, as a new fd
/// (O_CLOEXEC in `flags` = 0o2000000 sets close-on-exec).
fn sys_openpty(slave_out: usize, flags: u32) -> KResult<usize> {
    let (n, master) = crate::pty::open()?;
    let fd = install(
        OpenFile::new(master, crate::fs::abi::O_RDWR),
        flags & crate::fs::abi::O_CLOEXEC != 0,
    )?;
    if slave_out != 0 {
        copy_out(slave_out, &(n as u32).to_le_bytes())?;
    }
    crate::audit!("pty.open", "pty=ttyp{n} fd={fd}");
    Ok(fd)
}

fn install(of: OpenFile, cloexec: bool) -> KResult<usize> {
    let fd = current().inner.lock().fds.alloc(Arc::new(of), cloexec)?;
    Ok(fd as usize)
}

fn sys_socket(domain: u32, ty: u32, proto: u32) -> KResult<usize> {
    let fam = match domain {
        socket::AF_INET => Family::V4,
        socket::AF_INET6 => Family::V6,
        _ => return Err(nerr::EAFNOSUPPORT),
    };
    let kind = SockType::from_linux(ty).ok_or(nerr::EPROTONOSUPPORT)?;
    let id = match (fam, kind, proto) {
        (_, SockType::Stream, 0 | 6) | (_, SockType::Dgram, 0 | 17) => {
            socket::socket_family(fam, kind)?
        }
        // Linux "ping socket": unprivileged ICMP echo (see net::socket::socket_icmp).
        (Family::V4, SockType::Dgram, 1) => socket::socket_icmp()?,
        (Family::V6, SockType::Dgram, 58) => socket::socket_icmp6()?,
        _ => return Err(nerr::EPROTONOSUPPORT),
    };
    // From here the OpenFile owns the socket (closing it on drop).
    install(
        sockfile::open(id, ty & SOCK_NONBLOCK != 0),
        ty & SOCK_CLOEXEC != 0,
    )
}

fn sys_accept(fd: i64, addr: usize, addrlen: usize, flags: u32) -> KResult<usize> {
    let file = file(fd)?;
    let s = sockfile::of(&file)?;
    let (id, nb) = (s.id, s.nonblocking());
    let end = crate::timer::now_ns().saturating_add(s.rcv_timeout());
    let (child, peer) = loop {
        match socket::accept(id) {
            Err(e) if e == EAGAIN && !nb => {
                let left = end.saturating_sub(crate::timer::now_ns());
                if left == 0 {
                    return Err(EAGAIN);
                }
                socket::wait(id, socket::POLLIN, left);
                if socket::interrupted() {
                    return Err(crate::fs::errno::Errno::EINTR);
                }
            }
            r => break r?,
        }
    };
    let local = socket::local_addr(child).unwrap_or(SockAddr::ANY);
    let of = sockfile::open(child, flags & SOCK_NONBLOCK != 0);
    let newfd = install(of, flags & SOCK_CLOEXEC != 0)?;
    crate::audit!(
        "net.accept",
        "proto=tcp local={local} peer={peer} fd={newfd}"
    );
    if addr != 0 {
        write_addr(
            child,
            peer,
            socket::scope(child).unwrap_or(None),
            addr,
            addrlen,
        )?;
    }
    Ok(newfd)
}

/// Audit a network operation: `<event> proto=.. <what>=ADDR result=ok|ERRNO`.
fn net_event(event: &str, s: SocketId, what: &str, addr: SockAddr, r: &KResult<usize>) {
    let proto = socket::proto_name(s).unwrap_or("?");
    let res = match r {
        Ok(_) => "ok",
        Err(e) => nerr::name(*e),
    };
    crate::audit!(event, "proto={proto} {what}={addr} result={res}");
}

fn connect(s: SocketId, addr: SockAddr, nb: bool) -> KResult<usize> {
    match socket::connect(s, addr) {
        Err(e) if e == nerr::EINPROGRESS && !nb => {
            socket::wait(s, POLLOUT, FOREVER);
            if socket::interrupted() {
                return Err(crate::fs::errno::Errno::EINTR);
            }
            socket::connect(s, addr).map(|_| 0)
        }
        r => r.map(|_| 0),
    }
}

fn setsockopt(fd: i64, level: u32, opt: u32, val: usize, len: usize) -> KResult<usize> {
    let f = file(fd)?;
    let sf = sockfile::of(&f)?;
    match (level, opt) {
        (SOL_SOCKET, SO_RCVTIMEO) => {
            // struct timeval { tv_sec: i64, tv_usec: i64 }; all zero = no timeout.
            if len < 16 {
                return Err(EINVAL);
            }
            let b = copy_in(val, 16)?;
            let sec = i64::from_le_bytes(b[0..8].try_into().unwrap());
            let usec = i64::from_le_bytes(b[8..16].try_into().unwrap());
            if sec < 0 || !(0..1_000_000).contains(&usec) {
                return Err(EINVAL);
            }
            sf.set_rcv_timeout((sec as u64).saturating_mul(1_000_000_000) + usec as u64 * 1000);
            Ok(0)
        }
        (IPPROTO_IPV6, IPV6_JOIN_GROUP | IPV6_LEAVE_GROUP) => {
            // struct ipv6_mreq { in6_addr multiaddr; unsigned ifindex; }
            if len < 20 {
                return Err(EINVAL);
            }
            let b = copy_in(val, 20)?;
            let g = Ipv6(b[0..16].try_into().unwrap());
            let link = link_of_ifindex(u32::from_le_bytes(b[16..20].try_into().unwrap()))?;
            socket::group6(sf.id, g, link, opt == IPV6_JOIN_GROUP)?;
            Ok(0)
        }
        (IPPROTO_IPV6, IPV6_V6ONLY) => {
            if len < 4 {
                return Err(EINVAL);
            }
            let b = copy_in(val, 4)?;
            socket::set_v6only(sf.id, i32::from_le_bytes(b[0..4].try_into().unwrap()) != 0)?;
            Ok(0)
        }
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) | (IPPROTO_TCP, TCP_NODELAY) => {
            if len < 4 {
                return Err(EINVAL);
            }
            let b = copy_in(val, 4)?;
            let v = i32::from_le_bytes(b[0..4].try_into().unwrap());
            match opt {
                TCP_NODELAY if level == IPPROTO_TCP => socket::set_nodelay(sf.id, v != 0)?,
                SO_RCVBUF => socket::set_buf(sf.id, true, v.max(0) as usize)?,
                _ => socket::set_buf(sf.id, false, v.max(0) as usize)?,
            }
            Ok(0)
        }
        // Accepted and ignored: SO_REUSEADDR (we always allow it), SO_KEEPALIVE,
        // SO_REUSEPORT, SO_SNDTIMEO (sends only wait for buffer space).
        (SOL_SOCKET, 2 | 9 | 15 | SO_SNDTIMEO) => Ok(0),
        _ => Err(ENOPROTOOPT),
    }
}

/// ppoll(fds, nfds, timeout, sigmask): any fd kind (sockets and the console really wait;
/// other files are always ready). The sigmask is ignored (no signals yet).
fn sys_ppoll(fds: usize, nfds: usize, tmo: usize) -> KResult<usize> {
    if nfds > 256 {
        return Err(EINVAL);
    }
    let raw = copy_in(fds, nfds * 8)?;
    let mut entries: alloc::vec::Vec<(i32, u32, Option<Arc<OpenFile>>)> = raw
        .chunks(8)
        .map(|c| {
            let fd = i32::from_le_bytes(c[0..4].try_into().unwrap());
            let ev = u16::from_le_bytes([c[4], c[5]]) as u32;
            let f = if fd >= 0 { file(fd as i64).ok() } else { None };
            (fd, ev, f)
        })
        .collect();
    let timeout = if tmo == 0 {
        FOREVER
    } else {
        let t = copy_in(tmo, 16)?;
        let sec = i64::from_le_bytes(t[0..8].try_into().unwrap());
        let nsec = i64::from_le_bytes(t[8..16].try_into().unwrap());
        if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
            return Err(EINVAL);
        }
        (sec as u64).saturating_mul(1_000_000_000) + nsec as u64
    };
    let end = crate::timer::now_ns().saturating_add(timeout);
    let always = socket::POLLERR | socket::POLLHUP | socket::POLLNVAL;
    let mut out = alloc::vec![0u8; raw.len()];
    loop {
        let g = crate::poll::generation();
        let mut ready = 0;
        for (k, (fd, ev, f)) in entries.iter_mut().enumerate() {
            let rev = match (*fd, f.as_ref()) {
                (fd, _) if fd < 0 => 0,
                (_, None) => socket::POLLNVAL,
                (_, Some(f)) => f.inode().poll() & (*ev | always),
            };
            out[k * 8..k * 8 + 8].copy_from_slice(&raw[k * 8..k * 8 + 8]);
            out[k * 8 + 6..k * 8 + 8].copy_from_slice(&(rev as u16).to_le_bytes());
            if rev != 0 {
                ready += 1;
            }
        }
        let now = crate::timer::now_ns();
        if ready > 0 || now >= end {
            copy_out(fds, &out)?;
            return Ok(ready);
        }
        // Sleep until something becomes ready (kernel/src/poll.rs), re-checking anyway
        // every 20 ms for objects that don't notify.
        crate::poll::wait(g, end);
        if super::signal::interrupted() {
            return Err(EINTR);
        }
    }
}

fn getsockopt(s: SocketId, level: u32, opt: u32, val: usize, len: usize) -> KResult<usize> {
    let v: i32 = match (level, opt) {
        (SOL_SOCKET, SO_ERROR) => socket::take_error(s)?.map_or(0, |e| e.0),
        (SOL_SOCKET, SO_RCVBUF) => socket::get_buf(s, true)? as i32,
        (SOL_SOCKET, SO_SNDBUF) => socket::get_buf(s, false)? as i32,
        (IPPROTO_TCP, TCP_NODELAY) => socket::nodelay(s)? as i32,
        (IPPROTO_IPV6, IPV6_V6ONLY) if socket::family(s)? == Family::V6 => {
            socket::v6only(s)? as i32
        }
        (SOL_SOCKET, SO_TYPE) => match socket::sock_type(s)? {
            SockType::Stream => socket::SOCK_STREAM as i32,
            SockType::Dgram => socket::SOCK_DGRAM as i32,
        },
        _ => return Err(ENOPROTOOPT),
    };
    copy_out(val, &v.to_le_bytes())?;
    if len != 0 {
        copy_out(len, &4u32.to_le_bytes())?;
    }
    Ok(0)
}

/// The link behind an interface index (sin6_scope_id): lo 1, eth0 2, eth1 3; 0 = none.
fn link_of_ifindex(i: u32) -> KResult<Option<usize>> {
    match i {
        0 | 1 => Ok(None),
        2 | 3 => Ok(Some(i as usize - 2)),
        _ => Err(nerr::ENETUNREACH),
    }
}

/// A `struct sockaddr_in` (AF_INET socket) or `struct sockaddr_in6` (AF_INET6 socket) from
/// user memory. An IPv4-mapped IPv6 address comes back as IPv4. A scope id sets the
/// socket's link (for link-local addresses).
fn read_addr(s: SocketId, va: usize, len: usize) -> KResult<SockAddr> {
    if len < 2 {
        return Err(EINVAL);
    }
    let fam = u16::from_le_bytes(copy_in(va, 2)?[..2].try_into().unwrap()) as u32;
    match (socket::family(s)?, fam) {
        (Family::V4, socket::AF_INET) | (Family::V6, socket::AF_INET) => {
            // Lenient: a sockaddr_in on a dual-stack AF_INET6 socket is taken as the
            // v4-mapped address (Linux wants sockaddr_in6 there), unless IPV6_V6ONLY.
            if len < 16 {
                return Err(EINVAL);
            }
            let b = copy_in(va, 16)?;
            if socket::family(s)? == Family::V6 && socket::v6only(s)? {
                return Err(nerr::EAFNOSUPPORT);
            }
            Ok(SockAddr::new(
                Ipv4([b[4], b[5], b[6], b[7]]),
                u16::from_be_bytes([b[2], b[3]]),
            ))
        }
        (Family::V6, socket::AF_INET6) => {
            if len < 24 {
                return Err(EINVAL);
            }
            let b = copy_in(va, len.min(28))?;
            let ip = Ipv6(b[8..24].try_into().unwrap());
            if b.len() >= 28 {
                let id = u32::from_le_bytes(b[24..28].try_into().unwrap());
                let link = link_of_ifindex(id)?;
                if link.is_some() && (ip.is_link_local() || ip.is_link_multicast()) {
                    socket::set_scope(s, link)?;
                }
            }
            Ok(SockAddr::new(ip, u16::from_be_bytes([b[2], b[3]])))
        }
        _ => Err(nerr::EAFNOSUPPORT),
    }
}

/// Write the socket's family of sockaddr to `va`; `lenp` points at the caller's socklen_t
/// (in/out). AF_INET6 sockets see IPv4 peers as ::ffff:a.b.c.d.
fn write_addr(
    s: SocketId,
    a: SockAddr,
    scope: Option<usize>,
    va: usize,
    lenp: usize,
) -> KResult<usize> {
    let mut b = [0u8; 28];
    let n = if socket::family(s)? == Family::V6 {
        b[0..2].copy_from_slice(&(socket::AF_INET6 as u16).to_le_bytes());
        b[2..4].copy_from_slice(&a.port.to_be_bytes());
        let ip = a.ip.to_v6_mapped();
        b[8..24].copy_from_slice(&ip.0);
        if ip.is_link_local() || ip.is_link_multicast() {
            let id = scope.map_or(0, |k| ipv6::ifindex(Some(k)));
            b[24..28].copy_from_slice(&id.to_le_bytes());
        }
        28
    } else {
        b[0..2].copy_from_slice(&(socket::AF_INET as u16).to_le_bytes());
        b[2..4].copy_from_slice(&a.port.to_be_bytes());
        b[4..8].copy_from_slice(&a.ip.v4().unwrap_or_default().0);
        16
    };
    let room = if lenp != 0 {
        let l = copy_in(lenp, 4)?;
        u32::from_le_bytes([l[0], l[1], l[2], l[3]]) as usize
    } else {
        n
    };
    copy_out(va, &b[..room.min(n)])?;
    if lenp != 0 {
        copy_out(lenp, &(n as u32).to_le_bytes())?;
    }
    Ok(0)
}
