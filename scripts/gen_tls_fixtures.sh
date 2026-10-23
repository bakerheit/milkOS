#!/usr/bin/env bash
# Regenerate tls/testdata/chain: throwaway certificates for the tls crate's host tests
# (chain building, host names, signature algorithms), made with OpenSSL 3 (Homebrew's by
# default). Run it only to change the fixtures; the tests use the committed DER files and a
# fixed clock (2026-09-21), so they don't change over time.
#
#   scripts/gen_tls_fixtures.sh           OPENSSL=/path/to/openssl to use another one
#
# Private keys are made in a temporary directory and deleted, except the Ed25519 key of
# leaf-ed.der, whose 32-byte seed is kept as leaf-ed.TEST-ONLY-seed: the in-process test
# server signs its CertificateVerify with it. It protects nothing.
set -euo pipefail
cd "$(dirname "$0")/.."
OPENSSL="${OPENSSL:-/opt/homebrew/opt/openssl@3/bin/openssl}"
OUT=tls/testdata/chain
W="$(mktemp -d)"
trap 'rm -rf "$W"' EXIT
mkdir -p "$OUT"
NB=20250101000000Z
NA=20450101000000Z
SERIAL=1000

key() { # name alg
  case "$2" in
    rsa1024) "$OPENSSL" genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:1024 -out "$W/$1.key" 2>/dev/null ;;
    rsa2048) "$OPENSSL" genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$W/$1.key" 2>/dev/null ;;
    rsa3072) "$OPENSSL" genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$W/$1.key" 2>/dev/null ;;
    p256) "$OPENSSL" genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$W/$1.key" ;;
    p384) "$OPENSSL" genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-384 -out "$W/$1.key" ;;
    ed25519) "$OPENSSL" genpkey -algorithm ED25519 -out "$W/$1.key" ;;
  esac
}

# cert NAME KEY SUBJECT ISSUER(or self) EXTENSIONS [DIGEST] [NOT_BEFORE NOT_AFTER] [SIGOPTS...]
cert() {
  local name="$1" k="$2" subj="$3" issuer="$4" exts="$5" md="${6:--sha256}" nb="${7:-$NB}" na="${8:-$NA}"
  shift 8 2>/dev/null || shift $#
  SERIAL=$((SERIAL + 1))
  printf '[v3]\n%b\n' "$exts" >"$W/$name.ext"
  "$OPENSSL" req -new -key "$W/$k.key" -subj "$subj" -out "$W/$name.csr"
  local sign=(-CA "$W/$issuer.pem" -CAkey "$W/$issuer.cakey")
  [ "$issuer" = self ] && sign=(-key "$W/$k.key")
  [ "$md" = none ] && md=""
  "$OPENSSL" x509 -req -in "$W/$name.csr" "${sign[@]}" -extfile "$W/$name.ext" -extensions v3 \
    -not_before "$nb" -not_after "$na" -set_serial "$SERIAL" $md "$@" -out "$W/$name.pem" 2>/dev/null
  cp "$W/$k.key" "$W/$name.cakey"
  "$OPENSSL" x509 -in "$W/$name.pem" -outform DER -out "$OUT/$name.der"
}

CA='basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash'
CA0='basicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid'
CAI='basicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid'
SAN='subjectAltName=DNS:test.milk.example,DNS:*.wild.milk.example,IP:127.0.0.1,IP:10.0.2.2'
LEAF="basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n$SAN"
LEAF_EC="basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n$SAN"

for k in root-rsa:rsa2048 root-ec:p384 int-rsa:rsa3072 int-ec:p256 leaf-rsa:rsa2048 leaf-ec:p256 \
         other-root:p256 root-ed:ed25519 leaf-ed:ed25519 weak:rsa1024 int-x:p256 int-y:p256; do
  key "${k%%:*}" "${k#*:}"
done

cert root-rsa root-rsa "/O=milkOS tests/CN=milkOS Test Root RSA" self "$CA"
cert root-ec root-ec "/O=milkOS tests/CN=milkOS Test Root EC" self "$CA" -sha384
cert int-rsa int-rsa "/O=milkOS tests/CN=milkOS Test Intermediate RSA" root-rsa "$CA0\nextendedKeyUsage=serverAuth"
cert int-ec int-ec "/O=milkOS tests/CN=milkOS Test Intermediate EC" root-ec "$CAI" -sha384
cert leaf-rsa leaf-rsa "/CN=test.milk.example" int-rsa "$LEAF"
cert leaf-ec leaf-ec "/CN=test.milk.example" int-ec "$LEAF_EC"
cert leaf-rsa-sha512 leaf-rsa "/CN=test.milk.example" int-rsa "$LEAF" -sha512
cert leaf-pss leaf-rsa "/CN=test.milk.example" int-rsa "$LEAF" -sha256 "$NB" "$NA" \
  -sigopt rsa_padding_mode:pss -sigopt rsa_pss_saltlen:32
