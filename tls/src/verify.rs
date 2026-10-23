//! Certificate path validation (RFC 5280 6, as browsers apply it): from the server's
//! certificate up to a trust anchor, through the intermediates the server sent (in any
//! order, extras ignored), checking at every step:
//!
//! * the signature (ECDSA P-256/P-384, RSA PKCS#1 v1.5 and PSS within the RSA size policy,
//!   Ed25519; SHA-256/384/512 only),
//! * validity times against the clock (the anchor's are not checked: an anchor is a name and
//!   a key),
//! * basicConstraints: every issuer is a v3 CA; the server's certificate is not a CA,
//! * key usage: keyCertSign for issuers, digitalSignature for the server's key (when present),
//! * extended key usage: serverAuth in the server's certificate when it has the extension,
//!   serverAuth or anyExtendedKeyUsage in issuers that have it (as Firefox and Chrome do),
//! * pathLenConstraint, counting the non-self-issued intermediates below each issuer,
//! * name constraints (dNSName, iPAddress, directoryName) of every issuer and the anchor,
//! * no critical extension we don't understand,
//! * the server name against subjectAltName only ([`crate::name`]).
//!
//! The search is depth-first with backtracking (cross-signed roots work), bounded in depth
//! and in signature checks so a hostile chain can't make us spin.

use alloc::vec::Vec;

use crate::error::CertError;
use crate::name::{self, ServerName};
use crate::roots::RootStore;
use crate::x509::{self, Certificate, Hash, PublicKey, SigAlg};
use milkcrypto::{ed25519, p256, p384, rsa, sha256, sha512};

/// Most intermediates in a path.
const MAX_INTERMEDIATES: usize = 6;
/// Most certificates we look at from the server.
const MAX_PRESENTED: usize = 16;
/// Most signature checks per chain.
const MAX_SIGNATURE_CHECKS: usize = 48;
/// Most name-against-subtree comparisons per chain (as BoringSSL: 2^20).
const MAX_CONSTRAINT_CHECKS: usize = 1 << 20;

/// RSA modulus sizes we accept anywhere (chain and handshake).
#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub min_rsa_bits: usize,
    pub max_rsa_bits: usize,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            min_rsa_bits: 2048,
            max_rsa_bits: 4096,
        }
    }
}

pub fn digest(h: Hash, msg: &[u8]) -> Vec<u8> {
    match h {
        Hash::Sha256 => sha256::sha256(msg).to_vec(),
        Hash::Sha384 => sha512::sha384(msg).to_vec(),
        Hash::Sha512 => sha512::sha512(msg).to_vec(),
    }
}

fn rsa_key(n: &[u8], e: &[u8], policy: &Policy) -> Result<rsa::PublicKey, CertError> {
    let k = rsa::PublicKey::new(n, e).ok_or(CertError::Malformed)?;
    if k.bits() < policy.min_rsa_bits || k.bits() > policy.max_rsa_bits {
        return Err(CertError::WeakKey);
    }
    Ok(k)
}

/// Check `sig` over `msg` with `key` under `alg`.
pub fn verify_signature(
    alg: SigAlg,
    key: &PublicKey,
    msg: &[u8],
    sig: &[u8],
    policy: &Policy,
) -> Result<(), CertError> {
    let ok = match (alg, *key) {
        (_, PublicKey::Unsupported) => return Err(CertError::UnsupportedAlgorithm),
        (SigAlg::RsaPkcs1(h), PublicKey::Rsa { n, e, pss_only }) => {
            if pss_only {
                return Err(CertError::BadSignature);
            }
            rsa_key(n, e, policy)?.verify_pkcs1v15(h, &digest(h, msg), sig)
        }
        (SigAlg::RsaPss { hash, salt }, PublicKey::Rsa { n, e, .. }) => {
            rsa_key(n, e, policy)?.verify_pss(hash, &digest(hash, msg), sig, salt)
        }
        (SigAlg::Ecdsa(h), PublicKey::P256(pt)) => p256::verify_der(pt, &digest(h, msg), sig),
        (SigAlg::Ecdsa(h), PublicKey::P384(pt)) => p384::verify_der(pt, &digest(h, msg), sig),
        (SigAlg::Ed25519, PublicKey::Ed25519(pk)) => {
            match (<&[u8; 32]>::try_from(pk), <&[u8; 64]>::try_from(sig)) {
                (Ok(pk), Ok(sig)) => ed25519::verify(pk, msg, sig),
                _ => false,
            }
        }
        // The algorithm doesn't fit the key (an ECDSA signature from an RSA key, ...).
        _ => false,
    };
    if ok { Ok(()) } else { Err(CertError::BadSignature) }
}

