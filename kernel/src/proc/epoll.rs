//! epoll(7) and pselect6(2), on the readiness core (kernel/src/poll.rs).
//!
//! * `epoll_create1` (20) makes an epoll instance, itself an fd. `epoll_ctl` (21) ADD /
//!   MOD / DEL watches any fd whose object has a `poll` method (pipes, ttys, sockets of
//!   both families, devices); regular files and directories are EPERM, like Linux, and so
//!   is another epoll fd (no nesting). An entry goes away by itself when the watched file
//!   is closed everywhere (it holds the open file weakly).
//! * `epoll_pwait` (22) returns up to `maxevents` ready entries, in fd order.
//!   Level-triggered by default: reported while ready. **EPOLLET**: reported when it gets
//!   ready, and again only after new input / space (the object's `poll_seq` moved) or
//!   after it was seen not ready, so a reader must drain until EAGAIN. **EPOLLONESHOT**:
//!   disabled after one report until EPOLL_CTL_MOD re-arms it. EPOLLERR and EPOLLHUP are
//!   always reported. The signal mask argument is not applied (milkOS has no
//!   per-call masks); a pending signal ends the wait with EINTR.
//! * `pselect6` (72): fd_set read / write / except over the same core.
//!
//! Waiting: one scan of the entries, then sleep until anything notifies
//! (`poll::notify`), at most `poll::RECHECK_NS`, and scan again. O(entries) per wakeup.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use super::current;
use super::errno::*;
use super::syscall::{copy_in, copy_out, file};
use crate::fs::node::{Inode, InodeType, Stat, alloc_dev, downcast};
use crate::fs::{self, OpenFile};
use crate::poll::{self, *};
use crate::sync::SpinLock;

pub const EPOLL_CREATE1: u64 = 20;
pub const EPOLL_CTL: u64 = 21;
pub const EPOLL_PWAIT: u64 = 22;
pub const PSELECT6: u64 = 72;

const EPOLL_CTL_ADD: u32 = 1;
const EPOLL_CTL_DEL: u32 = 2;
const EPOLL_CTL_MOD: u32 = 3;
const EPOLLERR: u32 = 0x008;
const EPOLLHUP: u32 = 0x010;
const EPOLLONESHOT: u32 = 1 << 30;
const EPOLLET: u32 = 1 << 31;
const EPOLL_CLOEXEC: u32 = 0o2000000;
/// `struct epoll_event` on aarch64: u32 events, 4 bytes padding, u64 data.
const EVENT_SIZE: usize = 16;
const MAX_EVENTS: usize = 4096;

pub fn handles(nr: u64) -> bool {
    matches!(nr, EPOLL_CREATE1 | EPOLL_CTL | EPOLL_PWAIT | PSELECT6)
}

pub fn dispatch(nr: u64, a: [u64; 6]) -> KResult<usize> {
    match nr {
        EPOLL_CREATE1 => create(a[0] as u32),
        EPOLL_CTL => ctl(
            a[0] as i32 as i64,
            a[1] as u32,
            a[2] as i32 as i64,
            a[3] as usize,
        ),
        EPOLL_PWAIT => pwait(a[0] as i32 as i64, a[1] as usize, a[2] as i32, a[3] as i32),
        PSELECT6 => pselect6(
            a[0] as usize,
            a[1] as usize,
            a[2] as usize,
            a[3] as usize,
            a[4] as usize,
        ),
        _ => Err(ENOSYS),
    }
}

struct Entry {
    file: Weak<OpenFile>,
    events: u32,
    data: u64,
    /// `poll_seq` when last reported (edge-triggered), None = never.
    reported_seq: Option<u64>,
    /// Seen not ready since the last report (so the next readiness is an edge).
    seen_idle: bool,
    /// One-shot entry that fired; waits for EPOLL_CTL_MOD.
    disabled: bool,
}

pub struct Epoll {
    entries: SpinLock<BTreeMap<i64, Entry>>,
    ino: u64,
}

fn dev() -> u64 {
    static DEV: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let d = DEV.load(core::sync::atomic::Ordering::Relaxed);
    if d != 0 {
        return d;
    }
    let n = alloc_dev();
    DEV.store(n, core::sync::atomic::Ordering::Relaxed);
    n
}

