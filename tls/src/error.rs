//! What can go wrong, in words the Browser can show as they are ([`TlsError`]'s `Display`),
//! and the TLS alerts (RFC 8446 6) that go with it.

use alloc::string::String;
use core::fmt;

/// A TLS alert description (RFC 8446 6). A newtype rather than an enum: the server may send
/// any value, and we show its name if we know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alert(pub u8);

impl Alert {
    pub const CLOSE_NOTIFY: Alert = Alert(0);
    pub const UNEXPECTED_MESSAGE: Alert = Alert(10);
    pub const BAD_RECORD_MAC: Alert = Alert(20);
    pub const RECORD_OVERFLOW: Alert = Alert(22);
    pub const HANDSHAKE_FAILURE: Alert = Alert(40);
    pub const BAD_CERTIFICATE: Alert = Alert(42);
    pub const UNSUPPORTED_CERTIFICATE: Alert = Alert(43);
    pub const CERTIFICATE_REVOKED: Alert = Alert(44);
    pub const CERTIFICATE_EXPIRED: Alert = Alert(45);
    pub const CERTIFICATE_UNKNOWN: Alert = Alert(46);
    pub const ILLEGAL_PARAMETER: Alert = Alert(47);
    pub const UNKNOWN_CA: Alert = Alert(48);
    pub const ACCESS_DENIED: Alert = Alert(49);
    pub const DECODE_ERROR: Alert = Alert(50);
    pub const DECRYPT_ERROR: Alert = Alert(51);
    pub const PROTOCOL_VERSION: Alert = Alert(70);
    pub const INSUFFICIENT_SECURITY: Alert = Alert(71);
    pub const INTERNAL_ERROR: Alert = Alert(80);
    pub const INAPPROPRIATE_FALLBACK: Alert = Alert(86);
    pub const USER_CANCELED: Alert = Alert(90);
    pub const MISSING_EXTENSION: Alert = Alert(109);
    pub const UNSUPPORTED_EXTENSION: Alert = Alert(110);
    pub const UNRECOGNIZED_NAME: Alert = Alert(112);
    pub const BAD_CERTIFICATE_STATUS_RESPONSE: Alert = Alert(113);
    pub const UNKNOWN_PSK_IDENTITY: Alert = Alert(115);
    pub const CERTIFICATE_REQUIRED: Alert = Alert(116);
    pub const NO_APPLICATION_PROTOCOL: Alert = Alert(120);

    pub fn name(self) -> &'static str {
        match self.0 {
            0 => "close notify",
            10 => "unexpected message",
            20 => "bad record MAC",
            21 => "decryption failed",
            22 => "record overflow",
            40 => "handshake failure",
            42 => "bad certificate",
            43 => "unsupported certificate",
            44 => "certificate revoked",
            45 => "certificate expired",
            46 => "certificate unknown",
            47 => "illegal parameter",
            48 => "unknown CA",
            49 => "access denied",
            50 => "decode error",
            51 => "decrypt error",
            70 => "protocol version",
            71 => "insufficient security",
            80 => "internal error",
            86 => "inappropriate fallback",
            90 => "user canceled",
            109 => "missing extension",
            110 => "unsupported extension",
            112 => "unrecognized name",
            113 => "bad certificate status response",
            115 => "unknown PSK identity",
            116 => "certificate required",
            120 => "no application protocol",
            _ => "unknown alert",
        }
    }
}

/// Why a certificate (chain) was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertError {
    /// The server's certificate names other hosts (subjectAltName; the common name is never used).
    WrongHost,
    Expired,
    NotYetValid,
    /// No chain to a root in the store: an unknown or self-signed root, or a missing intermediate.
    UnknownIssuer,
    /// A signature in the chain doesn't verify.
    BadSignature,
    /// A signature or key algorithm we don't do (SHA-1, P-521, DSA, ...).
    UnsupportedAlgorithm,
    /// An RSA key outside 2048..=4096 bits (or the config's limits).
    WeakKey,
    /// Extended key usage without serverAuth (in the leaf or an intermediate).
    NotForServers,
    /// Key usage forbids what the key is used for (signing a handshake, or certificates).
    KeyUsage,
    /// DER that doesn't parse, or a certificate that breaks RFC 5280's structure rules.
    Malformed,
    /// More intermediates than we follow, or too much work to find a chain.
    ChainTooLong,
    /// An intermediate's pathLenConstraint is exceeded.
    PathLenExceeded,
    /// A name outside an issuer's permitted subtrees, or inside its excluded ones.
    NameConstraints,
    /// An issuer without basicConstraints cA = TRUE.
    NotACa,
    /// The server presented a CA certificate as its own.
    CaAsServer,
    /// The Certificate message was empty.
    NoCertificate,
    /// A critical extension we don't understand.
    UnknownCriticalExtension,
}

