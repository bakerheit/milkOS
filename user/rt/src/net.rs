//! Socket wrappers (Linux aarch64 numbers; milkOS: AF_INET and AF_INET6, TCP, UDP and ping
//! sockets). One address type for both families ([`SockAddr`], [`Ip`]); name lookup in the
//! getaddrinfo style ([`lookup_all`]: every address of a name, IPv6 first, as Linux orders
//! them) and a connect that tries them in turn ([`connect_any`]).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::sys::{Errno, Result, syscall};

pub const AF_INET: usize = 2;
pub const AF_INET6: usize = 10;
pub const SOCK_STREAM: usize = 1;
pub const SOCK_DGRAM: usize = 2;

pub const IPPROTO_ICMP: usize = 1;
pub const IPPROTO_ICMPV6: usize = 58;

/// Linux poll bits.
pub const POLLIN: u16 = 0x001;
pub const POLLOUT: u16 = 0x004;
pub const POLLERR: u16 = 0x008;
pub const POLLHUP: u16 = 0x010;
pub const POLLNVAL: u16 = 0x020;

mod nr {
    pub const PPOLL: usize = 73;
    pub const SETSOCKOPT: usize = 208;
    pub const SOCKET: usize = 198;
    pub const BIND: usize = 200;
    pub const LISTEN: usize = 201;
    pub const ACCEPT: usize = 202;
    pub const CONNECT: usize = 203;
    pub const GETSOCKNAME: usize = 204;
    pub const SENDTO: usize = 206;
    pub const RECVFROM: usize = 207;
    pub const SHUTDOWN: usize = 210;
    /// milkOS: look a host name up in the kernel's resolver (kernel/src/proc/sock.rs).
    pub const MILK_RESOLVE: usize = 0x1011;
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

/// An IPv4 or IPv6 address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ip {
    V4([u8; 4]),
    V6([u8; 16]),
}

impl From<[u8; 4]> for Ip {
    fn from(a: [u8; 4]) -> Ip {
        Ip::V4(a)
    }
}

impl Default for Ip {
    fn default() -> Ip {
        Ip::ANY4
    }
}

