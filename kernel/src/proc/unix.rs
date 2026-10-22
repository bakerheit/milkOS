//! AF_UNIX sockets: local IPC with file descriptor passing and peer credentials.
//!
//! * `SOCK_STREAM` (connection, byte stream) and `SOCK_DGRAM` (messages, connected or
//!   addressed per send). `socketpair(2)` makes a connected pair of either.
//! * Names: a **path** (a socket node made with `bind`, `S_IFSOCK`; connecting needs write
//!   permission on it, like Linux, and the capability sandbox's fs grant), or an
//!   **abstract** name (`sun_path[0] == 0`, a kernel-only namespace that vanishes with the
//!   socket; the sandbox treats it like the network: denied under `net=none`). Unlinking
//!   the path doesn't close the socket, it just can't be found any more. Socket nodes can
//!   only be made on tmpfs (`/tmp`, `/System/Services/Sockets`).
//! * **SCM_RIGHTS**: `sendmsg` can carry open files; the receiver gets new fds for the
//!   same open file descriptions (shared offset and flags), like dup across processes. On
//!   a stream they arrive with the first byte of their write; a read never runs past the
//!   next write that carries files, so they always come with "their" bytes.
//! * **Credentials**: `SO_PEERCRED` gives the peer's pid / euid / egid as they were at
//!   `connect` / `listen` / `socketpair`. With `SO_PASSCRED` on the receiver, every message
//!   comes with an `SCM_CREDENTIALS` record: the sender's pid and real ids, or what it sent
//!   itself, which the kernel only allows if they are really its own (root may claim any).
//!   Services authenticate callers with these.
//! * Readiness: `Inode::poll` / `poll_seq` for poll, select and epoll (kernel/src/poll.rs).
//!
//! Each socket has a 256 KiB receive buffer; a writer to a full peer blocks (or EAGAIN).
//! Files in flight in a buffer whose socket is closed are dropped (closed). A socket sent
//! over itself (a reference cycle) is never freed: Linux has a garbage collector for
//! that, milkOS doesn't.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::errno::*;
use crate::fs::node::{Inode, InodeRef, InodeType, Stat, alloc_dev, downcast};
use crate::fs::{self, OpenFile};
use crate::poll::{self, *};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

pub const AF_UNIX: u32 = 1;
pub const RCVBUF: usize = 256 * 1024;
/// Most files one message can carry (Linux SCM_MAX_FD).
pub const MAX_FDS: usize = 253;

const ECONNREFUSED: Errno = Errno(111);
const EADDRINUSE: Errno = Errno(98);
const ENOTCONN: Errno = Errno(107);
const EISCONN: Errno = Errno(106);
const EOPNOTSUPP: Errno = Errno(95);
const EDESTADDRREQ: Errno = Errno(89);
const ECONNRESET: Errno = Errno(104);
const EMSGSIZE: Errno = Errno(90);
const EPIPE: Errno = Errno(32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Stream,
    Dgram,
}

/// `struct ucred`: pid, uid, gid.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Ucred {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Ucred {
    pub fn bytes(&self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0..4].copy_from_slice(&self.pid.to_le_bytes());
        b[4..8].copy_from_slice(&self.uid.to_le_bytes());
        b[8..12].copy_from_slice(&self.gid.to_le_bytes());
        b
    }
}

/// The caller's credentials for SO_PEERCRED (effective ids) / SCM_CREDENTIALS (real ids).
fn own_cred(effective: bool) -> Ucred {
    let c = super::current_cred();
    let pid = crate::sched::current_pid();
    if effective {
        Ucred {
            pid,
            uid: c.euid,
            gid: c.egid,
        }
    } else {
        Ucred {
            pid,
            uid: c.ruid,
            gid: c.rgid,
        }
    }
}

/// A socket's name: what `sun_path` held (a path as given, or `\0` + abstract bytes).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Addr {
    Path(String),
    Abstract(Vec<u8>),
}

