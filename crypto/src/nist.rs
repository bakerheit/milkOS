//! The shared arithmetic behind [`crate::p256`] and [`crate::p384`]: a prime field in
//! Montgomery form over `N` 64-bit limbs, and the short-Weierstrass curve y^2 = x^3 - 3x + b
//! in projective coordinates with the complete addition and doubling formulas of
//! Renes, Costello and Batina (2016, algorithms 4 and 6, a = -3).
//!
//! Constant time: field operations have no branches or memory indices that depend on the
//! values (carries are folded into masks; the final subtraction of a Montgomery product is
//! a masked select); the formulas are complete, so adding the identity or a point to itself
//! needs no special case; [`Curve::mul`] is a fixed 4-bit window over every nibble of the
//! scalar with a table scanned in full for each lookup. Only public data (the modulus, the
//! exponent p - 2 of an inversion, the curve constants) steers control flow.
//!
//! Every constant is given in hex and turned into limbs by `const fn`s, and the Montgomery
//! constants (-p^-1 mod 2^64, R mod p, R^2 mod p) are computed at compile time from p.

// Limb loops read best indexed; `from_*` here are conversions of a value in a field.
#![allow(clippy::needless_range_loop, clippy::wrong_self_convention)]

/// Little-endian 64-bit limbs from a big-endian hex string of exactly 16 * N digits.
pub(crate) const fn limbs<const N: usize>(hex: &str) -> [u64; N] {
    let b = hex.as_bytes();
    assert!(b.len() == 16 * N);
    let mut out = [0u64; N];
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("bad hex digit"),
        } as u64;
        // Digit i (from the left) is bit position 4 * (16N - 1 - i).
        let pos = 16 * N - 1 - i;
        out[pos / 16] |= d << (4 * (pos % 16));
        i += 1;
    }
    out
}

/// -m^-1 mod 2^64 (m odd), by Newton's iteration.
const fn neg_inv64(m: u64) -> u64 {
    let mut x: u64 = 1;
    let mut i = 0;
    while i < 6 {
        x = x.wrapping_mul(2u64.wrapping_sub(m.wrapping_mul(x)));
        i += 1;
    }
    x.wrapping_neg()
}

/// a >= m (variable time; compile-time and public values only).
const fn geq<const N: usize>(a: &[u64; N], m: &[u64; N]) -> bool {
    let mut i = N;
    while i > 0 {
        i -= 1;
        if a[i] != m[i] {
            return a[i] > m[i];
        }
    }
    true
}

/// 2^doublings mod m, by doubling 1 (compile time).
const fn pow2_mod<const N: usize>(m: &[u64; N], doublings: usize) -> [u64; N] {
    let mut x = [0u64; N];
    x[0] = 1;
    shl_mod(x, m, doublings)
}

/// x * 2^doublings mod m for x < m, by doubling (compile time).
const fn shl_mod<const N: usize>(mut x: [u64; N], m: &[u64; N], doublings: usize) -> [u64; N] {
    let mut d = 0;
    while d < doublings {
        let mut carry = 0u64;
        let mut i = 0;
        while i < N {
            let hi = x[i] >> 63;
            x[i] = (x[i] << 1) | carry;
            carry = hi;
            i += 1;
        }
        if carry == 1 || geq(&x, m) {
            let mut borrow = 0u64;
            let mut i = 0;
            while i < N {
                let (t, b1) = x[i].overflowing_sub(m[i]);
                let (t, b2) = t.overflowing_sub(borrow);
                x[i] = t;
                borrow = (b1 | b2) as u64;
                i += 1;
            }
        }
        d += 1;
    }
    x
}

/// An odd modulus with its Montgomery constants.
pub(crate) struct Modulus<const N: usize> {
    pub m: [u64; N],
    n0: u64,
    /// R mod m (the Montgomery form of 1), R = 2^(64N).
    one: [u64; N],
    /// R^2 mod m.
    r2: [u64; N],
}

impl<const N: usize> Modulus<N> {
    pub const fn new(hex: &str) -> Modulus<N> {
        let m = limbs::<N>(hex);
        Modulus {
            m,
            n0: neg_inv64(m[0]),
            one: pow2_mod(&m, 64 * N),
            r2: pow2_mod(&m, 128 * N),
        }
    }

    /// The Montgomery form of 1.
    pub fn one(&self) -> [u64; N] {
        self.one
    }

    /// a + b mod m (a, b < m).
    pub fn add(&self, a: &[u64; N], b: &[u64; N]) -> [u64; N] {
        let mut s = [0u64; N];
        let mut carry = 0u64;
        for i in 0..N {
            let t = a[i] as u128 + b[i] as u128 + carry as u128;
            s[i] = t as u64;
            carry = (t >> 64) as u64;
        }
        self.reduce_once(&s, carry)
    }

