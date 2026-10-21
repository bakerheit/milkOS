//! ChaCha20-Poly1305 (RFC 8439) and XChaCha20-Poly1305 (draft-irtf-cfrg-xchacha-03).
//! `open` checks the tag (in constant time) before decrypting anything.

use super::{chacha, ct, poly1305::Poly1305};

fn tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&chacha::block(key, 0, nonce)[..32]);
    let mut p = Poly1305::new(&otk);
    ct::zeroize(&mut otk);
    let pad = [0u8; 16];
    p.update(aad);
    p.update(&pad[..(16 - aad.len() % 16) % 16]);
    p.update(ct);
    p.update(&pad[..(16 - ct.len() % 16) % 16]);
    p.update(&(aad.len() as u64).to_le_bytes());
    p.update(&(ct.len() as u64).to_le_bytes());
    p.finish()
}

/// Encrypt `data` in place; returns the tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    chacha::xor(key, 1, nonce, data);
    tag(key, nonce, aad, data)
}

/// Check `t` and decrypt `data` in place. False (and `data` untouched) if it's wrong.
pub fn open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8], t: &[u8; 16]) -> bool {
    if !ct::eq(&tag(key, nonce, aad, data), t) {
        return false;
    }
    chacha::xor(key, 1, nonce, data);
    true
}

/// XChaCha20-Poly1305: 24-byte nonce (safe to pick at random).
pub fn xseal(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    let (mut sub, n) = chacha::xkey(key, nonce);
    let t = seal(&sub, &n, aad, data);
    ct::zeroize(&mut sub);
    t
}

pub fn xopen(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], data: &mut [u8], t: &[u8; 16]) -> bool {
    let (mut sub, n) = chacha::xkey(key, nonce);
    let ok = open(&sub, &n, aad, data, t);
    ct::zeroize(&mut sub);
    ok
}
