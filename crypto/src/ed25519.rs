//! Ed25519 (RFC 8032), from scratch: key derivation, signing and verification.
//!
//! Field GF(2^255 - 19) in five 51-bit limbs (u128 products); points in extended twisted
//! Edwards coordinates (X:Y:Z:T) with the RFC's unified addition (also used for doubling);
//! scalars mod L by shift-and-subtract. Constants (d, sqrt(-1), the base point) are derived
//! at run time from their definitions rather than typed in.
//!
//! Not constant-time: fine for verifying packages (public data) and for the test vectors;
//! don't sign with secrets an attacker can time.
//! [`selftest`] runs the RFC 8032 section 7.1 vectors (TEST 1, 2, 3, SHA(abc)).

use crate::sha512::{Sha512, sha512};

use crate::field25519::Fe;

struct Consts {
    d: Fe,
    d2: Fe,
    sqrt_m1: Fe,
    base: Point,
}

fn consts() -> Consts {
    let d = Fe::small(121665).neg().mul(Fe::small(121666).invert());
    // sqrt(-1) = 2^((p-1)/4), (p-1)/4 = 2^253 - 5
    let mut e = [0xffu8; 32];
    e[0] = 0xfb;
    e[31] = 0x1f;
    let sqrt_m1 = Fe::small(2).pow(&e);
    let mut c = Consts {
        d,
        d2: d.add(d),
        sqrt_m1,
        base: Point::IDENTITY,
    };
    // B: y = 4/5, x even.
    let y = Fe::small(4).mul(Fe::small(5).invert());
    c.base = Point::decode(&y.to_bytes(), &c).expect("base point");
    c
}

#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

impl Point {
    const IDENTITY: Point = Point {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
        t: Fe::ZERO,
    };

    /// RFC 8032 5.1.4 (complete for a = -1; also doubles).
    fn add(&self, q: &Point, c: &Consts) -> Point {
        let a = self.y.sub(self.x).mul(q.y.sub(q.x));
        let b = self.y.add(self.x).mul(q.y.add(q.x));
        let cc = self.t.mul(c.d2).mul(q.t);
        let d = self.z.add(self.z).mul(q.z);
        let (e, f, g, h) = (b.sub(a), d.sub(cc), d.add(cc), b.add(a));
        Point {
            x: e.mul(f),
            y: g.mul(h),
            t: e.mul(h),
            z: f.mul(g),
        }
    }

    /// [k]P, k little-endian bytes (256 bits, double-and-add).
    fn mul(&self, k: &[u8; 32], c: &Consts) -> Point {
        let mut r = Point::IDENTITY;
        for i in (0..256).rev() {
            r = r.add(&r, c);
            if k[i / 8] >> (i % 8) & 1 == 1 {
                r = r.add(self, c);
            }
        }
        r
    }

    fn encode(&self) -> [u8; 32] {
        let zi = self.z.invert();
        let x = self.x.mul(zi);
        let y = self.y.mul(zi);
        let mut b = y.to_bytes();
        b[31] |= (x.is_negative() as u8) << 7;
        b
    }

    /// RFC 8032 5.1.3.
    fn decode(b: &[u8; 32], c: &Consts) -> Option<Point> {
        let sign = b[31] >> 7;
        let mut yb = *b;
        yb[31] &= 0x7f;
        let y = Fe::from_bytes(&yb);
        if y.to_bytes() != yb {
            return None; // y >= p
        }
        let y2 = y.sq();
        let u = y2.sub(Fe::ONE);
        let v = c.d.mul(y2).add(Fe::ONE);
        // x = u v^3 (u v^7)^((p-5)/8), (p-5)/8 = 2^252 - 3
        let mut e = [0xffu8; 32];
        e[0] = 0xfd;
        e[31] = 0x0f;
        let v3 = v.sq().mul(v);
        let v7 = v3.sq().mul(v);
        let mut x = u.mul(v3).mul(u.mul(v7).pow(&e));
        let vx2 = v.mul(x.sq());
        if vx2.eq(u) {
        } else if vx2.eq(u.neg()) {
            x = x.mul(c.sqrt_m1);
        } else {
            return None;
        }
        let zero = x.eq(Fe::ZERO);
        if zero && sign == 1 {
            return None;
        }
        if x.is_negative() != (sign == 1) {
            x = x.neg();
        }
        Some(Point {
            x,
            y,
            z: Fe::ONE,
            t: x.mul(y),
        })
    }
}

// ---- scalars mod L ------------------------------------------------------------------------

/// L = 2^252 + 27742317777372353535851937790883648493, as four 64-bit LE words.
const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0x0000000000000000,
    0x1000000000000000,
];

fn ge_l(r: &[u64; 5]) -> bool {
    if r[4] != 0 {
        return true;
    }
    for i in (0..4).rev() {
        if r[i] != L[i] {
            return r[i] > L[i];
        }
    }
    true
}

