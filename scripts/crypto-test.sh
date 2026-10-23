#!/usr/bin/env bash
# Host tests of the milkcrypto crate (<repo>/crypto) on the Mac, no VM: every module's
# boot self-test vectors, plus the longer vector sets and hostile-input cases kept out of
# the boot self-test (AES-GCM, ECDSA P-256/P-384, RSA PKCS#1 v1.5 / PSS, HKDF, SHA-384).
# Prints "crypto-unit: N passed" and fails if none ran. See scripts/hosttest.sh, docs/crypto.md.
exec "$(dirname "$0")/hosttest.sh" crypto crypto-unit
