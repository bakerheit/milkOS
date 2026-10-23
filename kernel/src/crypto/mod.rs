//! Kernel crypto: the shared primitives (`<repo>/crypto`, the `milkcrypto` crate, also
//! userland's `rt::crypto`) plus what only the kernel has, the CSPRNG ([`rng`]) behind
//! /dev/random, /dev/urandom and getrandom. See docs/crypto.md.

pub mod frames;
pub mod rng;

pub use milkcrypto::*;

/// Boot: run the shared crate's test vectors (a boot self-test, opt/milkos/selftest=crypto;
/// the CSPRNG checks its own ChaCha20 vectors every boot), then seed the CSPRNG (needs
/// virtio-rng probed, so right after `virtio::init`).
pub fn init() {
    if crate::selftest::wants(crate::selftest::CRYPTO) {
        vectors();
    }
    rng::init();
}

fn vectors() {
    let mut line = alloc::string::String::new();
    let mut all = true;
    for (name, p, t) in milkcrypto::selftest() {
        line.push_str(&alloc::format!(" {name} {p}/{t}"));
        all &= p == t;
    }
    crate::kprintln!(
        "[crypto] test vectors:{line}: {}",
        if all { "ok" } else { "FAILED" }
    );
}