fn sub_l(r: &mut [u64; 5]) {
    let mut borrow = 0u64;
    for i in 0..5 {
        let li = if i < 4 { L[i] } else { 0 };
        let (a, b1) = r[i].overflowing_sub(li);
        let (a, b2) = a.overflowing_sub(borrow);
        r[i] = a;
        borrow = (b1 | b2) as u64;
    }
}

/// A little-endian integer of any length mod L (bit by bit: r = 2r + bit, minus L).
fn reduce(bytes: &[u8]) -> [u8; 32] {
    let mut r = [0u64; 5];
    for i in (0..bytes.len() * 8).rev() {
        let bit = (bytes[i / 8] >> (i % 8) & 1) as u64;
        let mut carry = bit;
        for w in r.iter_mut() {
            let nc = *w >> 63;
            *w = (*w << 1) | carry;
            carry = nc;
        }
        if ge_l(&r) {
            sub_l(&mut r);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[8 * i..8 * i + 8].copy_from_slice(&r[i].to_le_bytes());
    }
    out
}

/// (a * b + c) mod L, all little-endian 32-byte scalars.
fn mul_add(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let w = |x: &[u8; 32], i: usize| u64::from_le_bytes(x[8 * i..8 * i + 8].try_into().unwrap());
    let mut p = [0u128; 9];
    for i in 0..4 {
        for j in 0..4 {
            let m = w(a, i) as u128 * w(b, j) as u128;
            p[i + j] += m & 0xffff_ffff_ffff_ffff;
            p[i + j + 1] += m >> 64;
        }
    }
    for (i, v) in p.iter_mut().take(4).enumerate() {
        *v += w(c, i) as u128;
    }
    let mut out = [0u8; 72];
    let mut carry = 0u128;
    for (i, v) in p.iter().enumerate() {
        let s = *v + carry;
        out[8 * i..8 * i + 8].copy_from_slice(&(s as u64).to_le_bytes());
        carry = s >> 64;
    }
    reduce(&out)
}

fn lt_l(s: &[u8; 32]) -> bool {
    let mut r = [0u64; 5];
    for (i, w) in r.iter_mut().take(4).enumerate() {
        *w = u64::from_le_bytes(s[8 * i..8 * i + 8].try_into().unwrap());
    }
    !ge_l(&r)
}

// ---- the API ------------------------------------------------------------------------------

fn expand(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let h = sha512(seed);
    let mut s: [u8; 32] = h[..32].try_into().unwrap();
    s[0] &= 248;
    s[31] &= 127;
    s[31] |= 64;
    (s, h[32..].try_into().unwrap())
}

/// The public key of a 32-byte secret seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let c = consts();
    let (s, _) = expand(seed);
    c.base.mul(&s, &c).encode()
}

/// Sign `msg` (RFC 8032 5.1.6).
pub fn sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let c = consts();
    let (s, prefix) = expand(seed);
    let a = c.base.mul(&s, &c).encode();
    let r = reduce(&Sha512::new().update(&prefix).update(msg).finish());
    let rp = c.base.mul(&r, &c).encode();
    let k = reduce(&Sha512::new().update(&rp).update(&a).update(msg).finish());
    let ss = mul_add(&k, &s, &r);
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&rp);
    sig[32..].copy_from_slice(&ss);
    sig
}

/// Verify (RFC 8032 5.1.7, cofactorless: [S]B == R + [k]A).
pub fn verify(public: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let c = consts();
    let rb: [u8; 32] = sig[..32].try_into().unwrap();
    let sb: [u8; 32] = sig[32..].try_into().unwrap();
    if !lt_l(&sb) {
        return false;
    }
    let (Some(a), Some(r)) = (Point::decode(public, &c), Point::decode(&rb, &c)) else {
        return false;
    };
    let k = reduce(
        &Sha512::new()
            .update(&rb)
            .update(public)
            .update(msg)
            .finish(),
    );
    let lhs = c.base.mul(&sb, &c);
    let rhs = r.add(&a.mul(&k, &c), &c);
    lhs.encode() == rhs.encode()
}

pub use crate::{unhex, unhex_vec};

/// RFC 8032 7.1: (secret, public, message, signature). Returns (passed checks, total):
/// for each vector, public key derivation, signing and verifying, plus a rejected tamper.
pub fn selftest() -> (usize, usize) {
    const V: [(&str, &str, &str, &str); 4] = [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
        (
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            "af82",
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        ),
        (
            "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
            "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
        ),
    ];
    let mut ok = 0;
    let mut n = 0;
    for (sk, pk, msg, sig) in V {
        let sk: [u8; 32] = unhex(sk).unwrap();
        let pk: [u8; 32] = unhex(pk).unwrap();
        let sig: [u8; 64] = unhex(sig).unwrap();
        let msg = unhex_vec(msg);
        n += 4;
        ok += (public_key(&sk) == pk) as usize;
        ok += (sign(&sk, &msg) == sig) as usize;
        ok += verify(&pk, &msg, &sig) as usize;
        let mut bad = sig;
        bad[5] ^= 1;
        ok += (!verify(&pk, &msg, &bad)) as usize;
    }
    (ok, n)
}
