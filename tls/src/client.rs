//! The client handshake (RFC 8446 4) and the post-handshake messages.
//!
//! ```text
//! ClientHello  ->
//!              <- HelloRetryRequest       (at most once: another group, maybe a cookie)
//! ClientHello  ->
//!              <- ServerHello             (keys: handshake traffic secrets)
//!              <- {EncryptedExtensions} {CertificateRequest} {Certificate}
//!                 {CertificateVerify} {Finished}
//! {Certificate} {Finished} ->             (an empty Certificate only if asked)
//!              <- [NewSessionTicket] [KeyUpdate] [application data]
//! ```
//!
//! Offered: TLS 1.3 only (supported_versions), the configured cipher suites and groups
//! (a key share for the first group only, so a server that wants another one sends a
//! HelloRetryRequest), signature_algorithms, SNI for DNS names, ALPN, and
//! psk_key_exchange_modes (psk_dhe_ke: servers then send tickets, which we parse and drop:
//! no resumption, no 0-RTT). Middlebox compatibility mode (a random legacy_session_id and
//! one dummy change_cipher_spec) as browsers do.

use alloc::vec;
use alloc::vec::Vec;

use crate::Transport;
use crate::codec::{Reader, put_u8, put_u16, put_u24, with_len};
use crate::error::{Alert, CertError, TlsError};
use crate::keys::{self, CipherSuite, HASH_LEN, RecordKeys, Schedule, Secret, Transcript};
use crate::name::ServerName;
use crate::record::{self, Incoming, RecordLayer};
use crate::verify;
#[cfg(test)]
use crate::x509::Certificate;
use crate::x509::{Hash, PublicKey, SigAlg};
use crate::{Config, NamedGroup};
use milkcrypto::{ct_eq, p256, p384, wipe, x25519};

// Handshake message types.
const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_NEW_SESSION_TICKET: u8 = 4;
const HS_ENCRYPTED_EXTENSIONS: u8 = 8;
const HS_CERTIFICATE: u8 = 11;
const HS_CERTIFICATE_REQUEST: u8 = 13;
const HS_CERTIFICATE_VERIFY: u8 = 15;
const HS_FINISHED: u8 = 20;
const HS_KEY_UPDATE: u8 = 24;

// Extension types.
const EXT_SERVER_NAME: u16 = 0;
const EXT_MAX_FRAGMENT_LENGTH: u16 = 1;
const EXT_SUPPORTED_GROUPS: u16 = 10;
const EXT_SIGNATURE_ALGORITHMS: u16 = 13;
const EXT_ALPN: u16 = 16;
const EXT_PADDING: u16 = 21;
const EXT_RECORD_SIZE_LIMIT: u16 = 28;
const EXT_PRE_SHARED_KEY: u16 = 41;
const EXT_EARLY_DATA: u16 = 42;
const EXT_SUPPORTED_VERSIONS: u16 = 43;
const EXT_COOKIE: u16 = 44;
const EXT_PSK_KEY_EXCHANGE_MODES: u16 = 45;
const EXT_KEY_SHARE: u16 = 51;

const TLS13: u16 = 0x0304;

/// SHA-256("HelloRetryRequest"): the ServerHello.random of a HelloRetryRequest.
const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91,
    0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

/// The signature schemes we offer (RFC 8446 4.2.3), most preferred first. The rsa_pkcs1
/// ones are for certificates only; a CertificateVerify with them is refused.
pub const SIGNATURE_SCHEMES: &[u16] = &[
    0x0403, // ecdsa_secp256r1_sha256
    0x0503, // ecdsa_secp384r1_sha384
    0x0807, // ed25519
    0x0804, // rsa_pss_rsae_sha256
    0x0805, // rsa_pss_rsae_sha384
    0x0806, // rsa_pss_rsae_sha512
    0x0809, // rsa_pss_pss_sha256
    0x080a, // rsa_pss_pss_sha384
    0x080b, // rsa_pss_pss_sha512
    0x0401, // rsa_pkcs1_sha256
    0x0501, // rsa_pkcs1_sha384
    0x0601, // rsa_pkcs1_sha512
];

/// An ephemeral key share; the secret is wiped on drop.
struct KeyShare {
    group: NamedGroup,
    secret: Vec<u8>,
    public: Vec<u8>,
}

