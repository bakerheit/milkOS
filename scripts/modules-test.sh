#!/usr/bin/env bash
# ES modules in the Browser, end to end on a private image, against local test servers only
# (scripts/modules_test_servers.py on the Mac's 127.0.0.1: a DNS server for a made-up zone and
# one web server answering for www.milk.test, api.milk.test and other.test, serving the pages
# in tests/modules; the guest is told internet=on with that DNS server, like fetch-test, so names
# work and nothing leaves the Mac). About three minutes. The pages report through console.log
# ("MODTEST group name result"), which the Browser prints on the serial line, and the server
# logs every request it gets. Checks:
#   * basic.html     inline and external modules importing each other, a cycle, each module run
#                    once, nomodule skipped, deferred and async order, DOMContentLoaded and
#                    load, top-level await that does not hold up its siblings, import() from a
#                    classic script (and the same module twice), import.meta.url, `this` and
#                    document.currentScript in a module
#   * map.html       an import map: bare specifiers, a prefix, a scope, a blocked name, a
#                    cross-origin address, the second map ignored and one added after loading
#                    began ignored; bare.html: no import map, the bare specifier fails
#                    (window.onerror, import() rejects), other modules unharmed
#   * cors.html      a module from another origin: blocked without CORS headers, allowed with
#                    them, through redirects too; cors-cred.html: cookies follow the
#                    crossorigin attribute (same-origin, anonymous, use-credentials)
#   * mime.html      wrong MIME types, 404 and 500, a bad dependency, JSON modules; each
#                    refusal an error event and a console error; the other modules still run
#   * bundle.html    a small app built from modules and a JSON file (screenshots before and
#                    after "the user" adds cheeses)
#   * limits.html    an endless import chain is cut at its depth limit, 200 modules at once are
#                    fine, more than a page may have are refused without being fetched
#   * abandon.html   a module still downloading when the user goes elsewhere is abandoned
#   * the JavaScript switch (the toolbar's menu): off, no module is fetched
#   scripts/modules-test.sh [workdir]
# Only the qemu and python processes started here are killed (by PID). The work directory is
# removed when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-modtest.XXXXXX)}"
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
(cd "$T" && exec python3 -I "$REPO/scripts/modules_test_servers.py" "$T/ports" "$REPO/tests/modules" >"$T/server.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$T/ports" ] && break; sleep 0.1; done
[ -s "$T/ports" ] || { echo "modules-test: FAIL: the test servers did not start"; exit 1; }
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
wait_srv() { for _ in $(seq $(( ${2:-20} * 10 ))); do grep -aEq -- "$1" "$T/server.log" && return 0; sleep 0.1; done; return 1; }
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
differ() { python3 scripts/shotwait.py --differ "$T/$1.png" "$T/$2.png"; }
esc_re() { printf '%s' "$1" | sed 's/[.[\*^$()+?{|]/\\&/g'; }
go() { q key meta_l-l type "$1" key ret; }
# mt GROUP NAME WANT: the page's report equals WANT (an exact string); mtr: matches the regex.
report_of() { log_lines | { grep -a "^\[browser\] console log: MODTEST $1 $2 " || true; } | tail -1 | sed "s/^\[browser\] console log: MODTEST $1 $2 //"; }
mt() {
  local got; got="$(report_of "$1" "$2")"
  if [ "$got" = "$3" ]; then ok "$1 $2: $3"; else bad "$1 $2: got [$got] wanted [$3]"; fi
}
mtr() {
  local got; got="$(report_of "$1" "$2")"
  if printf '%s' "$got" | grep -aEq -- "$3"; then ok "$1 $2: $got"; else bad "$1 $2: got [$got] wanted /$3/"; fi
}
# nomt GROUP NAME: the page did not report it.
nomt() { if [ -z "$(report_of "$1" "$2")" ]; then ok "$1 $2: never reported"; else bad "$1 $2 was reported: $(report_of "$1" "$2")"; fi; }
srvn() { grep -acE -- "$1" "$T/server.log" || true; }
# have_console LEVEL REGEX: the console printed such a line.
have_console() { wait_log "^\[browser\] console $1: $2" "${3:-5}"; }

ser "wait@120:$PR"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser --log-input http://$H/basic.html &" "wait:$PR"

