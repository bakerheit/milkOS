//! The DNS stub resolver (thread context, blocking with a timeout), under the boot's
//! internet policy ([`super::internet`]).
//!
//! Offline (every test boot; the policy as it always was, see docs/networking.md):
//! - `localhost` and `*.localhost` resolve to 127.0.0.1 locally, numeric addresses parse
//!   locally;
//! - anything else must end in `.test` (reserved, RFC 6761) or `EPERM`;
//! - queries go only to a server set with [`set_server`], which must be on our own subnet
//!   or loopback (in practice a test server on the Mac reached as 10.0.2.2:PORT). The DHCP
//!   DNS server (slirp's 10.0.2.3, which forwards to the host's real resolver) is never used.
//!
//! Online (`internet=on`: `scripts/run.sh`, `milkOS.command`): any name, A and AAAA, through
//! the DHCP-provided server (or the test override), with CNAME chains followed, TCP for a
//! truncated answer and a cache that keeps answers for their TTL. The message handling and
//! the lookup logic are milkweb's (`milkweb::dns`, host-tested); this file sends the packets.
//!
//! [`resolve_all`] is the one entry point (the Browser, `MILK_RESOLVE` for user space).

use alloc::string::String;
use alloc::vec::Vec;

use milkweb::dns::{self as wdns, Cache, Cached, DnsError, Want};

use super::errno::*;
use super::internet;
use super::socket::{self, POLLIN, POLLOUT, SockType};
use super::wire::{IpAddr, Ipv4, Ipv6, SockAddr, be16, put16};
use crate::fs::errno::{Errno, Result};
use crate::sync::SpinLock;

static SERVER: SpinLock<Option<SockAddr>> = SpinLock::new(None);
static CACHE: SpinLock<Cache> = SpinLock::new(Cache::new(128));

/// ENODATA: the name exists but has no address of the kind asked for.
pub const ENODATA: Errno = Errno(61);
/// ELOOP: a CNAME chain that goes round.
pub const ELOOP: Errno = Errno(40);
/// EIO: the server failed or sent something unreadable.
pub const EIO: Errno = Errno(5);

/// Configure the offline resolver's server. Must be loopback or on eth0's subnet (`EPERM`).
pub fn set_server(addr: SockAddr) -> Result<()> {
    if !internet::server_ok(addr) {
        return Err(EPERM);
    }
    *SERVER.lock() = Some(addr);
    Ok(())
}

pub fn server() -> Option<SockAddr> {
    *SERVER.lock()
}

/// (entries, hits, misses) of the online cache.
pub fn cache_stats() -> (usize, u64, u64) {
    let c = CACHE.lock();
    (c.len(), c.hits, c.misses)
}

/// Forget every cached answer.
pub fn flush_cache() {
    CACHE.lock().clear();
}

