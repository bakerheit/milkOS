//! The record layer (RFC 8446 5): framing over the transport, protection with the current
//! keys, padding removal, the change_cipher_spec compatibility records, and reassembly of
//! handshake messages that span or share records.

use alloc::vec;
use alloc::vec::Vec;

use crate::Transport;
use crate::error::{Alert, TlsError};
use crate::keys::RecordKeys;

pub const CT_CHANGE_CIPHER_SPEC: u8 = 20;
pub const CT_ALERT: u8 = 21;
pub const CT_HANDSHAKE: u8 = 22;
pub const CT_APPLICATION_DATA: u8 = 23;

/// Largest plaintext fragment (2^14).
pub const MAX_PLAINTEXT: usize = 16384;
/// Largest protected record body (2^14 + 256).
const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + 256;
/// Largest handshake message we accept (a long certificate chain fits easily).
pub const MAX_HANDSHAKE: usize = 1 << 17;

/// What [`RecordLayer::read`] found.
pub enum Incoming {
    Handshake,
    ApplicationData(Vec<u8>),
    /// A fatal alert, or close_notify.
    Alert(Alert),
}

pub struct RecordLayer<T: Transport> {
    pub transport: T,
    /// Bytes read from the transport, not yet a whole record.
    rbuf: Vec<u8>,
    /// Handshake bytes not yet a whole message.
    hs: Vec<u8>,
    pub read_keys: Option<RecordKeys>,
    pub write_keys: Option<RecordKeys>,
    /// Until the server's Finished: drop unprotected change_cipher_spec records.
    pub allow_ccs: bool,
    /// The record version we write: 0x0301 for the first ClientHello, then 0x0303.
    pub write_version: u16,
    /// The server sent close_notify, or the transport ended.
    pub eof: bool,
    /// The handshake is over (EOF without close_notify is now truncation).
    pub established: bool,
    /// Records in a row that carried nothing (change_cipher_spec, user_canceled, empty
    /// application data): a server dribbling them can't keep us busy forever.
    skipped: usize,
}

/// Most content-free records in a row.
const MAX_SKIPPED: usize = 32;

impl<T: Transport> RecordLayer<T> {
    pub fn new(transport: T) -> RecordLayer<T> {
        RecordLayer {
            transport,
            rbuf: Vec::new(),
            hs: Vec::new(),
            read_keys: None,
            write_keys: None,
            allow_ccs: true,
            write_version: 0x0301,
            eof: false,
            established: false,
            skipped: 0,
        }
    }

    fn fill(&mut self, want: usize) -> Result<(), TlsError> {
        let mut chunk = vec![0u8; 8192];
        while self.rbuf.len() < want {
            let n = self.transport.read(&mut chunk)?;
            if n == 0 {
                self.eof = true;
                return Err(if self.established {
                    TlsError::Truncated
                } else {
                    TlsError::ConnectionClosed
                });
            }
            self.rbuf.extend_from_slice(&chunk[..n.min(chunk.len())]);
        }
        Ok(())
    }

    /// Read one record and act on it: handshake bytes go to the reassembly buffer, the rest
    /// is returned. Unprotected change_cipher_spec records during the handshake are dropped.
    pub fn read(&mut self) -> Result<Incoming, TlsError> {
        loop {
            self.fill(5)?;
            let ctype = self.rbuf[0];
            let len = u16::from_be_bytes([self.rbuf[3], self.rbuf[4]]) as usize;
            let limit = if self.read_keys.is_some() {
                MAX_CIPHERTEXT
            } else {
                MAX_PLAINTEXT
            };
            if len > limit {
                return Err(TlsError::proto(Alert::RECORD_OVERFLOW, "a record was too long"));
            }
            if !matches!(
                ctype,
                CT_CHANGE_CIPHER_SPEC | CT_ALERT | CT_HANDSHAKE | CT_APPLICATION_DATA
            ) {
                return Err(TlsError::unexpected("a record of unknown type"));
            }
            self.fill(5 + len)?;
            let header: [u8; 5] = [self.rbuf[0], self.rbuf[1], self.rbuf[2], self.rbuf[3], self.rbuf[4]];
            let mut body: Vec<u8> = self.rbuf[5..5 + len].to_vec();
            self.rbuf.drain(..5 + len);
            self.skipped += 1;
            if self.skipped > MAX_SKIPPED {
                return Err(TlsError::unexpected("too many records with nothing in them"));
            }

            if ctype == CT_CHANGE_CIPHER_SPEC {
                if !self.allow_ccs || body != [1] {
                    return Err(TlsError::unexpected("a change_cipher_spec record"));
                }
                continue;
            }
            let (inner, data) = match &mut self.read_keys {
                Some(keys) => {
                    if ctype != CT_APPLICATION_DATA {
                        return Err(TlsError::unexpected("an unprotected record after the keys changed"));
                    }
                    if body.len() < 17 {
                        return Err(TlsError::Tampered);
                    }
                    let tag_at = body.len() - 16;
                    let mut tag = [0u8; 16];
                    tag.copy_from_slice(&body[tag_at..]);
                    body.truncate(tag_at);
                    if !keys.open(&header, &mut body, &tag) {
                        return Err(TlsError::Tampered);
                    }
                    // TLSInnerPlaintext: content, type, zero padding.
                    let Some(end) = body.iter().rposition(|&b| b != 0) else {
                        return Err(TlsError::unexpected("a record with no content type"));
                    };
                    let inner = body[end];
                    body.truncate(end);
                    if body.len() > MAX_PLAINTEXT {
                        return Err(TlsError::proto(Alert::RECORD_OVERFLOW, "a record was too long"));
                    }
                    (inner, body)
                }
                None => {
                    if ctype == CT_APPLICATION_DATA {
                        return Err(TlsError::unexpected("application data before the handshake"));
                    }
                    (ctype, body)
                }
            };
            if inner != CT_HANDSHAKE && !self.hs.is_empty() {
                return Err(TlsError::unexpected("a handshake message interrupted by another record"));
            }
            match inner {
                CT_HANDSHAKE => {
                    if data.is_empty() {
                        return Err(TlsError::unexpected("an empty handshake record"));
                    }
                    if self.hs.len() + data.len() > MAX_HANDSHAKE + 4 {
                        return Err(TlsError::proto(Alert::RECORD_OVERFLOW, "a handshake message was too long"));
                    }
                    self.hs.extend_from_slice(&data);
                    self.skipped = 0;
                    return Ok(Incoming::Handshake);
                }
                CT_ALERT => {
                    if data.len() != 2 {
                        return Err(TlsError::decode("a malformed alert"));
                    }
                    let a = Alert(data[1]);
                    if a == Alert::USER_CANCELED {
                        continue; // always followed by close_notify
                    }
                    if a == Alert::CLOSE_NOTIFY {
                        self.eof = true;
                    }
                    self.skipped = 0;
                    return Ok(Incoming::Alert(a));
                }
                CT_APPLICATION_DATA if data.is_empty() => continue,
                CT_APPLICATION_DATA => {
                    self.skipped = 0;
                    return Ok(Incoming::ApplicationData(data));
                }
                CT_CHANGE_CIPHER_SPEC => return Err(TlsError::unexpected("a protected change_cipher_spec")),
                _ => return Err(TlsError::unexpected("a record of unknown type")),
            }
        }
    }

