//! The authenticated key agreement Milk Drop and mssh run at the start of every TCP
//! connection, and the encrypted channel it yields.
//!
//! ```text
//! S (server): MAGIC HELLO <srole> name=N key=Ks nonce=Ns dh=Es [rot=old:sig]
//! C (client): MAGIC HELLO <crole> name=N key=Kc nonce=Nc dh=Ec sig=Sign_c(T(crole)) [rot=...]
//! S:          AUTH sig=Sign_s(T(srole))                (or DECLINE key-changed / bad-signature)
//!   T(role) = "<ctx> role Ns Nc Ks Kc Es Ec"
//! ```
//!
//! then [`Channel`] records, one ChaCha20-Poly1305 key per direction derived from the
//! X25519 secret and both nonces and keys. Each side's Ed25519 identity signs a transcript
//! that covers both X25519 keys, so the identities authenticate the key agreement. Trust
//! is the shared TOFU store (`ident::trust_rotated`): a changed key is refused loudly on
//! either side (unless the old key signed the new one).
//!
//! Milk Drop is [`DROP`] (byte for byte the protocol `drop` always spoke), mssh is
//! [`MSSH`] (its own magic and contexts, so a signature of one is never valid for the
//! other).

use alloc::string::String;
use rt::crypto::x25519;
use rt::{ed25519, eprintln, println};

use crate::ident::{self, Identity, Trust, fingerprint, hex};
use crate::proto::{self, Channel, Conn, field};
use crate::{audit, ip_str};

pub struct Protocol {
    /// First word of the hello lines.
    pub magic: &'static str,
    /// Transcript context and key-derivation label.
    pub hello_ctx: &'static str,
    pub key_ctx: &'static str,
    /// Program name for messages and audit events (`<app>.key_changed`, ...).
    pub app: &'static str,
    /// The roles' names in hellos, transcripts and audit records.
    pub client_role: &'static str,
    pub server_role: &'static str,
}

pub const DROP: Protocol = Protocol {
    magic: proto::XMAGIC,
    hello_ctx: "milkdrop-hello-v2",
    key_ctx: "milkdrop-v2",
    app: "drop",
    client_role: "send",
    server_role: "recv",
};

pub const MSSH: Protocol = Protocol {
    magic: "MSSH/1",
    hello_ctx: "mssh-hello-v1",
    key_ctx: "mssh-v1",
    app: "mssh",
    client_role: "client",
    server_role: "server",
};

/// An authenticated, encrypted connection.
pub struct Session {
    /// The peer's name and identity key, and what the trust store says about it.
    pub name: String,
    pub key: [u8; 32],
    pub trust: Trust,
    pub ch: Channel,
    /// Server's and client's nonces (bound into anything signed later, e.g. drop's offer).
    pub nonce_s: String,
    pub nonce_c: String,
}

/// Our side of the key agreement: an ephemeral X25519 key pair (fresh per connection).
struct Ephemeral {
    secret: [u8; 32],
    public: [u8; 32],
}

impl Ephemeral {
    fn new() -> Ephemeral {
        let secret = ident::random::<32>();
        Ephemeral {
            public: x25519::public_key(&secret),
            secret,
        }
    }
    fn shared(mut self, peer: &[u8; 32]) -> Result<[u8; 32], String> {
        let s = x25519::shared(&self.secret, peer);
        rt::crypto::wipe(&mut self.secret);
        s.ok_or_else(|| String::from("bad X25519 key from the peer"))
    }
}

/// ` rot=<old key>:<signature>` if our key replaced an older one.
fn rot_field(id: &Identity) -> String {
    ident::rotation(&id.public)
        .map(|r| alloc::format!(" rot={}:{}", hex(&r.old), hex(&r.sig)))
        .unwrap_or_default()
}

fn parse_rot(line: &str) -> Option<ident::Rotation> {
    let (o, s) = field(line, "rot")?.split_once(':')?;
    Some(ident::Rotation {
        old: ed25519::unhex::<32>(o)?,
        sig: ed25519::unhex::<64>(s)?,
    })
}

