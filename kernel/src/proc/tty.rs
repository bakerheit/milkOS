//! Terminals: a line discipline you can instantiate once per terminal.
//!
//! [`CONSOLE`] is the PL011 UART behind `/dev/console` (fd 0/1/2 of everything the kernel
//! starts). A Terminal window would be a second `static TERM1: Tty = Tty::new(..)`.
//!
//! ## API (for anyone adding a terminal)
//! ```ignore
//! static TERM: Tty = Tty::new("tty1", draw_bytes, 40, 120);   // output sink, rows, cols
//! tty::register(makedev(4, 1), &TERM);   // so ioctl(TIOCSPGRP/...) on its device node works
//! TERM.input(b"ls\r");                   // feed typed bytes (thread context, not IRQs)
//! TERM.read(buf) / TERM.write(buf)       // what the device node's read_at/write_at call
//! ```
//! `input` runs the line discipline: echo through the output sink, backspace, ^U kills the
//! line, Enter delivers it, ^D at the start of a line is end of file, ^C / ^\ flush input
//! and send SIGINT / SIGQUIT to the foreground process group (set with TIOCSPGRP).
//! `read` gives at most one line per call and blocks until one is there (interruptible:
//! EINTR on a signal). Before the scheduler runs, `read` is non-blocking (EAGAIN).
//!
//! termios: TCGETS/TCSETS* round-trip a Linux `struct termios`; the flags honoured are
//! ICANON (off: raw bytes, read returns whatever is there), ECHO, ISIG (^C/^\ signals)
//! and ICRNL. Window size: `TERM.set_winsize(rows, cols)` (the Terminal calls it when the
//! window is resized) or TIOCSWINSZ; a change sends SIGWINCH to the foreground group.
//!
//! The UART is fed by a kernel thread, "tty-uart", that sleeps until the RX interrupt
//! fires, so ^C works while nobody is reading.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use super::errno::*;
use super::signal::{self, SigInfo};
use crate::sched::{self, Priority, WaitQueue};
use crate::sync::SpinLock;
use crate::uart;

struct State {
    line: Vec<u8>,
    ready: VecDeque<u8>,
    eof: bool,
    /// Inside an ESC [ ... sequence (arrow keys etc.), which we swallow in cooked mode.
    esc: u8,
    /// Linux `struct termios` (asm-generic, 60 bytes), as last set.
    termios: [u8; 60],
}

// termios flag bits (asm-generic)
const ICRNL: u32 = 0o400;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ECHOE: u32 = 0o20;
const ECHOK: u32 = 0o40;
const IEXTEN: u32 = 0o100000;

/// Sane defaults, like a fresh Linux tty: cooked, echo, signals, CR -> NL, OPOST|ONLCR,
/// CS8|CREAD|B38400, ^C ^\ DEL ^U ^D.
const fn default_termios() -> [u8; 60] {
    let mut t = [0u8; 60];
    let iflag = ICRNL.to_le_bytes();
    let oflag = (0o1u32 | 0o4).to_le_bytes();
    let cflag = (0o60u32 | 0o200 | 0o17).to_le_bytes();
    let lflag = (ISIG | ICANON | ECHO | ECHOE | ECHOK | IEXTEN).to_le_bytes();
    let mut i = 0;
    while i < 4 {
        t[i] = iflag[i];
        t[4 + i] = oflag[i];
        t[8 + i] = cflag[i];
        t[12 + i] = lflag[i];
        i += 1;
    }
    // c_cc starts at 17: VINTR, VQUIT, VERASE, VKILL, VEOF, VTIME, VMIN
    t[17] = 0x03;
    t[18] = 0x1c;
    t[19] = 0x7f;
    t[20] = 0x15;
    t[21] = 0x04;
    t[22] = 0;
    t[23] = 1;
    t
}

impl State {
    fn flag(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.termios[off..off + 4].try_into().unwrap())
    }
    fn lflag(&self) -> u32 {
        self.flag(12)
    }
    fn iflag(&self) -> u32 {
        self.flag(0)
    }
}

