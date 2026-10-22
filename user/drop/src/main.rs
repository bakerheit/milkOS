//! drop: Milk Drop, AirDrop-style file sharing between milkOS machines on the VM-to-VM
//! link (eth1). Design and wire formats: docs/networking.md, "Milk Drop".
//!
//!   drop id                    this machine's name, fingerprint and eth1 address
//!   drop id --rotate           a new key (kernel CSPRNG), signed by the old one
//!   drop selftest              the crypto test vectors (ChaCha20, Poly1305, X25519, ...)
//!   drop name NEW              change the name peers see
//!   drop daemon [-q]           announce + answer queries (init starts it when eth1 exists)
//!   drop list [-w MS]          peers on the link (MILK_TABLE=1: a table)
//!   drop send PEER FILE        PEER: a name, NAME.local, a fingerprint prefix or an IPv4 address
//!   drop recv [-y] [-k]        accept one transfer into ~/Downloads (-y: no prompt,
//!                              -k: keep receiving until Ctrl-C)
//!   drop peers                 peers trusted on first use
//!   drop forget NAME           stop trusting NAME (after a legitimate key change)
//!
//! Trust: every machine has an Ed25519 identity (/System/Settings/milk/drop, made on first use from
//! the kernel CSPRNG). It is root's: the daemon (a root service) holds the secret and
//! signs hellos and offers for other users' `drop` over /System/Services/Sockets/drop.sock (SO_PEERCRED;
//! renaming and rotating need an administrator). Each user trusts peers on their own
//! (~/.config/milk/drop/known_peers); files land in that user's ~/Downloads, theirs. Each connection makes fresh X25519 keys; both sides sign a hello
//! transcript (both nonces, both identity keys, both X25519 keys), so the identities
//! authenticate the key agreement. Everything after the hello (offer, answer, data,
//! verdict) is ChaCha20-Poly1305 records, one key per direction, counter nonces. The
//! sender signs the offer (name, size, SHA-256), and the receiver checks the SHA-256
//! before the file appears. A peer's key is trusted on first use (fingerprint shown); a
//! known name with a different key is refused loudly, unless the old key signed the new
//! one (`drop id --rotate`). Every send / receive / refusal is an audit record
//! (`user.drop.*`).

#![no_std]
#![no_main]

extern crate alloc;

use milkdrop::session::{self, DROP};
use milkdrop::{audit, ident, ip_str, proto};

use alloc::string::String;
use alloc::vec::Vec;
use ident::{Identity, Trust, fingerprint, hex};
use proto::{Conn, MAGIC, Peer, XFER_PORT, field};
use rt::net::{self, SockAddr};
use rt::sha256::Sha256;
use rt::{ed25519, eprintln, mdns, print, println, sys};

rt::main!(main);

type Res<T> = Result<T, String>;

const PEERS_CACHE: &str = "/tmp/drop/peers";
const RECEIVING: &str = "/tmp/drop/receiving";
/// Sender side: largest file (it is read into memory).
const MAX_SEND: u64 = 64 << 20;
const CHUNK: usize = 16 * 1024;

