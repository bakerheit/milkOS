//! In-kernel demo services, run by the net thread after every pass: a tiny HTTP/1.0 server,
//! off by default ([`enable`] starts it on a port; the boot self-test does when
//! `scripts/net-test.sh` asks with `khttpd=PORT`). Userland `/System/Services/httpd` serves :80. It only uses the public socket API (non-blocking), so it doubles as a
//! test of it. Pages:
//!
//! - `/`         a small HTML page with the interface configuration;
//! - `/net`      `/proc/net/{dev,route,arp,tcp,snmp}` as text;
//! - `/big`      1 MiB of 32-byte numbered lines (exercises windowing / retransmission);
//! - else 404.
//!
//! `Connection: close` always: one request per connection, then FIN.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use super::errno::EAGAIN;
use super::socket::{self, SockType, SocketId};
use super::wire::{Ipv4, SockAddr};
use super::{procnet, with};
use crate::{kprintln, timer};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::SpinLock;

const MAX_CONNS: usize = 8;
const REQ_MAX: usize = 2048;
const IDLE_NS: u64 = 10_000_000_000;
pub const BIG_SIZE: usize = 1 << 20;
const BIG_LINE: usize = 32;

enum Body {
    Text(Vec<u8>),
    Big,
}

struct Conn {
    id: SocketId,
    req: Vec<u8>,
    head: Vec<u8>,
    body: Option<Body>,
    sent: usize,
    last: u64,
}

/// The server's state, owned by the net thread.
pub struct Http {
    listener: Option<SocketId>,
    conns: Vec<Conn>,
}

static SERVED: AtomicU64 = AtomicU64::new(0);

/// Bytes `[off, off+out.len())` of the /big body: line k is `"{k:08} milkOS big page ....\n"`.
pub fn big_bytes(off: usize, out: &mut [u8]) {
    for (j, b) in out.iter_mut().enumerate() {
        let p = off + j;
        let (line, col) = (p / BIG_LINE, p % BIG_LINE);
        *b = match col {
            0..8 => b'0' + ((line / 10usize.pow(7 - col as u32)) % 10) as u8,
            31 => b'\n',
            _ => b" milkOS big page ..............."[col - 8],
        };
    }
}

/// Handed from [`enable`] to the net thread.
static PENDING: SpinLock<Option<Http>> = SpinLock::new(None);
static PORT: AtomicU64 = AtomicU64::new(0);

/// Start the kernel HTTP server on `port` (it is off by default; userland `/System/Services/httpd`
/// normally owns :80). The port is listening when this returns. Thread context.
pub fn enable(port: u16) -> crate::fs::errno::Result<()> {
    let s = socket::socket(SockType::Stream)?;
    let r =
        socket::bind(s, SockAddr::new(Ipv4::UNSPECIFIED, port)).and_then(|_| socket::listen(s, 8));
    if let Err(e) = r {
        let _ = socket::close(s);
        return Err(e);
    }
    *PENDING.lock() = Some(Http {
        listener: Some(s),
        conns: Vec::new(),
    });
    PORT.store(port as u64, Ordering::Relaxed);
    kprintln!("[net] kernel httpd listening on :{port}");
    Ok(())
}

/// Port of the kernel HTTP server, if enabled.
pub fn port() -> Option<u16> {
    match PORT.load(Ordering::Relaxed) {
        0 => None,
        p => Some(p as u16),
    }
}

/// Net thread: pick up a server [`enable`] created.
pub fn adopt(slot: &mut Option<Http>) {
    if let Some(mut g) = PENDING.try_lock()
        && let Some(h) = g.take()
    {
        *slot = Some(h);
    }
}

/// How many requests the HTTP server has answered completely.
pub fn served() -> u64 {
    SERVED.load(Ordering::Relaxed)
}

/// One pass: accept, read requests, push responses. Net thread context.
pub fn run(h: &mut Http) {
    let Some(l) = h.listener else { return };
    let now = timer::now_ns();
    while h.conns.len() < MAX_CONNS {
        match socket::accept(l) {
            Ok((id, _peer)) => h.conns.push(Conn {
                id,
                req: Vec::new(),
                head: Vec::new(),
                body: None,
                sent: 0,
                last: now,
            }),
            Err(_) => break,
        }
    }
    let mut k = 0;
    while k < h.conns.len() {
        let done = drive(&mut h.conns[k], now);
        if done {
            let c = h.conns.swap_remove(k);
            if c.body.is_some() && c.sent == c.head.len() + body_len(&c) {
                SERVED.fetch_add(1, Ordering::Relaxed);
            }
            let _ = socket::close(c.id);
        } else {
            k += 1;
        }
    }
}

