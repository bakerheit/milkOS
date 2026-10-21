//! The Milk Drop wire formats: discovery datagrams (UDP, link-scope multicast on eth1) and
//! the line-based transfer protocol over TCP. See docs/networking.md, "Milk Drop".

use alloc::string::String;
use alloc::vec::Vec;
use rt::ed25519;
use rt::net::{self, SockAddr};

use crate::ident::{self, Identity, hex};
use rt::crypto::{self, aead};
use rt::sha256::Sha256;

/// Discovery: queries and announcements, on the mDNS group but our own port.
pub const DISC_PORT: u16 = 5359;
pub const GROUP: [u8; 4] = [224, 0, 0, 251];
/// Transfers (TCP), while `drop recv` runs.
pub const XFER_PORT: u16 = 5360;
/// Discovery datagrams.
pub const MAGIC: &str = "MILKDROP/1";
/// The transfer protocol: v2 = X25519 + ChaCha20-Poly1305 (v1 was signed but plaintext).
pub const XMAGIC: &str = "MILKDROP/2";
/// Longest protocol line we accept.
const MAX_LINE: usize = 1024;

pub fn group() -> SockAddr {
    SockAddr {
        ip: GROUP,
        port: DISC_PORT,
    }
}

/// `key=value` field of a protocol line.
pub fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|w| w.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

/// %-escape a file name for a protocol line (space, %, controls and non-ASCII).
pub fn esc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            o.push(b as char);
        } else {
            o.push_str(&alloc::format!("%{b:02x}"));
        }
    }
    o
}

pub fn unesc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- discovery ---------------------------------------------------------------------------

/// A peer that answered (or announced itself).
#[derive(Clone)]
pub struct Peer {
    pub name: String,
    pub host: String,
    pub key: [u8; 32],
    pub addr: [u8; 4],
    pub port: u16,
    pub receiving: bool,
    /// Its rotation statement (`rot=old:sig`), if its key replaced an older one.
    pub rot: Option<ident::Rotation>,
}

const ANNOUNCE_CTX: &str = "milkdrop-announce-v1";

/// Our signed announcement: who we are, where transfers go, whether `drop recv` is on.
pub fn announce(id: &Identity, host: &str, receiving: bool) -> String {
    let body = alloc::format!(
        "{MAGIC} ANNOUNCE name={} host={host} port={XFER_PORT} key={} fp={} recv={}{}",
        id.name,
        hex(&id.public),
        ident::fp_compact(&id.public),
        receiving as u8,
        ident::rotation(&id.public)
            .map(|r| alloc::format!(" rot={}:{}", hex(&r.old), hex(&r.sig)))
            .unwrap_or_default()
    );
    let sig = ed25519::sign(&id.seed, alloc::format!("{ANNOUNCE_CTX} {body}").as_bytes());
    alloc::format!("{body} sig={}", hex(&sig))
}

/// Parse and check an announcement (signature by the key it carries, fingerprint matches
/// the key). Says nothing about trust: that is decided on the TCP hello.
pub fn parse_announce(line: &str, from: SockAddr) -> Option<Peer> {
    let (body, sig) = line.rsplit_once(" sig=")?;
    let mut w = body.split(' ');
    if w.next() != Some(MAGIC) || w.next() != Some("ANNOUNCE") {
        return None;
    }
    let key = ed25519::unhex::<32>(field(body, "key")?)?;
    let sig = ed25519::unhex::<64>(sig)?;
    if field(body, "fp")? != ident::fp_compact(&key)
        || !ed25519::verify(
            &key,
            alloc::format!("{ANNOUNCE_CTX} {body}").as_bytes(),
            &sig,
        )
    {
        return None;
    }
    let name = field(body, "name")?;
    if !ident::name_ok(name) {
        return None;
    }
    Some(Peer {
        name: String::from(name),
        host: String::from(field(body, "host").unwrap_or("?")),
        key,
        addr: from.ip,
        port: field(body, "port")?.parse().ok()?,
        receiving: field(body, "recv") == Some("1"),
        rot: field(body, "rot").and_then(|r| {
            let (o, s) = r.split_once(':')?;
            Some(ident::Rotation {
                old: ed25519::unhex::<32>(o)?,
                sig: ed25519::unhex::<64>(s)?,
            })
        }),
    })
}

