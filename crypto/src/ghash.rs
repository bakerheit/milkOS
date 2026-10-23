//! GHASH, the universal hash of GCM (NIST SP 800-38D 6.4), constant time and table-free.
//!
//! Multiplication in GF(2^128) is a carry-less 128 x 128-bit product (three 64 x 64 products,
//! Karatsuba) followed by the reduction modulo x^128 + x^7 + x^2 + x + 1. Each carry-less
//! 64 x 64 product is built from ordinary integer multiplies on the operands split into four
//! masks with "holes" (every fourth bit), so the carries of an integer multiply land in bits
//! that are masked away; the high half comes from the same trick on the bit-reversed
//! operands. Integer multiplication is constant time on the CPUs milkOS runs on, and nothing
//! here branches on or indexes by secret data. This is BearSSL's `ghash_ctmul64`.

use crate::aes::wipe_u64;

/// Low 64 bits of the carry-less product of `x` and `y`.
fn bmul64(x: u64, y: u64) -> u64 {
    const M0: u64 = 0x1111_1111_1111_1111;
    const M1: u64 = 0x2222_2222_2222_2222;
    const M2: u64 = 0x4444_4444_4444_4444;
    const M3: u64 = 0x8888_8888_8888_8888;
    let (x0, x1, x2, x3) = (x & M0, x & M1, x & M2, x & M3);
    let (y0, y1, y2, y3) = (y & M0, y & M1, y & M2, y & M3);
    let m = |a: u64, b: u64| a.wrapping_mul(b);
    let z0 = m(x0, y0) ^ m(x1, y3) ^ m(x2, y2) ^ m(x3, y1);
    let z1 = m(x0, y1) ^ m(x1, y0) ^ m(x2, y3) ^ m(x3, y2);
    let z2 = m(x0, y2) ^ m(x1, y1) ^ m(x2, y0) ^ m(x3, y3);
    let z3 = m(x0, y3) ^ m(x1, y2) ^ m(x2, y1) ^ m(x3, y0);
    (z0 & M0) | (z1 & M1) | (z2 & M2) | (z3 & M3)
}

fn rev64(x: u64) -> u64 {
    x.reverse_bits()
}

/// A running GHASH under key `H`. Wiped on drop.
pub struct Ghash {
    // [h0, h1, h2, h0r, h1r, h2r, y0, y1]: H's halves (h1 = first 8 bytes), their XOR and
    // bit reversals, and the accumulator (y1 = first 8 bytes).
    s: [u64; 8],
}

impl Drop for Ghash {
    fn drop(&mut self) {
        wipe_u64(&mut self.s);
    }
}

impl Ghash {
    pub fn new(h: &[u8; 16]) -> Ghash {
        let mut a = [0u8; 8];
        a.copy_from_slice(&h[..8]);
        let h1 = u64::from_be_bytes(a);
        a.copy_from_slice(&h[8..]);
        let h0 = u64::from_be_bytes(a);
        crate::wipe(&mut a);
        let (h0r, h1r) = (rev64(h0), rev64(h1));
        Ghash {
            s: [h0, h1, h0 ^ h1, h0r, h1r, h0r ^ h1r, 0, 0],
        }
    }

    /// Y = (Y xor block) * H.
    fn block(&mut self, b: &[u8; 16]) {
        let [h0, h1, h2, h0r, h1r, h2r, mut y0, mut y1] = self.s;
        let mut a = [0u8; 8];
        a.copy_from_slice(&b[..8]);
        y1 ^= u64::from_be_bytes(a);
        a.copy_from_slice(&b[8..]);
        y0 ^= u64::from_be_bytes(a);

        let (y0r, y1r) = (rev64(y0), rev64(y1));
        let (y2, y2r) = (y0 ^ y1, y0r ^ y1r);
        let z0 = bmul64(y0, h0);
        let z1 = bmul64(y1, h1);
        let mut z2 = bmul64(y2, h2);
        let mut z0h = bmul64(y0r, h0r);
        let mut z1h = bmul64(y1r, h1r);
        let mut z2h = bmul64(y2r, h2r);
        z2 ^= z0 ^ z1;
        z2h ^= z0h ^ z1h;
        z0h = rev64(z0h) >> 1;
        z1h = rev64(z1h) >> 1;
        z2h = rev64(z2h) >> 1;

        // The 256-bit product (v3 high .. v0 low), in GCM's reflected bit order.
        let mut v0 = z0;
        let mut v1 = z0h ^ z2;
        let mut v2 = z1 ^ z2h;
        let mut v3 = z1h;
        v3 = (v3 << 1) | (v2 >> 63);
        v2 = (v2 << 1) | (v1 >> 63);
        v1 = (v1 << 1) | (v0 >> 63);
        v0 <<= 1;

        // Reduce modulo x^128 + x^7 + x^2 + x + 1.
        v2 ^= v0 ^ (v0 >> 1) ^ (v0 >> 2) ^ (v0 >> 7);
        v1 ^= (v0 << 63) ^ (v0 << 62) ^ (v0 << 57);
        v3 ^= v1 ^ (v1 >> 1) ^ (v1 >> 2) ^ (v1 >> 7);
        v2 ^= (v1 << 63) ^ (v1 << 62) ^ (v1 << 57);

        self.s[6] = v2;
        self.s[7] = v3;
        crate::wipe(&mut a);
    }