impl Drop for KeyShare {
    fn drop(&mut self) {
        wipe(&mut self.secret);
    }
}

impl KeyShare {
    fn generate(group: NamedGroup, cfg: &Config) -> Result<KeyShare, TlsError> {
        match group {
            NamedGroup::X25519 => {
                let mut s = [0u8; 32];
                cfg.rng.fill(&mut s);
                let public = x25519::public_key(&s).to_vec();
                let k = KeyShare {
                    group,
                    secret: s.to_vec(),
                    public,
                };
                wipe(&mut s);
                Ok(k)
            }
            NamedGroup::Secp256r1 => {
                for _ in 0..32 {
                    let mut s = [0u8; 32];
                    cfg.rng.fill(&mut s);
                    if let Some(p) = p256::public_key(&s) {
                        let k = KeyShare {
                            group,
                            secret: s.to_vec(),
                            public: p.to_vec(),
                        };
                        wipe(&mut s);
                        return Ok(k);
                    }
                }
                Err(TlsError::proto(Alert::INTERNAL_ERROR, "the random number generator failed"))
            }
            NamedGroup::Secp384r1 => {
                for _ in 0..32 {
                    let mut s = [0u8; 48];
                    cfg.rng.fill(&mut s);
                    if let Some(p) = p384::public_key(&s) {
                        let k = KeyShare {
                            group,
                            secret: s.to_vec(),
                            public: p.to_vec(),
                        };
                        wipe(&mut s);
                        return Ok(k);
                    }
                }
                Err(TlsError::proto(Alert::INTERNAL_ERROR, "the random number generator failed"))
            }
        }
    }

    /// The (EC)DHE shared secret with the server's share, wiped by the caller.
    fn agree(&self, peer: &[u8]) -> Result<Vec<u8>, TlsError> {
        let bad = || TlsError::illegal("an invalid key share");
        match self.group {
            NamedGroup::X25519 => {
                let s: [u8; 32] = self.secret[..].try_into().map_err(|_| bad())?;
                let p: [u8; 32] = peer.try_into().map_err(|_| bad())?;
                let mut z = x25519::shared(&s, &p).ok_or_else(bad)?;
                let v = z.to_vec();
                wipe(&mut z);
                Ok(v)
            }
            NamedGroup::Secp256r1 => {
                let s: [u8; 32] = self.secret[..].try_into().map_err(|_| bad())?;
                let mut z = p256::shared(&s, peer).ok_or_else(bad)?;
                let v = z.to_vec();
                wipe(&mut z);
                Ok(v)
            }
            NamedGroup::Secp384r1 => {
                let s: [u8; 48] = self.secret[..].try_into().map_err(|_| bad())?;
                let mut z = p384::shared(&s, peer).ok_or_else(bad)?;
                let v = z.to_vec();
                wipe(&mut z);
                Ok(v)
            }
        }
    }
}

/// What the handshake settled, for [`crate::Stream`].
pub struct Session {
    pub suite: CipherSuite,
    pub group: NamedGroup,
    pub alpn: Option<Vec<u8>>,
    /// The server's certificates as sent (leaf first).
    pub certificates: Vec<Vec<u8>>,
    pub hello_retry: bool,
    pub client_secret: Secret,
    pub server_secret: Secret,
}

/// Wrap a handshake body in its 4-byte header.
fn handshake_message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + body.len());
    put_u8(&mut m, kind);
    put_u24(&mut m, body.len() as u32);
    m.extend_from_slice(body);
    m
}

struct Hello {
    /// The whole ClientHello message (as hashed).
    msg: Vec<u8>,
    session_id: Vec<u8>,
    /// The extension types we sent (a server may only answer these).
    sent: Vec<u16>,
}

fn ext(out: &mut Vec<u8>, sent: &mut Vec<u16>, kind: u16, f: impl FnOnce(&mut Vec<u8>)) {
    put_u16(out, kind);
    with_len(out, 2, f);
    sent.push(kind);
}