fn text(p: &str) -> Option<String> {
    sys::read_file(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// eth1's IPv6 address on the VM link prefix (fd6d:696c:6b00:1::/64), once DAD is done.
fn eth1_addr6() -> Option<rt::net::Ip> {
    let t = text("/proc/net/ifaddr")?;
    let l = t.lines().find(|l| l.starts_with("eth1 "))?;
    l.split(' ')
        .filter_map(|w| w.strip_prefix("inet6="))
        .filter(|v| !v.contains(",tentative"))
        .filter_map(|v| rt::net::Ip::parse(v.split('/').next()?))
        .find(proto::on_vm_link6)
}

/// Does eth1 run IPv4 at all (its link-local probing, `linklocal=` in ifaddr)? Not in a
/// v6-only boot.
fn eth1_has_v4() -> bool {
    text("/proc/net/ifaddr").is_some_and(|t| {
        t.lines()
            .find(|l| l.starts_with("eth1 "))
            .is_some_and(|l| l.contains(" linklocal="))
    })
}

/// eth1's IPv4 address, if the link is up and configured.
fn eth1_addr() -> Option<[u8; 4]> {
    let t = text("/proc/net/ifaddr")?;
    let l = t.lines().find(|l| l.starts_with("eth1 "))?;
    let a = l.split(' ').find_map(|w| w.strip_prefix("inet="))?;
    SockAddr::parse(a, 0).and_then(|s| s.ip.v4())
}

fn hostname() -> String {
    sys::uname()
        .map(|u| u[1].clone())
        .unwrap_or_else(|_| String::from("milk"))
}

fn identity() -> Res<Identity> {
    let (id, created) = ident::load_or_create()?;
    if created {
        println!(
            "drop: new identity {} (fingerprint {}) in {}",
            id.name,
            fingerprint(&id.public),
            ident::DIR
        );
        audit(
            "drop.identity",
            &alloc::format!(
                "name={} fp={} key={}",
                id.name,
                ident::fp_compact(&id.public),
                hex(&id.public)
            ),
        );
    }
    Ok(id)
}

fn kib(n: u64) -> String {
    if n < 1024 {
        alloc::format!("{n} B")
    } else {
        alloc::format!("{} KiB", n.div_ceil(1024))
    }
}

/// Progress, printed every 10%: `[#####.....]  50%  512 KiB / 1024 KiB`.
struct Progress {
    total: u64,
    last: u64,
}

impl Progress {
    fn new(total: u64) -> Progress {
        Progress {
            total,
            last: u64::MAX,
        }
    }
    fn at(&mut self, done: u64) {
        let pct = (done * 100).checked_div(self.total).unwrap_or(100);
        let step = pct / 10;
        if step == self.last {
            return;
        }
        self.last = step;
        let bar: String = (0..10).map(|i| if i < step { '#' } else { '.' }).collect();
        println!(
            "drop: [{bar}] {pct:>3}%  {} / {}",
            kib(done),
            kib(self.total)
        );
    }
}

// ---- commands ----------------------------------------------------------------------------

fn cmd_id() -> Res<()> {
    let id = identity()?;
    println!("name:        {}", id.name);
    println!("fingerprint: {}", fingerprint(&id.public));
    println!("public key:  {}", hex(&id.public));
    let on: Vec<String> = eth1_addr()
        .map(rt::net::Ip::V4)
        .into_iter()
        .chain(eth1_addr6())
        .map(|a| alloc::format!("{a}"))
        .collect();
    if on.is_empty() {
        println!("link:        no eth1 (Milk Drop needs the VM-to-VM link)");
    } else {
        println!(
            "link:        eth1 {} (transfers on port {XFER_PORT})",
            on.join(" and ")
        );
    }
    println!("keys:        {}", ident::DIR);
    if let Some(r) = ident::rotation(&id.public) {
        println!("rotated:     from {} (signed by it)", fingerprint(&r.old));
    }
    if ident::legacy_key() {
        warn_legacy();
    } else {
        println!("rng:         kernel CSPRNG (ChaCha20, virtio-rng seeded)");
    }
    Ok(())
}

fn warn_legacy() {
    eprintln!("drop: WARNING: this identity was made with the old, weak random generator");
    eprintln!("drop: (before the kernel CSPRNG). Its secret key may be guessable. Replace it:");
    eprintln!("drop:     drop id --rotate");
    eprintln!("drop: (the old key signs the new one, so peers that trusted it move over)");
}

/// `drop id --rotate`: a new key from the kernel CSPRNG, signed by the old one.
fn cmd_rotate() -> Res<()> {
    let id = identity()?;
    let local = id.seed.is_some();
    let (old, new) = ident::rotate(&id)?;
    // Through the daemon, the daemon writes the record (with who asked).
    if local {
        audit(
            "drop.rotate",
            &alloc::format!(
                "name={} old_fp={} new_fp={} key={}",
                new.name,
                ident::fp_compact(&old),
                ident::fp_compact(&new.public),
                hex(&new.public)
            ),
        );
    }
    println!("drop: rotated {}'s identity", new.name);
    println!("drop:   old fingerprint: {}", fingerprint(&old));
    println!("drop:   new fingerprint: {}", fingerprint(&new.public));
    println!("drop: the old key signed the new one: peers that trusted it accept the new key");
    println!("drop: on next contact. The old secret is gone.");
    Ok(())
}

/// `drop selftest`: the shared crypto crate's test vectors, run in userland.
fn cmd_selftest() -> Res<()> {
    let mut all = true;
    let mut v = rt::crypto::selftest();
    let (p, t) = mdns::selftest();
    v.push(("mdns", p, t));
    let (p, t) = net::selftest();
    v.push(("addresses", p, t));
    for (name, p, t) in v {
        println!("drop: {name:<17} {p}/{t} vectors");
        all &= p == t;
    }
    if all {
        println!("drop: selftest ok");
        Ok(())
    } else {
        Err(String::from("selftest FAILED"))
    }
}

fn cmd_name(new: &str) -> Res<()> {
    let mut id = identity()?;
    let old = id.name.clone();
    ident::set_name(&mut id, new)?;
    if id.seed.is_some() {
        audit("drop.rename", &alloc::format!("old={old} new={new}"));
    }
    println!("drop: this machine is now {new} (was {old})");
    Ok(())
}

/// Announce every 10 s and answer queries at once. Keeps /tmp/drop/peers (what it heard).
fn cmd_daemon(quiet: bool) -> Res<()> {
    let mut id = identity()?;
    // An identity from the old, weak RNG is replaced at boot: the old key signs the new
    // one, so peers that trusted it follow (docs/networking.md, "Old keys must be rotated").
    if ident::legacy_key() {
        let (old, new) = ident::rotate(&id)?;
        println!(
            "drop: {}'s identity came from the old weak RNG: rotated it ({} -> {}, signed by the old key)",
            new.name,
            fingerprint(&old),
            fingerprint(&new.public)
        );
        audit(
            "drop.rotate",
            &alloc::format!(
                "name={} old_fp={} new_fp={} key={} reason=legacy-rng",
                new.name,
                ident::fp_compact(&old),
                ident::fp_compact(&new.public),
                hex(&new.public)
            ),
        );
        id = new;
    }
    // eth1's addresses: IPv4 link-local (169.254/16, a few seconds of ARP probing) unless
    // the link runs without IPv4, and the IPv6 VM-link address (DAD: half a second).
    let (mut addr, mut addr6) = (None, None);
    for _ in 0..100 {
        addr = eth1_addr();
        addr6 = eth1_addr6();
        if (addr.is_some() || !eth1_has_v4()) && addr6.is_some() {
            break;
        }
        sys::sleep_ms(100);
    }
    if addr.is_none() && addr6.is_none() {
        return Err(String::from(
            "no eth1 address (no VM-to-VM link); not announcing",
        ));
    }
    // Per family: the discovery socket (port 5359, with its group) and the mDNS responder
    // (5353), bound to eth1's address so nothing on eth0 reaches them. Over IPv6 both join
    // ff02::fb on eth1 (MLD), or the link wouldn't take the group's traffic.
    let bind_udp = |a: SockAddr| -> Option<i32> {
        let s = net::socket_for(&a, net::SOCK_DGRAM).ok()?;
        if a.ip.is_v6() {
            let _ = net::set_v6only(s, true);
        }
        let joined = !a.ip.is_v6() || net::group6(s, mdns::GROUP6, 3, true).is_ok();
        if joined && net::bind(s, &a).is_ok() {
            let _ = net::set_recv_timeout(s, 5);
            Some(s)
        } else {
            eprintln!("drop: can't use {a} (bind or group join failed)");
            let _ = sys::close(s);
            None
        }
    };
    let mut disc: Vec<(i32, SockAddr)> = Vec::new();
    let mut mdns_fds: Vec<i32> = Vec::new();
    let mut names: Vec<rt::net::Ip> = Vec::new();
    if let Some(a) = addr {
        let ip = rt::net::Ip::V4(a);
        disc.extend(bind_udp(SockAddr::new(ip, proto::DISC_PORT)).map(|s| (s, proto::group())));
        mdns_fds.extend(bind_udp(SockAddr::new(ip, mdns::PORT)));
        names.push(ip);
    }
    if let Some(a6) = addr6 {
        disc.extend(bind_udp(SockAddr::new(a6, proto::DISC_PORT)).map(|s| (s, proto::group6())));
        mdns_fds.extend(bind_udp(SockAddr::new(a6, mdns::PORT)));
        names.push(a6);
    }
    if disc.is_empty() {
        return Err(String::from("no discovery socket on eth1; not announcing"));
    }
    shared_tmp();
    let ctl = control_socket();
    if let Err(e) = &ctl {
        eprintln!(
            "drop: {} unavailable ({e}): other users can't send or receive",
            ident::SOCK
        );
    }
    let ctl = ctl.ok();
    if !quiet {
        let on: Vec<String> = names.iter().map(|a| alloc::format!("{a}")).collect();
        let groups: Vec<String> = disc.iter().map(|(_, g)| alloc::format!("{g}")).collect();
        println!(
            "drop: {} (fingerprint {}) on eth1 {}, announcing to {}, answering mDNS for {}.local",
            id.name,
            fingerprint(&id.public),
            on.join(" and "),
            groups.join(" and "),
            id.name
        );
    }
    let host = hostname();
    let mut heard: Vec<Peer> = Vec::new();
    let mut seen: Vec<([u8; 32], u64)> = Vec::new();
    let mut next = 0u64;
    let mut buf = [0u8; 1500];
    let mut me = id;
    // What we say about ourselves, read fresh for every announcement and answer: the name
    // may change (`drop name`), `drop recv` comes and goes. The daemon never makes a key.
    let current = |me: &mut Identity| -> String {
        if let Some(now) = ident::load() {
            *me = now;
        }
        proto::announce(me, &host, sys::stat_path(RECEIVING, false).is_ok(), addr6)
    };
    let query = alloc::format!("{MAGIC} QUERY");
    loop {
        let now = sys::clock_ns(sys::CLOCK_MONOTONIC);
        // Hidden (`drop visible off`, `network hide`): no announcements, no answers.
        let visible = ident::visible();
        if now >= next && visible {
            // Both families: IPv4 for older peers, ff02::fb for IPv6 ones.
            let ann = current(&mut me);
            for (s, g) in &disc {
                let _ = net::sendto(*s, ann.as_bytes(), g);
            }
            next = now + 10_000_000_000;
        }
        // Poll set: discovery sockets, then mDNS sockets, then the control socket.
        let mut p: Vec<net::PollFd> = disc
            .iter()
            .map(|(s, _)| *s)
            .chain(mdns_fds.iter().copied())
            .chain(ctl)
            .map(|fd| net::PollFd {
                fd,
                events: net::POLLIN,
                revents: 0,
            })
            .collect();
        if net::poll(&mut p, Some(1000)).is_err() {
            sys::sleep_ms(100);
            continue;
        }
        let ready = |k: usize| p[k].revents & net::POLLIN != 0;
        if let Some(l) = ctl
            && ready(p.len() - 1)
        {
            serve_control(l);
        }
        for (k, &s) in mdns_fds.iter().enumerate() {
            if ready(disc.len() + k)
                && let Ok((n, from)) = net::recvfrom(s, &mut buf)
            {
                if let Some(now) = ident::load() {
                    me = now;
                }
                // Read now, not at the top of the loop: poll may have waited a second.
                if ident::visible() {
                    answer_mdns(s, &buf[..n], from, &me.name, &names);
                }
            }
        }
        for (k, &(s, _)) in disc.iter().enumerate() {
            if !ready(k) {
                continue;
            }
            let Ok((n, from)) = net::recvfrom(s, &mut buf) else {
                continue;
            };
            let line = String::from_utf8_lossy(&buf[..n]);
            let line = line.trim_end();
            if line == query {
                if ident::visible() {
                    let _ = net::sendto(s, current(&mut me).as_bytes(), &from);
                }
            } else if let Some(peer) = proto::parse_announce(line, from)
                && peer.key != me.public
            {
                let key = peer.key;
                proto::merge(&mut heard, peer);
                let t = sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000;
                seen.retain(|(k, _)| *k != key);
                seen.push((key, t));
                let mut out = String::new();
                for p in &heard {
                    let t = seen.iter().find(|(k, _)| *k == p.key).map_or(0, |x| x.1);
                    out.push_str(&alloc::format!(
                        "{} {} {} {} {}\n",
                        p.name,
                        ip_str(p.addr),
                        ident::fp_compact(&p.key),
                        p.receiving as u8,
                        t
                    ));
                }
                let _ = ident::write_atomic(PEERS_CACHE, out.as_bytes(), 0o644);
            }
        }
    }
}

/// /tmp/drop holds the daemon's peer cache and the "someone is receiving" marker that any
/// user's `drop recv` writes: sticky and world-writable, like /tmp.
fn shared_tmp() {
    let _ = sys::mkdir("/tmp/drop", 0o1777);
    if sys::geteuid() == 0 {
        let _ = sys::chmod("/tmp/drop", 0o1777);
    }
}

/// The daemon's request socket (anyone may connect; requests are checked by SO_PEERCRED).
fn control_socket() -> Res<i32> {
    use rt::unix::{self, Name};
    let _ = sys::mkdir("/System/Services/Sockets", 0o755);
    let _ = sys::unlink(ident::SOCK);
    let l = unix::socket(unix::SOCK_STREAM | unix::SOCK_CLOEXEC)
        .map_err(|e| alloc::format!("socket: {e}"))?;
    unix::bind(l, &Name::Path(ident::SOCK)).map_err(|e| alloc::format!("bind: {e}"))?;
    let _ = sys::chmod(ident::SOCK, 0o666);
    unix::listen(l, 8).map_err(|e| alloc::format!("listen: {e}"))?;
    Ok(l)
}

/// One request on /System/Services/Sockets/drop.sock: read a line (a client gets 2 s), answer, hang up.
fn serve_control(l: i32) {
    use rt::unix;
    let Ok(c) = unix::accept(l, unix::SOCK_CLOEXEC) else {
        return;
    };
    let peer = unix::peer_cred(c).unwrap_or(unix::Cred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    });
    let mut line = Vec::new();
    let mut b = [0u8; 2048];
    for _ in 0..2 {
        let mut pf = [net::PollFd {
            fd: c,
            events: net::POLLIN,
            revents: 0,
        }];
        if !matches!(net::poll(&mut pf, Some(1000)), Ok(1..)) {
            continue;
        }
        match sys::read(c, &mut b) {
            Ok(0) | Err(_) => break,
            Ok(n) => line.extend_from_slice(&b[..n]),
        }
        if line.ends_with(b"\n") || line.len() > 8192 {
            break;
        }
    }
    let reply = control_request(String::from_utf8_lossy(&line).trim(), peer);
    let _ = sys::write_all(c, reply.as_bytes());
    let _ = sys::close(c);
}

fn unhex_vec(h: &str) -> Option<Vec<u8>> {
    if h.len() % 2 != 0 || h.len() > 8192 {
        return None;
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(h.get(i..i + 2)?, 16).ok())
        .collect()
}

/// `sign HEX` (people's accounts: root and uid 500..60000, not service accounts such as
/// nobody): a Milk Drop hello transcript or offer that names this machine's key;
/// `name NEW` and `rotate`: administrators (root or group admin) only.
fn control_request(line: &str, peer: rt::unix::Cred) -> String {
    const ADMIN_GID: u32 = 80;
    let acct = rt::users::by_uid(peer.uid);
    let admin = peer.uid == 0
        || acct
            .as_ref()
            .is_some_and(|p| rt::users::groups_of(&p.name, p.gid).contains(&ADMIN_GID));
    let person = peer.uid == 0 || (500..60_000).contains(&peer.uid);
    let who = alloc::format!(
        "uid={} user={}",
        peer.uid,
        acct.as_ref().map_or("?", |p| p.name.as_str())
    );
    let Some(mut id) = ident::load() else {
        return String::from("error this machine has no Milk Drop identity\n");
    };
    let (cmd, arg) = line.split_once(' ').unwrap_or((line, ""));
    let r: Res<String> = match cmd {
        "sign" if !person => Err(String::from(
            "Milk Drop is for people's accounts, not service accounts",
        )),
        "sign" => match unhex_vec(arg) {
            // A hello transcript must name this machine's key (one side of it); an offer
            // binds the session's two nonces instead.
            Some(msg)
                if core::str::from_utf8(&msg).is_ok_and(|m| {
                    (m.starts_with(ident::SIGNABLE[0]) && m.contains(&hex(&id.public)))
                        || m.starts_with(ident::SIGNABLE[1])
                }) =>
            {
                id.sign(&msg).map(|s| hex(&s))
            }
            _ => Err(String::from(
                "not a Milk Drop hello or offer for this machine's key",
            )),
        },
        "name" | "rotate" | "visible" if !admin => {
            audit(
                "drop.refused",
                &alloc::format!("{who} request={cmd} reason=not-an-administrator"),
            );
            Err(alloc::format!(
                "{cmd}: only an administrator may change this machine's identity"
            ))
        }
        "name" => {
            let old = id.name.clone();
            ident::set_name(&mut id, arg).map(|_| {
                audit(
                    "drop.rename",
                    &alloc::format!("old={old} new={arg} by {who}"),
                );
                String::new()
            })
        }
        "visible" if matches!(arg, "on" | "off") => ident::set_visible(arg == "on").map(|_| {
            audit("drop.visible", &alloc::format!("visible={arg} by {who}"));
            String::new()
        }),
        "rotate" => ident::rotate(&id).map(|(old, new)| {
            audit(
                "drop.rotate",
                &alloc::format!(
                    "name={} old_fp={} new_fp={} key={} by {who}",
                    new.name,
                    ident::fp_compact(&old),
                    ident::fp_compact(&new.public),
                    hex(&new.public)
                ),
            );
            alloc::format!("{} {}", hex(&old), hex(&new.public))
        }),
        _ => Err(String::from(
            "unknown request (sign HEX | name NEW | rotate)",
        )),
    };
    match r {
        Ok(v) if v.is_empty() => String::from("ok\n"),
        Ok(v) => alloc::format!("ok {v}\n"),
        Err(e) => alloc::format!("error {e}\n"),
    }
}

/// Answer an mDNS query (over either family) if it asks for `<name>.local`: A, AAAA or
/// ANY, with eth1's IPv4 and IPv6 addresses (`addrs`) as asked. Straight back to the
/// asker for a legacy unicast query (source port not 5353, as rt::mdns::resolve sends),
/// else to the group of the family it came in on.
fn answer_mdns(fd: i32, pkt: &[u8], from: SockAddr, name: &str, addrs: &[rt::net::Ip]) {
    let Some((id, qs)) = mdns::parse_query(pkt) else {
        return;
    };
    let mine = alloc::format!("{name}.local");
    let types: Vec<u16> = qs
        .iter()
        .filter(|(q, _)| q.eq_ignore_ascii_case(&mine))
        .map(|(_, ty)| *ty)
        .collect();
    let legacy = from.port != mdns::PORT;
    if let Some(reply) = mdns::answer_packet(
        if legacy { id } else { 0 },
        &mine,
        &types,
        addrs,
        10,
        legacy,
    ) {
        let to = match (legacy, from.ip.is_v6()) {
            (true, _) => from,
            (false, true) => mdns::group6(),
            (false, false) => mdns::group(),
        };
        let _ = net::sendto(fd, &reply, &to);
    }
}

fn trust_word(p: &Peer) -> String {
    match ident::trust_rotated(&p.name, &p.key, p.rot.as_ref()) {
        Trust::Rotated { .. } => String::from("rotated"),
        Trust::Known { as_name } if as_name == p.name => String::from("known"),
        Trust::Known { as_name } => alloc::format!("known as {as_name}"),
        Trust::New => String::from("new"),
        Trust::Changed { .. } => String::from("KEY CHANGED"),
    }
}

fn cmd_list(ms: u64, table: bool) -> Res<()> {
    let peers = proto::query(ms)?;
    use rt::table::{Table, Ty, Val};
    let mut t = Table::new(&[
        ("name", Ty::Str),
        ("address", Ty::Str),
        ("fingerprint", Ty::Str),
        ("receiving", Ty::Str),
        ("trust", Ty::Str),
        ("host", Ty::Str),
    ]);
    for p in &peers {
        t.push(alloc::vec![
            Val::str(&p.name),
            Val::str(&ip_str(p.addr)),
            Val::str(&fingerprint(&p.key)),
            Val::str(if p.receiving { "yes" } else { "no" }),
            Val::str(&trust_word(p)),
            Val::str(&p.host),
        ]);
    }
    if table {
        t.write(&mut rt::io::Out::stdout());
        return Ok(());
    }
    if peers.is_empty() {
        println!("drop: nobody answered on eth1 (is the other machine up?)");
        return Ok(());
    }
    let mut out = rt::io::Out::stdout();
    t.render(&mut out);
    out.flush();
    Ok(())
}

fn cmd_peers() -> Res<()> {
    let k = ident::known();
    if k.is_empty() {
        println!("drop: no trusted peers yet");
    }
    for e in k {
        println!("{:<20} {}  since {}", e.name, fingerprint(&e.key), e.since);
    }
    Ok(())
}

fn cmd_forget(name: &str) -> Res<()> {
    if ident::forget(name)? {
        audit("drop.forget", &alloc::format!("name={name}"));
        println!("drop: forgot {name}; its next key will be trusted on first use");
        Ok(())
    } else {
        Err(alloc::format!("{name} is not a trusted peer"))
    }
}

/// Where to send for `spec` (an address, `NAME.local`, a drop name or fingerprint
/// prefix): the addresses to try in order, and the announcement it matched, if any. A
/// peer found by discovery is reached over IPv4 unless `want` is V6 (then its announced
/// eth1 IPv6 address); `.local` names come back IPv6 first, as getaddrinfo orders them.
fn resolve(spec: &str, want: net::Want) -> Res<(Vec<SockAddr>, Option<Peer>)> {
    if let Some(a) = SockAddr::parse(spec, XFER_PORT) {
        return Ok((alloc::vec![a], None));
    }
    if mdns::is_local(spec) {
        let ips = mdns::resolve(spec, 1500, want)?;
        return Ok((
            ips.into_iter()
                .map(|ip| SockAddr::new(ip, XFER_PORT))
                .collect(),
            None,
        ));
    }
    let peers = proto::query(1500)?;
    let flat = spec.replace(' ', "").to_ascii_lowercase();
    let by_name: Vec<&Peer> = peers.iter().filter(|p| p.name == spec).collect();
    let pick: Vec<&Peer> = if !by_name.is_empty() {
        by_name
    } else if flat.len() >= 4 && flat.bytes().all(|c| c.is_ascii_hexdigit()) {
        peers
            .iter()
            .filter(|p| ident::fp_compact(&p.key).starts_with(&flat))
            .collect()
    } else {
        Vec::new()
    };
    match pick.as_slice() {
        [p] if want == net::Want::V6 => match p.addr6 {
            Some(a6) => Ok((alloc::vec![SockAddr::new(a6, p.port)], Some((*p).clone()))),
            None => Err(alloc::format!("{} announces no IPv6 address", p.name)),
        },
        [p] => Ok((
            alloc::vec![SockAddr::new(p.addr, p.port)],
            Some((*p).clone()),
        )),
        [] => Err(alloc::format!(
            "no peer {spec:?} on the link (`drop list` shows who is around)"
        )),
        _ => Err(alloc::format!(
            "{spec:?} matches several peers; use more of the fingerprint"
        )),
    }
}

fn cmd_send(spec: &str, path: &str, want: net::Want) -> Res<()> {
    let id = identity()?;
    let data = sys::read_file(path).map_err(|e| alloc::format!("{path}: {e}"))?;
    if data.len() as u64 > MAX_SEND {
        return Err(alloc::format!("{path}: larger than {} MiB", MAX_SEND >> 20));
    }
    let fname = path.rsplit('/').next().unwrap_or(path);
    let sha = hex(&rt::sha256::sha256(&data));
    let (addrs, announced) = resolve(spec, want)?;
    // The first address that takes the connection (refused everywhere: not receiving).
    let mut conn = None;
    let mut refused = false;
    let mut last = String::new();
    for addr in &addrs {
        let fd =
            net::socket_for(addr, net::SOCK_STREAM).map_err(|e| alloc::format!("socket: {e}"))?;
        match net::connect(fd, addr) {
            Ok(()) => {
                conn = Some((fd, *addr));
                break;
            }
            Err(e) => {
                let _ = sys::close(fd);
                refused |= e.0 == 111;
                last = alloc::format!("connect {addr}: {e}");
            }
        }
    }
    let Some((fd, addr)) = conn else {
        return Err(if refused {
            alloc::format!("{spec} isn't receiving (run `drop recv` on it)")
        } else {
            last
        });
    };
    let mut c = Conn::new(fd, 10_000);
    // The authenticated X25519 key agreement (milkdrop::session); encrypted from here on.
    let mut sess = session::connect(
        &mut c,
        &id,
        &DROP,
        announced.as_ref().map(|p| &p.key),
        addr.ip,
    )?;
    let (rname, rkey) = (sess.name.clone(), sess.key);
    let rname = rname.as_str();
    match &sess.trust {
        Trust::New => {
            println!(
                "drop: {rname} is new: fingerprint {} (trusted on first use)",
                fingerprint(&rkey)
            );
            ident::remember(rname, &rkey)?;
            audit(
                "drop.trust",
                &alloc::format!("name={rname} fp={} role=send", ident::fp_compact(&rkey)),
            );
        }
        Trust::Known { as_name } if as_name != rname => {
            println!("drop: {as_name} is now called {rname} (same key)");
            ident::remember(rname, &rkey)?;
        }
        Trust::Rotated { .. } => ident::remember(rname, &rkey)?,
        _ => println!("drop: {rname} ({}) verified", fingerprint(&rkey)),
    }
    let (nonce_r, nonce_s) = (sess.nonce_s.clone(), sess.nonce_c.clone());
    let (nonce_r, nonce_s) = (nonce_r.as_str(), nonce_s.as_str());
    let ch = &mut sess.ch;
    // 4. The signed offer.
    let size = data.len() as u64;
    let osig = id.sign(proto::offer_msg(nonce_r, nonce_s, fname, size, &sha).as_bytes())?;
    c.send_eline(
        ch,
        &alloc::format!(
            "OFFER name={} size={size} sha256={sha} sig={}",
            proto::esc(fname),
            hex(&osig)
        ),
    )?;
    println!(
        "drop: offering {fname} ({}, sha256 {}...) to {rname}, encrypted (X25519 + ChaCha20-Poly1305); waiting for it to accept",
        kib(size),
        &sha[..16]
    );
    c.set_timeout(120_000);
    let r = c.eline(ch)?;
    if r != "ACCEPT" {
        let reason = r.strip_prefix("DECLINE ").unwrap_or(&r);
        audit(
            "drop.send",
            &alloc::format!(
                "peer={rname} fp={} file={} size={size} sha256={sha} result=declined reason={reason}",
                ident::fp_compact(&rkey),
                proto::esc(fname)
            ),
        );
        return Err(alloc::format!("{rname} declined ({reason})"));
    }
    // 5. The data, one record per chunk, then the receiver's verdict.
    c.set_timeout(20_000);
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let mut prog = Progress::new(size);
    let mut off = 0usize;
    prog.at(0);
    while off < data.len() {
        let n = CHUNK.min(data.len() - off);
        c.send_rec(ch, &data[off..off + n])?;
        off += n;
        prog.at(off as u64);
    }
    let v = c.eline(ch)?;
    let ms = ((sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000).max(1);
    let ok = v == alloc::format!("DONE sha256={sha}");
    audit(
        "drop.send",
        &alloc::format!(
            "peer={rname} fp={} addr={} file={} size={size} sha256={sha} enc=x25519-chacha20poly1305 result={}",
            ident::fp_compact(&rkey),
            ip_str(addr.ip),
            proto::esc(fname),
            if ok { "ok" } else { "bad" }
        ),
    );
    if !ok {
        return Err(alloc::format!(
            "{rname} reports the data did not verify ({v})"
        ));
    }
    println!(
        "drop: sent {fname} to {rname}: {} in {ms} ms ({} KiB/s), sha256 verified by {rname}",
        kib(size),
        size * 1000 / 1024 / ms
    );
    Ok(())
}

/// A safe file name for ~/Downloads: the last path part, no leading dots, no controls.
fn safe_name(n: &str) -> String {
    let base = n.rsplit('/').next().unwrap_or(n).trim_start_matches('.');
    let s: String = base.chars().filter(|c| !c.is_control()).take(100).collect();
    if s.is_empty() {
        String::from("file")
    } else {
        s
    }
}

fn unique(dir: &str, name: &str) -> String {
    let p = alloc::format!("{dir}/{name}");
    if sys::stat_path(&p, false).is_err() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, alloc::format!(".{e}")),
        _ => (name, String::new()),
    };
    for i in 1.. {
        let p = alloc::format!("{dir}/{stem} ({i}){ext}");
        if sys::stat_path(&p, false).is_err() {
            return p;
        }
    }
    unreachable!()
}

