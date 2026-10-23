//! Unsigned big integers for public-key **verification** ([`crate::rsa`]): parse from and
//! to big-endian bytes, compare, and modular exponentiation with Montgomery multiplication
//! ([`Mont`]). Variable time (only public values go through here: moduli, public exponents,
//! signatures), but never panics: every length is checked and every operation is defined
//! for any input it accepts.

#![allow(clippy::needless_range_loop)]

use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// A non-negative integer, little-endian 64-bit limbs, no high zero limbs (zero is empty).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct BigUint {
    limbs: Vec<u64>,
}

impl BigUint {
    pub fn zero() -> BigUint {
        BigUint { limbs: Vec::new() }
    }

    pub fn from_u64(v: u64) -> BigUint {
        let mut b = BigUint { limbs: vec![v] };
        b.normalize();
        b
    }

    /// From big-endian bytes (leading zeros allowed).
    pub fn from_be_bytes(b: &[u8]) -> BigUint {
        let mut limbs = Vec::with_capacity(b.len().div_ceil(8));
        for chunk in b.rchunks(8) {
            let mut w = [0u8; 8];
            w[8 - chunk.len()..].copy_from_slice(chunk);
            limbs.push(u64::from_be_bytes(w));
        }
        let mut x = BigUint { limbs };
        x.normalize();
        x
    }

    /// Big-endian bytes, left-padded with zeros to exactly `len`; None if it doesn't fit.
    pub fn to_be_bytes(&self, len: usize) -> Option<Vec<u8>> {
        if self.byte_len() > len {
            return None;
        }
        let mut out = vec![0u8; len];
        for (i, l) in self.limbs.iter().enumerate() {
            for (j, byte) in l.to_le_bytes().iter().enumerate() {
                let pos = i * 8 + j; // byte index from the least significant end
                if pos < len {
                    out[len - 1 - pos] = *byte;
                }
            }
        }
        Some(out)
    }

    fn normalize(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub fn is_odd(&self) -> bool {
        self.limbs.first().is_some_and(|l| l & 1 == 1)
    }

    /// Number of significant bits (0 for zero).
    pub fn bits(&self) -> usize {
        match self.limbs.last() {
            Some(&top) => self.limbs.len() * 64 - top.leading_zeros() as usize,
            None => 0,
        }
    }

    pub fn byte_len(&self) -> usize {
        self.bits().div_ceil(8)
    }

    /// Bit `i` (0 = least significant).
    pub fn bit(&self, i: usize) -> bool {
        self.limbs
            .get(i / 64)
            .is_some_and(|l| (l >> (i % 64)) & 1 == 1)
    }

    pub fn limbs(&self) -> &[u64] {
        &self.limbs
    }
}

impl PartialOrd for BigUint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BigUint {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.limbs.len().cmp(&other.limbs.len()) {
            Ordering::Equal => {}
            o => return o,
        }
        for (a, b) in self.limbs.iter().rev().zip(other.limbs.iter().rev()) {
            match a.cmp(b) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    }
}

/// Montgomery arithmetic modulo an odd m > 1: [`Mont::pow`] computes base^exp mod m.
pub struct Mont {
    m: Vec<u64>,
    /// -m^-1 mod 2^64.
    n0: u64,
    /// R^2 mod m, R = 2^(64k).
    r2: Vec<u64>,
}

impl Mont {
    /// None unless m is odd and > 1.
    pub fn new(m: &BigUint) -> Option<Mont> {
        if !m.is_odd() || m.bits() < 2 {
            return None;
        }
        let k = m.limbs.len();
        let mut inv: u64 = 1;
        for _ in 0..6 {
            inv = inv.wrapping_mul(2u64.wrapping_sub(m.limbs[0].wrapping_mul(inv)));
        }
        // R^2 mod m by doubling 1, 128k times.
        let mut x = vec![0u64; k];
        x[0] = 1;
        for _ in 0..128 * k {
            double_mod(&mut x, &m.limbs);
        }
        Some(Mont {
            m: m.limbs.clone(),
            n0: inv.wrapping_neg(),
            r2: x,
        })
    }

    fn k(&self) -> usize {
        self.m.len()
    }

    /// a * b / R mod m, for a, b < m (k limbs each).
    fn mul(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let k = self.k();
        let mut t = vec![0u64; k + 2];
        for i in 0..k {
            let bi = b[i] as u128;
            let mut c: u64 = 0;
            for j in 0..k {
                let x = t[j] as u128 + (a[j] as u128) * bi + c as u128;
                t[j] = x as u64;
                c = (x >> 64) as u64;
            }
            let x = t[k] as u128 + c as u128;
            t[k] = x as u64;
            t[k + 1] = (x >> 64) as u64;
            let mm = t[0].wrapping_mul(self.n0) as u128;
            let x = t[0] as u128 + mm * (self.m[0] as u128);
            let mut c = (x >> 64) as u64;
            for j in 1..k {
                let x = t[j] as u128 + mm * (self.m[j] as u128) + c as u128;
                t[j - 1] = x as u64;
                c = (x >> 64) as u64;
            }
            let x = t[k] as u128 + c as u128;
            t[k - 1] = x as u64;
            t[k] = t[k + 1].wrapping_add((x >> 64) as u64);
        }
        let hi = t[k];
        t.truncate(k);
        if hi != 0 || !lt(&t, &self.m) {
            sub_in_place(&mut t, &self.m);
        }
        t
    }

