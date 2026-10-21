//! drop: Milk Drop, AirDrop-style file sharing between milkOS machines on the VM-to-VM
//! link (eth1). Design and wire formats: docs/networking.md, "Milk Drop".
//!
//!   drop id                    this machine's name, fingerprint and eth1 address
//!   drop name NEW              change the name peers see
//!   drop daemon [-q]           announce + answer queries (init starts it when eth1 exists)
//!   drop list [-w MS]          peers on the link (MILK_TABLE=1: a table)
//!   drop send PEER FILE        PEER: a name, a fingerprint prefix or an IPv4 address
//!   drop recv [-y] [-k]        accept files into ~/Downloads (-y: no prompt, -k: keep going)
//!   drop peers                 peers trusted on first use
//!   drop forget NAME           stop trusting NAME (after a legitimate key change)
//!
//! Trust: every machine has an Ed25519 identity (/etc/milk/drop, made on first use). Both
//! sides of a transfer sign a hello transcript (both nonces, both keys), the sender signs
//! the offer (name, size, SHA-256), and the receiver checks the data's SHA-256 before the
//! file appears. A peer's key is trusted on first use (fingerprint shown); a known name
//! with a different key is refused loudly. Every send / receive / refusal is an audit
//! record (`user.drop.*`).

#![no_std]
#![no_main]

extern crate alloc;

mod ident;
mod proto;

use alloc::string::String;
use alloc::vec::Vec;
use ident::{Identity, Trust, fingerprint, hex};
use proto::{Conn, MAGIC, Peer, XFER_PORT, field};
use rt::net::{self, SockAddr};
use rt::sha256::Sha256;
use rt::{ed25519, eprintln, print, println, sys};

rt::main!(main);

type Res<T> = Result<T, String>;

const PEERS_CACHE: &str = "/tmp/drop/peers";
const RECEIVING: &str = "/tmp/drop/receiving";
/// Sender side: largest file (it is read into memory).
const MAX_SEND: u64 = 64 << 20;
const CHUNK: usize = 16 * 1024;

fn audit(event: &str, detail: &str) {
    if let Ok(fd) = sys::open("/dev/audit", sys::O_WRONLY | sys::O_CLOEXEC) {
        let _ = sys::write_all(fd, alloc::format!("{event} {detail}\n").as_bytes());
        let _ = sys::close(fd);
    }
}

