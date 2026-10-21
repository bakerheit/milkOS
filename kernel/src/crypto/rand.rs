//! Random bytes for salts and nonces.
//!
//! TODO(net CSPRNG): the net agent is adding virtio-rng and a ChaCha20 CSPRNG behind
//! getrandom and /dev/urandom; switch `fill` to it when it lands. Until then this whitens
//! the current /dev/urandom path (xorshift64* seeded from the timer, NOT cryptographic)
//! plus fresh timer readings through SHA-256 and ChaCha20. Good enough for unique salts and
//! nonces in tests; volume keys are made on the Mac (mkmilkfs.py, os.urandom), not here.

use super::chacha;
use crate::audit::sha256::Sha256;

pub fn fill(out: &mut [u8]) {
    let mut h = Sha256::new();
    for _ in 0..16 {
        h.update(&crate::fs::devfs::random_u64().to_le_bytes());
        h.update(&crate::timer::counter().to_le_bytes());
        h.update(&crate::timer::now_ns().to_le_bytes());
    }
    let mut key = h.finish();
    out.fill(0);
    chacha::xor(&key, 0, &[0u8; 12], out);
    super::ct::zeroize(&mut key);
}