/// One write (stream) or datagram, with what travels with it.
struct Msg {
    data: Vec<u8>,
    off: usize,
    fds: Vec<Arc<OpenFile>>,
    cred: Ucred,
    from: Option<Addr>,
}

enum Conn {
    /// Fresh, or bound (stream not listening; dgram).
    Idle,
    Listening {
        backlog: usize,
        pending: VecDeque<Arc<UnixSock>>,
        /// The creds `connect`ers see as their peer's.
        cred: Ucred,
    },
    Connected(Weak<UnixSock>),
}

struct Inner {
    addr: Option<Addr>,
    conn: Conn,
    /// Was ever connected (to tell EPIPE from ENOTCONN once the peer is gone).
    was_connected: bool,
    rx: VecDeque<Msg>,
    rx_bytes: usize,
    shut_rd: bool,
    shut_wr: bool,
    /// The peer shut its write side (or closed): EOF once rx is drained.
    peer_shut_wr: bool,
    peer_cred: Option<Ucred>,
    passcred: bool,
    /// Datagram default destination (`connect` on a SOCK_DGRAM socket).
    dgram_peer: Option<Weak<UnixSock>>,
    rcv_timeout_ns: u64,
}

pub struct UnixSock {
    pub kind: Kind,
    me: Weak<UnixSock>,
    inner: SpinLock<Inner>,
    /// Blocked readers, writers, acceptors and connecters of this socket wait here.
    wq: WaitQueue,
    seq: AtomicU64,
    nonblock: AtomicBool,
    ino: u64,
}

static DEV: AtomicU64 = AtomicU64::new(0);
static NEXT_INO: AtomicU64 = AtomicU64::new(1);

fn dev() -> u64 {
    let d = DEV.load(Ordering::Relaxed);
    if d != 0 {
        return d;
    }
    let n = alloc_dev();
    match DEV.compare_exchange(0, n, Ordering::Relaxed, Ordering::Relaxed) {
        Ok(_) => n,
        Err(cur) => cur,
    }
}

/// Bound path sockets by their socket node, and abstract names.
/// (dev, ino) of a socket node -> the socket bound there.
type PathTable = Vec<((u64, u64), Weak<UnixSock>)>;
static PATHS: SpinLock<PathTable> = SpinLock::new(Vec::new());
static ABSTRACT: SpinLock<BTreeMap<Vec<u8>, Weak<UnixSock>>> = SpinLock::new(BTreeMap::new());

pub fn new(kind: Kind, nonblock: bool) -> Arc<UnixSock> {
    Arc::new_cyclic(|me| UnixSock {
        kind,
        me: me.clone(),
        inner: SpinLock::new(Inner {
            addr: None,
            conn: Conn::Idle,
            was_connected: false,
            rx: VecDeque::new(),
            rx_bytes: 0,
            shut_rd: false,
            shut_wr: false,
            peer_shut_wr: false,
            peer_cred: None,
            passcred: false,
            dgram_peer: None,
            rcv_timeout_ns: u64::MAX,
        }),
        wq: WaitQueue::new(),
        seq: AtomicU64::new(0),
        nonblock: AtomicBool::new(nonblock),
        ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
    })
}

/// The AF_UNIX socket behind an open file (syncing its non-blocking flag from O_NONBLOCK).
pub fn of(f: &OpenFile) -> Option<&UnixSock> {
    let s = downcast::<UnixSock>(f.inode())?;
    s.nonblock
        .store(f.flags() & fs::abi::O_NONBLOCK != 0, Ordering::Relaxed);
    Some(s)
}

/// A connected pair (socketpair(2)): both sides see the caller's credentials.
pub fn pair(kind: Kind, nonblock: bool) -> (Arc<UnixSock>, Arc<UnixSock>) {
    let (a, b) = (new(kind, nonblock), new(kind, nonblock));
    let cred = own_cred(true);
    for (x, y) in [(&a, &b), (&b, &a)] {
        let mut i = x.inner.lock();
        i.peer_cred = Some(cred);
        i.was_connected = true;
        match kind {
            Kind::Stream => i.conn = Conn::Connected(Arc::downgrade(y)),
            Kind::Dgram => i.dgram_peer = Some(Arc::downgrade(y)),
        }
    }
    (a, b)
}

