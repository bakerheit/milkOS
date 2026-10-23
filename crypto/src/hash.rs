//! A small trait over the SHA-2 hashes, so HMAC and HKDF ([`crate::hmac::Hmac`],
//! [`crate::hkdf`]) are written once for SHA-256, SHA-384 and SHA-512.

use crate::sha256::Sha256;
use crate::sha512::{Sha384, Sha512};

pub trait Hash: Clone {
    /// Digest length in bytes.
    const OUT: usize;
    /// Block length in bytes (the HMAC key pad).
    const BLOCK: usize;
    fn new() -> Self;
    fn input(&mut self, data: &[u8]);
    /// Write the digest (OUT bytes) to the front of `out`, which must be at least that long.
    fn result(&mut self, out: &mut [u8]);
}

/// The longest digest of the hashes here (SHA-512), for stack buffers.
pub const MAX_OUT: usize = 64;
/// The longest block (SHA-384/512).
pub const MAX_BLOCK: usize = 128;

impl Hash for Sha256 {
    const OUT: usize = 32;
    const BLOCK: usize = 64;
    fn new() -> Self {
        Sha256::new()
    }
    fn input(&mut self, data: &[u8]) {
        self.update(data);
    }
    fn result(&mut self, out: &mut [u8]) {
        out[..32].copy_from_slice(&self.finish());
    }
}

impl Hash for Sha384 {
    const OUT: usize = 48;
    const BLOCK: usize = 128;
    fn new() -> Self {
        Sha384::new()
    }
    fn input(&mut self, data: &[u8]) {
        self.update(data);
    }
    fn result(&mut self, out: &mut [u8]) {
        out[..48].copy_from_slice(&self.finish());
    }
}

impl Hash for Sha512 {
    const OUT: usize = 64;
    const BLOCK: usize = 128;
    fn new() -> Self {
        Sha512::new()
    }
    fn input(&mut self, data: &[u8]) {
        self.update(data);
    }
    fn result(&mut self, out: &mut [u8]) {
        out[..64].copy_from_slice(&self.finish());
    }
}

/// One-shot digest into `out` (OUT bytes are written).
pub fn digest<H: Hash>(data: &[u8], out: &mut [u8]) {
    let mut h = H::new();
    h.input(data);
    h.result(out);
}
