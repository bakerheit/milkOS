//! IPv4 socket wrappers (Linux aarch64 numbers; milkOS: AF_INET, TCP and UDP).

use crate::sys::{Errno, Result, syscall};
use core::fmt;

pub const AF_INET: usize = 2;
pub const SOCK_STREAM: usize = 1;
pub const SOCK_DGRAM: usize = 2;

mod nr {
    pub const SOCKET: usize = 198;
    pub const BIND: usize = 200;
    pub const LISTEN: usize = 201;
    pub const ACCEPT: usize = 202;
    pub const CONNECT: usize = 203;
    pub const SENDTO: usize = 206;
    pub const RECVFROM: usize = 207;
    pub const SHUTDOWN: usize = 210;
}

fn sc(n: usize, a: [usize; 6]) -> Result<usize> {
    // SAFETY: the wrappers below pass valid pointers and lengths.
    let r = unsafe { syscall(n, a) };
    if (-4095..0).contains(&r) {
        Err(Errno(-r as i32))
    } else {
        Ok(r as usize)
    }
}

/// An IPv4 address and port.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SockAddr {
    pub ip: [u8; 4],
    pub port: u16,
}

impl SockAddr {
    /// Parse "a.b.c.d:port" (or "a.b.c.d" with `default_port`). "localhost" is 127.0.0.1.
    pub fn parse(s: &str, default_port: u16) -> Option<SockAddr> {
        let (host, port) = match s.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (s, default_port),
        };
        if host == "localhost" {
            return Some(SockAddr {
                ip: [127, 0, 0, 1],
                port,
            });
        }
        let mut ip = [0u8; 4];
        let mut it = host.split('.');
        for b in ip.iter_mut() {
            *b = it.next()?.parse().ok()?;
        }
        if it.next().is_some() {
            return None;
        }
        Some(SockAddr { ip, port })
    }

    fn raw(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
        b[2..4].copy_from_slice(&self.port.to_be_bytes());
        b[4..8].copy_from_slice(&self.ip);
        b
    }
}

impl fmt::Display for SockAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.ip;
        write!(f, "{a}.{b}.{c}.{d}:{}", self.port)
    }
}

pub fn socket(ty: usize) -> Result<i32> {
    sc(nr::SOCKET, [AF_INET, ty, 0, 0, 0, 0]).map(|fd| fd as i32)
}

pub fn connect(fd: i32, a: &SockAddr) -> Result<()> {
    let raw = a.raw();
    sc(
        nr::CONNECT,
        [fd as usize, raw.as_ptr() as usize, 16, 0, 0, 0],
    )
    .map(|_| ())
}

pub fn bind(fd: i32, a: &SockAddr) -> Result<()> {
    let raw = a.raw();
    sc(nr::BIND, [fd as usize, raw.as_ptr() as usize, 16, 0, 0, 0]).map(|_| ())
}

pub fn listen(fd: i32, backlog: usize) -> Result<()> {
    sc(nr::LISTEN, [fd as usize, backlog, 0, 0, 0, 0]).map(|_| ())
}

/// Accept a connection: (fd, peer).
pub fn accept(fd: i32) -> Result<(i32, SockAddr)> {
    let mut raw = [0u8; 16];
    let mut len: u32 = 16;
    let nfd = sc(
        nr::ACCEPT,
        [
            fd as usize,
            raw.as_mut_ptr() as usize,
            &mut len as *mut u32 as usize,
            0,
            0,
            0,
        ],
    )?;
    let peer = SockAddr {
        ip: [raw[4], raw[5], raw[6], raw[7]],
        port: u16::from_be_bytes([raw[2], raw[3]]),
    };
    Ok((nfd as i32, peer))
}

pub fn send(fd: i32, buf: &[u8]) -> Result<usize> {
    sc(
        nr::SENDTO,
        [fd as usize, buf.as_ptr() as usize, buf.len(), 0, 0, 0],
    )
}

pub fn recv(fd: i32, buf: &mut [u8]) -> Result<usize> {
    sc(
        nr::RECVFROM,
        [fd as usize, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0],
    )
}

/// shutdown: 0 read, 1 write, 2 both.
pub fn shutdown(fd: i32, how: usize) -> Result<()> {
    sc(nr::SHUTDOWN, [fd as usize, how, 0, 0, 0, 0]).map(|_| ())
}
