//! TLS 1.3 for user programs: the shared `tls` crate (`<repo>/tls`, also in the kernel)
//! over rt sockets, with getrandom(2) and the realtime clock. See docs/tls.md.
//!
//! ```ignore
//! let mut s = rt::tls::connect("example.org", 443)?;   // DNS, TCP, handshake, verified
//! s.write_all(b"GET / HTTP/1.1\r\nHost: example.org\r\n\r\n").map_err(|e| e.to_string())?;
//! ```

use alloc::string::{String, ToString};
use alloc::sync::Arc;

pub use ::tls::*;

use crate::net;
use crate::sys;

/// A connected TCP socket as a TLS transport (blocking; closed on drop).
pub struct FdTransport {
    pub fd: i32,
}

impl Drop for FdTransport {
    fn drop(&mut self) {
        let _ = sys::close(self.fd);
    }
}

const EINTR: i32 = 4;
const EAGAIN: i32 = 11;

impl Transport for FdTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        loop {
            match net::recv(self.fd, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.0 == EINTR => {}
                Err(e) if e.0 == EAGAIN => return Err(IoError::TimedOut),
                Err(e) => return Err(IoError::Failed(String::from(e.name()))),
            }
        }
    }

    fn write_all(&mut self, mut data: &[u8]) -> Result<(), IoError> {
        while !data.is_empty() {
            match net::send(self.fd, data) {
                Ok(0) => return Err(IoError::Failed(String::from("connection closed"))),
                Ok(n) => data = &data[n.min(data.len())..],
                Err(e) if e.0 == EINTR => {}
                Err(e) => return Err(IoError::Failed(String::from(e.name()))),
            }
        }
        Ok(())
    }
}

/// getrandom(2), all of `out`. A program that can't get random numbers must not carry on
/// with TLS, so it exits.
pub fn getrandom(out: &mut [u8]) {
    let mut at = 0;
    while at < out.len() {
        match sys::getrandom(&mut out[at..]) {
            Ok(n) if n > 0 => at += n,
            Err(e) if e.0 == EINTR => {}
            _ => {
                crate::eprintln!("tls: getrandom failed");
                sys::exit(1);
            }
        }
    }
}

/// The realtime clock in Unix seconds (None before the RTC was read).
pub fn unix_seconds() -> Option<u64> {
    let s = sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000;
    (s > 0).then_some(s)
}

/// A client config with these roots and the process's random numbers and clock.
pub fn config(roots: Arc<RootStore>) -> Config {
    Config::new(roots, Arc::new(unix_seconds), Arc::new(getrandom))
}

/// The system root store: the compiled-in Mozilla roots, plus a test boot's test root
/// (`/proc/net/tls` has it only when a test script passed one to the kernel through fw_cfg).
/// Parsed on each call: build a client once.
pub fn system_roots() -> RootStore {
    let mut s = RootStore::mozilla();
    if let Ok(t) = sys::read_file("/proc/net/tls") {
        let t = String::from_utf8_lossy(&t);
        if let Some(b) = t.lines().find_map(|l| l.strip_prefix("test_root="))
            && let Some(der) = base64(b.trim())
        {
            let _ = s.add_der(&der);
        }
    }
    s
}

/// Standard base64 (no whitespace) to bytes.
pub fn base64(text: &str) -> Option<alloc::vec::Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let chars: alloc::vec::Vec<u8> =
        text.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    let mut out = alloc::vec::Vec::with_capacity(chars.len() * 3 / 4);
    for chunk in chars.chunks(4) {
        let mut acc = 0u32;
        for &c in chunk {
            acc = (acc << 6) | val(c)?;
        }
        match chunk.len() {
            4 => out.extend_from_slice(&acc.to_be_bytes()[1..]),
            3 => out.extend_from_slice(&(acc << 6).to_be_bytes()[1..3]),
            2 => out.push((acc >> 4) as u8),
            _ => return None,
        }
    }
    Some(out)
}

/// A client with the system roots.
pub fn client() -> Client {
    Client::new(config(Arc::new(system_roots())))
}

/// Resolve `host`, connect to `port`, and run the handshake (the certificate must name
/// `host`). Errors are sentences for the user.
pub fn connect(host: &str, port: u16) -> Result<Stream<FdTransport>, String> {
    connect_with(&client(), host, port)
}

pub fn connect_with(client: &Client, host: &str, port: u16) -> Result<Stream<FdTransport>, String> {
    let addrs = net::lookup_all(host, port, net::Want::Any)?;
    let (fd, _) = net::connect_any(&addrs, net::SOCK_STREAM)?;
    let _ = net::set_recv_timeout(fd, 15_000);
    client.connect(host, FdTransport { fd }).map_err(|e| e.to_string())
}
