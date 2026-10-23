#!/usr/bin/env bash
# The online paths (name lookups, HTTP/1.1, the Browser by host name) end to end, with no
# internet: every server is on the Mac's 127.0.0.1 (scripts/web_test_servers.py, python3 -I),
# and the guest is told (fw_cfg opt/milkos/net "internet=on dns=10.0.2.2:PORT") to use that DNS
# server, never slirp's 10.0.2.3. The netdev is unrestricted (like net-test's) so the guest can
# reach 10.0.2.2 = the Mac's loopback; the test DNS server only knows names under milk.test
# (everything else is NXDOMAIN, nothing is forwarded), so nothing leaves the Mac.
#
#   1. msh over the serial console: network status (internet on, the test DNS), network lookup
#      (a CNAME, the cache: asked once for two lookups, AAAA, a truncated UDP answer retried
#      over TCP, NXDOMAIN, example.com answered by the test server), network get through a
#      301/302/307 chain to a chunked + gzip page, a redirect loop, https:// to a closed port (TLS itself: scripts/tls-interop.sh),
#      /proc/net/internet.
#   2. The Browser (scripts/wm-demo.sh browseurl) opening http://www.milk.test:PORT/start:
#      its log line ([browser] ...: 200, title, 3 redirects) and a screenshot.
#
#   scripts/web-test.sh [DIR]     DIR: where to keep the screenshot (default: the work dir)
# Only the qemu and python processes started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
WORK="$(mktemp -d /tmp/milk-web-test.XXXXXX)"
SHOT_DIR="${1:-$WORK}"
mkdir -p "$SHOT_DIR"
SHOT="$SHOT_DIR/web-browser.png"
LOG="$WORK/serial.log"
QPID="" SPID="" DPID=""
cleanup() {
  [ -n "$DPID" ] && kill "$DPID" 2>/dev/null || true
  [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true
  [ -n "$SPID" ] && kill "$SPID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "web-test: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

(cd "$WORK" && exec python3 -I "$REPO/scripts/web_test_servers.py" "$WORK/ports" >"$WORK/servers.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$WORK/ports" ] && break; sleep 0.1; done
[ -s "$WORK/ports" ] || fail "test servers did not start"
eval "$(tr ' ' '\n' <"$WORK/ports" | sed 's/^/P_/')"
H="$P_http"
NETCFG="name=opt/milkos/net,string=internet=on dns=10.0.2.2:$P_dns"
echo "servers: dns 127.0.0.1:$P_dns (udp+tcp), http 127.0.0.1:$H (pid $SPID); guest: $NETCFG"

# ---- 1. msh ----
SERIAL="$WORK/serial.sock"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  -netdev "user,id=n0" -device virtio-net-device,netdev=n0 \
  -fw_cfg "$NETCFG" &
QPID=$!
P='wait:msh:/\$ '
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG" \
  'wait@240:\[net\] selftest' "$P" \
  'send:network status' 'wait:internet +on' "$P" \
  'send:network lookup www.milk.test' 'wait:network lookup: ' "$P" \
  'send:network lookup www.milk.test' 'wait:network lookup: ' "$P" \
  'send:network lookup --ipv6 v6.milk.test' 'wait:network lookup: ' "$P" \
  'send:network lookup big.milk.test' 'wait:network lookup: ' "$P" \
  'send:network lookup nope.milk.test' 'wait:network lookup: ' "$P" \
  'send:network lookup example.com' 'wait:network lookup: ' "$P" \
  "send:network get http://www.milk.test:$H/start" 'wait:network get: [^\n]* ms' "$P" \
  "send:network get http://www.milk.test:$H/loop" 'wait:network get: the redirects' "$P" \
  'send:network get https://www.milk.test/' 'wait:network get: connect' "$P" \
  'send:text show /proc/net/internet' 'wait:cache_misses=' "$P" \
  "done:$WORK/drive.done" >"$WORK/drive.log" 2>&1 &
DPID=$!
for _ in $(seq 1800); do
  [ -e "$WORK/drive.done" ] && break
  kill -0 "$QPID" 2>/dev/null || fail "qemu exited"
  kill -0 "$DPID" 2>/dev/null || { cat "$WORK/drive.log" >&2; fail "the msh session stalled"; }
  sleep 0.1
done
[ -e "$WORK/drive.done" ] || fail "the msh session did not finish"
sleep 0.3
kill "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true; DPID=""
T="$WORK/console.txt"
python3 -I scripts/console_clean.py "$LOG" >"$T"
grep '^\[net\] internet' "$T" || true
sed -n '/msh:\/\$ network status/,$p' "$T" | grep -v '^  \(received\|sent\|flags\)' | head -120
cp "$WORK/servers.log" "$WORK/servers1.log"
S="$WORK/servers1.log"
want() { grep -Eq -- "$1" "$T" || fail "${2:-console: no $1}"; }
want "^\[net\] internet: on, DNS 10\.0\.2\.2:$P_dns \(test override" "kernel: not told the test DNS"
want "^internet +on \(DNS 10\.0\.2\.2:$P_dns, test override\)$" "network status: internet line"
[ "$(grep -c '^www\.milk\.test has IPv4 address 10\.0\.2\.2$' "$T")" = 2 ] || fail "network lookup www.milk.test (CNAME to web.milk.test)"
[ "$(grep -c '^dns udp www\.milk\.test 1$' "$S")" = 1 ] || fail "the second lookup of www.milk.test wasn't answered from the cache"
want '^v6\.milk\.test has IPv6 address fec0::2$' "network lookup --ipv6 (AAAA)"
[ "$(grep -c '^big\.milk\.test has IPv4 address ' "$T")" = 40 ] || fail "network lookup big.milk.test: expected 40 addresses"
grep -q '^dns udp big\.milk\.test 1 truncated$' "$S" && grep -q '^dns tcp big\.milk\.test 1 ' "$S" || fail "no TCP retry of the truncated answer"
want "^network lookup: nope\.milk\.test: there's no such host$" "NXDOMAIN"
want "^network lookup: example\.com: there's no such host$" "example.com should go to the test DNS (NXDOMAIN there)"
grep -q '^dns udp example\.com 1$' "$S" || fail "example.com wasn't asked of the test server"
want "^network get: redirected to http://www\.milk\.test:$H/step2$" "redirect 1 (301, relative)"
want "^network get: redirected to http://web\.milk\.test:$H/step3$" "redirect 2 (302, another host name)"
want "^network get: redirected to http://web\.milk\.test:$H/page$" "redirect 3 (307)"
want "^<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Milk &amp; the Web</title>" "the page body (gunzipped)"
want "^network get: 200 OK, [0-9]+ bytes \(gzip, [0-9]+ on the wire\) from 10\.0\.2\.2:$H in [0-9]+ ms$" "network get summary (200, gzip)"
grep -q "^http /page host=web.milk.test:$H ua=milkOS/0.1 (network get)$" "$S" || fail "the page request's Host / User-Agent"
grep -q '^http /page sent [0-9]* bytes gzip chunked$' "$S" || fail "the server didn't send the page gzipped and chunked"
want "^network get: the redirects go round in a circle \(back to http://www\.milk\.test:$H/loop\)$" "redirect loop"
want "^network get: connect 10\.0\.2\.2:443: " "https:// (TLS: scripts/tls-interop.sh) to a port nobody listens on"
want "^internet=on$" "/proc/net/internet"
want "^dns=10\.0\.2\.2:$P_dns$" "/proc/net/internet dns"
want "^dns_from=test$" "/proc/net/internet dns_from"
echo "msh: network status (internet on, test DNS), lookup (CNAME, cache, AAAA, TCP after TC, NXDOMAIN), get (301/302/307, chunked + gzip, loop, https to a closed port), /proc/net/internet: ok"

# ---- 2. the Browser, by host name ----
URL="http://www.milk.test:$H/start"
BLOG="${SHOT%.png}.serial.log"
WM_HOSTNET=1 DEMO_WAIT=120 SETTLE=1 scripts/wm-demo.sh browseurl "$SHOT" -fw_cfg "$NETCFG" \
  -fw_cfg "name=opt/milkos/testurl,string=$URL" >"$WORK/wm-demo.out" 2>&1 || { cat "$WORK/wm-demo.out" >&2; fail "the Browser demo"; }
python3 -I scripts/png_check.py "$SHOT" || fail "the screenshot isn't a PNG"
grep -a '^\[browser\]' "$BLOG" | tr -d '\r' || true
grep -aq "^\[browser\] http://web\.milk\.test:$H/page: 200 OK, [0-9]* bytes, 'Milk & the Web', 3 redirects" "$BLOG" ||
  fail "the Browser didn't load the page by host name ($BLOG)"
grep -q "^http /page host=web.milk.test:$H ua=milkOS/0.1$" "$WORK/servers.log" || fail "no Browser request (User-Agent milkOS/0.1) for /page"
echo "Browser: $URL -> http://web.milk.test:$H/page (3 redirects), screenshot $SHOT"
echo "---- test server log ----"
cat "$WORK/servers.log"
cleanup
trap - EXIT
echo "web-test: PASS"
