//! GF(2^255 - 19), shared by Ed25519 and X25519: five 51-bit limbs, u128 products.
//! Arithmetic is branch-free on the values (constant time); `pow` (and so `invert`)
//! branches only on the public exponent.

const MASK: u64 = (1 << 51) - 1;

#[derive(Clone, Copy)]
pub(crate) struct Fe(pub(crate) [u64; 5]);

impl Fe {
    pub(crate) const ZERO: Fe = Fe([0; 5]);
    pub(crate) const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    pub(crate) fn small(v: u64) -> Fe {
        Fe([v, 0, 0, 0, 0])
    }

    pub(crate) fn from_bytes(b: &[u8; 32]) -> Fe {
        let w = |i: usize| u64::from_le_bytes(b[8 * i..8 * i + 8].try_into().unwrap());
        let (w0, w1, w2, w3) = (w(0), w(1), w(2), w(3) & !(1 << 63));
        Fe([
            w0 & MASK,
            ((w0 >> 51) | (w1 << 13)) & MASK,
            ((w1 >> 38) | (w2 << 26)) & MASK,
            ((w2 >> 25) | (w3 << 39)) & MASK,
            (w3 >> 12) & MASK,
        ])
    }

    pub(crate) fn carry(mut h: [u64; 5]) -> Fe {
        for _ in 0..2 {
            let c0 = h[0] >> 51;
            h[0] &= MASK;
            h[1] += c0;
            let c1 = h[1] >> 51;
            h[1] &= MASK;
            h[2] += c1;
            let c2 = h[2] >> 51;
            h[2] &= MASK;
            h[3] += c2;
            let c3 = h[3] >> 51;
            h[3] &= MASK;
            h[4] += c3;
            let c4 = h[4] >> 51;
            h[4] &= MASK;
            h[0] += 19 * c4;
        }
        Fe(h)
    }

    pub(crate) fn to_bytes(self) -> [u8; 32] {
        let mut h = Fe::carry(self.0).0;
        // Subtract p once if h >= p: q = 1 iff h + 19 overflows 2^255.
        let mut q = (h[0] + 19) >> 51;
        q = (h[1] + q) >> 51;
        q = (h[2] + q) >> 51;
        q = (h[3] + q) >> 51;
        q = (h[4] + q) >> 51;
        h[0] += 19 * q;
        for i in 0..4 {
            h[i + 1] += h[i] >> 51;
            h[i] &= MASK;
        }
        h[4] &= MASK;
        let w0 = h[0] | (h[1] << 51);
        let w1 = (h[1] >> 13) | (h[2] << 38);
        let w2 = (h[2] >> 26) | (h[3] << 25);
        let w3 = (h[3] >> 39) | (h[4] << 12);
        let mut out = [0u8; 32];
        for (i, w) in [w0, w1, w2, w3].iter().enumerate() {
            out[8 * i..8 * i + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    pub(crate) fn add(self, o: Fe) -> Fe {
        let mut h = [0u64; 5];
        for (i, v) in h.iter_mut().enumerate() {
            *v = self.0[i] + o.0[i];
        }
        Fe::carry(h)
    }

    pub(crate) fn sub(self, o: Fe) -> Fe {
        // + 2p keeps every limb positive (inputs are carried: limbs < 2^52).
        const TWO_P: [u64; 5] = [
            0xfffffffffffda,
            0xffffffffffffe,
            0xffffffffffffe,
            0xffffffffffffe,
            0xffffffffffffe,
        ];
        let mut h = [0u64; 5];
        for (i, v) in h.iter_mut().enumerate() {
            *v = self.0[i] + TWO_P[i] - o.0[i];
        }
        Fe::carry(h)
    }

    pub(crate) fn neg(self) -> Fe {
        Fe::ZERO.sub(self)
    }

    pub(crate) fn mul(self, o: Fe) -> Fe {
        let a = self.0;
        let b = o.0;
        let m = |x: u64, y: u64| x as u128 * y as u128;
        let (b1, b2, b3, b4) = (b[1] * 19, b[2] * 19, b[3] * 19, b[4] * 19);
        let r0 = m(a[0], b[0]) + m(a[1], b4) + m(a[2], b3) + m(a[3], b2) + m(a[4], b1);
        let r1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4) + m(a[3], b3) + m(a[4], b2);
        let r2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4) + m(a[4], b3);
        let r3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4);
        let r4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);
        let mask = MASK as u128;
        let c = r0 >> 51;
        let r1 = r1 + c;
        let c1 = r1 >> 51;
        let r2 = r2 + c1;
        let c2 = r2 >> 51;
        let r3 = r3 + c2;
        let c3 = r3 >> 51;
        let r4 = r4 + c3;
        let c4 = r4 >> 51;
        // Fold the top carry back (x 19) in u128: it can exceed 64 bits.
        let h0 = (r0 & mask) + c4 * 19;
        let h1 = (r1 & mask) + (h0 >> 51);
        let h = [
            (h0 & mask) as u64,
            h1 as u64,
            (r2 & mask) as u64,
            (r3 & mask) as u64,
            (r4 & mask) as u64,
        ];
        Fe::carry(h)
    }

    pub(crate) fn sq(self) -> Fe {
        self.mul(self)
    }

    /// self^e, e little-endian bytes.
    pub(crate) fn pow(self, e: &[u8; 32]) -> Fe {
        let mut r = Fe::ONE;
        for i in (0..256).rev() {
            r = r.sq();
            if e[i / 8] >> (i % 8) & 1 == 1 {
                r = r.mul(self);
            }
        }
        r
    }

    pub(crate) fn invert(self) -> Fe {
        // p - 2 = 2^255 - 21
        let mut e = [0xffu8; 32];
        e[0] = 0xeb;
        e[31] = 0x7f;
        self.pow(&e)
    }

    pub(crate) fn eq(self, o: Fe) -> bool {
        self.to_bytes() == o.to_bytes()
    }

    pub(crate) fn is_negative(self) -> bool {
        self.to_bytes()[0] & 1 == 1
    }
}

impl Fe {
    /// Constant-time conditional swap (`swap` is 0 or 1), for the Montgomery ladder.
    pub(crate) fn cswap(a: &mut Fe, b: &mut Fe, swap: u64) {
        let m = 0u64.wrapping_sub(swap);
        for i in 0..5 {
            let t = m & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }
}
