//! Chain building and path validation on the fixtures (scripts/gen_tls_fixtures.sh).

use super::{NOW, Prng, fixture, mutate};
use crate::error::CertError;
use crate::name::ServerName;
use crate::roots::RootStore;
use crate::verify::{Policy, verify_server_chain};
use alloc::vec::Vec;

fn store(roots: &[&str]) -> RootStore {
    let mut s = RootStore::empty();
    for r in roots {
        s.add_der(&fixture(r)).unwrap();
    }
    s
}

fn check_with(
    roots: &RootStore,
    chain: &[&str],
    host: &str,
    now: i64,
    policy: &Policy,
) -> Result<(), CertError> {
    let ders: Vec<Vec<u8>> = chain.iter().map(|n| fixture(n)).collect();
    let refs: Vec<&[u8]> = ders.iter().map(|d| &d[..]).collect();
    let name = ServerName::parse(host).expect("test host name");
    verify_server_chain(&refs, roots, now, &name, policy).map(|_| ())
}

fn check(chain: &[&str], host: &str) -> Result<(), CertError> {
    check_with(&store(&["root-ec", "root-rsa", "root-ed"]), chain, host, NOW, &Policy::default())
}

#[test]
fn good_chains() {
    assert_eq!(check(&["leaf-ec", "int-ec"], "test.milk.example"), Ok(()));
    assert_eq!(check(&["leaf-rsa", "int-rsa"], "test.milk.example"), Ok(()));
    assert_eq!(check(&["leaf-rsa-sha512", "int-rsa"], "test.milk.example"), Ok(()));
    assert_eq!(check(&["leaf-pss", "int-rsa"], "test.milk.example"), Ok(()));
    assert_eq!(check(&["leaf-ed"], "test.milk.example"), Ok(()));
    assert_eq!(check(&["leaf-noeku", "int-ec"], "test.milk.example"), Ok(()));
    // Order of the intermediates and extra certificates don't matter.
    assert_eq!(check(&["leaf-ec", "leaf-rsa", "int-rsa", "int-ec"], "test.milk.example"), Ok(()));
    assert_eq!(check(&["leaf-ec", "root-ec", "int-ec"], "test.milk.example"), Ok(()));
}

#[test]
fn host_names() {
    let ok = |h| check(&["leaf-ec", "int-ec"], h);
    assert_eq!(ok("TEST.milk.example."), Ok(()));
    assert_eq!(ok("www.wild.milk.example"), Ok(()));
    assert_eq!(ok("127.0.0.1"), Ok(()));
    assert_eq!(ok("10.0.2.2"), Ok(()));
    assert_eq!(ok("evil.example"), Err(CertError::WrongHost));
    assert_eq!(ok("wild.milk.example"), Err(CertError::WrongHost));
    assert_eq!(ok("a.b.wild.milk.example"), Err(CertError::WrongHost));
    assert_eq!(ok("127.0.0.2"), Err(CertError::WrongHost));
    assert_eq!(ok("::1"), Err(CertError::WrongHost));
    assert_eq!(ok("milk.example"), Err(CertError::WrongHost));
    // *.milk.example in the SAN.
    let other = |h| check(&["leaf-otherhost", "int-ec"], h);
    assert_eq!(other("test.milk.example"), Ok(()));
    assert_eq!(other("other.example"), Ok(()));
    assert_eq!(other("x.y.milk.example"), Err(CertError::WrongHost));
    // A DNS name in the common name only: no fallback.
    assert_eq!(check(&["leaf-nosan", "int-ec"], "test.milk.example"), Err(CertError::WrongHost));
}

