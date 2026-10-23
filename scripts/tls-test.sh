#!/usr/bin/env bash
# Host tests of the tls crate (<repo>/tls) on the Mac, no VM and no network: RFC 8448's
# handshake trace byte for byte (key schedule, record protection, the whole client replayed),
# X.509 parsing of the Mozilla roots and the test fixtures plus mutated and hostile DER,
# host-name matching, chain building (expired, wrong host, untrusted root, missing
# intermediate, EKU, path length, name constraints, cross-signing), and full handshakes
# against an in-process TLS 1.3 server (HelloRetryRequest, KeyUpdate, tickets,
# fragmentation, client certificate requests, protocol errors, mutated messages).
# Prints "tls-unit: N passed" and fails if none ran. See scripts/hosttest.sh, docs/tls.md.
exec "$(dirname "$0")/hosttest.sh" tls tls-unit
