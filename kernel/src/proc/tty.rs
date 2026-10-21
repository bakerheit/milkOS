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
//! Always cooked mode: termios settings are not implemented (TCGETS returns zeros).
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
use crate::sync::{self, SpinLock};
use crate::uart;

struct State {
    line: Vec<u8>,
    ready: VecDeque<u8>,
    eof: bool,
    /// Inside an ESC [ ... sequence (arrow keys etc.), which we swallow.
    esc: u8,
}

pub struct Tty {
    pub name: &'static str,
    st: SpinLock<State>,
    readers: WaitQueue,
    fg_pgrp: AtomicU32,
    output: fn(&[u8]),
    rows: u16,
    cols: u16,
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
            }),
            readers: WaitQueue::new(),
            fg_pgrp: AtomicU32::new(0),
            output,
            rows,
            cols,
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
            let pg = self.fg_pgrp.load(Ordering::Relaxed);
            if pg != 0 {
                let info = SigInfo {
                    code: signal::SI_KERNEL,
                    ..SigInfo::default()
                };
                signal::send_pgrp(pg, s, info);
            }
        }
        self.readers.wake_all();
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
                    let line_end = st
                        .ready
                        .iter()
                        .position(|&b| b == b'\n')
                        .map_or(usize::MAX, |i| i + 1);
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

    pub fn write(&self, buf: &[u8]) -> KResult<usize> {
        (self.output)(buf);
        Ok(buf.len())
    }

    pub fn foreground(&self) -> u32 {
        self.fg_pgrp.load(Ordering::Relaxed)
    }

    pub fn set_foreground(&self, pgid: u32) {
        self.fg_pgrp.store(pgid, Ordering::Relaxed);
    }

    /// Terminal ioctls. `arg` is the value read from the user pointer for the "set" calls;
    /// returned bytes (if any) are copied out to the user pointer by the syscall layer.
    pub fn ioctl(&self, cmd: u32, arg: Option<u32>) -> KResult<Option<Vec<u8>>> {
        match cmd {
            // struct termios (asm-generic, 60 bytes): zeroed is good enough for isatty().
            TCGETS => Ok(Some(alloc::vec![0u8; 60])),
            TCSETS | TCSETSW | TCSETSF => Ok(None), // accepted, ignored (always cooked)
            TIOCGWINSZ => Ok(Some(
                [self.rows, self.cols, 0, 0]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            )),
            TIOCGPGRP => Ok(Some(self.foreground().to_le_bytes().to_vec())),
            TIOCSPGRP => {
                let pg = arg.ok_or(EINVAL)?;
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

/// Which ioctls read a u32 argument from user memory first.
pub fn ioctl_reads_arg(cmd: u32) -> bool {
    cmd == TIOCSPGRP
}

impl State {
    /// One input byte. Returns a signal to send to the foreground group, if any.
    fn input(&mut self, b: u8, echo: &mut Vec<u8>) -> Option<u32> {
        if self.esc == 1 {
            self.esc = if b == b'[' { 2 } else { 0 };
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
            0x03 | 0x1c => {
                // ^C, ^\: flush, signal the foreground group.
                echo.extend_from_slice(if b == 0x03 { b"^C\n" } else { b"^\\\n" });
                self.line.clear();
                self.ready.clear();
                return Some(if b == 0x03 {
                    signal::SIGINT
                } else {
                    signal::SIGQUIT
                });
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
    sync::without_irqs(|| {
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
            RX_WQ.wait_until(|| uart::available() > 0);
            let mut v = Vec::new();
            while let Some(b) = uart::read_byte() {
                v.push(b);
            }
            CONSOLE.input(&v);
        }
    }));
}