impl CertError {
    pub fn message(self) -> &'static str {
        match self {
            CertError::WrongHost => "the certificate is for another site",
            CertError::Expired => "the certificate has expired",
            CertError::NotYetValid => "the certificate isn't valid yet (is the clock right?)",
            CertError::UnknownIssuer => "the certificate was not issued by a trusted authority",
            CertError::BadSignature => "the certificate's signature doesn't check out",
            CertError::UnsupportedAlgorithm => {
                "the certificate uses a signature or key type milkOS doesn't support"
            }
            CertError::WeakKey => "the certificate's key is too weak (or too big)",
            CertError::NotForServers => "the certificate isn't meant for websites",
            CertError::KeyUsage => "the certificate's key isn't allowed to be used this way",
            CertError::Malformed => "the certificate is damaged or malformed",
            CertError::ChainTooLong => "the certificate chain is too long",
            CertError::PathLenExceeded => {
                "an authority in the chain has more authorities under it than it is allowed"
            }
            CertError::NameConstraints => {
                "the certificate's authority isn't allowed to vouch for this site"
            }
            CertError::NotACa => "a certificate in the chain isn't allowed to issue certificates",
            CertError::CaAsServer => {
                "the site presented an authority's certificate instead of its own"
            }
            CertError::NoCertificate => "the site sent no certificate",
            CertError::UnknownCriticalExtension => {
                "the certificate has a critical extension milkOS doesn't understand"
            }
        }
    }

    /// The alert we send the server when we refuse its certificate.
    pub fn alert(self) -> Alert {
        match self {
            CertError::Expired | CertError::NotYetValid => Alert::CERTIFICATE_EXPIRED,
            CertError::UnknownIssuer => Alert::UNKNOWN_CA,
            CertError::UnsupportedAlgorithm | CertError::WeakKey => Alert::UNSUPPORTED_CERTIFICATE,
            CertError::NoCertificate => Alert::DECODE_ERROR,
            CertError::WrongHost
            | CertError::NotForServers
            | CertError::KeyUsage
            | CertError::NameConstraints
            | CertError::CaAsServer
            | CertError::NotACa
            | CertError::PathLenExceeded
            | CertError::ChainTooLong => Alert::CERTIFICATE_UNKNOWN,
            CertError::BadSignature
            | CertError::Malformed
            | CertError::UnknownCriticalExtension => Alert::BAD_CERTIFICATE,
        }
    }
}

impl fmt::Display for CertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// An error from the transport underneath (the TCP connection), in its own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IoError {
    TimedOut,
    /// Reset, refused, unreachable, ...: the transport's description.
    Failed(String),
}

impl fmt::Display for IoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IoError::TimedOut => f.write_str("timed out"),
            IoError::Failed(s) => f.write_str(s),
        }
    }
}

/// Everything [`crate::Client::connect`] and [`crate::Stream`] can fail with. `Display` is a
/// plain-English sentence for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TlsError {
    /// The transport failed.
    Io(IoError),
    /// The server closed the connection during the handshake.
    ConnectionClosed,
    /// The connection ended after the handshake without a close_notify (possible truncation).
    Truncated,
    /// The server answered with TLS 1.2 or older, or refused TLS 1.3 (protocol_version).
    NotTls13,
    /// No cipher suite / group / signature algorithm in common (or handshake_failure from the server).
    NoCommonAlgorithms,
    /// The server sent a fatal alert.
    PeerAlert(Alert),
    /// The certificate chain was refused.
    Certificate(CertError),
    /// CertificateVerify failed: the server doesn't hold the certificate's private key.
    BadHandshakeSignature,
    /// A record failed its integrity check, or Finished didn't match.
    Tampered,
    /// The server broke the protocol; `what` says how, `alert` is what we answered.
    Protocol { alert: Alert, what: &'static str },
    /// ALPN: the server picked a protocol we didn't offer, or refused ours.
    NoApplicationProtocol,
    /// The clock is unset (before 2020), so validity dates can't be checked.
    ClockNotSet,
    /// Not a DNS name or IP address we can check a certificate against.
    InvalidServerName,
    /// The stream was closed (by us, or by an earlier error) and can't be used.
    Closed,
}