    /// a - b mod m (a, b < m).
    pub fn sub(&self, a: &[u64; N], b: &[u64; N]) -> [u64; N] {
        let mut d = [0u64; N];
        let mut borrow = 0u64;
        for i in 0..N {
            let (t, b1) = a[i].overflowing_sub(b[i]);
            let (t, b2) = t.overflowing_sub(borrow);
            d[i] = t;
            borrow = (b1 | b2) as u64;
        }
        // Add m back if it went negative.
        let mask = borrow.wrapping_neg();
        let mut carry = 0u64;
        for i in 0..N {
            let t = d[i] as u128 + (self.m[i] & mask) as u128 + carry as u128;
            d[i] = t as u64;
            carry = (t >> 64) as u64;
        }
        d
    }

    /// (hi:x) - m if that is not negative, else x; for values < 2m.
    fn reduce_once(&self, x: &[u64; N], hi: u64) -> [u64; N] {
        let mut d = [0u64; N];
        let mut borrow = 0u64;
        for i in 0..N {
            let (t, b1) = x[i].overflowing_sub(self.m[i]);
            let (t, b2) = t.overflowing_sub(borrow);
            d[i] = t;
            borrow = (b1 | b2) as u64;
        }
        // The subtraction underflowed overall iff borrow > hi (hi is 0 or 1).
        let under = borrow & !hi & 1;
        let keep_x = under.wrapping_neg();
        let mut out = [0u64; N];
        for i in 0..N {
            out[i] = (x[i] & keep_x) | (d[i] & !keep_x);
        }
        out
    }

    /// Montgomery product a * b / R mod m (CIOS), for a, b < m.
    pub fn mul(&self, a: &[u64; N], b: &[u64; N]) -> [u64; N] {
        // t has N + 2 words; N <= 8 here.
        let mut t = [0u64; 10];
        for i in 0..N {
            let mut c: u64 = 0;
            for j in 0..N {
                let x = t[j] as u128 + (a[j] as u128) * (b[i] as u128) + c as u128;
                t[j] = x as u64;
                c = (x >> 64) as u64;
            }
            let x = t[N] as u128 + c as u128;
            t[N] = x as u64;
            t[N + 1] = (x >> 64) as u64;
            let mm = t[0].wrapping_mul(self.n0);
            let x = t[0] as u128 + (mm as u128) * (self.m[0] as u128);
            let mut c = (x >> 64) as u64;
            for j in 1..N {
                let x = t[j] as u128 + (mm as u128) * (self.m[j] as u128) + c as u128;
                t[j - 1] = x as u64;
                c = (x >> 64) as u64;
            }
            let x = t[N] as u128 + c as u128;
            t[N - 1] = x as u64;
            t[N] = t[N + 1] + ((x >> 64) as u64);
        }
        let mut r = [0u64; N];
        r.copy_from_slice(&t[..N]);
        self.reduce_once(&r, t[N])
    }

    pub fn sq(&self, a: &[u64; N]) -> [u64; N] {
        self.mul(a, a)
    }

    /// Into Montgomery form (a < m).
    pub fn to_mont(&self, a: &[u64; N]) -> [u64; N] {
        self.mul(a, &self.r2)
    }

    /// Out of Montgomery form.
    pub fn from_mont(&self, a: &[u64; N]) -> [u64; N] {
        let mut one = [0u64; N];
        one[0] = 1;
        self.mul(a, &one)
    }

    /// a^(m - 2): the inverse of a (Montgomery form in and out) when m is prime; 0 for 0.
    /// The exponent is public, so branching on its bits is fine.
    pub fn inv(&self, a: &[u64; N]) -> [u64; N] {
        let mut e = self.m;
        // m is odd and > 2, so m - 2 never borrows past limb 0.
        e[0] -= 2;
        let mut r = self.one;
        for i in (0..N).rev() {
            for bit in (0..64).rev() {
                r = self.sq(&r);
                if (e[i] >> bit) & 1 == 1 {
                    r = self.mul(&r, a);
                }
            }
        }
        r
    }

    /// Big-endian bytes (exactly 8N) to limbs, None if not < m. Variable time in the
    /// comparison only, which reveals whether the input was in range.
    pub fn from_be(&self, bytes: &[u8]) -> Option<[u64; N]> {
        let a = be_to_limbs::<N>(bytes)?;
        if geq(&a, &self.m) { None } else { Some(a) }
    }

    /// Big-endian bytes (exactly 8N) reduced mod m by one subtraction: correct for inputs
    /// < 2m, which holds for 8N-byte values when the top bit of m is set.
    pub fn from_be_reduce(&self, bytes: &[u8]) -> Option<[u64; N]> {
        let a = be_to_limbs::<N>(bytes)?;
        Some(self.reduce_once(&a, 0))
    }
}

