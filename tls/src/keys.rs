//! The TLS 1.3 key schedule (RFC 8446 7) and the record protection keys. Both cipher suites
//! we offer use SHA-256, so the transcript hash and HKDF are SHA-256 throughout.
//!
//! Every secret is a [`Secret`], wiped when dropped.

use milkcrypto::gcm::Aes128Gcm;
use milkcrypto::hkdf;
use milkcrypto::hmac::Hmac;
use milkcrypto::sha256::{Sha256, sha256};
use milkcrypto::{aead, wipe};

pub const HASH_LEN: usize = 32;

/// A 32-byte secret that wipes itself.
#[derive(Clone)]
pub struct Secret(pub [u8; HASH_LEN]);

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// The two cipher suites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CipherSuite {
    /// TLS_AES_128_GCM_SHA256 (0x1301).
    Aes128GcmSha256,
    /// TLS_CHACHA20_POLY1305_SHA256 (0x1303).
    Chacha20Poly1305Sha256,
}

impl CipherSuite {
    pub fn id(self) -> u16 {
        match self {
            CipherSuite::Aes128GcmSha256 => 0x1301,
            CipherSuite::Chacha20Poly1305Sha256 => 0x1303,
        }
    }

    pub fn from_id(id: u16) -> Option<CipherSuite> {
        match id {
            0x1301 => Some(CipherSuite::Aes128GcmSha256),
            0x1303 => Some(CipherSuite::Chacha20Poly1305Sha256),
            _ => None,
        }
    }

    pub fn key_len(self) -> usize {
        match self {
            CipherSuite::Aes128GcmSha256 => 16,
            CipherSuite::Chacha20Poly1305Sha256 => 32,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CipherSuite::Aes128GcmSha256 => "TLS_AES_128_GCM_SHA256",
            CipherSuite::Chacha20Poly1305Sha256 => "TLS_CHACHA20_POLY1305_SHA256",
        }
    }
}

/// HKDF-Extract with SHA-256.
pub fn extract(salt: &[u8], ikm: &[u8]) -> Secret {
    let mut out = [0u8; HASH_LEN];
    hkdf::extract::<Sha256>(salt, ikm, &mut out);
    Secret(out)
}

/// HKDF-Expand-Label(secret, label, context, out.len()) (RFC 8446 7.1). `label` without the
/// "tls13 " prefix; at most 249 bytes, context at most 255, out at most 255 * 32 (all fixed
/// by our callers).
pub fn expand_label(secret: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
    let mut info = [0u8; 2 + 1 + 255 + 1 + 255];
    let llen = 6 + label.len();
    info[..2].copy_from_slice(&(out.len() as u16).to_be_bytes());
    info[2] = llen as u8;
    info[3..9].copy_from_slice(b"tls13 ");
    info[9..9 + label.len()].copy_from_slice(label);
    let c = 3 + llen;
    info[c] = context.len() as u8;
    info[c + 1..c + 1 + context.len()].copy_from_slice(context);
    let ok = hkdf::expand::<Sha256>(secret, &info[..c + 1 + context.len()], out);
    debug_assert!(ok);
}

/// Derive-Secret(secret, label, transcript hash).
pub fn derive_secret(secret: &Secret, label: &[u8], transcript_hash: &[u8; HASH_LEN]) -> Secret {
    let mut out = [0u8; HASH_LEN];
    expand_label(&secret.0, label, transcript_hash, &mut out);
    Secret(out)
}

/// SHA-256 of nothing: the transcript hash for the "derived" steps.
pub fn empty_hash() -> [u8; HASH_LEN] {
    sha256(b"")
}

/// The running transcript hash (RFC 8446 4.4.1).
#[derive(Clone)]
pub struct Transcript {
    h: Sha256,
}

impl Transcript {
    pub fn new() -> Transcript {
        Transcript { h: Sha256::new() }
    }

    pub fn add(&mut self, handshake_message: &[u8]) {
        self.h.update(handshake_message);
    }

    pub fn hash(&self) -> [u8; HASH_LEN] {
        self.h.clone().finish()
    }

