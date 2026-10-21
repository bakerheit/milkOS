//! Syscall glue for AF_UNIX sockets (proc/unix.rs): user memory, the fd table, msghdr and
//! control messages. proc/sock.rs routes here for AF_UNIX fds.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::current;
use super::errno::*;
use super::syscall::{copy_in, copy_out, file};
use super::unix::{self, Addr, Kind, Ucred, UnixSock};
use crate::fs::OpenFile;

pub const SOCKETPAIR: u64 = 199;
pub const SENDMSG: u64 = 211;
pub const RECVMSG: u64 = 212;

const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_NONBLOCK: u32 = 0o4000;
const SOCK_CLOEXEC: u32 = 0o2000000;
const MSG_PEEK: u32 = 0x2;
const MSG_CTRUNC: u32 = 0x8;
const MSG_TRUNC: u32 = 0x20;
const MSG_DONTWAIT: u32 = 0x40;
const MSG_CMSG_CLOEXEC: u32 = 0x4000_0000;
const SOL_SOCKET: u32 = 1;
const SCM_RIGHTS: u32 = 1;
const SCM_CREDENTIALS: u32 = 2;
const SO_TYPE: u32 = 3;
const SO_ERROR: u32 = 4;
const SO_PASSCRED: u32 = 16;
const SO_PEERCRED: u32 = 17;
const SO_RCVTIMEO: u32 = 20;
const SO_ACCEPTCONN: u32 = 30;
const ENOPROTOOPT: Errno = Errno(92);
const EOPNOTSUPP: Errno = Errno(95);
const MAX_IO: usize = 1 << 20;

fn kind_of(ty: u32) -> KResult<Kind> {
    match ty & 0xf {
        SOCK_STREAM => Ok(Kind::Stream),
        SOCK_DGRAM => Ok(Kind::Dgram),
        _ => Err(Errno(94)), // ESOCKTNOSUPPORT
    }
}

fn open_file(s: Arc<UnixSock>, nonblock: bool) -> OpenFile {
    s.set_nonblocking(nonblock);
    let flags = crate::fs::abi::O_RDWR
        | if nonblock {
            crate::fs::abi::O_NONBLOCK
        } else {
            0
        };
    OpenFile::new(s, flags)
}

fn install(of: OpenFile, cloexec: bool) -> KResult<usize> {
    let fd = current().inner.lock().fds.alloc(Arc::new(of), cloexec)?;
    Ok(fd as usize)
}

pub fn socket(ty: u32, proto: u32) -> KResult<usize> {
    if proto != 0 {
        return Err(Errno(93)); // EPROTONOSUPPORT
    }
    let s = unix::new(kind_of(ty)?, ty & SOCK_NONBLOCK != 0);
    install(
        open_file(s, ty & SOCK_NONBLOCK != 0),
        ty & SOCK_CLOEXEC != 0,
    )
}

pub fn socketpair(domain: u32, ty: u32, proto: u32, sv: usize) -> KResult<usize> {
    if domain != unix::AF_UNIX {
        return Err(Errno(97)); // EAFNOSUPPORT: socketpair is AF_UNIX only
    }
    if proto != 0 {
        return Err(Errno(93));
    }
    let nb = ty & SOCK_NONBLOCK != 0;
    let (a, b) = unix::pair(kind_of(ty)?, nb);
    let cloexec = ty & SOCK_CLOEXEC != 0;
    let fa = install(open_file(a, nb), cloexec)?;
    let fb = match install(open_file(b, nb), cloexec) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = current().inner.lock().fds.close(fa as i64);
            return Err(e);
        }
    };
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&(fa as i32).to_le_bytes());
    out[4..8].copy_from_slice(&(fb as i32).to_le_bytes());
    copy_out(sv, &out)?;
    Ok(0)
}

fn read_addr(va: usize, len: usize) -> KResult<Option<Addr>> {
    if !(2..=110).contains(&len) {
        return Err(EINVAL);
    }
    unix::parse_addr(&copy_in(va, len)?)
}

/// Write a sockaddr_un (truncated to the caller's room) and its real length.
fn write_addr(a: Option<&Addr>, va: usize, lenp: usize) -> KResult<()> {
    if va == 0 || lenp == 0 {
        return Ok(());
    }
    let b = unix::encode_addr(a);
    let l = copy_in(lenp, 4)?;
    let room = u32::from_le_bytes([l[0], l[1], l[2], l[3]]) as usize;
    copy_out(va, &b[..room.min(b.len())])?;
    copy_out(lenp, &(b.len() as u32).to_le_bytes())
}