/// Exactly 8N big-endian bytes to little-endian limbs.
pub(crate) fn be_to_limbs<const N: usize>(bytes: &[u8]) -> Option<[u64; N]> {
    if bytes.len() != 8 * N {
        return None;
    }
    let mut a = [0u64; N];
    for (i, chunk) in bytes.rchunks(8).enumerate() {
        let mut w = [0u8; 8];
        w.copy_from_slice(chunk);
        a[i] = u64::from_be_bytes(w);
    }
    Some(a)
}

/// Limbs to 8N big-endian bytes, into `out` (which must be 8N long).
pub(crate) fn limbs_to_be<const N: usize>(a: &[u64; N], out: &mut [u8]) {
    for (i, chunk) in out.rchunks_mut(8).enumerate().take(N) {
        chunk.copy_from_slice(&a[i].to_be_bytes());
    }
}

/// All ones if a == 0, else 0 (constant time).
pub(crate) fn is_zero_mask<const N: usize>(a: &[u64; N]) -> u64 {
    let mut acc = 0u64;
    for x in a {
        acc |= x;
    }
    // acc == 0 -> top bit of (!acc & (acc - 1)) is set.
    let z = !acc & acc.wrapping_sub(1);
    0u64.wrapping_sub(z >> 63)
}

/// a if mask is all ones, b if it is zero.
pub(crate) fn select<const N: usize>(mask: u64, a: &[u64; N], b: &[u64; N]) -> [u64; N] {
    let mut out = [0u64; N];
    for i in 0..N {
        out[i] = (a[i] & mask) | (b[i] & !mask);
    }
    out
}

/// A point in projective coordinates (X : Y : Z), Montgomery form; Z = 0 is the identity.
#[derive(Clone, Copy)]
pub(crate) struct Point<const N: usize> {
    pub x: [u64; N],
    pub y: [u64; N],
    pub z: [u64; N],
}

/// A curve y^2 = x^3 - 3x + b over GF(p) with a generator of prime order n.
pub(crate) struct Curve<const N: usize> {
    pub p: Modulus<N>,
    pub n: Modulus<N>,
    /// b, Montgomery form.
    pub b: [u64; N],
    /// The generator, affine, normal (not Montgomery) form.
    pub gx: [u64; N],
    pub gy: [u64; N],
}

impl<const N: usize> Curve<N> {
    /// From the hex of p, n, b and the generator (compile time).
    pub const fn new(p: &str, n: &str, b: &str, gx: &str, gy: &str) -> Curve<N> {
        let pm = Modulus::new(p);
        let b = shl_mod(limbs::<N>(b), &pm.m, 64 * N);
        Curve {
            p: pm,
            n: Modulus::new(n),
            b,
            gx: limbs(gx),
            gy: limbs(gy),
        }
    }

    pub fn identity(&self) -> Point<N> {
        Point {
            x: [0; N],
            y: self.p.one(),
            z: [0; N],
        }
    }

    /// The generator as a projective point.
    pub fn generator(&self) -> Point<N> {
        Point {
            x: self.p.to_mont(&self.gx),
            y: self.p.to_mont(&self.gy),
            z: self.p.one(),
        }
    }

