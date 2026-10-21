//! `/dev/tty1`: the desktop Terminal's terminal device. Same cooked line discipline as the
//! console tty (`proc/console.rs`: echo, backspace, ^U, ^C drops the line, ^D at line start is
//! EOF, one line per read), but its input comes from the window system (keys typed into the
//! Terminal window, via [`input`]) and its output goes to a buffer the Terminal drains and draws
//! ([`take_output`]), instead of the UART.
//!
//! A kernel thread ("term-sh") keeps a `msh` running on it, restarting it when it exits.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::fs::node::makedev;
use crate::fs::{self, Inode, InodeType, Stat};
use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;
use crate::{kprintln, proc};

pub const PATH: &str = "/dev/tty1";

struct Tty {
    /// The line being typed (cooked mode), not yet readable.
    line: Vec<u8>,
    /// Completed input, ready for `read`.
    ready: VecDeque<u8>,
    eof: bool,
    /// Bytes written by the program (and echo), waiting for the Terminal to draw them.
    out: Vec<u8>,
}

static TTY: SpinLock<Tty> = SpinLock::new(Tty {
    line: Vec::new(),
    ready: VecDeque::new(),
    eof: false,
    out: Vec::new(),
});
/// Readers wait here for a line.
static RX_WQ: WaitQueue = WaitQueue::new();
/// Bumped on every output, so the Terminal can tell cheaply whether to drain.
static OUT_GEN: AtomicU64 = AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

const OUT_MAX: usize = 64 * 1024;

fn push_out(t: &mut Tty, b: &[u8]) {
    if t.out.len() + b.len() > OUT_MAX {
        let drop = (t.out.len() + b.len() - OUT_MAX).min(t.out.len());
        t.out.drain(..drop);
    }
    t.out.extend_from_slice(b);
    OUT_GEN.fetch_add(1, Ordering::Relaxed);
    super::WAKE.wake_all();
}

/// Feed one byte typed into the Terminal window.
pub fn input(b: u8) {
    let mut wake = false;
    {
        let mut t = TTY.lock();
        match b {
            b'\r' | b'\n' => {
                push_out(&mut t, b"\n");
                t.line.push(b'\n');
                let l = core::mem::take(&mut t.line);
                t.ready.extend(l);
                wake = true;
            }
            0x7f | 0x08 => {
                while let Some(c) = t.line.pop() {
                    if c & 0xc0 != 0x80 {
                        push_out(&mut t, b"\x08 \x08");
                        break;
                    }
                }
            }
            0x15 => {
                while let Some(c) = t.line.pop() {
                    if c & 0xc0 != 0x80 {
                        push_out(&mut t, b"\x08 \x08");
                    }
                }
            }
            0x03 => {
                push_out(&mut t, b"^C\n");
                t.line.clear();
                t.ready.push_back(b'\n');
                wake = true;
            }
            0x04 => {
                if t.line.is_empty() {
                    t.eof = true;
                } else {
                    let l = core::mem::take(&mut t.line);
                    t.ready.extend(l);
                }
                wake = true;
            }
            b'\t' | 0x20.. => {
                t.line.push(b);
                push_out(&mut t, &[b]);
            }
            _ => {}
        }
    }
    if wake {
        RX_WQ.wake_all();
    }
}

/// Take everything written since the last call. Cheap when nothing is new.
pub fn take_output(seen_gen: &mut u64) -> Option<Vec<u8>> {
    let g = OUT_GEN.load(Ordering::Relaxed);
    if g == *seen_gen {
        return None;
    }
    *seen_gen = g;
    let mut t = TTY.lock();
    Some(core::mem::take(&mut t.out))
}

fn read(buf: &mut [u8]) -> fs::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    loop {
        {
            let mut t = TTY.lock();
            if !t.ready.is_empty() {
                let line_end = t
                    .ready
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(usize::MAX, |i| i + 1);
                let n = buf.len().min(t.ready.len()).min(line_end);
                for (i, b) in t.ready.drain(..n).enumerate() {
                    buf[i] = b;
                }
                return Ok(n);
            }
            if t.eof {
                t.eof = false;
                return Ok(0);
            }
        }
        RX_WQ.wait_until(|| {
            let t = TTY.lock();
            !t.ready.is_empty() || t.eof
        });
    }
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
        read(buf)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> fs::Result<usize> {
        push_out(&mut TTY.lock(), buf);
        Ok(buf.len())
    }
    fn truncate(&self, _len: u64) -> fs::Result<()> {
        Ok(())
    }
    /// isatty() only checks that TCGETS succeeds. (Filling in termios / winsize needs the
    /// syscall layer's copy_out, which only the console gets today.)
    fn ioctl(&self, cmd: u32, _arg: usize) -> fs::Result<usize> {
        match cmd {
            0x5401 | 0x5413 | 0x540f => Ok(0),
            _ => Err(fs::Errno::ENOTTY),
        }
    }
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
    STARTED.store(true, Ordering::Relaxed);
    drop(sched::spawn("term-sh", || {
        loop {
            const ENV: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb"];
            match proc::spawn_on("/bin/msh", &["msh"], ENV, PATH, "/home/milk") {
                Ok(pid) => {
                    kprintln!("wm: msh pid {pid} on {PATH}");
                    let st = proc::wait_kernel_child(pid);
                    push_out(
                        &mut TTY.lock(),
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
