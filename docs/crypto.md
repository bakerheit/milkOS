# Crypto

All the cryptography in milkOS is written from scratch and lives in **one place**: the
`milkcrypto` crate at `<repo>/crypto`. It is `no_std` + `alloc`, has no dependencies, and is
linked into both worlds:

| where | how | adds |
|---|---|---|
| kernel | `kernel/Cargo.toml` path dependency; `kernel/src/crypto/mod.rs` re-exports it as `crate::crypto::*` | `crypto::rng`: the CSPRNG behind `/dev/random`, `/dev/urandom`, getrandom(278) |
| userland | `user/rt/Cargo.toml` path dependency; `rt::crypto` (plus the old paths `rt::sha256`, `rt::sha512`, `rt::ed25519`) | nothing: programs use the primitives directly |

`crypto` is a member of the root (kernel) workspace, so `cargo clippy` / `cargo fmt` at the
root cover it. `kernel/build.rs` watches it too, so userland rebuilds when it changes.

## Modules

| module | what | test vectors (`selftest()`) |
|---|---|---|
| `chacha20` | ChaCha20 block function and stream cipher (RFC 8439), `hchacha20` (the XChaCha20 subkey) | RFC 8439 2.3.2, 2.4.2; draft-irtf-cfrg-xchacha 2.2.1 |
| `sha256` | SHA-256 (FIPS 180-4), streaming + one-shot | NIST |
| `sha512` | SHA-512 (FIPS 180-4) | NIST |
| `ed25519` | Ed25519 sign / verify (RFC 8032) | RFC 8032 7.1 |
| `field25519` (private) | GF(2^255-19), shared by Ed25519 and X25519; constant-time `cswap` | via the curves |
| crate root | `hex`, `unhex`, `unhex_vec`, `ct_eq` (constant-time compare), `wipe` (volatile zeroing) | |

`milkcrypto::selftest()` runs every module's vectors. The kernel prints them at boot
(`[crypto] test vectors: chacha20 4/4 sha256 5/5 ...: ok`).

**Adding a primitive** (e.g. XChaCha20-Poly1305, a password KDF): put it in
`crypto/src/<name>.rs`, give it a `selftest()` with the standard's vectors, list it in
`lib.rs` and here. Build on what is there: XChaCha20 = `hchacha20(key, nonce[..16])` and
then ChaCha20 with that subkey and nonce `[0, 0, 0, 0] || nonce[16..24]`. Don't copy
primitives into other crates.

The kernel's audit log keeps its own SHA-256 (`kernel/src/audit/sha256.rs`) on purpose:
`audit verify` checks the kernel's hash chain with an independent implementation.

## The kernel CSPRNG (`kernel/src/crypto/rng.rs`)

* **Sources.**
  * **virtio-rng** (`kernel/src/virtio/rng.rs`): qemu
    `-object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0`,
    which every boot script in `scripts/` passes. The driver is polled: post a buffer, spin
    with a timeout. No IRQ is involved.
  * **Timer jitter**: CNTVCT around a small data-dependent loop.
  * Writes to `/dev/random` (mixed in, no entropy credited).
* **Pool.** A SHA-256 state. Every input is hashed into it, and it chains across reseeds.
* **Generator.** ChaCha20 with **fast key erasure**. Each request (in 4 KiB pieces) runs
  the keystream under the current key; the first 32 bytes become the next key at once, the
  rest is output, and the scratch is wiped. A key is used once, and an attacker who reads
  memory later can't reconstruct earlier output (forward secrecy).
* **Reseed.** key = SHA-256("milkOS rng reseed" || key || pool || counter). This happens
  at boot (64 virtio-rng bytes + 4096 jitter samples), then on the first request after
  60 s or 1 MiB of output (32 fresh virtio-rng bytes + jitter).
* **Blocking.** Only until the first seed, which happens at boot right after the virtio
  probe, before any process exists. In practice it never blocks.
* **No virtio-rng**: it seeds from jitter alone and prints a boxed `WARNING` at boot, and
  `/proc/rng` says `source=jitter-only(WEAK)`.
* **Concurrency.** One generator behind a spinlock (not per-CPU); requests are cut into
  4 KiB pieces, so the lock is held for about 64 blocks at a time.
* **Boot self-test**:
  * the ChaCha20 vectors;
  * key erasure: the key moves on every request;
  * on 64 KiB of output: the share of one bits (within 5 sigma of 50%), a chi-square over
    byte values (< 400 with 255 dof), and no repeated 16-byte blocks.

  It looks like `[rng] ChaCha20 CSPRNG seeded from virtio-rng (64 bytes) + 4096 jitter
  samples; chacha20 4/4 vectors, key erasure ok, 64 KiB: 49.97% ones, chi2 211.5 (255 dof),
  0 repeated blocks: ok`. That catches a broken generator, not a weak seed.
* **Users.**
  * `/dev/random` and `/dev/urandom` (devfs);
  * getrandom(278), which reads `/dev/urandom`;
  * TCP initial sequence numbers: RFC 6528, SHA-256 of the 4-tuple keyed by a boot-time
    secret from the RNG, plus a 4 µs clock.
* **`/proc/rng`**: `seeded= source= reseeds= device_bytes= output_bytes=`.

Not constant-time: `ed25519` (scalar multiplication branches on bits; it signs with Milk
Drop identities, which a local attacker could time). The field arithmetic itself is
branch-free.
