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

/// `struct termios` as the Linux TCGETS/TCSETS ioctls see it (36 bytes: four u32 flag
/// words, c_line, c_cc[19]). musl's userspace struct is longer; the kernel copies 36.
#[derive(Clone, Copy)]
pub struct Termios {
    pub iflag: u32,
    pub oflag: u32,
    pub cflag: u32,
    pub lflag: u32,
    pub line: u8,
    pub cc: [u8; 19],
}

pub const TERMIOS_LEN: usize = 36;
// c_lflag bits.
pub const ISIG: u32 = 0o1;
pub const ICANON: u32 = 0o2;
pub const ECHO: u32 = 0o10;
pub const ECHOE: u32 = 0o20;
pub const ECHOK: u32 = 0o40;
pub const ECHOCTL: u32 = 0o1000;
pub const ECHOKE: u32 = 0o4000;
pub const IEXTEN: u32 = 0o100000;
// c_iflag bits.
pub const ICRNL: u32 = 0o400;
pub const IXON: u32 = 0o2000;
// c_cc indices.
const VINTR: usize = 0;
const VERASE: usize = 2;
const VKILL: usize = 3;
const VEOF: usize = 4;

impl Termios {
    /// What `stty sane` gives: cooked, echoing, ^C/^U/^D/DEL as usual.
    pub const SANE: Termios = Termios {
        iflag: ICRNL | IXON,
        oflag: 0o5,    // OPOST | ONLCR
        cflag: 0o2277, // B38400 | CS8 | CREAD | HUPCL
        lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN,
        line: 0,
        cc: [
            3, 0x1c, 0x7f, 0x15, 4, 0, 1, 0, 0x11, 0x13, 0x1a, 0, 0x12, 0x0f, 0x17, 0x16, 0, 0, 0,
        ],
    };