impl UnixSock {
    fn arc(&self) -> Arc<UnixSock> {
        self.me.upgrade().expect("live socket")
    }

    /// Something arrived for this socket (data, a connection, hangup): bump its event
    /// counter, wake waiters here and in poll/epoll.
    fn changed(&self) {
        self.seq.fetch_add(1, Ordering::Release);
        self.wq.wake_all();
        poll::notify();
    }

    /// We consumed from our receive buffer (or backlog): writers / connecters blocked on
    /// us may go on, and our stream peer becomes writable again (its edge, not ours).
    fn consumed(&self) {
        self.wq.wake_all();
        if let Some(p) = self.peer() {
            p.seq.fetch_add(1, Ordering::Release);
        }
        poll::notify();
    }

    fn nonblocking(&self) -> bool {
        self.nonblock.load(Ordering::Relaxed)
    }

    /// read(2) / write(2) on the socket honour this (the open file's O_NONBLOCK, synced
    /// at creation, accept4, fcntl(F_SETFL) and every socket call).
    pub fn set_nonblocking(&self, on: bool) {
        self.nonblock.store(on, Ordering::Relaxed);
    }

    fn peer(&self) -> Option<Arc<UnixSock>> {
        match &self.inner.lock().conn {
            Conn::Connected(w) => w.upgrade(),
            _ => None,
        }
    }

    pub fn local_addr(&self) -> Option<Addr> {
        self.inner.lock().addr.clone()
    }

    pub fn peer_addr(&self) -> KResult<Option<Addr>> {
        let p = match self.kind {
            Kind::Stream => self.peer(),
            Kind::Dgram => self
                .inner
                .lock()
                .dgram_peer
                .as_ref()
                .and_then(Weak::upgrade),
        };
        match p {
            Some(p) => Ok(p.local_addr()),
            None => Err(ENOTCONN),
        }
    }

    pub fn peer_cred(&self) -> Option<Ucred> {
        self.inner.lock().peer_cred
    }

    pub fn set_passcred(&self, on: bool) {
        self.inner.lock().passcred = on;
    }

    pub fn passcred(&self) -> bool {
        self.inner.lock().passcred
    }

    pub fn set_rcv_timeout(&self, ns: u64) {
        self.inner.lock().rcv_timeout_ns = if ns == 0 { u64::MAX } else { ns };
    }

    // ---- names -----------------------------------------------------------------------

    pub fn bind(&self, addr: Option<Addr>) -> KResult<()> {
        if self.inner.lock().addr.is_some() {
            return Err(EINVAL);
        }
        let addr = match addr {
            Some(a) => a,
            None => autobind_name(),
        };
        match &addr {
            Addr::Abstract(n) => {
                let mut t = ABSTRACT.lock();
                if t.get(n).is_some_and(|w| w.strong_count() > 0) {
                    return Err(EADDRINUSE);
                }
                t.insert(n.clone(), self.me.clone());
            }
            Addr::Path(p) => {
                let node =
                    match fs::mknod_at(Some(&super::syscall::cwd()), p, InodeType::Socket, 0o755) {
                        Ok(n) => n,
                        Err(fs::Errno::EEXIST) => return Err(EADDRINUSE),
                        Err(e) => return Err(e),
                    };
                let mut t = PATHS.lock();
                t.retain(|(_, w)| w.strong_count() > 0);
                t.push(((node.dev(), node.ino()), self.me.clone()));
            }
        }
        self.inner.lock().addr = Some(addr);
        Ok(())
    }