#[test]
fn time() {
    assert_eq!(check(&["leaf-expired", "int-ec"], "test.milk.example"), Err(CertError::Expired));
    assert_eq!(check(&["leaf-future", "int-ec"], "test.milk.example"), Err(CertError::NotYetValid));
    let roots = store(&["root-ec"]);
    let p = Policy::default();
    // 2025-01-01 00:00:00 is the first valid second, 2045-01-01 00:00:00 the last.
    assert_eq!(check_with(&roots, &["leaf-ec", "int-ec"], "test.milk.example", 1735689600, &p), Ok(()));
    assert_eq!(
        check_with(&roots, &["leaf-ec", "int-ec"], "test.milk.example", 1735689599, &p),
        Err(CertError::NotYetValid)
    );
    assert_eq!(check_with(&roots, &["leaf-ec", "int-ec"], "test.milk.example", 2366841600, &p), Ok(()));
    assert_eq!(
        check_with(&roots, &["leaf-ec", "int-ec"], "test.milk.example", 2366841601, &p),
        Err(CertError::Expired)
    );
    // An expired intermediate.
    assert_eq!(
        check(&["leaf-under-expired-int", "int-expired"], "test.milk.example"),
        Err(CertError::Expired)
    );
}

#[test]
fn trust() {
    // No issuer at all: a missing intermediate, or a root we don't know.
    assert_eq!(check(&["leaf-ec"], "test.milk.example"), Err(CertError::UnknownIssuer));
    assert_eq!(check(&["leaf-untrusted"], "test.milk.example"), Err(CertError::UnknownIssuer));
    assert_eq!(
        check(&["leaf-untrusted", "other-root"], "test.milk.example"),
        Err(CertError::UnknownIssuer)
    );
    let only_ec = store(&["root-ec"]);
    assert_eq!(
        check_with(&only_ec, &["leaf-rsa", "int-rsa"], "test.milk.example", NOW, &Policy::default()),
        Err(CertError::UnknownIssuer)
    );
    // Trusting the server certificate itself is no help: it needs its issuer.
    let leaf_as_root = store(&["leaf-ed"]);
    assert_eq!(
        check_with(&leaf_as_root, &["leaf-ed"], "test.milk.example", NOW, &Policy::default()),
        Err(CertError::UnknownIssuer)
    );
    assert_eq!(check(&[], "test.milk.example"), Err(CertError::NoCertificate));
}

#[test]
fn cross_signed_backtracking() {
    // int-ec exists twice: under root-ec and cross-signed by root-rsa. With only root-rsa
    // trusted the search must back out of the root-ec branch.
    let rsa_only = store(&["root-rsa"]);
    let p = Policy::default();
    for chain in [
        &["leaf-ec", "int-ec", "int-ec-cross"][..],
        &["leaf-ec", "int-ec-cross", "int-ec"][..],
    ] {
        assert_eq!(check_with(&rsa_only, chain, "test.milk.example", NOW, &p), Ok(()));
    }
    let ec_only = store(&["root-ec"]);
    assert_eq!(
        check_with(&ec_only, &["leaf-ec", "int-ec-cross", "int-ec"], "test.milk.example", NOW, &p),
        Ok(())
    );
    assert_eq!(
        check_with(&ec_only, &["leaf-ec", "int-ec-cross"], "test.milk.example", NOW, &p),
        Err(CertError::UnknownIssuer)
    );
}

#[test]
fn usage_and_constraints() {
    let h = "test.milk.example";
    assert_eq!(check(&["leaf-clientauth", "int-ec"], h), Err(CertError::NotForServers));
    assert_eq!(
        check(&["leaf-under-clientauth", "int-clientauth"], h),
        Err(CertError::NotForServers)
    );
    assert_eq!(check(&["leaf-ca", "int-ec"], h), Err(CertError::CaAsServer));
    assert_eq!(check(&["leaf-ku", "int-ec"], h), Err(CertError::KeyUsage));
    assert_eq!(check(&["leaf-critical", "int-ec"], h), Err(CertError::UnknownCriticalExtension));
    assert_eq!(check(&["leaf-weak", "int-rsa"], h), Err(CertError::WeakKey));
    assert_eq!(
        check(&["leaf-pathlen", "int-under-pathlen0", "int-rsa"], h),
        Err(CertError::PathLenExceeded)
    );
    assert_eq!(check(&["leaf-under-notca", "int-notca"], h), Err(CertError::NotACa));
    // An issuer that is a leaf (no CA bit) can't issue either.
    assert_eq!(check(&["leaf-pathlen", "leaf-ec"], h), Err(CertError::UnknownIssuer));
    // RSA size policy: int-rsa's key is 3072 bits.
    let strict = Policy {
        min_rsa_bits: 4096,
        max_rsa_bits: 4096,
    };
    assert_eq!(
        check_with(&store(&["root-rsa"]), &["leaf-ec", "int-ec-cross", "int-ec"], h, NOW, &strict),
        Err(CertError::WeakKey)
    );
}