/// One AF_UNIX socket syscall on `f` (an AF_UNIX socket `s`).
pub fn call(nr: u64, a: [u64; 6], f: &Arc<OpenFile>, s: &UnixSock) -> KResult<usize> {
    use super::sock::*;
    let (a1, a2, a3, a4, a5) = (
        a[1] as usize,
        a[2] as usize,
        a[3] as usize,
        a[4] as usize,
        a[5] as usize,
    );
    let nb = f.flags() & crate::fs::abi::O_NONBLOCK != 0;
    match nr {
        BIND => s.bind(read_addr(a1, a2)?).map(|_| 0),
        LISTEN => s.listen(a1).map(|_| 0),
        CONNECT => match read_addr(a1, a2)? {
            Some(addr) => s.connect(&addr).map(|_| 0),
            None => Err(EINVAL),
        },
        ACCEPT | ACCEPT4 => {
            let flags = if nr == ACCEPT4 { a3 as u32 } else { 0 };
            let c = s.accept()?;
            let peer = c.peer_addr().ok().flatten();
            let fd = install(
                open_file(c, flags & SOCK_NONBLOCK != 0),
                flags & SOCK_CLOEXEC != 0,
            )?;
            write_addr(peer.as_ref(), a1, a2)?;
            Ok(fd)
        }
        GETSOCKNAME => write_addr(s.local_addr().as_ref(), a1, a2).map(|_| 0),
        GETPEERNAME => write_addr(s.peer_addr()?.as_ref(), a1, a2).map(|_| 0),
        SENDTO => {
            let data = copy_in(a1, a2.min(MAX_IO))?;
            let to = if a4 != 0 { read_addr(a4, a5)? } else { None };
            let nb = nb || a3 as u32 & MSG_DONTWAIT != 0;
            s.send(&data, Vec::new(), unix::default_cred(), to.as_ref(), nb)
        }
        RECVFROM => {
            let mut buf = alloc::vec![0u8; a2.min(MAX_IO)];
            let fl = a3 as u32;
            let r = s.recv(&mut buf, fl & MSG_PEEK != 0, nb || fl & MSG_DONTWAIT != 0)?;
            copy_out(a1, &buf[..r.n])?;
            write_addr(r.from.as_ref(), a4, a5)?;
            Ok(if fl & MSG_TRUNC != 0 { r.full } else { r.n })
        }
        SHUTDOWN => match a1 {
            0 => s.shutdown(true, false),
            1 => s.shutdown(false, true),
            2 => s.shutdown(true, true),
            _ => Err(EINVAL),
        }
        .map(|_| 0),
        SETSOCKOPT => setsockopt(s, a1 as u32, a2 as u32, a3, a4),
        GETSOCKOPT => getsockopt(s, a1 as u32, a2 as u32, a3, a4),
        SENDMSG => sendmsg(s, a1, a2 as u32, nb),
        RECVMSG => recvmsg(s, a1, a2 as u32, nb),
        _ => Err(EOPNOTSUPP),
    }
}

fn setsockopt(s: &UnixSock, level: u32, opt: u32, val: usize, len: usize) -> KResult<usize> {
    if level != SOL_SOCKET {
        return Err(ENOPROTOOPT);
    }
    match opt {
        SO_PASSCRED => {
            let v = copy_in(val, 4.min(len))?;
            s.set_passcred(v.iter().any(|&b| b != 0));
            Ok(0)
        }
        SO_RCVTIMEO => {
            if len < 16 {
                return Err(EINVAL);
            }
            let b = copy_in(val, 16)?;
            let sec = i64::from_le_bytes(b[0..8].try_into().unwrap());
            let usec = i64::from_le_bytes(b[8..16].try_into().unwrap());
            if sec < 0 || !(0..1_000_000).contains(&usec) {
                return Err(EINVAL);
            }
            s.set_rcv_timeout((sec as u64).saturating_mul(1_000_000_000) + usec as u64 * 1000);
            Ok(0)
        }
        // Accepted and ignored: SO_REUSEADDR, SO_SNDBUF, SO_RCVBUF, SO_SNDTIMEO.
        2 | 7 | 8 | 21 => Ok(0),
        _ => Err(ENOPROTOOPT),
    }
}

