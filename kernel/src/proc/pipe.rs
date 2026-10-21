//! Pipes: a 64 KiB ring shared by a read end and a write end, each a VFS inode, so a pipe
//! end sits in the fd table as an ordinary `Arc<OpenFile>` (dup, fork, close just work).
//!
//! * read blocks while the pipe is empty and a writer exists; returns 0 (EOF) once every
//!   write end is closed.
//! * write blocks while full; writes of at most `PIPE_BUF` (4 KiB) are atomic (never
//!   interleaved with another writer's). With no reader left: SIGPIPE + EPIPE.
//! * Blocking waits are interruptible: a pending signal gives EINTR (or the short count).
//! * An end is "closed" when its inode is dropped, i.e. when the last `OpenFile` (and so
//!   the last fd, across dup and fork) referencing it goes away.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::errno::*;
use crate::fs::node::{Inode, InodeRef, InodeType, Stat, alloc_dev};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

pub const PIPE_BUF: usize = 4096;
pub const CAPACITY: usize = 64 * 1024;
pub const EPIPE: Errno = Errno(32);

struct Pipe {
    buf: SpinLock<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
    /// Readers wait here for data / writer gone.
    rd_wq: WaitQueue,
    /// Writers wait here for space / reader gone.
    wr_wq: WaitQueue,
}

pub struct PipeEnd {
    pipe: Arc<Pipe>,
    write: bool,
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

/// A new pipe: (read end, write end).
pub fn new(nonblock: bool) -> (InodeRef, InodeRef) {
    let pipe = Arc::new(Pipe {
        buf: SpinLock::new(VecDeque::new()),
        readers: AtomicUsize::new(1),
        writers: AtomicUsize::new(1),
        rd_wq: WaitQueue::new(),
        wr_wq: WaitQueue::new(),
    });
    let ino = NEXT_INO.fetch_add(1, Ordering::Relaxed);
    let end = |write| -> InodeRef {
        Arc::new(PipeEnd {
            pipe: pipe.clone(),
            write,
            nonblock: AtomicBool::new(nonblock),
            ino,
        })
    };
    (end(false), end(true))
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        if self.write {
            self.pipe.writers.fetch_sub(1, Ordering::Relaxed);
            self.pipe.rd_wq.wake_all();
        } else {
            self.pipe.readers.fetch_sub(1, Ordering::Relaxed);
            self.pipe.wr_wq.wake_all();
        }
    }
}

impl PipeEnd {
    /// "pipe:[ino]", like Linux's /proc/<pid>/fd links.
    pub fn label(&self) -> alloc::string::String {
        alloc::format!("pipe:[{}]", self.ino)
    }
}

impl Inode for PipeEnd {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        dev()
    }
    fn kind(&self) -> InodeType {
        InodeType::Fifo
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(dev(), self.ino, InodeType::Fifo, 0o600);
        st.size = self.pipe.buf.lock().len() as u64;
        st.blksize = PIPE_BUF as u32;
        st
    }

    fn read_at(&self, _off: u64, out: &mut [u8]) -> crate::fs::Result<usize> {
        if self.write {
            return Err(EBADF);
        }
        let p = &*self.pipe;
        loop {
            {
                let mut b = p.buf.lock();
                if !b.is_empty() {
                    let n = out.len().min(b.len());
                    for (o, v) in out.iter_mut().zip(b.drain(..n)) {
                        *o = v;
                    }
                    drop(b);
                    p.wr_wq.wake_all();
                    return Ok(n);
                }
            }
            if p.writers.load(Ordering::Relaxed) == 0 {
                return Ok(0);
            }
            if self.nonblock.load(Ordering::Relaxed) {
                return Err(EAGAIN);
            }
            if super::signal::interrupted() {
                return Err(EINTR);
            }
            p.rd_wq.wait_until(|| {
                !p.buf.lock().is_empty()
                    || p.writers.load(Ordering::Relaxed) == 0
                    || super::signal::interrupted()
            });
        }
    }

    fn write_at(&self, _off: u64, data: &[u8]) -> crate::fs::Result<usize> {
        if !self.write {
            return Err(EBADF);
        }
        let p = &*self.pipe;
        let atomic = data.len() <= PIPE_BUF;
        let mut done = 0;
        while done < data.len() {
            if p.readers.load(Ordering::Relaxed) == 0 {
                super::signal::send_current(super::signal::SIGPIPE);
                return if done > 0 { Ok(done) } else { Err(EPIPE) };
            }
            let need = if atomic { data.len() } else { 1 };
            {
                let mut b = p.buf.lock();
                let space = CAPACITY - b.len();
                if space >= need {
                    let n = space.min(data.len() - done);
                    b.extend(&data[done..done + n]);
                    done += n;
                    drop(b);
                    p.rd_wq.wake_all();
                    continue;
                }
            }
            if self.nonblock.load(Ordering::Relaxed) {
                return if done > 0 { Ok(done) } else { Err(EAGAIN) };
            }
            if super::signal::interrupted() {
                return if done > 0 { Ok(done) } else { Err(EINTR) };
            }
            p.wr_wq.wait_until(|| {
                CAPACITY - p.buf.lock().len() >= need
                    || p.readers.load(Ordering::Relaxed) == 0
                    || super::signal::interrupted()
            });
        }
        Ok(done)
    }

    fn truncate(&self, _len: u64) -> crate::fs::Result<()> {
        Err(EINVAL)
    }
}
