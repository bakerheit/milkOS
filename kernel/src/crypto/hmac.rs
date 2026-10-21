//! HMAC-SHA256 (RFC 2104 / 4231) and PBKDF2-HMAC-SHA256 (RFC 8018), on the audit log's
//! SHA-256.

use crate::audit::sha256::Sha256;

pub struct HmacSha256 {
    inner: Sha256,
    outer: Sha256,
}

impl HmacSha256 {
    pub fn new(key: &[u8]) -> HmacSha256 {
        let mut k = [0u8; 64];
        if key.len() > 64 {
            k[..32].copy_from_slice(&crate::audit::sha256::sha256(key));
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
        super::ct::zeroize(&mut k);
        super::ct::zeroize(&mut ipad);
        super::ct::zeroize(&mut opad);
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