fn text(p: &str) -> Option<String> {
    sys::read_file(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// eth1's IPv4 address, if the link is up and configured.
fn eth1_addr() -> Option<[u8; 4]> {
    let t = text("/proc/net/ifaddr")?;
    let l = t.lines().find(|l| l.starts_with("eth1 "))?;
    let a = l.split(' ').find_map(|w| w.strip_prefix("inet="))?;
    SockAddr::parse(a, 0).map(|s| s.ip)
}

fn ip_str(ip: [u8; 4]) -> String {
    alloc::format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
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
    match eth1_addr() {
        Some(a) => println!(
            "link:        eth1 {} (transfers on port {XFER_PORT})",
            ip_str(a)
        ),
        None => println!("link:        no eth1 (Milk Drop needs the VM-to-VM link)"),
    }
    println!("keys:        {}", ident::DIR);
    Ok(())
}

fn cmd_name(new: &str) -> Res<()> {
    let mut id = identity()?;
    let old = id.name.clone();
    ident::set_name(&mut id, new)?;
    audit("drop.rename", &alloc::format!("old={old} new={new}"));
    println!("drop: this machine is now {new} (was {old})");
    Ok(())
}

/// Announce every 10 s and answer queries at once. Keeps /tmp/drop/peers (what it heard).
fn cmd_daemon(quiet: bool) -> Res<()> {
    let id = identity()?;
    let mut addr = None;
    for _ in 0..100 {
        addr = eth1_addr();
        if addr.is_some() {
            break;
        }
        sys::sleep_ms(100);
    }
    let addr = addr.ok_or("no eth1 address (no VM-to-VM link); not announcing")?;
    let fd = net::socket(net::SOCK_DGRAM).map_err(|e| alloc::format!("socket: {e}"))?;
    net::bind(
        fd,
        &SockAddr {
            ip: addr,
            port: proto::DISC_PORT,
        },
    )
    .map_err(|e| alloc::format!("bind {}:{}: {e}", ip_str(addr), proto::DISC_PORT))?;
    let _ = sys::mkdir("/tmp/drop", 0o755);
    if !quiet {
        println!(
            "drop: {} (fingerprint {}) on eth1 {}, announcing to 224.0.0.251:{}",
            id.name,
            fingerprint(&id.public),
            ip_str(addr),
            proto::DISC_PORT
        );
    }
    let host = hostname();
    let mut heard: Vec<(Peer, u64)> = Vec::new();
    let mut next = 0u64;
    let mut buf = [0u8; 1500];
    let mut me = id;
    // What we say about ourselves, read fresh for every announcement and answer: the name
    // may change (`drop name`), `drop recv` comes and goes. The daemon never makes a key.
    let current = |me: &mut Identity| -> String {
        if let Some(now) = ident::load() {
            *me = now;
        }
        proto::announce(me, &host, sys::stat_path(RECEIVING, false).is_ok())
    };
    let _ = net::set_recv_timeout(fd, 1000);
    loop {
        let now = sys::clock_ns(sys::CLOCK_MONOTONIC);
        if now >= next {
            let _ = net::sendto(fd, current(&mut me).as_bytes(), &proto::group());
            next = now + 10_000_000_000;
        }
        let Ok((n, from)) = net::recvfrom(fd, &mut buf) else {
            continue; // the 1 s timeout (or an error): go round
        };
        let line = String::from_utf8_lossy(&buf[..n]);
        let line = line.trim_end();
        if line == alloc::format!("{MAGIC} QUERY") {
            let _ = net::sendto(fd, current(&mut me).as_bytes(), &from);
        } else if let Some(peer) = proto::parse_announce(line, from)
            && peer.key != me.public
        {
            heard.retain(|(p, _)| p.key != peer.key);
            heard.push((peer, sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000));
            let mut s = String::new();
            for (p, t) in &heard {
                s.push_str(&alloc::format!(
                    "{} {} {} {} {}\n",
                    p.name,
                    ip_str(p.addr),
                    ident::fp_compact(&p.key),
                    p.receiving as u8,
                    t
                ));
            }
            let _ = ident::write_atomic(PEERS_CACHE, s.as_bytes(), 0o644);
        }
    }
}

fn trust_word(p: &Peer) -> String {
    match ident::trust(&p.name, &p.key) {
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

/// Refuse a changed key, loudly.
fn key_changed(role: &str, name: &str, old: &[u8; 32], new: &[u8; 32], addr: [u8; 4]) -> String {
    eprintln!("drop: ************************************************************");
    eprintln!("drop: WARNING: {name}'s KEY HAS CHANGED. Refusing to talk to it.");
    eprintln!("drop:   trusted fingerprint: {}", fingerprint(old));
    eprintln!("drop:   offered fingerprint: {}", fingerprint(new));
    eprintln!("drop: Someone may be pretending to be {name}, or it was reinstalled.");
    eprintln!("drop: If you know it was reinstalled: drop forget {name}");
    eprintln!("drop: ************************************************************");
    audit(
        "drop.key_changed",
        &alloc::format!(
            "role={role} name={name} addr={} trusted_fp={} offered_fp={} action=refused",
            ip_str(addr),
            ident::fp_compact(old),
            ident::fp_compact(new)
        ),
    );
    alloc::format!("{name}'s key changed; refused")
}

fn resolve(spec: &str) -> Res<(SockAddr, Option<Peer>)> {
    if let Some(a) = SockAddr::parse(spec, XFER_PORT) {
        return Ok((a, None));
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
        [p] => Ok((
            SockAddr {
                ip: p.addr,
                port: p.port,
            },
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

fn cmd_send(spec: &str, path: &str) -> Res<()> {
    let id = identity()?;
    let data = sys::read_file(path).map_err(|e| alloc::format!("{path}: {e}"))?;
    if data.len() as u64 > MAX_SEND {
        return Err(alloc::format!("{path}: larger than {} MiB", MAX_SEND >> 20));
    }
    let fname = path.rsplit('/').next().unwrap_or(path);
    let sha = hex(&rt::sha256::sha256(&data));
    let (addr, announced) = resolve(spec)?;
    let fd = net::socket(net::SOCK_STREAM).map_err(|e| alloc::format!("socket: {e}"))?;
    if let Err(e) = net::connect(fd, &addr) {
        let _ = sys::close(fd);
        return Err(if e.0 == 111 {
            alloc::format!("{spec} isn't receiving (run `drop recv` on it)")
        } else {
            alloc::format!("connect {addr}: {e}")
        });
    }
    let mut c = Conn::new(fd, 10_000);
    // 1. The receiver's hello.
    let h = c.line()?;
    let mut w = h.split(' ');
    if w.next() != Some(MAGIC) || w.next() != Some("HELLO") || w.next() != Some("recv") {
        return Err(alloc::format!("{addr} doesn't speak Milk Drop"));
    }
    let rname = field(&h, "name")
        .filter(|n| ident::name_ok(n))
        .ok_or("bad hello")?;
    let rkey = field(&h, "key")
        .and_then(ed25519::unhex::<32>)
        .ok_or("bad hello key")?;
    let nonce_r = field(&h, "nonce")
        .filter(|n| n.len() == 32)
        .ok_or("bad hello nonce")?;
    if let Some(p) = &announced
        && p.key != rkey
    {
        audit(
            "drop.send",
            &alloc::format!(
                "peer={rname} addr={} result=refused reason=key_not_announced",
                ip_str(addr.ip)
            ),
        );
        return Err(alloc::format!(
            "{addr} answered with a key that {} did not announce; refused",
            p.name
        ));
    }
    let trust = ident::trust(rname, &rkey);
    if let Trust::Changed { old } = &trust {
        return Err(key_changed("send", rname, old, &rkey, addr.ip));
    }
    // 2. Our hello, signed over the transcript.
    let nonce_s = hex(&ident::random::<16>());
    let sig = ed25519::sign(
        &id.seed,
        proto::transcript("send", nonce_r, &nonce_s, &rkey, &id.public).as_bytes(),
    );
    c.send_line(&alloc::format!(
        "{MAGIC} HELLO send name={} key={} nonce={nonce_s} sig={}",
        id.name,
        hex(&id.public),
        hex(&sig)
    ))?;
    // 3. The receiver proves its key.
    let a = c.line()?;
    if let Some(reason) = a.strip_prefix("DECLINE ") {
        return Err(alloc::format!("{rname} refused: {reason}"));
    }
    let rsig = a
        .strip_prefix("AUTH sig=")
        .and_then(ed25519::unhex::<64>)
        .ok_or("bad AUTH")?;
    if !ed25519::verify(
        &rkey,
        proto::transcript("recv", nonce_r, &nonce_s, &rkey, &id.public).as_bytes(),
        &rsig,
    ) {
        audit(
            "drop.send",
            &alloc::format!(
                "peer={rname} addr={} result=refused reason=bad_auth",
                ip_str(addr.ip)
            ),
        );
        return Err(alloc::format!("{rname} failed to prove its key; refused"));
    }
    match &trust {
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
        _ => println!("drop: {rname} ({}) verified", fingerprint(&rkey)),
    }
    // 4. The signed offer.
    let size = data.len() as u64;
    let osig = ed25519::sign(
        &id.seed,
        proto::offer_msg(nonce_r, &nonce_s, fname, size, &sha).as_bytes(),
    );
    c.send_line(&alloc::format!(
        "OFFER name={} size={size} sha256={sha} sig={}",
        proto::esc(fname),
        hex(&osig)
    ))?;
    println!(
        "drop: offering {fname} ({}, sha256 {}...) to {rname}; waiting for it to accept",
        kib(size),
        &sha[..16]
    );
    c.set_timeout(120_000);
    let r = c.line()?;
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
    // 5. The data, then the receiver's verdict.
    c.set_timeout(20_000);
    let t0 = sys::clock_ns(sys::CLOCK_MONOTONIC);
    let mut prog = Progress::new(size);
    let mut off = 0usize;
    prog.at(0);
    while off < data.len() {
        let n = CHUNK.min(data.len() - off);
        c.send(&data[off..off + n])?;
        off += n;
        prog.at(off as u64);
    }
    let v = c.line()?;
    let ms = ((sys::clock_ns(sys::CLOCK_MONOTONIC) - t0) / 1_000_000).max(1);
    let ok = v == alloc::format!("DONE sha256={sha}");
    audit(
        "drop.send",
        &alloc::format!(
            "peer={rname} fp={} addr={} file={} size={size} sha256={sha} result={}",
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
        .unwrap_or("/home/milk");
    alloc::format!("{home}/Downloads")
}

/// Handle one incoming connection. Ok(true) = a file was received.
fn serve(id: &Identity, c: &mut Conn, from: SockAddr, yes: bool, dir: &str) -> Res<bool> {
    let nonce_r = hex(&ident::random::<16>());
    c.send_line(&alloc::format!(
        "{MAGIC} HELLO recv name={} key={} nonce={nonce_r}",
        id.name,
        hex(&id.public)
    ))?;
    let h = c.line()?;
    let mut w = h.split(' ');
    if w.next() != Some(MAGIC) || w.next() != Some("HELLO") || w.next() != Some("send") {
        return Err(alloc::format!("{from}: not a Milk Drop sender"));
    }
    let sname = field(&h, "name")
        .filter(|n| ident::name_ok(n))
        .ok_or("bad hello")?;
    let skey = field(&h, "key")
        .and_then(ed25519::unhex::<32>)
        .ok_or("bad hello key")?;
    let nonce_s = field(&h, "nonce")
        .filter(|n| n.len() == 32)
        .ok_or("bad nonce")?;
    let ssig = field(&h, "sig")
        .and_then(ed25519::unhex::<64>)
        .ok_or("bad hello sig")?;
    if !ed25519::verify(
        &skey,
        proto::transcript("send", &nonce_r, nonce_s, &id.public, &skey).as_bytes(),
        &ssig,
    ) {
        audit(
            "drop.recv",
            &alloc::format!(
                "peer={sname} addr={} result=refused reason=bad_hello",
                ip_str(from.ip)
            ),
        );
        let _ = c.send_line("DECLINE bad-signature");
        return Err(alloc::format!(
            "{sname} at {from}: hello signature doesn't verify; refused"
        ));
    }
    let trust = ident::trust(sname, &skey);
    if let Trust::Changed { old } = &trust {
        let _ = c.send_line("DECLINE key-changed");
        return Err(key_changed("recv", sname, old, &skey, from.ip));
    }
    let asig = ed25519::sign(
        &id.seed,
        proto::transcript("recv", &nonce_r, nonce_s, &id.public, &skey).as_bytes(),
    );
    c.send_line(&alloc::format!("AUTH sig={}", hex(&asig)))?;
    let o = c.line()?;
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
        let _ = c.send_line("DECLINE bad-offer");
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
        _ => alloc::format!("{sname} (fingerprint {fp})"),
    };
    println!("drop: {who} wants to send you {name} ({}).", kib(size));
    let ok = if yes {
        println!("drop: accepting (-y)");
        true
    } else {
        ask("drop: accept? [y/N] ")
    };
    if !ok {
        let _ = c.send_line("DECLINE declined-by-user");
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
    if !matches!(trust, Trust::Known { ref as_name } if as_name == sname) {
        ident::remember(sname, &skey)?;
        if matches!(trust, Trust::New) {
            audit(
                "drop.trust",
                &alloc::format!("name={sname} fp={} role=recv", ident::fp_compact(&skey)),
            );
        }
    }
    let _ = sys::mkdir(dir, 0o755);
    let part = alloc::format!("{dir}/.{name}.part");
    let fd = sys::open_mode(
        &part,
        sys::O_WRONLY | sys::O_CREAT | sys::O_TRUNC | sys::O_CLOEXEC,
        0o644,
    )
    .map_err(|e| alloc::format!("{part}: {e}"))?;
    c.send_line("ACCEPT")?;
    c.set_timeout(20_000);
    let mut h = Sha256::new();
    let mut got = 0u64;
    let mut prog = Progress::new(size);
    prog.at(0);
    let r: Res<()> = (|| {
        while got < size {
            let d = c.data((size - got).min(CHUNK as u64) as usize)?;
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
        let _ = c.send_line(&alloc::format!("BAD sha256={have}"));
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
    let _ = c.send_line(&alloc::format!("DONE sha256={have}"));
    audit(
        "drop.recv",
        &alloc::format!(
            "peer={sname} fp={} addr={} file={} size={size} sha256={have} saved={} result=ok",
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
    let addr = eth1_addr().ok_or("no eth1 address (Milk Drop needs the VM-to-VM link)")?;
    let l = net::socket(net::SOCK_STREAM).map_err(|e| alloc::format!("socket: {e}"))?;
    let _ = net::set_reuseaddr(l);
    let me = SockAddr {
        ip: addr,
        port: XFER_PORT,
    };
    net::bind(l, &me).map_err(|e| alloc::format!("bind {me}: {e} (another drop recv?)"))?;
    net::listen(l, 2).map_err(|e| alloc::format!("listen: {e}"))?;
    let _ = sys::mkdir("/tmp/drop", 0o755);
    let _ = ident::write_atomic(
        RECEIVING,
        alloc::format!("{}\n", sys::getpid()).as_bytes(),
        0o644,
    );
    println!(
        "drop: receiving as {} (fingerprint {}) on {me}; files go to {}",
        id.name,
        fingerprint(&id.public),
        dir
    );
    let r = loop {
        let (fd, from) = match net::accept(l) {
            Ok(x) => x,
            Err(e) if e.0 == 4 => continue,
            Err(e) => break Err(alloc::format!("accept: {e}")),
        };
        let mut c = Conn::new(fd, 10_000);
        match serve(&id, &mut c, from, yes, dir) {
            Ok(true) if !keep => break Ok(()),
            Ok(_) => {}
            Err(e) => eprintln!("drop: {e}"),
        }
    };
    let _ = sys::unlink(RECEIVING);
    let _ = sys::close(l);
    r
}

fn usage() -> i32 {
    eprintln!(
        "usage: drop id | name NEW | list [-w MS] | send PEER FILE | recv [-y] [-k]\n       drop peers | forget NAME | daemon [-q]"
    );
    2
}

fn main(args: &rt::Args) -> i32 {
    let a: Vec<&str> = args.argv.iter().skip(1).copied().collect();
    let table = rt::table::requested(args);
    let r = match a.as_slice() {
        ["id"] => cmd_id(),
        ["name", n] => cmd_name(n),
        ["daemon"] => cmd_daemon(false),
        ["daemon", "-q"] => cmd_daemon(true),
        ["list"] => cmd_list(1500, table),
        ["list", "-w", ms] => match ms.parse() {
            Ok(ms) => cmd_list(ms, table),
            Err(_) => return usage(),
        },
        ["peers"] => cmd_peers(),
        ["forget", n] => cmd_forget(n),
        ["send", peer, file] => cmd_send(peer, file),
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