impl Inode for Epoll {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        dev()
    }
    fn kind(&self) -> InodeType {
        InodeType::File
    }
    fn stat(&self) -> Stat {
        Stat::new(dev(), self.ino, InodeType::File, 0o600)
    }
    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> fs::Result<usize> {
        Err(EINVAL)
    }
    fn write_at(&self, _off: u64, _buf: &[u8]) -> fs::Result<usize> {
        Err(EINVAL)
    }
    /// Readable when some entry is ready (so an epoll fd can sit in poll / select).
    fn poll(&self) -> u32 {
        let mut out = Vec::new();
        self.scan(&mut out, 1, true);
        if out.is_empty() { 0 } else { POLLIN }
    }
}

impl Epoll {
    /// Collect up to `max` (events, data) into `out`. `peek`: don't change edge /
    /// one-shot state (for `poll` on the epoll fd itself).
    fn scan(&self, out: &mut Vec<(u32, u64)>, max: usize, peek: bool) {
        let mut entries = self.entries.lock();
        let mut dead = Vec::new();
        for (&fd, e) in entries.iter_mut() {
            if out.len() >= max {
                break;
            }
            if e.disabled {
                continue;
            }
            let Some(f) = e.file.upgrade() else {
                dead.push(fd);
                continue;
            };
            let rev = f.inode().poll() & (e.events | EPOLLERR | EPOLLHUP);
            if rev == 0 {
                if !peek {
                    e.seen_idle = true;
                }
                continue;
            }
            if e.events & EPOLLET != 0 {
                let seq = f.inode().poll_seq();
                let edge = e.reported_seq != Some(seq) || e.seen_idle;
                if !edge {
                    continue;
                }
                if !peek {
                    e.reported_seq = Some(seq);
                    e.seen_idle = false;
                }
            }
            if !peek && e.events & EPOLLONESHOT != 0 {
                e.disabled = true;
            }
            out.push((rev, e.data));
        }
        for fd in dead {
            entries.remove(&fd);
        }
    }
}

fn create(flags: u32) -> KResult<usize> {
    if flags & !EPOLL_CLOEXEC != 0 {
        return Err(EINVAL);
    }
    static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
    let ep = Arc::new(Epoll {
        entries: SpinLock::new(BTreeMap::new()),
        ino: NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
    });
    let of = OpenFile::new(ep, fs::abi::O_RDWR);
    let fd = current()
        .inner
        .lock()
        .fds
        .alloc(Arc::new(of), flags & EPOLL_CLOEXEC != 0)?;
    Ok(fd as usize)
}

fn epoll_of(f: &OpenFile) -> KResult<&Epoll> {
    downcast::<Epoll>(f.inode()).ok_or(EINVAL)
}

fn ctl(epfd: i64, op: u32, fd: i64, ev: usize) -> KResult<usize> {
    let ef = file(epfd)?;
    let ep = epoll_of(&ef)?;
    let target = file(fd)?;
    if downcast::<Epoll>(target.inode()).is_some() {
        return Err(EINVAL); // no nested epoll
    }
    if matches!(target.inode().kind(), InodeType::File | InodeType::Dir) {
        return Err(EPERM); // always ready: Linux refuses them too
    }
    let (events, data) = if op == EPOLL_CTL_DEL {
        (0, 0)
    } else {
        let b = copy_in(ev, EVENT_SIZE)?;
        (
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            u64::from_le_bytes(b[8..16].try_into().unwrap()),
        )
    };
    let mut entries = ep.entries.lock();
    // An entry whose file was closed (and the fd number reused) doesn't count.
    if entries
        .get(&fd)
        .is_some_and(|e| !e.file.upgrade().is_some_and(|f| Arc::ptr_eq(&f, &target)))
    {
        entries.remove(&fd);
    }
    match op {
        EPOLL_CTL_ADD => {
            if entries.contains_key(&fd) {
                return Err(EEXIST);
            }
            entries.insert(
                fd,
                Entry {
                    file: Arc::downgrade(&target),
                    events,
                    data,
                    reported_seq: None,
                    seen_idle: false,
                    disabled: false,
                },
            );
        }
        EPOLL_CTL_MOD => {
            let e = entries.get_mut(&fd).ok_or(ENOENT)?;
            e.events = events;
            e.data = data;
            e.disabled = false;
            e.reported_seq = None;
        }
        EPOLL_CTL_DEL => {
            entries.remove(&fd).ok_or(ENOENT)?;
        }
        _ => return Err(EINVAL),
    }
    drop(entries);
    poll::notify(); // a waiter may now have something to report
    Ok(0)
}

