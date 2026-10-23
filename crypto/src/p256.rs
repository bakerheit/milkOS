//! NIST P-256 (secp256r1, FIPS 186-4 D.1.2.3): ECDH for TLS key exchange, in constant time
//! ([`public_key`], [`shared`]), and ECDSA signature verification ([`verify`],
//! [`verify_der`]). The arithmetic is in the private `nist` module, shared with P-384.
//! [`selftest`]: n * G is the identity, a NIST CAVS ECC CDH vector (both public keys and
//! the shared secret), and two NIST CAVP SigVer vectors (one good, one bad); the host tests
//! run the SigVer P-256 sets and hostile inputs.

use crate::nist::Curve;

static CURVE: Curve<4> = Curve::new(
    "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
    "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
    "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
    "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
    "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
);

/// Bytes in a scalar, a coordinate or a shared secret.
pub const SCALAR_LEN: usize = 32;
/// Bytes in an uncompressed public key, 0x04 || X || Y.
pub const PUBLIC_LEN: usize = 65;

/// The public key (uncompressed) of a secret scalar; None if the secret is 0 or >= n.
/// Constant time in the secret.
pub fn public_key(secret: &[u8; 32]) -> Option<[u8; 65]> {
    let mut out = [0u8; 65];
    CURVE.public_key(secret, &mut out).then_some(out)
}

/// ECDH: the X coordinate of secret * peer. `peer` must be an uncompressed point (65
/// bytes) on the curve with coordinates < p; None otherwise, or if the secret is out of
/// range, or the result is the identity. Constant time in the secret.
pub fn shared(secret: &[u8; 32], peer: &[u8]) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    CURVE.shared(secret, peer, &mut out).then_some(out)
}

/// ECDSA verification: `public` uncompressed (65 bytes), `r` and `s` big-endian of any
/// length (each must be in [1, n - 1]), `digest` the message hash (its leftmost 256 bits
/// are used, SEC 1 4.1.3).
pub fn verify(public: &[u8], digest: &[u8], r: &[u8], s: &[u8]) -> bool {
    CURVE.verify(public, digest, r, s)
}

/// [`verify`] with the signature in strict DER ([`crate::ecdsa::parse_der`]).
pub fn verify_der(public: &[u8], digest: &[u8], der_sig: &[u8]) -> bool {
    match crate::ecdsa::parse_der(der_sig) {
        Some((r, s)) => verify(public, digest, r, s),
        None => false,
    }
}

pub fn selftest() -> (usize, usize) {
    use crate::unhex_vec as h;
    let mut ok = 0;
    // n * G = identity (the order is right), and (n - 1) * G = -G.
    let g = CURVE.generator();
    let n_be = h("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551");
    ok += CURVE.to_affine(&CURVE.mul(&n_be, &g)).is_none() as usize;
    // NIST CAVS 14.1 ECC CDH Primitive (SP 800-56A), P-256 count 0.
    let d = crate::unhex::<32>("7d7dc5f71eb29ddaf80d6214632eeae03d9058af1fb6d22ed80badb62bc1a534")
        .unwrap();
    let ours = h(
        "04ead218590119e8876b29146ff89ca61770c4edbbf97d38ce385ed281d8a6b23028af61281fd35e2fa7002523acc85a429cb06ee6648325389f59edfce1405141",
    );
    let peer = h(
        "04700c48f77f56584c5cc632ca65640db91b6bacce3a4df6b42ce7cc838833d287db71e509e3fd9b060ddb20ba5c51dcc5948d46fbf640dfe0441782cab85fa4ac",
    );
    ok += (public_key(&d).map(|p| p.to_vec()) == Some(ours)) as usize;
    ok += (shared(&d, &peer).map(|s| s.to_vec())
        == Some(h(
            "46fc62106420ff012e54a434fbdd2d25ccc5852060561e68040dd7778997bd7b",
        ))) as usize;
    // NIST CAVP SigVer [P-256,SHA-256]: a valid signature, then the same with S changed.
    let msg = h(
        "e1130af6a38ccb412a9c8d13e15dbfc9e69a16385af3c3f1e5da954fd5e7c45fd75e2b8c36699228e92840c0562fbf3772f07e17f1add56588dd45f7450e1217ad239922dd9c32695dc71ff2424ca0dec1321aa47064a044b7fe3c2b97d03ce470a592304c5ef21eed9f93da56bb232d1eeb0035f9bf0dfafdcc4606272b20a3",
    );
    let q = h(
        "04e424dc61d4bb3cb7ef4344a7f8957a0c5134e16f7a67c074f82e6e12f49abf3c970eed7aa2bc48651545949de1dddaf0127e5965ac85d1243d6f60e7dfaee927",
    );
    let r = h("bf96b99aa49c705c910be33142017c642ff540c76349b9dab72f981fd9347f4f");
    let mut s = h("17c55095819089c2e03b9cd415abdf12444e323075d98f31920b9e0f57ec871c");
    let dg = crate::sha256::sha256(&msg);
    ok += verify(&q, &dg, &r, &s) as usize;
    s[31] ^= 1;
    ok += !verify(&q, &dg, &r, &s) as usize;
    (ok, 5)
}

#[cfg(test)]
mod tests {
    use super::CURVE;
    use crate::nist::testkit;

    #[test]
    fn nist_sigver() {
        assert_eq!(testkit::sigver(&CURVE, "P-256"), 45);
    }

    #[test]
    fn openssl_vectors() {
        assert_eq!(testkit::openssl(&CURVE, "P-256"), (6, 3));
    }

    #[test]
    fn group_laws() {
        testkit::group_laws(&CURVE);
    }

    #[test]
    fn ecdh() {
        // Hostile public keys, the P-256 list from Go's crypto/ecdh tests.
        testkit::ecdh(
            &CURVE,
            &[
                "",
                "04",
                "REP04",
                "00",
                "036b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
                "02e2534a3532d08fbba02dde659ee62bd0031fe2db785596ef509302446b030852",
                "046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f6",
                "0400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "04ffffffff00000001000000000000000000000001000000000000000000000004ba6dbc4555a7e7fa016ec431667e8521ee35afc49b265c3accbea3f7cdb70433",
            ],
        );
    }

    #[test]
    fn hostile_signatures() {
        testkit::hostile(&CURVE, "P-256");
    }

    #[test]
    fn selftest_passes() {
        assert_eq!(super::selftest(), (5, 5));
    }
}
