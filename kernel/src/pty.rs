//! Pseudo-terminals: a master (a file the owner reads the terminal's output from and
//! writes its "keyboard" into) and a slave `/dev/ttypN`, which is an ordinary
//! `proc::tty::Tty`: the same line discipline as the console and the desktop Terminal
//! (echo, editing, ^C -> SIGINT to the foreground group, termios, window size). `mssh`
//! runs a remote shell on one.
//!
//! * `MILK_OPENPTY` (0x1002, milkOS-specific, routed through `proc::sock`): takes a free
//!   pty, resets its tty, and returns a master fd; the slave number is written to `*arg0`
//!   (a u32). The caller opens `/dev/ttypN`.
//! * Master `read`: what the slave side printed (blocks until there is some; EINTR on a
//!   signal); `write`: typed bytes, fed to the line discipline; `poll`: readable when
//!   output is waiting. `ioctl`s on the master (TIOCSWINSZ!) go to the same tty through
//!   the tty registry (rdev 128:N), like the slave's (136:N, Linux's pty majors).
//! * Closing the last master fd hangs the slave up (SIGHUP to its foreground group, EOF
//!   to readers) and frees the pty.
//! * Output not yet read by the master is kept up to 1 MiB, then dropped (the line
//!   discipline's echo writes from inside `input`, so the sink can't block).

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::fs::errno::{Errno, Result};
use crate::fs::node::makedev;
use crate::fs::{self, Inode, InodeType, Stat};
use crate::proc::tty::{self, Tty};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

pub const MAX: usize = 8;
const OUT_CAP: usize = 1 << 20;
pub const MASTER_MAJOR: u32 = 128;
pub const SLAVE_MAJOR: u32 = 136;

fn out0(b: &[u8]) {
    push(0, b)
}
fn out1(b: &[u8]) {
    push(1, b)
}
fn out2(b: &[u8]) {
    push(2, b)
}
fn out3(b: &[u8]) {
    push(3, b)
}
fn out4(b: &[u8]) {
    push(4, b)
}
fn out5(b: &[u8]) {
    push(5, b)
}
fn out6(b: &[u8]) {
    push(6, b)
}
fn out7(b: &[u8]) {
    push(7, b)
}

static TTYS: [Tty; MAX] = [
    Tty::new("ttyp0", out0, 24, 80),
    Tty::new("ttyp1", out1, 24, 80),
    Tty::new("ttyp2", out2, 24, 80),
    Tty::new("ttyp3", out3, 24, 80),
    Tty::new("ttyp4", out4, 24, 80),
    Tty::new("ttyp5", out5, 24, 80),
    Tty::new("ttyp6", out6, 24, 80),
    Tty::new("ttyp7", out7, 24, 80),
];

static OUT: [SpinLock<VecDeque<u8>>; MAX] = [const { SpinLock::new(VecDeque::new()) }; MAX];
static OUT_WQ: [WaitQueue; MAX] = [const { WaitQueue::new() }; MAX];
static USED: [AtomicBool; MAX] = [const { AtomicBool::new(false) }; MAX];
static PUBLISHED: AtomicBool = AtomicBool::new(false);

fn push(n: usize, b: &[u8]) {
    // The SD card log (rpi/logbuf.rs) gets what runs on a pseudo-terminal prints.
    #[cfg(feature = "raspi")]
    crate::rpi::logbuf::pty(n, b);
    {
        let mut q = OUT[n].lock();
        let room = OUT_CAP.saturating_sub(q.len());
        q.extend(&b[..b.len().min(room)]);
    }
    OUT_WQ[n].wake_all();
    crate::poll::notify();
}

struct Master {
    n: usize,
}

impl Inode for Master {
    fn ino(&self) -> u64 {
        0x9100 + self.n as u64
    }
    fn dev(&self) -> u64 {
        0
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(0, self.ino(), InodeType::CharDev, 0o600);
        st.rdev = makedev(MASTER_MAJOR, self.n as u32);
        st.blksize = 1024;
        st
    }
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> Result<usize> {
        loop {
            {
                let mut q = OUT[self.n].lock();
                if !q.is_empty() {
                    let k = buf.len().min(q.len());
                    for (d, s) in buf.iter_mut().zip(q.drain(..k)) {
                        *d = s;
                    }
                    return Ok(k);
                }
            }
            if crate::proc::signal::interrupted() {
                return Err(Errno::EINTR);
            }
            OUT_WQ[self.n].wait_until(|| {
                !OUT[self.n].lock().is_empty() || crate::proc::signal::interrupted()
            });
        }
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> Result<usize> {
        TTYS[self.n].input(buf);
        Ok(buf.len())
    }
    fn truncate(&self, _len: u64) -> Result<()> {
        Ok(())
    }
    fn poll(&self) -> u32 {
        if OUT[self.n].lock().is_empty() {
            0x4
        } else {
            0x1 | 0x4
        }
    }
}

impl Drop for Master {
    fn drop(&mut self) {
        TTYS[self.n].hangup();
        OUT[self.n].lock().clear();
        USED[self.n].store(false, Ordering::Release);
    }
}

struct Slave {
    n: usize,
}

impl Inode for Slave {
    fn ino(&self) -> u64 {
        0x9200 + self.n as u64
    }
    fn dev(&self) -> u64 {
        0
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(0, self.ino(), InodeType::CharDev, 0o620);
        st.rdev = makedev(SLAVE_MAJOR, self.n as u32);
        st.blksize = 1024;
        st
    }
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> Result<usize> {
        TTYS[self.n].read(buf)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> Result<usize> {
        TTYS[self.n].write(buf)
    }
    fn truncate(&self, _len: u64) -> Result<()> {
        Ok(())
    }
    fn poll(&self) -> u32 {
        if TTYS[self.n].readable() {
            0x1 | 0x4
        } else {
            0x4
        }
    }
}

/// `/dev/ttyp0..7` and the tty registry entries, the first time a pty is wanted.
fn publish() {
    if PUBLISHED.swap(true, Ordering::AcqRel) {
        return;
    }
    for (n, t) in TTYS.iter().enumerate() {
        tty::register(makedev(SLAVE_MAJOR, n as u32), t);
        tty::register(makedev(MASTER_MAJOR, n as u32), t);
        let name = alloc::format!("ttyp{n}");
        if let Err(e) = fs::devfs::add_device(&name, Arc::new(Slave { n })) {
            crate::kprintln!("pty: can't add /dev/{name}: {e}");
        }
    }
}

/// A free pty: (its number, the master inode). EAGAIN if all are taken.
pub fn open() -> Result<(usize, Arc<dyn Inode>)> {
    publish();
    for (n, used) in USED.iter().enumerate() {
        if used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            OUT[n].lock().clear();
            TTYS[n].reset(24, 80);
            return Ok((n, Arc::new(Master { n })));
        }
    }
    Err(Errno::EAGAIN)
}