fn build_client_hello(
    cfg: &Config,
    name: &ServerName,
    random: &[u8; 32],
    session_id: &[u8],
    shares: &[KeyShare],
    cookie: Option<&[u8]>,
) -> Hello {
    let mut sent = Vec::new();
    let mut exts = Vec::new();
    if let Some(host) = name.sni() {
        ext(&mut exts, &mut sent, EXT_SERVER_NAME, |o| {
            with_len(o, 2, |o| {
                put_u8(o, 0); // host_name
                with_len(o, 2, |o| o.extend_from_slice(host.as_bytes()));
            })
        });
    }
    ext(&mut exts, &mut sent, EXT_SUPPORTED_GROUPS, |o| {
        with_len(o, 2, |o| {
            for g in &cfg.groups {
                put_u16(o, g.id());
            }
        })
    });
    ext(&mut exts, &mut sent, EXT_SIGNATURE_ALGORITHMS, |o| {
        with_len(o, 2, |o| {
            for &s in SIGNATURE_SCHEMES {
                put_u16(o, s);
            }
        })
    });
    if !cfg.alpn.is_empty() {
        ext(&mut exts, &mut sent, EXT_ALPN, |o| {
            with_len(o, 2, |o| {
                for p in &cfg.alpn {
                    with_len(o, 1, |o| o.extend_from_slice(p));
                }
            })
        });
    }
    ext(&mut exts, &mut sent, EXT_SUPPORTED_VERSIONS, |o| {
        with_len(o, 1, |o| put_u16(o, TLS13));
    });
    ext(&mut exts, &mut sent, EXT_PSK_KEY_EXCHANGE_MODES, |o| {
        with_len(o, 1, |o| put_u8(o, 1)); // psk_dhe_ke
    });
    if let Some(c) = cookie {
        ext(&mut exts, &mut sent, EXT_COOKIE, |o| {
            with_len(o, 2, |o| o.extend_from_slice(c));
        });
    }
    ext(&mut exts, &mut sent, EXT_KEY_SHARE, |o| {
        with_len(o, 2, |o| {
            for k in shares {
                put_u16(o, k.group.id());
                with_len(o, 2, |o| o.extend_from_slice(&k.public));
            }
        })
    });

    let body = |exts: &[u8]| {
        let mut b = Vec::with_capacity(64 + exts.len());
        put_u16(&mut b, 0x0303);
        b.extend_from_slice(random);
        with_len(&mut b, 1, |o| o.extend_from_slice(session_id));
        with_len(&mut b, 2, |o| {
            for s in &cfg.cipher_suites {
                put_u16(o, s.id());
            }
        });
        with_len(&mut b, 1, |o| put_u8(o, 0)); // null compression
        with_len(&mut b, 2, |o| o.extend_from_slice(exts));
        b
    };
    // RFC 7685: keep the ClientHello out of 256..=511 bytes, which some middleboxes choke on.
    let mut b = body(&exts);
    let len = 4 + b.len();
    if (256..512).contains(&len) {
        let pad = (512 - len).saturating_sub(4);
        ext(&mut exts, &mut sent, EXT_PADDING, |o| o.resize(o.len() + pad, 0));
        b = body(&exts);
    }
    Hello {
        msg: handshake_message(HS_CLIENT_HELLO, &b),
        session_id: session_id.to_vec(),
        sent,
    }
}

/// The extension types in a ClientHello we were handed (the RFC 8448 replay test).
#[cfg(test)]
fn hello_from_bytes(msg: &[u8]) -> Hello {
    let mut r = Reader::new(&msg[4..]);
    r.take(2 + 32);
    let session_id = r.vec8().unwrap_or(&[]).to_vec();
    r.vec16();
    r.vec8();
    let mut e = Reader::new(r.vec16().unwrap_or(&[]));
    let mut sent = Vec::new();
    while let (Some(t), Some(_)) = (e.u16(), e.vec16()) {
        sent.push(t);
    }
    Hello {
        msg: msg.to_vec(),
        session_id,
        sent,
    }
}

/// Extensions of a message as (type, data), refusing duplicates.
fn extensions(data: &[u8]) -> Result<Vec<(u16, &[u8])>, TlsError> {
    let mut r = Reader::new(data);
    let mut out: Vec<(u16, &[u8])> = Vec::new();
    while !r.is_empty() {
        let t = r.u16().ok_or(TlsError::decode("a malformed extension"))?;
        let v = r.vec16().ok_or(TlsError::decode("a malformed extension"))?;
        if out.iter().any(|(x, _)| *x == t) {
            return Err(TlsError::illegal("a repeated extension"));
        }
        out.push((t, v));
    }
    Ok(out)
}

