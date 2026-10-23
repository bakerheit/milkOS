//! RSA signature **verification** (RFC 8017): RSASSA-PKCS1-v1_5 ([`PublicKey::verify_pkcs1v15`])
//! and RSASSA-PSS with MGF1 ([`PublicKey::verify_pss`]), over SHA-256, SHA-384 and SHA-512.
//! There is no signing and no decryption here, so nothing is secret and the arithmetic
//! ([`crate::bignum`]) may take variable time. Hostile keys and signatures never panic;
//! they fail to verify.
//!
//! PKCS#1 v1.5 is checked the safe way: the whole encoded message is rebuilt from the
//! digest (with the DigestInfo's NULL parameter, as RFC 8017 9.2 writes it) and compared to
//! the signature's, so no parser of the padding can be fooled.
//!
//! [`selftest`]: one PKCS#1 v1.5 and one PSS signature (2048-bit, SHA-256) made with
//! OpenSSL, and each with one bit changed. The host tests have 2048/3072/4096-bit keys,
//! all three hashes, both schemes, edge cases and a mutation fuzz.

use alloc::vec::Vec;

use crate::bignum::{BigUint, Mont};
use crate::sha256::Sha256;
use crate::sha512::{Sha384, Sha512};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    /// Digest length in bytes.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(self) -> usize {
        match self {
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 => 64,
        }
    }

    /// The DER DigestInfo prefix (RFC 8017 9.2 note 1), AlgorithmIdentifier with NULL.
    fn digest_info(self) -> &'static [u8] {
        match self {
            Hash::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            Hash::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            Hash::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
        }
    }

    /// Hash the concatenation of `parts`.
    fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<H: crate::hash::Hash>(parts: &[&[u8]]) -> Vec<u8> {
            let mut h = H::new();
            for p in parts {
                h.input(p);
            }
            let mut out = alloc::vec![0u8; H::OUT];
            h.result(&mut out);
            out
        }
        match self {
            Hash::Sha256 => run::<Sha256>(parts),
            Hash::Sha384 => run::<Sha384>(parts),
            Hash::Sha512 => run::<Sha512>(parts),
        }
    }
}

/// An RSA public key (n, e).
pub struct PublicKey {
    n: BigUint,
    e: BigUint,
    mont: Mont,
}

impl PublicKey {
    /// n and e big-endian, leading zero bytes allowed. None unless n is odd,
    /// 512 <= bits(n) <= 8192, e is odd and 3 <= e < n. (Policy such as "2048 to 4096 bits
    /// only" is the caller's.)
    pub fn new(n: &[u8], e: &[u8]) -> Option<PublicKey> {
        let n = BigUint::from_be_bytes(n);
        let e = BigUint::from_be_bytes(e);
        let bits = n.bits();
        if !(512..=8192).contains(&bits) || !n.is_odd() {
            return None;
        }
        if !e.is_odd() || e < BigUint::from_u64(3) || e >= n {
            return None;
        }
        let mont = Mont::new(&n)?;
        Some(PublicKey { n, e, mont })
    }

    /// The modulus length in bits.
    pub fn bits(&self) -> usize {
        self.n.bits()
    }

    /// k, the modulus length in bytes.
    fn k(&self) -> usize {
        self.n.byte_len()
    }

    /// RSAVP1: sig^e mod n as `len` bytes; None unless sig is exactly k bytes and < n, or
    /// if the result doesn't fit `len` bytes.
    fn open(&self, sig: &[u8], len: usize) -> Option<Vec<u8>> {
        if sig.len() != self.k() {
            return None;
        }
        let s = BigUint::from_be_bytes(sig);
        if s >= self.n {
            return None;
        }
        self.mont.pow(&s, &self.e)?.to_be_bytes(len)
    }