    /// base^exp mod m. `base` must be < m (else None).
    pub fn pow(&self, base: &BigUint, exp: &BigUint) -> Option<BigUint> {
        let k = self.k();
        if base.limbs.len() > k || !lt(&pad(&base.limbs, k), &self.m) {
            return None;
        }
        let b = self.mul(&pad(&base.limbs, k), &self.r2);
        // 1 in Montgomery form: R mod m = mul(1, R^2).
        let mut one = vec![0u64; k];
        one[0] = 1;
        let mut acc = self.mul(&one, &self.r2);
        for i in (0..exp.bits()).rev() {
            acc = self.mul(&acc, &acc);
            if exp.bit(i) {
                acc = self.mul(&acc, &b);
            }
        }
        let mut r = BigUint {
            limbs: self.mul(&acc, &one),
        };
        r.normalize();
        Some(r)
    }
}

fn pad(a: &[u64], k: usize) -> Vec<u64> {
    let mut v = a.to_vec();
    v.resize(k, 0);
    v
}

/// a < b for equal-length limb slices.
fn lt(a: &[u64], b: &[u64]) -> bool {
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        if x != y {
            return x < y;
        }
    }
    false
}

/// a -= b (equal lengths); the final borrow is dropped (callers ensure a >= b, or that the
/// true value had an extra high bit).
fn sub_in_place(a: &mut [u64], b: &[u64]) {
    let mut borrow = 0u64;
    for (x, y) in a.iter_mut().zip(b) {
        let (t, b1) = x.overflowing_sub(*y);
        let (t, b2) = t.overflowing_sub(borrow);
        *x = t;
        borrow = (b1 | b2) as u64;
    }
}

/// x = 2x mod m (x < m, equal lengths).
fn double_mod(x: &mut [u64], m: &[u64]) {
    let mut carry = 0u64;
    for l in x.iter_mut() {
        let hi = *l >> 63;
        *l = (*l << 1) | carry;
        carry = hi;
    }
    if carry == 1 || !lt(x, m) {
        sub_in_place(x, m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(h: &str) -> BigUint {
        BigUint::from_be_bytes(&crate::unhex_vec(h))
    }

    #[test]
    fn bytes_round_trip() {
        let x = b("00000102030405060708090a0b0c0d0e0f10");
        assert_eq!(x.bits(), 8 * 15 + 1);
        assert_eq!(
            crate::hex(&x.to_be_bytes(16).unwrap()),
            "0102030405060708090a0b0c0d0e0f10"
        );
        assert_eq!(x.to_be_bytes(15), None);
        assert_eq!(crate::hex(&x.to_be_bytes(18).unwrap())[..8], *"00000102");
        assert!(BigUint::from_be_bytes(&[0, 0]).is_zero());
        assert_eq!(BigUint::zero().to_be_bytes(0), Some(vec![]));
        assert!(b("05") < b("0100"));
    }

    #[test]
    fn small_pow() {
        let m = Mont::new(&BigUint::from_u64(1_000_000_007)).unwrap();
        // 3^(10^9+6) = 1 (Fermat), 2^10 = 1024, 7^0 = 1.
        assert_eq!(
            m.pow(&BigUint::from_u64(3), &BigUint::from_u64(1_000_000_006)),
            Some(BigUint::from_u64(1))
        );
        assert_eq!(
            m.pow(&BigUint::from_u64(2), &BigUint::from_u64(10)),
            Some(BigUint::from_u64(1024))
        );
        assert_eq!(
            m.pow(&BigUint::from_u64(7), &BigUint::zero()),
            Some(BigUint::from_u64(1))
        );
        assert_eq!(
            m.pow(&BigUint::zero(), &BigUint::from_u64(5)),
            Some(BigUint::zero())
        );
        // base >= m is refused.
        assert_eq!(
            m.pow(&BigUint::from_u64(1_000_000_007), &BigUint::from_u64(2)),
            None
        );
        assert!(Mont::new(&BigUint::from_u64(10)).is_none());
        assert!(Mont::new(&BigUint::from_u64(1)).is_none());
        assert!(Mont::new(&BigUint::zero()).is_none());
    }

    #[test]
    fn big_pow_against_known() {
        // Mersenne prime 2^127 - 1: a^(p-1) = 1 and a^p = a.
        let p = b("7fffffffffffffffffffffffffffffff");
        let m = Mont::new(&p).unwrap();
        let a = b("123456789abcdef0fedcba9876543210");
        let pm1 = b("7ffffffffffffffffffffffffffffffe");
        assert_eq!(m.pow(&a, &pm1), Some(BigUint::from_u64(1)));
        assert_eq!(m.pow(&a, &p), Some(a.clone()));
        // A modulus whose top limb is full (2^192 - 2^64 - 1, prime): Fermat again.
        let p = b("fffffffffffffffffffffffffffffffeffffffffffffffff");
        let m = Mont::new(&p).unwrap();
        let pm1 = b("fffffffffffffffffffffffffffffffefffffffffffffffe");
        assert_eq!(m.pow(&a, &pm1), Some(BigUint::from_u64(1)));
    }
}