    pub fn listen(&self, backlog: usize) -> KResult<()> {
        if self.kind != Kind::Stream {
            return Err(EOPNOTSUPP);
        }
        let mut i = self.inner.lock();
        if i.addr.is_none() {
            drop(i);
            self.bind(None)?;
            i = self.inner.lock();
        }
        match &mut i.conn {
            Conn::Idle => {
                i.conn = Conn::Listening {
                    backlog: backlog.clamp(1, 4096),
                    pending: VecDeque::new(),
                    cred: own_cred(true),
                }
            }
            Conn::Listening { backlog: b, .. } => *b = backlog.clamp(1, 4096),
            Conn::Connected(_) => return Err(EINVAL),
        }
        Ok(())
    }

    // ---- connection ------------------------------------------------------------------

    pub fn connect(&self, addr: &Addr) -> KResult<()> {
        let target = resolve(addr)?;
        if target.kind != self.kind {
            return Err(ECONNREFUSED); // Linux: EPROTOTYPE
        }
        if self.kind == Kind::Dgram {
            self.inner.lock().dgram_peer = Some(Arc::downgrade(&target));
            return Ok(());
        }
        if matches!(self.inner.lock().conn, Conn::Connected(_)) {
            return Err(EISCONN);
        }
        let me_cred = own_cred(true);
        loop {
            // The server's end of the connection, waiting in the listener's queue.
            let mut li = target.inner.lock();
            let laddr = li.addr.clone();
            let Conn::Listening {
                backlog,
                pending,
                cred,
            } = &mut li.conn
            else {
                return Err(ECONNREFUSED);
            };
            if pending.len() < *backlog {
                let srv = new(Kind::Stream, false);
                {
                    let mut si = srv.inner.lock();
                    si.conn = Conn::Connected(self.me.clone());
                    si.peer_cred = Some(me_cred);
                    si.was_connected = true;
                }
                let listener_cred = *cred;
                srv.inner.lock().addr = laddr;
                pending.push_back(srv.clone());
                drop(li);
                {
                    let mut mi = self.inner.lock();
                    mi.conn = Conn::Connected(Arc::downgrade(&srv));
                    mi.peer_cred = Some(listener_cred);
                    mi.was_connected = true;
                }
                target.changed();
                return Ok(());
            }
            drop(li);
            if self.nonblocking() {
                return Err(EAGAIN);
            }
            if super::signal::interrupted() {
                return Err(EINTR);
            }
            target.wq.wait_until(|| {
                super::signal::interrupted()
                    || match &target.inner.lock().conn {
                        Conn::Listening {
                            backlog, pending, ..
                        } => pending.len() < *backlog,
                        _ => true,
                    }
            });
        }
    }

    pub fn accept(&self) -> KResult<Arc<UnixSock>> {
        let deadline = self.deadline();
        loop {
            {
                let mut i = self.inner.lock();
                let Conn::Listening { pending, .. } = &mut i.conn else {
                    return Err(EINVAL);
                };
                if let Some(s) = pending.pop_front() {
                    drop(i);
                    self.consumed(); // room in the backlog: wake connecters
                    return Ok(s);
                }
            }
            self.block(deadline, || match &self.inner.lock().conn {
                Conn::Listening { pending, .. } => !pending.is_empty(),
                _ => true,
            })?;
        }
    }

    pub fn shutdown(&self, rd: bool, wr: bool) -> KResult<()> {
        let peer = self.peer();
        if self.kind == Kind::Stream && peer.is_none() && !self.inner.lock().was_connected {
            return Err(ENOTCONN);
        }
        {
            let mut i = self.inner.lock();
            i.shut_rd |= rd;
            i.shut_wr |= wr;
        }
        if let Some(p) = peer {
            if wr {
                p.inner.lock().peer_shut_wr = true;
            }
            p.changed();
        }
        self.changed();
        Ok(())
    }

    // ---- data ------------------------------------------------------------------------

    fn deadline(&self) -> u64 {
        let t = self.inner.lock().rcv_timeout_ns;
        crate::timer::now_ns().saturating_add(t)
    }