fn ask(q: &str) -> bool {
    print!("{q}");
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while let Ok(1) = sys::read(0, &mut b) {
        if b[0] == b'\n' {
            break;
        }
        line.push(b[0]);
    }
    matches!(
        String::from_utf8_lossy(&line).trim(),
        "y" | "Y" | "yes" | "YES" | "Yes"
    )
}

fn downloads(envp: &[&str]) -> String {
    let home = envp
        .iter()
        .find_map(|e| e.strip_prefix("HOME="))
        .unwrap_or("/Users/milk");
    alloc::format!("{home}/Downloads")
}

/// Handle one incoming connection. Ok(true) = a file was received.
fn serve(id: &Identity, c: &mut Conn, from: SockAddr, yes: bool, dir: &str) -> Res<bool> {
    // The authenticated X25519 key agreement (milkdrop::session); encrypted from here on.
    let mut sess = session::accept(c, id, &DROP, from.ip)?;
    let (sname, skey, trust) = (sess.name.clone(), sess.key, sess.trust.clone());
    let sname = sname.as_str();
    let (nonce_r, nonce_s) = (sess.nonce_s.clone(), sess.nonce_c.clone());
    let nonce_s = nonce_s.as_str();
    let ch = &mut sess.ch;
    let o = c.eline(ch)?;
    let (Some(oname), Some(size), Some(sha), Some(osig)) = (
        field(&o, "name").map(proto::unesc),
        field(&o, "size").and_then(|s| s.parse::<u64>().ok()),
        field(&o, "sha256").filter(|s| s.len() == 64),
        field(&o, "sig").and_then(ed25519::unhex::<64>),
    ) else {
        return Err(String::from("bad OFFER"));
    };
    if !o.starts_with("OFFER ")
        || !ed25519::verify(
            &skey,
            proto::offer_msg(&nonce_r, nonce_s, &oname, size, sha).as_bytes(),
            &osig,
        )
    {
        let _ = c.send_eline(ch, "DECLINE bad-offer");
        return Err(alloc::format!(
            "{sname}: offer signature doesn't verify; refused"
        ));
    }
    let name = safe_name(&oname);
    let fp = fingerprint(&skey);
    let who = match &trust {
        Trust::New => alloc::format!(
            "{sname} (NEW peer, fingerprint {fp}; accepting trusts this key from now on)"
        ),
        Trust::Known { as_name } if as_name != sname => {
            alloc::format!("{sname} (known as {as_name}, fingerprint {fp})")
        }
        Trust::Rotated { old } => alloc::format!(
            "{sname} (rotated key {fp}, signed by its old key {})",
            fingerprint(old)
        ),
        _ => alloc::format!("{sname} (fingerprint {fp})"),
    };
    println!(
        "drop: {who} wants to send you {name} ({}), encrypted.",
        kib(size)
    );
    let ok = if yes {
        println!("drop: accepting (-y)");
        true
    } else {
        ask("drop: accept? [y/N] ")
    };
    if !ok {
        let _ = c.send_eline(ch, "DECLINE declined-by-user");
        audit(
            "drop.decline",
            &alloc::format!(
                "peer={sname} fp={} file={} size={size}",
                ident::fp_compact(&skey),
                proto::esc(&name)
            ),
        );
        println!("drop: declined");
        return Ok(false);
    }
    session::settle_trust(&DROP, "recv", sname, &skey, &trust, from.ip)?;
    if !matches!(trust, Trust::Known { ref as_name } if as_name == sname) {
        ident::remember(sname, &skey)?;
        if matches!(trust, Trust::New) {
            audit(
                "drop.trust",
                &alloc::format!("name={sname} fp={} role=recv", ident::fp_compact(&skey)),
            );
        }
    }
    // Root receiving into a user's home (the console's HOME is /Users/milk): the folder
    // and the file belong to that user, as if they had received it themselves.
    let owner = if sys::geteuid() == 0 {
        dir.rsplit_once('/')
            .and_then(|(home, _)| sys::stat_path(home, true).ok())
            .map(|st| (st.uid, st.gid))
            .filter(|&(uid, _)| uid != 0)
    } else {
        None
    };
    if sys::mkdir(dir, 0o755).is_ok()
        && let Some((uid, gid)) = owner
    {
        let _ = sys::chown(dir, uid, gid);
    }
    let part = alloc::format!("{dir}/.{name}.part");
    let fd = sys::open_mode(
        &part,
        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
        0o644,
    )
    .map_err(|e| alloc::format!("{part}: {e}"))?;
    c.send_eline(ch, "ACCEPT")?;
    c.set_timeout(20_000);
    let mut h = Sha256::new();
    let mut got = 0u64;
    let mut prog = Progress::new(size);
    prog.at(0);
    let r: Res<()> = (|| {
        while got < size {
            let d = c.recv_rec(ch)?;
            if d.is_empty() || got + d.len() as u64 > size {
                return Err(String::from(
                    "the sender sent more (or less) than it offered",
                ));
            }
            sys::write_all(fd, &d).map_err(|e| alloc::format!("{part}: {e}"))?;
            h.update(&d);
            got += d.len() as u64;
            prog.at(got);
        }
        Ok(())
    })();
    let _ = sys::close(fd);
    if let Err(e) = r {
        let _ = sys::unlink(&part);
        audit(
            "drop.recv",
            &alloc::format!(
                "peer={sname} file={} size={size} got={got} result=failed",
                proto::esc(&name)
            ),
        );
        return Err(e);
    }
    let have = hex(&h.finish());
    if have != sha {
        let _ = c.send_eline(ch, &alloc::format!("BAD sha256={have}"));
        let _ = sys::unlink(&part);
        audit(
            "drop.recv",
            &alloc::format!(
                "peer={sname} fp={} file={} size={size} sha256={have} expected={sha} result=bad",
                ident::fp_compact(&skey),
                proto::esc(&name)
            ),
        );
        return Err(alloc::format!(
            "{name}: SHA-256 mismatch (got {have}); discarded"
        ));
    }
    let dest = unique(dir, &name);
    sys::rename(&part, &dest).map_err(|e| alloc::format!("{dest}: {e}"))?;
    if let Some((uid, gid)) = owner {
        let _ = sys::chown(&dest, uid, gid);
    }
    let _ = c.send_eline(ch, &alloc::format!("DONE sha256={have}"));
    audit(
        "drop.recv",
        &alloc::format!(
            "peer={sname} fp={} addr={} file={} size={size} sha256={have} saved={} enc=x25519-chacha20poly1305 result=ok",
            ident::fp_compact(&skey),
            ip_str(from.ip),
            proto::esc(&name),
            proto::esc(&dest)
        ),
    );
    println!(
        "drop: received {dest} ({}) from {sname}, sha256 {have} ok",
        kib(size)
    );
    Ok(true)
}