#[test]
fn name_constraints() {
    assert_eq!(check(&["leaf-nc-ok", "int-nc"], "www.allowed.example"), Ok(()));
    assert_eq!(check(&["leaf-nc-ok", "int-nc"], "10.1.2.3"), Ok(()));
    assert_eq!(check(&["leaf-nc-bad", "int-nc"], "test.milk.example"), Err(CertError::NameConstraints));
    // *.allowed.example could be bad.allowed.example, which is excluded.
    assert_eq!(
        check(&["leaf-nc-excluded", "int-nc"], "www.allowed.example"),
        Err(CertError::NameConstraints)
    );
    // The same with a trailing dot ("*.allowed.example.") must not slip past the exclusion.
    assert_eq!(check(&["leaf-nc-dot", "int-nc"], "bad.allowed.example"), Err(CertError::NameConstraints));
    // iPAddress is one form: an IPv6 name is outside IPv4-only permitted subtrees.
    assert_eq!(check(&["leaf-nc-ipv6", "int-nc"], "www.allowed.example"), Err(CertError::NameConstraints));
}

#[test]
fn constraint_work_is_bounded() {
    use crate::x509::Certificate;
    let d = fixture("leaf-nc-ok");
    let c = Certificate::parse(&d).unwrap();
    let nc_der = fixture("int-nc");
    let nc = Certificate::parse(&nc_der).unwrap().name_constraints.unwrap();
    let mut budget = 2;
    assert!(!crate::name::within_constraints(nc, &c, &mut budget));
    assert_eq!(budget, 0);
    let mut budget = 1000;
    assert!(crate::name::within_constraints(nc, &c, &mut budget));
    assert!(budget < 1000);
}

#[test]
fn hostile_chains_never_panic_or_pass() {
    let roots = store(&["root-ec", "root-rsa", "root-ed"]);
    let p = Policy::default();
    let name = ServerName::parse("test.milk.example").unwrap();
    let good: Vec<Vec<u8>> = ["leaf-ec", "int-ec"].iter().map(|n| fixture(n)).collect();
    let mut r = Prng(0x5eed_c4a1);
    let mut accepted = 0;
    for i in 0..3000 {
        let mut chain = good.clone();
        let which = i % 2;
        mutate(&mut chain[which], &mut r);
        if r.below(4) == 0 {
            mutate(&mut chain[which], &mut r);
        }
        let refs: Vec<&[u8]> = chain.iter().map(|d| &d[..]).collect();
        if verify_server_chain(&refs, &roots, NOW, &name, &p).is_ok() && chain != good {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 0, "a mutated chain was accepted");
    // Many look-alike intermediates: bounded work.
    let mut many: Vec<Vec<u8>> = vec![fixture("leaf-ec")];
    for _ in 0..40 {
        many.push(fixture("int-ec-cross"));
    }
    let refs: Vec<&[u8]> = many.iter().map(|d| &d[..]).collect();
    let only_ec = store(&["root-ec"]);
    assert!(verify_server_chain(&refs, &only_ec, NOW, &name, &p).is_err());
}

#[test]
fn mozilla_roots_load() {
    let s = RootStore::mozilla();
    assert_eq!(s.len(), 121, "every root in assets/ca/mozilla-roots.der parses");
    // None of our test roots is in there.
    let ours = fixture("root-ec");
    assert!(s.iter().all(|a| a.der() != &ours[..]));
}