    /// Wait on this socket until `ready()` or a signal, the deadline or (non-blocking)
    /// right away: EAGAIN / EINTR.
    fn block(&self, deadline: u64, ready: impl Fn() -> bool) -> KResult<()> {
        if self.nonblocking() {
            return Err(EAGAIN);
        }
        if super::signal::interrupted() {
            return Err(EINTR);
        }
        let now = crate::timer::now_ns();
        if now >= deadline {
            return Err(EAGAIN);
        }
        self.wq
            .wait_until_timeout(|| ready() || super::signal::interrupted(), deadline - now);
        Ok(())
    }

    /// Send `data` with `fds` and `cred`. Stream: to the connected peer, possibly in
    /// several pieces while the peer's buffer fills (the files go with the first). Dgram:
    /// one message to `to` or the connected peer.
    pub fn send(
        &self,
        data: &[u8],
        mut fds: Vec<Arc<OpenFile>>,
        cred: Ucred,
        to: Option<&Addr>,
        nonblock: bool,
    ) -> KResult<usize> {
        let nb = nonblock || self.nonblocking();
        match self.kind {
            Kind::Dgram => {
                let target = match to {
                    Some(a) => resolve(a)?,
                    None => self
                        .inner
                        .lock()
                        .dgram_peer
                        .as_ref()
                        .and_then(Weak::upgrade)
                        .ok_or(EDESTADDRREQ)?,
                };
                if target.kind != Kind::Dgram {
                    return Err(ECONNREFUSED);
                }
                if data.len() > RCVBUF {
                    return Err(EMSGSIZE);
                }
                let from = self.local_addr();
                loop {
                    {
                        let mut ti = target.inner.lock();
                        if ti.shut_rd {
                            return Err(ECONNREFUSED);
                        }
                        if ti.rx_bytes + data.len() <= RCVBUF || ti.rx.is_empty() {
                            ti.rx_bytes += data.len();
                            ti.rx.push_back(Msg {
                                data: data.to_vec(),
                                off: 0,
                                fds: core::mem::take(&mut fds),
                                cred,
                                from: from.clone(),
                            });
                            drop(ti);
                            target.changed();
                            return Ok(data.len());
                        }
                    }
                    if nb {
                        return Err(EAGAIN);
                    }
                    if super::signal::interrupted() {
                        return Err(EINTR);
                    }
                    target.wq.wait_until(|| {
                        let ti = target.inner.lock();
                        ti.rx_bytes + data.len() <= RCVBUF
                            || ti.shut_rd
                            || super::signal::interrupted()
                    });
                }
            }
            Kind::Stream => {
                if to.is_some() && self.peer().is_none() {
                    return Err(ENOTCONN); // sendto on a stream: the address is ignored
                }
                if data.is_empty() {
                    // Files need at least one byte to travel with on a stream.
                    return if fds.is_empty() { Ok(0) } else { Err(EINVAL) };
                }
                let mut done = 0;
                loop {
                    let Some(peer) = self.peer() else {
                        if self.inner.lock().was_connected {
                            super::signal::send_current(super::signal::SIGPIPE);
                            return if done > 0 { Ok(done) } else { Err(EPIPE) };
                        }
                        return Err(ENOTCONN);
                    };
                    if self.inner.lock().shut_wr {
                        super::signal::send_current(super::signal::SIGPIPE);
                        return if done > 0 { Ok(done) } else { Err(EPIPE) };
                    }
                    {
                        let mut pi = peer.inner.lock();
                        if pi.shut_rd {
                            drop(pi);
                            super::signal::send_current(super::signal::SIGPIPE);
                            return if done > 0 { Ok(done) } else { Err(EPIPE) };
                        }
                        let space = RCVBUF.saturating_sub(pi.rx_bytes);
                        if space > 0 {
                            let n = space.min(data.len() - done);
                            pi.rx_bytes += n;
                            pi.rx.push_back(Msg {
                                data: data[done..done + n].to_vec(),
                                off: 0,
                                fds: core::mem::take(&mut fds),
                                cred,
                                from: None,
                            });
                            done += n;
                            drop(pi);
                            peer.changed();
                            if done == data.len() {
                                return Ok(done);
                            }
                            continue;
                        }
                    }
                    if nb {
                        return if done > 0 { Ok(done) } else { Err(EAGAIN) };
                    }
                    if super::signal::interrupted() {
                        return if done > 0 { Ok(done) } else { Err(EINTR) };
                    }
                    peer.wq.wait_until(|| {
                        let pi = peer.inner.lock();
                        pi.rx_bytes < RCVBUF || pi.shut_rd || super::signal::interrupted()
                    });
                }
            }
        }
    }

