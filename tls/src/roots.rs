//! Trust anchors. [`RootStore::mozilla`] is the Mozilla CA bundle compiled in from
//! `assets/ca/mozilla-roots.der` (made by `scripts/gen_roots.py`, which records its source,
//! date and SHA-256; the list is `assets/ca/mozilla-roots.txt`). An anchor is a root
//! certificate's subject name, public key and name constraints (RFC 5280 6.1.1 (d)): its
//! own signature, validity and other extensions are not looked at.

use alloc::vec::Vec;

use crate::error::CertError;
use crate::x509::{self, Certificate, PublicKey};

/// The compiled-in roots: DER certificates, back to back.
pub static MOZILLA_ROOTS_DER: &[u8] = include_bytes!("../../assets/ca/mozilla-roots.der");

enum Bytes {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl Bytes {
    fn get(&self) -> &[u8] {
        match self {
            Bytes::Static(s) => s,
            Bytes::Owned(v) => v,
        }
    }
}

pub struct TrustAnchor {
    der: Bytes,
    subject: (usize, usize),
    spki: (usize, usize),
    name_constraints: Option<(usize, usize)>,
}

fn range(outer: &[u8], inner: &[u8]) -> (usize, usize) {
    (inner.as_ptr() as usize - outer.as_ptr() as usize, inner.len())
}

impl TrustAnchor {
    fn new(der: Bytes) -> Result<TrustAnchor, CertError> {
        let (subject, spki, nc) = {
            let d = der.get();
            let c = Certificate::parse(d)?;
            (
                range(d, c.subject),
                range(d, c.spki),
                c.name_constraints.map(|n| range(d, n)),
            )
        };
        Ok(TrustAnchor {
            der,
            subject,
            spki,
            name_constraints: nc,
        })
    }

    fn slice(&self, r: (usize, usize)) -> &[u8] {
        self.der.get().get(r.0..r.0 + r.1).unwrap_or(&[])
    }

    /// The root's subject Name (whole TLV).
    pub fn subject(&self) -> &[u8] {
        self.slice(self.subject)
    }

    pub fn key(&self) -> PublicKey<'_> {
        x509::spki_key(self.slice(self.spki)).unwrap_or(PublicKey::Unsupported)
    }

    pub fn name_constraints(&self) -> Option<&[u8]> {
        self.name_constraints.map(|r| self.slice(r))
    }

    /// The whole root certificate.
    pub fn der(&self) -> &[u8] {
        self.der.get()
    }
}

#[derive(Default)]
pub struct RootStore {
    anchors: Vec<TrustAnchor>,
}

impl RootStore {
    /// No roots at all (add some with [`RootStore::add_der`]).
    pub fn empty() -> RootStore {
        RootStore::default()
    }

    /// The Mozilla CA bundle. Parsing takes a few milliseconds; build it once and share it.
    pub fn mozilla() -> RootStore {
        let mut s = RootStore::empty();
        for der in x509::split_der(MOZILLA_ROOTS_DER) {
            if let Ok(a) = TrustAnchor::new(Bytes::Static(der)) {
                s.anchors.push(a);
            }
        }
        s
    }

    /// Trust one more root, given as a DER certificate (copied).
    pub fn add_der(&mut self, der: &[u8]) -> Result<(), CertError> {
        self.anchors.push(TrustAnchor::new(Bytes::Owned(der.to_vec()))?);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &TrustAnchor> {
        self.anchors.iter()
    }

    /// The anchors whose subject is `name` (byte for byte).
    pub fn by_subject<'s>(&'s self, name: &'s [u8]) -> impl Iterator<Item = &'s TrustAnchor> + 's {
        self.anchors.iter().filter(move |a| a.subject() == name)
    }
}