fn cmd_recv(yes: bool, keep: bool, dir: &str) -> Res<()> {
    let id = identity()?;
    // A listener per eth1 address (IPv4 link-local, IPv6 VM-link, v6-only): only eth1, a
    // transfer never arrives through eth0.
    let mut ls: Vec<(i32, SockAddr)> = Vec::new();
    let wants: Vec<SockAddr> = eth1_addr()
        .map(|a| SockAddr::v4(a, XFER_PORT))
        .into_iter()
        .chain(eth1_addr6().map(|a6| SockAddr::new(a6, XFER_PORT)))
        .collect();
    if wants.is_empty() {
        return Err(String::from(
            "no eth1 address (Milk Drop needs the VM-to-VM link)",
        ));
    }
    for me in wants {
        let s =
            net::socket_for(&me, net::SOCK_STREAM).map_err(|e| alloc::format!("socket: {e}"))?;
        if me.ip.is_v6() {
            let _ = net::set_v6only(s, true);
        }
        let _ = net::set_reuseaddr(s);
        if let Err(e) = net::bind(s, &me).and_then(|_| net::listen(s, 2)) {
            let _ = sys::close(s);
            for (o, _) in ls {
                let _ = sys::close(o);
            }
            return Err(alloc::format!("bind {me}: {e} (another drop recv?)"));
        }
        ls.push((s, me));
    }
    shared_tmp();
    let _ = ident::write_atomic(
        RECEIVING,
        alloc::format!("{}\n", sys::getpid()).as_bytes(),
        0o644,
    );
    let on: Vec<String> = ls.iter().map(|(_, a)| alloc::format!("{a}")).collect();
    println!(
        "drop: receiving as {} (fingerprint {}) on {}; files go to {}",
        id.name,
        fingerprint(&id.public),
        on.join(" and "),
        dir
    );
    let r = loop {
        let mut pf: Vec<net::PollFd> = ls
            .iter()
            .map(|&(fd, _)| net::PollFd {
                fd,
                events: net::POLLIN,
                revents: 0,
            })
            .collect();
        match net::poll(&mut pf, None) {
            Ok(_) => {}
            Err(e) if e.0 == 4 => continue,
            Err(e) => break Err(alloc::format!("poll: {e}")),
        }
        let Some(which) = pf.iter().find(|p| p.revents != 0).map(|p| p.fd) else {
            continue;
        };
        let (fd, from) = match net::accept(which) {
            Ok(x) => x,
            Err(e) if e.0 == 4 || e.0 == 11 => continue,
            Err(e) => break Err(alloc::format!("accept: {e}")),
        };
        let mut c = Conn::new(fd, 10_000);
        // One transfer (received, declined or failed), unless -k.
        match serve(&id, &mut c, from, yes, dir) {
            Ok(_) if !keep => break Ok(()),
            Ok(_) => {}
            Err(e) if !keep => break Err(e),
            Err(e) => eprintln!("drop: {e}"),
        }
    };
    let _ = sys::unlink(RECEIVING);
    for (s, _) in ls {
        let _ = sys::close(s);
    }
    r
}

