//! Socket syscalls (Linux aarch64 numbers) on top of the kernel socket API in `net::socket`.
//! Written by the net side; kept here because it needs the fd table and user copies.
//!
//! AF_INET only: SOCK_STREAM (TCP), SOCK_DGRAM (UDP, or ICMP echo with IPPROTO_ICMP, the
//! Linux "ping socket"), `struct sockaddr_in` (16 bytes). Also ppoll(2), for any fd. Blocking
//! unless the fd is O_NONBLOCK (SOCK_NONBLOCK / fcntl) or the call passes MSG_DONTWAIT.
//! No signals exist yet, so a blocking call waits until the socket is ready.

use alloc::sync::Arc;

use super::current;
use super::errno::*;
use super::syscall::{copy_in, copy_out, file};
use crate::fs::OpenFile;
use crate::net::errno as nerr;
use crate::net::socket::{self, POLLOUT, Shutdown, SockType, SocketId};
use crate::net::sockfile::{self, FOREVER};
use crate::net::wire::{Ipv4, SockAddr};

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
const SO_SNDTIMEO: u32 = 21;
const ENOPROTOOPT: Errno = Errno(92);
const MAX_IO: usize = 1 << 20;

/// Is `nr` one of ours?
pub fn handles(nr: u64) -> bool {
    matches!(nr, SOCKET | BIND..=SHUTDOWN | ACCEPT4 | PPOLL)
}

/// Is `addr` reachable with only the Local network capability (loopback / link-local)?
fn is_local_addr(addr: &SockAddr) -> bool {
    addr.ip.is_loopback() || addr.ip.0[0] == 169 && addr.ip.0[1] == 254 || addr.ip.is_unspecified()
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
    match nr {
        SOCKET => {
            if super::current_caps().net == super::caps::Net::None {
                return Err(super::caps::deny("socket", "net capability dropped"));
            }
            sys_socket(a[0] as u32, a[1] as u32, a[2] as u32)
        }
        PPOLL => sys_ppoll(a[0] as usize, a1, a2),
        BIND => with_sock(fd, |s, _| socket::bind(s, read_addr(a1, a2)?).map(|_| 0)),
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
            let addr = read_addr(a1, a2)?;
            net_allows(&addr, "connect")?;
            let r = connect(s, addr, nb);
            if !matches!(r, Err(e) if e == nerr::EALREADY || e == nerr::EISCONN) {
                net_event("net.connect", s, "dst", addr, &r);
            }
            r
        }),
        GETSOCKNAME => with_sock(fd, |s, _| write_addr(socket::local_addr(s)?, a1, a2)),
        GETPEERNAME => with_sock(fd, |s, _| write_addr(socket::peer_addr(s)?, a1, a2)),
        SENDTO => with_sock(fd, |s, nb| {
            let data = copy_in(a1, a2.min(MAX_IO))?;
            let to = if a4 != 0 {
                Some(read_addr(a4, a5)?)
            } else {
                None
            };
            if let Some(to) = &to {
                net_allows(to, "sendto")?;
            }
            if let Some(to) = to {
                let key = ((to.ip.to_u32() as u64) << 16 | to.port as u64) + 1;
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
            let (n, from) = sockfile::recv(s, &mut buf, nb, timeout_of(fd))?;
            copy_out(a1, &buf[..n])?;
            if a4 != 0 {
                write_addr(from, a4, a5)?;
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

fn install(of: OpenFile, cloexec: bool) -> KResult<usize> {
    let fd = current().inner.lock().fds.alloc(Arc::new(of), cloexec)?;
    Ok(fd as usize)
}

fn sys_socket(domain: u32, ty: u32, proto: u32) -> KResult<usize> {
    if domain != socket::AF_INET {
        return Err(nerr::EAFNOSUPPORT);
    }
    let kind = SockType::from_linux(ty).ok_or(nerr::EPROTONOSUPPORT)?;
    let id = match (kind, proto) {
        (SockType::Stream, 0 | 6) | (SockType::Dgram, 0 | 17) => socket::socket(kind)?,
        // Linux "ping socket": unprivileged ICMP echo (see net::socket::socket_icmp).
        (SockType::Dgram, 1) => socket::socket_icmp()?,
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
        write_addr(peer, addr, addrlen)?;
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
        // Accepted and ignored: SO_REUSEADDR (we always allow it), SO_SNDBUF, SO_RCVBUF,
        // SO_KEEPALIVE, SO_REUSEPORT, SO_SNDTIMEO (sends only wait for buffer space),
        // TCP_NODELAY (we never delay).
        (SOL_SOCKET, 2 | 7 | 8 | 9 | 15 | SO_SNDTIMEO) | (IPPROTO_TCP, 1) => Ok(0),
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
        // Sleep until a socket changes, re-checking other fds (the console) every 20 ms.
        let g = crate::net::ready_generation();
        crate::net::READY_WQ.wait_until_timeout(
            || crate::net::ready_generation() != g,
            (end - now).min(20_000_000),
        );
    }
}

fn getsockopt(s: SocketId, level: u32, opt: u32, val: usize, len: usize) -> KResult<usize> {
    let v: i32 = match (level, opt) {
        (SOL_SOCKET, SO_ERROR) => socket::take_error(s)?.map_or(0, |e| e.0),
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

/// `struct sockaddr_in` from user memory.
fn read_addr(va: usize, len: usize) -> KResult<SockAddr> {
    if len < 16 {
        return Err(EINVAL);
    }
    let b = copy_in(va, 16)?;
    if u16::from_le_bytes([b[0], b[1]]) as u32 != socket::AF_INET {
        return Err(nerr::EAFNOSUPPORT);
    }
    Ok(SockAddr::new(
        Ipv4([b[4], b[5], b[6], b[7]]),
        u16::from_be_bytes([b[2], b[3]]),
    ))
}

/// Write a `struct sockaddr_in` to `va`; `lenp` points at the caller's socklen_t (in/out).
fn write_addr(a: SockAddr, va: usize, lenp: usize) -> KResult<usize> {
    let mut b = [0u8; 16];
    b[0..2].copy_from_slice(&(socket::AF_INET as u16).to_le_bytes());
    b[2..4].copy_from_slice(&a.port.to_be_bytes());
    b[4..8].copy_from_slice(&a.ip.0);
    let room = if lenp != 0 {
        let l = copy_in(lenp, 4)?;
        u32::from_le_bytes([l[0], l[1], l[2], l[3]]) as usize
    } else {
        16
    };
    copy_out(va, &b[..room.min(16)])?;
    if lenp != 0 {
        copy_out(lenp, &16u32.to_le_bytes())?;
    }
    Ok(0)
}