    /// Absorb `data`, zero-padding its last partial block (GCM pads the AAD and the
    /// ciphertext separately, so call this once for each).
    pub fn update_padded(&mut self, data: &[u8]) {
        let (blocks, rest) = data.as_chunks::<16>();
        for b in blocks {
            self.block(b);
        }
        if !rest.is_empty() {
            let mut b = [0u8; 16];
            b[..rest.len()].copy_from_slice(rest);
            self.block(&b);
            crate::wipe(&mut b);
        }
    }

    /// Absorb the length block (`aad_len` and `ct_len` are byte counts; GCM hashes them as
    /// 64-bit bit counts) and return the hash S.
    pub fn finish(mut self, aad_len: u64, ct_len: u64) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&aad_len.wrapping_mul(8).to_be_bytes());
        b[8..].copy_from_slice(&ct_len.wrapping_mul(8).to_be_bytes());
        self.block(&b);
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&self.s[7].to_be_bytes());
        out[8..].copy_from_slice(&self.s[6].to_be_bytes());
        out
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)] // test-only reference code, written like the standards
pub(crate) mod tests {
    use super::*;

    /// GF(2^128) multiply the slow way (SP 800-38D Algorithm 1), for tests only.
    pub(crate) fn ref_mul(x: u128, y: u128) -> u128 {
        let r: u128 = 0xe1 << 120;
        let mut z = 0u128;
        let mut v = y;
        for i in 0..128 {
            if (x >> (127 - i)) & 1 == 1 {
                z ^= v;
            }
            v = if v & 1 == 1 { (v >> 1) ^ r } else { v >> 1 };
        }
        z
    }

    pub(crate) fn ref_ghash(h: &[u8; 16], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let h = u128::from_be_bytes(*h);
        let mut y = 0u128;
        for part in [aad, ct] {
            for c in part.chunks(16) {
                let mut b = [0u8; 16];
                b[..c.len()].copy_from_slice(c);
                y = ref_mul(y ^ u128::from_be_bytes(b), h);
            }
        }
        let len = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        ref_mul(y ^ len, h).to_be_bytes()
    }

    #[test]
    fn matches_reference() {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        // Edge values of H too: 0, 1 (the top bit, GCM's "1"), all ones.
        let mut hs: alloc::vec::Vec<[u8; 16]> = alloc::vec![[0; 16], [0xff; 16], {
            let mut h = [0u8; 16];
            h[0] = 0x80;
            h
        }];
        for _ in 0..50 {
            hs.push(core::array::from_fn(|_| next() as u8));
        }
        for h in hs {
            for (alen, clen) in [
                (0, 0),
                (0, 1),
                (1, 0),
                (15, 17),
                (16, 16),
                (33, 64),
                (7, 100),
            ] {
                let aad: alloc::vec::Vec<u8> = (0..alen).map(|_| next() as u8).collect();
                let ct: alloc::vec::Vec<u8> = (0..clen).map(|_| next() as u8).collect();
                let mut g = Ghash::new(&h);
                g.update_padded(&aad);
                g.update_padded(&ct);
                assert_eq!(g.finish(alen as u64, clen as u64), ref_ghash(&h, &aad, &ct));
            }
        }
    }
}
