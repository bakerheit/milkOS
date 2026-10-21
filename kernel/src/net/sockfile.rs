//! Sockets as files: an [`Inode`] of type `Socket` wrapping a [`SocketId`], so a socket
//! fits the process fd table (`Arc<OpenFile>`) like any file. `read`/`write` on it are
//! `recv`/`send`; dropping the last `OpenFile` closes the socket.
//!
//! Blocking: unless the socket is non-blocking, calls sleep on the stack's readiness wait
//! queue ([`socket::wait`]) and retry. `read(2)`/`write(2)` go through `OpenFile`, which
//! does not pass the open flags down, so they use the non-blocking flag recorded here
//! (set from `SOCK_NONBLOCK` and refreshed from the fd's `O_NONBLOCK` by every socket
//! syscall); the socket syscalls themselves always look at the fd's flags.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::errno::*;
use super::socket::{self, POLLIN, POLLOUT, SocketId};
use crate::fs::errno::Result;
use crate::fs::node::{Inode, InodeRef, InodeType, Stat, alloc_dev};
use crate::fs::{OpenFile, abi, node};

/// Wait "forever" (blocking calls have no timeout, like Linux without SO_RCVTIMEO).
pub const FOREVER: u64 = u64::MAX / 2;

static SOCK_DEV: AtomicU64 = AtomicU64::new(0);
static NEXT_INO: AtomicU64 = AtomicU64::new(1);

pub struct SocketFile {
    pub id: SocketId,
    ino: u64,
    nonblock: AtomicBool,
    /// SO_RCVTIMEO in ns (0 = wait forever).
    rcvtimeo: AtomicU64,
    /// Last sendto destination that was audited (ip << 16 | port, +1; 0 = none), so a UDP
    /// or ping stream is logged once per destination, not per packet.
    pub audited_dst: AtomicU64,
}

impl SocketFile {
    pub fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Relaxed)
    }
    pub fn set_nonblocking(&self, on: bool) {
        self.nonblock.store(on, Ordering::Relaxed);
    }
    /// Blocking receive/accept timeout (`SO_RCVTIMEO`), or FOREVER.
    pub fn rcv_timeout(&self) -> u64 {
        match self.rcvtimeo.load(Ordering::Relaxed) {
            0 => FOREVER,
            t => t,
        }
    }
    pub fn set_rcv_timeout(&self, ns: u64) {
        self.rcvtimeo.store(ns, Ordering::Relaxed);
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = socket::close(self.id);
    }
}

fn dev() -> u64 {
    let d = SOCK_DEV.load(Ordering::Relaxed);
    if d != 0 {
        return d;
    }
    let d = alloc_dev();
    SOCK_DEV.store(d, Ordering::Relaxed);
    d
}

/// Wrap `id` in an `OpenFile` (O_RDWR, plus O_NONBLOCK if asked). The file owns the socket.
pub fn open(id: SocketId, nonblock: bool) -> OpenFile {
    let inode: InodeRef = Arc::new(SocketFile {
        id,
        ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
        nonblock: AtomicBool::new(nonblock),
        rcvtimeo: AtomicU64::new(0),
        audited_dst: AtomicU64::new(0),
    });
    let flags = abi::O_RDWR | if nonblock { abi::O_NONBLOCK } else { 0 };
    OpenFile::new(inode, flags)
}

/// The socket behind an open file, or `ENOTSOCK`. Also syncs the recorded non-blocking flag
/// with the file's `O_NONBLOCK`.
pub fn of(f: &OpenFile) -> Result<&SocketFile> {
    let s = node::downcast::<SocketFile>(f.inode()).ok_or(ENOTSOCK)?;
    s.set_nonblocking(f.flags() & abi::O_NONBLOCK != 0);
    Ok(s)
}

/// recv, blocking unless `nonblock` (for at most `timeout_ns`, then `EAGAIN`, like
/// SO_RCVTIMEO on Linux).
pub fn recv(
    id: SocketId,
    buf: &mut [u8],
    nonblock: bool,
    timeout_ns: u64,
) -> Result<(usize, super::wire::SockAddr)> {
    let end = crate::timer::now_ns().saturating_add(timeout_ns);
    loop {
        match socket::recvfrom(id, buf) {
            Err(e) if e == EAGAIN && !nonblock => {
                let left = end.saturating_sub(crate::timer::now_ns());
                if left == 0 {
                    return Err(EAGAIN);
                }
                socket::wait(id, POLLIN, left);
            }
            r => return r,
        }
    }
}

/// send everything (blocking) or what fits (non-blocking, `EAGAIN` if nothing does).
pub fn send(
    id: SocketId,
    buf: &[u8],
    to: Option<super::wire::SockAddr>,
    nonblock: bool,
) -> Result<usize> {
    let mut off = 0;
    loop {
        let r = match to {
            Some(a) => socket::sendto(id, &buf[off..], a),
            None => socket::send(id, &buf[off..]),
        };
        match r {
            Ok(n) => {
                off += n;
                if off >= buf.len() || nonblock {
                    return Ok(off);
                }
            }
            Err(e) if e == EAGAIN && !nonblock => {
                socket::wait(id, POLLOUT, FOREVER);
            }
            Err(e) if off > 0 => {
                let _ = e;
                return Ok(off);
            }
            Err(e) => return Err(e),
        }
    }
}

impl Inode for SocketFile {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        dev()
    }
    fn kind(&self) -> InodeType {
        InodeType::Socket
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(dev(), self.ino, InodeType::Socket, 0o777);
        st.blksize = socket::BUF_SIZE as u32;
        st
    }
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> Result<usize> {
        recv(self.id, buf, self.nonblocking(), self.rcv_timeout()).map(|(n, _)| n)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> Result<usize> {
        send(self.id, buf, None, self.nonblocking())
    }
    fn truncate(&self, _len: u64) -> Result<()> {
        Err(EINVAL)
    }
    fn poll(&self) -> u32 {
        socket::poll(self.id)
    }
}