    /// Receive into `buf`. Returns (bytes, the message's real length for MSG_TRUNC on a
    /// datagram, files, sender cred if the socket passes creds, sender address).
    pub fn recv(&self, buf: &mut [u8], peek: bool, nonblock: bool) -> KResult<Received> {
        let deadline = self.deadline();
        loop {
            {
                let mut i = self.inner.lock();
                let passcred = i.passcred;
                if !i.rx.is_empty() {
                    let r = match self.kind {
                        Kind::Dgram => take_dgram(&mut i, buf, peek, passcred),
                        Kind::Stream => take_stream(&mut i, buf, peek, passcred),
                    };
                    drop(i);
                    if !peek {
                        self.consumed(); // room for writers
                    }
                    return Ok(r);
                }
                let eof = match self.kind {
                    Kind::Stream => {
                        i.shut_rd
                            || i.peer_shut_wr
                            || (i.was_connected
                                && !matches!(&i.conn, Conn::Connected(w) if w.strong_count() > 0))
                    }
                    Kind::Dgram => i.shut_rd,
                };
                if eof || buf.is_empty() {
                    return Ok(Received::default());
                }
                if self.kind == Kind::Stream
                    && !i.was_connected
                    && !matches!(i.conn, Conn::Connected(_))
                {
                    return Err(ENOTCONN);
                }
            }
            if nonblock {
                return Err(EAGAIN);
            }
            self.block(deadline, || {
                let i = self.inner.lock();
                !i.rx.is_empty()
                    || i.shut_rd
                    || i.peer_shut_wr
                    || matches!(&i.conn, Conn::Connected(w) if w.strong_count() == 0)
            })?;
        }
    }
}

#[derive(Default)]
pub struct Received {
    pub n: usize,
    /// Length of the whole datagram (> n if it was truncated).
    pub full: usize,
    pub fds: Vec<Arc<OpenFile>>,
    pub cred: Option<Ucred>,
    pub from: Option<Addr>,
}

fn take_dgram(i: &mut Inner, buf: &mut [u8], peek: bool, passcred: bool) -> Received {
    let m = if peek {
        let m = i.rx.front().unwrap();
        Msg {
            data: m.data.clone(),
            off: 0,
            fds: Vec::new(),
            cred: m.cred,
            from: m.from.clone(),
        }
    } else {
        let m = i.rx.pop_front().unwrap();
        i.rx_bytes -= m.data.len();
        m
    };
    let n = buf.len().min(m.data.len());
    buf[..n].copy_from_slice(&m.data[..n]);
    Received {
        n,
        full: m.data.len(),
        fds: m.fds,
        cred: passcred.then_some(m.cred),
        from: m.from,
    }
}

fn take_stream(i: &mut Inner, buf: &mut [u8], peek: bool, passcred: bool) -> Received {
    let mut r = Received::default();
    let mut idx = 0;
    while r.n < buf.len() && idx < i.rx.len() {
        let m = &mut i.rx[idx];
        // Files travel with the first byte of their write: don't run into the next one
        // that carries some (it comes with the next read), nor mix senders' creds.
        if r.n > 0 && (!m.fds.is_empty() || (passcred && r.cred != Some(m.cred))) {
            break;
        }
        if r.n == 0 {
            r.cred = passcred.then_some(m.cred);
            if !peek {
                r.fds = core::mem::take(&mut m.fds);
            }
        }
        let avail = &m.data[m.off..];
        let k = avail.len().min(buf.len() - r.n);
        buf[r.n..r.n + k].copy_from_slice(&avail[..k]);
        r.n += k;
        if !peek {
            m.off += k;
        }
        idx += 1;
    }
    if !peek {
        while i
            .rx
            .front()
            .is_some_and(|m| m.off == m.data.len() && m.fds.is_empty())
        {
            let m = i.rx.pop_front().unwrap();
            i.rx_bytes -= m.data.len();
        }
    }
    r.full = r.n;
    r
}

