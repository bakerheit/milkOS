# TLS

milkOS has a TLS 1.3 client (RFC 8446), written from scratch in one shared crate, `tls`
(`<repo>/tls`), `no_std` + `alloc`, on `milkcrypto` only. It is linked into the kernel
(`net::tls`: its self-tests) and into user space (`rt::tls`, used by `network get` and the
Browser through `rt::web` / milkweb's `secure`). `https://` pages are fetched with it (milkweb's
`secure`, see docs/networking.md).

## The API

```rust
let cfg = tls::Config::new(roots, clock, rng);      // Arc<RootStore>, Arc<dyn Clock>, Arc<dyn Rng>
let client = tls::Client::new(cfg);
let mut s = client.connect("example.org", tcp)?;    // tcp: impl tls::Transport
s.write_all(b"GET / HTTP/1.1\r\nHost: example.org\r\n\r\n")?;
let n = s.read(&mut buf)?;                          // Ok(0): the server sent close_notify
s.close()?;                                         // our close_notify
s.info().summary();                                 // "TLS 1.3, TLS_AES_128_GCM_SHA256, X25519"
```

* `Transport`: `read(&mut [u8]) -> Result<usize, IoError>` (0 = closed) and
  `write_all(&[u8])`. A `Stream` is itself a `Transport`.
* The host injects the trust store, the clock and the random numbers: the kernel gives the
  CSPRNG (`crypto::rng::fill`) and the RTC clock (`fs::clock`), user space getrandom(2) and
  CLOCK_REALTIME. `net::tls::connect(host, addr)` and `rt::tls::connect(host, port)` do
  TCP and the handshake in one call.
* `TlsError`'s `Display` is a sentence for the user: "the certificate is for another site",
  "the certificate has expired", "the certificate was not issued by a trusted authority",
  "the site doesn't speak TLS 1.3", "the site and milkOS have no encryption method in
  common", "the secure connection was tampered with", ... (`tls/src/error.rs`). On any
  failure the server is sent the matching alert.
* A read after the TCP connection ended without close_notify is `TlsError::Truncated`
  (milkweb treats it as the end of the body; HTTP's framing tells a cut body).

## What it speaks

| | |
|---|---|
| versions | TLS 1.3 only (a TLS 1.2 server is refused: "doesn't speak TLS 1.3") |
| cipher suites | TLS_CHACHA20_POLY1305_SHA256 (preferred: no AES instructions here), TLS_AES_128_GCM_SHA256 |
| groups | X25519 (the key share sent), secp256r1, secp384r1 (by HelloRetryRequest) |
| signatures (handshake) | ecdsa_secp256r1_sha256, ecdsa_secp384r1_sha384, ed25519, rsa_pss_rsae_sha256/384/512, rsa_pss_pss_sha256/384/512 |
| signatures (certificates) | ECDSA P-256 / P-384 with SHA-256/384/512, RSA PKCS#1 v1.5 and RSA-PSS (2048..4096 bits) with SHA-256/384/512, Ed25519 |
| extensions | server_name (DNS names only), supported_groups, signature_algorithms, ALPN (`http/1.1`), supported_versions, psk_key_exchange_modes, key_share, cookie, padding (RFC 7685) |
| after the handshake | NewSessionTicket (parsed and dropped), KeyUpdate both ways, and our own KeyUpdate after 2^23 records |
| not done | 0-RTT, resumption, client certificates (a CertificateRequest gets an empty Certificate), TLS 1.2, AES-256, P-521, OCSP / CRLs, SCTs, AIA fetching |

Middlebox compatibility mode (a random legacy_session_id, one dummy change_cipher_spec) is on,
as in browsers. Records: 2^14 plaintext, padding stripped, keys only change on a record
boundary, handshake messages may span and share records (up to 128 KiB each), at most 32
content-free records in a row. Secrets (`keys::Secret`, the AEAD keys, IVs, ECDHE scalars)
are wiped when dropped.

## Certificates

Always verified; there is no switch to turn it off (the test-only hooks exist in the crate's
own unit tests and nowhere else). `tls/src/verify.rs`:

* the chain: depth-first from the server's certificate through what it sent (any order,
  extras ignored, cross-signing handled by backtracking) to a trust anchor; at most 6
  intermediates, 48 signature checks and 2^20 name-constraint comparisons per chain;