impl Ip {
    pub const ANY4: Ip = Ip::V4([0; 4]);
    pub const ANY6: Ip = Ip::V6([0; 16]);
    pub const LOCALHOST4: Ip = Ip::V4([127, 0, 0, 1]);
    pub const LOCALHOST6: Ip = Ip::V6([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

    /// Dotted quad or IPv6 text (RFC 4291, `::` and a trailing dotted quad allowed).
    pub fn parse(s: &str) -> Option<Ip> {
        if s.contains(':') {
            parse6(s).map(Ip::V6)
        } else {
            parse4(s).map(Ip::V4)
        }
    }

    pub fn is_v6(&self) -> bool {
        matches!(self, Ip::V6(_))
    }

    pub fn v4(&self) -> Option<[u8; 4]> {
        match *self {
            Ip::V4(a) => Some(a),
            Ip::V6(_) => None,
        }
    }

    /// `::ffff:a.b.c.d` as IPv4; anything else as it is.
    pub fn unmapped(self) -> Ip {
        match self {
            Ip::V6(b) if b[..10] == [0; 10] && b[10] == 0xff && b[11] == 0xff => {
                Ip::V4([b[12], b[13], b[14], b[15]])
            }
            a => a,
        }
    }

    /// IPv6 link-local (fe80::/10): needs an interface (scope) to mean anything.
    pub fn is_link_local(&self) -> bool {
        matches!(self, Ip::V6(b) if b[0] == 0xfe && b[1] & 0xc0 == 0x80)
    }

    pub fn is_unspecified(&self) -> bool {
        match self {
            Ip::V4(a) => *a == [0; 4],
            Ip::V6(a) => *a == [0; 16],
        }
    }
}

fn parse4(s: &str) -> Option<[u8; 4]> {
    let mut ip = [0u8; 4];
    let mut it = s.split('.');
    for b in ip.iter_mut() {
        let t = it.next()?;
        if t.is_empty() || t.len() > 3 {
            return None;
        }
        *b = t.parse().ok()?;
    }
    if it.next().is_some() {
        return None;
    }
    Some(ip)
}

fn parse6(s: &str) -> Option<[u8; 16]> {
    let (head, tail) = match s.split_once("::") {
        Some((h, t)) => (h, Some(t)),
        None => (s, None),
    };
    let words_of = |p: &str| -> Option<Vec<u16>> {
        let mut out = Vec::new();
        if p.is_empty() {
            return Some(out);
        }
        let parts: Vec<&str> = p.split(':').collect();
        for (k, w) in parts.iter().enumerate() {
            if k == parts.len() - 1 && w.contains('.') {
                let v4 = parse4(w)?;
                out.push(u16::from_be_bytes([v4[0], v4[1]]));
                out.push(u16::from_be_bytes([v4[2], v4[3]]));
            } else {
                if w.is_empty() || w.len() > 4 {
                    return None;
                }
                out.push(u16::from_str_radix(w, 16).ok()?);
            }
        }
        Some(out)
    };
    let mut words = words_of(head)?;
    if let Some(t) = tail {
        let tw = words_of(t)?;
        if words.len() + tw.len() > 7 {
            return None;
        }
        words.resize(8 - tw.len(), 0);
        words.extend(tw);
    }
    if words.len() != 8 {
        return None;
    }
    let mut a = [0u8; 16];
    for (i, w) in words.iter().enumerate() {
        a[2 * i..2 * i + 2].copy_from_slice(&w.to_be_bytes());
    }
    Some(a)
}

impl fmt::Display for Ip {
    /// IPv6 per RFC 5952: lowercase, no leading zeros, the longest run of 2+ zero words as
    /// `::`; IPv4-mapped as `::ffff:a.b.c.d`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = match *self {
            Ip::V4([a, b, c, d]) => return write!(f, "{a}.{b}.{c}.{d}"),
            Ip::V6(b) => b,
        };
        if let Ip::V4(v4) = self.unmapped() {
            return write!(f, "::ffff:{}", Ip::V4(v4));
        }
        let mut w = [0u16; 8];
        for (i, x) in w.iter_mut().enumerate() {
            *x = u16::from_be_bytes([b[2 * i], b[2 * i + 1]]);
        }
        let (mut best, mut best_len, mut cur, mut cur_len) = (8, 0, 0, 0);
        for (i, &x) in w.iter().enumerate() {
            if x == 0 {
                if cur_len == 0 {
                    cur = i;
                }
                cur_len += 1;
                if cur_len > best_len {
                    best = cur;
                    best_len = cur_len;
                }
            } else {
                cur_len = 0;
            }
        }
        if best_len < 2 {
            best = 8;
        }
        let mut i = 0;
        while i < 8 {
            if i == best {
                f.write_str("::")?;
                i += best_len;
                continue;
            }
            if i > 0 && i != best + best_len {
                f.write_str(":")?;
            }
            write!(f, "{:x}", w[i])?;
            i += 1;
        }
        Ok(())
    }
}

/// Interface index (sin6_scope_id) to name: lo 1, eth0 2, eth1 3.
pub fn if_name(index: u32) -> Option<&'static str> {
    match index {
        1 => Some("lo"),
        2 => Some("eth0"),
        3 => Some("eth1"),
        _ => None,
    }
}

/// Interface name (or number) to index.
pub fn if_index(name: &str) -> Option<u32> {
    match name {
        "lo" => Some(1),
        "eth0" => Some(2),
        "eth1" => Some(3),
        n => n.parse().ok(),
    }
}

/// An address and port; `scope` is the interface index of a link-local IPv6 address
/// (0: none).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SockAddr {
    pub ip: Ip,
    pub port: u16,
    pub scope: u32,
}

impl SockAddr {
    pub const ANY: SockAddr = SockAddr {
        ip: Ip::ANY4,
        port: 0,
        scope: 0,
    };

    pub const fn new(ip: Ip, port: u16) -> SockAddr {
        SockAddr { ip, port, scope: 0 }
    }

    pub const fn v4(ip: [u8; 4], port: u16) -> SockAddr {
        SockAddr::new(Ip::V4(ip), port)
    }

    /// The IPv6 wildcard: a dual-stack listener (IPv4 too, unless IPV6_V6ONLY).
    pub const fn any6(port: u16) -> SockAddr {
        SockAddr::new(Ip::ANY6, port)
    }

