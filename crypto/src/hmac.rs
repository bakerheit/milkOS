//! HMAC-SHA256 (RFC 2104) and PBKDF2-HMAC-SHA256 (RFC 8018). [`selftest`]: RFC 4231
//! cases 1, 2 and 6 (a key longer than a block), RFC 7914 section 11 (PBKDF2, c = 1).

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
    let mut out = [0u8; 64];
    pbkdf2_sha256(b"passwd", b"salt", 1, &mut out);
    ok += (out[..]
        == crate::unhex_vec(
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783",
        )[..]) as usize;
    (ok, 4)
}
