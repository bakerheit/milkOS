//! AF_UNIX sockets (kernel/src/proc/unix.rs): stream and datagram sockets named by a path
//! or an abstract name, socketpair, file descriptor passing (SCM_RIGHTS) and peer
//! credentials (SO_PEERCRED, SO_PASSCRED / SCM_CREDENTIALS).

use alloc::vec::Vec;

use crate::sys::{Errno, Result, syscall};

pub const AF_UNIX: usize = 1;
pub const SOCK_STREAM: usize = 1;
pub const SOCK_DGRAM: usize = 2;
pub const SOCK_NONBLOCK: usize = 0o4000;
pub const SOCK_CLOEXEC: usize = 0o2000000;
const SOL_SOCKET: usize = 1;
const SCM_RIGHTS: u32 = 1;
const SCM_CREDENTIALS: u32 = 2;
const SO_PASSCRED: usize = 16;
const SO_PEERCRED: usize = 17;

fn sc(n: usize, a: [usize; 6]) -> Result<usize> {
    // SAFETY: callers pass valid pointers for each call.
    let r = unsafe { syscall(n, a) };
    if r < 0 {
        Err(Errno(-r as i32))
    } else {
        Ok(r as usize)
    }
}

/// A name: a filesystem path, or an abstract name (no file, gone with the socket).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Name<'a> {
    Path(&'a str),
    Abstract(&'a [u8]),
}

fn sockaddr(n: &Name) -> Vec<u8> {
    let mut v = Vec::from((AF_UNIX as u16).to_le_bytes());
    match n {
        Name::Path(p) => {
            v.extend_from_slice(p.as_bytes());
            v.push(0);
        }
        Name::Abstract(a) => {
            v.push(0);
            v.extend_from_slice(a);
        }
    }
    v
}

/// `struct ucred`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Cred {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

pub fn socket(ty: usize) -> Result<i32> {
    sc(198, [AF_UNIX, ty, 0, 0, 0, 0]).map(|fd| fd as i32)
}

/// A connected pair.
pub fn socketpair(ty: usize) -> Result<(i32, i32)> {
    let mut sv = [0i32; 2];
    sc(199, [AF_UNIX, ty, 0, sv.as_mut_ptr() as usize, 0, 0])?;
    Ok((sv[0], sv[1]))
}

pub fn bind(fd: i32, n: &Name) -> Result<()> {
    let a = sockaddr(n);
    sc(200, [fd as usize, a.as_ptr() as usize, a.len(), 0, 0, 0]).map(|_| ())
}

pub fn listen(fd: i32, backlog: usize) -> Result<()> {
    sc(201, [fd as usize, backlog, 0, 0, 0, 0]).map(|_| ())
}

pub fn connect(fd: i32, n: &Name) -> Result<()> {
    let a = sockaddr(n);
    sc(203, [fd as usize, a.as_ptr() as usize, a.len(), 0, 0, 0]).map(|_| ())
}

/// accept4 (flags SOCK_NONBLOCK | SOCK_CLOEXEC).
pub fn accept(fd: i32, flags: usize) -> Result<i32> {
    sc(242, [fd as usize, 0, 0, flags, 0, 0]).map(|f| f as i32)
}

/// Send a datagram to `n` (SOCK_DGRAM).
pub fn sendto(fd: i32, buf: &[u8], n: &Name) -> Result<usize> {
    let a = sockaddr(n);
    sc(
        206,
        [
            fd as usize,
            buf.as_ptr() as usize,
            buf.len(),
            0,
            a.as_ptr() as usize,
            a.len(),
        ],
    )
}

/// The peer's credentials as of connect / listen / socketpair.
pub fn peer_cred(fd: i32) -> Result<Cred> {
    let mut b = [0u8; 12];
    let mut len = 12u32;
    sc(
        209,
        [
            fd as usize,
            SOL_SOCKET,
            SO_PEERCRED,
            b.as_mut_ptr() as usize,
            &mut len as *mut u32 as usize,
            0,
        ],
    )?;
    let w = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    Ok(Cred {
        pid: w(0),
        uid: w(4),
        gid: w(8),
    })
}

/// Ask for an SCM_CREDENTIALS record with every message received.
pub fn set_passcred(fd: i32, on: bool) -> Result<()> {
    let v: i32 = on as i32;
    sc(
        208,
        [
            fd as usize,
            SOL_SOCKET,
            SO_PASSCRED,
            &v as *const i32 as usize,
            4,
            0,
        ],
    )
    .map(|_| ())
}

fn cmsg(ty: u32, body: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&((16 + body.len()) as u64).to_le_bytes());
    out.extend_from_slice(&(SOL_SOCKET as u32).to_le_bytes());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(body);
    while out.len() % 8 != 0 {
        out.push(0);
    }
}

