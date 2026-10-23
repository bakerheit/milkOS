//! HMAC (RFC 2104): HMAC-SHA256 ([`HmacSha256`]), and [`Hmac`] over any [`Hash`] (SHA-256,
//! SHA-384, SHA-512, for TLS and HKDF); PBKDF2-HMAC-SHA256 (RFC 8018). [`selftest`]: RFC 4231
//! cases 1, 2 and 6 (a key longer than a block) for SHA-256, SHA-384 and SHA-512, RFC 7914
//! section 11 (PBKDF2, c = 1).

use crate::hash::{Hash, MAX_BLOCK, MAX_OUT};
use crate::sha256::{Sha256, sha256};

pub struct HmacSha256 {
    inner: Sha256,
    outer: Sha256,
}

impl HmacSha256 {
    pub fn new(key: &[u8]) -> HmacSha256 {
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&sha256(key));
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let (mut ipad, mut opad) = ([0x36u8; 64], [0x5cu8; 64]);
        for i in 0..64 {
            ipad[i] ^= k[i];
            opad[i] ^= k[i];
        }
        let mut inner = Sha256::new();
        inner.update(&ipad);
        let mut outer = Sha256::new();
        outer.update(&opad);
        crate::wipe(&mut k);
        crate::wipe(&mut ipad);
        crate::wipe(&mut opad);
        HmacSha256 { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) -> &mut Self {
        self.inner.update(data);
        self
    }

    pub fn finish(mut self) -> [u8; 32] {
        let ih = self.inner.finish();
        self.outer.update(&ih);
        self.outer.finish()
    }
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut h = HmacSha256::new(key);
    h.update(data);
    h.finish()
}

/// HMAC over any [`Hash`].
#[derive(Clone)]
pub struct Hmac<H: Hash> {
    inner: H,
    outer: H,
}

impl<H: Hash> Hmac<H> {
    pub fn new(key: &[u8]) -> Hmac<H> {
        let mut k = [0u8; MAX_BLOCK];
        if key.len() > H::BLOCK {
            crate::hash::digest::<H>(key, &mut k);
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut pad = [0u8; MAX_BLOCK];
        for i in 0..H::BLOCK {
            pad[i] = k[i] ^ 0x36;
        }
        let mut inner = H::new();
        inner.input(&pad[..H::BLOCK]);
        for i in 0..H::BLOCK {
            pad[i] = k[i] ^ 0x5c;
        }
        let mut outer = H::new();
        outer.input(&pad[..H::BLOCK]);
        crate::wipe(&mut k);
        crate::wipe(&mut pad);
        Hmac { inner, outer }
    }

    pub fn update(&mut self, data: &[u8]) -> &mut Self {
        self.inner.input(data);
        self
    }

    /// The tag (H::OUT bytes) into the front of `out`.
    pub fn finish_into(mut self, out: &mut [u8]) {
        let mut ih = [0u8; MAX_OUT];
        self.inner.result(&mut ih);
        self.outer.input(&ih[..H::OUT]);
        self.outer.result(out);
        crate::wipe(&mut ih);
    }
}

/// One-shot HMAC into the front of `out` (H::OUT bytes).
pub fn hmac<H: Hash>(key: &[u8], data: &[u8], out: &mut [u8]) {
    let mut h = Hmac::<H>::new(key);
    h.update(data);
    h.finish_into(out);
}

/// PBKDF2-HMAC-SHA256 into `out`.
pub fn pbkdf2_sha256(pass: &[u8], salt: &[u8], iters: u32, out: &mut [u8]) {
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        let mut h = HmacSha256::new(pass);
        h.update(salt).update(&((i as u32) + 1).to_be_bytes());
        let mut u = h.finish();
        let mut t = u;
        for _ in 1..iters {
            u = hmac_sha256(pass, &u);
            for (a, b) in t.iter_mut().zip(u.iter()) {
                *a ^= b;
            }
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

pub fn selftest() -> (usize, usize) {
    let mut ok = 0;
    let v = |key: &[u8], data: &[u8], want: &str| {
        hmac_sha256(key, data)[..] == crate::unhex_vec(want)[..]
    };
    ok += v(
        &[0x0b; 20],
        b"Hi There",
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
    ) as usize;
    ok += v(
        b"Jefe",
        b"what do ya want for nothing?",
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
    ) as usize;
    ok += v(
        &[0xaa; 131],
        b"Test Using Larger Than Block-Size Key - Hash Key First",
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
    ) as usize;
    // RFC 4231 for SHA-384 and SHA-512 (cases 1, 2, 6).
    use crate::sha512::{Sha384, Sha512};
    let cases: [(&[u8], &[u8], &str, &str); 3] = [
        (
            &[0x0b; 20],
            b"Hi There",
            "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6",
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
        ),
        (
            b"Jefe",
            b"what do ya want for nothing?",
            "af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649",
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737",
        ),
        (
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
            "4ece084485813e9088d2c63a041bc5b44f9ef1012a2b588f3cd11f05033ac4c60c2ef6ab4030fe8296248df163f44952",
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f3526b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598",
        ),
    ];
    let mut t = [0u8; 64];
    for (key, data, w384, w512) in cases {
        hmac::<Sha384>(key, data, &mut t);
        ok += (crate::hex(&t[..48]) == w384) as usize;
        hmac::<Sha512>(key, data, &mut t);
        ok += (crate::hex(&t) == w512) as usize;
        hmac::<Sha256>(key, data, &mut t);
        ok += (t[..32] == hmac_sha256(key, data)) as usize;
    }
    let mut out = [0u8; 64];
    pbkdf2_sha256(b"passwd", b"salt", 1, &mut out);
    ok += (out[..]
        == crate::unhex_vec(
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783",
        )[..]) as usize;
    (ok, 13)
}
