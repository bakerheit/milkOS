//! XChaCha20 and XChaCha20-Poly1305 (draft-irtf-cfrg-xchacha-03): a 192-bit nonce (safe
//! to pick at random) on top of [`crate::chacha20::hchacha20`] + ChaCha20 / the RFC 8439
//! AEAD. [`selftest`]: the draft's A.3.1 AEAD vector, plus a wrong tag.

use crate::{aead, chacha20};

/// The (subkey, 12-byte nonce) XChaCha20 runs ChaCha20 with.
pub fn xkey(key: &[u8; 32], nonce: &[u8; 24]) -> ([u8; 32], [u8; 12]) {
    let sub = chacha20::hchacha20(key, nonce[..16].try_into().unwrap());
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&nonce[16..]);
    (sub, n)
}

/// XOR `data` with the XChaCha20 keystream from block `counter`.
pub fn xor(key: &[u8; 32], nonce: &[u8; 24], counter: u32, data: &mut [u8]) {
    let (mut sub, n) = xkey(key, nonce);
    chacha20::xor(&sub, counter, &n, data);
    crate::wipe(&mut sub);
}

/// XChaCha20-Poly1305: encrypt `data` in place; returns the tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    let (mut sub, n) = xkey(key, nonce);
    let t = aead::seal(&sub, &n, aad, data);
    crate::wipe(&mut sub);
    t
}

/// Check the tag (constant time), then decrypt in place. False: altered (data untouched).
pub fn open(key: &[u8; 32], nonce: &[u8; 24], aad: &[u8], data: &mut [u8], tag: &[u8; 16]) -> bool {
    let (mut sub, n) = xkey(key, nonce);
    let ok = aead::open(&sub, &n, aad, data, tag);
    crate::wipe(&mut sub);
    ok
}

pub fn selftest() -> (usize, usize) {
    let mut ok = 0;
    let key: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
    let nonce: [u8; 24] = crate::unhex("404142434445464748494a4b4c4d4e4f5051525354555657").unwrap();
    let aad = crate::unhex_vec("50515253c0c1c2c3c4c5c6c7");
    let pt = *b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let mut m = pt;
    let t = seal(&key, &nonce, &aad, &mut m);
    let want = crate::unhex_vec(
        "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b4522f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff921f9664c97637da9768812f615c68b13b52e",
    );
    ok += (m[..] == want[..]) as usize;
    ok += (t[..] == crate::unhex_vec("c0875924c1c7987947deafd8780acf49")[..]) as usize;
    let mut c = m;
    ok += (open(&key, &nonce, &aad, &mut c, &t) && c == pt) as usize;
    let mut bad = t;
    bad[0] ^= 1;
    let mut c2 = m;
    ok += (!open(&key, &nonce, &aad, &mut c2, &bad) && c2 == m) as usize;
    (ok, 4)
}