    pub fn to_bytes(&self) -> [u8; TERMIOS_LEN] {
        let mut b = [0u8; TERMIOS_LEN];
        for (k, v) in [self.iflag, self.oflag, self.cflag, self.lflag]
            .iter()
            .enumerate()
        {
            b[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        b[16] = self.line;
        b[17..].copy_from_slice(&self.cc);
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<Termios> {
        if b.len() < TERMIOS_LEN {
            return None;
        }
        let w = |k: usize| u32::from_le_bytes([b[k], b[k + 1], b[k + 2], b[k + 3]]);
        let mut cc = [0u8; 19];
        cc.copy_from_slice(&b[17..TERMIOS_LEN]);
        Some(Termios {
            iflag: w(0),
            oflag: w(4),
            cflag: w(8),
            lflag: w(12),
            line: b[16],
            cc,
        })
    }

    pub fn canonical(&self) -> bool {
        self.lflag & ICANON != 0
    }
}

struct Tty {
    termios: Termios,
    /// Window size in character cells (the Terminal keeps it current) and pixels.
    winsize: [u16; 4],
    /// The line being typed (cooked mode), not yet readable.
    line: Vec<u8>,
    /// Completed input, ready for `read`.
    ready: VecDeque<u8>,
    eof: bool,
    /// Inside an ESC [ ... sequence in cooked mode (arrow keys etc.), which is swallowed.
    esc: u8,
    /// Bytes written by the program (and echo), waiting for the Terminal to draw them.
    out: Vec<u8>,
}

static TTY: SpinLock<Tty> = SpinLock::new(Tty {
    termios: Termios::SANE,
    winsize: [24, 80, 0, 0],
    line: Vec::new(),
    ready: VecDeque::new(),
    eof: false,
    esc: 0,
    out: Vec::new(),
});
/// Readers wait here for input.
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

/// Is the tty in canonical (line) mode? The Terminal sends arrow keys either way; this is for
/// showing the mode.
pub fn canonical() -> bool {
    TTY.lock().termios.canonical()
}

/// The Terminal's size in cells and pixels. A change would raise SIGWINCH once there are
/// signals; until then programs re-read TIOCGWINSZ.
pub fn set_winsize(cols: u16, rows: u16, px_w: u16, px_h: u16) {
    TTY.lock().winsize = [rows, cols, px_w, px_h];
}

/// Feed one byte typed into the Terminal window, through the line discipline.
pub fn input(b: u8) {
    let mut wake = false;
    {
        let mut t = TTY.lock();
        let tm = t.termios;
        let echo = tm.lflag & ECHO != 0;
        let b = if b == b'\r' && tm.iflag & ICRNL != 0 {
            b'\n'
        } else {
            b
        };
        if !tm.canonical() {
            // Raw / cbreak: every byte is readable at once (VMIN 1, VTIME 0 semantics).
            t.ready.push_back(b);
            if echo && (b >= 0x20 || b == b'\n' || b == b'\t') {
                push_out(&mut t, &[b]);
            }
            wake = true;
        } else if t.esc == 1 {
            t.esc = if b == b'[' || b == b'O' { 2 } else { 0 };
        } else if t.esc == 2 {
            if (0x40..=0x7e).contains(&b) {
                t.esc = 0;
            }
        } else if b == 0x1b {
            t.esc = 1;
        } else if b == b'\n' {
            if echo {
                push_out(&mut t, b"\n");
            }
            t.line.push(b'\n');
            let l = core::mem::take(&mut t.line);
            t.ready.extend(l);
            wake = true;
        } else if b == tm.cc[VERASE] || b == 0x08 {
            while let Some(c) = t.line.pop() {
                if c & 0xc0 != 0x80 {
                    if echo {
                        push_out(&mut t, b"\x08 \x08");
                    }
                    break;
                }
            }
        } else if b == tm.cc[VKILL] {
            while let Some(c) = t.line.pop() {
                if c & 0xc0 != 0x80 && echo {
                    push_out(&mut t, b"\x08 \x08");
                }
            }
        } else if b == tm.cc[VINTR] && tm.lflag & ISIG != 0 {
            // No signals yet: drop the line and hand the reader an empty one.
            push_out(&mut t, b"^C\n");
            t.line.clear();
            t.ready.push_back(b'\n');
            wake = true;
        } else if b == tm.cc[VEOF] {
            if t.line.is_empty() {
                t.eof = true;
            } else {
                let l = core::mem::take(&mut t.line);
                t.ready.extend(l);
            }
            wake = true;
        } else if b == b'\t' || b >= 0x20 {
            t.line.push(b);
            if echo {
                push_out(&mut t, &[b]);
            }
        }
    }
    if wake {
        RX_WQ.wake_all();
    }
}

/// Bytes of user memory an ioctl reads (TCSETS* take a termios, TIOCSWINSZ a winsize).
pub fn ioctl_in_len(cmd: u32) -> usize {
    match cmd {
        TCSETS | TCSETSW | TCSETSF => TERMIOS_LEN,
        TIOCSWINSZ => 8,
        _ => 0,
    }
}

const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TIOCGPGRP: u32 = 0x540f;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;

/// The tty ioctls, called by the syscall layer for `/dev/tty1` (it copies `input` in and
/// the result out). None = ENOTTY.
pub fn ioctl(cmd: u32, input: &[u8]) -> Option<Vec<u8>> {
    let mut t = TTY.lock();
    match cmd {
        TCGETS => Some(t.termios.to_bytes().to_vec()),
        TCSETS | TCSETSW | TCSETSF => {
            let new = Termios::from_bytes(input)?;
            if cmd == TCSETSF {
                t.ready.clear();
                t.line.clear();
            }
            // Leaving canonical mode makes the half-typed line readable as it is.
            if t.termios.canonical() && !new.canonical() {
                let l = core::mem::take(&mut t.line);
                t.ready.extend(l);
            }
            t.termios = new;
            drop(t);
            RX_WQ.wake_all();
            Some(Vec::new())
        }
        TIOCGWINSZ => Some(t.winsize.iter().flat_map(|v| v.to_le_bytes()).collect()),
        TIOCSWINSZ if input.len() >= 8 => {
            for k in 0..4 {
                t.winsize[k] = u16::from_le_bytes([input[2 * k], input[2 * k + 1]]);
            }
            Some(Vec::new())
        }
        TIOCGPGRP => Some(1u32.to_le_bytes().to_vec()),
        _ => None,
    }
}

/// Is this the stat of `/dev/tty1`?
pub fn is_tty1(st: &Stat) -> bool {
    st.kind == InodeType::CharDev && st.rdev == makedev(4, 1)
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
                // Canonical: at most one line per read. Raw: whatever is there (VMIN 1).
                let line_end = if t.termios.canonical() {
                    t.ready
                        .iter()
                        .position(|&b| b == b'\n')
                        .map_or(usize::MAX, |i| i + 1)
                } else {
                    usize::MAX
                };
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
