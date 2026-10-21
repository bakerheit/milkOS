//! ChaCha20 (RFC 8439), HChaCha20 and XChaCha20 (draft-irtf-cfrg-xchacha-03). No table
//! lookups and no secret-dependent branches: constant time by construction.

const SIGMA: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

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

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

fn state(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u32; 16] {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&SIGMA);
    for i in 0..8 {
        s[4 + i] = le32(&key[4 * i..]);
    }
    s[12] = counter;
    for i in 0..3 {
        s[13 + i] = le32(&nonce[4 * i..]);
    }
    s
}

/// One 64-byte keystream block.
pub fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let init = state(key, counter, nonce);
    let mut s = init;
    rounds(&mut s);
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// XOR `data` with the ChaCha20 keystream starting at block `counter`.
pub fn xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let ks = block(key, counter.wrapping_add(i as u32), nonce);
        for (d, k) in chunk.iter_mut().zip(ks.iter()) {
            *d ^= k;
        }
    }
}

/// HChaCha20: a subkey from a key and a 16-byte nonce.
pub fn hchacha20(key: &[u8; 32], nonce: &[u8; 16]) -> [u8; 32] {
    let mut s = [0u32; 16];
    s[..4].copy_from_slice(&SIGMA);
    for i in 0..8 {
        s[4 + i] = le32(&key[4 * i..]);
    }
    for i in 0..4 {
        s[12 + i] = le32(&nonce[4 * i..]);
    }
    rounds(&mut s);
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[4 * i..4 * i + 4].copy_from_slice(&s[i].to_le_bytes());
        out[16 + 4 * i..16 + 4 * i + 4].copy_from_slice(&s[12 + i].to_le_bytes());
    }
    out
}

/// XChaCha20's (subkey, 12-byte nonce) for a 24-byte nonce.
pub fn xkey(key: &[u8; 32], nonce: &[u8; 24]) -> ([u8; 32], [u8; 12]) {
    let sub = hchacha20(key, nonce[..16].try_into().unwrap());
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&nonce[16..]);
    (sub, n)
}

/// XOR `data` with the XChaCha20 keystream (24-byte nonce) from block `counter`.
pub fn xxor(key: &[u8; 32], nonce: &[u8; 24], counter: u32, data: &mut [u8]) {
    let (sub, n) = xkey(key, nonce);
    xor(&sub, counter, &n, data);
    super::ct::zeroize(&mut { sub });
}
