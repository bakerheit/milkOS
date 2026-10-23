//! What the padlock says about a secure connection: who the certificate was issued to and by,
//! when it is valid, the names it covers, and what TLS negotiated.

use alloc::string::String;
use alloc::vec::Vec;

use tls::x509::{self, Certificate, GeneralName};

use crate::date;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CertInfo {
    /// "TLS 1.3, TLS_AES_128_GCM_SHA256, X25519".
    pub connection: String,
    pub subject: Option<String>,
    pub issuer: Option<String>,
    /// "23 Oct 2026".
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    /// The DNS names in subjectAltName (the first few).
    pub names: Vec<String>,
    /// Certificates the server sent (leaf first).
    pub chain: usize,
}

/// The padlock's details for an established connection.
pub fn describe(info: &tls::ConnectionInfo) -> CertInfo {
    let mut c = CertInfo {
        connection: info.summary(),
        chain: info.certificates.len(),
        ..CertInfo::default()
    };
    if let Some(leaf) = info.certificates.first()
        && let Ok(cert) = Certificate::parse(leaf)
    {
        c.subject = cert.subject_cn();
        c.issuer = cert.issuer_display();
        c.valid_from = Some(date::day_month_year(cert.not_before));
        c.valid_to = Some(date::day_month_year(cert.not_after));
        if let Some(san) = cert.san {
            for n in x509::general_names(san).take(8) {
                if let GeneralName::Dns(d) = n {
                    c.names.push(String::from_utf8_lossy(d).into_owned());
                }
            }
        }
    }
    c
}

impl CertInfo {
    /// The lines of the padlock's panel.
    pub fn lines(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(s) = &self.subject {
            v.push(alloc::format!("Issued to: {s}"));
        }
        if let Some(i) = &self.issuer {
            v.push(alloc::format!("Issued by: {i}"));
        }
        if let (Some(f), Some(t)) = (&self.valid_from, &self.valid_to) {
            v.push(alloc::format!("Valid: {f} to {t}"));
        }
        if !self.names.is_empty() {
            v.push(alloc::format!("Names: {}", self.names.join(", ")));
        }
        v.push(alloc::format!("Connection: {}", self.connection));
        v.push(alloc::format!(
            "Checked against the Mozilla roots ({} certificate{} sent)",
            self.chain,
            if self.chain == 1 { "" } else { "s" }
        ));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_a_test_certificate() {
        let leaf = include_bytes!("../../../tls/testdata/chain/leaf-ec.der").to_vec();
        let int = include_bytes!("../../../tls/testdata/chain/int-ec.der").to_vec();
        let info = tls::ConnectionInfo {
            suite: tls::CipherSuite::Aes128GcmSha256,
            group: tls::NamedGroup::X25519,
            alpn: None,
            hello_retry: false,
            certificates: alloc::vec![leaf, int],
            server_name: tls::ServerName::Dns(String::from("milk.test")),
        };
        let c = describe(&info);
        assert!(c.connection.starts_with("TLS 1.3, "), "{c:?}");
        assert_eq!(c.chain, 2);
        assert!(
            c.issuer.is_some() && c.valid_from.is_some() && c.valid_to.is_some(),
            "{c:?}"
        );
        let lines = c.lines();
        assert!(lines.iter().any(|l| l.starts_with("Issued by: ")));
        assert!(lines.iter().any(|l| l.contains("2 certificates sent")));
        // Garbage instead of a certificate: still a description, just a shorter one.
        let bad = tls::ConnectionInfo {
            certificates: alloc::vec![alloc::vec![0x30, 0x82, 0xff]],
            ..info
        };
        let c = describe(&bad);
        assert!(c.subject.is_none() && c.issuer.is_none());
        assert!(!c.lines().is_empty());
    }
}