cert leaf-expired leaf-ec "/CN=test.milk.example" int-ec "$LEAF_EC" -sha256 20200101000000Z 20210101000000Z
cert leaf-future leaf-ec "/CN=test.milk.example" int-ec "$LEAF_EC" -sha256 20400101000000Z 20450101000000Z
cert leaf-otherhost leaf-ec "/CN=test.milk.example" int-ec \
  "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:other.example,DNS:*.milk.example"
cert leaf-nosan leaf-ec "/CN=test.milk.example" int-ec "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth"
cert leaf-clientauth leaf-ec "/CN=test.milk.example" int-ec "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=clientAuth\n$SAN"
cert leaf-noeku leaf-ec "/CN=test.milk.example" int-ec "basicConstraints=critical,CA:FALSE\n$SAN"
cert leaf-ca leaf-ec "/CN=test.milk.example" int-ec "basicConstraints=critical,CA:TRUE\n$SAN"
cert leaf-ku leaf-ec "/CN=test.milk.example" int-ec "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,keyCertSign\n$SAN"
cert leaf-critical leaf-ec "/CN=test.milk.example" int-ec "$LEAF_EC\n1.3.6.1.4.1.55555.1=critical,DER:0500"
cert leaf-weak weak "/CN=test.milk.example" int-rsa "$LEAF"
# An intermediate limited to clientAuth, with a leaf under it.
cert int-clientauth int-x "/O=milkOS tests/CN=milkOS Test Intermediate clientAuth" root-ec "$CAI\nextendedKeyUsage=clientAuth" -sha384
cert leaf-under-clientauth leaf-ec "/CN=test.milk.example" int-clientauth "$LEAF_EC"
# pathlen: int-rsa has pathlen 0, so a CA under it may not issue.
cert int-under-pathlen0 int-y "/O=milkOS tests/CN=milkOS Test Sub-Intermediate" int-rsa "$CAI"
cert leaf-pathlen leaf-ec "/CN=test.milk.example" int-under-pathlen0 "$LEAF_EC"
# An issuer without the CA bit.
cert int-notca int-x "/O=milkOS tests/CN=milkOS Test Not A CA" root-ec "basicConstraints=critical,CA:FALSE\nsubjectKeyIdentifier=hash" -sha384
cert leaf-under-notca leaf-ec "/CN=test.milk.example" int-notca "$LEAF_EC"
# An expired intermediate.
cert int-expired int-x "/O=milkOS tests/CN=milkOS Test Expired Intermediate" root-ec "$CAI" -sha384 20200101000000Z 20210101000000Z
cert leaf-under-expired-int leaf-ec "/CN=test.milk.example" int-expired "$LEAF_EC" -sha256 20200101000000Z "$NA"
# A root nobody trusts.
cert other-root other-root "/O=milkOS tests/CN=milkOS Untrusted Root" self "$CA"
cert leaf-untrusted leaf-ec "/CN=test.milk.example" other-root "$LEAF_EC"
# Name constraints: only allowed.example (DNS) and 10.0.0.0/8 (IP).
cert int-nc int-x "/O=milkOS tests/CN=milkOS Test Constrained" root-ec \
  "$CAI\nnameConstraints=critical,permitted;DNS:allowed.example,permitted;IP:10.0.0.0/255.0.0.0,excluded;DNS:bad.allowed.example" -sha384
cert leaf-nc-ok leaf-ec "/CN=www.allowed.example" int-nc \
  "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:www.allowed.example,IP:10.1.2.3"
cert leaf-nc-bad leaf-ec "/CN=test.milk.example" int-nc "$LEAF_EC"
cert leaf-nc-excluded leaf-ec "/CN=bad.allowed.example" int-nc \
  "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:www.allowed.example,DNS:*.allowed.example"
# Wildcards with a trailing dot, and an IPv6 name under IPv4-only permitted subtrees.
cert leaf-nc-dot leaf-ec "/CN=www.allowed.example" int-nc \
  "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:*.allowed.example."
cert leaf-nc-ipv6 leaf-ec "/CN=www.allowed.example" int-nc \
  "basicConstraints=critical,CA:FALSE\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:www.allowed.example,IP:2001:db8::1"
# int-ec again, cross-signed by the RSA root (same subject and key): backtracking.
cert int-ec-cross int-ec "/O=milkOS tests/CN=milkOS Test Intermediate EC" root-rsa "$CAI"
# Ed25519 all the way (the in-process test server's chain).
cert root-ed root-ed "/O=milkOS tests/CN=milkOS Test Root Ed25519" self "$CA" none
cert leaf-ed leaf-ed "/CN=test.milk.example" root-ed "$LEAF_EC" none
"$OPENSSL" pkey -in "$W/leaf-ed.key" -outform DER | tail -c 32 | xxd -p -c 64 >"$OUT/leaf-ed.TEST-ONLY-seed"

ls -l "$OUT"