/// Ask the link who is there: one multicast QUERY, then collect answers for `ms`.
pub fn query(ms: u64) -> Result<Vec<Peer>, String> {
    let fd = net::socket(net::SOCK_DGRAM).map_err(|e| alloc::format!("socket: {e}"))?;
    let q = alloc::format!("{MAGIC} QUERY");
    let r = (|| {
        net::sendto(fd, q.as_bytes(), &group()).map_err(|e| {
            alloc::format!(
                "query on 224.0.0.251:{DISC_PORT}: {e} (no eth1? Milk Drop needs the VM-to-VM link)"
            )
        })?;
        let end = rt::sys::clock_ns(rt::sys::CLOCK_MONOTONIC) + ms * 1_000_000;
        let mut peers: Vec<Peer> = Vec::new();
        let mut buf = [0u8; 1500];
        let mut resent = false;
        loop {
            let now = rt::sys::clock_ns(rt::sys::CLOCK_MONOTONIC);
            if now >= end {
                break;
            }
            if !resent && now + ms * 1_000_000 / 2 >= end {
                // One retry halfway (a datagram can be lost).
                let _ = net::sendto(fd, q.as_bytes(), &group());
                resent = true;
            }
            let _ = net::set_recv_timeout(fd, ((end - now) / 1_000_000).clamp(1, 100));
            let Ok((n, from)) = net::recvfrom(fd, &mut buf) else {
                continue;
            };
            let line = String::from_utf8_lossy(&buf[..n]);
            if let Some(peer) = parse_announce(line.trim_end(), from)
                && !peers.iter().any(|x| x.key == peer.key)
            {
                peers.push(peer);
            }
        }
        Ok(peers)
    })();
    let _ = rt::sys::close(fd);
    r
}

// ---- the transfer connection -------------------------------------------------------------

/// Hello transcript both sides sign (with their role): binds both nonces, both identity
/// keys and both ephemeral X25519 keys, so a signature can't be replayed into another
/// session or for another peer, and nobody in the middle can swap the X25519 keys.
#[allow(clippy::too_many_arguments)]
pub fn transcript(
    ctx: &str,
    role: &str,
    nonce_r: &str,
    nonce_s: &str,
    key_r: &[u8],
    key_s: &[u8],
    dh_r: &[u8],
    dh_s: &[u8],
) -> String {
    alloc::format!(
        "{ctx} {role} {nonce_r} {nonce_s} {} {} {} {}",
        hex(key_r),
        hex(key_s),
        hex(dh_r),
        hex(dh_s)
    )
}

/// What a rotated identity signs with its OLD key: "this new key replaces me".
pub fn rotation_msg(old: &[u8; 32], new: &[u8; 32]) -> String {
    alloc::format!("milkdrop-rotate-v1 {} {}", hex(old), hex(new))
}

/// The session after the hello: one ChaCha20-Poly1305 key per direction, derived from
/// the X25519 shared secret and the transcript; nonces are a per-direction counter.
pub struct Channel {
    tx: [u8; 32],
    rx: [u8; 32],
    tx_n: u64,
    rx_n: u64,
}

/// Longest plaintext in one record.
pub const MAX_RECORD: usize = 32 * 1024;

impl Channel {
    pub fn derive(
        ctx: &str,
        shared: &[u8; 32],
        nonce_r: &str,
        nonce_s: &str,
        dh_r: &[u8; 32],
        dh_s: &[u8; 32],
        sender: bool,
    ) -> Channel {
        let k = |label: &[u8]| {
            let mut h = Sha256::new();
            h.update(label)
                .update(shared)
                .update(nonce_r.as_bytes())
                .update(nonce_s.as_bytes())
                .update(dh_r)
                .update(dh_s);
            h.finish()
        };
        let (s2r, r2s) = (
            k(alloc::format!("{ctx} key s->r").as_bytes()),
            k(alloc::format!("{ctx} key r->s").as_bytes()),
        );
        let (tx, rx) = if sender { (s2r, r2s) } else { (r2s, s2r) };
        Channel {
            tx,
            rx,
            tx_n: 0,
            rx_n: 0,
        }
    }

    fn nonce(n: u64) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[4..].copy_from_slice(&n.to_be_bytes());
        b
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        crypto::wipe(&mut self.tx);
        crypto::wipe(&mut self.rx);
    }
}