    /// After a HelloRetryRequest: the transcript restarts with a synthetic message_hash
    /// message holding the hash of ClientHello1 (RFC 8446 4.4.1).
    pub fn restart_for_retry(&mut self) {
        let ch1 = self.hash();
        let mut h = Sha256::new();
        h.update(&[254, 0, 0, HASH_LEN as u8]);
        h.update(&ch1);
        self.h = h;
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

/// finished_key = HKDF-Expand-Label(base, "finished", "", 32); verify_data =
/// HMAC(finished_key, transcript hash) (RFC 8446 4.4.4).
pub fn finished_verify_data(base: &Secret, transcript_hash: &[u8; HASH_LEN]) -> [u8; HASH_LEN] {
    let mut fk = [0u8; HASH_LEN];
    expand_label(&base.0, b"finished", b"", &mut fk);
    let mut m = Hmac::<Sha256>::new(&fk);
    m.update(transcript_hash);
    let mut out = [0u8; HASH_LEN];
    m.finish_into(&mut out);
    wipe(&mut fk);
    out
}

/// application_traffic_secret_N+1 (RFC 8446 7.2).
pub fn next_traffic_secret(s: &Secret) -> Secret {
    let mut out = [0u8; HASH_LEN];
    expand_label(&s.0, b"traffic upd", b"", &mut out);
    Secret(out)
}

/// The schedule's secrets, step by step.
pub struct Schedule {
    pub suite: CipherSuite,
    pub handshake_secret: Secret,
    pub client_hs: Secret,
    pub server_hs: Secret,
}

impl Schedule {
    /// Early Secret (no PSK), then Handshake Secret from the (EC)DHE shared secret, then the
    /// handshake traffic secrets over ClientHello..ServerHello.
    pub fn handshake(suite: CipherSuite, shared: &[u8], hello_hash: &[u8; HASH_LEN]) -> Schedule {
        let early = extract(&[0u8; HASH_LEN], &[0u8; HASH_LEN]);
        let derived = derive_secret(&early, b"derived", &empty_hash());
        let handshake_secret = extract(&derived.0, shared);
        let client_hs = derive_secret(&handshake_secret, b"c hs traffic", hello_hash);
        let server_hs = derive_secret(&handshake_secret, b"s hs traffic", hello_hash);
        Schedule {
            suite,
            handshake_secret,
            client_hs,
            server_hs,
        }
    }

    /// Master Secret, then the application traffic secrets (client, server) over
    /// ClientHello..server Finished.
    pub fn application(&self, finished_hash: &[u8; HASH_LEN]) -> (Secret, Secret) {
        let derived = derive_secret(&self.handshake_secret, b"derived", &empty_hash());
        let master = extract(&derived.0, &[0u8; HASH_LEN]);
        (
            derive_secret(&master, b"c ap traffic", finished_hash),
            derive_secret(&master, b"s ap traffic", finished_hash),
        )
    }
}

/// The AEAD of a suite, keyed.
pub enum Aead {
    Aes(alloc::boxed::Box<Aes128Gcm>),
    Chacha([u8; 32]),
}

impl Drop for Aead {
    fn drop(&mut self) {
        if let Aead::Chacha(k) = self {
            wipe(k);
        }
    }
}

/// One direction's record protection: key, static IV and sequence number (RFC 8446 5.3).
pub struct RecordKeys {
    aead: Aead,
    iv: [u8; 12],
    pub seq: u64,
}

impl Drop for RecordKeys {
    fn drop(&mut self) {
        wipe(&mut self.iv);
    }
}

impl RecordKeys {
    /// key = HKDF-Expand-Label(secret, "key", "", key_len), iv = ...("iv", "", 12).
    pub fn new(suite: CipherSuite, secret: &Secret) -> RecordKeys {
        let mut key = [0u8; 32];
        let klen = suite.key_len();
        expand_label(&secret.0, b"key", b"", &mut key[..klen]);
        let mut iv = [0u8; 12];
        expand_label(&secret.0, b"iv", b"", &mut iv);
        let aead = match suite {
            CipherSuite::Aes128GcmSha256 => {
                let mut k16 = [0u8; 16];
                k16.copy_from_slice(&key[..16]);
                let a = Aes128Gcm::new(&k16);
                wipe(&mut k16);
                Aead::Aes(alloc::boxed::Box::new(a))
            }
            CipherSuite::Chacha20Poly1305Sha256 => Aead::Chacha(key),
        };
        wipe(&mut key);
        RecordKeys { aead, iv, seq: 0 }
    }

    fn nonce(&self) -> [u8; 12] {
        let mut n = self.iv;
        for (a, b) in n[4..].iter_mut().zip(self.seq.to_be_bytes()) {
            *a ^= b;
        }
        n
    }

    /// Encrypt `data` in place with the next sequence number; returns the tag. None when the
    /// sequence number is used up (2^64 - 1 records: never in practice).
    pub fn seal(&mut self, aad: &[u8], data: &mut [u8]) -> Option<[u8; 16]> {
        if self.seq == u64::MAX {
            return None;
        }
        let nonce = self.nonce();
        let tag = match &self.aead {
            Aead::Aes(a) => a.seal(&nonce, aad, data),
            Aead::Chacha(k) => aead::seal(k, &nonce, aad, data),
        };
        self.seq += 1;
        Some(tag)
    }

    /// Check and decrypt in place with the next sequence number. False if altered (or the
    /// sequence number is used up).
    pub fn open(&mut self, aad: &[u8], data: &mut [u8], tag: &[u8; 16]) -> bool {
        if self.seq == u64::MAX {
            return false;
        }
        let nonce = self.nonce();
        let ok = match &self.aead {
            Aead::Aes(a) => a.open(&nonce, aad, data, tag),
            Aead::Chacha(k) => aead::open(k, &nonce, aad, data, tag),
        };
        if ok {
            self.seq += 1;
        }
        ok
    }
}
