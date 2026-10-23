//! tls: the TLS 1.3 client of milkOS (RFC 8446), `no_std` + `alloc`, on milkcrypto only.
//! The kernel's Browser uses it (kernel/src/net/tls.rs) and so can user programs (rt::tls).
//!
//! ```ignore
//! let client = tls::Client::new(tls::Config::new(roots, clock, rng));
//! let mut s = client.connect("example.org", tcp)?;      // tcp: impl tls::Transport
//! s.write_all(b"GET / HTTP/1.1\r\nHost: example.org\r\n\r\n")?;
//! let n = s.read(&mut buf)?;                            // 0 = the server closed cleanly
//! s.close()?;                                           // close_notify
//! ```
//!
//! * Protocol: TLS 1.3 only. TLS_AES_128_GCM_SHA256 and TLS_CHACHA20_POLY1305_SHA256;
//!   X25519, secp256r1 and secp384r1 (a key share for the first, HelloRetryRequest for the
//!   others); SNI, ALPN (http/1.1 by default); NewSessionTicket (parsed, dropped) and
//!   KeyUpdate (both ways). No 0-RTT, no resumption, no client certificates (an empty
//!   Certificate if a server asks).
//! * Certificates: always verified ([`verify`]): chain to a [`RootStore`] anchor, validity
//!   against the [`Clock`], basicConstraints, key usage, serverAuth, path length, name
//!   constraints, the host name against subjectAltName ([`name`]). There is no switch to
//!   turn this off.
//! * Errors: [`TlsError`]'s `Display` is a sentence the Browser can show.
//!
//! Module map: [`client`] handshake, [`record`] record layer, [`keys`] key schedule,
//! [`x509`] + [`der`] certificates, [`verify`] path validation, [`name`] host names,
//! [`roots`] trust anchors, [`error`] errors. See docs/tls.md.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod client;
pub mod codec;
pub mod der;
pub mod error;
pub mod interop;
pub mod keys;
pub mod name;
pub mod record;
pub mod roots;
pub mod verify;
pub mod x509;

#[cfg(test)]
mod tests;

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

pub use error::{Alert, CertError, IoError, TlsError};
pub use keys::CipherSuite;
pub use name::ServerName;
pub use roots::RootStore;
pub use verify::Policy;

use client::PostHandshake;
use keys::{RecordKeys, Secret};
use record::{Incoming, RecordLayer};

/// Times before this (2020-01-01) mean the clock was never set.
pub const EARLIEST_PLAUSIBLE_TIME: u64 = 1_577_836_800;

/// A byte stream to run TLS over (a TCP connection).
pub trait Transport {
    /// Read some bytes into `buf` (blocking until there is at least one); Ok(0) when the
    /// other side has closed.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError>;
    /// Write all of `data`.
    fn write_all(&mut self, data: &[u8]) -> Result<(), IoError>;
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        (**self).read(buf)
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), IoError> {
        (**self).write_all(data)
    }
}

/// A cryptographically secure random number generator (the kernel CSPRNG, getrandom).
pub trait Rng: Send + Sync {
    fn fill(&self, out: &mut [u8]);
}

impl<F: Fn(&mut [u8]) + Send + Sync> Rng for F {
    fn fill(&self, out: &mut [u8]) {
        self(out)
    }
}

/// The wall clock: Unix seconds, None if unknown.
pub trait Clock: Send + Sync {
    fn unix_seconds(&self) -> Option<u64>;
}

impl<F: Fn() -> Option<u64> + Send + Sync> Clock for F {
    fn unix_seconds(&self) -> Option<u64> {
        self()
    }
}

/// Key exchange groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamedGroup {
    X25519,
    Secp256r1,
    Secp384r1,
}

impl NamedGroup {
    pub fn id(self) -> u16 {
        match self {
            NamedGroup::X25519 => 0x001d,
            NamedGroup::Secp256r1 => 0x0017,
            NamedGroup::Secp384r1 => 0x0018,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            NamedGroup::X25519 => "X25519",
            NamedGroup::Secp256r1 => "secp256r1",
            NamedGroup::Secp384r1 => "secp384r1",
        }
    }
}

/// A ClientHello to send instead of building one, with its X25519 secret (the RFC 8448
/// replay test only).
#[cfg(test)]
pub(crate) struct TestHello {
    pub client_hello: Vec<u8>,
    pub x25519_secret: [u8; 32],
}

