//! X.509 certificates (RFC 5280), parsed from DER without copying: the fields path
//! validation needs, the extensions it understands, the signature and key algorithms
//! milkcrypto can verify. [`Certificate::parse`] never panics; anything malformed is an
//! error, never a guess.

use alloc::string::String;
use alloc::vec::Vec;

use crate::der::{self, Der};
use crate::error::CertError;
pub use milkcrypto::rsa::Hash;

// Object identifiers, as their DER contents bytes.
const OID_RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_RSASSA_PSS: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0a];
const OID_SHA256_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const OID_SHA384_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
const OID_SHA512_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
const OID_MGF1: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x08];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
const OID_ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
const OID_ED25519: &[u8] = &[0x2b, 0x65, 0x70];
const OID_SHA256: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01];
const OID_SHA384: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x02];
const OID_SHA512: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x03];

const OID_SUBJECT_KEY_ID: &[u8] = &[0x55, 0x1d, 0x0e];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_NAME_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x1e];
const OID_CERT_POLICIES: &[u8] = &[0x55, 0x1d, 0x20];
const OID_POLICY_MAPPINGS: &[u8] = &[0x55, 0x1d, 0x21];
const OID_AUTHORITY_KEY_ID: &[u8] = &[0x55, 0x1d, 0x23];
const OID_POLICY_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x24];
const OID_EXT_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const OID_INHIBIT_ANY_POLICY: &[u8] = &[0x55, 0x1d, 0x36];
const OID_EKU_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const OID_EKU_ANY: &[u8] = &[0x55, 0x1d, 0x25, 0x00];
pub(crate) const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
pub(crate) const OID_ORGANIZATION: &[u8] = &[0x55, 0x04, 0x0a];

/// KeyUsage bits (RFC 5280 4.2.1.3), as [`der::bit_string_flags`] numbers them.
pub const KU_DIGITAL_SIGNATURE: u16 = 1 << 0;
pub const KU_KEY_CERT_SIGN: u16 = 1 << 5;

/// A signature algorithm we can verify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigAlg {
    RsaPkcs1(Hash),
    /// RSASSA-PSS with MGF1 over the same hash and this salt length.
    RsaPss { hash: Hash, salt: usize },
    Ecdsa(Hash),
    Ed25519,
}