    /// RSASSA-PKCS1-v1_5 (RFC 8017 8.2.2): `sig` must be exactly k bytes and < n; the
    /// encoded message 00 01 FF.. 00 DigestInfo digest is rebuilt and compared in full.
    /// `digest` is the message's hash under `hash`.
    pub fn verify_pkcs1v15(&self, hash: Hash, digest: &[u8], sig: &[u8]) -> bool {
        let k = self.k();
        let info = hash.digest_info();
        if digest.len() != hash.len() || k < info.len() + digest.len() + 11 {
            return false;
        }
        let Some(em) = self.open(sig, k) else {
            return false;
        };
        let mut want = Vec::with_capacity(k);
        want.push(0x00);
        want.push(0x01);
        want.resize(k - info.len() - digest.len() - 1, 0xff);
        want.push(0x00);
        want.extend_from_slice(info);
        want.extend_from_slice(digest);
        crate::ct_eq(&em, &want)
    }

    /// RSASSA-PSS (RFC 8017 8.1.2 and EMSA-PSS-VERIFY, 9.1.2), MGF1 with the same hash,
    /// with exactly `salt_len` bytes of salt. `digest` is the message's hash.
    pub fn verify_pss(&self, hash: Hash, digest: &[u8], sig: &[u8], salt_len: usize) -> bool {
        let h_len = hash.len();
        if digest.len() != h_len {
            return false;
        }
        let mod_bits = self.bits();
        let em_bits = mod_bits - 1; // bits() >= 512 here
        let em_len = em_bits.div_ceil(8);
        // 9.1.2 step 3.
        let Some(need) = h_len.checked_add(salt_len).and_then(|x| x.checked_add(2)) else {
            return false;
        };
        if em_len < need {
            return false;
        }
        // 8.1.2 step 2: EM = I2OSP(m, emLen); m must fit in emLen bytes.
        let Some(em) = self.open(sig, em_len) else {
            return false;
        };
        // Step 4: the trailer.
        if em[em_len - 1] != 0xbc {
            return false;
        }
        let db_len = em_len - h_len - 1;
        let (masked_db, rest) = em.split_at(db_len);
        let h = &rest[..h_len];
        // Step 6: the bits above emBits must be zero.
        let top_bits = 8 * em_len - em_bits; // 0..=7
        let top_mask: u8 = if top_bits == 0 {
            0
        } else {
            0xffu8 << (8 - top_bits)
        };
        if masked_db[0] & top_mask != 0 {
            return false;
        }
        // Steps 7-9: DB = maskedDB xor MGF1(H).
        let mut db = mgf1(hash, h, db_len);
        for (d, m) in db.iter_mut().zip(masked_db) {
            *d ^= m;
        }
        db[0] &= !top_mask;
        // Step 10: PS zeros, then 0x01.
        let ps_len = em_len - h_len - salt_len - 2;
        if db[..ps_len].iter().any(|&b| b != 0) || db[ps_len] != 0x01 {
            return false;
        }
        let salt = &db[db_len - salt_len..];
        // Steps 12-14: H' = Hash(0^8 || mHash || salt).
        let h2 = hash.digest(&[&[0u8; 8], digest, salt]);
        crate::ct_eq(h, &h2)
    }
}

/// MGF1 (RFC 8017 B.2.1): `len` bytes from `seed`.
fn mgf1(hash: Hash, seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + hash.len());
    let mut counter: u32 = 0;
    while out.len() < len {
        out.extend_from_slice(&hash.digest(&[seed, &counter.to_be_bytes()]));
        counter = counter.wrapping_add(1);
    }
    out.truncate(len);
    out
}

pub fn selftest() -> (usize, usize) {
    use crate::unhex_vec as h;
    let n = h(SELFTEST_N);
    let Some(key) = PublicKey::new(&n, &[1, 0, 1]) else {
        return (0, 4);
    };
    let dg = crate::sha256::sha256(b"milkOS RSA self-test");
    let mut ok = 0;
    let mut s1 = h(SELFTEST_PKCS1);
    let mut s2 = h(SELFTEST_PSS);
    ok += key.verify_pkcs1v15(Hash::Sha256, &dg, &s1) as usize;
    ok += key.verify_pss(Hash::Sha256, &dg, &s2, 32) as usize;
    s1[100] ^= 4;
    s2[100] ^= 4;
    ok += !key.verify_pkcs1v15(Hash::Sha256, &dg, &s1) as usize;
    ok += !key.verify_pss(Hash::Sha256, &dg, &s2, 32) as usize;
    (ok, 4)
}