impl TlsError {
    pub(crate) fn proto(alert: Alert, what: &'static str) -> TlsError {
        TlsError::Protocol { alert, what }
    }
    pub(crate) fn decode(what: &'static str) -> TlsError {
        TlsError::Protocol {
            alert: Alert::DECODE_ERROR,
            what,
        }
    }
    pub(crate) fn unexpected(what: &'static str) -> TlsError {
        TlsError::Protocol {
            alert: Alert::UNEXPECTED_MESSAGE,
            what,
        }
    }
    pub(crate) fn illegal(what: &'static str) -> TlsError {
        TlsError::Protocol {
            alert: Alert::ILLEGAL_PARAMETER,
            what,
        }
    }

    /// The alert to send the server before giving up on this error (None: send nothing,
    /// the connection is already gone or the server ended it).
    pub(crate) fn alert(&self) -> Option<Alert> {
        match self {
            TlsError::Io(_)
            | TlsError::ConnectionClosed
            | TlsError::Truncated
            | TlsError::PeerAlert(_)
            | TlsError::Closed => None,
            TlsError::NotTls13 => Some(Alert::PROTOCOL_VERSION),
            TlsError::NoCommonAlgorithms => Some(Alert::HANDSHAKE_FAILURE),
            TlsError::Certificate(e) => Some(e.alert()),
            TlsError::BadHandshakeSignature => Some(Alert::DECRYPT_ERROR),
            TlsError::Tampered => Some(Alert::BAD_RECORD_MAC),
            TlsError::Protocol { alert, .. } => Some(*alert),
            TlsError::NoApplicationProtocol => Some(Alert::NO_APPLICATION_PROTOCOL),
            TlsError::ClockNotSet | TlsError::InvalidServerName => Some(Alert::INTERNAL_ERROR),
        }
    }
}

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TlsError::Io(e) => write!(f, "the connection failed ({e})"),
            TlsError::ConnectionClosed => {
                f.write_str("the site closed the connection during the secure handshake")
            }
            TlsError::Truncated => f.write_str("the secure connection was cut off before the end"),
            TlsError::NotTls13 => f.write_str("the site doesn't speak TLS 1.3"),
            TlsError::NoCommonAlgorithms => {
                f.write_str("the site and milkOS have no encryption method in common")
            }
            TlsError::PeerAlert(a) => {
                write!(f, "the site refused the secure connection ({})", a.name())
            }
            TlsError::Certificate(e) => f.write_str(e.message()),
            TlsError::BadHandshakeSignature => {
                f.write_str("the site couldn't prove it owns its certificate")
            }
            TlsError::Tampered => f.write_str(
                "the secure connection was tampered with (data failed its integrity check)",
            ),
            TlsError::Protocol { what, .. } => write!(f, "the site broke the TLS protocol ({what})"),
            TlsError::NoApplicationProtocol => {
                f.write_str("the site doesn't offer HTTP/1.1 over its secure connection")
            }
            TlsError::ClockNotSet => {
                f.write_str("the clock isn't set, so certificates can't be checked")
            }
            TlsError::InvalidServerName => f.write_str("that isn't a valid site name"),
            TlsError::Closed => f.write_str("the secure connection is already closed"),
        }
    }
}

impl From<CertError> for TlsError {
    fn from(e: CertError) -> TlsError {
        TlsError::Certificate(e)
    }
}

impl From<IoError> for TlsError {
    fn from(e: IoError) -> TlsError {
        TlsError::Io(e)
    }
}
