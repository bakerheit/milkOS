//! The HTTP/1.1 client (milkweb, shared with the kernel's Browser) over user sockets:
//! [`RtNet`] looks a host up ([`crate::net`]: numeric, localhost, NAME.local, else the
//! kernel's resolver under the boot's internet policy) and connects with a time limit;
//! [`get`] fetches a page, following redirects and undoing gzip / deflate.
//!
//! ```ignore
//! let mut n = rt::web::RtNet::new(rt::net::Want::Any);
//! let url = rt::web::Url::parse("http://example.com/")?;
//! let page = rt::web::get(&mut n, &url, &rt::web::Options::default())?;
//! ```
//!
//! `https://` gives [`Error::SecureUnsupported`] until there is TLS: then `RtNet::has_tls`
//! says true and `secure` wraps the stream (`tls::Client::connect(host, stream)`).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use milkweb::http::Net;
pub use milkweb::http::{Error, Failure, Options, Response, Transport, USER_AGENT};
pub use milkweb::url::Url;

use crate::net::{self, Ip, POLLIN, POLLOUT, PollFd, SockAddr, Want};
use crate::sys::{self, Errno};

/// Nothing received for this long: a read gives up.
pub const IDLE_MS: u64 = 15_000;

pub struct RtNet {
    pub want: Want,
    /// The address the last connection went to (for "... from ADDR" lines).
    pub peer: Option<SockAddr>,
    pub idle_ms: u64,
}

impl RtNet {
    pub fn new(want: Want) -> RtNet {
        RtNet {
            want,
            peer: None,
            idle_ms: IDLE_MS,
        }
    }
}

const SOCK_NONBLOCK: usize = 0o4000;

fn lookup(host: &str, want: Want) -> Result<alloc::vec::Vec<SockAddr>, Error> {
    let local = host == "localhost"
        || net::SockAddr::parse(host, 0).is_some()
        || crate::mdns::is_local(host);
    if local {
        return net::resolve_all(host, want).map_err(Error::NoDns);
    }
    let ips: alloc::vec::Vec<Ip> = net::kernel_resolve(host, want).map_err(|e| match e.0 {
        1 => Error::Offline(String::from(host)),
        2 | 61 => Error::NotFound(String::from(host)),
        22 => Error::Url(milkweb::url::UrlError::BadHost(String::from(host))),
        _ => Error::NoDns(net::lookup_words(host, e)),
    })?;
    Ok(ips.into_iter().map(|ip| SockAddr::new(ip, 0)).collect())
}

fn wait(fd: i32, events: u16, ms: u64) -> bool {
    let mut p = [PollFd {
        fd,
        events,
        revents: 0,
    }];
    matches!(net::poll(&mut p, Some(ms)), Ok(n) if n > 0)
}

/// Connect `a` within `ms` (a non-blocking socket, then poll).
pub fn connect_timeout(a: &SockAddr, ms: u64) -> Result<i32, Errno> {
    let fam = if a.ip.is_v6() {
        net::AF_INET6
    } else {
        net::AF_INET
    };
    let fd = net::socket_family(fam, net::SOCK_STREAM | SOCK_NONBLOCK, 0)?;
    let r = match net::connect(fd, a) {
        Ok(()) => Ok(fd),
        Err(e) if e.0 == 115 || e.0 == 114 => {
            if !wait(fd, POLLOUT, ms) {
                Err(Errno(110))
            } else {
                match net::connect(fd, a) {
                    Ok(()) => Ok(fd),
                    Err(e) if e.0 == 106 => Ok(fd), // EISCONN: it is
                    Err(e) => Err(e),
                }
            }
        }
        Err(e) => Err(e),
    };
    if r.is_err() {
        let _ = sys::close(fd);
    }
    r
}

impl Net for RtNet {
    fn connect(
        &mut self,
        host: &str,
        port: u16,
        timeout_ms: u64,
    ) -> Result<Box<dyn Transport>, Error> {
        let addrs = lookup(host, self.want)?;
        let mut last = Error::Connect(format!("{host}: no address"));
        for mut a in addrs {
            a.port = port;
            match connect_timeout(&a, timeout_ms) {
                Ok(fd) => {
                    self.peer = Some(a);
                    return Ok(Box::new(Stream {
                        fd,
                        idle_ms: self.idle_ms,
                    }));
                }
                Err(e) => last = Error::Connect(format!("connect {a}: {e}")),
            }
        }
        Err(last)
    }

    fn now_ms(&mut self) -> u64 {
        sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
    }
}

/// A connected, non-blocking TCP socket; closed when dropped.
pub struct Stream {
    fd: i32,
    idle_ms: u64,
}

impl Transport for Stream {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        loop {
            match net::recv(self.fd, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.0 == 11 => {
                    if !wait(self.fd, POLLIN, self.idle_ms) {
                        return Err(Error::Timeout);
                    }
                }
                Err(e) => return Err(Error::Io(format!("the answer broke off: {e}"))),
            }
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        loop {
            match net::send(self.fd, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.0 == 11 => {
                    if !wait(self.fd, POLLOUT, self.idle_ms) {
                        return Err(Error::Timeout);
                    }
                }
                Err(e) => return Err(Error::Io(format!("couldn't send the request: {e}"))),
            }
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = sys::close(self.fd);
    }
}

/// GET `url` (redirects followed, bodies decoded).
pub fn get(n: &mut RtNet, url: &Url, o: &Options) -> Result<Response, Failure> {
    milkweb::http::get(n, url, o)
}