struct ServerHello<'a> {
    retry: bool,
    random: &'a [u8],
    session_id: &'a [u8],
    suite: u16,
    exts: Vec<(u16, &'a [u8])>,
}

fn parse_server_hello(body: &[u8]) -> Result<ServerHello<'_>, TlsError> {
    let d = || TlsError::decode("a malformed ServerHello");
    let mut r = Reader::new(body);
    let version = r.u16().ok_or_else(d)?;
    let random = r.take(32).ok_or_else(d)?;
    let session_id = r.vec8().ok_or_else(d)?;
    let suite = r.u16().ok_or_else(d)?;
    let compression = r.u8().ok_or_else(d)?;
    // A TLS 1.2 ServerHello may end here (no extensions at all).
    let exts = if r.is_empty() {
        Vec::new()
    } else {
        let e = r.vec16().ok_or_else(d)?;
        if !r.is_empty() {
            return Err(d());
        }
        extensions(e)?
    };
    let sv = exts.iter().find(|(t, _)| *t == EXT_SUPPORTED_VERSIONS);
    let Some((_, sv)) = sv else {
        return Err(TlsError::NotTls13); // TLS 1.2 or older
    };
    if sv.len() != 2 || u16::from_be_bytes([sv[0], sv[1]]) != TLS13 {
        return Err(TlsError::illegal("a version we didn't offer"));
    }
    if version != 0x0303 || compression != 0 || session_id.len() > 32 {
        return Err(TlsError::illegal("a malformed ServerHello"));
    }
    Ok(ServerHello {
        retry: random == HRR_RANDOM,
        random,
        session_id,
        suite,
        exts,
    })
}

/// The scheme-to-algorithm rules for a CertificateVerify (RFC 8446 4.4.3, 4.2.3): curve and
/// hash fixed together for ECDSA; RSA only with PSS (rsae for rsaEncryption keys, pss for
/// RSASSA-PSS keys), salt length = hash length.
fn cv_algorithm(scheme: u16, key: &PublicKey) -> Option<SigAlg> {
    let pss = |h: Hash| SigAlg::RsaPss {
        hash: h,
        salt: h.len(),
    };
    Some(match (scheme, key) {
        (0x0403, PublicKey::P256(_)) => SigAlg::Ecdsa(Hash::Sha256),
        (0x0503, PublicKey::P384(_)) => SigAlg::Ecdsa(Hash::Sha384),
        (0x0807, PublicKey::Ed25519(_)) => SigAlg::Ed25519,
        (0x0804, PublicKey::Rsa { pss_only: false, .. }) => pss(Hash::Sha256),
        (0x0805, PublicKey::Rsa { pss_only: false, .. }) => pss(Hash::Sha384),
        (0x0806, PublicKey::Rsa { pss_only: false, .. }) => pss(Hash::Sha512),
        (0x0809, PublicKey::Rsa { pss_only: true, .. }) => pss(Hash::Sha256),
        (0x080a, PublicKey::Rsa { pss_only: true, .. }) => pss(Hash::Sha384),
        (0x080b, PublicKey::Rsa { pss_only: true, .. }) => pss(Hash::Sha512),
        _ => return None,
    })
}

/// Read the next handshake message, reading records as needed. During the handshake only
/// handshake records are expected; an alert ends it.
fn next_message<T: Transport>(rl: &mut RecordLayer<T>, seen_server_hello: bool) -> Result<Vec<u8>, TlsError> {
    loop {
        if let Some(m) = rl.take_handshake()? {
            return Ok(m);
        }
        match rl.read()? {
            Incoming::Handshake => {}
            Incoming::ApplicationData(_) => {
                return Err(TlsError::unexpected("application data during the handshake"));
            }
            Incoming::Alert(a) => {
                return Err(match a {
                    Alert::CLOSE_NOTIFY => TlsError::ConnectionClosed,
                    Alert::PROTOCOL_VERSION => TlsError::NotTls13,
                    Alert::HANDSHAKE_FAILURE | Alert::INSUFFICIENT_SECURITY if !seen_server_hello => {
                        TlsError::NoCommonAlgorithms
                    }
                    a => TlsError::PeerAlert(a),
                });
            }
        }
    }
}

