//! The console tty: fd 0/1/2 of every process started by the kernel, on the PL011 UART.
//!
//! Output goes through the same sink as `kprintln!` (UART + screen console). Input is
//! line-buffered ("cooked") in the kernel like a Unix tty: echo, backspace, ^U kills the
//! line, ^C drops it, ^D at the start of a line is end-of-file, Enter delivers the line.
//! Readers sleep on a wait queue that the UART RX interrupt kicks.
//!
//! MERGE NOTE (wm): a Terminal window can be a second implementation of this (same line
//! discipline, its own input queue fed by the keyboard driver, output drawn in the window).

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use super::errno::*;
use super::fs::{File, FileType, Stat};
use crate::sched::{Mutex, WaitQueue};
use crate::{sync, uart};

pub struct Console;

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

impl File for Console {
    fn read(&self, buf: &mut [u8]) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut tty = TTY.lock();
        loop {
            if !tty.ready.is_empty() {
                let n = buf.len().min(tty.ready.len());
                for (i, b) in tty.ready.drain(..n).enumerate() {
                    buf[i] = b;
                }
                return Ok(n);
            }
            if tty.eof {
                tty.eof = false;
                return Ok(0);
            }
            RX_WQ.wait_until(|| uart::available() > 0);
            let mut echo = Vec::new();
            while let Some(b) = uart::read_byte() {
                tty.input(b, &mut echo);
            }
            if !echo.is_empty() {
                write_bytes(&echo);
            }
        }
    }

    fn write(&self, buf: &[u8]) -> KResult<usize> {
        write_bytes(buf);
        Ok(buf.len())
    }

    fn stat(&self) -> KResult<Stat> {
        Ok(Stat {
            ino: 4,
            kind: FileType::CharDevice,
            perm: 0o620,
            size: 0,
            rdev: (5 << 8) | 1, // like Linux's /dev/console (5, 1)
        })
    }

    fn is_tty(&self) -> bool {
        true
    }
}
