//! `/dev/tty1`: the desktop Terminal's terminal device. It is a `proc::tty::Tty` (the
//! same line discipline as the UART console: echo, editing, ^C -> SIGINT to the
//! foreground process group, termios, window size), whose input comes from keys typed into
//! the Terminal window ([`input`]) and whose output goes to a buffer the Terminal drains
//! and draws ([`take_output`]).
//!
//! A kernel thread ("term-sh") keeps a `msh` running on it, restarting it when it exits.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::fs::node::makedev;
use crate::fs::{self, Inode, InodeType, Stat};
use crate::proc::tty::Tty;
use crate::sched;
use crate::sync::SpinLock;
use crate::{kprintln, proc};

pub const PATH: &str = "/dev/tty1";

/// The terminal itself. 24 x 80 until the Terminal window reports its size.
pub static TTY1: Tty = Tty::new("tty1", push_out, 24, 80);

/// Bytes written by programs (and echo), waiting for the Terminal to draw them.
static OUT: SpinLock<Vec<u8>> = SpinLock::new(Vec::new());
/// Bumped on every output, so the Terminal can tell cheaply whether to drain.
static OUT_GEN: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

const OUT_MAX: usize = 64 * 1024;

fn push_out(b: &[u8]) {
    {
        let mut out = OUT.lock();
        if out.len() + b.len() > OUT_MAX {
            let drop = (out.len() + b.len() - OUT_MAX).min(out.len());
            out.drain(..drop);
        }
        out.extend_from_slice(b);
    }
    OUT_GEN.fetch_add(1, Ordering::Relaxed);
    super::WAKE.wake_all();
}

/// Feed one byte typed into the Terminal window (wm thread).
pub fn input(b: u8) {
    TTY1.input(&[b]);
}

/// The Terminal window's text area changed size: rows x cols character cells.
/// Sends SIGWINCH to the foreground job when it actually changes.
pub fn set_winsize(rows: u16, cols: u16) {
    TTY1.set_winsize(rows, cols);
}

/// Take everything written since the last call. Cheap when nothing is new.
pub fn take_output(seen_gen: &mut u64) -> Option<Vec<u8>> {
    let g = OUT_GEN.load(Ordering::Relaxed);
    if g == *seen_gen {
        return None;
    }
    *seen_gen = g;
    Some(core::mem::take(&mut *OUT.lock()))
}

struct TtyNode {
    ino: u64,
}

impl Inode for TtyNode {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        0
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(0, self.ino, InodeType::CharDev, 0o620);
        st.rdev = makedev(4, 1); // Linux tty1
        st.blksize = 1024;
        st
    }
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> fs::Result<usize> {
        TTY1.read(buf)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> fs::Result<usize> {
        TTY1.write(buf)
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        Ok(())
    }
    // ioctls (termios, winsize, foreground group) are handled by the syscall layer through
    // the tty registry (`proc::tty::register`, done in start_shell), not here.
}

/// Publish `/dev/tty1` and start the thread that keeps a shell on it. Idempotent. False if
/// there is no `/bin/msh` (then the Terminal keeps its built-in shell).
pub fn start_shell() -> bool {
    if STARTED.load(Ordering::Relaxed) {
        return true;
    }
    if fs::stat("/bin/msh").is_err() {
        return false;
    }
    if fs::stat(PATH).is_err()
        && let Err(e) = fs::devfs::add_device("tty1", Arc::new(TtyNode { ino: 0x7771 }))
    {
        kprintln!("wm: cannot publish {PATH}: {e}");
        return false;
    }
    proc::tty::register(makedev(4, 1), &TTY1);
    STARTED.store(true, Ordering::Relaxed);
    drop(sched::spawn("term-sh", || {
        loop {
            const ENV: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb"];
            match proc::spawn_on("/bin/msh", &["msh"], ENV, PATH, "/home/milk") {
                Ok(pid) => {
                    kprintln!("wm: msh pid {pid} on {PATH}");
                    let st = proc::wait_kernel_child(pid);
                    push_out(
                        alloc::format!("\n[msh exited ({st:#x}), starting a new one]\n").as_bytes(),
                    );
                }
                Err(e) => {
                    kprintln!("wm: cannot start msh on {PATH}: errno {}", e.0);
                    sched::sleep_ms(2000);
                }
            }
        }
    }));
    true
}

/// Is tty1 in canonical (line) mode? (The Terminal keeps Ctrl+L for itself only then.)
pub fn canonical() -> bool {
    const ICANON: u32 = 0o2;
    match TTY1.ioctl(proc::tty::TCGETS, &[]) {
        Ok(Some(t)) if t.len() >= 16 => {
            u32::from_le_bytes([t[12], t[13], t[14], t[15]]) & ICANON != 0
        }
        _ => true,
    }
}