fn check_time(c: &Certificate, now: i64) -> Result<(), CertError> {
    if now < c.not_before {
        return Err(CertError::NotYetValid);
    }
    if now > c.not_after {
        return Err(CertError::Expired);
    }
    Ok(())
}

/// The checks on the server's own certificate that don't need the chain.
fn check_leaf(leaf: &Certificate, now: i64, name: &ServerName, policy: &Policy) -> Result<(), CertError> {
    if leaf.unknown_critical {
        return Err(CertError::UnknownCriticalExtension);
    }
    if !name::san_matches(leaf, name) {
        return Err(CertError::WrongHost);
    }
    check_time(leaf, now)?;
    if leaf.is_ca() {
        return Err(CertError::CaAsServer);
    }
    if let Some(eku) = leaf.ext_key_usage
        && !eku.server_auth
    {
        return Err(CertError::NotForServers);
    }
    if let Some(ku) = leaf.key_usage
        && ku & x509::KU_DIGITAL_SIGNATURE == 0
    {
        return Err(CertError::KeyUsage);
    }
    if leaf.key == PublicKey::Unsupported || leaf.sig_alg.is_none() {
        return Err(CertError::UnsupportedAlgorithm);
    }
    if let PublicKey::Rsa { n, e, .. } = leaf.key {
        rsa_key(n, e, policy)?; // the key that will sign the handshake
    }
    Ok(())
}

/// The checks on an issuer (an intermediate) that don't involve the child.
fn check_issuer(c: &Certificate, now: i64, below: usize) -> Result<(), CertError> {
    if c.unknown_critical {
        return Err(CertError::UnknownCriticalExtension);
    }
    if c.version != 3 || !c.is_ca() {
        return Err(CertError::NotACa);
    }
    if let Some(ku) = c.key_usage
        && ku & x509::KU_KEY_CERT_SIGN == 0
    {
        return Err(CertError::KeyUsage);
    }
    if let Some(eku) = c.ext_key_usage
        && !eku.server_auth
        && !eku.any
    {
        return Err(CertError::NotForServers);
    }
    check_time(c, now)?;
    if let Some(bc) = c.basic_constraints
        && let Some(max) = bc.path_len
        && below as u64 > max
    {
        return Err(CertError::PathLenExceeded);
    }
    Ok(())
}

struct Search<'a, 'r> {
    leaf: &'r Certificate<'a>,
    presented: &'r [Certificate<'a>],
    roots: &'r RootStore,
    now: i64,
    policy: &'r Policy,
    checks: usize,
    constraint_budget: usize,
    /// The most specific error seen so far: (how far up the path it was, error).
    best: (usize, CertError),
}

impl<'a, 'r> Search<'a, 'r> {
    fn note(&mut self, depth: usize, e: CertError) {
        if depth > self.best.0 {
            self.best = (depth, e);
        }
    }

    fn signature(&mut self, child: &Certificate, key: &PublicKey) -> Result<(), CertError> {
        if self.checks >= MAX_SIGNATURE_CHECKS {
            return Err(CertError::ChainTooLong);
        }
        self.checks += 1;
        let alg = child.sig_alg.ok_or(CertError::UnsupportedAlgorithm)?;
        verify_signature(alg, key, child.tbs, child.signature, self.policy)
    }