/// Refuse a changed key, loudly (and audit it). Returns the error text.
pub fn key_changed(
    p: &Protocol,
    role: &str,
    name: &str,
    old: &[u8; 32],
    new: &[u8; 32],
    addr: rt::net::Ip,
) -> String {
    let a = p.app;
    eprintln!("{a}: ************************************************************");
    eprintln!("{a}: WARNING: {name}'s KEY HAS CHANGED. Refusing to talk to it.");
    eprintln!("{a}:   trusted fingerprint: {}", fingerprint(old));
    eprintln!("{a}:   offered fingerprint: {}", fingerprint(new));
    eprintln!("{a}: Someone may be pretending to be {name}, or it was reinstalled.");
    eprintln!("{a}: If you know it was reinstalled: drop forget {name}");
    eprintln!("{a}: ************************************************************");
    audit(
        &alloc::format!("{a}.key_changed"),
        &alloc::format!(
            "role={role} name={name} addr={} trusted_fp={} offered_fp={} action=refused",
            ip_str(addr),
            ident::fp_compact(old),
            ident::fp_compact(new)
        ),
    );
    alloc::format!("{name}'s key changed; refused")
}

/// Say what we decided about a rotated key and record it. Err for a refused change.
pub fn settle_trust(
    p: &Protocol,
    role: &str,
    name: &str,
    key: &[u8; 32],
    trust: &Trust,
    addr: rt::net::Ip,
) -> Result<(), String> {
    match trust {
        Trust::Changed { old } => Err(key_changed(p, role, name, old, key, addr)),
        Trust::Rotated { old } => {
            println!(
                "{}: {name} rotated its key (signed by its old key {}): now {}",
                p.app,
                fingerprint(old),
                fingerprint(key)
            );
            audit(
                &alloc::format!("{}.key_rotated", p.app),
                &alloc::format!(
                    "role={role} name={name} old_fp={} new_fp={} action=accepted",
                    ident::fp_compact(old),
                    ident::fp_compact(key)
                ),
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

fn parse_hello<'a>(
    p: &Protocol,
    h: &'a str,
    role: &str,
) -> Option<(&'a str, [u8; 32], &'a str, [u8; 32])> {
    let mut w = h.split(' ');
    if w.next() != Some(p.magic) || w.next() != Some("HELLO") || w.next() != Some(role) {
        return None;
    }
    Some((
        field(h, "name").filter(|n| ident::name_ok(n))?,
        field(h, "key").and_then(ed25519::unhex::<32>)?,
        field(h, "nonce").filter(|n| n.len() == 32)?,
        field(h, "dh").and_then(ed25519::unhex::<32>)?,
    ))
}

/// The connecting side. `announced`: the key discovery said this peer has (checked).
/// A changed key is refused here; a rotated one is accepted (and reported).
pub fn connect(
    c: &mut Conn,
    id: &Identity,
    p: &Protocol,
    announced: Option<&[u8; 32]>,
    addr: rt::net::Ip,
) -> Result<Session, String> {
    let h = c.line()?;
    let Some((name, key, nonce_s, sdh)) = parse_hello(p, &h, p.server_role) else {
        return Err(alloc::format!(
            "{} doesn't speak {} (is it an older milkOS?)",
            ip_str(addr),
            p.magic
        ));
    };
    if let Some(k) = announced
        && *k != key
    {
        audit(
            &alloc::format!("{}.refused", p.app),
            &alloc::format!("peer={name} addr={} reason=key_not_announced", ip_str(addr)),
        );
        return Err(alloc::format!(
            "{} answered with a key it did not announce; refused",
            ip_str(addr)
        ));
    }
    let trust = ident::trust_rotated(name, &key, parse_rot(&h).as_ref());
    if let Trust::Changed { old } = &trust {
        return Err(key_changed(p, p.client_role, name, old, &key, addr));
    }
    let nonce_c = hex(&ident::random::<16>());
    let eph = Ephemeral::new();
    let cdh = eph.public;
    let t = |role: &str| {
        proto::transcript(
            p.hello_ctx,
            role,
            nonce_s,
            &nonce_c,
            &key,
            &id.public,
            &sdh,
            &cdh,
        )
    };
    let sig = id.sign(t(p.client_role).as_bytes())?;
    c.send_line(&alloc::format!(
        "{} HELLO {} name={} key={} nonce={nonce_c} dh={} sig={}{}",
        p.magic,
        p.client_role,
        id.name,
        hex(&id.public),
        hex(&cdh),
        hex(&sig),
        rot_field(id)
    ))?;
    let a = c.line()?;
    if let Some(reason) = a.strip_prefix("DECLINE ") {
        return Err(alloc::format!("{name} refused: {reason}"));
    }
    let ssig = a
        .strip_prefix("AUTH sig=")
        .and_then(ed25519::unhex::<64>)
        .ok_or("bad AUTH")?;
    if !ed25519::verify(&key, t(p.server_role).as_bytes(), &ssig) {
        audit(
            &alloc::format!("{}.refused", p.app),
            &alloc::format!("peer={name} addr={} reason=bad_auth", ip_str(addr)),
        );
        return Err(alloc::format!("{name} failed to prove its key; refused"));
    }
    let mut shared = eph.shared(&sdh)?;
    let ch = Channel::derive(p.key_ctx, &shared, nonce_s, &nonce_c, &sdh, &cdh, true);
    rt::crypto::wipe(&mut shared);
    let s = Session {
        name: String::from(name),
        key,
        trust,
        ch,
        nonce_s: String::from(nonce_s),
        nonce_c,
    };
    settle_trust(p, p.client_role, &s.name, &s.key, &s.trust, addr)?;
    Ok(s)
}

/// The accepting side. A changed key is refused (DECLINE key-changed); otherwise the
/// caller decides what to do with `trust` (remember a new key, report a rotation with
/// [`settle_trust`]) once it has decided to go on.
pub fn accept(
    c: &mut Conn,
    id: &Identity,
    p: &Protocol,
    from: rt::net::Ip,
) -> Result<Session, String> {
    let nonce_s = hex(&ident::random::<16>());
    let eph = Ephemeral::new();
    let sdh = eph.public;
    c.send_line(&alloc::format!(
        "{} HELLO {} name={} key={} nonce={nonce_s} dh={}{}",
        p.magic,
        p.server_role,
        id.name,
        hex(&id.public),
        hex(&sdh),
        rot_field(id)
    ))?;
    let h = c.line()?;
    let Some((name, key, nonce_c, cdh)) = parse_hello(p, &h, p.client_role) else {
        return Err(alloc::format!("{}: not a {} client", ip_str(from), p.magic));
    };
    let csig = field(&h, "sig")
        .and_then(ed25519::unhex::<64>)
        .ok_or("bad hello sig")?;
    let t = |role: &str| {
        proto::transcript(
            p.hello_ctx,
            role,
            &nonce_s,
            nonce_c,
            &id.public,
            &key,
            &sdh,
            &cdh,
        )
    };
    if !ed25519::verify(&key, t(p.client_role).as_bytes(), &csig) {
        audit(
            &alloc::format!("{}.refused", p.app),
            &alloc::format!("peer={name} addr={} reason=bad_hello", ip_str(from)),
        );
        let _ = c.send_line("DECLINE bad-signature");
        return Err(alloc::format!(
            "{name} at {}: hello signature doesn't verify; refused",
            ip_str(from)
        ));
    }
    let trust = ident::trust_rotated(name, &key, parse_rot(&h).as_ref());
    if let Trust::Changed { old } = &trust {
        let _ = c.send_line("DECLINE key-changed");
        return Err(key_changed(p, p.server_role, name, old, &key, from));
    }
    let asig = id.sign(t(p.server_role).as_bytes())?;
    c.send_line(&alloc::format!("AUTH sig={}", hex(&asig)))?;
    let mut shared = eph.shared(&cdh)?;
    let ch = Channel::derive(p.key_ctx, &shared, &nonce_s, nonce_c, &sdh, &cdh, false);
    rt::crypto::wipe(&mut shared);
    Ok(Session {
        name: String::from(name),
        key,
        trust,
        ch,
        nonce_s,
        nonce_c: String::from(nonce_c),
    })
}