pub struct Tty {
    pub name: &'static str,
    st: SpinLock<State>,
    readers: WaitQueue,
    fg_pgrp: AtomicU32,
    output: fn(&[u8]),
    /// rows << 16 | cols
    winsize: AtomicU32,
}

impl Tty {
    pub const fn new(name: &'static str, output: fn(&[u8]), rows: u16, cols: u16) -> Tty {
        Tty {
            name,
            st: SpinLock::new(State {
                line: Vec::new(),
                ready: VecDeque::new(),
                eof: false,
                esc: 0,
                termios: default_termios(),
            }),
            readers: WaitQueue::new(),
            fg_pgrp: AtomicU32::new(0),
            output,
            winsize: AtomicU32::new((rows as u32) << 16 | cols as u32),
        }
    }

    /// (rows, cols).
    pub fn winsize(&self) -> (u16, u16) {
        let w = self.winsize.load(Ordering::Relaxed);
        ((w >> 16) as u16, w as u16)
    }

    /// Set the window size; SIGWINCH to the foreground group if it changed. Thread context.
    pub fn set_winsize(&self, rows: u16, cols: u16) {
        let new = (rows as u32) << 16 | cols as u32;
        if self.winsize.swap(new, Ordering::Relaxed) != new {
            self.signal_foreground(signal::SIGWINCH);
        }
    }

    fn signal_foreground(&self, sig: u32) {
        let pg = self.fg_pgrp.load(Ordering::Relaxed);
        if pg != 0 {
            let info = SigInfo {
                code: signal::SI_KERNEL,
                ..SigInfo::default()
            };
            signal::send_pgrp(pg, sig, info);
        }
    }

    /// Feed typed bytes through the line discipline. Thread context only (allocates,
    /// sends signals).
    pub fn input(&self, bytes: &[u8]) {
        let mut echo = Vec::new();
        let mut sig = None;
        {
            let mut st = self.st.lock();
            for &b in bytes {
                if let Some(s) = st.input(b, &mut echo) {
                    sig = Some(s);
                }
            }
        }
        if !echo.is_empty() {
            (self.output)(&echo);
        }
        if let Some(s) = sig {
            self.signal_foreground(s);
        }
        self.readers.wake_all();
        crate::poll::notify();
    }

    /// Blocking read of at most one line.
    pub fn read(&self, buf: &mut [u8]) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if !sched::started() {
                // No feeder thread yet: take whatever the UART has, never block.
                let mut v = Vec::new();
                while let Some(b) = uart::read_byte() {
                    v.push(b);
                }
                if !v.is_empty() {
                    self.input(&v);
                }
            }
            {
                let mut st = self.st.lock();
                if !st.ready.is_empty() {
                    let canon = st.lflag() & ICANON != 0;
                    let line_end = if canon {
                        st.ready
                            .iter()
                            .position(|&b| b == b'\n')
                            .map_or(usize::MAX, |i| i + 1)
                    } else {
                        usize::MAX
                    };
                    let n = buf.len().min(st.ready.len()).min(line_end);
                    for (i, b) in st.ready.drain(..n).enumerate() {
                        buf[i] = b;
                    }
                    return Ok(n);
                }
                if st.eof {
                    st.eof = false;
                    return Ok(0);
                }
            }
            if !sched::started() {
                return Err(EAGAIN);
            }
            if signal::interrupted() {
                return Err(EINTR);
            }
            self.readers.wait_until(|| {
                let st = self.st.lock();
                !st.ready.is_empty() || st.eof || signal::interrupted()
            });
        }
    }

    /// For poll(2): would `read` return now (a line, any input in raw mode, or ^D)?
    pub fn readable(&self) -> bool {
        let st = self.st.lock();
        !st.ready.is_empty() || st.eof
    }

    pub fn write(&self, buf: &[u8]) -> KResult<usize> {
        (self.output)(buf);
        Ok(buf.len())
    }

