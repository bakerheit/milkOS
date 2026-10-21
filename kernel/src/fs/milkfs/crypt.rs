//! Encryption at rest for milkfs (see docs/filesystem.md, "Encryption at rest").
//!
//! A random 256-bit volume key (VK) encrypts every block except the two superblocks and
//! the keyring. VK is stored only wrapped, in key slots in the keyring block: one per
//! passphrase (scrypt) and one for the recovery key. Two keys are derived from VK:
//! `enc = HMAC(VK, "milkfs v1 enc")`, `mac = HMAC(VK, "milkfs v1 mac")`.
//!
//! **Blocks: XChaCha20 with a synthetic IV (SIV).** A block's checksum field (in the
//! pointer to it, 8 bytes) becomes a keyed tag over its plaintext and location:
//!
//! ```text
//! tag   = HMAC-SHA256(mac, "MKB1" | block u64 | birth u64 | plaintext)[..8]
//! nonce = tag | block u64 | birth u64                       (24 bytes)
//! ct    = plaintext XOR XChaCha20(enc, nonce)
//! ```
//!
//! Reading decrypts with the nonce rebuilt from the pointer and checks the tag (constant
//! time). Why: blocks are exactly 4 KiB and pointers have 8 spare bytes, no room for a
//! stored 24-byte nonce and 16-byte tag; and copy-on-write can reuse a (block, birth)
//! within one transaction, so a counter or random nonce couldn't be rebuilt. With SIV the
//! nonce is a function of the content: the same nonce means the same plaintext and the
//! same ciphertext, so a "reused" nonce leaks only that two writes were identical, never
//! a keystream. The tag binds the block number and birth, so blocks can't be swapped or
//! replayed elsewhere; tampering is caught with probability 1 - 2^-64 per block. The
//! checksum is thus over the plaintext (keyed): verifying a block needs the key.
//!
//! **Superblock**: stays plaintext (it has to be read before unlocking: label, generation,
//! snapshot names and times, sizes are visible), but carries `sb_mac` (HMAC-SHA256 with
//! `mac` over its first `SB_MAC` bytes), checked after unlocking: a tampered superblock
//! (e.g. an old root pointer) refuses to mount.
//!
//! **Key slots**: `KEK = scrypt(passphrase, salt, N, r, p)` (or, for the recovery key,
//! `HMAC(salt, recovery bytes)`); `wrapped|tag = XChaCha20-Poly1305(KEK, nonce,
//! aad = "milkfs slot" | uuid | kind, VK)` with a random nonce. Changing a passphrase
//! rewraps VK in a new slot; no block is rewritten.

use alloc::string::String;
use alloc::vec::Vec;

use super::format::*;
use crate::crypto::{ct_eq, hmac, wipe, xchacha20};
use crate::fs::errno::{Errno, Result};

/// scrypt parameters for new passphrase slots: N = 2^16, r = 8, p = 4: 64 MiB, ~0.35 s.
pub const KDF_LOG_N: u8 = 16;
pub const KDF_R: u8 = 8;
pub const KDF_P: u8 = 4;

/// The keys of an unlocked volume (zeroized on drop).
pub struct VolumeKey {
    vk: [u8; 32],
    enc: [u8; 32],
    mac: [u8; 32],
}

impl Drop for VolumeKey {
    fn drop(&mut self) {
        wipe(&mut self.vk);
        wipe(&mut self.enc);
        wipe(&mut self.mac);
    }
}

impl core::fmt::Debug for VolumeKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("VolumeKey(..)")
    }
}

impl VolumeKey {
    pub fn from_master(vk: &[u8; 32]) -> VolumeKey {
        VolumeKey {
            vk: *vk,
            enc: hmac::hmac_sha256(vk, b"milkfs v1 enc"),
            mac: hmac::hmac_sha256(vk, b"milkfs v1 mac"),
        }
    }

    fn tag(&self, block: u64, birth: u64, data: &[u8]) -> u64 {
        let mut h = hmac::HmacSha256::new(&self.mac);
        h.update(b"MKB1")
            .update(&block.to_le_bytes())
            .update(&birth.to_le_bytes())
            .update(data);
        u64::from_le_bytes(h.finish()[..8].try_into().unwrap())
    }

    fn nonce(tag: u64, block: u64, birth: u64) -> [u8; 24] {
        let mut n = [0u8; 24];
        n[..8].copy_from_slice(&tag.to_le_bytes());
        n[8..16].copy_from_slice(&block.to_le_bytes());
        n[16..].copy_from_slice(&birth.to_le_bytes());
        n
    }

    /// Encrypt block `block` (born `birth`) in place; returns its checksum (the tag).
    pub fn seal(&self, block: u64, birth: u64, data: &mut [u8]) -> u64 {
        let tag = self.tag(block, birth, data);
        xchacha20::xor(&self.enc, &Self::nonce(tag, block, birth), 0, data);
        tag
    }

