//! Stub DNS resolver (RFC 1035, A records only), thread context, blocking with a timeout.
//!
//! Safety policy (deliberate, see docs/networking.md): this resolver must never cause a
//! lookup on the real internet. So:
//! - `localhost` and `*.localhost` resolve to 127.0.0.1 locally, dotted quads parse locally;
//! - anything else must end in `.test` (reserved, RFC 6761) or `EPERM`;
//! - queries go only to a server set with [`set_server`], which must be on our own subnet
//!   or loopback (in practice a test server on the Mac reached as 10.0.2.2:PORT). The DHCP
//!   DNS server (slirp's 10.0.2.3, which forwards to the host's real resolver) is never used.

use super::errno::*;
use super::socket::{self, POLLIN, SockType};
use super::wire::{Ipv4, SockAddr, be16, put16};
use crate::fs::errno::Result;
use crate::sync::SpinLock;

static SERVER: SpinLock<Option<SockAddr>> = SpinLock::new(None);

/// Configure the resolver's server. Must be loopback or on eth0's subnet (`EPERM`).
pub fn set_server(addr: SockAddr) -> Result<()> {
    let cfg = super::config();
    let ok = addr.ip.is_loopback()
        || (cfg.configured() && addr.ip.same_net(cfg.addr, cfg.mask) && addr.ip != cfg.addr);
    if !ok || addr.port == 0 {
        return Err(EPERM);
    }
    *SERVER.lock() = Some(addr);
    Ok(())
}

pub fn server() -> Option<SockAddr> {
    *SERVER.lock()
}

fn allowed_name(name: &str) -> bool {
    let n = name.trim_end_matches('.');
    n.len() <= 253
        && n.ends_with(".test")
        && n.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}

/// Resolve `name` to an IPv4 address, waiting at most `timeout_ms` (2 tries).
/// `ENOENT`: no such name (NXDOMAIN / no A record). `ETIMEDOUT`: no answer.
/// `EPERM`: name not allowed by the policy above. `EADDRNOTAVAIL`: no server configured.
pub fn resolve(name: &str, timeout_ms: u64) -> Result<Ipv4> {
    let lower = name.trim_end_matches('.');
    if lower.eq_ignore_ascii_case("localhost") || lower.to_ascii_lowercase().ends_with(".localhost")
    {
        return Ok(Ipv4::LOCALHOST);
    }
    if let Some(ip) = Ipv4::parse(lower) {
        return Ok(ip);
    }
    if !allowed_name(&lower.to_ascii_lowercase()) {
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