/// A fresh terminal again (a pty being reused): default termios, no pending input or
    /// EOF, no foreground group, the given size. Thread context.
    pub fn reset(&self, rows: u16, cols: u16) {
        {
            let mut st = self.st.lock();
            st.line.clear();
            st.ready.clear();
            st.eof = false;
            st.esc = 0;
            st.termios = default_termios();
        }
        self.fg_pgrp.store(0, Ordering::Relaxed);
        self.winsize
            .store((rows as u32) << 16 | cols as u32, Ordering::Relaxed);
    }

    /// The other end went away (a pty's master closed): SIGHUP to the foreground group,
    /// and readers see end of file. Thread context.
    pub fn hangup(&self) {
        self.signal_foreground(signal::SIGHUP);
        self.st.lock().eof = true;
        self.readers.wake_all();
        crate::poll::notify();
    }

    pub fn foreground(&self) -> u32 {
        self.fg_pgrp.load(Ordering::Relaxed)
    }

    pub fn set_foreground(&self, pgid: u32) {
        self.fg_pgrp.store(pgid, Ordering::Relaxed);
    }

    /// Terminal ioctls. `arg` holds the bytes read from the user pointer for the "set"
    /// calls (see [`ioctl_in_size`]); returned bytes (if any) are copied out to the user
    /// pointer by the syscall layer.
    pub fn ioctl(&self, cmd: u32, arg: &[u8]) -> KResult<Option<Vec<u8>>> {
        let u32_arg = || -> KResult<u32> {
            Ok(u32::from_le_bytes(
                arg.get(..4).ok_or(EINVAL)?.try_into().unwrap(),
            ))
        };
        match cmd {
            TCGETS => Ok(Some(self.st.lock().termios.to_vec())),
            TCSETS | TCSETSW | TCSETSF => {
                let mut st = self.st.lock();
                st.termios.copy_from_slice(arg.get(..60).ok_or(EINVAL)?);
                if cmd == TCSETSF {
                    st.ready.clear();
                    st.line.clear();
                }
                // Leaving cooked mode: a half-typed line becomes readable.
                if st.lflag() & ICANON == 0 && !st.line.is_empty() {
                    let l = core::mem::take(&mut st.line);
                    st.ready.extend(l);
                }
                drop(st);
                self.readers.wake_all();
                Ok(None)
            }
            TIOCGWINSZ => {
                let (r, c) = self.winsize();
                Ok(Some(
                    [r, c, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect(),
                ))
            }
            TIOCSWINSZ => {
                let w = arg.get(..4).ok_or(EINVAL)?;
                self.set_winsize(
                    u16::from_le_bytes([w[0], w[1]]),
                    u16::from_le_bytes([w[2], w[3]]),
                );
                Ok(None)
            }
            TIOCGPGRP => Ok(Some(self.foreground().to_le_bytes().to_vec())),
            TIOCSPGRP => {
                let pg = u32_arg()?;
                if pg == 0 {
                    return Err(EINVAL);
                }
                self.set_foreground(pg);
                Ok(None)
            }
            _ => Err(ENOTTY),
        }
    }
}

pub const TCGETS: u32 = 0x5401;
pub const TCSETS: u32 = 0x5402;
pub const TCSETSW: u32 = 0x5403;
pub const TCSETSF: u32 = 0x5404;
pub const TIOCGPGRP: u32 = 0x540f;
pub const TIOCSPGRP: u32 = 0x5410;
pub const TIOCGWINSZ: u32 = 0x5413;
pub const TIOCSWINSZ: u32 = 0x5414;

/// How many bytes of user memory an ioctl reads as its argument (0: none).
pub fn ioctl_in_size(cmd: u32) -> usize {
    match cmd {
        TIOCSPGRP => 4,
        TIOCSWINSZ => 8,
        TCSETS | TCSETSW | TCSETSF => 60,
        _ => 0,
    }
}

