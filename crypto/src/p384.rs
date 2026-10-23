//! NIST P-384 (secp384r1, FIPS 186-4 D.1.2.4): ECDSA signature verification ([`verify`],
//! [`verify_der`]), for certificates signed with P-384 keys, and constant-time ECDH
//! ([`public_key`], [`shared`]) on the same code as P-256 (the private `nist` module).
//! [`selftest`]: n * G is the identity, a NIST CAVS ECC CDH vector, and two NIST CAVP
//! SigVer vectors (one good, one bad); the host tests run the SigVer P-384 sets.

use crate::nist::Curve;

static CURVE: Curve<6> = Curve::new(
    "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
    "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
    "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
    "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
    "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f",
);

/// Bytes in a scalar, a coordinate or a shared secret.
pub const SCALAR_LEN: usize = 48;
/// Bytes in an uncompressed public key, 0x04 || X || Y.
pub const PUBLIC_LEN: usize = 97;

/// The public key (uncompressed) of a secret scalar; None if the secret is 0 or >= n.
pub fn public_key(secret: &[u8; 48]) -> Option<[u8; 97]> {
    let mut out = [0u8; 97];
    CURVE.public_key(secret, &mut out).then_some(out)
}

/// ECDH: the X coordinate of secret * peer (see [`crate::p256::shared`]).
pub fn shared(secret: &[u8; 48], peer: &[u8]) -> Option<[u8; 48]> {
    let mut out = [0u8; 48];
    CURVE.shared(secret, peer, &mut out).then_some(out)
}

/// ECDSA verification: `public` uncompressed (97 bytes), `r` and `s` big-endian of any
/// length (each in [1, n - 1]), `digest` the message hash (leftmost 384 bits used).
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
    let g = CURVE.generator();
    let n_be = h(
        "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
    );
    ok += CURVE.to_affine(&CURVE.mul(&n_be, &g)).is_none() as usize;
    // NIST CAVS 14.1 ECC CDH Primitive (SP 800-56A), P-384 count 0.
    let d = crate::unhex::<48>("3cc3122a68f0d95027ad38c067916ba0eb8c38894d22e1b15618b6818a661774ad463b205da88cf699ab4d43c9cf98a1").unwrap();
    let ours = h(
        "049803807f2f6d2fd966cdd0290bd410c0190352fbec7ff6247de1302df86f25d34fe4a97bef60cff548355c015dbb3e5fba26ca69ec2f5b5d9dad20cc9da711383a9dbe34ea3fa5a2af75b46502629ad54dd8b7d73a8abb06a3a3be47d650cc99",
    );
    let peer = h(
        "04a7c76b970c3b5fe8b05d2838ae04ab47697b9eaf52e764592efda27fe7513272734466b400091adbf2d68c58e0c50066ac68f19f2e1cb879aed43a9969b91a0839c4c38a49749b661efedf243451915ed0905a32b060992b468c64766fc8437a",
    );
    ok += (public_key(&d).map(|p| p.to_vec()) == Some(ours)) as usize;
    ok += (shared(&d, &peer).map(|s| s.to_vec())
        == Some(h(
            "5f9d29dc5e31a163060356213669c8ce132e22f57c9a04f40ba7fcead493b457e5621e766c40a2e3d4d6a04b25e533f1",
        ))) as usize;
    // NIST CAVP SigVer [P-384,SHA-384]: a valid signature, then the same with S changed.
    let msg = h(
        "9dd789ea25c04745d57a381f22de01fb0abd3c72dbdefd44e43213c189583eef85ba662044da3de2dd8670e6325154480155bbeebb702c75781ac32e13941860cb576fe37a05b757da5b5b418f6dd7c30b042e40f4395a342ae4dce05634c33625e2bc524345481f7e253d9551266823771b251705b4a85166022a37ac28f1bd",
    );
    let q = h(
        "04cb908b1fd516a57b8ee1e14383579b33cb154fece20c5035e2b3765195d1951d75bd78fb23e00fef37d7d064fd9af144cd99c46b5857401ddcff2cf7cf822121faf1cbad9a011bed8c551f6f59b2c360f79bfbe32adbcaa09583bdfdf7c374bb",
    );
    let r = h(
        "33f64fb65cd6a8918523f23aea0bbcf56bba1daca7aff817c8791dc92428d605ac629de2e847d43cee55ba9e4a0e83ba",
    );
    let mut s = h(
        "4428bb478a43ac73ecd6de51ddf7c28ff3c2441625a081714337dd44fea8011bae71959a10947b6ea33f77e128d3c6ae",
    );
    let dg = crate::sha512::sha384(&msg);
    ok += verify(&q, &dg, &r, &s) as usize;
    s[47] ^= 1;
    ok += !verify(&q, &dg, &r, &s) as usize;
    (ok, 5)
}

#[cfg(test)]
mod tests {
    use super::CURVE;
    use crate::nist::testkit;

    #[test]
    fn nist_sigver() {
        assert_eq!(testkit::sigver(&CURVE, "P-384"), 45);
    }

    #[test]
    fn openssl_vectors() {
        assert_eq!(testkit::openssl(&CURVE, "P-384"), (6, 3));
    }

    #[test]
    fn group_laws() {
        testkit::group_laws(&CURVE);
    }

    #[test]
    fn ecdh() {
        // Hostile public keys, the P-384 list from Go's crypto/ecdh tests.
        testkit::ecdh(
            &CURVE,
            &[
                "",
                "04",
                "REP04",
                "00",
                "03aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
                "0208d999057ba3d2d969260045c55b97f089025959a6f434d651d207d19fb96e9e4fe0e86ebe0e64f85b96a9c75295df61",
                "04aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab73617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e60",
                "04000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
                "04fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff000000000000000100000001732152442fb6ee5c3e6ce1d920c059bc623563814d79042b903ce60f1d4487fccd450a86da03f3e6ed525d02017bfdb3",
            ],
        );
    }

    #[test]
    fn hostile_signatures() {
        testkit::hostile(&CURVE, "P-384");
    }

    #[test]
    fn selftest_passes() {
        assert_eq!(super::selftest(), (5, 5));
    }
}