/// A subject public key we can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicKey<'a> {
    /// Modulus and exponent (big-endian magnitudes). `pss_only` for id-RSASSA-PSS keys.
    Rsa {
        n: &'a [u8],
        e: &'a [u8],
        pss_only: bool,
    },
    /// Uncompressed SEC 1 points (65 / 97 bytes).
    P256(&'a [u8]),
    P384(&'a [u8]),
    Ed25519(&'a [u8]),
    /// Something else (P-521, DSA, ...): parsed, but nothing can be verified with it.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BasicConstraints {
    pub ca: bool,
    pub path_len: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ExtKeyUsage {
    pub server_auth: bool,
    pub any: bool,
}

/// One parsed certificate, borrowing from its DER.
#[derive(Clone, Debug)]
pub struct Certificate<'a> {
    pub der: &'a [u8],
    /// The TBSCertificate, as signed (the whole TLV).
    pub tbs: &'a [u8],
    /// None when the algorithm is one we can't verify.
    pub sig_alg: Option<SigAlg>,
    pub signature: &'a [u8],
    /// 1 or 3 (v2 certificates are treated as v1: no extensions allowed).
    pub version: u8,
    /// The issuer and subject Names (whole TLVs, compared byte for byte).
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    pub not_before: i64,
    pub not_after: i64,
    /// The whole SubjectPublicKeyInfo TLV.
    pub spki: &'a [u8],
    pub key: PublicKey<'a>,
    pub basic_constraints: Option<BasicConstraints>,
    pub key_usage: Option<u16>,
    pub ext_key_usage: Option<ExtKeyUsage>,
    /// The GeneralNames of subjectAltName (contents of the SEQUENCE), checked to parse.
    pub san: Option<&'a [u8]>,
    /// The NameConstraints SEQUENCE contents.
    pub name_constraints: Option<&'a [u8]>,
    pub subject_key_id: Option<&'a [u8]>,
    pub authority_key_id: Option<&'a [u8]>,
    /// A critical extension we don't know (refused for anything but a trust anchor).
    pub unknown_critical: bool,
}

/// A name in subjectAltName or a name constraint that we check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneralName<'a> {
    Dns(&'a [u8]),
    Ip(&'a [u8]),
    /// directoryName: a Name (whole TLV).
    Directory(&'a [u8]),
    /// rfc822Name, URI, otherName, ...: tag and contents.
    Other(u8, &'a [u8]),
}

/// Iterate the GeneralName entries of a GeneralNames (already validated by the parser).
pub fn general_names(seq: &[u8]) -> impl Iterator<Item = GeneralName<'_>> {
    let mut d = Der::new(seq);
    core::iter::from_fn(move || {
        let (tag, v, _) = d.any()?;
        general_name(tag, v)
    })
}

/// One GeneralName from its tag and contents.
pub fn general_names_one(tag: u8, v: &[u8]) -> Option<GeneralName<'_>> {
    general_name(tag, v)
}

fn general_name(tag: u8, v: &[u8]) -> Option<GeneralName<'_>> {
    Some(match tag {
        0x82 => GeneralName::Dns(v),
        0x87 => GeneralName::Ip(v),
        // directoryName [4] is EXPLICIT (Name is a CHOICE): the contents are one Name TLV.
        0xa4 => {
            let mut d = Der::new(v);
            let name = d.read_tlv(der::SEQUENCE)?;
            d.end()?;
            GeneralName::Directory(name)
        }
        _ => GeneralName::Other(tag, v),
    })
}

/// Check that GeneralNames parses: every entry a TLV, dNSName IA5 (ASCII), iPAddress 4 or 16
/// bytes (or 8 / 32 with a mask, when `masks`), directoryName a Name.
fn check_general_names(seq: &[u8], masks: bool) -> Option<()> {
    let mut d = Der::new(seq);
    while !d.is_empty() {
        let (tag, v, _) = d.any()?;
        check_general_name(tag, v, masks)?;
    }
    Some(())
}

fn check_general_name(tag: u8, v: &[u8], masks: bool) -> Option<()> {
    match general_name(tag, v)? {
        GeneralName::Dns(s) => s.is_ascii().then_some(()),
        GeneralName::Ip(a) => {
            let ok = if masks {
                a.len() == 8 || a.len() == 32
            } else {
                a.len() == 4 || a.len() == 16
            };
            ok.then_some(())
        }
        GeneralName::Directory(n) => {
            name_rdns(n)?;
            Some(())
        }
        GeneralName::Other(..) => Some(()),
    }
}

/// The RDNs (whole SET TLVs) of a Name, in order. None if malformed.
pub fn name_rdns(name: &[u8]) -> Option<Vec<&[u8]>> {
    let mut d = Der::new(name);
    let mut seq = d.sequence()?;
    d.end()?;
    let mut out = Vec::new();
    while !seq.is_empty() {
        let set = seq.read_tlv(der::SET)?;
        // Each RDN: SET OF AttributeTypeAndValue ::= SEQUENCE { type OID, value ANY }.
        let mut s = Der::new(set);
        s.read(der::SET)?;
        out.push(set);
    }
    Some(out)
}

/// The first value of attribute `oid` in a Name, as text (for messages; not for security).
pub fn name_attribute(name: &[u8], oid: &[u8]) -> Option<String> {
    for rdn in name_rdns(name)? {
        let mut s = Der::new(rdn);
        let mut set = Der::new(s.read(der::SET)?);
        while !set.is_empty() {
            let mut atv = set.sequence()?;
            let t = atv.read(der::OID)?;
            let (_tag, v, _) = atv.any()?;
            if t == oid {
                return Some(String::from_utf8_lossy(v).into_owned());
            }
        }
    }
    None
}

fn hash_alg(alg: &[u8]) -> Option<Hash> {
    // AlgorithmIdentifier { OID, NULL or absent }.
    let mut d = Der::new(alg);
    let oid = d.read(der::OID)?;
    if !d.is_empty() {
        d.read(der::NULL)?;
        d.end()?;
    }
    match oid {
        OID_SHA256 => Some(Hash::Sha256),
        OID_SHA384 => Some(Hash::Sha384),
        OID_SHA512 => Some(Hash::Sha512),
        _ => None,
    }
}

/// RSASSA-PSS-params (RFC 4055): we need the hash, MGF1 with the same hash, an explicit
/// salt length and the standard trailer. The SHA-1 defaults are refused.
fn pss_params(params: &[u8]) -> Option<SigAlg> {
    let mut d = Der::new(params);
    let hash = hash_alg(Der::new(d.read(der::ctx_cons(0))?).read(der::SEQUENCE)?)?;
    let mut mgf = Der::new(d.read(der::ctx_cons(1))?).sequence()?;
    if mgf.read(der::OID)? != OID_MGF1 {
        return None;
    }
    if hash_alg(mgf.read(der::SEQUENCE)?)? != hash {
        return None;
    }
    mgf.end()?;
    let mut salt_der = Der::new(d.read(der::ctx_cons(2))?);
    let salt = der::small_uint(salt_der.read(der::INTEGER)?)?;
    salt_der.end()?;
    if let Some(t) = d.optional(der::ctx_cons(3))? {
        let mut td = Der::new(t);
        if der::small_uint(td.read(der::INTEGER)?)? != 1 {
            return None;
        }
        td.end()?;
    }
    d.end()?;
    if salt > 1024 {
        return None;
    }
    Some(SigAlg::RsaPss {
        hash,
        salt: salt as usize,
    })
}

/// A signature AlgorithmIdentifier's contents. Outer None = malformed; inner None = an
/// algorithm we don't verify.
fn sig_alg(alg: &[u8]) -> Option<Option<SigAlg>> {
    let mut d = Der::new(alg);
    let oid = d.read(der::OID)?;
    let rest_is_null = |d: &mut Der| -> bool {
        d.is_empty() || (d.read(der::NULL).is_some_and(|v| v.is_empty()) && d.is_empty())
    };
    Some(match oid {
        OID_SHA256_RSA if rest_is_null(&mut d) => Some(SigAlg::RsaPkcs1(Hash::Sha256)),
        OID_SHA384_RSA if rest_is_null(&mut d) => Some(SigAlg::RsaPkcs1(Hash::Sha384)),
        OID_SHA512_RSA if rest_is_null(&mut d) => Some(SigAlg::RsaPkcs1(Hash::Sha512)),
        OID_ECDSA_SHA256 if d.is_empty() => Some(SigAlg::Ecdsa(Hash::Sha256)),
        OID_ECDSA_SHA384 if d.is_empty() => Some(SigAlg::Ecdsa(Hash::Sha384)),
        OID_ECDSA_SHA512 if d.is_empty() => Some(SigAlg::Ecdsa(Hash::Sha512)),
        OID_ED25519 if d.is_empty() => Some(SigAlg::Ed25519),
        OID_RSASSA_PSS => {
            let params = d.read(der::SEQUENCE)?;
            d.end()?;
            pss_params(params)
        }
        OID_SHA256_RSA | OID_SHA384_RSA | OID_SHA512_RSA | OID_ECDSA_SHA256
        | OID_ECDSA_SHA384 | OID_ECDSA_SHA512 | OID_ED25519 => return None, // bad parameters
        _ => None,
    })
}

/// SubjectPublicKeyInfo contents. Outer None = malformed.
fn public_key(spki: &[u8]) -> Option<PublicKey<'_>> {
    let mut d = Der::new(spki);
    let mut alg = d.sequence()?;
    let bits = der::bit_string_bytes(d.read(der::BIT_STRING)?)?;
    d.end()?;
    let oid = alg.read(der::OID)?;
    match oid {
        OID_RSA_ENCRYPTION | OID_RSASSA_PSS => {
            let pss_only = oid == OID_RSASSA_PSS;
            if pss_only {
                // Parameters absent, or RSASSA-PSS-params restricting the key (accepted:
                // the signature's own parameters are what we check).
                if !alg.is_empty() {
                    alg.read(der::SEQUENCE)?;
                }
            } else if !alg.is_empty() && !alg.read(der::NULL)?.is_empty() {
                return None;
            }
            alg.end()?;
            let mut k = Der::new(bits);
            let mut seq = k.sequence()?;
            k.end()?;
            let n = der::positive_integer(seq.read(der::INTEGER)?)?;
            let e = der::positive_integer(seq.read(der::INTEGER)?)?;
            seq.end()?;
            Some(PublicKey::Rsa { n, e, pss_only })
        }
        OID_EC_PUBLIC_KEY => {
            // ECParameters: only namedCurve.
            let curve = match alg.peek_tag() {
                Some(der::OID) => alg.read(der::OID)?,
                _ => {
                    alg.any()?;
                    alg.end()?;
                    return Some(PublicKey::Unsupported);
                }
            };
            alg.end()?;
            match curve {
                OID_P256 if bits.len() == 65 && bits[0] == 4 => Some(PublicKey::P256(bits)),
                OID_P384 if bits.len() == 97 && bits[0] == 4 => Some(PublicKey::P384(bits)),
                OID_P256 | OID_P384 => Some(PublicKey::Unsupported), // compressed points
                _ => Some(PublicKey::Unsupported),
            }
        }
        OID_ED25519 => {
            alg.end()?;
            (bits.len() == 32).then_some(PublicKey::Ed25519(bits))
        }
        _ => Some(PublicKey::Unsupported),
    }
}

/// The key of a whole SubjectPublicKeyInfo TLV.
pub fn spki_key(spki: &[u8]) -> Option<PublicKey<'_>> {
    let mut d = Der::new(spki);
    let inner = d.read(der::SEQUENCE)?;
    d.end()?;
    public_key(inner)
}