impl State {
    /// One input byte. Returns a signal to send to the foreground group, if any.
    fn input(&mut self, mut b: u8, echo: &mut Vec<u8>) -> Option<u32> {
        let (lflag, iflag) = (self.lflag(), self.iflag());
        if b == b'\r' && iflag & ICRNL != 0 {
            b = b'\n';
        }
        if lflag & ISIG != 0 && (b == 0x03 || b == 0x1c) {
            if lflag & ECHO != 0 {
                echo.extend_from_slice(if b == 0x03 { b"^C\n" } else { b"^\\\n" });
            }
            self.line.clear();
            self.ready.clear();
            return Some(if b == 0x03 {
                signal::SIGINT
            } else {
                signal::SIGQUIT
            });
        }
        if lflag & ICANON == 0 {
            // Raw: every byte is readable at once.
            self.ready.push_back(b);
            if lflag & ECHO != 0 {
                echo.push(b);
            }
            return None;
        }
        let mut sink = Vec::new();
        let echo = if lflag & ECHO != 0 { echo } else { &mut sink };
        if self.esc == 1 {
            self.esc = if b == b'[' || b == b'O' { 2 } else { 0 };
            return None;
        }
        if self.esc == 2 {
            if (0x40..=0x7e).contains(&b) {
                self.esc = 0; // final byte
            }
            return None;
        }
        match b {
            b'\r' | b'\n' => {
                echo.push(b'\n');
                self.line.push(b'\n');
                self.ready.extend(self.line.drain(..));
            }
            0x7f | 0x08 => {
                // Drop one whole UTF-8 character.
                while let Some(c) = self.line.pop() {
                    if c & 0xc0 != 0x80 {
                        echo.extend_from_slice(b"\x08 \x08");
                        break;
                    }
                }
            }
            0x15 => {
                // ^U
                while let Some(c) = self.line.pop() {
                    if c & 0xc0 != 0x80 {
                        echo.extend_from_slice(b"\x08 \x08");
                    }
                }
            }
            0x04 => {
                // ^D
                if self.line.is_empty() {
                    self.eof = true;
                } else {
                    self.ready.extend(self.line.drain(..));
                }
            }
            0x1b => self.esc = 1,
            b'\t' | 0x20.. => {
                self.line.push(b);
                echo.push(b);
            }
            _ => {}
        }
        None
    }
}

// ---- the console ---------------------------------------------------------------------

/// Console output: the `kprintln!` sink (UART + screen). Invalid UTF-8 is replaced.
pub fn console_output(b: &[u8]) {
    let s = String::from_utf8_lossy(b);
    crate::sink::with_print_lock(|| {
        let _ = crate::console_writer().write_str(&s);
    });
}

pub static CONSOLE: Tty = Tty::new("console", console_output, 25, 80);

static RX_WQ: WaitQueue = WaitQueue::new();

fn on_rx() {
    RX_WQ.wake_all();
}

static REGISTRY: SpinLock<Vec<(u64, &'static Tty)>> = SpinLock::new(Vec::new());

/// Make `tty` the terminal behind character device `rdev` (for ioctls on its node).
pub fn register(rdev: u64, tty: &'static Tty) {
    REGISTRY.lock().push((rdev, tty));
}

pub fn by_rdev(rdev: u64) -> Option<&'static Tty> {
    REGISTRY
        .lock()
        .iter()
        .find(|(r, _)| *r == rdev)
        .map(|(_, t)| *t)
}

/// Hook the UART up to [`CONSOLE`] and start its feeder thread. After `sched::init`.
pub fn init() {
    register(crate::fs::node::makedev(5, 1), &CONSOLE);
    uart::set_rx_hook(on_rx);
    drop(sched::spawn_prio("tty-uart", Priority::High, || {
        loop {
            // The RX interrupt wakes us; the 1 s timeout is only a defence-in-depth re-check
            // of the UART FIFO in case an interrupt is ever lost (uart::poll reports it).
            if !RX_WQ.wait_until_timeout(|| uart::available() > 0, 1_000_000_000) {
                uart::poll();
                continue;
            }
            let mut v = Vec::new();
            while let Some(b) = uart::read_byte() {
                v.push(b);
            }
            CONSOLE.input(&v);
        }
    }));
}