    /// Numeric `ADDR[:PORT]`: `a.b.c.d[:p]`, `[v6][:p]` or a bare `v6` (no port then),
    /// IPv6 optionally with `%IFACE` (`fe80::1%eth1`). `localhost` is 127.0.0.1 here
    /// ([`lookup_all`] gives ::1 as well).
    pub fn parse(s: &str, default_port: u16) -> Option<SockAddr> {
        let (host, port) = split_host_port(s, default_port).ok()?;
        if host == "localhost" {
            return Some(SockAddr::new(Ip::LOCALHOST4, port));
        }
        parse_host(host, port)
    }

    pub fn family(&self) -> usize {
        if self.ip.is_v6() { AF_INET6 } else { AF_INET }
    }

    fn from_raw(raw: &[u8; 28]) -> SockAddr {
        let port = u16::from_be_bytes([raw[2], raw[3]]);
        if u16::from_le_bytes([raw[0], raw[1]]) as usize == AF_INET6 {
            let ip = Ip::V6(raw[8..24].try_into().unwrap()).unmapped();
            let scope = u32::from_le_bytes(raw[24..28].try_into().unwrap());
            SockAddr { ip, port, scope }
        } else {
            SockAddr::v4([raw[4], raw[5], raw[6], raw[7]], port)
        }
    }

    /// The `struct sockaddr_in` / `sockaddr_in6` bytes and their length.
    fn raw(&self) -> ([u8; 28], usize) {
        let mut b = [0u8; 28];
        b[2..4].copy_from_slice(&self.port.to_be_bytes());
        match self.ip {
            Ip::V4(a) => {
                b[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
                b[4..8].copy_from_slice(&a);
                (b, 16)
            }
            Ip::V6(a) => {
                b[0..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
                b[8..24].copy_from_slice(&a);
                b[24..28].copy_from_slice(&self.scope.to_le_bytes());
                (b, 28)
            }
        }
    }
}

impl fmt::Display for SockAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ip {
            Ip::V4(_) => write!(f, "{}:{}", self.ip, self.port),
            Ip::V6(_) if self.scope != 0 => match if_name(self.scope) {
                Some(n) => write!(f, "[{}%{n}]:{}", self.ip, self.port),
                None => write!(f, "[{}%{}]:{}", self.ip, self.scope, self.port),
            },
            Ip::V6(_) => write!(f, "[{}]:{}", self.ip, self.port),
        }
    }
}

/// `HOST:PORT`, `[V6]:PORT`, `[V6]`, a bare IPv6 address or `HOST` -> (host, port).
fn split_host_port(spec: &str, default_port: u16) -> core::result::Result<(&str, u16), String> {
    let port_of = |p: &str| {
        p.parse::<u16>()
            .map_err(|_| alloc::format!("{spec}: bad port '{p}'"))
    };
    if let Some(rest) = spec.strip_prefix('[') {
        let (h, after) = rest
            .split_once(']')
            .ok_or_else(|| alloc::format!("{spec}: missing ']'"))?;
        return match after.strip_prefix(':') {
            Some(p) => Ok((h, port_of(p)?)),
            None if after.is_empty() => Ok((h, default_port)),
            None => Err(alloc::format!("{spec}: junk after ']'")),
        };
    }
    if spec.matches(':').count() > 1 {
        return Ok((spec, default_port)); // a bare IPv6 address
    }
    match spec.rsplit_once(':') {
        Some((h, p)) => Ok((h, port_of(p)?)),
        None => Ok((spec, default_port)),
    }
}

/// A numeric host (with an optional IPv6 `%IFACE`).
fn parse_host(host: &str, port: u16) -> Option<SockAddr> {
    let (h, scope) = match host.split_once('%') {
        Some((h, s)) => (h, if_index(s)?),
        None => (host, 0),
    };
    let ip = Ip::parse(h)?;
    if scope != 0 && !ip.is_v6() {
        return None;
    }
    Some(SockAddr { ip, port, scope })
}

pub fn socket(ty: usize) -> Result<i32> {
    socket_proto(ty, 0)
}

/// An AF_INET6 socket (dual-stack unless [`set_v6only`]).
pub fn socket6(ty: usize) -> Result<i32> {
    socket_family(AF_INET6, ty, 0)
}

/// A socket of the family that can reach `a`.
pub fn socket_for(a: &SockAddr, ty: usize) -> Result<i32> {
    socket_family(a.family(), ty, 0)
}

/// socket(AF_INET, ty, proto); e.g. `(SOCK_DGRAM, IPPROTO_ICMP)` is a ping socket.
pub fn socket_proto(ty: usize, proto: usize) -> Result<i32> {
    socket_family(AF_INET, ty, proto)
}