/// sendmsg with files (SCM_RIGHTS) and optionally claimed credentials (SCM_CREDENTIALS;
/// the kernel refuses anything but your own).
pub fn send_with(fd: i32, data: &[u8], fds: &[i32], cred: Option<Cred>) -> Result<usize> {
    let iov = [data.as_ptr() as usize, data.len()];
    let mut ctl = Vec::new();
    if !fds.is_empty() {
        let body: Vec<u8> = fds.iter().flat_map(|f| f.to_le_bytes()).collect();
        cmsg(SCM_RIGHTS, &body, &mut ctl);
    }
    if let Some(c) = cred {
        let mut body = Vec::new();
        for v in [c.pid, c.uid, c.gid] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        cmsg(SCM_CREDENTIALS, &body, &mut ctl);
    }
    let mut hdr = [0u64; 7];
    hdr[2] = iov.as_ptr() as u64;
    hdr[3] = 1;
    if !ctl.is_empty() {
        hdr[4] = ctl.as_ptr() as u64;
        hdr[5] = ctl.len() as u64;
    }
    sc(211, [fd as usize, hdr.as_ptr() as usize, 0, 0, 0, 0])
}

/// What `recv_with` got.
#[derive(Default, Debug)]
pub struct Received {
    pub n: usize,
    pub fds: Vec<i32>,
    pub cred: Option<Cred>,
    /// The kernel had to drop control data (too many fds for `max_fds`).
    pub ctrunc: bool,
}

/// recvmsg with room for `max_fds` files (they arrive close-on-exec) and a credentials
/// record (if SO_PASSCRED is on).
pub fn recv_with(fd: i32, buf: &mut [u8], max_fds: usize) -> Result<Received> {
    let iov = [buf.as_mut_ptr() as usize, buf.len()];
    let mut ctl = alloc::vec![0u8; 16 + max_fds * 4 + 8 + 32];
    let mut hdr = [0u64; 7];
    hdr[2] = iov.as_ptr() as u64;
    hdr[3] = 1;
    hdr[4] = ctl.as_mut_ptr() as u64;
    hdr[5] = ctl.len() as u64;
    const MSG_CMSG_CLOEXEC: usize = 0x4000_0000;
    let n = sc(
        212,
        [
            fd as usize,
            hdr.as_mut_ptr() as usize,
            MSG_CMSG_CLOEXEC,
            0,
            0,
            0,
        ],
    )?;
    let clen = hdr[5] as usize;
    let mut r = Received {
        n,
        ctrunc: hdr[6] as u32 & 0x8 != 0,
        ..Default::default()
    };
    let mut off = 0;
    while off + 16 <= clen {
        let l = u64::from_le_bytes(ctl[off..off + 8].try_into().unwrap()) as usize;
        let ty = u32::from_le_bytes(ctl[off + 12..off + 16].try_into().unwrap());
        if l < 16 || off + l > clen {
            break;
        }
        let body = &ctl[off + 16..off + l];
        match ty {
            SCM_RIGHTS => {
                for c in body.as_chunks::<4>().0 {
                    r.fds.push(i32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
            SCM_CREDENTIALS if body.len() >= 12 => {
                let w = |o: usize| u32::from_le_bytes(body[o..o + 4].try_into().unwrap());
                r.cred = Some(Cred {
                    pid: w(0),
                    uid: w(4),
                    gid: w(8),
                });
            }
            _ => {}
        }
        off += l.next_multiple_of(8);
    }
    Ok(r)
}