echo "-- the basics"
wait_log "^\[browser\] console log: MODTEST basic end " 60 && ok "basic.html ran to its end" || bad "basic.html did not finish: $(log_lines | grep -a 'MODTEST basic\|console' | tail -4)"
mt basic graph "a b b,fromA"
mt basic cycle "cycle sees a=a"
mt basic once "1 1"
mt basic this-currentScript "undefined true"
mt basic strict "true"
mt basic meta "/basic.html"
mt basic nomodule "undefined"
mt basic dom-ready "interactive true"
mt basic dynamic "hello from dyn dyn-default"
mt basic dynamic-same "hello from dyn"
mtr basic dynamic-404 "^TypeError: Failed to fetch dynamically imported module: .*/basic/nothere\.js$"
mt basic order "ok"
nomt basic window-error
[ "$(srvn '^http GET www\.milk\.test /count/basic\.js ')" = 1 ] && ok "a module imported twice was fetched once" || bad "count/basic.js was fetched $(srvn '^http GET www\.milk\.test /count/basic\.js ') times"
[ "$(srvn '^http GET www\.milk\.test /basic/tla\.js ')" = 1 ] && ok "a module named by a script and an import was fetched once" || bad "tla.js was fetched $(srvn '^http GET www\.milk\.test /basic/tla\.js ') times"
shot modules-01-basic

echo "-- an import map"
go "$H/map.html"
wait_log "^\[browser\] console log: MODTEST map late-map " 30 && ok "map.html ran" || bad "map.html did not finish: $(log_lines | grep -a 'MODTEST map\|console' | tail -4)"
mt map bare "[[1,2],[3,4],[5]]"
mt map prefix "app/run.js -> app/helper.js"
mt map top-level "cheese-v1"
mt map scope "legacy uses cheese-v0"
mt map cross-origin "cors-open"
mtr map blocked "^TypeError: Failed to resolve module specifier"
mtr map unmapped "^TypeError: Failed to resolve module specifier \"nothing-here\""
mt map first-wins "true"
mtr map late-map "^TypeError: Failed to resolve module specifier \"later\""
have_console warn ".*second import map was ignored" && ok "the console says the second import map was ignored" || bad "no note about the second import map"
have_console warn ".*after module loading had started" && ok "the console says the late import map was ignored" || bad "no note about the late import map"
[ "$(srvn '/lib/wrong\.js')" = 0 ] && ok "the second import map's address was never used" || bad "the second map was used"

echo "-- no import map"
go "$H/bare.html"
wait_log "^\[browser\] console log: MODTEST bare dotted " 30 && ok "bare.html ran" || bad "bare.html did not finish: $(log_lines | grep -a 'MODTEST bare\|console' | tail -4)"
mtr bare onerror "^Uncaught TypeError: Failed to resolve module specifier \"lodash-lite\"\. Relative references must start with either \"/\", \"\./\", or \"\.\./\"\. @/map/needs-bare\.js$"
mt bare other-module "ran"
mtr bare dynamic "^TypeError: Failed to resolve module specifier \"lodash-lite\""
mtr bare dotted "^TypeError: Failed to fetch dynamically imported module: .*/nothing$"
nomt bare must-not-run

echo "-- another origin: CORS"
go "$H/cors.html"
wait_log "^\[browser\] console log: MODTEST cors end " 60 && ok "cors.html ran to its end" || bad "cors.html did not finish: $(log_lines | grep -a 'MODTEST cors\|console' | tail -4)"
mt cors static-open "loaded"
mt cors static-closed "blocked"
mt cors import-open "cors-open"
mt cors import-closed "rejected TypeError"
mt cors import-other-open "cors-open"
mt cors origin "http://$H"
mt cors redirect-to-closed "rejected TypeError"
mt cors redirect-to-open "cors-open"
have_console error ".*Access-Control-Allow-Origin" && ok "the console explains the CORS failure" || bad "no CORS explanation in the console: $(log_lines | grep -a 'console error' | tail -3)"
srvn '^http GET api\.milk\.test /cors/closed\.js ' | grep -qv '^0$' && ok "the blocked module was requested (CORS is judged on the answer)" || bad "closed.js was never requested"

echo "-- credentials"
go "$H/cors-cred.html"
wait_log "^\[browser\] console log: MODTEST cred use-credentials " 40 && ok "cors-cred.html ran" || bad "cors-cred.html did not finish: $(log_lines | grep -a 'MODTEST cred\|console' | tail -4)"
mt cred same-origin-module "sid=m1 | api: None"
mt cred anonymous "api: None"
mt cred use-credentials "api: xs=1"

