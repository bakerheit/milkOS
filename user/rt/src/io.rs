//! Buffered output and line input for the tools.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::sys;

/// Buffered writer on an fd. Call `flush` before exiting (exit doesn't run destructors).
pub struct Out {
    fd: i32,
    buf: Vec<u8>,
    /// Write failed (e.g. EPIPE): further output is dropped.
    pub broken: bool,
}

impl Out {
    pub fn new(fd: i32) -> Out {
        Out {
            fd,
            buf: Vec::with_capacity(4096),
            broken: false,
        }
    }
    pub fn stdout() -> Out {
        Out::new(1)
    }
    pub fn write(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= 4096 {
            self.flush();
        }
    }
    pub fn line(&mut self, s: &str) {
        self.write(s.as_bytes());
        self.write(b"\n");
    }
    pub fn flush(&mut self) {
        if !self.buf.is_empty() && !self.broken && sys::write_all(self.fd, &self.buf).is_err() {
            self.broken = true;
        }
        self.buf.clear();
    }
}

impl fmt::Write for Out {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write(s.as_bytes());
        Ok(())
    }
}

impl Drop for Out {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Reads an fd line by line (lines without the newline).
pub struct Lines {
    fd: i32,
    buf: Vec<u8>,
    start: usize,
    eof: bool,
}

impl Lines {
    pub fn new(fd: i32) -> Lines {
        Lines {
            fd,
            buf: Vec::new(),
            start: 0,
            eof: false,
        }
    }

    /// Next line as bytes; None at the end.
    pub fn next_bytes(&mut self) -> Option<Vec<u8>> {
        loop {
            if let Some(i) = self.buf[self.start..].iter().position(|&b| b == b'\n') {
                let l = self.buf[self.start..self.start + i].to_vec();
                self.start += i + 1;
                return Some(l);
            }
            if self.eof {
                if self.start < self.buf.len() {
                    let l = self.buf[self.start..].to_vec();
                    self.start = self.buf.len();
                    return Some(l);
                }
                return None;
            }
            if self.start > 0 {
                self.buf.drain(..self.start);
                self.start = 0;
            }
            let mut chunk = [0u8; 4096];
            match sys::read(self.fd, &mut chunk) {
                Ok(0) | Err(_) => self.eof = true,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
            }
        }
    }
}

impl Iterator for Lines {
    type Item = String;
    fn next(&mut self) -> Option<String> {
        self.next_bytes()
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }
}

/// Open each path ("-" = stdin) in turn; reports errors as `prog: path: error` on stderr.
/// Calls `f(fd, name)`. Returns false if any could not be opened.
pub fn each_input(prog: &str, paths: &[&str], mut f: impl FnMut(i32, &str)) -> bool {
    let mut ok = true;
    let stdin = ["-"];
    let paths = if paths.is_empty() { &stdin[..] } else { paths };
    for &p in paths {
        if p == "-" {
            f(0, "-");
            continue;
        }
        match sys::open(p, sys::O_RDONLY | sys::O_CLOEXEC) {
            Ok(fd) => {
                f(fd, p);
                let _ = sys::close(fd);
            }
            Err(e) => {
                crate::eprintln!("{prog}: {p}: {e}");
                ok = false;
            }
        }
    }
    ok
}

/// Split argv into flags (each "-xyz" contributes x, y, z; "--" ends flags) and operands.
pub fn flags(args: &[&'static str]) -> (Vec<char>, Vec<&'static str>) {
    let mut f = Vec::new();
    let mut rest = Vec::new();
    let mut done = false;
    for &a in args {
        if !done && a == "--" {
            done = true;
        } else if !done
            && a.len() > 1
            && a.starts_with('-')
            && !a[1..].starts_with(|c: char| c.is_ascii_digit())
        {
            f.extend(a[1..].chars());
        } else {
            rest.push(a);
        }
    }
    (f, rest)
}
