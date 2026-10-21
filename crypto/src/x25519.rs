//! X25519 Diffie-Hellman (RFC 7748): the Montgomery ladder over GF(2^255 - 19) with a
//! constant-time swap, a fixed 255 iterations, no secret-dependent branches.
//! [`selftest`]: RFC 7748 5.2 (both vectors), 6.1 (Alice and Bob), and the iterated test
//! after 1 and 1000 rounds.

use crate::field25519::Fe;

pub const BASE: [u8; 32] = {
    let mut b = [0u8; 32];
    b[0] = 9;
    b
};

/// scalar * u. `k` is clamped here; the top bit of `u` is ignored (RFC 7748 5).
pub fn x25519(k: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut s = *k;
    s[0] &= 248;
    s[31] &= 127;
    s[31] |= 64;
    let x1 = Fe::from_bytes(u);
    let (mut x2, mut z2, mut x3, mut z3) = (Fe::ONE, Fe::ZERO, x1, Fe::ONE);
    let a24 = Fe::small(121_665);
    let mut swap = 0u64;
    for t in (0..255).rev() {
        let bit = ((s[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= bit;
        Fe::cswap(&mut x2, &mut x3, swap);
        Fe::cswap(&mut z2, &mut z3, swap);
        swap = bit;
        let a = x2.add(z2);
        let aa = a.sq();
        let b = x2.sub(z2);
        let bb = b.sq();
        let e = aa.sub(bb);
        let c = x3.add(z3);
        let d = x3.sub(z3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x3 = da.add(cb).sq();
        z3 = x1.mul(da.sub(cb).sq());
        x2 = aa.mul(bb);
        z2 = e.mul(aa.add(a24.mul(e)));
    }
    Fe::cswap(&mut x2, &mut x3, swap);
    Fe::cswap(&mut z2, &mut z3, swap);
    crate::wipe(&mut s);
    x2.mul(z2.invert()).to_bytes()
}

/// The public key of a secret scalar.
pub fn public_key(k: &[u8; 32]) -> [u8; 32] {
    x25519(k, &BASE)
}

/// A shared secret, or None if it came out all zero (the peer sent a low-order point).
pub fn shared(k: &[u8; 32], peer: &[u8; 32]) -> Option<[u8; 32]> {
    let s = x25519(k, peer);
    (!crate::ct_eq(&s, &[0u8; 32])).then_some(s)
}

pub fn selftest() -> (usize, usize) {
    let h = |s: &str| crate::unhex::<32>(s).unwrap();
    let mut ok = 0;
    // 5.2
    ok += (x25519(
        &h("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4"),
        &h("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c"),
    ) == h("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552")) as usize;
    ok += (x25519(
        &h("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d"),
        &h("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493"),
    ) == h("95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957")) as usize;
    // 6.1
    let a = h("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    let b = h("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    let (pa, pb) = (public_key(&a), public_key(&b));
    ok += (pa == h("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")) as usize;
    ok += (pb == h("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f")) as usize;
    let k = h("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    ok += (shared(&a, &pb) == Some(k) && shared(&b, &pa) == Some(k)) as usize;
    // 5.2 iterated: k = u = 9; k, u = x25519(k, u), k.
    let (mut kk, mut uu) = (BASE, BASE);
    let mut after1 = [0u8; 32];
    for i in 0..1000 {
        let r = x25519(&kk, &uu);
        uu = kk;
        kk = r;
        if i == 0 {
            after1 = kk;
        }
    }
    ok +=
        (after1 == h("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079")) as usize;
    ok += (kk == h("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51")) as usize;
    // A low-order point gives no shared secret.
    ok += shared(&a, &[0u8; 32]).is_none() as usize;
    (ok, 8)
}
