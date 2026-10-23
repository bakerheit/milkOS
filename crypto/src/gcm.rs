//! AES-128-GCM AEAD (NIST SP 800-38D) with a 96-bit nonce and a 16-byte tag, as TLS 1.3's
//! TLS_AES_128_GCM_SHA256 uses it: CTR mode on the bitsliced [`Aes128`] (four counter
//! blocks per pass) and the constant-time [`Ghash`]. A (key, nonce) pair must never repeat.
//! One message may be at most 2^36 - 32 bytes (the 32-bit block counter); [`Aes128Gcm::open`]
//! refuses longer ones, and sealing that much is a caller bug (TLS records are 16 KiB).
//!
//! [`selftest`]: test cases 1 to 4 of the GCM specification (McGrew and Viega, AES-128),
//! plus a flipped tag, ciphertext and AAD that must be refused.

use crate::aes::{Aes128, wipe_u32};
use crate::ghash::Ghash;

pub const TAG: usize = 16;

/// The longest message SP 800-38D allows: 2^32 - 2 counter blocks.
const MAX_LEN: u64 = (1 << 36) - 32;

/// An AES-128-GCM key: the AES key schedule and the hash key H = E(K, 0). Wiped on drop
/// (both parts wipe themselves).
pub struct Aes128Gcm {
    aes: Aes128,
    h: [u8; 16],
}

impl Drop for Aes128Gcm {
    fn drop(&mut self) {
        crate::wipe(&mut self.h);
    }
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl Aes128Gcm {
    pub fn new(key: &[u8; 16]) -> Aes128Gcm {
        let aes = Aes128::new(key);
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);
        Aes128Gcm { aes, h }
    }

    /// XOR `data` with the keystream of counter blocks nonce || ctr, ctr = `start`, ...
    fn ctr(&self, nonce: &[u8; 12], start: u32, data: &mut [u8]) {
        let n = [le32(&nonce[0..4]), le32(&nonce[4..8]), le32(&nonce[8..12])];
        let mut ctr = start;
        let mut w = [0u32; 16];
        for chunk in data.chunks_mut(64) {
            for i in 0..4 {
                w[4 * i..4 * i + 3].copy_from_slice(&n);
                // The counter is big-endian in bytes 12..16; words are little-endian.
                w[4 * i + 3] = ctr.wrapping_add(i as u32).swap_bytes();
            }
            self.aes.encrypt_words(&mut w);
            for (i, d) in chunk.iter_mut().enumerate() {
                *d ^= (w[i / 4] >> (8 * (i % 4))) as u8;
            }
            ctr = ctr.wrapping_add(4);
        }
        wipe_u32(&mut w);
    }

    /// GHASH(AAD, C) xor E(K, J0), J0 = nonce || 1.
    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let mut g = Ghash::new(&self.h);
        g.update_padded(aad);
        g.update_padded(ct);
        let mut t = g.finish(aad.len() as u64, ct.len() as u64);
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        self.aes.encrypt_block(&mut j0);
        for (a, b) in t.iter_mut().zip(j0.iter()) {
            *a ^= b;
        }
        crate::wipe(&mut j0);
        t
    }

    /// Encrypt `data` in place; returns the tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
        self.ctr(nonce, 2, data);
        self.tag(nonce, aad, data)
    }

    /// Check the tag (constant time), then decrypt `data` in place. False (and `data` left
    /// as ciphertext) if anything was altered, or if `data` is longer than GCM allows.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &[u8; 16]) -> bool {
        if data.len() as u64 > MAX_LEN || aad.len() as u64 > u64::MAX / 8 {
            return false;
        }
        let mut want = self.tag(nonce, aad, data);
        let good = crate::ct_eq(&want, tag);
        crate::wipe(&mut want);
        if !good {
            return false;
        }
        self.ctr(nonce, 2, data);
        true
    }
}

/// One-shot [`Aes128Gcm::seal`].
pub fn seal(key: &[u8; 16], nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    Aes128Gcm::new(key).seal(nonce, aad, data)
}

/// One-shot [`Aes128Gcm::open`].
pub fn open(key: &[u8; 16], nonce: &[u8; 12], aad: &[u8], data: &mut [u8], tag: &[u8; 16]) -> bool {
    Aes128Gcm::new(key).open(nonce, aad, data, tag)
}