/// What the offline policy lets through to a server: `.test` names only.
pub fn offline_allows(name: &str) -> bool {
    let n = name.trim_end_matches('.').to_ascii_lowercase();
    n.len() <= 253
        && n.ends_with(".test")
        && n.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

fn is_localhost(name: &str) -> bool {
    let n = name.trim_end_matches('.');
    n.eq_ignore_ascii_case("localhost") || n.to_ascii_lowercase().ends_with(".localhost")
}

/// Resolve `name` to an IPv4 address, waiting at most `timeout_ms` (2 tries).
/// `ENOENT`: no such name (NXDOMAIN / no A record). `ETIMEDOUT`: no answer.
/// `EPERM`: name not allowed by the offline policy. `EADDRNOTAVAIL`: no server configured.
/// Online, any name goes to the internet resolver ([`resolve_all`]).
pub fn resolve(name: &str, timeout_ms: u64) -> Result<Ipv4> {
    let lower = name.trim_end_matches('.');
    if is_localhost(lower) {
        return Ok(Ipv4::LOCALHOST);
    }
    if let Some(ip) = Ipv4::parse(lower) {
        return Ok(ip);
    }
    if internet::online() {
        return resolve_all(lower, Want::V4, timeout_ms)?
            .into_iter()
            .find_map(IpAddr::v4)
            .ok_or(ENOENT);
    }
    if !offline_allows(lower) {
        return Err(EPERM);
    }
    let server = server().ok_or(EADDRNOTAVAIL)?;
    let mut q = [0u8; 300];
    let id = (crate::timer::counter() as u16) ^ 0x6d6b;
    let qlen = build_query(&mut q, id, lower);
    let s = socket::socket(SockType::Dgram)?;
    let r = (|| {
        socket::connect(s, server)?;
        for _ in 0..2 {
            socket::send(s, &q[..qlen])?;
            let deadline = crate::timer::now_ns() + timeout_ms * 1_000_000 / 2;
            loop {
                let left = deadline.saturating_sub(crate::timer::now_ns());
                if left == 0 || socket::wait(s, POLLIN, left) == 0 {
                    break;
                }
                let mut buf = [0u8; 512];
                let Ok(n) = socket::recv(s, &mut buf) else {
                    continue;
                };
                if let Some(ans) = parse_answer(&buf[..n], id) {
                    return ans;
                }
            }
        }
        Err(ETIMEDOUT)
    })();
    let _ = socket::close(s);
    r
}

/// Every address of `name` (IPv4 first), under the boot's policy:
/// numeric addresses and localhost locally; offline, `.test` names through [`resolve`]'s
/// server (A only) and `EPERM` for the rest; online, a real lookup.
/// Errors: `ENOENT` no such host, `ENODATA` no address of that kind, `ETIMEDOUT` no answer,
/// `EPERM` refused offline, `EADDRNOTAVAIL` no DNS server, `EINVAL` not a name, `EIO` the
/// server failed, `ELOOP` a CNAME loop.
pub fn resolve_all(name: &str, want: Want, timeout_ms: u64) -> Result<Vec<IpAddr>> {
    let n = name.trim().trim_end_matches('.');
    if is_localhost(n) {
        return Ok(match want {
            Want::V6 => alloc::vec![IpAddr::V6(Ipv6::LOOPBACK)],
            _ => alloc::vec![IpAddr::V4(Ipv4::LOCALHOST)],
        });
    }
    if let Some(a) = internet::literal(n) {
        return match (want, a.is_v6()) {
            (Want::V4, true) | (Want::V6, false) => Err(ENODATA),
            _ => Ok(alloc::vec![a]),
        };
    }
    if !internet::online() {
        if want == Want::V6 {
            return Err(if offline_allows(n) { ENODATA } else { EPERM });
        }
        return resolve(n, timeout_ms).map(|a| alloc::vec![IpAddr::V4(a)]);
    }
    let (server, _) = internet::dns_server().ok_or(EADDRNOTAVAIL)?;
    let mut h = KernelHost {
        server,
        timeout_ms: timeout_ms.max(200),
    };
    match wdns::resolve(&mut h, n, want) {
        Ok(addrs) => Ok(addrs
            .into_iter()
            .map(|a| match a {
                wdns::Addr::V4(b) => IpAddr::V4(Ipv4(b)),
                wdns::Addr::V6(b) => IpAddr::V6(Ipv6(b)),
            })
            .collect()),
        Err(e) => Err(errno_of(&e)),
    }
}

pub fn errno_of(e: &DnsError) -> Errno {
    match e {
        DnsError::BadName => EINVAL,
        DnsError::NotFound => ENOENT,
        DnsError::NoData => ENODATA,
        DnsError::Timeout => ETIMEDOUT,
        DnsError::Server(_) | DnsError::Malformed(_) => EIO,
        DnsError::Io(_) => ENETUNREACH,
        DnsError::CnameLoop => ELOOP,
    }
}

/// The resolver's sending side: one server, a time budget per question.
struct KernelHost {
    server: SockAddr,
    timeout_ms: u64,
}

fn io(e: Errno) -> DnsError {
    if e == ETIMEDOUT {
        DnsError::Timeout
    } else {
        DnsError::Io(String::from(crate::net::errno::name(e)))
    }
}

impl wdns::Host for KernelHost {
    fn udp(
        &mut self,
        q: &[u8],
        ok: &mut dyn FnMut(&[u8]) -> bool,
    ) -> core::result::Result<Vec<u8>, DnsError> {
        let s = socket::socket_family(socket::Family::V4, SockType::Dgram).map_err(io)?;
        let r = (|| {
            socket::connect(s, self.server).map_err(io)?;
            let mut buf = alloc::vec![0u8; 1500];
            // Two tries, half the time each.
            for _ in 0..2 {
                socket::send(s, q).map_err(io)?;
                let deadline = crate::timer::now_ns() + self.timeout_ms * 1_000_000 / 2;
                loop {
                    let left = deadline.saturating_sub(crate::timer::now_ns());
                    if left == 0 || socket::wait(s, POLLIN, left) & POLLIN == 0 {
                        break;
                    }
                    let Ok(n) = socket::recv(s, &mut buf) else {
                        continue;
                    };
                    if ok(&buf[..n]) {
                        return Ok(buf[..n].to_vec());
                    }
                }
                if socket::interrupted() {
                    break;
                }
            }
            Err(DnsError::Timeout)
        })();
        let _ = socket::close(s);
        r
    }

    fn tcp(&mut self, q: &[u8]) -> core::result::Result<Vec<u8>, DnsError> {
        let s = socket::socket_family(socket::Family::V4, SockType::Stream).map_err(io)?;
        let deadline = crate::timer::now_ns() + self.timeout_ms * 1_000_000;
        let left = || deadline.saturating_sub(crate::timer::now_ns());
        let r = (|| {
            loop {
                match socket::connect(s, self.server) {
                    Ok(()) => break,
                    Err(e) if e == EINPROGRESS || e == EALREADY => {
                        if left() == 0 || socket::wait(s, POLLOUT, left()) == 0 {
                            return Err(DnsError::Timeout);
                        }
                    }
                    Err(e) => return Err(io(e)),
                }
            }
            let mut msg = Vec::with_capacity(q.len() + 2);
            msg.extend_from_slice(&(q.len() as u16).to_be_bytes());
            msg.extend_from_slice(q);
            let mut sent = 0;
            while sent < msg.len() {
                match socket::send(s, &msg[sent..]) {
                    Ok(n) => sent += n,
                    Err(e) if e == EAGAIN => {
                        if left() == 0 || socket::wait(s, POLLOUT, left()) == 0 {
                            return Err(DnsError::Timeout);
                        }
                    }
                    Err(e) => return Err(io(e)),
                }
            }
            let mut got = Vec::new();
            let mut buf = [0u8; 2048];
            loop {
                if got.len() >= 2 && got.len() >= 2 + be16(&got, 0) as usize {
                    let n = be16(&got, 0) as usize;
                    return Ok(got[2..2 + n].to_vec());
                }
                match socket::recv(s, &mut buf) {
                    Ok(0) => return Err(DnsError::Malformed("the TCP answer was cut off")),
                    Ok(n) => {
                        got.extend_from_slice(&buf[..n]);
                        if got.len() > 70_000 {
                            return Err(DnsError::Malformed("a TCP answer that won't end"));
                        }
                    }
                    Err(e) if e == EAGAIN => {
                        if left() == 0 || socket::wait(s, POLLIN, left()) == 0 {
                            return Err(DnsError::Timeout);
                        }
                    }
                    Err(e) => return Err(io(e)),
                }
            }
        })();
        let _ = socket::close(s);
        r
    }

    fn now_secs(&mut self) -> u64 {
        crate::timer::now_ns() / 1_000_000_000
    }

    fn new_id(&mut self) -> u16 {
        crate::crypto::rng::next_u64() as u16
    }

    fn cache_get(&mut self, name: &str, qtype: u16, now: u64) -> Option<Cached> {
        CACHE.lock().get(name, qtype, now)
    }

    fn cache_put(&mut self, name: &str, qtype: u16, value: Cached, ttl: u32, now: u64) {
        CACHE.lock().put(name, qtype, value, ttl, now)
    }
}

fn build_query(q: &mut [u8], id: u16, name: &str) -> usize {
    put16(q, 0, id);
    put16(q, 2, 0x0100); // recursion desired
    put16(q, 4, 1); // one question
    q[6..12].fill(0);
    let mut k = 12;
    for label in name.split('.') {
        q[k] = label.len() as u8;
        q[k + 1..k + 1 + label.len()].copy_from_slice(label.as_bytes());
        k += 1 + label.len();
    }
    q[k] = 0;
    put16(q, k + 1, 1); // QTYPE A
    put16(q, k + 3, 1); // QCLASS IN
    k + 5
}

/// Skip a (possibly compressed) name at `k`; returns the offset after it.
fn skip_name(m: &[u8], mut k: usize) -> Option<usize> {
    loop {
        let len = *m.get(k)? as usize;
        if len == 0 {
            return Some(k + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(k + 2);
        }
        k += 1 + len;
    }
}

/// None: not our answer (wrong id / malformed), keep waiting.
fn parse_answer(m: &[u8], id: u16) -> Option<Result<Ipv4>> {
    if m.len() < 12 || be16(m, 0) != id || m[2] & 0x80 == 0 {
        return None;
    }
    let rcode = m[3] & 0x0f;
    if rcode == 3 {
        return Some(Err(ENOENT));
    }
    if rcode != 0 {
        return Some(Err(ETIMEDOUT));
    }
    let qd = be16(m, 4);
    let an = be16(m, 6);
    let mut k = 12;
    for _ in 0..qd {
        k = skip_name(m, k)? + 4;
    }
    for _ in 0..an {
        k = skip_name(m, k)?;
        if m.len() < k + 10 {
            return None;
        }
        let ty = be16(m, k);
        let class = be16(m, k + 2);
        let rdlen = be16(m, k + 8) as usize;
        k += 10;
        if ty == 1 && class == 1 && rdlen == 4 && m.len() >= k + 4 {
            return Some(Ok(Ipv4([m[k], m[k + 1], m[k + 2], m[k + 3]])));
        }
        k += rdlen;
    }
    Some(Err(ENOENT))
}