echo "-- what is refused"
go "$H/mime.html"
wait_log "^\[browser\] console log: MODTEST mime end " 60 && ok "mime.html ran to its end" || bad "mime.html did not finish: $(log_lines | grep -a 'MODTEST mime\|console' | tail -4)"
for w in plain html none json-as-js status500 missing bad-dependency; do mt mime "$w" error; done
mt mime good-sibling loaded
mt mime fine-ran yes
nomt mime must-not-run
mt mime json-module "milk 1+2+3"
for w in plain html missing status404 json-without-attribute json-wrong-type css-type; do mt mime "dynamic-$w" TypeError; done
have_console error ".*Failed to load module script http://$(esc_re "$H")/mime/plain\.js: expected a JavaScript module script but the server responded with a MIME type of \"text/plain\"" && ok "the console names the module and the MIME type it came with" || bad "no console error for the wrong MIME type: $(log_lines | grep -a 'console error' | tail -3)"
have_console error ".*Failed to load module script http://$(esc_re "$H")/nothing/here\.js: the server responded with a status of 404" && ok "the console reports the 404 import" || bad "no console error for the 404"
have_console error ".*status of 500" && ok "and the 500" || bad "no console error for the 500"
have_console error ".*sent no MIME type" && ok "and the missing Content-Type" || bad "no console error for the missing type"

echo "-- a small app built from modules"
go "$H/bundle.html"
wait_log "^\[browser\] console log: MODTEST bundle rendered " 40 && ok "bundle.html rendered" || bad "bundle.html did not render: $(log_lines | grep -a 'MODTEST bundle\|console' | tail -4)"
mt bundle rendered "4 cards, 0 items"
sleep 0.5
shot modules-02-bundle
wait_log "^\[browser\] console log: MODTEST bundle after-clicks " 20 && ok "the clicks ran" || bad "no after-clicks report"
mt bundle after-clicks '3 items | Total: $24.00'
sleep 0.5
shot modules-03-bundle-clicked modules-02-bundle
differ modules-02-bundle modules-03-bundle-clicked && ok "the page redrew with the basket" || bad "the page looks the same after the clicks"

echo "-- limits"
go "$H/limits.html"
wait_log "^\[browser\] console log: MODTEST limits end " 120 && ok "limits.html ran to its end" || bad "limits.html did not finish: $(log_lines | grep -a 'MODTEST limits\|console' | tail -4)"
mtr limits depth "nest too deeply"
mt limits wide-200 200
mtr limits wide-2300 "^TypeError: This page imports too many modules \(the limit is 2000\)"
CHAIN="$(srvn '^http GET www\.milk\.test /gen/chain/')"
[ "$CHAIN" -ge 200 ] && [ "$CHAIN" -le 262 ] && ok "the import chain was followed $CHAIN modules deep, then cut" || bad "the chain was fetched $CHAIN times"
LEAF="$(srvn '^http GET www\.milk\.test /gen/leaf/')"
[ "$LEAF" = 200 ] && ok "only the 200 modules that were allowed were fetched" || bad "$LEAF leaf modules were fetched (wanted 200)"

echo "-- a module still downloading when the user leaves"
go "$H/abandon.html"
wait_srv "^http GET www\.milk\.test /stream/forever\.js " 20 && ok "the module began to download" || bad "the stream module was never asked for"
sleep 2
go "$H/basic.html"
wait_srv "^aborted /stream/forever\.js" 20 && ok "going to another page abandoned the download (the server saw the connection close)" || bad "the download went on after the navigation"

echo "-- the JavaScript switch"
# Where the window's body is: a right click (it does nothing on a page) is logged in body coordinates.
q move 512 400 click right
set -- $(log_lines | sed -n 's/^\[browser\] input down \(-\{0,1\}[0-9]*\) \(-\{0,1\}[0-9]*\) button=2 .*/\1 \2/p' | tail -1)
if [ -n "${1:-}" ]; then BX=$((512 - $1)); BY=$((400 - $2)); else BX=86; BY=91; bad "no right click logged, guessing the window's place"; fi
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript off" 5 && ok "the menu turned JavaScript off" || bad "no javascript off"
BEFORE="$(srvn '^http GET www\.milk\.test /basic/a\.js ')"
go "$H/basic.html"
sleep 4
AFTER="$(srvn '^http GET www\.milk\.test /basic/a\.js ')"
[ "$BEFORE" = "$AFTER" ] && ok "with JavaScript off no module was fetched" || bad "modules were fetched with JavaScript off ($BEFORE -> $AFTER)"
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript on" 5 && ok "and on again" || bad "no javascript on"

echo "-- after all that"
log_lines | grep -a "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || ok "the Browser stayed up"
go "$H/bundle.html"
wait_log "^\[browser\] console log: MODTEST bundle after-clicks " 40 && ok "and still loads pages and runs their modules (a second bundle.html run)" || bad "the Browser didn't recover"
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "modules-test: PASS"
else
  echo "modules-test: FAIL (logs and screenshots in $T)"
  exit 1
fi