pub fn offer_msg(nonce_r: &str, nonce_s: &str, name: &str, size: u64, sha: &str) -> String {
    alloc::format!(
        "milkdrop-offer-v1 {nonce_r} {nonce_s} {} {size} {sha}",
        esc(name)
    )
}

/// A TCP connection with a line reader on top.
pub struct Conn {
    pub fd: i32,
    buf: Vec<u8>,
}

impl Conn {
    pub fn new(fd: i32, timeout_ms: u64) -> Conn {
        let _ = net::set_recv_timeout(fd, timeout_ms);
        Conn {
            fd,
            buf: Vec::new(),
        }
    }

    pub fn set_timeout(&self, ms: u64) {
        let _ = net::set_recv_timeout(self.fd, ms);
    }

    fn fill(&mut self) -> Result<(), String> {
        let mut b = [0u8; 4096];
        match net::recv(self.fd, &mut b) {
            Ok(0) => Err(String::from("the peer closed the connection")),
            Ok(n) => {
                self.buf.extend_from_slice(&b[..n]);
                Ok(())
            }
            Err(e) if e.0 == 11 => Err(String::from("timed out waiting for the peer")),
            Err(e) => Err(alloc::format!("receive: {e}")),
        }
    }

    pub fn line(&mut self) -> Result<String, String> {
        loop {
            if let Some(i) = self.buf.iter().position(|&c| c == b'\n') {
                let l: Vec<u8> = self.buf.drain(..=i).collect();
                return Ok(String::from_utf8_lossy(&l[..i]).into_owned());
            }
            if self.buf.len() > MAX_LINE {
                return Err(String::from("protocol line too long"));
            }
            self.fill()?;
        }
    }

    pub fn send(&mut self, b: &[u8]) -> Result<(), String> {
        let mut off = 0;
        while off < b.len() {
            match net::send(self.fd, &b[off..]) {
                Ok(0) => return Err(String::from("the peer closed the connection")),
                Ok(n) => off += n,
                Err(e) if e.0 == 4 => {}
                Err(e) => return Err(alloc::format!("send: {e}")),
            }
        }
        Ok(())
    }

    pub fn send_line(&mut self, l: &str) -> Result<(), String> {
        self.send(alloc::format!("{l}\n").as_bytes())
    }

    /// Exactly `n` bytes.
    fn exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() < n {
            self.fill()?;
        }
        Ok(self.buf.drain(..n).collect())
    }

    /// One encrypted record: `[length: u32 BE][ciphertext][tag: 16]`, the length as AAD,
    /// nonce = the direction's counter.
    pub fn send_rec(&mut self, ch: &mut Channel, pt: &[u8]) -> Result<(), String> {
        if pt.len() > MAX_RECORD {
            return Err(String::from("record too large"));
        }
        let len = (pt.len() as u32).to_be_bytes();
        let mut rec = Vec::with_capacity(4 + pt.len() + aead::TAG);
        rec.extend_from_slice(&len);
        rec.extend_from_slice(pt);
        let tag = aead::seal(&ch.tx, &Channel::nonce(ch.tx_n), &len, &mut rec[4..]);
        ch.tx_n += 1;
        rec.extend_from_slice(&tag);
        self.send(&rec)
    }

    /// The next record, decrypted and authenticated (anything altered, replayed, dropped
    /// or reordered fails the tag: the counter is in the nonce).
    pub fn recv_rec(&mut self, ch: &mut Channel) -> Result<Vec<u8>, String> {
        let h = self.exact(4)?;
        let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
        if len > MAX_RECORD {
            return Err(String::from("record too large"));
        }
        let mut body = self.exact(len + aead::TAG)?;
        let tag: [u8; 16] = body[len..].try_into().unwrap();
        body.truncate(len);
        if !aead::open(&ch.rx, &Channel::nonce(ch.rx_n), &h, &mut body, &tag) {
            return Err(String::from(
                "a record failed authentication (altered on the wire?); connection dropped",
            ));
        }
        ch.rx_n += 1;
        Ok(body)
    }

    pub fn send_eline(&mut self, ch: &mut Channel, l: &str) -> Result<(), String> {
        self.send_rec(ch, l.as_bytes())
    }

    pub fn eline(&mut self, ch: &mut Channel) -> Result<String, String> {
        let b = self.recv_rec(ch)?;
        Ok(String::from_utf8_lossy(&b).into_owned())
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = rt::sys::close(self.fd);
    }
}