    /// Name constraints of a complete path (checked only once a path reaches an anchor, so
    /// an untrusted chain costs nothing here): each issuer's over the certificates below it,
    /// the anchor's over all of them.
    fn path_constraints_ok(&mut self, anchor_nc: Option<&[u8]>, path: &[usize]) -> bool {
        let presented = self.presented;
        let leaf = self.leaf;
        let below = |p: &[usize]| -> Vec<&'r Certificate<'a>> {
            core::iter::once(leaf).chain(p.iter().map(|&i| &presented[i])).collect()
        };
        for (j, &i) in path.iter().enumerate() {
            if let Some(nc) = presented[i].name_constraints {
                for c in below(&path[..j]) {
                    if !name::within_constraints(nc, c, &mut self.constraint_budget) {
                        return false;
                    }
                }
            }
        }
        if let Some(nc) = anchor_nc {
            for c in below(path) {
                if !name::within_constraints(nc, c, &mut self.constraint_budget) {
                    return false;
                }
            }
        }
        true
    }

    /// Find an issuer for `child` (the top of `path`, or the leaf), recursively.
    fn find(&mut self, child: &Certificate<'a>, path: &mut Vec<usize>) -> Result<(), CertError> {
        let depth = path.len() + 1;
        let roots = self.roots;
        let presented = self.presented;
        // A trust anchor?
        for anchor in roots.by_subject(child.issuer) {
            let key = anchor.key();
            match self.signature(child, &key) {
                Ok(()) => {
                    if self.path_constraints_ok(anchor.name_constraints(), path) {
                        return Ok(());
                    }
                    self.note(depth, CertError::NameConstraints);
                }
                Err(CertError::ChainTooLong) => return Err(CertError::ChainTooLong),
                Err(e) => self.note(depth, e),
            }
        }
        if path.len() >= MAX_INTERMEDIATES {
            self.note(depth, CertError::ChainTooLong);
            return Err(self.best.1);
        }
        // An intermediate the server sent?
        let below = path.iter().filter(|&&i| !presented[i].self_issued()).count();
        for (i, cand) in presented.iter().enumerate() {
            if path.contains(&i) {
                continue;
            }
            if cand.subject != child.issuer || cand.der == self.leaf.der {
                continue;
            }
            if let Err(e) = check_issuer(cand, self.now, below) {
                self.note(depth, e);
                continue;
            }
            match self.signature(child, &cand.key) {
                Ok(()) => {}
                Err(CertError::ChainTooLong) => return Err(CertError::ChainTooLong),
                Err(e) => {
                    self.note(depth, e);
                    continue;
                }
            }
            path.push(i);
            match self.find(cand, path) {
                Ok(()) => return Ok(()),
                Err(CertError::ChainTooLong) if self.checks >= MAX_SIGNATURE_CHECKS => {
                    return Err(CertError::ChainTooLong);
                }
                Err(_) => {}
            }
            path.pop();
        }
        Err(self.best.1)
    }
}

/// Validate the server's chain (`certs[0]` is the server's certificate) for `name` at `now`
/// (Unix seconds). Returns the parsed server certificate.
pub fn verify_server_chain<'a>(
    certs: &[&'a [u8]],
    roots: &RootStore,
    now: i64,
    name: &ServerName,
    policy: &Policy,
) -> Result<Certificate<'a>, CertError> {
    let (&leaf_der, rest) = certs.split_first().ok_or(CertError::NoCertificate)?;
    let leaf = Certificate::parse(leaf_der)?;
    check_leaf(&leaf, now, name, policy)?;
    let presented: Vec<Certificate<'a>> = rest
        .iter()
        .take(MAX_PRESENTED)
        .filter_map(|d| Certificate::parse(d).ok())
        .collect();
    let mut s = Search {
        leaf: &leaf,
        presented: &presented,
        roots,
        now,
        policy,
        checks: 0,
        constraint_budget: MAX_CONSTRAINT_CHECKS,
        best: (0, CertError::UnknownIssuer),
    };
    let mut path = Vec::new();
    s.find(&leaf, &mut path)?;
    Ok(leaf)
}