    /// Decrypt in place and check the tag. False: wrong key, damaged or tampered.
    pub fn open(&self, block: u64, birth: u64, csum: u64, data: &mut [u8]) -> bool {
        xchacha20::xor(&self.enc, &Self::nonce(csum, block, birth), 0, data);
        ct_eq(
            &self.tag(block, birth, data).to_le_bytes(),
            &csum.to_le_bytes(),
        )
    }

    /// MAC of a superblock's first `SB_MAC` bytes.
    pub fn sb_mac(&self, sb: &[u8]) -> u64 {
        let mut h = hmac::HmacSha256::new(&self.mac);
        h.update(b"MKSB").update(&sb[..SB_MAC]);
        u64::from_le_bytes(h.finish()[..8].try_into().unwrap())
    }
}

fn aad(uuid: &[u8; 16], kind: u8) -> Vec<u8> {
    let mut a = Vec::from(&b"milkfs slot"[..]);
    a.extend_from_slice(uuid);
    a.push(kind);
    a
}

/// The recovery key as typed: 32 hex digits, any dashes or spaces ignored.
pub fn parse_recovery(s: &str) -> Option<[u8; 16]> {
    let h: Vec<u8> = s.bytes().filter(|c| *c != b'-' && *c != b' ').collect();
    if h.len() != 32 || !h.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(core::str::from_utf8(&h[2 * i..2 * i + 2]).ok()?, 16).ok()?;
    }
    Some(out)
}

/// The recovery key formatted for people: 8 groups of 4 hex digits.
pub fn format_recovery(k: &[u8; 16]) -> String {
    let mut s = String::new();
    for (i, b) in k.iter().enumerate() {
        if i > 0 && i % 2 == 0 {
            s.push('-');
        }
        s.push_str(&alloc::format!("{b:02X}"));
    }
    s
}

fn kek(slot: &KeySlot, secret: &[u8]) -> Option<[u8; 32]> {
    let mut k = [0u8; 32];
    match slot.kind {
        SLOT_PASSPHRASE => crate::crypto::frames::scrypt(
            secret,
            &slot.salt,
            slot.log_n,
            slot.r as u32,
            slot.p as u32,
            &mut k,
        )?,
        SLOT_RECOVERY => k = hmac::hmac_sha256(&slot.salt, secret),
        _ => return None,
    }
    Some(k)
}

/// Unwrap the volume key with `secret` (a passphrase, or the recovery key as typed).
/// Returns the key and the index of the slot that opened.
pub fn unlock(ring: &Keyring, secret: &str) -> Result<(VolumeKey, usize)> {
    let recovery = parse_recovery(secret);
    for (i, s) in ring.slots.iter().enumerate() {
        let input: &[u8] = match (s.kind, &recovery) {
            (SLOT_RECOVERY, Some(r)) => r,
            (SLOT_PASSPHRASE, _) => secret.as_bytes(),
            _ => continue,
        };
        let Some(mut k) = kek(s, input) else {
            return Err(Errno::ENOMEM); // scrypt couldn't get its memory
        };
        let mut vk = s.wrapped;
        let ok = xchacha20::open(&k, &s.nonce, &aad(&ring.uuid, s.kind), &mut vk, &s.tag);
        wipe(&mut k);
        if ok {
            let key = VolumeKey::from_master(&vk);
            wipe(&mut vk);
            return Ok((key, i));
        }
    }
    Err(Errno::EACCES)
}

/// A new slot of `kind` holding `key`'s volume key, opened by `secret`.
pub fn wrap(ring_uuid: &[u8; 16], key: &VolumeKey, kind: u8, secret: &[u8]) -> Result<KeySlot> {
    let mut s = KeySlot {
        kind,
        log_n: KDF_LOG_N,
        r: KDF_R,
        p: KDF_P,
        ..KeySlot::default()
    };
    crate::crypto::rng::fill(&mut s.salt);
    crate::crypto::rng::fill(&mut s.nonce);
    let mut k = kek(&s, secret).ok_or(Errno::ENOMEM)?;
    s.wrapped = key.vk;
    s.tag = xchacha20::seal(&k, &s.nonce, &aad(ring_uuid, kind), &mut s.wrapped);
    wipe(&mut k);
    Ok(s)
}

impl VolumeKey {
    /// The same volume key?
    pub fn same_as(&self, o: &VolumeKey) -> bool {
        ct_eq(&self.vk, &o.vk)
    }
}