/// A free abstract name (Linux autobind: 5 hex digits).
fn autobind_name() -> Addr {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed) & 0xfffff;
        let name = alloc::format!("{n:05x}").into_bytes();
        if !ABSTRACT
            .lock()
            .get(&name)
            .is_some_and(|w| w.strong_count() > 0)
        {
            return Addr::Abstract(name);
        }
    }
}

/// The live socket an address names. Paths need write permission on the socket node
/// (and, under the sandbox, an fs grant for it); abstract names a network capability.
fn resolve(addr: &Addr) -> KResult<Arc<UnixSock>> {
    match addr {
        Addr::Abstract(n) => {
            if super::current_caps().net == super::caps::Net::None {
                return Err(super::caps::deny(
                    "connect",
                    "abstract unix socket: net capability dropped",
                ));
            }
            ABSTRACT
                .lock()
                .get(n)
                .and_then(Weak::upgrade)
                .ok_or(ECONNREFUSED)
        }
        Addr::Path(p) => {
            super::syscall::check_fs(fs::abi::AT_FDCWD as i64, p, true, "connect")?;
            let node = fs::lookup_at(Some(&super::syscall::cwd()), p, true)?;
            if node.kind() != InodeType::Socket {
                return Err(ECONNREFUSED);
            }
            fs::perm::may(&node, fs::perm::W)?;
            let key = (node.dev(), node.ino());
            PATHS
                .lock()
                .iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, w)| w.upgrade())
                .ok_or(ECONNREFUSED)
        }
    }
}

impl Drop for UnixSock {
    fn drop(&mut self) {
        // Take what we hold out first: dropping files in flight can drop other sockets,
        // and the peer is locked on its own.
        let (conn, rx, addr) = {
            let mut i = self.inner.lock();
            (
                core::mem::replace(&mut i.conn, Conn::Idle),
                core::mem::take(&mut i.rx),
                i.addr.take(),
            )
        };
        // EOF the peer; refuse (drop) connections still waiting in the backlog.
        if let Conn::Connected(w) = &conn
            && let Some(p) = w.upgrade()
        {
            p.inner.lock().peer_shut_wr = true;
            p.changed();
        }
        drop(conn);
        drop(rx); // closes files still in flight
        match &addr {
            Some(Addr::Abstract(n)) => {
                let mut t = ABSTRACT.lock();
                if t.get(n).is_some_and(|w| w.strong_count() == 0) {
                    t.remove(n);
                }
            }
            Some(Addr::Path(_)) => PATHS.lock().retain(|(_, w)| w.strong_count() > 0),
            None => {}
        }
        poll::notify();
    }
}