fn body_len(c: &Conn) -> usize {
    match &c.body {
        Some(Body::Text(t)) => t.len(),
        Some(Body::Big) => BIG_SIZE,
        None => 0,
    }
}

/// Returns true when the connection is finished (or dead).
fn drive(c: &mut Conn, now: u64) -> bool {
    if c.body.is_none() {
        let mut buf = [0u8; 512];
        loop {
            match socket::recv(c.id, &mut buf) {
                Ok(0) => return true, // closed before a full request
                Ok(n) => {
                    c.last = now;
                    c.req.extend_from_slice(&buf[..n]);
                    if c.req.len() > REQ_MAX {
                        return true;
                    }
                }
                Err(e) if e == EAGAIN => break,
                Err(_) => return true,
            }
        }
        if !c.req.windows(4).any(|w| w == b"\r\n\r\n") && !c.req.windows(2).any(|w| w == b"\n\n") {
            return now.saturating_sub(c.last) > IDLE_NS;
        }
        respond(c);
    }
    let total = c.head.len() + body_len(c);
    while c.sent < total {
        let mut chunk = [0u8; 1460];
        let n = if c.sent < c.head.len() {
            let n = (c.head.len() - c.sent).min(chunk.len());
            chunk[..n].copy_from_slice(&c.head[c.sent..c.sent + n]);
            n
        } else {
            let off = c.sent - c.head.len();
            let n = (total - c.sent).min(chunk.len());
            match c.body.as_ref().unwrap() {
                Body::Text(t) => chunk[..n].copy_from_slice(&t[off..off + n]),
                Body::Big => big_bytes(off, &mut chunk[..n]),
            }
            n
        };
        match socket::send(c.id, &chunk[..n]) {
            Ok(m) => {
                c.sent += m;
                c.last = now;
            }
            Err(e) if e == EAGAIN => {
                return now.saturating_sub(c.last) > IDLE_NS;
            }
            Err(_) => return true,
        }
    }
    true // all queued; close() sends FIN after it
}

fn respond(c: &mut Conn) {
    let line = c.req.split(|&b| b == b'\n').next().unwrap_or(&[]);
    let mut parts = line.split(|&b| b == b' ');
    let method = parts.next().unwrap_or(&[]);
    let path = parts.next().unwrap_or(&[]);
    let (status, ctype, body) = if method != b"GET" && method != b"HEAD" {
        (
            "405 Method Not Allowed",
            "text/plain",
            Body::Text(b"GET only\n".to_vec()),
        )
    } else {
        match path {
            b"/" | b"/index.html" => ("200 OK", "text/html", Body::Text(index())),
            b"/net" => ("200 OK", "text/plain", Body::Text(net_text())),
            b"/big" => ("200 OK", "text/plain", Body::Big),
            _ => (
                "404 Not Found",
                "text/plain",
                Body::Text(b"not found\n".to_vec()),
            ),
        }
    };
    let len = match &body {
        Body::Text(t) => t.len(),
        Body::Big => BIG_SIZE,
    };
    let mut head = String::new();
    let _ = write!(
        head,
        "HTTP/1.0 {status}\r\nServer: milkOS-httpd\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
    );
    c.head = head.into_bytes();
    c.body = Some(if method == b"HEAD" {
        Body::Text(Vec::new())
    } else {
        body
    });
}

fn index() -> Vec<u8> {
    let (cfg, mac) = with(|st| (st.iface.eth0().cfg, st.iface.eth0().mac));
    let up = timer::now_ns() / 1_000_000_000;
    let mut s = String::new();
    let _ = write!(
        s,
        "<!doctype html>\n<html><head><title>milkOS</title></head>\n<body>\n\
         <h1>Hello from milkOS</h1>\n<p>Served by the kernel's own TCP/IP stack.</p>\n<ul>\n\
         <li>eth0: {} / {} (mac {})</li>\n<li>gateway: {}</li>\n<li>uptime: {} s</li>\n</ul>\n\
         <p><a href=\"/net\">/net</a> &middot; <a href=\"/big\">/big</a></p>\n</body></html>\n",
        cfg.addr,
        cfg.mask,
        mac,
        cfg.gateway.unwrap_or_default(),
        up
    );
    s.into_bytes()
}

fn net_text() -> Vec<u8> {
    let mut s = String::new();
    for &(name, f) in procnet::FILES {
        let _ = writeln!(s, "==> /proc/net/{name} <==");
        s.push_str(&procnet::render(f));
        s.push('\n');
    }
    s.into_bytes()
}
