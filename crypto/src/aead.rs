//! ChaCha20-Poly1305 AEAD (RFC 8439 2.8): 256-bit key, 96-bit nonce, 128-bit tag. A
//! (key, nonce) pair must never repeat. [`selftest`]: RFC 8439 2.8.2, plus tamper checks.

use crate::chacha20;
use crate::poly1305::Poly1305;

pub const TAG: usize = 16;

fn mac(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let mut otk = [0u8; 32];
    let mut b0 = chacha20::block(key, 0, nonce);
    otk.copy_from_slice(&b0[..32]);
    let mut p = Poly1305::new(&otk);
    let zeros = [0u8; 16];
    p.update(aad).update(&zeros[..(16 - aad.len() % 16) % 16]);
    p.update(ct).update(&zeros[..(16 - ct.len() % 16) % 16]);
    p.update(&(aad.len() as u64).to_le_bytes())
        .update(&(ct.len() as u64).to_le_bytes());
    let t = p.finish();
    crate::wipe(&mut otk);
    crate::wipe(&mut b0);
    t
}

/// Encrypt `data` in place; returns the tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    chacha20::xor(key, 1, nonce, data);
    mac(key, nonce, aad, data)
}

/// Check the tag (constant time), then decrypt `data` in place. False (and `data` left
/// as ciphertext) if anything was altered.
pub fn open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &[u8; 16]) -> bool {
    if !crate::ct_eq(&mac(key, nonce, aad, data), tag) {
        return false;
    }
    chacha20::xor(key, 1, nonce, data);
    true
}

pub fn selftest() -> (usize, usize) {
    let mut ok = 0;
    let key: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
    let nonce = crate::unhex("070000004041424344454647").unwrap();
    let aad = crate::unhex_vec("50515253c0c1c2c3c4c5c6c7");
    let pt = *b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let mut m = pt;
    let t = seal(&key, &nonce, &aad, &mut m);
    let want = crate::unhex_vec(
        "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116",
    );
    ok += (m[..] == want[..]) as usize;
    ok += (t[..] == crate::unhex_vec("1ae10b594f09e26a7e902ecbd0600691")[..]) as usize;
    let mut c = m;
    ok += (open(&key, &nonce, &aad, &mut c, &t) && c == pt) as usize;
    let mut bad = m;
    bad[7] ^= 1;
    ok += !open(&key, &nonce, &aad, &mut bad, &t) as usize;
    let mut c2 = m;
    ok += !open(&key, &nonce, b"other aad", &mut c2, &t) as usize;
    (ok, 5)
}