    /// A whole handshake message (header included) from the buffer, if there is one.
    pub fn take_handshake(&mut self) -> Result<Option<Vec<u8>>, TlsError> {
        if self.hs.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([0, self.hs[1], self.hs[2], self.hs[3]]) as usize;
        if len > MAX_HANDSHAKE {
            return Err(TlsError::proto(Alert::RECORD_OVERFLOW, "a handshake message was too long"));
        }
        if self.hs.len() < 4 + len {
            return Ok(None);
        }
        let msg: Vec<u8> = self.hs.drain(..4 + len).collect();
        Ok(Some(msg))
    }

    /// Is a handshake message partly buffered? (Keys may only change on a record boundary
    /// with nothing pending: RFC 8446 5.1.)
    pub fn handshake_pending(&self) -> bool {
        !self.hs.is_empty()
    }

    /// Write `data` as records of type `ctype` (fragmented to 2^14), protected if there
    /// are write keys.
    pub fn write(&mut self, ctype: u8, data: &[u8]) -> Result<(), TlsError> {
        let mut out = Vec::with_capacity(data.len() + 32);
        if data.is_empty() {
            // An empty application_data record is allowed (and harmless); an empty record of
            // any other type is not, so nothing is written.
            if ctype == CT_APPLICATION_DATA {
                self.encode(ctype, &[], &mut out)?;
            }
        } else {
            for chunk in data.chunks(MAX_PLAINTEXT) {
                self.encode(ctype, chunk, &mut out)?;
            }
        }
        if !out.is_empty() {
            self.transport.write_all(&out)?;
        }
        Ok(())
    }

    /// One unprotected record regardless of keys (the compatibility change_cipher_spec).
    pub fn write_plain(&mut self, ctype: u8, data: &[u8]) -> Result<(), TlsError> {
        let mut out = Vec::with_capacity(5 + data.len());
        out.push(ctype);
        out.extend_from_slice(&0x0303u16.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
        self.transport.write_all(&out)?;
        Ok(())
    }

    fn encode(&mut self, ctype: u8, chunk: &[u8], out: &mut Vec<u8>) -> Result<(), TlsError> {
        match &mut self.write_keys {
            None => {
                out.push(ctype);
                out.extend_from_slice(&self.write_version.to_be_bytes());
                out.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
                out.extend_from_slice(chunk);
            }
            Some(keys) => {
                let body_len = chunk.len() + 1 + 16;
                let header = [
                    CT_APPLICATION_DATA,
                    0x03,
                    0x03,
                    (body_len >> 8) as u8,
                    body_len as u8,
                ];
                let start = out.len();
                out.extend_from_slice(&header);
                out.extend_from_slice(chunk);
                out.push(ctype);
                let tag = keys
                    .seal(&header, &mut out[start + 5..])
                    .ok_or(TlsError::proto(Alert::INTERNAL_ERROR, "too many records under one key"))?;
                out.extend_from_slice(&tag);
            }
        }
        Ok(())
    }

    /// Best effort: tell the server why we're giving up.
    pub fn send_alert(&mut self, a: Alert) {
        let level = if a == Alert::CLOSE_NOTIFY || a == Alert::USER_CANCELED {
            1
        } else {
            2
        };
        let _ = self.write(CT_ALERT, &[level, a.0]);
    }
}