fn getsockopt(s: &UnixSock, level: u32, opt: u32, val: usize, lenp: usize) -> KResult<usize> {
    if level != SOL_SOCKET {
        return Err(ENOPROTOOPT);
    }
    let out: Vec<u8> = match opt {
        SO_PEERCRED => {
            // Unconnected: pid 0, uid/gid -1 (like Linux).
            let c = s.peer_cred().unwrap_or(Ucred {
                pid: 0,
                uid: u32::MAX,
                gid: u32::MAX,
            });
            c.bytes().to_vec()
        }
        SO_PASSCRED => (s.passcred() as i32).to_le_bytes().to_vec(),
        SO_TYPE => (match s.kind {
            Kind::Stream => SOCK_STREAM as i32,
            Kind::Dgram => SOCK_DGRAM as i32,
        })
        .to_le_bytes()
        .to_vec(),
        SO_ERROR => 0i32.to_le_bytes().to_vec(),
        SO_ACCEPTCONN => 0i32.to_le_bytes().to_vec(),
        _ => return Err(ENOPROTOOPT),
    };
    let room = if lenp != 0 {
        let l = copy_in(lenp, 4)?;
        u32::from_le_bytes([l[0], l[1], l[2], l[3]]) as usize
    } else {
        out.len()
    };
    let n = room.min(out.len());
    copy_out(val, &out[..n])?;
    if lenp != 0 {
        copy_out(lenp, &(n as u32).to_le_bytes())?;
    }
    Ok(0)
}

// ---- msghdr ------------------------------------------------------------------------------

struct MsgHdr {
    name: usize,
    namelen: u32,
    iov: Vec<(usize, usize)>,
    control: usize,
    controllen: usize,
}

const MSGHDR_SIZE: usize = 56;

fn read_msghdr(va: usize) -> KResult<MsgHdr> {
    let b = copy_in(va, MSGHDR_SIZE)?;
    let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap()) as usize;
    let (iov_va, iovlen) = (u64_at(16), u64_at(24));
    if iovlen > 1024 {
        return Err(EINVAL);
    }
    let raw = copy_in(iov_va, iovlen * 16)?;
    let iov = raw
        .chunks(16)
        .map(|c| {
            (
                u64::from_le_bytes(c[0..8].try_into().unwrap()) as usize,
                u64::from_le_bytes(c[8..16].try_into().unwrap()) as usize,
            )
        })
        .collect();
    Ok(MsgHdr {
        name: u64_at(0),
        namelen: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        iov,
        control: u64_at(32),
        controllen: u64_at(40).min(64 * 1024),
    })
}

fn align8(n: usize) -> usize {
    n.next_multiple_of(8)
}

fn sendmsg(s: &UnixSock, va: usize, flags: u32, nb: bool) -> KResult<usize> {
    let m = read_msghdr(va)?;
    let mut data = Vec::new();
    for &(base, len) in &m.iov {
        if data.len() + len > MAX_IO {
            return Err(EMSGSIZE_);
        }
        data.extend_from_slice(&copy_in(base, len)?);
    }
    let to = if m.name != 0 && m.namelen > 0 {
        read_addr(m.name, m.namelen as usize)?
    } else {
        None
    };
    let mut fds: Vec<Arc<OpenFile>> = Vec::new();
    let mut cred = unix::default_cred();
    if m.control != 0 && m.controllen > 0 {
        let c = copy_in(m.control, m.controllen)?;
        let mut off = 0;
        while off + 16 <= c.len() {
            let clen = u64::from_le_bytes(c[off..off + 8].try_into().unwrap()) as usize;
            let level = u32::from_le_bytes(c[off + 8..off + 12].try_into().unwrap());
            let ty = u32::from_le_bytes(c[off + 12..off + 16].try_into().unwrap());
            if clen < 16 || off + clen > c.len() {
                return Err(EINVAL);
            }
            let body = &c[off + 16..off + clen];
            match (level, ty) {
                (SOL_SOCKET, SCM_RIGHTS) => {
                    for fd in body.as_chunks::<4>().0 {
                        let fd = i32::from_le_bytes([fd[0], fd[1], fd[2], fd[3]]);
                        fds.push(file(fd as i64)?);
                    }
                    if fds.len() > unix::MAX_FDS {
                        return Err(EINVAL);
                    }
                }
                (SOL_SOCKET, SCM_CREDENTIALS) => {
                    if body.len() < 12 {
                        return Err(EINVAL);
                    }
                    let w = |o: usize| u32::from_le_bytes(body[o..o + 4].try_into().unwrap());
                    cred = unix::check_claimed(Ucred {
                        pid: w(0),
                        uid: w(4),
                        gid: w(8),
                    })?;
                }
                _ => return Err(EINVAL),
            }
            off += align8(clen);
        }
    }
    if !fds.is_empty() {
        crate::audit!("unix.scm_rights", "fds={}", fds.len());
    }
    s.send(
        &data,
        fds,
        cred,
        to.as_ref(),
        nb || flags & MSG_DONTWAIT != 0,
    )
}

