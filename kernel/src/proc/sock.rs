//! Socket syscalls (Linux aarch64 numbers) on top of the kernel socket API in `net::socket`.
//! Written by the net side; kept here because it needs the fd table and user copies.
//!
//! AF_INET only, SOCK_STREAM / SOCK_DGRAM, `struct sockaddr_in` (16 bytes). Blocking
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

const SOCK_NONBLOCK: u32 = 0o4000;
const SOCK_CLOEXEC: u32 = 0o2000000;
const MSG_DONTWAIT: u32 = 0x40;
const SOL_SOCKET: u32 = 1;
const IPPROTO_TCP: u32 = 6;
const SO_TYPE: u32 = 3;
const SO_ERROR: u32 = 4;
const ENOPROTOOPT: Errno = Errno(92);
const MAX_IO: usize = 1 << 20;

/// Is `nr` one of ours?
pub fn handles(nr: u64) -> bool {
    matches!(nr, SOCKET | BIND..=SHUTDOWN | ACCEPT4)
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
        SOCKET => sys_socket(a[0] as u32, a[1] as u32, a[2] as u32),
        BIND => with_sock(fd, |s, _| socket::bind(s, read_addr(a1, a2)?).map(|_| 0)),
        LISTEN => with_sock(fd, |s, _| socket::listen(s, a1).map(|_| 0)),
        ACCEPT => sys_accept(fd, a1, a2, 0),
        ACCEPT4 => sys_accept(fd, a1, a2, a3 as u32),
        CONNECT => with_sock(fd, |s, nb| connect(s, read_addr(a1, a2)?, nb)),
        GETSOCKNAME => with_sock(fd, |s, _| write_addr(socket::local_addr(s)?, a1, a2)),
        GETPEERNAME => with_sock(fd, |s, _| write_addr(socket::peer_addr(s)?, a1, a2)),
        SENDTO => with_sock(fd, |s, nb| {
            let data = copy_in(a1, a2.min(MAX_IO))?;
            let to = if a4 != 0 {
                Some(read_addr(a4, a5)?)
            } else {
                None
            };
            sockfile::send(s, &data, to, nb || a3 as u32 & MSG_DONTWAIT != 0)
        }),
        RECVFROM => with_sock(fd, |s, nb| {
            let mut buf = alloc::vec![0u8; a2.min(MAX_IO)];
            let (n, from) = sockfile::recv(s, &mut buf, nb || a3 as u32 & MSG_DONTWAIT != 0)?;
            copy_out(a1, &buf[..n])?;
            if a4 != 0 {
                write_addr(from, a4, a5)?;
            }
            Ok(n)
        }),
        SETSOCKOPT => with_sock(fd, |_, _| setsockopt(a1 as u32, a2 as u32)),
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

fn install(of: OpenFile, cloexec: bool) -> KResult<usize> {
    let fd = current().inner.lock().fds.alloc(Arc::new(of), cloexec)?;
    Ok(fd as usize)
}

fn sys_socket(domain: u32, ty: u32, proto: u32) -> KResult<usize> {
    if domain != socket::AF_INET {
        return Err(nerr::EAFNOSUPPORT);
    }
    let kind = SockType::from_linux(ty).ok_or(nerr::EPROTONOSUPPORT)?;
    let proto_ok = match kind {
        SockType::Stream => proto == 0 || proto == 6,
        SockType::Dgram => proto == 0 || proto == 17,
    };
    if !proto_ok {
        return Err(nerr::EPROTONOSUPPORT);
    }
    let id = socket::socket(kind)?;
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
    let (child, peer) = loop {
        match socket::accept(id) {
            Err(e) if e == EAGAIN && !nb => {
                socket::wait(id, socket::POLLIN, FOREVER);
            }
            r => break r?,
        }
    };
    let of = sockfile::open(child, flags & SOCK_NONBLOCK != 0);
    let newfd = install(of, flags & SOCK_CLOEXEC != 0)?;
    if addr != 0 {
        write_addr(peer, addr, addrlen)?;
    }
    Ok(newfd)
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

fn setsockopt(level: u32, opt: u32) -> KResult<usize> {
    // Accepted and ignored: SO_REUSEADDR (we always allow it), SO_KEEPALIVE, SO_SNDBUF,
    // SO_RCVBUF, SO_REUSEPORT, TCP_NODELAY (we never delay).
    match (level, opt) {
        (SOL_SOCKET, 2 | 7 | 8 | 9 | 15) | (IPPROTO_TCP, 1) => Ok(0),
        _ => Err(ENOPROTOOPT),
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
