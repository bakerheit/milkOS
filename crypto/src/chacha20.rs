//! ChaCha20 (RFC 8439): the block function, the stream cipher, and HChaCha20 (the subkey
//! step of XChaCha20, draft-irtf-cfrg-xchacha) for whoever builds XChaCha on top.
//! Constant time (adds, xors and rotations only). [`selftest`] runs the RFC 8439 2.3.2 and
//! 2.4.2 vectors and the HChaCha20 vector.

const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574]; // "expand 32-byte k"

#[inline(always)]
fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

fn rounds(s: &mut [u32; 16]) {
    for _ in 0..10 {
        qr(s, 0, 4, 8, 12);
        qr(s, 1, 5, 9, 13);
        qr(s, 2, 6, 10, 14);
        qr(s, 3, 7, 11, 15);
        qr(s, 0, 5, 10, 15);
        qr(s, 1, 6, 11, 12);
        qr(s, 2, 7, 8, 13);
        qr(s, 3, 4, 9, 14);
    }
}

fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]])
}

fn init(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u32; 16] {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&SIGMA);
    for i in 0..8 {
        s[4 + i] = le32(key, i);
    }
    s[12] = counter;
    for i in 0..3 {
        s[13 + i] = le32(nonce, i);
    }
    s
}

/// One 64-byte keystream block (RFC 8439 2.3).
pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let s0 = init(key, counter, nonce);
    let mut s = s0;
    rounds(&mut s);
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(s0[i]).to_le_bytes());
    }
    out
}

/// Encrypt or decrypt `data` in place: xor with the keystream starting at block `counter`
/// (RFC 8439 2.4). The caller keeps (key, nonce, counter) unique per message.
pub fn xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let ks = block(key, counter.wrapping_add(i as u32), nonce);
        for (b, k) in chunk.iter_mut().zip(ks.iter()) {
            *b ^= k;
        }
    }
}

/// HChaCha20: a 32-byte subkey from a key and a 16-byte nonce (XChaCha20 uses the first
/// 16 bytes of its 24-byte nonce here, then ChaCha20 with the subkey and the last 8).
pub fn hchacha20(key: &[u8; 32], nonce: &[u8; 16]) -> [u8; 32] {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&SIGMA);
    for i in 0..8 {
        s[4 + i] = le32(key, i);
    }
    for i in 0..4 {
        s[12 + i] = le32(nonce, i);
    }
    rounds(&mut s);
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].to_le_bytes());
        out[16 + 4 * i..20 + 4 * i].copy_from_slice(&s[12 + i].to_le_bytes());
    }
    out
}

/// (passed, total).
pub fn selftest() -> (usize, usize) {
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    let mut ok = 0;
    // RFC 8439 2.3.2: the block function.
    let b = block(&key, 1, &crate::unhex("000000090000004a00000000").unwrap());
    ok += (b[..]
        == crate::unhex_vec(
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4ed2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e",
        )[..]) as usize;
    // RFC 8439 2.4.2: encryption (114 bytes, crosses a block boundary).
    let mut m = *b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let n = crate::unhex("000000000000004a00000000").unwrap();
    xor(&key, 1, &n, &mut m);
    ok += (m[..]
        == crate::unhex_vec(
            "6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0bf91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d807ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab77937365af90bbf74a35be6b40b8eedf2785e42874d",
        )[..]) as usize;
    xor(&key, 1, &n, &mut m);
    ok += m.starts_with(b"Ladies and Gentlemen") as usize;
    // HChaCha20 (draft-irtf-cfrg-xchacha 2.2.1).
    let h = hchacha20(
        &key,
        &crate::unhex("000000090000004a0000000031415927").unwrap(),
    );
    ok += (h[..]
        == crate::unhex_vec("82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc")[..])
        as usize;
    (ok, 4)
}
