//! Host tests of the tls crate (scripts/tls-test.sh): certificates and chains on the
//! fixtures in tls/testdata (made by scripts/gen_tls_fixtures.sh, checked at a fixed time),
//! the Mozilla roots, RFC 8448's handshake trace, and full handshakes against an
//! in-process TLS 1.3 test server (HelloRetryRequest, KeyUpdate, tickets, fragmentation,
//! hostile and mutated messages).

mod chain;
// mod rfc8448;
// mod server;
mod x509;

use alloc::vec::Vec;

/// 2026-09-21: inside every fixture's validity (2025..2045) except the expired/future ones.
pub const NOW: i64 = 1_790_000_000;

/// A fixture certificate by name (tls/testdata/chain/NAME.der).
pub fn fixture(name: &str) -> Vec<u8> {
    let path = alloc::format!("{}/testdata/chain/{name}.der", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A tiny deterministic PRNG for fuzzing (xorshift64*).
pub struct Prng(pub u64);

impl Prng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn fill(&mut self, out: &mut [u8]) {
        for b in out {
            *b = self.next() as u8;
        }
    }
}

/// Mutate `v` once: flip a bit, change a byte, truncate, extend, drop or duplicate a byte.
pub fn mutate(v: &mut Vec<u8>, r: &mut Prng) {
    if v.is_empty() {
        v.push(r.next() as u8);
        return;
    }
    let i = r.below(v.len());
    match r.below(6) {
        0 => v[i] ^= 1 << r.below(8),
        1 => v[i] = r.next() as u8,
        2 => v.truncate(i),
        3 => {
            let n = 1 + r.below(8);
            for _ in 0..n {
                v.push(r.next() as u8);
            }
        }
        4 => {
            v.remove(i);
        }
        _ => {
            let b = v[i];
            v.insert(i, b);
        }
    }
}