pub fn socket_family(family: usize, ty: usize, proto: usize) -> Result<i32> {
    sc(nr::SOCKET, [family, ty, proto, 0, 0, 0]).map(|fd| fd as i32)
}

/// IPV6_JOIN_GROUP / IPV6_LEAVE_GROUP: listen to IPv6 multicast `group` on interface
/// `ifindex` (0: where it routes). The kernel reports it with MLD and only then takes the
/// group's traffic.
pub fn group6(fd: i32, group: [u8; 16], ifindex: u32, join: bool) -> Result<()> {
    let mut mreq = [0u8; 20];
    mreq[..16].copy_from_slice(&group);
    mreq[16..].copy_from_slice(&ifindex.to_le_bytes());
    let opt = if join { 20 } else { 21 };
    sc(
        nr::SETSOCKOPT,
        [fd as usize, 41, opt, mreq.as_ptr() as usize, 20, 0],
    )
    .map(|_| ())
}

/// IPV6_V6ONLY: an AF_INET6 socket that takes only IPv6 (before bind).
pub fn set_v6only(fd: i32, on: bool) -> Result<()> {
    let v: i32 = on as i32;
    sc(
        nr::SETSOCKOPT,
        [fd as usize, 41, 26, &v as *const i32 as usize, 4, 0],
    )
    .map(|_| ())
}

pub fn sendto(fd: i32, buf: &[u8], to: &SockAddr) -> Result<usize> {
    let (raw, len) = to.raw();
    sc(
        nr::SENDTO,
        [
            fd as usize,
            buf.as_ptr() as usize,
            buf.len(),
            0,
            raw.as_ptr() as usize,
            len,
        ],
    )
}

pub fn recvfrom(fd: i32, buf: &mut [u8]) -> Result<(usize, SockAddr)> {
    let mut raw = [0u8; 28];
    let mut len: u32 = 28;
    let n = sc(
        nr::RECVFROM,
        [
            fd as usize,
            buf.as_mut_ptr() as usize,
            buf.len(),
            0,
            raw.as_mut_ptr() as usize,
            &mut len as *mut u32 as usize,
        ],
    )?;
    Ok((n, SockAddr::from_raw(&raw)))
}

/// SO_RCVTIMEO: blocking receives give EAGAIN after `ms` (0 = wait forever).
pub fn set_recv_timeout(fd: i32, ms: u64) -> Result<()> {
    let tv: [i64; 2] = [(ms / 1000) as i64, (ms % 1000 * 1000) as i64];
    sc(
        nr::SETSOCKOPT,
        [fd as usize, 1, 20, tv.as_ptr() as usize, 16, 0],
    )
    .map(|_| ())
}

/// SO_REUSEADDR (accepted; milkOS always allows rebinding over closing connections).
pub fn set_reuseaddr(fd: i32) -> Result<()> {
    let one: i32 = 1;
    sc(
        nr::SETSOCKOPT,
        [fd as usize, 1, 2, &one as *const i32 as usize, 4, 0],
    )
    .map(|_| ())
}

/// `struct pollfd`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PollFd {
    pub fd: i32,
    pub events: u16,
    pub revents: u16,
}

/// ppoll with a timeout in ms (None = forever). Returns how many fds are ready.
pub fn poll(fds: &mut [PollFd], timeout_ms: Option<u64>) -> Result<usize> {
    let ts: [i64; 2] = match timeout_ms {
        Some(ms) => [(ms / 1000) as i64, (ms % 1000 * 1_000_000) as i64],
        None => [0, 0],
    };
    let tsp = if timeout_ms.is_some() {
        ts.as_ptr() as usize
    } else {
        0
    };
    sc(
        nr::PPOLL,
        [fds.as_mut_ptr() as usize, fds.len(), tsp, 0, 0, 0],
    )
}

pub fn connect(fd: i32, a: &SockAddr) -> Result<()> {
    let (raw, len) = a.raw();
    sc(
        nr::CONNECT,
        [fd as usize, raw.as_ptr() as usize, len, 0, 0, 0],
    )
    .map(|_| ())
}

pub fn bind(fd: i32, a: &SockAddr) -> Result<()> {
    let (raw, len) = a.raw();
    sc(nr::BIND, [fd as usize, raw.as_ptr() as usize, len, 0, 0, 0]).map(|_| ())
}