impl Inode for UnixSock {
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
        st.size = self.inner.lock().rx_bytes as u64;
        st
    }
    /// read(2): a recv without flags, files or addresses (files sent with it are closed).
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> fs::Result<usize> {
        let nb = self.nonblocking();
        self.recv(buf, false, nb).map(|r| r.n)
    }
    /// write(2): a send to the connected peer.
    fn write_at(&self, _off: u64, data: &[u8]) -> fs::Result<usize> {
        let nb = self.nonblocking();
        self.send(data, Vec::new(), own_cred(false), None, nb)
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        Err(EINVAL)
    }

    fn poll(&self) -> u32 {
        let (mut r, peer, dpeer) = {
            let i = self.inner.lock();
            let mut r = 0;
            if !i.rx.is_empty() {
                r |= POLLIN | POLLRDNORM;
            }
            let peer = match &i.conn {
                Conn::Listening { pending, .. } => {
                    if !pending.is_empty() {
                        r |= POLLIN | POLLRDNORM;
                    }
                    return r;
                }
                Conn::Connected(w) => w.upgrade(),
                Conn::Idle => None,
            };
            if self.kind == Kind::Stream {
                if i.peer_shut_wr || i.shut_rd {
                    r |= POLLIN | POLLRDNORM | POLLRDHUP;
                }
                if i.was_connected && peer.is_none() {
                    r |= POLLHUP | POLLIN | POLLRDNORM;
                }
                if i.shut_wr && (i.shut_rd || i.peer_shut_wr) {
                    r |= POLLHUP;
                }
            }
            let dpeer = i.dgram_peer.as_ref().and_then(Weak::upgrade);
            (r, peer, dpeer)
        };
        match self.kind {
            Kind::Stream => {
                if let Some(p) = peer
                    && p.inner.lock().rx_bytes < RCVBUF
                {
                    r |= POLLOUT | POLLWRNORM;
                }
            }
            Kind::Dgram => match dpeer {
                Some(p) if p.inner.lock().rx_bytes >= RCVBUF => {}
                _ => r |= POLLOUT | POLLWRNORM,
            },
        }
        r
    }

    fn poll_seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }
}

/// ECONNRESET is what Linux gives a reader whose peer vanished with unread data on its
/// side; we report plain EOF instead (kept for callers that want the constant).
#[allow(dead_code)]
pub const RESET: Errno = ECONNRESET;

// ---- sockaddr_un ---------------------------------------------------------------------

/// Parse a `struct sockaddr_un` of `len` bytes. `Ok(None)` for just the family (autobind).
pub fn parse_addr(b: &[u8]) -> KResult<Option<Addr>> {
    if b.len() < 2 || u16::from_le_bytes([b[0], b[1]]) as u32 != AF_UNIX {
        return Err(EINVAL);
    }
    let path = &b[2..b.len().min(110)];
    if path.is_empty() {
        return Ok(None);
    }
    if path[0] == 0 {
        return Ok(Some(Addr::Abstract(path[1..].to_vec())));
    }
    let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    let s = core::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
    if s.is_empty() {
        return Err(EINVAL);
    }
    Ok(Some(Addr::Path(String::from(s))))
}

/// `struct sockaddr_un` bytes for `a` (just the family for an unnamed socket).
pub fn encode_addr(a: Option<&Addr>) -> Vec<u8> {
    let mut v = Vec::from((AF_UNIX as u16).to_le_bytes());
    match a {
        None => {}
        Some(Addr::Path(p)) => {
            v.extend_from_slice(p.as_bytes());
            v.push(0);
        }
        Some(Addr::Abstract(n)) => {
            v.push(0);
            v.extend_from_slice(n);
        }
    }
    v
}

/// SCM_CREDENTIALS a sender claims: only its own pid and ids (root: anything).
pub fn check_claimed(c: Ucred) -> KResult<Ucred> {
    let me = super::current_cred();
    if me.is_root() {
        return Ok(c);
    }
    let pid_ok = c.pid == crate::sched::current_pid();
    let uid_ok = [me.ruid, me.euid, me.suid].contains(&c.uid);
    let gid_ok = [me.rgid, me.egid, me.sgid].contains(&c.gid);
    if pid_ok && uid_ok && gid_ok {
        Ok(c)
    } else {
        Err(EPERM)
    }
}

/// Default SCM_CREDENTIALS for the caller (pid, real uid, real gid).
pub fn default_cred() -> Ucred {
    own_cred(false)
}

/// For fd-passing: an inode that is an AF_UNIX socket.
#[allow(dead_code)]
pub fn is_unix(n: &InodeRef) -> bool {
    downcast::<UnixSock>(n).is_some()
}
