#!/usr/bin/env bash
# fetch and XMLHttpRequest in the Browser, end to end on a private image, against local test
# servers only (scripts/fetch_test_servers.py on the Mac's 127.0.0.1: a DNS server for a made-up
# zone and one web server answering for www.milk.test, api.milk.test and other.test, serving the
# pages in tests/fetch; the guest is told internet=on with that DNS server, like browser-test,
# so names work and nothing leaves the Mac). About two minutes. The pages report through
# console.log ("FETCHTEST group name result"), which the Browser prints on the serial line, and
# the server logs every request it gets. Checks:
#   * basic.html     same-origin fetch: text, json, status, POST/PUT/DELETE bodies (JSON, form,
#                    multipart, bytes), redirects (follow, manual, error), network errors,
#                    abort and AbortSignal.timeout, a response over the size limit, Set-Cookie
#                    hidden from the page, cookies sent and omitted, ten parallel requests,
#                    sendBeacon, unhandledrejection
#   * xhr.html       XMLHttpRequest: the event order, responseType text/json/arraybuffer/blob,
#                    headers, progress with lengthComputable, upload events, timeout, abort,
#                    network errors, synchronous XHR refused, unsafe headers dropped, CORS
#   * cors.html      cross-origin fetch: Allow-Origin, exposed headers, preflight (and its
#                    cache), a denied preflight, credentials include/omit with cookies, the
#                    wildcard with credentials refused, no third-party cookies, no-cors opaque
#                    responses, same-origin mode, redirects across origins, forbidden headers
#   * render.html    a page that loads JSON and builds its content with script (screenshots)
#   * the console panel's Network list (Cmd-Alt-J, then its label), and the Browser staying up
#   scripts/fetch-test.sh [workdir]
# Only the qemu and python processes started here are killed (by PID). The work directory is
# removed when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-fetchtest.XXXXXX)}"
mkdir -p "$T"
M="python3 scripts/mkmilkfs.py"
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/a.img"
rm -f "$IMG" "$T/ports"
$M mkfs "$IMG" --size 64M >/dev/null
$M mkdir "$IMG" /Users/milk >/dev/null
QPID="" SPID=""
cleanup() {
  [ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true
  [ -n "$SPID" ] && kill "$SPID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
(cd "$T" && exec python3 -I "$REPO/scripts/fetch_test_servers.py" "$T/ports" "$REPO/tests/fetch" >"$T/server.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$T/ports" ] && break; sleep 0.1; done
[ -s "$T/ports" ] || { echo "fetch-test: FAIL: the test servers did not start"; exit 1; }
eval "$(tr ' ' '\n' <"$T/ports" | sed 's/^/P_/')"
P="$P_http"
H="www.milk.test:$P"
echo "servers: dns 127.0.0.1:$P_dns, http 127.0.0.1:$P (pid $SPID)"
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -chardev "socket,id=s0,path=$SOCK,server=on,wait=off,logfile=$LOG" -serial chardev:s0 \
  -device virtio-gpu-device -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
  -netdev user,id=n0 -device virtio-net-device,netdev=n0 \
  -fw_cfg "name=opt/milkos/net,string=internet=on dns=10.0.2.2:$P_dns" \
  -qmp "unix:$QMP,server,nowait" >"$T/qemu.out" 2>&1 &
QPID=$!
PR='msh:[^\n]*\$ '
n=0
ser() { # steps...: run console steps, wait for them to finish, disconnect
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$T/drive.log" "$@" "done:$T/ser.$n" &
  local d=$!
  for _ in $(seq 600); do [ -e "$T/ser.$n" ] && break; sleep 0.2; done
  kill "$d" 2>/dev/null || true; wait "$d" 2>/dev/null || true
}
q() { python3 scripts/qmp.py "$QMP" "$@"; }
clean_log() { tr -d '\r' < "$LOG"; }
log_lines() { clean_log | sed -E 's/^msh:[^$]*\$ //'; }
wait_log() { for _ in $(seq $(( ${2:-20} * 10 ))); do log_lines | grep -aE -- "$1" >/dev/null && return 0; sleep 0.1; done; return 1; }
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
differ() { python3 scripts/shotwait.py --differ "$T/$1.png" "$T/$2.png"; }
esc_re() { printf '%s' "$1" | sed 's/[.[\*^$()+?{|]/\\&/g'; }
go() { q key meta_l-l type "$1" key ret; }
# ft GROUP NAME WANT: the page's report equals WANT (an exact string); ftr: matches the regex.
report_of() { log_lines | { grep -a "^\[browser\] console log: FETCHTEST $1 $2 " || true; } | tail -1 | sed "s/^\[browser\] console log: FETCHTEST $1 $2 //"; }
ft() {
  local got; got="$(report_of "$1" "$2")"
  if [ "$got" = "$3" ]; then ok "$1 $2: $3"; else bad "$1 $2: got [$got] wanted [$3]"; fi
}
ftr() {
  local got; got="$(report_of "$1" "$2")"
  if printf '%s' "$got" | grep -aEq -- "$3"; then ok "$1 $2: $got"; else bad "$1 $2: got [$got] wanted /$3/"; fi
}
srv() { grep -aE -- "$1" "$T/server.log" >/dev/null; }
srvn() { grep -acE -- "$1" "$T/server.log" || true; }

ser "wait@120:$PR"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser --log-input http://$H/basic.html &" "wait:$PR"

echo "-- fetch, same origin"
wait_log "^\[browser\] console log: FETCHTEST basic end" 90 && ok "basic.html ran to its end" || bad "basic.html did not finish: $(log_lines | grep -a 'FETCHTEST basic' | tail -2)"
ft basic text "hello milk"
ft basic json "3 milk true"
ft basic status "404 false Status 404 status 404"
ft basic response "basic /api/hello false text/plain; charset=utf-8 10"
ft basic post-json "POST application/json t1 {\"a\":1} true"
ft basic put "PUT text/plain;charset=UTF-8 put body"
ft basic delete "DELETE 0"
ft basic urlencoded "application/x-www-form-urlencoded;charset=UTF-8 a=1+2&b=x%26y"
ft basic formdata "multipart/form-data true true true"
ft basic bytes "256 32640 255"
ft basic binary-body "hi! 3"
ft basic redirect "true /api/hello hello milk"
ft basic redirect-manual "opaqueredirect 0"
ft basic redirect-error "TypeError: Failed to fetch"
ft basic network-error "TypeError: Failed to fetch"
ft basic refused-scheme "TypeError: Failed to fetch"
ft basic abort "AbortError fast=true"
ft basic timeout-signal "TimeoutError"
ft basic too-big "TypeError: Failed to fetch"
ft basic set-cookie-hidden "null false"
ft basic cookie-sent "sid=abc123"
ft basic cookie-omit "none"
ft basic document-cookie "sid=abc123"
ft basic parallel "10 true"
ft basic beacon "true"
srv "^beacon ping=1" && ok "the beacon reached the server" || bad "no beacon at the server"
ft basic request-object "via request true"
ft basic unhandledrejection "TypeError"
wait_log "^\[browser\] console error: .*nosuch\.test" 5 && ok "the console says which request failed and why" || bad "no console line for the failed request: $(log_lines | grep -a 'console error' | tail -3)"
wait_log "^\[browser\] console error: .*(bigger|too big|limit)" 5 && ok "the console says the response was over the limit" || bad "no console line about the big response"
wait_log "^\[browser\] request GET http://$(esc_re "$H")/api/hello: 200 OK, 10 bytes" 5 && ok "requests are logged on the serial line" || bad "no request line on the serial log"
srv "^http POST www\.milk\.test /api/echo origin=http://$(esc_re "$H")" && ok "a same-origin POST carries Origin" || bad "no Origin on the POST"
srv "^http GET www\.milk\.test /api/whoami origin=None cookie=sid=abc123" && ok "the cookie went with a same-origin fetch" || bad "no cookie on /api/whoami: $(grep -a whoami "$T/server.log" | head -3)"
shot fetch-01-basic

echo "-- XMLHttpRequest"
go "$H/xhr.html"
wait_log "^\[browser\] console log: FETCHTEST xhr end" 90 && ok "xhr.html ran to its end" || bad "xhr.html did not finish: $(log_lines | grep -a 'FETCHTEST xhr' | tail -2)"
ft xhr get "200 | OK | hello milk | text/plain; charset=utf-8 | 4"
ftr xhr events "^readystatechange1,loadstart,readystatechange2,.*readystatechange4,(progress,)?load,loadend$"
ft xhr json "1+2+3 object"
ft xhr arraybuffer "256 200"
ft xhr blob "256 application/octet-stream"
ft xhr all-headers "content-length: 10 / content-type: text/plain; charset=utf-8"
ft xhr post "POST x1 text/x-test xhr body"
ftr xhr status-404 "^404 readystatechange1,loadstart,readystatechange2,.*readystatechange4,.*load,loadend$"
ft xhr progress "events>=3=true computable=true last=204800 size=204800"
ft xhr upload "loadstart,progress,load,loadend status=200"
ft xhr timeout "loadstart,readystatechange4,timeout,loadend status=0 fast=true"
ft xhr abort "loadstart,readystatechange4,abort,loadend state=0 status=0"
ft xhr error "loadstart,readystatechange4,error,loadend status=0"
ft xhr sync-refused "InvalidAccessError"
ft xhr unsafe-header "false ok"
ft xhr twice "InvalidStateError"
ft xhr cross-origin "200 open yes null"
ft xhr cross-origin-blocked "loadstart,readystatechange4,error,loadend status=0 text=[]"
ft xhr preflighted-put "PUT cx {\"v\":1}"
ftr xhr upload-listener-preflights "status=200$"
srv "^preflight POST" && ok "upload listeners forced a preflight" || bad "no preflight for the XHR with upload listeners"
wait_log "^\[browser\] console warn: Synchronous XMLHttpRequest" 5 && ok "the console explains why synchronous XHR is refused" || bad "no console warning about synchronous XHR"
wait_log "^\[browser\] console warn: Refused to set unsafe header" 5 && ok "an unsafe request header was refused with a note" || bad "no note about the unsafe header"

echo "-- fetch, cross origin"
go "$H/cors.html"
wait_log "^\[browser\] console log: FETCHTEST cors end" 90 && ok "cors.html ran to its end" || bad "cors.html did not finish: $(log_lines | grep -a 'FETCHTEST cors' | tail -2)"
ft cors origin "http://$H"
ft cors open "cors 200 open yes null text/plain; charset=utf-8"
ft cors origin-header "http://$H"
ftr cors closed "^rejected TypeError: Failed to fetch$"
ftr cors closed-other-site "^rejected TypeError: Failed to fetch$"
wait_log "^\[browser\] console error: .*Access-Control-Allow-Origin" 5 && ok "the console explains the CORS failure (the page only saw a TypeError)" || bad "no CORS explanation in the console: $(log_lines | grep -a 'console error' | tail -3)"
ft cors preflight-put "cors PUT abc application/json {\"x\":1} http://$H"
srv "^preflight PUT x-token,content-type|^preflight PUT content-type,x-token" && ok "the preflight named the method and the headers" || bad "preflight lines: $(grep -a '^preflight' "$T/server.log" | head -3)"
ft cors preflight-cached "again"
ft cors preflight-delete "DELETE"
ft cors preflight-denied "rejected TypeError: Failed to fetch"
srv "^http OPTIONS api\.milk\.test /cors/nopreflight" && ok "the denied preflight was sent" || bad "no OPTIONS /cors/nopreflight"
srv "^http PUT api\.milk\.test /cors/nopreflight" && bad "the PUT went out after a denied preflight" || ok "the PUT was never sent after the preflight was refused"
ft cors header-not-allowed "rejected TypeError: Failed to fetch"
[ "$(srvn '^http OPTIONS api\.milk\.test /cors/preflight ')" = 2 ] && ok "two preflights for /cors/preflight: the first (cached for the rest), and the one with an unlisted header" || bad "preflight count: $(srvn '^http OPTIONS api\.milk\.test /cors/preflight ') (wanted 2)"
ft cors no-cookie-without-credentials "null"
ft cors credentials-include-sets-cookie "null"
ft cors credentials-include-sends-cookie "xs=1"
ft cors credentials-default-omits "null"
ftr cors credentials-with-star "^rejected TypeError"
ftr cors credentials-open-star "^rejected TypeError"
ft cors third-party-cookie-not-set "tried"
srv "^http GET other\.test /cors/closed .*cookie=None" && ok "no cookie went to another site" || bad "a cookie went to other.test: $(grep -a 'other.test' "$T/server.log" | head -3)"
ft cors no-cors "opaque 0 \"\" 0"
ftr cors same-origin-mode "^rejected TypeError"
ft cors redirect-to-cors "cors true open"
ftr cors redirect-to-closed "^rejected TypeError"
ft cors forbidden-headers "false www.milk.test 1 undefined /cors.html undefined"

echo "-- a page built from JSON"
go "$H/render.html"
wait_log "^\[browser\] console log: FETCHTEST render start" 20 && ok "render.html started" || bad "render.html did not start"
shot fetch-02-render-loading
wait_log "^\[browser\] console log: FETCHTEST render loaded" 30 && ok "render.html loaded its data" || bad "render.html did not finish"
ft render loaded "4 34.15"
sleep 1
shot fetch-03-render-loaded fetch-02-render-loading
differ fetch-02-render-loading fetch-03-render-loaded && ok "the page redrew with the cheeses the script built from JSON" || bad "the page looks the same after the data came"

echo "-- the console panel's Network list"
# Where the window's body is: a right click (it does nothing on a page) is logged in body
# coordinates.
q move 512 300 click right
set -- $(log_lines | sed -n 's/^\[browser\] input down \(-\{0,1\}[0-9]*\) \(-\{0,1\}[0-9]*\) button=2 .*/\1 \2/p' | tail -1)
if [ -n "${1:-}" ]; then BX=$((512 - $1)); BY=$((300 - $2)); else BX=86; BY=91; bad "no right click logged, guessing the window's place"; fi
q key meta_l-alt-j sleep 1
shot fetch-04-console
# The panel is the bottom third of the page area; its second label is the Network list.
q move $((BX + 112)) $((BY + 420)) click sleep 1
shot fetch-05-network fetch-04-console
differ fetch-04-console fetch-05-network && ok "the Network label shows another list (the requests the page made)" || bad "the panel looks the same after clicking Network"

echo "-- after all that"
log_lines | grep -a "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || ok "the Browser stayed up"
go "$H/basic.html"
wait_log "^\[browser\] console log: FETCHTEST basic end" 90 && ok "and still loads pages and runs them (a second basic.html run)" || bad "the Browser didn't recover"
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "fetch-test: PASS"
else
  echo "fetch-test: FAIL (logs and screenshots in $T)"
  exit 1
fi