impl super::volume::State {
    /// The current keyring (encrypted volumes).
    pub fn read_keyring(&self) -> Result<Keyring> {
        if self.keyring.is_none() {
            return Err(Errno::EINVAL);
        }
        let mut b = [0u8; BLOCK];
        self.store.cache.read(self.keyring.block, &mut b)?;
        if xxh64(&b) != self.keyring.csum {
            return Err(Errno::EIO);
        }
        Keyring::decode(&b).ok_or(Errno::EIO)
    }

    /// Rewrap the volume key under passphrase `new`, `old` (a passphrase or the recovery
    /// key) proving we may: a new keyring block, one commit, no data rewritten.
    pub fn rewrap(&mut self, old: &str, new: &str) -> Result<()> {
        let key = self.store.crypt.clone().ok_or(Errno::EINVAL)?;
        let ring = self.read_keyring()?;
        let (k2, idx) = unlock(&ring, old)?;
        if !k2.same_as(&key) {
            return Err(Errno::EACCES);
        }
        let slot = wrap(&ring.uuid, &key, SLOT_PASSPHRASE, new.as_bytes())?;
        let mut ring2 = ring.clone();
        let target = if ring.slots[idx].kind == SLOT_PASSPHRASE {
            Some(idx)
        } else {
            ring.slots.iter().position(|s| s.kind == SLOT_PASSPHRASE)
        };
        match target {
            Some(t) => ring2.slots[t] = slot,
            None => ring2.slots.push(slot),
        }
        self.commit()?;
        let p = self.store.write_plain(&ring2.encode())?;
        self.store
            .free_after_commit(self.keyring.block, self.keyring.birth);
        self.keyring = p;
        self.meta_dirty = true;
        self.commit()
    }

    /// Lock: drop the volume key (wiped once its last holder lets go, see `VolumeKey`'s
    /// `Drop`) and the decoded tree nodes, and refuse every write from now on (a block
    /// written without the key would land in plaintext). Reads fail too: every pointer's
    /// checksum is a SIV tag that no longer verifies. Commit first. True if the key was
    /// wiped right here (nothing else held it).
    pub fn forget_key(&mut self) -> bool {
        self.broken = true;
        let k = self.store.crypt.take();
        self.store.forget_nodes();
        k.is_some_and(|k| alloc::sync::Arc::strong_count(&k) == 1)
    }
}

// ---- encrypted send streams -------------------------------------------------------------

/// First 8 bytes of an encrypted stream (a plain one starts with its header record).
pub const SEALED_MAGIC: &[u8; 8] = b"MILKSTRX";
pub const SEALED_VERSION: u32 = 1;
/// The envelope in front of an encrypted stream's records.
pub const ENVELOPE_LEN: usize = 128;
/// Sealed-record flag: this is the last record (END); anything after it is refused.
pub const SEAL_LAST: u32 = 1;
const ENV_AUTH: usize = 48;

/// The key of one encrypted send stream (random, wrapped in the envelope under a key from
/// the stream's passphrase). Records are sealed in order with a counter nonce.
///
/// ```text
/// envelope (128 bytes, plaintext, authenticated):
///   0 "MILKSTRX"  8 version u32  12 log_n u8 r u8 p u8 kind u8 (1 = passphrase)
///  16 salt [16]  32 nonce prefix [16]  48 wrap nonce [24]  72 wrapped key [32]
/// 104 wrap tag [16]  120 zero [8]
/// KEK = scrypt(passphrase, salt, 2^log_n, r, p)
/// wrapped|tag = XChaCha20-Poly1305(KEK, wrap nonce, aad = "milkfs stream key" | envelope[0..48], SK)
/// record i: len u32 | flags u32 | XChaCha20-Poly1305(SK, prefix | i u64, aad = "MKSR" | i u64 | len | flags, plain record) | tag [16]
/// ```
pub struct StreamKey {
    key: [u8; 32],
    prefix: [u8; 16],
    next: u64,
}

impl Drop for StreamKey {
    fn drop(&mut self) {
        wipe(&mut self.key);
    }
}

fn stream_wrap_aad(env: &[u8]) -> Vec<u8> {
    let mut a = Vec::from(&b"milkfs stream key"[..]);
    a.extend_from_slice(&env[..ENV_AUTH]);
    a
}

