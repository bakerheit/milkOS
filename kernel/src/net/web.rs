//! milkweb's HTTP client over kernel sockets: [`KernelNet`] looks a host up (under the
//! boot's policy: offline, numeric addresses and localhost only) and connects, and
//! [`Stream`] is a TCP connection as a `milkweb::http::Transport`. Thread context only (it
//! sleeps on the socket). Used by the Browser; `rt::web` is the same for user space.
//!
//! The `https://` hook: `KernelNet::has_tls` is false, so the client refuses such addresses
//! before connecting. With the `tls` crate, `has_tls` returns true and `secure` wraps the
//! `Stream` in `tls::Client::connect(host, stream)`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use milkweb::dns::Want;
use milkweb::http::{Error, Net, Transport};

use super::errno::*;
use super::socket::{self, Family, POLLIN, POLLOUT, SockType, SocketId};
use super::wire::SockAddr;
use super::{dns, internet};
use crate::fs::errno::Errno;

/// Nothing received for this long: the read gives up.
pub const IDLE_MS: u64 = 15_000;

pub struct KernelNet {
    /// The address the last connection went to.
    pub peer: Option<SockAddr>,
}

impl KernelNet {
    pub fn new() -> KernelNet {
        KernelNet { peer: None }
    }
}

fn why(e: Errno) -> &'static str {
    match e {
        e if e == ECONNREFUSED => "connection refused",
        e if e == ETIMEDOUT => "timed out",
        e if e == ENETUNREACH => "no route (no internet?)",
        e if e == EHOSTUNREACH => "host unreachable",
        e if e == ECONNRESET => "connection reset",
        e => crate::net::errno::name(e),
    }
}

/// A name-lookup error as the client's error.
pub fn lookup_error(host: &str, e: Errno) -> Error {
    match e {
        e if e == EPERM => Error::Offline(String::from(host)),
        e if e == ENOENT => Error::NotFound(String::from(host)),
        e if e == dns::ENODATA => Error::NotFound(String::from(host)),
        e if e == EADDRNOTAVAIL => Error::NoDns(format!(
            "{host}: there's no DNS server yet (is the network up? Settings > Network)"
        )),
        e if e == ETIMEDOUT => Error::NoDns(format!(
            "{host}: the name server didn't answer (no internet?)"
        )),
        e if e == EINVAL => Error::Url(milkweb::url::UrlError::BadHost(String::from(host))),
        e => Error::NoDns(format!("{host}: the lookup failed ({})", why(e))),
    }
}

fn connect_one(addr: SockAddr, timeout_ms: u64) -> Result<SocketId, Errno> {
    let fam = if addr.ip.is_v6() {
        Family::V6
    } else {
        Family::V4
    };
    let s = socket::socket_family(fam, SockType::Stream)?;
    let deadline = crate::timer::now_ns() + timeout_ms * 1_000_000;
    loop {
        match socket::connect(s, addr) {
            Ok(()) => return Ok(s),
            Err(e) if e == EINPROGRESS || e == EALREADY => {
                let left = deadline.saturating_sub(crate::timer::now_ns());
                if left == 0 || socket::wait(s, POLLOUT, left) == 0 {
                    let _ = socket::close(s);
                    return Err(ETIMEDOUT);
                }
            }
            Err(e) => {
                let _ = socket::close(s);
                return Err(e);
            }
        }
    }
}

impl Net for KernelNet {
    fn connect(
        &mut self,
        host: &str,
        port: u16,
        timeout_ms: u64,
    ) -> Result<Box<dyn Transport>, Error> {
        // Offline, the Browser keeps to numeric addresses and localhost (not even .test).
        let addrs = match internet::literal(host) {
            Some(a) => alloc::vec![a],
            None if !internet::online() => return Err(Error::Offline(String::from(host))),
            None => dns::resolve_all(host, Want::Any, 4000).map_err(|e| lookup_error(host, e))?,
        };
        let mut last = Error::Connect(format!("{host}: no address"));
        for ip in addrs {
            let addr = SockAddr::new(ip, port);
            match connect_one(addr, timeout_ms) {
                Ok(s) => {
                    self.peer = Some(addr);
                    return Ok(Box::new(Stream { s }));
                }
                Err(e) => last = Error::Connect(format!("can't connect to {addr}: {}", why(e))),
            }
        }
        Err(last)
    }

    fn now_ms(&mut self) -> u64 {
        crate::timer::now_ns() / 1_000_000
    }
}

/// A connected TCP socket; closed when dropped.
pub struct Stream {
    s: SocketId,
}

impl Transport for Stream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        loop {
            match socket::recv(self.s, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e == EAGAIN => {
                    if socket::wait(self.s, POLLIN, IDLE_MS * 1_000_000) == 0 {
                        return Err(Error::Timeout);
                    }
                }
                Err(e) => return Err(Error::Io(format!("the connection broke ({})", why(e)))),
            }
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        loop {
            match socket::send(self.s, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e == EAGAIN => {
                    if socket::wait(self.s, POLLOUT, IDLE_MS * 1_000_000) == 0 {
                        return Err(Error::Timeout);
                    }
                }
                Err(e) => return Err(Error::Io(format!("couldn't send ({})", why(e)))),
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = socket::close(self.s);
    }
}