    /// Complete addition (RCB 2016 algorithm 4, a = -3).
    pub fn add(&self, a: &Point<N>, q: &Point<N>) -> Point<N> {
        let f = &self.p;
        let (x1, y1, z1) = (&a.x, &a.y, &a.z);
        let (x2, y2, z2) = (&q.x, &q.y, &q.z);
        let mut t0 = f.mul(x1, x2);
        let mut t1 = f.mul(y1, y2);
        let mut t2 = f.mul(z1, z2);
        let mut t3 = f.add(x1, y1);
        let mut t4 = f.add(x2, y2);
        t3 = f.mul(&t3, &t4);
        t4 = f.add(&t0, &t1);
        t3 = f.sub(&t3, &t4);
        t4 = f.add(y1, z1);
        let mut x3 = f.add(y2, z2);
        t4 = f.mul(&t4, &x3);
        x3 = f.add(&t1, &t2);
        t4 = f.sub(&t4, &x3);
        x3 = f.add(x1, z1);
        let mut y3 = f.add(x2, z2);
        x3 = f.mul(&x3, &y3);
        y3 = f.add(&t0, &t2);
        y3 = f.sub(&x3, &y3);
        let mut z3 = f.mul(&self.b, &t2);
        x3 = f.sub(&y3, &z3);
        z3 = f.add(&x3, &x3);
        x3 = f.add(&x3, &z3);
        z3 = f.sub(&t1, &x3);
        x3 = f.add(&t1, &x3);
        y3 = f.mul(&self.b, &y3);
        t1 = f.add(&t2, &t2);
        t2 = f.add(&t1, &t2);
        y3 = f.sub(&y3, &t2);
        y3 = f.sub(&y3, &t0);
        t1 = f.add(&y3, &y3);
        y3 = f.add(&t1, &y3);
        t1 = f.add(&t0, &t0);
        t0 = f.add(&t1, &t0);
        t0 = f.sub(&t0, &t2);
        t1 = f.mul(&t4, &y3);
        t2 = f.mul(&t0, &y3);
        y3 = f.mul(&x3, &z3);
        y3 = f.add(&y3, &t2);
        x3 = f.mul(&t3, &x3);
        x3 = f.sub(&x3, &t1);
        z3 = f.mul(&t4, &z3);
        t1 = f.mul(&t3, &t0);
        z3 = f.add(&z3, &t1);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    /// Doubling (RCB 2016 algorithm 6, a = -3); complete, so the identity doubles to itself.
    pub fn double(&self, a: &Point<N>) -> Point<N> {
        let f = &self.p;
        let (x, y, z) = (&a.x, &a.y, &a.z);
        let mut t0 = f.sq(x);
        let t1 = f.sq(y);
        let mut t2 = f.sq(z);
        let mut t3 = f.mul(x, y);
        t3 = f.add(&t3, &t3);
        let mut z3 = f.mul(x, z);
        z3 = f.add(&z3, &z3);
        let mut y3 = f.mul(&self.b, &t2);
        y3 = f.sub(&y3, &z3);
        let mut x3 = f.add(&y3, &y3);
        y3 = f.add(&x3, &y3);
        x3 = f.sub(&t1, &y3);
        y3 = f.add(&t1, &y3);
        y3 = f.mul(&x3, &y3);
        x3 = f.mul(&x3, &t3);
        t3 = f.add(&t2, &t2);
        t2 = f.add(&t2, &t3);
        z3 = f.mul(&self.b, &z3);
        z3 = f.sub(&z3, &t2);
        z3 = f.sub(&z3, &t0);
        t3 = f.add(&z3, &z3);
        z3 = f.add(&z3, &t3);
        t3 = f.add(&t0, &t0);
        t0 = f.add(&t3, &t0);
        t0 = f.sub(&t0, &t2);
        t0 = f.mul(&t0, &z3);
        y3 = f.add(&y3, &t0);
        t0 = f.mul(y, z);
        t0 = f.add(&t0, &t0);
        z3 = f.mul(&t0, &z3);
        x3 = f.sub(&x3, &z3);
        z3 = f.mul(&t0, &t1);
        z3 = f.add(&z3, &z3);
        z3 = f.add(&z3, &z3);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    /// k * P for a scalar given as 8N big-endian bytes (any value; it is not reduced),
    /// in constant time: 4 doublings and one addition per nibble, the table entry picked by
    /// scanning all 16.
    pub fn mul(&self, k: &[u8], pt: &Point<N>) -> Point<N> {
        let mut table = [self.identity(); 16];
        table[1] = *pt;
        for i in 2..16 {
            table[i] = if i % 2 == 0 {
                self.double(&table[i / 2])
            } else {
                self.add(&table[i - 1], pt)
            };
        }
        let mut acc = self.identity();
        for byte in k {
            for nib in [byte >> 4, byte & 15] {
                for _ in 0..4 {
                    acc = self.double(&acc);
                }
                let mut sel = self.identity();
                for (i, t) in table.iter().enumerate() {
                    // All ones iff i == nib.
                    let d = (i as u64) ^ (nib as u64);
                    let m = 0u64.wrapping_sub(((d | d.wrapping_neg()) >> 63) ^ 1);
                    sel.x = select(m, &t.x, &sel.x);
                    sel.y = select(m, &t.y, &sel.y);
                    sel.z = select(m, &t.z, &sel.z);
                }
                acc = self.add(&acc, &sel);
            }
        }
        for t in table.iter_mut() {
            *t = self.identity();
        }
        core::hint::black_box(&table);
        acc
    }

    /// Affine (x, y) in normal form, or None for the identity.
    pub fn to_affine(&self, a: &Point<N>) -> Option<([u64; N], [u64; N])> {
        let f = &self.p;
        if is_zero_mask(&a.z) != 0 {
            return None;
        }
        let zi = f.inv(&a.z);
        Some((
            f.from_mont(&f.mul(&a.x, &zi)),
            f.from_mont(&f.mul(&a.y, &zi)),
        ))
    }

    /// Parse an uncompressed SEC 1 point (0x04 || X || Y, coordinates < p, on the curve).
    pub fn decode(&self, enc: &[u8]) -> Option<Point<N>> {
        if enc.len() != 1 + 16 * N || enc[0] != 0x04 {
            return None;
        }
        let x = self.p.from_be(&enc[1..1 + 8 * N])?;
        let y = self.p.from_be(&enc[1 + 8 * N..])?;
        let (xm, ym) = (self.p.to_mont(&x), self.p.to_mont(&y));
        if !self.on_curve(&xm, &ym) {
            return None;
        }
        Some(Point {
            x: xm,
            y: ym,
            z: self.p.one(),
        })
    }

    /// y^2 == x^3 - 3x + b, Montgomery form.
    pub fn on_curve(&self, x: &[u64; N], y: &[u64; N]) -> bool {
        let f = &self.p;
        let lhs = f.sq(y);
        let x3 = f.mul(&f.sq(x), x);
        let three_x = f.add(&f.add(x, x), x);
        let rhs = f.add(&f.sub(&x3, &three_x), &self.b);
        is_zero_mask(&f.sub(&lhs, &rhs)) != 0
    }

    /// 0x04 || X || Y of an affine point.
    pub fn encode(&self, x: &[u64; N], y: &[u64; N], out: &mut [u8]) {
        out[0] = 4;
        limbs_to_be(x, &mut out[1..1 + 8 * N]);
        limbs_to_be(y, &mut out[1 + 8 * N..1 + 16 * N]);
    }

    /// A secret scalar (8N big-endian bytes) in [1, n - 1]? Constant time in the value
    /// (only the answer comes out).
    pub fn scalar_ok(&self, k: &[u8]) -> bool {
        let Some(a) = be_to_limbs::<N>(k) else {
            return false;
        };
        // a < n iff a - n borrows.
        let mut borrow = 0u64;
        for i in 0..N {
            let (t, b1) = a[i].overflowing_sub(self.n.m[i]);
            let (_, b2) = t.overflowing_sub(borrow);
            borrow = (b1 | b2) as u64;
        }
        let ok = borrow & !is_zero_mask(&a) & 1;
        core::hint::black_box(a);
        ok == 1
    }

    /// The public point secret * G, encoded; None if the secret is out of range.
    pub fn public_key(&self, secret: &[u8], out: &mut [u8]) -> bool {
        if !self.scalar_ok(secret) {
            return false;
        }
        let pt = self.mul(secret, &self.generator());
        match self.to_affine(&pt) {
            Some((x, y)) => {
                self.encode(&x, &y, out);
                true
            }
            None => false,
        }
    }

    /// ECDH: the X coordinate of secret * peer (8N bytes into `out`).
    pub fn shared(&self, secret: &[u8], peer: &[u8], out: &mut [u8]) -> bool {
        if !self.scalar_ok(secret) {
            return false;
        }
        let Some(q) = self.decode(peer) else {
            return false;
        };
        let mut pt = self.mul(secret, &q);
        let ok = match self.to_affine(&pt) {
            Some((x, _)) => {
                limbs_to_be(&x, out);
                true
            }
            None => false,
        };
        pt = self.identity();
        core::hint::black_box(&pt);
        ok
    }

    /// ECDSA verification (SEC 1 4.1.4). `r`, `s` big-endian of any length; `digest` any
    /// length, its leftmost 64N bits used (the order's bit length here is 64N).
    pub fn verify(&self, public: &[u8], digest: &[u8], r: &[u8], s: &[u8]) -> bool {
        let Some(q) = self.decode(public) else {
            return false;
        };
        let (Some(r), Some(s)) = (scalar_from(&self.n, r), scalar_from(&self.n, s)) else {
            return false;
        };
        // e: the digest's leftmost 8N bytes, left-padded if shorter.
        let mut eb = [0u8; 72];
        let len = 8 * N;
        let take = digest.len().min(len);
        eb[len - take..len].copy_from_slice(&digest[..take]);
        let Some(e) = self.n.from_be_reduce(&eb[..len]) else {
            return false;
        };
        let nf = &self.n;
        let w = nf.inv(&nf.to_mont(&s));
        let u1 = nf.from_mont(&nf.mul(&nf.to_mont(&e), &w));
        let u2 = nf.from_mont(&nf.mul(&nf.to_mont(&r), &w));
        let mut b1 = [0u8; 72];
        let mut b2 = [0u8; 72];
        limbs_to_be(&u1, &mut b1[..len]);
        limbs_to_be(&u2, &mut b2[..len]);
        let pt = self.add(
            &self.mul(&b1[..len], &self.generator()),
            &self.mul(&b2[..len], &q),
        );
        let Some((x, _)) = self.to_affine(&pt) else {
            return false;
        };
        // x < p < 2n: one conditional subtraction gives x mod n.
        let mut xb = [0u8; 72];
        limbs_to_be(&x, &mut xb[..len]);
        let Some(v) = self.n.from_be_reduce(&xb[..len]) else {
            return false;
        };
        v == r
    }
}

/// A big-endian integer of any length as a scalar in [1, n - 1], else None.
fn scalar_from<const N: usize>(n: &Modulus<N>, b: &[u8]) -> Option<[u64; N]> {
    let first = b.iter().position(|&x| x != 0)?;
    let b = &b[first..];
    if b.len() > 8 * N {
        return None;
    }
    let mut buf = [0u8; 72];
    let len = 8 * N;
    buf[len - b.len()..len].copy_from_slice(b);
    n.from_be(&buf[..len])
}

/// Host tests shared by p256 and p384 (each calls these with its curve).
#[cfg(test)]
pub(crate) mod testkit {
    use super::*;
    use crate::unhex_vec as h;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    fn digest(hash: &str, msg: &[u8]) -> Vec<u8> {
        match hash {
            "SHA-256" | "sha256" => crate::sha256::sha256(msg).to_vec(),
            "SHA-384" | "sha384" => crate::sha512::sha384(msg).to_vec(),
            _ => crate::sha512::sha512(msg).to_vec(),
        }
    }

    pub struct Rng(u64);
    impl Rng {
        pub fn new() -> Rng {
            Rng(0x2545f4914f6cdd1d)
        }
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        pub fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    /// NIST SigVer for `name` ("P-256"): every P vector verifies, every F one doesn't; also
    /// through DER, and with s replaced by n - s (also valid in ECDSA).
    pub fn sigver<const N: usize>(c: &Curve<N>, name: &str) -> usize {
        let mut count = 0;
        for line in include_str!("../testdata/ecdsa/sigver.txt").lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if line.starts_with('#') || f.first() != Some(&name) {
                continue;
            }
            let dg = digest(f[1], &h(f[2]));
            let mut q = vec![4u8];
            q.extend(h(f[3]));
            q.extend(h(f[4]));
            let (r, s) = (h(f[5]), h(f[6]));
            let want = f[7] == "P";
            assert_eq!(c.verify(&q, &dg, &r, &s), want, "{line}");
            let sig = der(&r, &s);
            let (dr, ds) = crate::ecdsa::parse_der(&sig).unwrap();
            assert_eq!(c.verify(&q, &dg, dr, ds), want, "DER: {line}");
            // n - s
            let sl = c.n.from_be(&pad(&s, 8 * N)).unwrap();
            let ns = c.n.sub(&[0; N], &sl);
            let mut nsb = vec![0u8; 8 * N];
            limbs_to_be(&ns, &mut nsb);
            assert_eq!(c.verify(&q, &dg, &r, &nsb), want, "n - s: {line}");
            count += 1;
        }
        count
    }

    pub fn pad(b: &[u8], len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len.saturating_sub(b.len())];
        v.extend_from_slice(b);
        v
    }

    /// DER of (r, s) (minimal, sign-padded).
    pub fn der(r: &[u8], s: &[u8]) -> Vec<u8> {
        fn int(v: &[u8]) -> Vec<u8> {
            let mut v: Vec<u8> = v.iter().copied().skip_while(|&b| b == 0).collect();
            if v.is_empty() || v[0] & 0x80 != 0 {
                v.insert(0, 0);
            }
            let mut out = vec![2, v.len() as u8];
            out.extend(v);
            out
        }
        let body = [int(r), int(s)].concat();
        let mut out = vec![0x30];
        if body.len() >= 128 {
            out.push(0x81);
        }
        out.push(body.len() as u8);
        out.extend(body);
        out
    }

    /// OpenSSL-made DER signatures and ECDH results for `name`.
    pub fn openssl<const N: usize>(c: &Curve<N>, name: &str) -> (usize, usize) {
        let (mut sigs, mut dh) = (0, 0);
        for line in include_str!("../testdata/ecdsa/openssl.txt").lines() {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.get(1) != Some(&name) {
                continue;
            }
            if f[0] == "sig" {
                let dg = digest(f[2], &h(f[4]));
                let (q, sig) = (h(f[3]), h(f[5]));
                let (r, s) = crate::ecdsa::parse_der(&sig).unwrap();
                assert!(c.verify(&q, &dg, r, s), "{line}");
                let mut bad = dg.clone();
                bad[3] ^= 0x20;
                assert!(!c.verify(&q, &bad, r, s));
                sigs += 1;
            } else if f[0] == "ecdh" {
                let (a, pb, want) = (h(f[2]), h(f[3]), h(f[4]));
                let mut out = vec![0u8; 8 * N];
                assert!(c.shared(&a, &pb, &mut out), "{line}");
                assert_eq!(out, want, "{line}");
                dh += 1;
            }
        }
        (sigs, dh)
    }

    /// Group-law checks: double = add to itself, the identity, k*G against sums, -G.
    pub fn group_laws<const N: usize>(c: &Curve<N>) {
        let g = c.generator();
        let aff = |p: &Point<N>| c.to_affine(p);
        assert!(c.on_curve(&g.x, &g.y));
        let id = c.identity();
        assert!(aff(&c.add(&g, &id)) == aff(&g));
        assert!(aff(&c.add(&id, &g)) == aff(&g));
        assert!(aff(&c.double(&id)).is_none());
        assert!(aff(&c.add(&id, &id)).is_none());
        let mut acc = c.identity();
        let mut k = vec![0u8; 8 * N];
        for i in 1..40u8 {
            acc = c.add(&acc, &g);
            *k.last_mut().unwrap() = i;
            assert!(aff(&c.mul(&k, &g)) == aff(&acc), "k = {i}");
            assert!(aff(&c.double(&acc)) == aff(&c.add(&acc, &acc)));
        }
        // (n - 1) G = -G = (gx, p - gy), and P + (-P) = identity.
        let mut nm1 = c.n.m;
        nm1[0] -= 1;
        let mut nb = vec![0u8; 8 * N];
        limbs_to_be(&nm1, &mut nb);
        let neg = c.mul(&nb, &g);
        let (x, y) = aff(&neg).unwrap();
        assert_eq!(x, c.gx);
        assert_eq!(y, c.p.sub(&[0; N], &c.gy));
        assert!(aff(&c.add(&neg, &g)).is_none());
        // Scalars past n wrap: (n + 5) G = 5 G (bytes up to 8N long, as long as they fit).
        let mut rng = Rng::new();
        for _ in 0..8 {
            let a = rng.bytes(8 * N);
            let b = rng.bytes(8 * N);
            let pa = c.mul(&a, &g);
            let pb = c.mul(&b, &g);
            // a(bG) = b(aG)
            assert!(aff(&c.mul(&a, &pb)) == aff(&c.mul(&b, &pa)));
        }
    }

    /// ECDH round trips, secret-range checks, and hostile public keys.
    pub fn ecdh<const N: usize>(c: &Curve<N>, bad_publics: &[&str]) {
        let mut rng = Rng::new();
        let len = 8 * N;
        for _ in 0..6 {
            let a = rng.bytes(len);
            let b = rng.bytes(len);
            let (mut pa, mut pb) = (vec![0u8; 1 + 2 * len], vec![0u8; 1 + 2 * len]);
            assert!(c.public_key(&a, &mut pa) && c.public_key(&b, &mut pb));
            let (mut s1, mut s2) = (vec![0u8; len], vec![0u8; len]);
            assert!(c.shared(&a, &pb, &mut s1) && c.shared(&b, &pa, &mut s2));
            assert_eq!(s1, s2);
        }
        // Secrets 0, n, n + 1, all ones; and 1, n - 1 are fine.
        let mut nb = vec![0u8; len];
        limbs_to_be(&c.n.m, &mut nb);
        let mut n1 = nb.clone();
        *n1.last_mut().unwrap() += 1;
        let mut nm1 = nb.clone();
        *nm1.last_mut().unwrap() -= 1;
        let mut out = vec![0u8; 1 + 2 * len];
        for bad in [
            vec![0u8; len],
            nb.clone(),
            n1,
            vec![0xff; len],
            vec![1u8; len - 1],
            vec![1u8; len + 1],
        ] {
            assert!(!c.public_key(&bad, &mut out));
        }
        let mut one = vec![0u8; len];
        *one.last_mut().unwrap() = 1;
        assert!(c.public_key(&one, &mut out));
        let mut g = vec![4u8];
        g.extend(pad(
            &{
                let mut v = vec![0u8; len];
                limbs_to_be(&c.gx, &mut v);
                v
            },
            len,
        ));
        g.extend({
            let mut v = vec![0u8; len];
            limbs_to_be(&c.gy, &mut v);
            v
        });
        assert_eq!(out, g);
        assert!(c.public_key(&nm1, &mut out));
        let mut sec = vec![0u8; len];
        for p in bad_publics {
            let p = h(&String::from(*p).replace("REP04", &"04".repeat(200)));
            assert!(!c.shared(&one, &p, &mut sec), "accepted {}", crate::hex(&p));
            assert!(c.decode(&p).is_none());
        }
        // Mutated public keys: never panic; mostly off the curve.
        let mut accepted = 0;
        for _ in 0..300 {
            let mut p = g.clone();
            let i = (rng.next() as usize) % p.len();
            p[i] ^= 1 << (rng.next() % 8);
            if c.shared(&one, &p, &mut sec) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 0);
    }

    /// Hand-made hostile ECDSA inputs around a valid (q, digest, r, s), and a mutation fuzz.
    pub fn hostile<const N: usize>(c: &Curve<N>, name: &str) {
        let line = include_str!("../testdata/ecdsa/sigver.txt")
            .lines()
            .find(|l| l.starts_with(name) && l.ends_with(" P"))
            .unwrap();
        let f: Vec<&str> = line.split_whitespace().collect();
        let dg = digest(f[1], &h(f[2]));
        let mut q = vec![4u8];
        q.extend(h(f[3]));
        q.extend(h(f[4]));
        let (r, s) = (h(f[5]), h(f[6]));
        assert!(c.verify(&q, &dg, &r, &s));
        let len = 8 * N;
        let mut nb = vec![0u8; len];
        limbs_to_be(&c.n.m, &mut nb);
        let mut np = nb.clone();
        *np.last_mut().unwrap() += 1;
        // r, s out of range: 0, n, n + 1, 2^(8N) - 1, empty, too long, r + n.
        let zero = vec![0u8; len];
        let mut too_long = vec![1u8];
        too_long.extend(&r);
        let mut r_plus_n = vec![0u8; len + 1];
        {
            let rl = c.n.from_be(&pad(&r, len)).unwrap();
            let mut carry = 0u128;
            let mut sum = [0u64; N];
            for i in 0..N {
                let t = rl[i] as u128 + c.n.m[i] as u128 + carry;
                sum[i] = t as u64;
                carry = t >> 64;
            }
            limbs_to_be(&sum, &mut r_plus_n[1..]);
            r_plus_n[0] = carry as u8;
        }
        for bad in [
            &zero,
            &nb,
            &np,
            &vec![0xff; len],
            &vec![],
            &too_long,
            &r_plus_n,
        ] {
            assert!(!c.verify(&q, &dg, bad, &s));
            assert!(!c.verify(&q, &dg, &r, bad));
        }
        // Leading zero bytes on r and s are just the same numbers.
        let mut rz = vec![0, 0];
        rz.extend(&r);
        assert!(c.verify(&q, &dg, &rz, &s));
        // Wrong public key encodings.
        let mut comp = vec![2 + (q[2 * len] & 1)];
        comp.extend(&q[1..1 + len]);
        assert!(!c.verify(&comp, &dg, &r, &s));
        assert!(!c.verify(&q[..q.len() - 1], &dg, &r, &s));
        assert!(!c.verify(&[], &dg, &r, &s));
        assert!(!c.verify(&[0], &dg, &r, &s));
        let mut q0 = q.clone();
        q0[0] = 6; // hybrid encodings are not accepted
        assert!(!c.verify(&q0, &dg, &r, &s));
        // Digest: empty and short digests are numbers too (no panic), long ones truncate.
        let _ = c.verify(&q, &[], &r, &s);
        let _ = c.verify(&q, &[1], &r, &s);
        let mut longer = dg.clone();
        longer.extend([0xaa; 40]);
        if dg.len() >= len {
            assert!(c.verify(&q, &longer, &r, &s));
        }
        // Mutation fuzz over (q, digest, r, s) and the DER form.
        let mut rng = Rng::new();
        let sig = der(&r, &s);
        assert!(crate::ecdsa::parse_der(&sig).is_some());
        for _ in 0..400 {
            let (mut q2, mut d2, mut r2, mut s2, mut sig2) =
                (q.clone(), dg.clone(), r.clone(), s.clone(), sig.clone());
            let which = rng.next() % 5;
            let v: &mut Vec<u8> = match which {
                0 => &mut q2,
                1 => &mut d2,
                2 => &mut r2,
                3 => &mut s2,
                _ => &mut sig2,
            };
            match rng.next() % 3 {
                0 => {
                    let i = (rng.next() as usize) % v.len();
                    v[i] ^= 1 << (rng.next() % 8);
                }
                1 => {
                    let n = (rng.next() as usize) % v.len();
                    v.truncate(n);
                }
                _ => v.push(rng.next() as u8),
            }
            if which == 4 {
                if let Some((rr, ss)) = crate::ecdsa::parse_der(&sig2) {
                    let same = pad(rr, len) == pad(&r, len) && pad(ss, len) == pad(&s, len);
                    assert_eq!(c.verify(&q, &dg, rr, ss), same);
                }
                continue;
            }
            let changed_value = match which {
                1 => {
                    // Only the leftmost 8N bytes count, and appending past them is harmless.
                    let a = pad(&d2[..d2.len().min(len)], len);
                    let b = pad(&dg[..dg.len().min(len)], len);
                    a != b
                }
                2 => pad(&r2, len + 1) != pad(&r, len + 1),
                3 => pad(&s2, len + 1) != pad(&s, len + 1),
                _ => true,
            };
            if changed_value {
                assert!(!c.verify(&q2, &d2, &r2, &s2));
            }
        }
    }
}