impl StreamKey {
    /// A fresh stream key under `passphrase`; returns it and the envelope to write first.
    pub fn create(passphrase: &[u8]) -> Result<(StreamKey, [u8; ENVELOPE_LEN])> {
        let mut env = [0u8; ENVELOPE_LEN];
        env[..8].copy_from_slice(SEALED_MAGIC);
        env[8..12].copy_from_slice(&SEALED_VERSION.to_le_bytes());
        env[12..16].copy_from_slice(&[KDF_LOG_N, KDF_R, KDF_P, SLOT_PASSPHRASE]);
        crate::crypto::rng::fill(&mut env[16..48]);
        crate::crypto::rng::fill(&mut env[48..72]);
        let mut sk = StreamKey {
            key: [0; 32],
            prefix: env[32..48].try_into().unwrap(),
            next: 0,
        };
        crate::crypto::rng::fill(&mut sk.key);
        let mut kek = [0u8; 32];
        crate::crypto::frames::scrypt(
            passphrase,
            &env[16..32],
            KDF_LOG_N,
            KDF_R as u32,
            KDF_P as u32,
            &mut kek,
        )
        .ok_or(Errno::ENOMEM)?;
        let mut w = sk.key;
        let nonce: [u8; 24] = env[48..72].try_into().unwrap();
        let tag = xchacha20::seal(&kek, &nonce, &stream_wrap_aad(&env), &mut w);
        wipe(&mut kek);
        env[72..104].copy_from_slice(&w);
        env[104..120].copy_from_slice(&tag);
        Ok((sk, env))
    }

    /// Open an envelope with `passphrase`: EINVAL if it isn't one this kernel reads,
    /// EACCES for a wrong passphrase (or an altered envelope).
    pub fn open(env: &[u8; ENVELOPE_LEN], passphrase: &[u8]) -> Result<StreamKey> {
        let (log_n, r, p, kind) = (env[12], env[13], env[14], env[15]);
        if &env[..8] != SEALED_MAGIC
            || u32::from_le_bytes(env[8..12].try_into().unwrap()) != SEALED_VERSION
            || kind != SLOT_PASSPHRASE
            || env[120..] != [0; 8]
            || !crate::crypto::scrypt::params_ok(log_n, r as u32, p as u32)
            || log_n > 20
            || 128 * r as u64 * (1u64 << log_n) > 128 << 20 // at most 128 MiB of scrypt
            || p > 16
        {
            return Err(Errno::EINVAL);
        }
        let mut kek = [0u8; 32];
        crate::crypto::frames::scrypt(
            passphrase,
            &env[16..32],
            log_n,
            r as u32,
            p as u32,
            &mut kek,
        )
        .ok_or(Errno::ENOMEM)?;
        let mut sk = StreamKey {
            key: env[72..104].try_into().unwrap(),
            prefix: env[32..48].try_into().unwrap(),
            next: 0,
        };
        let nonce: [u8; 24] = env[48..72].try_into().unwrap();
        let tag: [u8; 16] = env[104..120].try_into().unwrap();
        let ok = xchacha20::open(&kek, &nonce, &stream_wrap_aad(env), &mut sk.key, &tag);
        wipe(&mut kek);
        if !ok {
            return Err(Errno::EACCES);
        }
        Ok(sk)
    }

    fn nonce_aad(&self, len: u32, flags: u32) -> ([u8; 24], [u8; 20]) {
        let mut n = [0u8; 24];
        n[..16].copy_from_slice(&self.prefix);
        n[16..].copy_from_slice(&self.next.to_le_bytes());
        let mut a = [0u8; 20];
        a[..4].copy_from_slice(b"MKSR");
        a[4..12].copy_from_slice(&self.next.to_le_bytes());
        a[12..16].copy_from_slice(&len.to_le_bytes());
        a[16..].copy_from_slice(&flags.to_le_bytes());
        (n, a)
    }

    /// Append the next record, sealed, to `out`.
    pub fn seal_record(&mut self, plain: &[u8], last: bool, out: &mut Vec<u8>) {
        let (len, flags) = (plain.len() as u32, if last { SEAL_LAST } else { 0 });
        let (n, a) = self.nonce_aad(len, flags);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        let at = out.len();
        out.extend_from_slice(plain);
        let tag = xchacha20::seal(&self.key, &n, &a, &mut out[at..]);
        out.extend_from_slice(&tag);
        self.next += 1;
    }

    /// Open the next sealed record (`hdr` = its len and flags, `body` = ciphertext and
    /// tag) in place; returns the flags. EIO if it was altered, reordered or isn't next.
    pub fn open_record(&mut self, hdr: &[u8; 8], body: &mut Vec<u8>) -> Result<u32> {
        let len = u32::from_le_bytes(hdr[..4].try_into().unwrap());
        let flags = u32::from_le_bytes(hdr[4..].try_into().unwrap());
        if body.len() != len as usize + 16 || flags & !SEAL_LAST != 0 {
            return Err(Errno::EIO);
        }
        let tag: [u8; 16] = body[len as usize..].try_into().unwrap();
        body.truncate(len as usize);
        let (n, a) = self.nonce_aad(len, flags);
        if !xchacha20::open(&self.key, &n, &a, body, &tag) {
            return Err(Errno::EIO);
        }
        self.next += 1;
        Ok(flags)
    }
}
