# Cryptography (`kernel/src/crypto/`)

Written from scratch, shared by the kernel: milkfs encryption at rest uses it now, and the
kernel CSPRNG and encrypted Milk Drop are meant to build on it too. Every primitive is
checked against its published test vectors at boot (`crypto::selftest`, run by the fs
self-test: `crypto: 14/14 known-answer tests pass ... in 18 ms`).

| module | what | vectors |
|---|---|---|
| `chacha` | ChaCha20 block and stream (RFC 8439), HChaCha20, XChaCha20 (24-byte nonce) | RFC 8439 2.3.2, 2.4.2; XChaCha draft 2.2.1 |
| `poly1305` | Poly1305 one-time MAC, 26-bit limbs | RFC 8439 2.5.2 |
| `aead` | ChaCha20-Poly1305 and XChaCha20-Poly1305 (`seal`/`open`, `xseal`/`xopen`) | RFC 8439 2.8.2; draft-irtf-cfrg-xchacha-03 A.3.1; a wrong tag is refused |
| `hmac` | HMAC-SHA256, PBKDF2-HMAC-SHA256 (SHA-256 from `audit::sha256`) | RFC 4231 case 2; RFC 7914 11 |
| `scrypt` | scrypt, the memory-hard password KDF | RFC 7914 12 (both vectors, incl. N=1024 r=8 p=16) |
| `ct` | constant-time `eq`, volatile `zeroize` | |
| `rand` | salts and nonces: a stopgap (see below) | |

Why these:

- **ChaCha20 / XChaCha20 rather than AES**: constant time in plain software (adds, rotates,
  xors; no tables, so no cache-timing leaks), fast without AES instructions, and simple
  enough to get right from scratch. XChaCha20's 192-bit nonce can be picked at random.
- **Poly1305 / AEAD** where a nonce and a 16-byte tag can be stored (milkfs key slots).
- **HMAC-SHA256** as a PRF (key derivation, and milkfs's per-block synthetic IV, where a
  PRF is required and Poly1305, a one-time MAC, would not be safe).
- **scrypt rather than Argon2id**: memory-hard, standardised with test vectors, and it
  needs only Salsa20/8 + PBKDF2-HMAC-SHA256 on top of what we have (Argon2 needs BLAKE2b).
  Its big array (N blocks of 128 r bytes) is taken from the frame allocator and handed
  back (wiped) afterwards, not from the kernel heap, which never shrinks.
- **Constant time where it matters**: tag and MAC comparisons (`ct::eq`, no early exit);
  ChaCha and Poly1305 have no secret-dependent branches or lookups (Poly1305's final
  reduction uses masks). Keys and intermediate secrets are zeroized when dropped.

`rand::fill` is a **stopgap**: until the kernel CSPRNG lands (net agent: virtio-rng +
ChaCha20 behind getrandom and /dev/urandom), it whitens the current /dev/urandom path
(xorshift seeded from the timer, not cryptographic) and fresh timer readings through
SHA-256 + ChaCha20. Only salts and nonces come from it; milkfs volume keys are generated
on the Mac (`mkmilkfs.py`, `os.urandom`). Marked `TODO(net CSPRNG)` in the code.