/// The validated contents of one extension, applied to `c`.
fn extension<'a>(c: &mut Certificate<'a>, oid: &[u8], critical: bool, v: &'a [u8]) -> Option<()> {
    let mut d = Der::new(v);
    match oid {
        OID_BASIC_CONSTRAINTS => {
            let mut s = d.sequence()?;
            d.end()?;
            let mut ca = false;
            if s.peek_tag() == Some(der::BOOLEAN) {
                ca = der::boolean(s.read(der::BOOLEAN)?)?;
            }
            let path_len = match s.optional(der::INTEGER)? {
                Some(i) => Some(der::small_uint(i)?),
                None => None,
            };
            s.end()?;
            c.basic_constraints = Some(BasicConstraints { ca, path_len });
        }
        OID_KEY_USAGE => {
            c.key_usage = Some(der::bit_string_flags(d.read(der::BIT_STRING)?)?);
            d.end()?;
        }
        OID_EXT_KEY_USAGE => {
            let mut s = d.sequence()?;
            d.end()?;
            let mut eku = ExtKeyUsage::default();
            if s.is_empty() {
                return None; // SEQUENCE SIZE (1..MAX)
            }
            while !s.is_empty() {
                match s.read(der::OID)? {
                    OID_EKU_SERVER_AUTH => eku.server_auth = true,
                    OID_EKU_ANY => eku.any = true,
                    _ => {}
                }
            }
            c.ext_key_usage = Some(eku);
        }
        OID_SUBJECT_ALT_NAME => {
            let names = d.read(der::SEQUENCE)?;
            d.end()?;
            check_general_names(names, false)?;
            c.san = Some(names);
        }
        OID_NAME_CONSTRAINTS => {
            let nc = d.read(der::SEQUENCE)?;
            d.end()?;
            let mut s = Der::new(nc);
            let mut any = false;
            for tag in [der::ctx_cons(0), der::ctx_cons(1)] {
                if let Some(subtrees) = s.optional(tag)? {
                    any = true;
                    let mut st = Der::new(subtrees);
                    if st.is_empty() {
                        return None;
                    }
                    while !st.is_empty() {
                        let mut sub = st.sequence()?;
                        let (t, gv, _) = sub.any()?;
                        check_general_name(t, gv, true)?;
                        // minimum [0] must be 0 and maximum [1] absent (RFC 5280).
                        if let Some(min) = sub.optional(der::ctx_prim(0))?
                            && der::small_uint(min)? != 0
                        {
                            return None;
                        }
                        if !sub.is_empty() {
                            return None;
                        }
                    }
                }
            }
            s.end()?;
            if !any {
                return None;
            }
            c.name_constraints = Some(nc);
        }
        OID_SUBJECT_KEY_ID => {
            c.subject_key_id = Some(d.read(der::OCTET_STRING)?);
            d.end()?;
        }
        OID_AUTHORITY_KEY_ID => {
            let mut s = d.sequence()?;
            d.end()?;
            c.authority_key_id = s.optional(der::ctx_prim(0))?;
        }
        // Policies: understood well enough to ignore (we don't enforce policies).
        OID_CERT_POLICIES | OID_POLICY_MAPPINGS | OID_POLICY_CONSTRAINTS
        | OID_INHIBIT_ANY_POLICY => {}
        _ => {
            if critical {
                c.unknown_critical = true;
            }
        }
    }
    Some(())
}