/// What a client trusts and offers. The trust store, clock and random numbers come from the
/// host (kernel or user space), so the crate stays `no_std`.
pub struct Config {
    pub roots: Arc<RootStore>,
    pub clock: Arc<dyn Clock>,
    pub rng: Arc<dyn Rng>,
    /// ALPN protocols to offer, most preferred first ([] = no ALPN).
    pub alpn: Vec<Vec<u8>>,
    /// Cipher suites to offer, most preferred first.
    pub cipher_suites: Vec<CipherSuite>,
    /// Groups to offer, most preferred first; the first one gets a key share.
    pub groups: Vec<NamedGroup>,
    /// RSA key sizes accepted (2048..=4096 by default).
    pub policy: Policy,
    /// Send a KeyUpdate after this many records under one key (AES-GCM's limits are far
    /// beyond any page; this keeps us inside them anyway).
    pub rekey_after_records: u64,
    /// Middlebox compatibility mode (RFC 8446 D.4), as browsers do.
    pub middlebox_compat: bool,
    #[cfg(test)]
    pub(crate) test_hello: Option<TestHello>,
    #[cfg(test)]
    pub(crate) test_skip_certificate_checks: bool,
}

impl Config {
    /// The defaults: ChaCha20-Poly1305 then AES-128-GCM (we have no AES instructions, and
    /// ChaCha20 is faster in software), X25519 then P-256 then P-384, ALPN http/1.1.
    pub fn new(roots: Arc<RootStore>, clock: Arc<dyn Clock>, rng: Arc<dyn Rng>) -> Config {
        Config {
            roots,
            clock,
            rng,
            alpn: vec![b"http/1.1".to_vec()],
            cipher_suites: vec![CipherSuite::Chacha20Poly1305Sha256, CipherSuite::Aes128GcmSha256],
            groups: vec![NamedGroup::X25519, NamedGroup::Secp256r1, NamedGroup::Secp384r1],
            policy: Policy::default(),
            rekey_after_records: 1 << 23,
            middlebox_compat: true,
            #[cfg(test)]
            test_hello: None,
            #[cfg(test)]
            test_skip_certificate_checks: false,
        }
    }
}

/// Opens TLS connections with one [`Config`].
pub struct Client {
    config: Arc<Config>,
}

