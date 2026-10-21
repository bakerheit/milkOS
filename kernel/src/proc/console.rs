//! The console tty behind `/dev/console` (fd 0/1/2 of every process the kernel starts):
//! the input side. devfs's console node calls [`read`] and [`ioctl`]; writes stay in devfs
//! (straight to the `kprintln!` sink).
//!
//! Input is line-buffered ("cooked") like a Unix tty: echo, backspace, ^U kills the line,
//! ^C drops it, ^D at the start of a line is end-of-file, Enter delivers the line. Readers
//! sleep on a wait queue that the UART RX interrupt kicks. Before the scheduler is up there
//! is nobody to wait for, so `read` is non-blocking then (EAGAIN when empty), which is
//! what the fs boot self-test expects.
//!
//! MERGE NOTE (wm): a Terminal window can be a second instance of this (same line
//! discipline, its own input queue fed by the keyboard driver, output drawn in the window).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use super::errno::*;
use crate::sched::{self, Mutex, WaitQueue};
use crate::{sync, uart};

struct Tty {
    line: Vec<u8>,
    ready: VecDeque<u8>,
    eof: bool,
    /// Inside an ESC [ ... sequence (arrow keys etc.), which we swallow.
    esc: u8,
}

static TTY: Mutex<Tty> = Mutex::new(Tty {
    line: Vec::new(),
    ready: VecDeque::new(),
    eof: false,
    esc: 0,
});
static RX_WQ: WaitQueue = WaitQueue::new();

fn on_rx() {
    RX_WQ.wake_all();
}

pub fn init() {
    uart::set_rx_hook(on_rx);
}

/// Write bytes to the console (UART + screen). Invalid UTF-8 is replaced.
pub fn write_bytes(b: &[u8]) {
    let s = String::from_utf8_lossy(b);
    sync::without_irqs(|| {
        let _ = crate::console_writer().write_str(&s);
    });
}

impl Tty {
    fn input(&mut self, b: u8, echo: &mut Vec<u8>) {
        if self.esc == 1 {
            self.esc = if b == b'[' { 2 } else { 0 };
            return;
        }
        if self.esc == 2 {
            if (0x40..=0x7e).contains(&b) {
                self.esc = 0; // final byte
            }
            return;
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
            0x03 => {
                // ^C: no signals yet; drop the line and hand the reader an empty one.
                echo.extend_from_slice(b"^C\n");
                self.line.clear();
                self.ready.push_back(b'\n');
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
    }
}

/// Blocking cooked read (see the module docs).
pub fn read(buf: &mut [u8]) -> KResult<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let mut tty = TTY.lock();
    loop {
        if !tty.ready.is_empty() {
            // Canonical mode: at most one line per read.
            let line_end = tty
                .ready
                .iter()
                .position(|&b| b == b'\n')
                .map_or(usize::MAX, |i| i + 1);
            let n = buf.len().min(tty.ready.len()).min(line_end);
            for (i, b) in tty.ready.drain(..n).enumerate() {
                buf[i] = b;
            }
            return Ok(n);
        }
        if tty.eof {
            tty.eof = false;
            return Ok(0);
        }
        if uart::available() == 0 {
            if !sched::started() {
                return Err(EAGAIN);
            }
            RX_WQ.wait_until(|| uart::available() > 0);
        }
        let mut echo = Vec::new();
        while let Some(b) = uart::read_byte() {
            tty.input(b, &mut echo);
        }
        if !echo.is_empty() {
            write_bytes(&echo);
        }
    }
}

/// For poll(2): would a read return without blocking (a line or ^D is waiting)? Feeds
/// pending UART bytes through the line discipline first (echoing them, as a read would).
/// If another thread is in `read` right now, that reader gets the line: not readable.
pub fn readable() -> bool {
    let Some(mut tty) = TTY.try_lock() else {
        return false;
    };
    let mut echo = Vec::new();
    while let Some(b) = uart::read_byte() {
        tty.input(b, &mut echo);
    }
    if !echo.is_empty() {
        write_bytes(&echo);
    }
    !tty.ready.is_empty() || tty.eof
}

const TCGETS: u32 = 0x5401;
const TIOCGWINSZ: u32 = 0x5413;
const TIOCGPGRP: u32 = 0x540f;

/// The tty ioctls libc's `isatty` and friends use. Results that need user memory are
/// returned as bytes for the syscall layer to copy out.
pub fn ioctl(cmd: u32) -> KResult<Vec<u8>> {
    match cmd {
        // struct termios (asm-generic, 60 bytes): zeroed is good enough for isatty().
        TCGETS => Ok(alloc::vec![0u8; 60]),
        // struct winsize { rows, cols, xpixel, ypixel }: u16 each.
        TIOCGWINSZ => Ok([25u16, 80, 0, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect()),
        TIOCGPGRP => Ok(1u32.to_le_bytes().to_vec()),
        _ => Err(ENOTTY),
    }
}