fn expect(msg: &[u8], kind: u8, what: &'static str) -> Result<(), TlsError> {
    if msg.first() != Some(&kind) {
        return Err(TlsError::unexpected(what));
    }
    Ok(())
}

/// Run the handshake on `rl` (fresh). On success the record layer holds the application
/// traffic keys.
pub fn handshake<T: Transport>(
    cfg: &Config,
    name: &ServerName,
    rl: &mut RecordLayer<T>,
) -> Result<Session, TlsError> {
    if cfg.cipher_suites.is_empty() || cfg.groups.is_empty() {
        return Err(TlsError::NoCommonAlgorithms);
    }
    let mut random = [0u8; 32];
    cfg.rng.fill(&mut random);
    let mut session_id = vec![0u8; if cfg.middlebox_compat { 32 } else { 0 }];
    cfg.rng.fill(&mut session_id);
    let mut shares = vec![KeyShare::generate(cfg.groups[0], cfg)?];

    #[cfg(test)]
    let hello = match &cfg.test_hello {
        Some(t) => {
            shares = vec![KeyShare {
                group: NamedGroup::X25519,
                secret: t.x25519_secret.to_vec(),
                public: x25519::public_key(&t.x25519_secret).to_vec(),
            }];
            hello_from_bytes(&t.client_hello)
        }
        None => build_client_hello(cfg, name, &random, &session_id, &shares, None),
    };
    #[cfg(not(test))]
    let hello = build_client_hello(cfg, name, &random, &session_id, &shares, None);

    let mut transcript = Transcript::new();
    transcript.add(&hello.msg);
    rl.write(record::CT_HANDSHAKE, &hello.msg)?;
    rl.write_version = 0x0303;
    let mut sent_ccs = false;
    let mut sent = hello.sent.clone();
    let session_id = hello.session_id.clone();

    // ServerHello, or HelloRetryRequest then ServerHello.
    let mut retry_suite: Option<u16> = None;
    let sh_msg = loop {
        let m = next_message(rl, false)?;
        expect(&m, HS_SERVER_HELLO, "something other than a ServerHello")?;
        let sh = parse_server_hello(&m[4..])?;
        if sh.session_id != &session_id[..] {
            return Err(TlsError::illegal("the ServerHello didn't echo our session id"));
        }
        if !cfg.cipher_suites.iter().any(|s| s.id() == sh.suite) {
            return Err(TlsError::illegal("a cipher suite we didn't offer"));
        }
        if let Some(rs) = retry_suite
            && rs != sh.suite
        {
            return Err(TlsError::illegal("the cipher suite changed after HelloRetryRequest"));
        }
        if !sh.retry {
            // Downgrade protection (RFC 8446 4.1.3) can't trigger here: we only accept 1.3.
            break m;
        }
        if retry_suite.is_some() {
            return Err(TlsError::unexpected("a second HelloRetryRequest"));
        }
        retry_suite = Some(sh.suite);
        let mut new_group = None;
        let mut cookie = None;
        for (t, v) in &sh.exts {
            match *t {
                EXT_SUPPORTED_VERSIONS => {}
                EXT_KEY_SHARE => {
                    let mut r = Reader::new(v);
                    let g = r.u16().filter(|_| r.is_empty()).ok_or(TlsError::decode("a malformed HelloRetryRequest"))?;
                    let g = cfg
                        .groups
                        .iter()
                        .copied()
                        .find(|x| x.id() == g)
                        .ok_or(TlsError::illegal("a HelloRetryRequest for a group we don't support"))?;
                    if shares.iter().any(|k| k.group == g) {
                        return Err(TlsError::illegal("a HelloRetryRequest for the key share we sent"));
                    }
                    new_group = Some(g);
                }
                EXT_COOKIE => {
                    let mut r = Reader::new(v);
                    let c = r.vec16().filter(|c| !c.is_empty() && r.is_empty()).ok_or(TlsError::decode("a malformed cookie"))?;
                    cookie = Some(c.to_vec());
                }
                _ => return Err(TlsError::proto(Alert::UNSUPPORTED_EXTENSION, "an unexpected extension in HelloRetryRequest")),
            }
        }
        if new_group.is_none() && cookie.is_none() {
            return Err(TlsError::illegal("a HelloRetryRequest that changes nothing"));
        }
        transcript.restart_for_retry();
        transcript.add(&m);
        if let Some(g) = new_group {
            shares = vec![KeyShare::generate(g, cfg)?];
        }
        let hello2 = build_client_hello(cfg, name, &random, &session_id, &shares, cookie.as_deref());
        sent = hello2.sent.clone();
        if !session_id.is_empty() {
            rl.write_plain(record::CT_CHANGE_CIPHER_SPEC, &[1])?;
            sent_ccs = true;
        }
        transcript.add(&hello2.msg);
        rl.write(record::CT_HANDSHAKE, &hello2.msg)?;
    };

    // ServerHello: the key share, the schedule.
    let sh = parse_server_hello(&sh_msg[4..])?;
    let suite = CipherSuite::from_id(sh.suite).ok_or(TlsError::illegal("a cipher suite we didn't offer"))?;
    let _ = sh.random;
    let mut server_share = None;
    for (t, v) in &sh.exts {
        match *t {
            EXT_SUPPORTED_VERSIONS => {}
            EXT_KEY_SHARE => {
                let mut r = Reader::new(v);
                let g = r.u16();
                let k = r.vec16();
                match (g, k) {
                    (Some(g), Some(k)) if r.is_empty() && !k.is_empty() => server_share = Some((g, k)),
                    _ => return Err(TlsError::decode("a malformed key share")),
                }
            }
            EXT_PRE_SHARED_KEY => return Err(TlsError::proto(Alert::UNSUPPORTED_EXTENSION, "a pre-shared key we didn't offer")),
            _ => return Err(TlsError::proto(Alert::UNSUPPORTED_EXTENSION, "an unexpected extension in ServerHello")),
        }
    }
    let Some((group_id, server_pub)) = server_share else {
        return Err(TlsError::proto(Alert::MISSING_EXTENSION, "a ServerHello without a key share"));
    };
    let share = shares
        .iter()
        .find(|k| k.group.id() == group_id)
        .ok_or(TlsError::illegal("a key share for a group we didn't send"))?;
    let group = share.group;
    let mut shared = share.agree(server_pub)?;
    drop(shares);
    transcript.add(&sh_msg);
    let schedule = Schedule::handshake(suite, &shared, &transcript.hash());
    wipe(&mut shared);
    if rl.handshake_pending() {
        return Err(TlsError::unexpected("handshake data across the key change"));
    }
    rl.read_keys = Some(RecordKeys::new(suite, &schedule.server_hs));
    rl.write_keys = Some(RecordKeys::new(suite, &schedule.client_hs));

    // EncryptedExtensions.
    let ee = next_message(rl, true)?;
    expect(&ee, HS_ENCRYPTED_EXTENSIONS, "something other than EncryptedExtensions")?;
    transcript.add(&ee);
    let mut r = Reader::new(&ee[4..]);
    let ee_exts = r
        .vec16()
        .filter(|_| r.is_empty())
        .ok_or(TlsError::decode("malformed EncryptedExtensions"))?;
    let mut alpn = None;
    for (t, v) in extensions(ee_exts)? {
        if !sent.contains(&t) {
            return Err(TlsError::proto(Alert::UNSUPPORTED_EXTENSION, "an extension we didn't ask for"));
        }
        match t {
            EXT_SERVER_NAME => {
                if !v.is_empty() {
                    return Err(TlsError::decode("a server_name answer that isn't empty"));
                }
            }
            EXT_SUPPORTED_GROUPS | EXT_RECORD_SIZE_LIMIT | EXT_MAX_FRAGMENT_LENGTH => {}
            EXT_ALPN => {
                let mut r = Reader::new(v);
                let mut list = r
                    .vec16()
                    .filter(|_| r.is_empty())
                    .map(Reader::new)
                    .ok_or(TlsError::decode("a malformed ALPN answer"))?;
                let p = list
                    .vec8()
                    .filter(|p| !p.is_empty() && list.is_empty())
                    .ok_or(TlsError::decode("a malformed ALPN answer"))?;
                if !cfg.alpn.iter().any(|a| a[..] == *p) {
                    return Err(TlsError::NoApplicationProtocol);
                }
                alpn = Some(p.to_vec());
            }
            EXT_EARLY_DATA => return Err(TlsError::proto(Alert::UNSUPPORTED_EXTENSION, "early data we didn't offer")),
            _ => return Err(TlsError::illegal("an extension that doesn't belong in EncryptedExtensions")),
        }
    }

    // CertificateRequest (we have no certificate: we'll send an empty one), Certificate.
    let mut msg = next_message(rl, true)?;
    let mut cert_request_context: Option<Vec<u8>> = None;
    if msg.first() == Some(&HS_CERTIFICATE_REQUEST) {
        transcript.add(&msg);
        let mut r = Reader::new(&msg[4..]);
        let ctx = r.vec8().ok_or(TlsError::decode("a malformed CertificateRequest"))?;
        r.vec16().filter(|_| r.is_empty()).ok_or(TlsError::decode("a malformed CertificateRequest"))?;
        cert_request_context = Some(ctx.to_vec());
        msg = next_message(rl, true)?;
    }
    expect(&msg, HS_CERTIFICATE, "something other than a Certificate")?;
    let certificates = parse_certificate(&msg[4..])?;
    let now = cfg.clock.unix_seconds().filter(|&t| t >= crate::EARLIEST_PLAUSIBLE_TIME).ok_or(TlsError::ClockNotSet)?;
    let refs: Vec<&[u8]> = certificates.iter().map(|c| &c[..]).collect();
    #[cfg(test)]
    let leaf = if cfg.test_skip_certificate_checks {
        Certificate::parse(refs[0])?
    } else {
        verify::verify_server_chain(&refs, &cfg.roots, now as i64, name, &cfg.policy)?
    };
    #[cfg(not(test))]
    let leaf = verify::verify_server_chain(&refs, &cfg.roots, now as i64, name, &cfg.policy)?;
    transcript.add(&msg);

    // CertificateVerify.
    let cv = next_message(rl, true)?;
    expect(&cv, HS_CERTIFICATE_VERIFY, "something other than CertificateVerify")?;
    let mut r = Reader::new(&cv[4..]);
    let scheme = r.u16().ok_or(TlsError::decode("a malformed CertificateVerify"))?;
    let sig = r.vec16().filter(|_| r.is_empty()).ok_or(TlsError::decode("a malformed CertificateVerify"))?;
    if !SIGNATURE_SCHEMES.contains(&scheme) {
        return Err(TlsError::illegal("a signature scheme we didn't offer"));
    }
    let alg = cv_algorithm(scheme, &leaf.key)
        .ok_or(TlsError::illegal("a signature scheme that doesn't fit the certificate"))?;
    let mut signed = Vec::with_capacity(64 + 34 + HASH_LEN);
    signed.resize(64, 0x20);
    signed.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
    signed.extend_from_slice(&transcript.hash());
    match verify::verify_signature(alg, &leaf.key, &signed, sig, &cfg.policy) {
        Ok(()) => {}
        Err(CertError::WeakKey) => return Err(TlsError::Certificate(CertError::WeakKey)),
        Err(CertError::Malformed) => return Err(TlsError::Certificate(CertError::Malformed)),
        Err(_) => return Err(TlsError::BadHandshakeSignature),
    }
    transcript.add(&cv);

    // Server Finished.
    let fin = next_message(rl, true)?;
    expect(&fin, HS_FINISHED, "something other than Finished")?;
    let want = keys::finished_verify_data(&schedule.server_hs, &transcript.hash());
    if !ct_eq(&fin[4..], &want) {
        return Err(TlsError::Tampered);
    }
    transcript.add(&fin);
    if rl.handshake_pending() {
        return Err(TlsError::unexpected("handshake data across the key change"));
    }
    let (client_secret, server_secret) = schedule.application(&transcript.hash());
    rl.read_keys = Some(RecordKeys::new(suite, &server_secret));
    rl.allow_ccs = false;

    // Our flight: [CCS] [empty Certificate] Finished.
    if !session_id.is_empty() && !sent_ccs {
        rl.write_plain(record::CT_CHANGE_CIPHER_SPEC, &[1])?;
    }
    if let Some(ctx) = cert_request_context {
        let mut b = Vec::new();
        with_len(&mut b, 1, |o| o.extend_from_slice(&ctx));
        with_len(&mut b, 3, |_| {});
        let m = handshake_message(HS_CERTIFICATE, &b);
        transcript.add(&m);
        rl.write(record::CT_HANDSHAKE, &m)?;
    }
    let verify_data = keys::finished_verify_data(&schedule.client_hs, &transcript.hash());
    let m = handshake_message(HS_FINISHED, &verify_data);
    rl.write(record::CT_HANDSHAKE, &m)?;
    rl.write_keys = Some(RecordKeys::new(suite, &client_secret));
    rl.established = true;

    Ok(Session {
        suite,
        group,
        alpn,
        certificates,
        hello_retry: retry_suite.is_some(),
        client_secret,
        server_secret,
    })
}