fn usage() -> i32 {
    eprintln!(
        "usage: drop id [--rotate] | name NEW | list [-w MS] | send [-4|-6] PEER FILE | recv [-y] [-k]\n       drop visible [on|off]\n       drop peers | forget NAME | daemon [-q] | selftest"
    );
    2
}

fn main(args: &rt::Args) -> i32 {
    let a: Vec<&str> = args.argv.iter().skip(1).copied().collect();
    let table = rt::table::requested(args);
    let r = match a.as_slice() {
        ["id"] => cmd_id(),
        ["id", "--rotate"] => cmd_rotate(),
        ["selftest"] => cmd_selftest(),
        ["name", n] => cmd_name(n),
        ["daemon"] => cmd_daemon(false),
        ["daemon", "-q"] => cmd_daemon(true),
        ["list"] => cmd_list(1500, table),
        ["list", "-w", ms] => match ms.parse() {
            Ok(ms) => cmd_list(ms, table),
            Err(_) => return usage(),
        },
        ["peers"] => cmd_peers(),
        ["visible"] => {
            println!(
                "drop: this machine is {} on the link",
                if ident::visible() {
                    "visible"
                } else {
                    "hidden"
                }
            );
            Ok(())
        }
        ["visible", v @ ("on" | "off")] => ident::set_visible(*v == "on").map(|_| {
            println!(
                "drop: this machine is now {} on the link",
                if *v == "on" { "visible" } else { "hidden" }
            )
        }),
        ["forget", n] => cmd_forget(n),
        ["send", peer, file] => cmd_send(peer, file, net::Want::Any),
        ["send", "-4", peer, file] => cmd_send(peer, file, net::Want::V4),
        ["send", "-6", peer, file] => cmd_send(peer, file, net::Want::V6),
        ["recv", rest @ ..] if rest.iter().all(|f| matches!(*f, "-y" | "-k")) => cmd_recv(
            rest.contains(&"-y"),
            rest.contains(&"-k"),
            &downloads(&args.envp),
        ),
        _ => return usage(),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("drop: {e}");
            1
        }
    }
}
