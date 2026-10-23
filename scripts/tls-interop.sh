#!/usr/bin/env bash
# TLS interop test: milkOS's TLS 1.3 client against OpenSSL 3's s_server, all on the Mac's
# loopback.
#   1. A throwaway test CA is made at test time (a P-384 root, an RSA-3072 intermediate),
#      with an RSA-2048 leaf and an ECDSA P-256 leaf for interop.milk.test, 127.0.0.1 and
#      10.0.2.2 (the Mac as the guest sees it).
#   2. s_server instances on 127.0.0.1 only, each with one setup: both cipher suites with
#      both keys, P-256 / P-384 only (HelloRetryRequest), ALPN, TLS 1.2 only, and -WWW
#      for a 1.2 MB download per suite.
#   3. The host-built client (tls/probe) runs every case against them (scripts list below).
#   4. milkOS boots headless and its kernel runs the same cases against 10.0.2.2 (the
#      cases in fw_cfg opt/milkos/tlstest, the test CA in opt/milkos/tlstest-root; the
#      kernel trusts that CA for this self-test only, never in its system store).
#   5. In the same boot, the user-space tool /System/Developer/tlsprobe (rt::tls) runs
#      cases too, with the test CA typed in over the serial console, and `network get` loads
#      https:// pages (by name through the test DNS of scripts/web_test_servers.py, and by
#      address) and refuses one for a name the certificate doesn't have.
#   6. The Browser (scripts/wm-demo.sh browseurl) opens the https:// page, then shows the
#      certificate error page for the wrong name.
#   The test CA reaches the guest as fw_cfg opt/milkos/tlstest-root: the kernel trusts it on
#   top of the Mozilla roots for that boot only (with a WARNING line), and the self-test cases
#   use it alone; roots=mozilla cases check that the Mozilla roots don't trust it.
#
#   scripts/tls-interop.sh             OPENSSL=... to use another OpenSSL 3
#   TLS_INTEROP_HOST_ONLY=1 scripts/tls-interop.sh    steps 1-3 only (no VM)
#   KEEP_SHOTS=DIR scripts/tls-interop.sh             keep the Browser screenshots in DIR
#
# Network safety: every server binds 127.0.0.1; the guest only contacts 10.0.2.2 (qemu's
# alias for the Mac's loopback). No real internet. Only processes started here are killed,
# by PID. Work files in a fresh mktemp directory.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
REPO="$PWD"
OPENSSL="${OPENSSL:-/opt/homebrew/opt/openssl@3/bin/openssl}"
WORK="$(mktemp -d /tmp/milk-tls-interop.XXXXXX)"
LOG="$WORK/serial.log"
PIDS=()
cleanup() {
  for p in "${PIDS[@]+"${PIDS[@]}"}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "tls-interop: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

"$OPENSSL" version 2>/dev/null | grep -q '^OpenSSL 3' || fail "needs OpenSSL 3 (brew install openssl@3, or set OPENSSL=)"

# ---- 1. the throwaway CA ----
C="$WORK/ca"
mkdir -p "$C"
NB="$(date -u -v-1d +%Y%m%d%H%M%SZ)"
NA="$(date -u -v+30d +%Y%m%d%H%M%SZ)"
SAN="subjectAltName=DNS:interop.milk.test,DNS:www.milk.test,IP:127.0.0.1,IP:10.0.2.2"
"$OPENSSL" genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-384 -out "$C/root.key" 2>/dev/null
"$OPENSSL" genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 -out "$C/int.key" 2>/dev/null
"$OPENSSL" genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$C/rsa.key" 2>/dev/null
"$OPENSSL" genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$C/ec.key" 2>/dev/null
printf '[v3]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n' >"$C/ca.ext"
printf '[v3]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid\n' >"$C/int.ext"
printf '[v3]\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n%s\n' "$SAN" >"$C/leaf.ext"
mkcert() { # name key subject issuer-name ext
  "$OPENSSL" req -new -key "$C/$2.key" -subj "$3" -out "$C/$1.csr"
  local sign=(-CA "$C/$4.pem" -CAkey "$C/$4.key")
  [ "$4" = self ] && sign=(-key "$C/$2.key")
  "$OPENSSL" x509 -req -in "$C/$1.csr" "${sign[@]}" -extfile "$C/$5" -extensions v3 \
    -not_before "$NB" -not_after "$NA" -set_serial "0x$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')" \
    -sha384 -out "$C/$1.pem" 2>/dev/null
}
mkcert root root "/O=milkOS tests/CN=milkOS Interop Test CA (throwaway)" self ca.ext
mkcert int int "/O=milkOS tests/CN=milkOS Interop Test Intermediate" root int.ext
mkcert rsa rsa "/CN=interop.milk.test" int leaf.ext
mkcert ec ec "/CN=interop.milk.test" root leaf.ext
"$OPENSSL" x509 -in "$C/root.pem" -outform DER -out "$C/root.der"
cp "$C/int.pem" "$C/rsa-chain.pem"
echo "test CA: $(wc -c <"$C/root.der" | tr -d ' ') bytes of DER, valid $NB..$NA"

# 1.2 MB of a numbered pattern for the downloads, and its SHA-256.
python3 -I -c "
import sys
sys.stdout.buffer.write(b''.join(b'%08d milkOS TLS interop .........\n' % i for i in range(32768)))" >"$WORK/big.bin"
BIG_SHA="$(shasum -a 256 "$WORK/big.bin" | cut -d' ' -f1)"
BIG_LEN="$(wc -c <"$WORK/big.bin" | tr -d ' ')"

# A page for network get and the Browser.
printf '<!DOCTYPE html><html><head><title>Secure milk</title></head><body><h1>Secure milk</h1><p>Served over TLS 1.3 by a test server on the Mac.</p></body></html>\n' >"$WORK/secure.html"

# ---- 2. the servers ----
free_ports() {
  python3 -I -c "
import socket, sys
ss = [socket.socket() for _ in range(int(sys.argv[1]))]
for s in ss: s.bind(('127.0.0.1', 0))
print(*[s.getsockname()[1] for s in ss])" "$1"
}
read -r P_RSA_AES P_RSA_CHACHA P_EC_AES P_EC_CHACHA P_P256 P_P384 P_ALPN P_TLS12 P_BIG_AES P_BIG_CHACHA < <(free_ports 10)
# The test DNS (names under milk.test; www.milk.test -> web.milk.test -> 10.0.2.2), as web-test uses.
(cd "$WORK" && exec python3 -I "$REPO/scripts/web_test_servers.py" "$WORK/ports" >"$WORK/web-servers.log" 2>&1) &
PIDS+=($!)
for _ in $(seq 50); do [ -s "$WORK/ports" ] && break; sleep 0.1; done
[ -s "$WORK/ports" ] || fail "the test DNS did not start"
eval "$(tr ' ' '\n' <"$WORK/ports" | sed 's/^/W_/')"
NETCFG="name=opt/milkos/net,string=internet=on dns=10.0.2.2:$W_dns"
serve() { # port log args...
  local port="$1" log="$2"
  shift 2
  (cd "$WORK" && exec "$OPENSSL" s_server -accept "127.0.0.1:$port" -quiet "$@" </dev/null >"$WORK/$log.log" 2>&1) &
  PIDS+=($!)
}
RSA=(-cert "$C/rsa.pem" -key "$C/rsa.key" -cert_chain "$C/rsa-chain.pem")
EC=(-cert "$C/ec.pem" -key "$C/ec.key")
serve "$P_RSA_AES" rsa-aes -tls1_3 -www "${RSA[@]}" -ciphersuites TLS_AES_128_GCM_SHA256
serve "$P_RSA_CHACHA" rsa-chacha -tls1_3 -www "${RSA[@]}" -ciphersuites TLS_CHACHA20_POLY1305_SHA256
serve "$P_EC_AES" ec-aes -tls1_3 -www "${EC[@]}" -ciphersuites TLS_AES_128_GCM_SHA256
serve "$P_EC_CHACHA" ec-chacha -tls1_3 -www "${EC[@]}" -ciphersuites TLS_CHACHA20_POLY1305_SHA256
serve "$P_P256" p256 -tls1_3 -www "${EC[@]}" -groups P-256
serve "$P_P384" p384 -tls1_3 -www "${RSA[@]}" -groups P-384
serve "$P_ALPN" alpn -tls1_3 -www "${EC[@]}" -alpn http/1.1
serve "$P_TLS12" tls12 -tls1_2 -www "${RSA[@]}"
serve "$P_BIG_AES" big-aes -tls1_3 -WWW "${RSA[@]}" -ciphersuites TLS_AES_128_GCM_SHA256
serve "$P_BIG_CHACHA" big-chacha -tls1_3 -WWW "${EC[@]}" -ciphersuites TLS_CHACHA20_POLY1305_SHA256
for p in $P_RSA_AES $P_RSA_CHACHA $P_EC_AES $P_EC_CHACHA $P_P256 $P_P384 $P_ALPN $P_TLS12 $P_BIG_AES $P_BIG_CHACHA; do
  for _ in $(seq 100); do
    python3 -I -c "import socket,sys; socket.create_connection(('127.0.0.1', int(sys.argv[1])), 1).close()" "$p" 2>/dev/null && break
    sleep 0.1
  done
done
echo "s_server on 127.0.0.1: $P_RSA_AES $P_RSA_CHACHA $P_EC_AES $P_EC_CHACHA $P_P256 $P_P384 $P_ALPN $P_TLS12 $P_BIG_AES $P_BIG_CHACHA"

# The cases (tls/src/interop.rs), for host HOST (127.0.0.1 from the Mac, 10.0.2.2 from the guest).
cases() {
  local h="$1" n=interop.milk.test
  echo "rsa-aes,$h:$P_RSA_AES,$n,suite=aes"
  echo "rsa-chacha,$h:$P_RSA_CHACHA,$n,suite=chacha"
  echo "ecdsa-aes,$h:$P_EC_AES,$n,suite=aes,group=x25519"
  echo "ecdsa-chacha,$h:$P_EC_CHACHA,$n,suite=chacha,group=x25519"
  echo "hrr-p256,$h:$P_P256,$n,group=secp256r1"
  echo "hrr-p384,$h:$P_P384,$n,group=secp384r1"
  echo "alpn,$h:$P_ALPN,$n,alpn=http/1.1"
  echo "by-ip,$h:$P_EC_AES,$h"
  echo "big-aes,$h:$P_BIG_AES,$n,suite=aes,get=/big.bin:$BIG_LEN:$BIG_SHA"
  echo "big-chacha,$h:$P_BIG_CHACHA,$n,suite=chacha,get=/big.bin:$BIG_LEN:$BIG_SHA"
  echo "wrong-host,$h:$P_EC_AES,other.milk.test,expect=the_certificate_is_for_another_site"
  echo "untrusted-root,$h:$P_RSA_AES,$n,roots=mozilla,expect=the_certificate_was_not_issued_by_a_trusted_authority"
  echo "tls12-only,$h:$P_TLS12,$n,expect=the_site_doesn't_speak_TLS_1.3"
}

# ---- 3. the host-built client ----
echo "---- host: tls/probe ----"
(cd tls/probe && cargo build -q --target aarch64-apple-darwin --target-dir "$REPO/target/host-tests") || fail "tls/probe build"
PROBE="$REPO/target/host-tests/aarch64-apple-darwin/debug/tlsprobe"
# shellcheck disable=SC2046
"$PROBE" "$C/root.der" $(cases 127.0.0.1) | tee "$WORK/host.txt" || fail "host probe"
HOST_N="$(grep -c ': ok: ' "$WORK/host.txt")"
if [ -n "${TLS_INTEROP_HOST_ONLY:-}" ]; then
  cleanup; trap - EXIT; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$WORK" || true
  echo "tls-interop: PASS (host only: $HOST_N cases)"
  exit 0
fi

# ---- 4. and 5. the guest ----
echo "---- guest: kernel (net::tls::selftest) and user space (/System/Developer/tlsprobe) ----"
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$WORK/milkos-kernel.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
GUEST_CASES="$(cases 10.0.2.2 | tr '\n' ' ')"
printf '%s' "$GUEST_CASES" >"$WORK/tlstest.cfg"
SERIAL="$WORK/serial.sock"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
  -accel hvf -cpu host -m 512M -kernel "$KERNEL" \
  -chardev "socket,id=s0,path=$SERIAL,server=on,wait=on" -serial chardev:s0 -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  -netdev user,id=n0 -device virtio-net-device,netdev=n0 \
  -fw_cfg "name=opt/milkos/tlstest,file=$WORK/tlstest.cfg" \
  -fw_cfg "name=opt/milkos/tlstest-root,file=$C/root.der" -fw_cfg "$NETCFG" &
QPID=$!
PIDS+=("$QPID")
# The user-space tool reads the test CA and its cases from files, typed in here in short
# lines (the console takes them a character at a time).
P='wait:msh:/\$ '
STEPS=('wait@300:\[tls\] selftest' "$P")
while read -r chunk; do
  STEPS+=("send:print $chunk >> /tmp/tlsprobe.root" "$P")
done < <(base64 <"$C/root.der" | tr -d '\n' | fold -w 96; echo)
while read -r c; do
  STEPS+=("send:print $c >> /tmp/tlsprobe.cases" "$P")
done < <(cases 10.0.2.2 | grep -E '^(rsa-chacha|ecdsa-aes|hrr-p256|big-chacha|wrong-host|untrusted-root),')
STEPS+=("send:/System/Developer/tlsprobe /tmp/tlsprobe.root /tmp/tlsprobe.cases" 'wait@180:\[tlsprobe\] [a-zA-Z]+: [0-9]+ of [0-9]+ cases' "$P")
PAGE="$P_BIG_CHACHA"
STEPS+=("send:network get https://www.milk.test:$PAGE/secure.html" 'wait:network get: [^\n]* ms' "$P"
  "send:network get https://10.0.2.2:$PAGE/secure.html" 'wait:network get: [^\n]* ms' "$P"
  "send:network get https://web.milk.test:$PAGE/secure.html" 'wait:network get: web' "$P"
  'send:text show /proc/net/tls' 'wait:test_root=' "$P")
python3 -I scripts/serial_drive.py "$SERIAL" "$LOG" "${STEPS[@]}" "done:$WORK/drive.done" >"$WORK/drive.log" 2>&1 &
DPID=$!
PIDS+=("$DPID")
for _ in $(seq 3000); do
  [ -e "$WORK/drive.done" ] && break
  kill -0 "$QPID" 2>/dev/null || fail "qemu exited"
  kill -0 "$DPID" 2>/dev/null || { cat "$WORK/drive.log" >&2; fail "the serial session stalled"; }
  sleep 0.1
done
[ -e "$WORK/drive.done" ] || fail "timed out"
grep -a '^\[tls\]' "$LOG" | tr -d '\r'
grep -aq '\[tls\] selftest ok' "$LOG" || fail "the kernel's cases did not all pass"
GUEST_N="$(grep -ac '^\[tls\] .*: ok: ' "$LOG")"
python3 -I scripts/console_clean.py "$LOG" | grep '^\[tlsprobe\]' | tee "$WORK/user.txt"
grep -q '^\[tlsprobe\] ok: ' "$WORK/user.txt" || fail "the user-space cases did not all pass"
USER_N="$(grep -c ': ok: ' "$WORK/user.txt")"
grep -aq '\[tls\] 121 Mozilla roots parsed in [0-9]* ms; system store 122 roots' "$LOG" || fail "the root stores"
grep -aq '\[tls\] WARNING: this boot trusts a TEST root' "$LOG" || fail "no warning about the test root"
T="$WORK/console.txt"
python3 -I scripts/console_clean.py "$LOG" >"$T"
sed -n '/network get https:/,$p' "$T" | grep -v '^<' | head -20
[ "$(grep -cE "^network get: 200 ok, [0-9]+ bytes from 10\.0\.2\.2:$PAGE in [0-9]+ ms$" "$T")" = 2 ] || fail "network get https:// (by name and by address)"
[ "$(grep -c '^<!DOCTYPE html><html><head><title>Secure milk</title>' "$T")" -ge 2 ] || fail "network get https://: the page body"
grep -q "^network get: web\.milk\.test: the certificate is for another site$" "$T" || fail "network get https:// with the wrong name"
grep -q '^mozilla_roots=121$' "$T" && grep -q '^test_root=MII' "$T" || fail "/proc/net/tls"
echo "network get https:// (test DNS name, IP, wrong name refused), /proc/net/tls: ok"
kill "$QPID" 2>/dev/null || true

# ---- 6. the Browser ----
echo "---- Browser ----"
browse() { # name url
  WM_HOSTNET=1 DEMO_WAIT=120 SETTLE=1 scripts/wm-demo.sh browseurl "$WORK/$1.png" -fw_cfg "$NETCFG" \
    -fw_cfg "name=opt/milkos/testurl,string=$2" -fw_cfg "name=opt/milkos/tlstest-root,file=$C/root.der" \
    >"$WORK/$1.out" 2>&1 || { cat "$WORK/$1.out" >&2; fail "the Browser demo ($1)"; }
  python3 -I scripts/png_check.py "$WORK/$1.png" >/dev/null || fail "the $1 screenshot isn't a PNG"
  grep -a '^\[browser\]' "$WORK/$1.serial.log" | tr -d '\r'
}
browse secure "https://www.milk.test:$PAGE/secure.html"
grep -aq "^\[browser\] https://www\.milk\.test:$PAGE/secure\.html: 200 ok, [0-9]* bytes, 'Secure milk', 0 redirects" "$WORK/secure.serial.log" ||
  fail "the Browser didn't load the https page"
browse wronghost "https://web.milk.test:$PAGE/secure.html"
grep -aq "^\[browser\] https://web\.milk\.test:$PAGE/secure\.html: web\.milk\.test: the certificate is for another site" "$WORK/wronghost.serial.log" ||
  fail "the Browser didn't refuse the wrong name"
[ -n "${KEEP_SHOTS:-}" ] && cp "$WORK"/secure.png "$WORK"/wronghost.png "$KEEP_SHOTS"/
cleanup
trap - EXIT
[ "${MILK_KEEP:-}" = 1 ] || rm -rf "$WORK" || true
echo "tls-interop: PASS (host $HOST_N cases, guest kernel $GUEST_N, guest user space $USER_N, network get https, Browser https + certificate error)"