/// Certificate (RFC 8446 4.4.2): empty context, then entries of (cert_data, extensions).
fn parse_certificate(body: &[u8]) -> Result<Vec<Vec<u8>>, TlsError> {
    let d = || TlsError::decode("a malformed Certificate message");
    let mut r = Reader::new(body);
    let ctx = r.vec8().ok_or_else(d)?;
    if !ctx.is_empty() {
        return Err(TlsError::illegal("a Certificate with a request context"));
    }
    let mut list = Reader::new(r.vec24().ok_or_else(d)?);
    if !r.is_empty() {
        return Err(d());
    }
    let mut certs = Vec::new();
    while !list.is_empty() {
        let c = list.vec24().ok_or_else(d)?;
        let e = list.vec16().ok_or_else(d)?;
        if c.is_empty() {
            return Err(d());
        }
        extensions(e)?; // well-formed; contents (OCSP, SCTs) aren't used
        certs.push(c.to_vec());
    }
    if certs.is_empty() {
        return Err(TlsError::Certificate(CertError::NoCertificate));
    }
    Ok(certs)
}

/// What a post-handshake message asks of the connection.
pub enum PostHandshake {
    /// A NewSessionTicket, parsed and dropped (no resumption).
    Ticket,
    /// KeyUpdate: the read key changed; `respond` = the server asked us to update ours.
    KeyUpdate { respond: bool },
}

