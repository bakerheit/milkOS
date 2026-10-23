# Crypto

All the cryptography in milkOS is written from scratch and lives in **one place**: the
`milkcrypto` crate at `<repo>/crypto`. It is `no_std` + `alloc`, has no dependencies, and is
linked into both worlds:

| where | how | adds |
|---|---|---|
| kernel | `kernel/Cargo.toml` path dependency; `kernel/src/crypto/mod.rs` re-exports it as `crate::crypto::*` | `crypto::rng`: the CSPRNG behind `/dev/random`, `/dev/urandom`, getrandom(278) |
| userland | `user/rt/Cargo.toml` path dependency; `rt::crypto` (plus the old paths `rt::sha256`, `rt::sha512`, `rt::ed25519`) | nothing: programs use the primitives directly |
| tls | `tls/Cargo.toml` path dependency (the TLS 1.3 client, docs/tls.md) | random numbers and the clock come from its host |
| milkfs | `milkfs/Cargo.toml` path dependency (the file system crate, kernel and host tests) | nothing: random bytes and scrypt's memory come from its host (`milkfs::platform`) |

`crypto` is a member of the root (kernel) workspace, so `cargo clippy` / `cargo fmt` at the
root cover it. `kernel/build.rs` watches it too, so userland rebuilds when it changes.

## Modules

| module | what | test vectors (`selftest()`) |
|---|---|---|
| `chacha20` | ChaCha20 block function and stream cipher (RFC 8439), `hchacha20` (the XChaCha20 subkey) | RFC 8439 2.3.2, 2.4.2; draft-irtf-cfrg-xchacha 2.2.1 |
| `sha256` | SHA-256 (FIPS 180-4), streaming + one-shot | NIST |
| `sha512` | SHA-512 (FIPS 180-4) | NIST |
| `ed25519` | Ed25519 sign / verify (RFC 8032) | RFC 8032 7.1 |
| `poly1305` | Poly1305 one-time MAC (RFC 8439 2.5), 26-bit limbs, streaming | RFC 8439 2.5.2 (one-shot + streamed), A.3 #6 and #7 (the 2^130-5 carries) |
| `aead` | ChaCha20-Poly1305 (RFC 8439 2.8): `seal` / `open` (tag checked in constant time before decrypting) | RFC 8439 2.8.2, plus a flipped bit and a wrong AAD |
| `x25519` | X25519 (RFC 7748): Montgomery ladder, constant-time swap, `public_key`, `shared` (None for an all-zero result) | RFC 7748 5.2 (both), 6.1, the iterated test after 1 and 1000 rounds |
| `xchacha20` | XChaCha20 stream (`xor`) and XChaCha20-Poly1305 (`seal` / `open`, 24-byte nonce, safe to pick at random), on `hchacha20` + ChaCha20 / `aead` | draft-irtf-cfrg-xchacha-03 A.3.1, plus a wrong tag |
| `hmac` | HMAC-SHA256 (`HmacSha256`, `hmac_sha256`), PBKDF2-HMAC-SHA256 | RFC 4231 cases 1, 2, 6; RFC 7914 11 |
| `scrypt` | scrypt (RFC 7914), the memory-hard password KDF; its big array comes from a `Scratch` the caller provides (`VecScratch` on the heap, or the kernel's page frames) | RFC 7914 12, both vectors |
| `sha512` (also) | SHA-384 (`Sha384`, `sha384`): SHA-512 with its own IV, cut to 48 bytes | NIST (3) |
| `hash` | the `Hash` trait over SHA-256/384/512, so HMAC and HKDF are written once | via hmac, hkdf |
| `hmac` (also) | `Hmac<H>` over any `Hash` | RFC 4231 1, 2, 6 for SHA-384/512 |
| `hkdf` | HKDF extract / expand (RFC 5869) over any `Hash` (the TLS 1.3 key schedule) | RFC 5869 A.1-A.3; SHA-384 cross-checked with Python |
| `aes` | AES-128 (FIPS 197), bitsliced (BearSSL's aes_ct64 layout: 4 blocks in 8 u64 bit planes, the Boyar-Peralta S-box circuit, also in the key schedule): no table indexed by secret data | FIPS 197 B, C.1 |
| `ghash`, `gcm` | GHASH as carry-less multiplies built from integer multiplies with masked holes (BearSSL's ghash_ctmul64), and AES-128-GCM (`Aes128Gcm`, `seal` / `open`, the tag checked before decrypting); ~200 MB/s on the Mac | GCM spec cases 1-4 + tamper; host: Go's AES-GCM table, a reference, every bit flip |
| `bignum`, `rsa` | unsigned big integers (Montgomery, variable time: only public data) and RSA PKCS#1 v1.5 / PSS verification, 512..8192-bit moduli, e < 2^33 | OpenSSL-made signatures; host: 2048-4096-bit keys, SHA-256/384/512, hostile encodings, fuzz |
| `nist` (private), `p256`, `p384` | NIST P-256 / P-384: constant-time Montgomery fields (u64 limbs, masks), complete a = -3 formulas (Renes-Costello-Batina), a fixed 4-bit window with masked table scans; ECDH (`public_key`, `shared`, the peer point checked) and ECDSA verification | NIST CDH, SigVer; host: NIST SigVer (Go's copy), OpenSSL vectors, group laws, hostile points and signatures |
| `ecdsa` | strict DER ECDSA signatures (`parse_der`) | host: every prefix and mutation |
| `field25519` (private) | GF(2^255-19), shared by Ed25519 and X25519; constant-time `cswap` | via the curves |
| crate root | `hex`, `unhex`, `unhex_vec`, `ct_eq` (constant-time compare), `wipe` (volatile zeroing) | |

`milkcrypto::selftest()` runs every module's vectors. The longer sets and the hostile-input
cases are host tests: `scripts/crypto-test.sh` (check-all smoke set). The kernel prints them at boot
(`[crypto] test vectors: chacha20 4/4 sha256 5/5 ...: ok`).

**Why scrypt (not Argon2id)** for passwords: memory-hard, standardised with test vectors,
and it needs only Salsa20/8 + PBKDF2-HMAC-SHA256 on top of what is here (Argon2 needs
BLAKE2b). milkfs uses N = 2^16, r = 8, p = 4 (64 MiB, ~0.35 s). In the kernel,
`crypto::frames::scrypt` puts the array in page frames taken for the call and handed back
wiped (the kernel heap never shrinks); the fs self-test runs RFC 7914's vector through it.
**HMAC-SHA256** is the PRF (key derivation, milkfs's per-block synthetic IV, where a PRF is
required and Poly1305, a one-time MAC, wouldn't do). Users: milkfs encryption at rest
and encrypted `disks send` streams (XChaCha20-Poly1305 per record with a counter nonce,
scrypt-wrapped stream key; `milkfs/src/crypt.rs`, docs/filesystem.md); its salts and nonces come from
`crypto::rng::fill`. `scripts/mkmilkfs.py` has its own stdlib-only copy for the Mac side
(`mkmilkfs.py selftest-crypto` runs the same vectors).

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
* **Userland users of the primitives**:
  * `apps`: Ed25519 package signatures, SHA-256;
  * `audit verify`: SHA-256;
  * Milk Drop (`user/drop`): Ed25519 identities, X25519 per connection,
    ChaCha20-Poly1305 records, SHA-256; `drop selftest` runs every vector in userland.
* **`/proc/rng`**: `seeded= source= reseeds= device_bytes= output_bytes=`.

Not constant-time: `ed25519` (scalar multiplication branches on bits; it signs with Milk
Drop identities, which a local attacker could time). The field arithmetic itself is
branch-free.