fn pwait(epfd: i64, events: usize, max: i32, timeout_ms: i32) -> KResult<usize> {
    if max <= 0 || max as usize > MAX_EVENTS {
        return Err(EINVAL);
    }
    let ef = file(epfd)?;
    let ep = epoll_of(&ef)?;
    let deadline = if timeout_ms < 0 {
        u64::MAX
    } else {
        crate::timer::now_ns().saturating_add(timeout_ms as u64 * 1_000_000)
    };
    let mut out = Vec::new();
    loop {
        let g = poll::generation();
        ep.scan(&mut out, max as usize, false);
        if !out.is_empty() {
            let mut b = alloc::vec![0u8; out.len() * EVENT_SIZE];
            for (k, (ev, data)) in out.iter().enumerate() {
                b[k * EVENT_SIZE..k * EVENT_SIZE + 4].copy_from_slice(&ev.to_le_bytes());
                b[k * EVENT_SIZE + 8..k * EVENT_SIZE + 16].copy_from_slice(&data.to_le_bytes());
            }
            copy_out(events, &b)?;
            return Ok(out.len());
        }
        if timeout_ms == 0 || crate::timer::now_ns() >= deadline {
            return Ok(0);
        }
        if super::signal::interrupted() {
            return Err(EINTR);
        }
        poll::wait(g, deadline);
    }
}

// ---- pselect6 -----------------------------------------------------------------------------

fn bit(set: &[u8], fd: usize) -> bool {
    set[fd / 8] & (1 << (fd % 8)) != 0
}

fn pselect6(nfds: usize, rd: usize, wr: usize, ex: usize, tmo: usize) -> KResult<usize> {
    if nfds > 1024 {
        return Err(EINVAL);
    }
    let bytes = nfds.div_ceil(64) * 8;
    let load = |va: usize| -> KResult<Vec<u8>> {
        if va == 0 {
            Ok(alloc::vec![0u8; bytes])
        } else {
            copy_in(va, bytes)
        }
    };
    let (rs, ws, es) = (load(rd)?, load(wr)?, load(ex)?);
    // The files once, up front: EBADF for a bad fd in any set.
    let mut fds: Vec<(usize, Arc<OpenFile>)> = Vec::new();
    for fd in 0..nfds {
        if bit(&rs, fd) || bit(&ws, fd) || bit(&es, fd) {
            fds.push((fd, file(fd as i64)?));
        }
    }
    let deadline = if tmo == 0 {
        u64::MAX
    } else {
        let t = copy_in(tmo, 16)?;
        let sec = i64::from_le_bytes(t[0..8].try_into().unwrap());
        let nsec = i64::from_le_bytes(t[8..16].try_into().unwrap());
        if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
            return Err(EINVAL);
        }
        crate::timer::now_ns()
            .saturating_add((sec as u64).saturating_mul(1_000_000_000) + nsec as u64)
    };
    loop {
        let g = poll::generation();
        let (mut ro, mut wo, mut eo) = (
            alloc::vec![0u8; bytes],
            alloc::vec![0u8; bytes],
            alloc::vec![0u8; bytes],
        );
        let mut n = 0;
        for (fd, f) in &fds {
            let p = f.inode().poll();
            let mut set = |s: &[u8], o: &mut [u8], cond: bool| {
                if bit(s, *fd) && cond {
                    o[fd / 8] |= 1 << (fd % 8);
                    n += 1;
                }
            };
            set(&rs, &mut ro, p & (POLLIN | POLLHUP | POLLERR) != 0);
            set(&ws, &mut wo, p & (POLLOUT | POLLERR) != 0);
            set(&es, &mut eo, p & POLLPRI != 0);
        }
        let now = crate::timer::now_ns();
        if n > 0 || now >= deadline {
            for (va, o) in [(rd, &ro), (wr, &wo), (ex, &eo)] {
                if va != 0 {
                    copy_out(va, o)?;
                }
            }
            return Ok(n);
        }
        if super::signal::interrupted() {
            return Err(EINTR);
        }
        poll::wait(g, deadline);
    }
}

/// For `/proc/<pid>/fd` and friends: is this an epoll instance?
#[allow(dead_code)]
pub fn is_epoll(f: &OpenFile) -> bool {
    downcast::<Epoll>(f.inode()).is_some()
}