/// The socket's own address (getsockname).
pub fn local_addr(fd: i32) -> Result<SockAddr> {
    let mut raw = [0u8; 28];
    let mut len: u32 = 28;
    sc(
        nr::GETSOCKNAME,
        [
            fd as usize,
            raw.as_mut_ptr() as usize,
            &mut len as *mut u32 as usize,
            0,
            0,
            0,
        ],
    )?;
    Ok(SockAddr::from_raw(&raw))
}

pub fn listen(fd: i32, backlog: usize) -> Result<()> {
    sc(nr::LISTEN, [fd as usize, backlog, 0, 0, 0, 0]).map(|_| ())
}

/// Accept a connection: (fd, peer). A dual-stack listener's IPv4 peers come back as IPv4.
pub fn accept(fd: i32) -> Result<(i32, SockAddr)> {
    let mut raw = [0u8; 28];
    let mut len: u32 = 28;
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
    Ok((nfd as i32, SockAddr::from_raw(&raw)))
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

/// Which families a lookup may return (`-4` / `-6` options of the tools).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    Any,
    V4,
    V6,
}

impl Want {
    fn takes(self, ip: &Ip) -> bool {
        match self {
            Want::Any => true,
            Want::V4 => !ip.is_v6(),
            Want::V6 => ip.is_v6(),
        }
    }
}

/// Every address of a host for a user program (getaddrinfo-style, IPv6 first):
///
/// * a numeric IPv4 or IPv6 address (`fe80::1%eth1` with its interface): no lookup;
/// * `localhost`: ::1 and 127.0.0.1;
/// * `NAME.local`: mDNS on the VM-to-VM link (eth1), [`crate::mdns`]: AAAA over
///   ff02::fb and A over 224.0.0.251;
/// * anything else: the kernel's resolver ([`kernel_resolve`], IPv4 first). With the
///   internet on that is real DNS; offline (every test boot) it refuses all but `.test`.
pub fn resolve_all(host: &str, want: Want) -> core::result::Result<Vec<SockAddr>, String> {
    let mut out = Vec::new();
    if host == "localhost" {
        out.push(SockAddr::new(Ip::LOCALHOST6, 0));
        out.push(SockAddr::new(Ip::LOCALHOST4, 0));
    } else if let Some(a) = parse_host(host, 0) {
        out.push(a);
    } else if crate::mdns::is_local(host) {
        for ip in crate::mdns::resolve(host, 1500, want)? {
            out.push(SockAddr::new(ip, 0));
        }
    } else {
        match kernel_resolve(host, want) {
            Ok(v) => out.extend(v.into_iter().map(|ip| SockAddr::new(ip, 0))),
            Err(e) => return Err(lookup_words(host, e)),
        }
    }
    out.retain(|a| want.takes(&a.ip));
    if out.is_empty() {
        let fam = if want == Want::V6 { "IPv6" } else { "IPv4" };
        return Err(alloc::format!("{host}: no {fam} address"));
    }
    Ok(out)
}

/// The kernel's lookup of `host` (`MILK_RESOLVE`): its addresses, IPv4 first. Errors:
/// ENOENT no such host, ENODATA none of that family (61), ETIMEDOUT no answer, EPERM
/// refused (the internet is off), EADDRNOTAVAIL no DNS server, EINVAL not a name, EIO the
/// server failed, ELOOP a CNAME loop, EACCES the sandbox's net capability.
pub fn kernel_resolve(host: &str, want: Want) -> Result<Vec<Ip>> {
    const MAX: usize = 64;
    let mut buf = alloc::vec![0u8; 17 * MAX];
    let w = match want {
        Want::Any => 0,
        Want::V4 => 1,
        Want::V6 => 2,
    };
    let n = sc(
        nr::MILK_RESOLVE,
        [
            host.as_ptr() as usize,
            host.len(),
            w,
            buf.as_mut_ptr() as usize,
            MAX,
            0,
        ],
    )?;
    let mut v = Vec::new();
    for rec in buf.chunks(17).take(n) {
        if rec[0] == 6 {
            let mut a = [0u8; 16];
            a.copy_from_slice(&rec[1..17]);
            v.push(Ip::V6(a));
        } else {
            v.push(Ip::V4([rec[1], rec[2], rec[3], rec[4]]));
        }
    }
    Ok(v)
}

