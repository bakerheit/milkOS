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
//! | [`poly1305`] | Poly1305 one-time MAC (RFC 8439 2.5) | RFC 8439 2.5.2, A.3 |
//! | [`aead`] | ChaCha20-Poly1305 AEAD (RFC 8439 2.8) | RFC 8439 2.8.2 + tamper checks |
//! | [`x25519`] | X25519 key agreement (RFC 7748) | RFC 7748 5.2, 6.1, iterated x1000 |
//! | [`xchacha20`] | XChaCha20 stream and XChaCha20-Poly1305 (24-byte nonce) | xchacha draft A.3.1 + a wrong tag |
//! | [`hmac`] | HMAC-SHA256 (RFC 2104), PBKDF2-HMAC-SHA256 (RFC 8018) | RFC 4231 1, 2, 6; RFC 7914 11 |
//! | [`scrypt`] | scrypt (RFC 7914), the array from a caller-chosen [`scrypt::Scratch`] | RFC 7914 12 (both) |
//! | [`hash`] | the [`hash::Hash`] trait over SHA-256/384/512 | via hmac, hkdf |
//! | [`hkdf`] | HKDF extract / expand (RFC 5869) over any [`hash::Hash`] | RFC 5869 A.1-A.3, SHA-384 cross-checked |
//! | [`aes`] | AES-128 block cipher (FIPS 197), bitsliced, constant time | FIPS 197 B, C.1 |
//! | [`gcm`] | AES-128-GCM AEAD (SP 800-38D) on a constant-time GHASH | GCM spec test cases |
//! | [`bignum`] | unsigned big integers for public-key verification (no panics on hostile input) | via rsa |
//! | [`rsa`] | RSA PKCS#1 v1.5 and RSA-PSS signature verification (RFC 8017) | see the module |
//! | [`p256`] | NIST P-256: ECDH (constant time) and ECDSA verification | see the module |
//! | [`p384`] | NIST P-384: ECDSA verification | see the module |
//! | [`ecdsa`] | ECDSA signatures in DER (strict parsing) | see the module |
//!
//! (The kernel's audit log keeps its own SHA-256, `kernel/src/audit/sha256.rs`, on
//! purpose: `audit verify` checks the kernel's hashes with this independent copy.)

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod aead;
pub mod aes;
pub mod bignum;
pub mod chacha20;
pub mod ecdsa;
pub mod ed25519;
mod field25519;
pub mod gcm;
pub mod ghash;
pub mod hash;
pub mod hkdf;
pub mod hmac;
mod nist;
pub mod p256;
pub mod p384;
pub mod rsa;
pub mod poly1305;
pub mod scrypt;
pub mod sha256;
pub mod sha512;
pub mod x25519;
pub mod xchacha20;

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
    let (p, t) = poly1305::selftest();
    v.push(("poly1305", p, t));
    let (p, t) = aead::selftest();
    v.push(("chacha20poly1305", p, t));
    let (p, t) = x25519::selftest();
    v.push(("x25519", p, t));
    let (p, t) = xchacha20::selftest();
    v.push(("xchacha20poly1305", p, t));
    let (p, t) = hmac::selftest();
    v.push(("hmac-sha256+pbkdf2", p, t));
    let (p, t) = scrypt::selftest();
    v.push(("scrypt", p, t));
    let (p, t) = hkdf::selftest();
    v.push(("hkdf", p, t));
    let (p, t) = aes::selftest();
    v.push(("aes128", p, t));
    let (p, t) = gcm::selftest();
    v.push(("aes128gcm", p, t));
    let (p, t) = rsa::selftest();
    v.push(("rsa", p, t));
    let (p, t) = p256::selftest();
    v.push(("p256", p, t));
    let (p, t) = p384::selftest();
    v.push(("p384", p, t));
    v
}

#[cfg(test)]
mod tests {
    /// Every module's boot self-test passes in full.
    #[test]
    fn selftests_pass() {
        for (name, p, t) in super::selftest() {
            assert!(t > 0, "{name}: no vectors");
            assert_eq!(p, t, "{name}: {p}/{t}");
        }
    }
}