/// GCM specification test cases 1-4 (AES-128), and tampering that must be caught.
pub fn selftest() -> (usize, usize) {
    use crate::unhex_vec as h;
    const P: &str = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255";
    const C: &str = "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091473f5985";
    // key, nonce, plaintext, aad, ciphertext, tag
    let cases: [(&str, &str, &str, &str, &str, &str); 4] = [
        (
            "00000000000000000000000000000000",
            "000000000000000000000000",
            "",
            "",
            "",
            "58e2fccefa7e3061367f1d57a4e7455a",
        ),
        (
            "00000000000000000000000000000000",
            "000000000000000000000000",
            "00000000000000000000000000000000",
            "",
            "0388dace60b6a392f328c2b971b2fe78",
            "ab6e47d42cec13bdf53a67b21257bddf",
        ),
        (
            "feffe9928665731c6d6a8f9467308308",
            "cafebabefacedbaddecaf888",
            P,
            "",
            C,
            "4d5c2af327cd64a62cf35abd2ba6fab4",
        ),
        (
            "feffe9928665731c6d6a8f9467308308",
            "cafebabefacedbaddecaf888",
            &P[..120],
            "feedfacedeadbeeffeedfacedeadbeefabaddad2",
            &C[..120],
            "5bc94fbc3221a5db94fae95ae7121a47",
        ),
    ];
    let mut ok = 0;
    let mut total = 0;
    for (k, n, p, a, c, t) in cases {
        total += 3;
        let (Some(k), Some(n), Some(t)) = (
            crate::unhex::<16>(k),
            crate::unhex::<12>(n),
            crate::unhex::<16>(t),
        ) else {
            continue;
        };
        let (pt, aad) = (h(p), h(a));
        let g = Aes128Gcm::new(&k);
        let mut m = pt.clone();
        let tag = g.seal(&n, &aad, &mut m);
        ok += (crate::hex(&m) == c && tag == t) as usize;
        let mut d = m.clone();
        ok += (g.open(&n, &aad, &mut d, &tag) && d == pt) as usize;
        // A flipped tag bit, ciphertext bit (when there is one) or AAD bit is refused, and
        // the buffer is left as it was.
        let mut bad_tag = tag;
        bad_tag[15] ^= 0x80;
        let mut d1 = m.clone();
        let mut refused = !g.open(&n, &aad, &mut d1, &bad_tag) && d1 == m;
        if !m.is_empty() {
            let mut d2 = m.clone();
            d2[0] ^= 1;
            refused &= !g.open(&n, &aad, &mut d2, &tag);
        }
        let mut aad2 = aad.clone();
        aad2.push(0);
        let mut d3 = m.clone();
        refused &= !g.open(&n, &aad2, &mut d3, &tag);
        ok += refused as usize;
    }
    (ok, total)
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)] // test-only reference code, written like the standards
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    /// GCM from the test-only reference AES and GHASH.
    fn reference(key: &[u8; 16], nonce: &[u8; 12], aad: &[u8], pt: &[u8]) -> (Vec<u8>, [u8; 16]) {
        let e = |b: [u8; 16]| crate::aes::tests::reference(key, &b);
        let h = e([0; 16]);
        let mut ct = pt.to_vec();
        for (i, c) in ct.chunks_mut(16).enumerate() {
            let mut cb = [0u8; 16];
            cb[..12].copy_from_slice(nonce);
            cb[12..].copy_from_slice(&(i as u32 + 2).to_be_bytes());
            for (x, k) in c.iter_mut().zip(e(cb).iter()) {
                *x ^= k;
            }
        }
        let s = crate::ghash::tests::ref_ghash(&h, aad, &ct);
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        let ej = e(j0);
        (ct, core::array::from_fn(|i| s[i] ^ ej[i]))
    }

    #[test]
    fn spec_cases() {
        assert_eq!(selftest(), (12, 12));
    }

    #[test]
    fn go_vectors() {
        let mut n = 0;
        for line in include_str!("../testdata/gcm/aes128gcm.txt").lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let f: Vec<Vec<u8>> = line
                .split_whitespace()
                .map(|x| {
                    if x == "-" {
                        Vec::new()
                    } else {
                        crate::unhex_vec(x)
                    }
                })
                .collect();
            let key: [u8; 16] = f[0][..].try_into().unwrap();
            let nonce: [u8; 12] = f[1][..].try_into().unwrap();
            let (pt, aad, want) = (&f[2], &f[3], &f[4]);
            let mut m = pt.clone();
            let tag = seal(&key, &nonce, aad, &mut m);
            assert_eq!(m[..], want[..pt.len()], "ciphertext, line {line}");
            assert_eq!(tag[..], want[pt.len()..], "tag, line {line}");
            let t: [u8; 16] = want[pt.len()..].try_into().unwrap();
            assert!(open(&key, &nonce, aad, &mut m, &t));
            assert_eq!(&m, pt);
            n += 1;
        }
        assert_eq!(n, 19);
    }

    #[test]
    fn lengths_against_reference() {
        let mut r = Rng(0x0123_4567_89ab_cdef);
        let mut lens: Vec<usize> = (0..=64).collect();
        lens.extend([
            79, 80, 81, 127, 128, 129, 255, 256, 257, 1000, 4096, 16384, 16385,
        ]);
        for &plen in &lens {
            for alen in [0usize, 1, 13, 16, 17, 64] {
                let key: [u8; 16] = r.bytes(16)[..].try_into().unwrap();
                let nonce: [u8; 12] = r.bytes(12)[..].try_into().unwrap();
                let pt = r.bytes(plen);
                let aad = r.bytes(alen);
                let (want_ct, want_tag) = reference(&key, &nonce, &aad, &pt);
                let mut m = pt.clone();
                let tag = seal(&key, &nonce, &aad, &mut m);
                assert_eq!(m, want_ct, "plen {plen} alen {alen}");
                assert_eq!(tag, want_tag, "plen {plen} alen {alen}");
            }
        }
    }

    #[test]
    fn every_bit_flip_refused() {
        let mut r = Rng(42);
        let key: [u8; 16] = r.bytes(16)[..].try_into().unwrap();
        let nonce: [u8; 12] = r.bytes(12)[..].try_into().unwrap();
        let g = Aes128Gcm::new(&key);
        let pt = r.bytes(37);
        let aad = r.bytes(21);
        let mut ct = pt.clone();
        let tag = g.seal(&nonce, &aad, &mut ct);
        for bit in 0..128 {
            let mut t = tag;
            t[bit / 8] ^= 1 << (bit % 8);
            let mut d = ct.clone();
            assert!(!g.open(&nonce, &aad, &mut d, &t));
            assert_eq!(d, ct, "a failed open must leave the ciphertext alone");
        }
        for bit in 0..ct.len() * 8 {
            let mut d = ct.clone();
            d[bit / 8] ^= 1 << (bit % 8);
            assert!(!g.open(&nonce, &aad, &mut d, &tag));
        }
        for bit in 0..aad.len() * 8 {
            let mut a = aad.clone();
            a[bit / 8] ^= 1 << (bit % 8);
            let mut d = ct.clone();
            assert!(!g.open(&nonce, &a, &mut d, &tag));
        }
        for bit in 0..96 {
            let mut n = nonce;
            n[bit / 8] ^= 1 << (bit % 8);
            let mut d = ct.clone();
            assert!(!g.open(&n, &aad, &mut d, &tag));
        }
        // Moving bytes between the AAD and the ciphertext is caught by the length block.
        let mut joined = aad.clone();
        joined.extend_from_slice(&ct);
        let mut d = joined[aad.len() - 1..].to_vec();
        assert!(!g.open(&nonce, &aad[..aad.len() - 1], &mut d, &tag));
        // Truncated ciphertext.
        let mut d = ct[..ct.len() - 1].to_vec();
        assert!(!g.open(&nonce, &aad, &mut d, &tag));
        let mut d = ct.clone();
        assert!(g.open(&nonce, &aad, &mut d, &tag) && d == pt);
    }

    #[test]
    fn empty_everything() {
        let g = Aes128Gcm::new(&[0; 16]);
        let mut m: [u8; 0] = [];
        let tag = g.seal(&[0; 12], &[], &mut m);
        assert_eq!(crate::hex(&tag), "58e2fccefa7e3061367f1d57a4e7455a");
        assert!(g.open(&[0; 12], &[], &mut m, &tag));
        assert!(!g.open(&[0; 12], &[0], &mut m, &tag));
    }

    #[test]
    fn fuzz_round_trip() {
        let mut r = Rng(0xfeed_beef);
        for _ in 0..300 {
            let key: [u8; 16] = r.bytes(16)[..].try_into().unwrap();
            let nonce: [u8; 12] = r.bytes(12)[..].try_into().unwrap();
            let plen = (r.next() % 3000) as usize;
            let alen = (r.next() % 100) as usize;
            let pt = r.bytes(plen);
            let aad = r.bytes(alen);
            let g = Aes128Gcm::new(&key);
            let mut m = pt.clone();
            let tag = g.seal(&nonce, &aad, &mut m);
            if plen >= 16 {
                assert_ne!(m, pt);
            }
            assert!(g.open(&nonce, &aad, &mut m, &tag));
            assert_eq!(m, pt);
        }
    }

    #[test]
    fn counter_crosses_byte_boundaries() {
        // 300 blocks: the counter's low byte wraps (2 + 254 = 256), checked against the
        // reference, so the big-endian counter is right past one byte.
        let mut r = Rng(7);
        let key: [u8; 16] = r.bytes(16)[..].try_into().unwrap();
        let nonce: [u8; 12] = r.bytes(12)[..].try_into().unwrap();
        let pt = r.bytes(300 * 16 + 5);
        let (want, wt) = reference(&key, &nonce, b"", &pt);
        let mut m = pt.clone();
        assert_eq!(seal(&key, &nonce, b"", &mut m), wt);
        assert_eq!(m, want);
    }

    #[test]
    #[ignore]
    fn speed() {
        // cargo test --release --lib gcm::tests::speed -- --ignored --nocapture
        let g = Aes128Gcm::new(&[7; 16]);
        let mut buf = alloc::vec![0u8; 16384];
        let n = 2000;
        let t = std::time::Instant::now();
        for i in 0..n {
            let mut nonce = [0u8; 12];
            nonce[..4].copy_from_slice(&(i as u32).to_be_bytes());
            core::hint::black_box(g.seal(&nonce, &[0; 13], &mut buf));
        }
        let s = t.elapsed().as_secs_f64();
        std::println!(
            "AES-128-GCM seal, 16 KiB records: {:.1} MB/s",
            (n * 16384) as f64 / s / 1e6
        );
    }
}