impl Client {
    pub fn new(config: Config) -> Client {
        Client {
            config: Arc::new(config),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Handshake with `server_name` (a DNS name or an IP literal: what the certificate must
    /// name) over `transport`. On failure the server is sent the fitting alert.
    pub fn connect<T: Transport>(&self, server_name: &str, transport: T) -> Result<Stream<T>, TlsError> {
        let name = ServerName::parse(server_name).ok_or(TlsError::InvalidServerName)?;
        let mut rl = RecordLayer::new(transport);
        match client::handshake(&self.config, &name, &mut rl) {
            Ok(s) => Ok(Stream {
                config: self.config.clone(),
                rl,
                info: ConnectionInfo {
                    suite: s.suite,
                    group: s.group,
                    alpn: s.alpn,
                    hello_retry: s.hello_retry,
                    certificates: s.certificates,
                    server_name: name,
                },
                client_secret: s.client_secret,
                server_secret: s.server_secret,
                plain: Vec::new(),
                plain_at: 0,
                state: State::Open,
                tickets: 0,
                key_updates_received: 0,
                post_handshake_run: 0,
            }),
            Err(e) => {
                if let Some(a) = e.alert() {
                    rl.send_alert(a);
                }
                Err(e)
            }
        }
    }
}

/// What was negotiated, for the Browser's padlock.
#[derive(Clone, Debug)]
pub struct ConnectionInfo {
    pub suite: CipherSuite,
    pub group: NamedGroup,
    pub alpn: Option<Vec<u8>>,
    pub hello_retry: bool,
    /// The server's certificates as sent, leaf first.
    pub certificates: Vec<Vec<u8>>,
    pub server_name: ServerName,
}

impl ConnectionInfo {
    /// "TLS 1.3, TLS_AES_128_GCM_SHA256, X25519".
    pub fn summary(&self) -> String {
        let mut s = String::from("TLS 1.3, ");
        s.push_str(self.suite.name());
        s.push_str(", ");
        s.push_str(self.group.name());
        s
    }

    /// Who issued the server's certificate (organisation or common name).
    pub fn issuer(&self) -> Option<String> {
        x509::Certificate::parse(self.certificates.first()?).ok()?.issuer_display()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    /// We received close_notify: reads return 0; writes still allowed (half close).
    PeerClosed,
    /// We sent close_notify, or an error ended the connection.
    Closed,
}

/// Post-handshake messages we take in a row without application data before calling it abuse.
const MAX_POST_HANDSHAKE_RUN: usize = 256;

/// An established TLS connection.
pub struct Stream<T: Transport> {
    config: Arc<Config>,
    rl: RecordLayer<T>,
    info: ConnectionInfo,
    client_secret: Secret,
    server_secret: Secret,
    /// Decrypted application data not yet handed out.
    plain: Vec<u8>,
    plain_at: usize,
    state: State,
    pub tickets: usize,
    pub key_updates_received: usize,
    post_handshake_run: usize,
}

impl<T: Transport> Stream<T> {
    pub fn info(&self) -> &ConnectionInfo {
        &self.info
    }

    /// Fail and end the connection (sending the fitting alert).
    fn fail(&mut self, e: TlsError) -> TlsError {
        if self.state != State::Closed {
            if let Some(a) = e.alert() {
                self.rl.send_alert(a);
            }
            self.state = State::Closed;
        }
        e
    }

    /// Read decrypted application data into `buf`. Ok(0) once the server has sent
    /// close_notify; [`TlsError::Truncated`] if the connection ended without one.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.plain_at < self.plain.len() {
                let n = buf.len().min(self.plain.len() - self.plain_at);
                buf[..n].copy_from_slice(&self.plain[self.plain_at..self.plain_at + n]);
                self.plain_at += n;
                if self.plain_at == self.plain.len() {
                    self.plain.clear();
                    self.plain_at = 0;
                }
                return Ok(n);
            }
            match self.state {
                State::Open => {}
                State::PeerClosed => return Ok(0),
                State::Closed => return Err(TlsError::Closed),
            }
            match self.read_record() {
                Ok(()) => {}
                Err(e) => return Err(self.fail(e)),
            }
        }
    }

    /// One record: application data into `plain`, post-handshake messages acted on, alerts.
    fn read_record(&mut self) -> Result<(), TlsError> {
        match self.rl.read()? {
            Incoming::ApplicationData(d) => {
                self.post_handshake_run = 0;
                self.plain = d;
                self.plain_at = 0;
            }
            Incoming::Alert(Alert::CLOSE_NOTIFY) => self.state = State::PeerClosed,
            Incoming::Alert(a) => {
                self.state = State::Closed;
                return Err(TlsError::PeerAlert(a));
            }
            Incoming::Handshake => {
                while let Some(m) = self.rl.take_handshake()? {
                    self.post_handshake_run += 1;
                    if self.post_handshake_run > MAX_POST_HANDSHAKE_RUN {
                        return Err(TlsError::unexpected("endless post-handshake messages"));
                    }
                    match client::post_handshake(&m)? {
                        PostHandshake::Ticket => self.tickets += 1,
                        PostHandshake::KeyUpdate { respond } => {
                            if self.rl.handshake_pending() {
                                return Err(TlsError::unexpected("handshake data across a key change"));
                            }
                            self.key_updates_received += 1;
                            self.server_secret = keys::next_traffic_secret(&self.server_secret);
                            self.rl.read_keys = Some(RecordKeys::new(self.info.suite, &self.server_secret));
                            if respond && self.state != State::Closed {
                                self.update_write_key(false)?;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Send KeyUpdate (`request`: ask the server to update too), then switch our key.
    fn update_write_key(&mut self, request: bool) -> Result<(), TlsError> {
        self.rl.write(record::CT_HANDSHAKE, &client::key_update_message(request))?;
        self.client_secret = keys::next_traffic_secret(&self.client_secret);
        self.rl.write_keys = Some(RecordKeys::new(self.info.suite, &self.client_secret));
        Ok(())
    }

    /// Ask the server for a KeyUpdate both ways (it answers with its own).
    pub fn key_update(&mut self) -> Result<(), TlsError> {
        if self.state == State::Closed {
            return Err(TlsError::Closed);
        }
        self.update_write_key(true).map_err(|e| self.fail(e))
    }

    /// Write all of `data` as application data.
    pub fn write_all(&mut self, data: &[u8]) -> Result<(), TlsError> {
        if self.state == State::Closed {
            return Err(TlsError::Closed);
        }
        for chunk in data.chunks(record::MAX_PLAINTEXT) {
            if self.rl.write_keys.as_ref().is_some_and(|k| k.seq >= self.config.rekey_after_records) {
                self.update_write_key(false).map_err(|e| self.fail(e))?;
            }
            self.rl
                .write(record::CT_APPLICATION_DATA, chunk)
                .map_err(|e| self.fail(e))?;
        }
        Ok(())
    }

    /// Same as [`Stream::write_all`], returning `data.len()`.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, TlsError> {
        self.write_all(data)?;
        Ok(data.len())
    }

    /// Send close_notify. Reading afterwards is an error; the transport stays open for the
    /// caller to close.
    pub fn close(&mut self) -> Result<(), TlsError> {
        if self.state == State::Closed {
            return Ok(());
        }
        self.state = State::Closed;
        self.rl.write(record::CT_ALERT, &[1, 0])
    }

    /// The transport, for the caller to close.
    pub fn into_inner(self) -> T {
        self.rl.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.rl.transport
    }
}

/// A TLS stream is itself a transport (so an HTTP client written against [`Transport`]
/// runs over it unchanged).
impl<T: Transport> Transport for Stream<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        Stream::read(self, buf).map_err(|e| IoError::Failed(e.to_string()))
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), IoError> {
        Stream::write_all(self, data).map_err(|e| IoError::Failed(e.to_string()))
    }
}
