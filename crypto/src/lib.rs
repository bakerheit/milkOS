//! milkcrypto: the cryptographic primitives of milkOS, written from scratch, in ONE place,
//! shared by the kernel (`kernel/src/crypto`, which adds the CSPRNG) and userland
//! (`rt::crypto`, also re-exported as `rt::sha256`, `rt::sha512`, `rt::ed25519`).
//! `no_std` + `alloc`, no dependencies; the only `unsafe` is the volatile write in
//! [`wipe`]. Every module has a `selftest()` with its standard's test vectors; [`selftest`]
//! runs them all. See docs/crypto.md.
//!
//! | module | what | vectors |
//! |---|---|---|
//! | [`chacha20`] | ChaCha20 block + stream (RFC 8439), HChaCha20 (XChaCha subkeys) | RFC 8439 2.3.2, 2.4.2; xchacha draft 2.2.1 |
//! | [`sha256`] | SHA-256 (FIPS 180-4) | NIST |
//! | [`sha512`] | SHA-512 (FIPS 180-4) | NIST |
//! | [`ed25519`] | Ed25519 signatures (RFC 8032) | RFC 8032 7.1 |
//!
//! (The kernel's audit log keeps its own SHA-256, `kernel/src/audit/sha256.rs`, on
//! purpose: `audit verify` checks the kernel's hashes with this independent copy.)

#![no_std]

extern crate alloc;

pub mod chacha20;
pub mod ed25519;
mod field25519;
pub mod sha256;
pub mod sha512;

/// Hex to a fixed-size array (None if malformed or the wrong length).
pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Hex to bytes (test vectors; malformed pairs become 0).
pub fn unhex_vec(s: &str) -> alloc::vec::Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0))
        .collect()
}

/// Lowercase hex.
pub fn hex(b: &[u8]) -> alloc::string::String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = alloc::string::String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

/// Constant-time equality of two byte strings (length is public).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    d == 0
}

/// Overwrite secrets before they go out of scope (volatile, so it isn't optimised away).
pub fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        // SAFETY: a plain write through a valid &mut.
        unsafe { core::ptr::write_volatile(x, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Every module's vectors: (name, passed, total).
pub fn selftest() -> alloc::vec::Vec<(&'static str, usize, usize)> {
    let mut v = alloc::vec::Vec::new();
    let (p, t) = chacha20::selftest();
    v.push(("chacha20", p, t));
    let (p, t) = sha256::selftest();
    v.push(("sha256", p, t));
    let (p, t) = sha512::selftest();
    v.push(("sha512", p, t));
    let (p, t) = ed25519::selftest();
    v.push(("ed25519", p, t));
    v
}
