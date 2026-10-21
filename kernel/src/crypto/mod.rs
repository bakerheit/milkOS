//! Cryptography, from scratch, shared by the kernel (milkfs encryption now; the net
//! agent's CSPRNG and encrypted Milk Drop are meant to use it too).
//!
//! - `chacha`: ChaCha20 (RFC 8439), HChaCha20, XChaCha20.
//! - `poly1305`: Poly1305 (RFC 8439).
//! - `aead`: ChaCha20-Poly1305 and XChaCha20-Poly1305.
//! - `hmac`: HMAC-SHA256, PBKDF2-HMAC-SHA256 (SHA-256 from `audit::sha256`).
//! - `scrypt`: scrypt (RFC 7914), its big array in page frames.
//! - `ct`: constant-time compare, zeroize.
//! - `rand`: salts and nonces (a stopgap until the kernel CSPRNG lands).
//!
//! [`selftest`] runs the published test vectors (RFC 8439, draft-irtf-cfrg-xchacha-03,
//! RFC 4231, RFC 7914) at boot.

// A shared API: not every primitive has a user in the kernel yet.
#![allow(dead_code)]

pub mod aead;
pub mod chacha;
pub mod ct;
pub mod hmac;
pub mod poly1305;
pub mod rand;
pub mod scrypt;

use alloc::string::String;
use alloc::vec::Vec;

fn unhex(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|c| c.is_ascii_hexdigit()).collect();
    s.chunks(2)
        .map(|p| u8::from_str_radix(core::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

const SUNSCREEN: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";

/// The known-answer tests. Returns the names of those that failed (empty = all good) and
/// how many ran.
pub fn selftest() -> (Vec<String>, usize) {
    let mut bad = Vec::new();
    let mut n = 0;
    let mut check = |name: &str, ok: bool| {
        n += 1;
        if !ok {
            bad.push(String::from(name));
        }
    };
    let k32 = |s: &str| -> [u8; 32] { unhex(s).try_into().unwrap() };
    let key: [u8; 32] = core::array::from_fn(|i| i as u8);
    // RFC 8439 2.3.2: the block function.
    let blk = chacha::block(
        &key,
        1,
        &unhex("000000090000004a00000000").try_into().unwrap(),
    );
    check(
        "chacha20 block (RFC 8439 2.3.2)",
        blk[..]
            == unhex(
                "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4ed2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e",
            )[..],
    );
    // RFC 8439 2.4.2: encryption.
    let mut m = SUNSCREEN.to_vec();
    chacha::xor(
        &key,
        1,
        &unhex("000000000000004a00000000").try_into().unwrap(),
        &mut m,
    );
    check(
        "chacha20 encrypt (RFC 8439 2.4.2)",
        m == unhex(
            "6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0bf91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d807ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab77937365af90bbf74a35be6b40b8eedf2785e42874d",
        ),
    );
    // RFC 8439 2.5.2: Poly1305.
    check(
        "poly1305 (RFC 8439 2.5.2)",
        poly1305::mac(
            &k32("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b"),
            b"Cryptographic Forum Research Group",
        )[..]
            == unhex("a8061dc1305136c6c22b8baf0c0127a9")[..],
    );
    // RFC 8439 2.8.2: AEAD.
    let akey = k32("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
    let aad = unhex("50515253c0c1c2c3c4c5c6c7");
    let n12: [u8; 12] = unhex("070000004041424344454647").try_into().unwrap();
    let mut m = SUNSCREEN.to_vec();
    let t = aead::seal(&akey, &n12, &aad, &mut m);
    check(
        "chacha20-poly1305 seal (RFC 8439 2.8.2)",
        m == unhex(
            "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116",
        ) && t[..] == unhex("1ae10b594f09e26a7e902ecbd0600691")[..],
    );
    let ok = aead::open(&akey, &n12, &aad, &mut m, &t);
    check("chacha20-poly1305 open", ok && m == SUNSCREEN);
    let mut bad_tag = t;
    bad_tag[0] ^= 1;
    let before = m.clone();
    check(
        "chacha20-poly1305 refuses a wrong tag",
        !aead::open(&akey, &n12, &aad, &mut m, &bad_tag) && m == before,
    );
    // draft-irtf-cfrg-xchacha-03 2.2.1 and A.3.1.
    check(
        "hchacha20 (xchacha draft 2.2.1)",
        chacha::hchacha20(
            &key,
            &unhex("000000090000004a0000000031415927")
                .try_into()
                .unwrap(),
        )[..]
            == unhex("82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc")[..],
    );
    let xn: [u8; 24] = unhex("404142434445464748494a4b4c4d4e4f5051525354555657")
        .try_into()
        .unwrap();
    let mut m = SUNSCREEN.to_vec();
    let t = aead::xseal(&akey, &xn, &aad, &mut m);
    check(
        "xchacha20-poly1305 (xchacha draft A.3.1)",
        m == unhex(
            "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b4522f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff921f9664c97637da9768812f615c68b13b52e",
        ) && t[..] == unhex("c0875924c1c7987947deafd8780acf49")[..],
    );
    check(
        "xchacha20-poly1305 open",
        aead::xopen(&akey, &xn, &aad, &mut m, &t) && m == SUNSCREEN,
    );
    // RFC 4231 test case 2.
    check(
        "hmac-sha256 (RFC 4231 case 2)",
        hmac::hmac_sha256(b"Jefe", b"what do ya want for nothing?")[..]
            == unhex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")[..],
    );
    // RFC 7914 section 11 and 12.
    let mut out = [0u8; 64];
    hmac::pbkdf2_sha256(b"passwd", b"salt", 1, &mut out);
    check(
        "pbkdf2-hmac-sha256 (RFC 7914 11)",
        out[..]
            == unhex(
                "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783",
            )[..],
    );
    let ok = scrypt::scrypt(b"", b"", 4, 1, 1, &mut out).is_some();
    check(
        "scrypt N=16 r=1 p=1 (RFC 7914 12)",
        ok && out[..]
            == unhex(
                "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906",
            )[..],
    );
    let ok = scrypt::scrypt(b"password", b"NaCl", 10, 8, 16, &mut out).is_some();
    check(
        "scrypt N=1024 r=8 p=16 (RFC 7914 12)",
        ok && out[..]
            == unhex(
                "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b3731622eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640",
            )[..],
    );
    check(
        "ct::eq",
        ct::eq(b"abc", b"abc") && !ct::eq(b"abc", b"abd") && !ct::eq(b"ab", b"abc"),
    );
    (bad, n)
}