/// A lookup failure in words (`HOST: ...`).
pub fn lookup_words(host: &str, e: Errno) -> String {
    match e.0 {
        1 => alloc::format!(
            "{host}: not looked up: the internet is off (offline mode), so milkOS resolves only numeric addresses, localhost, NAME.local and .test names"
        ),
        2 => alloc::format!("{host}: there's no such host"),
        61 => alloc::format!("{host}: the host has no address of that kind"),
        110 => alloc::format!("{host}: the name server didn't answer (no internet?)"),
        99 => {
            alloc::format!("{host}: there's no DNS server yet (is the network up? network status)")
        }
        22 => alloc::format!("{host}: isn't a host name"),
        40 => alloc::format!("{host}: its aliases (CNAMEs) go round in a circle"),
        13 => alloc::format!("{host}: not looked up: this program may not use the network"),
        _ => alloc::format!("{host}: the lookup failed: {e}"),
    }
}

/// The first address of `host` (see [`resolve_all`]).
pub fn resolve(host: &str) -> core::result::Result<Ip, String> {
    resolve_all(host, Want::Any).map(|v| v[0].ip)
}

/// Every address of `HOST[:PORT]` (`[V6]:PORT` for IPv6 with a port), `default_port` if
/// there is none: getaddrinfo's results, in the order to try them.
pub fn lookup_all(
    spec: &str,
    default_port: u16,
    want: Want,
) -> core::result::Result<Vec<SockAddr>, String> {
    let (host, port) = split_host_port(spec, default_port)?;
    let mut v = resolve_all(host, want)?;
    for a in v.iter_mut() {
        a.port = port;
    }
    Ok(v)
}

/// The first address of `HOST[:PORT]` (see [`lookup_all`]).
pub fn lookup(spec: &str, default_port: u16) -> core::result::Result<SockAddr, String> {
    lookup_all(spec, default_port, Want::Any).map(|v| v[0])
}

/// Connect a socket of type `ty` to the first of `addrs` that answers (each with a socket
/// of its family): (fd, the address used). The error names the last failure.
pub fn connect_any(addrs: &[SockAddr], ty: usize) -> core::result::Result<(i32, SockAddr), String> {
    let mut last = String::from("no address");
    for a in addrs {
        let fd = match socket_for(a, ty) {
            Ok(fd) => fd,
            Err(e) => {
                last = alloc::format!("socket: {e}");
                continue;
            }
        };
        match connect(fd, a) {
            Ok(()) => return Ok((fd, *a)),
            Err(e) => {
                last = alloc::format!("connect {a}: {e}");
                let _ = crate::sys::close(fd);
            }
        }
    }
    Err(last)
}

/// Parse/format round trips. (passed, total)
pub fn selftest() -> (usize, usize) {
    let cases: [(&str, &str); 6] = [
        ("::1", "::1"),
        ("fe80:0:0:0:5054:ff:fe12:3456", "fe80::5054:ff:fe12:3456"),
        ("2001:db8::0:1", "2001:db8::1"),
        ("::ffff:10.0.2.2", "::ffff:10.0.2.2"),
        ("1:0:0:2:0:0:0:3", "1:0:0:2::3"),
        ("10.0.2.15", "10.0.2.15"),
    ];
    let mut ok = 0;
    for (i, o) in cases {
        ok += Ip::parse(i).is_some_and(|a| alloc::format!("{a}") == o) as usize;
    }
    let a = SockAddr::parse("[fe80::1%eth1]:5353", 0);
    ok += a.is_some_and(|a| {
        a.scope == 3 && a.port == 5353 && alloc::format!("{a}") == "[fe80::1%eth1]:5353"
    }) as usize;
    ok += (SockAddr::parse("::1", 7) == Some(SockAddr::new(Ip::LOCALHOST6, 7))) as usize;
    ok += (SockAddr::parse("1.2.3.4:80", 0) == Some(SockAddr::v4([1, 2, 3, 4], 80))) as usize;
    ok += (Ip::parse("1::2::3").is_none() && Ip::parse("1.2.3").is_none()) as usize;
    ok += lookup_all("localhost:9", 0, Want::Any).is_ok_and(|v| {
        v.len() == 2 && v[0].ip == Ip::LOCALHOST6 && v[1].ip == Ip::LOCALHOST4 && v[0].port == 9
    }) as usize;
    (ok, 11)
}