/// Parse a post-handshake message (RFC 8446 4.6).
pub fn post_handshake(msg: &[u8]) -> Result<PostHandshake, TlsError> {
    let body = &msg[4..];
    match msg[0] {
        HS_NEW_SESSION_TICKET => {
            let d = || TlsError::decode("a malformed NewSessionTicket");
            let mut r = Reader::new(body);
            let lifetime = r.u32().ok_or_else(d)?;
            r.u32().ok_or_else(d)?; // ticket_age_add
            r.vec8().ok_or_else(d)?; // ticket_nonce
            let ticket = r.vec16().ok_or_else(d)?;
            let exts = r.vec16().ok_or_else(d)?;
            if !r.is_empty() || ticket.is_empty() {
                return Err(d());
            }
            if lifetime > 604_800 {
                return Err(TlsError::illegal("a session ticket lifetime over 7 days"));
            }
            extensions(exts)?;
            Ok(PostHandshake::Ticket)
        }
        HS_KEY_UPDATE => match body {
            [0] => Ok(PostHandshake::KeyUpdate { respond: false }),
            [1] => Ok(PostHandshake::KeyUpdate { respond: true }),
            [_] => Err(TlsError::illegal("a malformed KeyUpdate")),
            _ => Err(TlsError::decode("a malformed KeyUpdate")),
        },
        HS_CERTIFICATE_REQUEST => Err(TlsError::unexpected("a post-handshake CertificateRequest")),
        _ => Err(TlsError::unexpected("a handshake message after the handshake")),
    }
}

/// A KeyUpdate message (RFC 8446 4.6.3).
pub fn key_update_message(request: bool) -> Vec<u8> {
    handshake_message(HS_KEY_UPDATE, &[request as u8])
}