impl<'a> Certificate<'a> {
    pub fn parse(der: &'a [u8]) -> Result<Certificate<'a>, CertError> {
        Self::parse_inner(der).ok_or(CertError::Malformed)
    }

    fn parse_inner(input: &'a [u8]) -> Option<Certificate<'a>> {
        let mut top = Der::new(input);
        let (tag, body, _) = top.any()?;
        if tag != der::SEQUENCE {
            return None;
        }
        top.end()?;
        let mut cert = Der::new(body);
        let tbs = cert.read_tlv(der::SEQUENCE)?;
        let outer_alg = cert.read(der::SEQUENCE)?;
        let signature = der::bit_string_bytes(cert.read(der::BIT_STRING)?)?;
        cert.end()?;

        let mut t = Der::new(tbs).sequence()?;
        let mut version = 1u8;
        if let Some(v) = t.optional(der::ctx_cons(0))? {
            let mut vd = Der::new(v);
            version = match der::small_uint(vd.read(der::INTEGER)?)? {
                0 => 1,
                1 => 2,
                2 => 3,
                _ => return None,
            };
            vd.end()?;
        }
        let serial = t.read(der::INTEGER)?;
        if serial.is_empty() || serial.len() > 21 {
            return None;
        }
        let inner_alg = t.read(der::SEQUENCE)?;
        if inner_alg != outer_alg {
            return None; // RFC 5280 4.1.1.2: must be identical
        }
        let issuer = t.read_tlv(der::SEQUENCE)?;
        name_rdns(issuer)?;
        let mut validity = t.sequence()?;
        let (tag, v, _) = validity.any()?;
        let not_before = der::time(tag, v)?;
        let (tag, v, _) = validity.any()?;
        let not_after = der::time(tag, v)?;
        validity.end()?;
        let subject = t.read_tlv(der::SEQUENCE)?;
        name_rdns(subject)?;
        let spki = t.read_tlv(der::SEQUENCE)?;
        let key = public_key(Der::new(spki).read(der::SEQUENCE)?)?;
        // issuerUniqueID [1], subjectUniqueID [2]: allowed (v2+), ignored.
        t.optional(der::ctx_prim(1))?;
        t.optional(der::ctx_prim(2))?;
        let sig_alg = sig_alg(outer_alg)?;
        let mut c = Certificate {
            der: input,
            tbs,
            sig_alg,
            signature,
            version,
            issuer,
            subject,
            not_before,
            not_after,
            spki,
            key,
            basic_constraints: None,
            key_usage: None,
            ext_key_usage: None,
            san: None,
            name_constraints: None,
            subject_key_id: None,
            authority_key_id: None,
            unknown_critical: false,
        };
        if let Some(exts) = t.optional(der::ctx_cons(3))? {
            if version != 3 {
                return None;
            }
            let mut ed = Der::new(exts);
            let mut list = ed.sequence()?;
            ed.end()?;
            if list.is_empty() {
                return None;
            }
            let mut seen: Vec<&[u8]> = Vec::new();
            while !list.is_empty() {
                let mut ext = list.sequence()?;
                let oid = ext.read(der::OID)?;
                let mut critical = false;
                if ext.peek_tag() == Some(der::BOOLEAN) {
                    critical = der::boolean(ext.read(der::BOOLEAN)?)?;
                }
                let value = ext.read(der::OCTET_STRING)?;
                ext.end()?;
                if seen.contains(&oid) {
                    return None; // RFC 5280 4.2: at most one instance of each
                }
                seen.push(oid);
                extension(&mut c, oid, critical, value)?;
            }
        }
        t.end()?;
        Some(c)
    }

    /// Self-issued: the subject and issuer are the same name (RFC 5280 6.1).
    pub fn self_issued(&self) -> bool {
        self.subject == self.issuer
    }

    pub fn is_ca(&self) -> bool {
        self.basic_constraints.is_some_and(|b| b.ca)
    }

    /// The SAN entries (empty without the extension).
    pub fn san_names(&self) -> impl Iterator<Item = GeneralName<'a>> {
        general_names(self.san.unwrap_or(&[]))
    }

    /// The subject's common name, for messages ("issued to ...").
    pub fn subject_cn(&self) -> Option<String> {
        name_attribute(self.subject, OID_COMMON_NAME)
    }

    pub fn issuer_display(&self) -> Option<String> {
        name_attribute(self.issuer, OID_ORGANIZATION)
            .or_else(|| name_attribute(self.issuer, OID_COMMON_NAME))
    }
}

/// Split concatenated DER certificates (the root store's blob) into single certificates.
pub fn split_der(mut blob: &[u8]) -> impl Iterator<Item = &[u8]> {
    core::iter::from_fn(move || {
        if blob.is_empty() {
            return None;
        }
        let mut d = Der::new(blob);
        match d.any() {
            Some((der::SEQUENCE, _, whole)) => {
                blob = &blob[whole.len()..];
                Some(whole)
            }
            _ => {
                blob = &[];
                None
            }
        }
    })
}
