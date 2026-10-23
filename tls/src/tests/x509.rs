//! X.509 parsing: real certificates (the Mozilla roots and the fixtures), and hostile or
//! mutated DER, which must never panic.

use super::{Prng, fixture, mutate};
use crate::roots::MOZILLA_ROOTS_DER;
use crate::x509::{self, BasicConstraints, Certificate, GeneralName, Hash, PublicKey, SigAlg};
use alloc::vec::Vec;

#[test]
fn every_mozilla_root_parses() {
    let mut n = 0;
    let mut keys = [0usize; 5];
    for der in x509::split_der(MOZILLA_ROOTS_DER) {
        let c = Certificate::parse(der).unwrap_or_else(|e| panic!("root {n}: {e}"));
        assert!(c.self_issued(), "root {n} is self-issued");
        match c.key {
            PublicKey::Rsa { .. } => keys[0] += 1,
            PublicKey::P256(_) => keys[1] += 1,
            PublicKey::P384(_) => keys[2] += 1,
            PublicKey::Ed25519(_) => keys[3] += 1,
            PublicKey::Unsupported => keys[4] += 1,
        }
        n += 1;
    }
    assert_eq!(n, 121);
    // assets/ca/mozilla-roots.txt: 80 RSA, 3 P-256, 37 P-384, 1 P-521 (unsupported).
    assert_eq!(keys, [80, 3, 37, 0, 1]);
}

#[test]
fn fixture_fields() {
    let d = fixture("leaf-ec");
    let c = Certificate::parse(&d).unwrap();
    assert_eq!(c.version, 3);
    assert_eq!(c.sig_alg, Some(SigAlg::Ecdsa(Hash::Sha256)));
    assert!(matches!(c.key, PublicKey::P256(p) if p.len() == 65));
    assert_eq!(c.not_before, 1735689600);
    assert_eq!(c.not_after, 2366841600);
    assert_eq!(c.basic_constraints, Some(BasicConstraints { ca: false, path_len: None }));
    assert_eq!(c.key_usage, Some(x509::KU_DIGITAL_SIGNATURE));
    assert!(c.ext_key_usage.unwrap().server_auth);
    let names: Vec<GeneralName> = c.san_names().collect();
    assert_eq!(
        names,
        [
            GeneralName::Dns(b"test.milk.example"),
            GeneralName::Dns(b"*.wild.milk.example"),
            GeneralName::Ip(&[127, 0, 0, 1]),
            GeneralName::Ip(&[10, 0, 2, 2]),
        ]
    );
    assert_eq!(c.subject_cn().as_deref(), Some("test.milk.example"));
    assert_eq!(c.issuer_display().as_deref(), Some("milkOS tests"));

    let i = fixture("int-rsa");
    let c = Certificate::parse(&i).unwrap();
    assert_eq!(c.basic_constraints, Some(BasicConstraints { ca: true, path_len: Some(0) }));
    assert!(matches!(c.key, PublicKey::Rsa { pss_only: false, .. }));
    assert_eq!(c.sig_alg, Some(SigAlg::RsaPkcs1(Hash::Sha256)));
    assert!(c.subject_key_id.is_some() && c.authority_key_id.is_some());

    let p = fixture("leaf-pss");
    assert_eq!(
        Certificate::parse(&p).unwrap().sig_alg,
        Some(SigAlg::RsaPss { hash: Hash::Sha256, salt: 32 })
    );
    let e = fixture("leaf-ed");
    let c = Certificate::parse(&e).unwrap();
    assert_eq!(c.sig_alg, Some(SigAlg::Ed25519));
    assert!(matches!(c.key, PublicKey::Ed25519(k) if k.len() == 32));
    assert!(Certificate::parse(&fixture("leaf-critical")).unwrap().unknown_critical);
    assert!(Certificate::parse(&fixture("int-nc")).unwrap().name_constraints.is_some());
    assert_eq!(Certificate::parse(&fixture("root-ec")).unwrap().sig_alg, Some(SigAlg::Ecdsa(Hash::Sha384)));
}

fn all_real() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = x509::split_der(MOZILLA_ROOTS_DER).map(|d| d.to_vec()).collect();
    for f in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/chain")).unwrap() {
        let p = f.unwrap().path();
        if p.extension().is_some_and(|e| e == "der") {
            v.push(std::fs::read(p).unwrap());
        }
    }
    v
}

#[test]
fn every_prefix_and_suffix_is_refused() {
    for der in all_real().iter().take(40) {
        for n in 0..der.len() {
            assert!(Certificate::parse(&der[..n]).is_err());
        }
        let mut longer = der.clone();
        longer.push(0);
        assert!(Certificate::parse(&longer).is_err());
    }
}

#[test]
fn mutated_der_never_panics() {
    let certs = all_real();
    let mut r = Prng(0xdec0_de11);
    let mut parsed = 0;
    for i in 0..60_000 {
        let mut d = certs[i % certs.len()].clone();
        for _ in 0..1 + r.below(3) {
            mutate(&mut d, &mut r);
        }
        if let Ok(c) = Certificate::parse(&d) {
            parsed += 1;
            // Walk everything a verifier would touch.
            let _ = c.san_names().count();
            let _ = c.subject_cn();
            let _ = c.issuer_display();
            if let Some(nc) = c.name_constraints {
                let _ = crate::name::within_constraints(nc, &c, &mut 1_000_000);
            }
        }
    }
    // Plenty still parse (mutations inside strings and signatures), and none panicked.
    assert!(parsed > 1000, "{parsed}");
}

#[test]
fn hostile_der() {
    let cases: &[&[u8]] = &[
        &[],
        &[0x30],
        &[0x30, 0x00],
        &[0x30, 0x80, 0x00, 0x00],
        &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff],
        &[0x30, 0x83, 0xff, 0xff, 0xff],
        &[0x30, 0x03, 0x30, 0x01, 0x30],
        &[0x31, 0x00],
        &[0xff; 64],
    ];
    for c in cases {
        assert!(Certificate::parse(c).is_err());
    }
    // Deep nesting.
    let mut deep = Vec::new();
    for _ in 0..1000 {
        deep.insert(0, 0x00);
        deep.insert(0, 0x30);
    }
    assert!(Certificate::parse(&deep).is_err());
    // Random bytes.
    let mut r = Prng(7);
    for _ in 0..5000 {
        let mut b = alloc::vec![0u8; r.below(600)];
        r.fill(&mut b);
        if !b.is_empty() && r.below(2) == 0 {
            b[0] = 0x30;
        }
        assert!(Certificate::parse(&b).is_err());
    }
}

#[test]
fn split_der_stops_at_garbage() {
    let a = fixture("leaf-ec");
    let mut blob = a.clone();
    blob.extend_from_slice(&a);
    blob.extend_from_slice(&[0x30, 0x05, 1]);
    assert_eq!(x509::split_der(&blob).count(), 2);
}