const SELFTEST_N: &str = "94f68dbeb4bc53cfa0ee4841a1f2d64ae07d83527aad1cb1dedff5e1c44871e0c04ba626826f1769658b6ea29e5f3974dc50d11c77c2862972f55de6b9a118554d99d87b68ddfa432aadf1f652ed452efa612d819c13adbc9d4236440cc28eaa88f18e5b5a2c206c385ccec652a6254b373d164b0729614c777de01166cdfdfd8c498a905a16afef47db8001e86c8d7f6b9442519dd8819a78fcb3a517a81a1f2814843d656511f705dca11b2393c6d610d25abbf05314b949b1a15b5dbe4ec400a62a93fbd63eeebece89b4ecff0ca00edd9932b32fd9a646953d0f31d67e0136cd78c056f019b0c73d34a1d593880fb0938b81f39dc84b492db29fe64ad003";
const SELFTEST_PKCS1: &str = "0fecbde32ff9665d80bbb0d634cb3116a4d8a8ea1881c927f8fa26544fb68f525c8a743b628fde5e3cb7fef80f03f7ce5155af857290f809c61871dab034ff9536a687ab33a75776620a7a28091a4733061440da846f20821d04a6e1863fb751e25f018e8129b077329f3a179ecc373a2f4719e22de355cc1e8434d1dd9c6163e8e62918d7f8e4bbad9bb6b713fe288fb5adce194320938d3b1fe3608b1a6a849a539228f006cce525f14b454cb1660ce9abbdca3a75f6070c615a259e7f92c44e4d54012f68a59b168f509ca6696c63a51cc5eb97f359c2c450e48ef874021524f9033ec40ffb02bfb32610d655e16b53284231d723796544e59d629f892e8b";
const SELFTEST_PSS: &str = "1813b1c74e97b6d4fb42ff2bc946caf08b44d204548c03460402453aa09a0a6899c21560bc2bfd945f911baf49ffb8c7d4aab8da2eb3b78df4e935ce432946c7dbf4b5061412d9ff6cd97a9e225fbaf641a92c064c8a347adec1ea69ffcb1d74e4c5068deebb56bd1bce01ebf12c3a36d3e2c42f0891b55db334f0dd797d3fad8d6c8e1782d9b0de783357b3025fad488e90137b1cd0ba9258498251e4c6d59d50b74eab764f7726671cfac0a11c4a4ca2b804bb22fe45ff01c369e6fc3796f93ed2fd47dedd5a234bab441a6140d8f2f397f7a00dfbef6f8057f20e825648acec49137a597e794326087d31160772cb6c732547b4c5f4e245d8b1e95223ab25";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unhex_vec as h;
    use alloc::vec;

    /// (bits, n, e, [(scheme, hash, msg, sig)]) from testdata/rsa/vectors.txt.
    #[allow(clippy::type_complexity)]
    fn vectors() -> Vec<(
        usize,
        Vec<u8>,
        Vec<u8>,
        Vec<(String, Hash, Vec<u8>, Vec<u8>)>,
    )> {
        let mut out: Vec<(
            usize,
            Vec<u8>,
            Vec<u8>,
            Vec<(String, Hash, Vec<u8>, Vec<u8>)>,
        )> = Vec::new();
        for line in include_str!("../testdata/rsa/vectors.txt").lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            match f.first() {
                Some(&"key") => out.push((f[1].parse().unwrap(), h(f[2]), h(f[3]), Vec::new())),
                Some(&"pkcs1") | Some(&"pss") => {
                    let hash = match f[1] {
                        "sha256" => Hash::Sha256,
                        "sha384" => Hash::Sha384,
                        _ => Hash::Sha512,
                    };
                    let msg = if f[2] == "-" { Vec::new() } else { h(f[2]) };
                    out.last_mut()
                        .unwrap()
                        .3
                        .push((f[0].into(), hash, msg, h(f[3])));
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn openssl_vectors() {
        let mut n_ok = 0;
        for (bits, n, e, sigs) in vectors() {
            let key = PublicKey::new(&n, &e).unwrap();
            assert_eq!(key.bits(), bits);
            for (scheme, hash, msg, sig) in &sigs {
                let dg = hash.digest(&[msg]);
                let other = if *hash == Hash::Sha256 {
                    Hash::Sha384
                } else {
                    Hash::Sha256
                };
                let dg_other = other.digest(&[msg]);
                if scheme == "pkcs1" {
                    assert!(
                        key.verify_pkcs1v15(*hash, &dg, sig),
                        "{bits} pkcs1 {hash:?}"
                    );
                    assert!(!key.verify_pss(*hash, &dg, sig, hash.len()));
                    assert!(!key.verify_pkcs1v15(other, &dg_other, sig));
                } else {
                    assert!(
                        key.verify_pss(*hash, &dg, sig, hash.len()),
                        "{bits} pss {hash:?}"
                    );
                    assert!(!key.verify_pss(*hash, &dg, sig, hash.len() - 1));
                    assert!(!key.verify_pss(*hash, &dg, sig, 0));
                    assert!(!key.verify_pkcs1v15(*hash, &dg, sig));
                    assert!(!key.verify_pss(other, &dg_other, sig, other.len()));
                }
                // Another message, a flipped bit anywhere, a wrong length.
                let mut bad = dg.clone();
                bad[0] ^= 1;
                assert!(!key.verify_pkcs1v15(*hash, &bad, sig));
                assert!(!key.verify_pss(*hash, &bad, sig, hash.len()));
                for i in [0, 1, sig.len() / 2, sig.len() - 1] {
                    let mut s = sig.clone();
                    s[i] ^= 0x10;
                    assert!(!key.verify_pkcs1v15(*hash, &dg, &s));
                    assert!(!key.verify_pss(*hash, &dg, &s, hash.len()));
                }
                let mut long = vec![0u8];
                long.extend_from_slice(sig);
                assert!(!key.verify_pkcs1v15(*hash, &dg, &long));
                assert!(!key.verify_pss(*hash, &dg, &long, hash.len()));
                assert!(!key.verify_pkcs1v15(*hash, &dg, &sig[1..]));
                assert!(!key.verify_pss(*hash, &dg, &sig[1..], hash.len()));
                assert!(!key.verify_pkcs1v15(*hash, &dg[1..], sig));
                n_ok += 1;
            }
        }
        assert_eq!(n_ok, 26);
    }

    #[test]
    fn key_checks() {
        let v = vectors();
        let n = &v[0].1;
        assert!(PublicKey::new(n, &[1, 0, 1]).is_some());
        assert!(PublicKey::new(n, &[0, 0, 1, 0, 1]).is_some()); // leading zeros fine
        assert!(PublicKey::new(n, &[1]).is_none()); // e = 1
        assert!(PublicKey::new(n, &[2]).is_none());
        assert!(PublicKey::new(n, &[1, 0, 0]).is_none()); // even
        assert!(PublicKey::new(n, &[]).is_none());
        assert!(PublicKey::new(n, n).is_none()); // e >= n
        let mut even = n.clone();
        *even.last_mut().unwrap() &= 0xfe;
        assert!(PublicKey::new(&even, &[3]).is_none());
        assert!(PublicKey::new(&[], &[3]).is_none());
        assert!(PublicKey::new(&n[..63], &[3]).is_none()); // 511 bits or fewer... (504)
        assert!(PublicKey::new(&[0xff; 1025], &[3]).is_none()); // 8200 bits
        assert!(PublicKey::new(&[0xff; 1024], &[3]).is_some()); // 8192 bits, odd
        // sig = 0, sig = n, sig = n - 1 and all 0xff never verify.
        let key = PublicKey::new(n, &[1, 0, 1]).unwrap();
        let dg = [7u8; 32];
        let mut nm1 = n.clone();
        *nm1.last_mut().unwrap() -= 1;
        for sig in [vec![0u8; 256], n.clone(), nm1, vec![0xff; 256]] {
            assert!(!key.verify_pkcs1v15(Hash::Sha256, &dg, &sig));
            assert!(!key.verify_pss(Hash::Sha256, &dg, &sig, 32));
        }
    }

    /// A toy key with public factors, n = (2^521 - 1)(2^607 - 1), 1128 bits, so the tests
    /// can sign any encoded message they like (TEST ONLY: its factors are famous primes).
    const TOY_D: &str = "2a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a7fd5802a5555aaaa5555aaaa5555aa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa0055ffaa01";

    /// n of the toy key (hex of (2^521 - 1)(2^607 - 1), from Python).
    const TOY_N: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7ffffffffffffffffffffe0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001";

    fn toy() -> (PublicKey, Mont, BigUint) {
        let n = h(TOY_N);
        let key = PublicKey::new(&n, &[1, 0, 1]).unwrap();
        let m = Mont::new(&BigUint::from_be_bytes(&n)).unwrap();
        (key, m, BigUint::from_be_bytes(&h(TOY_D)))
    }

    fn sign(m: &Mont, d: &BigUint, em: &[u8]) -> Vec<u8> {
        m.pow(&BigUint::from_be_bytes(em), d)
            .unwrap()
            .to_be_bytes(141)
            .unwrap()
    }

    fn pkcs1_em(prefix: &[u8], digest: &[u8], k: usize) -> Vec<u8> {
        let mut em = vec![0x00, 0x01];
        em.resize(k - prefix.len() - digest.len() - 1, 0xff);
        em.push(0);
        em.extend_from_slice(prefix);
        em.extend_from_slice(digest);
        em
    }

    #[test]
    fn toy_key_pkcs1_padding() {
        let (key, m, d) = toy();
        assert_eq!(key.bits(), 1128);
        let dg = Hash::Sha256.digest(&[b"toy"]);
        let good = pkcs1_em(Hash::Sha256.digest_info(), &dg, 141);
        assert!(key.verify_pkcs1v15(Hash::Sha256, &dg, &sign(&m, &d, &good)));
        // DigestInfo without the NULL parameter (some old signers): refused.
        let no_null = h("302f300b0609608648016503040201 0420"
            .replace(' ', "")
            .as_str());
        let em = pkcs1_em(&no_null, &dg, 141);
        assert!(!key.verify_pkcs1v15(Hash::Sha256, &dg, &sign(&m, &d, &em)));
        // Block type 2, a zero in the padding, a short padding with garbage, no separator.
        let mut em = good.clone();
        em[1] = 2;
        assert!(!key.verify_pkcs1v15(Hash::Sha256, &dg, &sign(&m, &d, &em)));
        let mut em = good.clone();
        em[20] = 0;
        assert!(!key.verify_pkcs1v15(Hash::Sha256, &dg, &sign(&m, &d, &em)));
        let mut em = good.clone();
        let sep = 141 - 19 - 32 - 1;
        em[sep] = 0xff;
        assert!(!key.verify_pkcs1v15(Hash::Sha256, &dg, &sign(&m, &d, &em)));
        // The digest hidden after garbage (the classic lax-parser attack): the hash is
        // right-aligned but the DigestInfo is longer than it should be.
        let mut em = vec![
            0x00, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
        ];
        em.extend_from_slice(Hash::Sha256.digest_info());
        em.extend_from_slice(&dg);
        em.resize(141, 0x41);
        assert!(!key.verify_pkcs1v15(Hash::Sha256, &dg, &sign(&m, &d, &em)));
        // The wrong hash's DigestInfo.
        let em = pkcs1_em(Hash::Sha384.digest_info(), &dg, 141);
        assert!(!key.verify_pkcs1v15(Hash::Sha256, &dg, &sign(&m, &d, &em)));
    }

    fn pss_em(hash: Hash, digest: &[u8], salt: &[u8], em_len: usize) -> Vec<u8> {
        let hh = hash.digest(&[&[0u8; 8], digest, salt]);
        let db_len = em_len - hash.len() - 1;
        let mut db = vec![0u8; db_len - salt.len() - 1];
        db.push(1);
        db.extend_from_slice(salt);
        let mask = mgf1(hash, &hh, db_len);
        for (a, b) in db.iter_mut().zip(mask) {
            *a ^= b;
        }
        db[0] &= 0x7f; // emBits = 1127: the top bit of 141 bytes is clear
        let mut em = db;
        em.extend_from_slice(&hh);
        em.push(0xbc);
        em
    }

    #[test]
    fn toy_key_pss() {
        let (key, m, d) = toy();
        for hash in [Hash::Sha256, Hash::Sha384, Hash::Sha512] {
            let dg = hash.digest(&[b"toy pss"]);
            let salt: Vec<u8> = (0..hash.len() as u8).collect();
            let em = pss_em(hash, &dg, &salt, 141);
            assert!(key.verify_pss(hash, &dg, &sign(&m, &d, &em), hash.len()));
            // Salt of another length: verifies only with that length.
            let em0 = pss_em(hash, &dg, &[], 141);
            assert!(key.verify_pss(hash, &dg, &sign(&m, &d, &em0), 0));
            assert!(!key.verify_pss(hash, &dg, &sign(&m, &d, &em0), hash.len()));
            // Wrong trailer.
            let mut e = em.clone();
            *e.last_mut().unwrap() = 0xbb;
            assert!(!key.verify_pss(hash, &dg, &sign(&m, &d, &e), hash.len()));
            // The top bit (above emBits) set.
            let mut e = em.clone();
            e[0] |= 0x80;
            assert!(!key.verify_pss(hash, &dg, &sign(&m, &d, &e), hash.len()));
            // A nonzero byte in PS, or the 0x01 separator changed (masked: flip in DB).
            let mut e = em.clone();
            e[5] ^= 1;
            assert!(!key.verify_pss(hash, &dg, &sign(&m, &d, &e), hash.len()));
            // A salt length too big for the modulus.
            assert!(!key.verify_pss(hash, &dg, &sign(&m, &d, &em), 141));
            assert!(!key.verify_pss(hash, &dg, &sign(&m, &d, &em), usize::MAX));
        }
    }

    #[test]
    fn mutation_fuzz() {
        // xorshift64, fixed seed.
        let mut st = 0x9e3779b97f4a7c15u64;
        let mut rnd = move || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            st
        };
        let v = vectors();
        let mut tried = 0;
        for (_, n, e, sigs) in v.iter().take(2) {
            let key = PublicKey::new(n, e).unwrap();
            for (scheme, hash, msg, sig) in sigs.iter().take(3) {
                let dg = hash.digest(&[msg]);
                for _ in 0..60 {
                    let mut s = sig.clone();
                    match rnd() % 4 {
                        0 => {
                            let i = (rnd() as usize) % s.len();
                            s[i] ^= 1 << (rnd() % 8);
                        }
                        1 => s.truncate((rnd() as usize) % s.len()),
                        2 => s.push(rnd() as u8),
                        _ => {
                            let i = (rnd() as usize) % s.len();
                            s[i] = rnd() as u8;
                            let j = (rnd() as usize) % s.len();
                            s[j] = rnd() as u8;
                        }
                    }
                    if s == *sig {
                        continue;
                    }
                    let a = if scheme == "pkcs1" {
                        key.verify_pkcs1v15(*hash, &dg, &s)
                    } else {
                        key.verify_pss(*hash, &dg, &s, hash.len())
                    };
                    assert!(!a);
                    tried += 1;
                }
            }
        }
        // Random keys and signatures: never panic.
        for _ in 0..200 {
            let len = (rnd() % 300) as usize;
            let n: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
            let e: Vec<u8> = (0..(rnd() % 5) as usize).map(|_| rnd() as u8).collect();
            if let Some(k) = PublicKey::new(&n, &e) {
                let s: Vec<u8> = (0..len).map(|_| rnd() as u8).collect();
                let _ = k.verify_pkcs1v15(Hash::Sha256, &[0; 32], &s);
                let _ = k.verify_pss(Hash::Sha512, &[0; 64], &s, 64);
            }
        }
        assert!(tried > 300);
    }
}