const EMSGSIZE_: Errno = Errno(90);

fn recvmsg(s: &UnixSock, va: usize, flags: u32, nb: bool) -> KResult<usize> {
    let m = read_msghdr(va)?;
    let total: usize = m.iov.iter().map(|x| x.1).sum::<usize>().min(MAX_IO);
    let mut buf = alloc::vec![0u8; total];
    let r = s.recv(
        &mut buf,
        flags & MSG_PEEK != 0,
        nb || flags & MSG_DONTWAIT != 0,
    )?;
    // Scatter.
    let mut off = 0;
    for &(base, len) in &m.iov {
        if off >= r.n {
            break;
        }
        let k = len.min(r.n - off);
        copy_out(base, &buf[off..off + k])?;
        off += k;
    }
    let mut out_flags = 0u32;
    if r.full > r.n {
        out_flags |= MSG_TRUNC;
    }
    // Control: SCM_RIGHTS (new fds), SCM_CREDENTIALS.
    let mut ctl: Vec<u8> = Vec::new();
    let room = if m.control != 0 { m.controllen } else { 0 };
    if let Some(c) = r.cred {
        let need = 16 + align8(12);
        if ctl.len() + need <= room {
            push_cmsg(&mut ctl, SCM_CREDENTIALS, &c.bytes());
        } else {
            out_flags |= MSG_CTRUNC;
        }
    }
    if !r.fds.is_empty() {
        let fit = room.saturating_sub(ctl.len()).saturating_sub(16) / 4;
        let n = r.fds.len().min(fit);
        if n < r.fds.len() {
            out_flags |= MSG_CTRUNC; // the rest are closed (dropped below)
        }
        if n > 0 {
            let cloexec = flags & MSG_CMSG_CLOEXEC != 0;
            let mut body = Vec::with_capacity(n * 4);
            for f in r.fds.into_iter().take(n) {
                let fd = current().inner.lock().fds.alloc(f, cloexec)?;
                body.extend_from_slice(&(fd as i32).to_le_bytes());
            }
            push_cmsg(&mut ctl, SCM_RIGHTS, &body);
        }
    }
    if !ctl.is_empty() {
        copy_out(m.control, &ctl[..ctl.len().min(room)])?;
    }
    // msg_controllen (40), msg_flags (48), msg_namelen (8) + name.
    copy_out(va + 40, &(ctl.len().min(room) as u64).to_le_bytes())?;
    copy_out(va + 48, &out_flags.to_le_bytes())?;
    if m.name != 0 {
        let b = unix::encode_addr(r.from.as_ref());
        copy_out(m.name, &b[..b.len().min(m.namelen as usize)])?;
        copy_out(va + 8, &(b.len() as u32).to_le_bytes())?;
    } else {
        copy_out(va + 8, &0u32.to_le_bytes())?;
    }
    Ok(if flags & MSG_TRUNC != 0 { r.full } else { r.n })
}

fn push_cmsg(ctl: &mut Vec<u8>, ty: u32, body: &[u8]) {
    let len = 16 + body.len();
    ctl.extend_from_slice(&(len as u64).to_le_bytes());
    ctl.extend_from_slice(&SOL_SOCKET.to_le_bytes());
    ctl.extend_from_slice(&ty.to_le_bytes());
    ctl.extend_from_slice(body);
    while ctl.len() % 8 != 0 {
        ctl.push(0);
    }
}