* validity times against the clock (a clock before 2020 is "the clock isn't set");
* basicConstraints (issuers are v3 CAs; a CA certificate as the server's is refused), key
  usage (keyCertSign, digitalSignature), extended key usage (serverAuth in the server's
  certificate if it has the extension; issuers' must allow serverAuth or any),
  pathLenConstraint, name constraints (dNSName, iPAddress, directoryName; excluded wildcards
  fail closed), unknown critical extensions refused;
* the host name against subjectAltName only (RFC 6125): case-insensitive, a wildcard only as
  the whole leftmost label and never with fewer than two labels after it, IPs only against
  iPAddress entries, no common-name fallback.

DER is parsed strictly (`tls/src/der.rs`, `x509.rs`): definite minimal lengths, the inner and
outer signature algorithms identical, at most one of each extension. Nothing panics on
hostile input (fuzzed in the host tests).

## The root store

`assets/ca/mozilla-roots.der`: the roots of Mozilla's CA program with the websites trust bit
(NSS certdata.txt), 121 certificates (80 RSA, 3 P-256, 37 P-384, 1 P-521 that we can't use),
129,143 bytes of DER, listed with fingerprints in `assets/ca/mozilla-roots.txt`. Made by
`scripts/gen_roots.py` from curl's `cacert.pem` (mk-ca-bundle.pl 1.33, Mozilla data as of
2026-09-25) as Homebrew's ca-certificates installs it; the script records the bundle's date
and SHA-256 and refuses another file (`--new FILE` moves to a newer one, `--check` compares).
The data is under the MPL 2.0 (`assets/ca/NOTICE.txt`, `assets/ca/MPL-2.0.txt`). An anchor is
a name, a key and name constraints (RFC 5280 6.1.1): roots' own validity isn't checked.
Distrust-after dates in certdata aren't applied (curl's bundle drops them).

**Test roots.** A test script may pass a throwaway CA as fw_cfg `opt/milkos/tlstest-root`:
the kernel then prints a WARNING and trusts it on top of the Mozilla roots for that boot
(user space through `/proc/net/tls`'s `test_root=` line: the Browser and `network get`). Normal boots have
no such file; `/proc/net/tls` says `test_root=none`.

## Tests

* `scripts/crypto-test.sh` (crypto-unit) and `scripts/tls-test.sh` (tls-unit), host tests in
  check-all's smoke set. tls-unit: RFC 8448 section 3 byte for byte (the X25519 keys, every
  secret of the key schedule, the protected records, and the whole trace replayed through the
  real client with the RFC's ClientHello swapped in: its Finished, data and close_notify
  records come out identical), X.509 parsing of the 121 roots and the fixtures plus 60,000
  mutations, host names, chains (expired, not yet valid, wrong host, untrusted root, missing
  intermediate, EKU, key usage, path length, not a CA, name constraints, cross-signing,
  unknown critical extensions, weak keys), and handshakes against an in-process TLS 1.3
  server (HelloRetryRequest with and without cookies, KeyUpdate both ways, tickets, 1-byte
  fragments, CertificateRequest, 25 kinds of misbehaviour, mutated messages). The fixtures in
  `tls/testdata/chain` come from `scripts/gen_tls_fixtures.sh` (OpenSSL 3).
* `scripts/tls-interop.sh` (tls-interop, default set): a throwaway CA made at test time,
  OpenSSL 3 `s_server`s on 127.0.0.1 (both suites with RSA and ECDSA certificates, P-256 and
  P-384 only, ALPN, TLS 1.2 only, 1.2 MB downloads), and the same cases run from the Mac
  (`tls/probe`, a std build of the crate), the guest kernel (`net::tls::selftest`, the cases
  in fw_cfg `opt/milkos/tlstest`), and guest user space (`/System/Developer/tlsprobe`); then
  `network get https://` by name (the test DNS of web-test) and by address, and the Browser
  opening an https page and showing the certificate error page. Everything stays on the Mac's
  loopback; the guest is offline but for 10.0.2.2.
