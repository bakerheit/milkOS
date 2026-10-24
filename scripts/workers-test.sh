#!/usr/bin/env bash
# Web Workers in the Browser, end to end on a private image, against local test servers only
# (scripts/workers_test_servers.py on the Mac's 127.0.0.1: a DNS server for a made-up zone and one
# web server answering for www.milk.test, api.milk.test (the same site, another origin) and other.test
# (another site), serving the pages in tests/workers; the guest is told internet=on with that DNS
# server, like fetch-test, so names work and nothing leaves the Mac). About four minutes. The pages
# and the workers report through console.log ("WRKTEST group name value"), which the Browser prints
# on the serial line (a line from a worker carries "[worker name]" after "log"), and the server logs
# every request it gets. Checks:
#   * basic.html      a classic worker computing and replying, a module worker with static and
#                     dynamic imports, importScripts, a worker from a blob: address and one from
#                     a data: address; what the worker's global looks like (no window, no document)
#   * net.html        fetch and XMLHttpRequest from a worker: its own origin, the page's cookie,
#                     CORS (open, closed, blocked and reported in the console with the worker's
#                     name), importScripts across origins, a script of the wrong type
#   * xfer.html       an ArrayBuffer given away (and gone from the sender), message ports both
#                     ways, the structured clone of Date, Map, Set, RegExp, -0, NaN
#   * terminate.html  terminate(): the ticks stop, nothing arrives after it, the server hears no
#                     more
#   * error.html      an error thrown in a worker reaches onerror with the file and line, a syntax
#                     error, a missing script, a script of the wrong type, self.onerror
#   * many.html       seventeen workers: sixteen run, the next is refused
#   * cross.html      worker scripts from another origin are refused
#   * leave.html      navigating away ends the page's workers; frame.html: so does removing the
#                     frame that made one
#   * flood.html      5000 messages each way, in order
#   * runaway.html    a worker that never returns is stopped alone: the page keeps ticking, the
#                     other worker answers
#   * the JavaScript switch (the toolbar's menu): off, no worker script is ever requested
#   scripts/workers-test.sh [workdir]
# Only the qemu and python processes started here are killed (by PID). The work directory is
# removed when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-wrktest.XXXXXX)}"
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
(cd "$T" && exec python3 -I "$REPO/scripts/workers_test_servers.py" "$T/ports" "$REPO/tests/workers" >"$T/server.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$T/ports" ] && break; sleep 0.1; done
[ -s "$T/ports" ] || { echo "workers-test: FAIL: the test servers did not start"; exit 1; }
eval "$(tr ' ' '\n' <"$T/ports" | sed 's/^/P_/')"
P="$P_http"
H="www.milk.test:$P"
echo "servers: dns 127.0.0.1:$P_dns, http 127.0.0.1:$P (pid $SPID)"
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -chardev "socket,id=s0,path=$SOCK,server=on,wait=on,logfile=$LOG" -serial chardev:s0 \
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
esc_re() { printf '%s' "$1" | sed 's/[.[\*^$()+?{|]/\\&/g'; }
go() { q key meta_l-l type "$1" key ret; }
# A console line a page printed: "[browser] console LEVEL: WRKTEST ..." from the page, or
# "[browser] console LEVEL [worker NAME]: WRKTEST ..." from a worker.
CL='^\[browser\] console log( \[worker [^]]*\])?: WRKTEST'
report_of() { log_lines | { grep -aE "$CL $1 $2 " || true; } | tail -1 | sed -E "s/$CL $1 $2 //"; }
who_of() { log_lines | { grep -aE "$CL $1 $2 " || true; } | tail -1 | sed -nE 's/^\[browser\] console log \[worker ([^]]*)\]:.*/\1/p'; }
# mt GROUP NAME WANT: the report equals WANT (an exact string); mtr: matches the regex.
mt() {
  local got; got="$(report_of "$1" "$2")"
  if [ "$got" = "$3" ]; then ok "$1 $2: $3"; else bad "$1 $2: got [$got] wanted [$3]"; fi
}
mtr() {
  local got; got="$(report_of "$1" "$2")"
  if printf '%s' "$got" | grep -aEq -- "$3"; then ok "$1 $2: $got"; else bad "$1 $2: got [$got] wanted /$3/"; fi
}
nomt() { if [ -z "$(report_of "$1" "$2")" ]; then ok "$1 $2: never reported"; else bad "$1 $2 was reported: $(report_of "$1" "$2")"; fi; }
# mtw GROUP NAME WORKER: the line came from that worker (its name), "" for the page itself.
mtw() {
  local got; got="$(who_of "$1" "$2")"
  if [ "$got" = "$3" ]; then ok "$1 $2 came from [${3:-the page}]"; else bad "$1 $2 came from [$got] wanted [$3]"; fi
}
srvn() { grep -acE -- "$1" "$T/server.log" || true; }
have_console() { wait_log "^\[browser\] console $1.*$2" "${3:-5}"; }
finish_of() { wait_log "$CL $1 end " "${2:-40}"; }
want() { [ -z "${WORKERS_TEST_ONLY:-}" ] || case " $WORKERS_TEST_ONLY " in *" $1 "*) return 0;; *) return 1;; esac; }
calib() {
  q move 512 400 click right
  sleep 0.5
  set -- $(log_lines | sed -n 's/^\[browser\] input down \(-\{0,1\}[0-9]*\) \(-\{0,1\}[0-9]*\) button=2 .*/\1 \2/p' | tail -1)
  if [ -n "${1:-}" ]; then BX=$((512 - $1)); BY=$((400 - $2)); else BX=86; BY=91; bad "no right click logged, guessing the window's place"; fi
}

ser "wait@120:$PR"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser --log-input http://$H/basic.html &" "wait:$PR"

if want basic; then
echo "-- the basics: classic, module, importScripts, blob and data workers"
finish_of basic 60 && ok "basic.html ran to its end" || bad "basic.html did not finish: $(log_lines | grep -a 'WRKTEST\|console' | tail -5)"
mt basic classic "fib(25) = 75025"
mt basic module "module says hello v7 late=dynamic url=/module-worker.mjs"
mt basic importScripts "BA ab missing:NetworkError"
mt basic blob "blob worker says hi from blob:"
mt basic data "data worker, origin null data:"
mt basic scope "[object DedicatedWorkerGlobalScope] undefined/undefined/undefined"
mtr basic location "^/basic-worker\.js http://www\.milk\.test:$P false calc$"
[ "$(srvn '^http GET www\.milk\.test /basic-worker\.js ')" = 1 ] && ok "the classic worker's script was fetched once" || bad "basic-worker.js was fetched $(srvn '^http GET www\.milk\.test /basic-worker\.js ') times"
[ "$(srvn 'GET .* /module-worker\.mjs ')" = 1 ] && ok "and the module's" || bad "module-worker.mjs fetched $(srvn 'GET .* /module-worker\.mjs ') times"
[ "$(srvn 'GET .* /lib/(greet|version|late)\.mjs ')" = 3 ] && ok "its imports once each (static and dynamic)" || bad "the module's imports: $(srvn 'GET .* /lib/(greet|version|late)\.mjs ')"
grep -aE '^http GET www\.milk\.test /basic-worker\.js .*dest=(worker|empty|script)?' "$T/server.log" >/dev/null && ok "(the worker script request was logged)" || bad "no request for basic-worker.js"
[ "$(srvn 'blob')" = 0 ] && ok "the blob worker fetched nothing" || bad "something asked the server for the blob"
shot workers-01-basic
fi
if want net; then
echo "-- a worker on the network"
go "$H/net.html"
finish_of net 60 && ok "net.html ran to its end" || bad "net.html did not finish: $(log_lines | grep -a 'WRKTEST net\|console' | tail -5)"
mtw net worker-started netw
mt net same-origin "answer 42"
mt net cors-open "open true"
mt net cors-closed "failed TypeError: Failed to fetch"
mtr net echo "^cookie=wk=1 site=null referer=http://www\.milk\.test:PORT/net-worker\.js$"
mt net echo-cross "cookie=null origin=http://www.milk.test:PORT site=null"
mt net echo-include "failed TypeError: Failed to fetch"
mt net xhr 'status 200 {"answer":42}'
mt net xhr-blocked "error event"
mt net import "cors:ok nocors:NetworkError badtype:NetworkError same:ok corsLoaded=string nocorsLoaded=undefined"
have_console error "\[worker netw\].*Access-Control|\[worker netw\].*CORS" 5 && ok "the console says why the fetch was blocked, naming the worker" || bad "no CORS line tagged [worker netw]: $(log_lines | grep -a 'console error' | tail -3)"
shot workers-02-net
fi
if want xfer; then
echo "-- buffers, ports and the structured clone"
go "$H/xfer.html"
finish_of xfer 60 && ok "xfer.html ran to its end" || bad "xfer.html did not finish: $(log_lines | grep -a 'WRKTEST xfer\|console' | tail -5)"
mt xfer sender-detached "0 0"
mt xfer worker-buffer "2097152 sum=267386880"
mtw xfer worker-buffer xfer
mt xfer worker-detached "0"
mt xfer buffer-back "2097152 bytes, [7] = 21, last = 1572861"
mt xfer worker-port "1 true"
mt xfer port-to-worker "ready > echo:hello"
mt xfer port-from-worker "upper:SHOUT"
mt xfer clone "86400000 a 2 gix+ 3 1001 true true true"
mt xfer not-cloneable "DataCloneError"
shot workers-03-xfer
fi
if want terminate; then
echo "-- terminate()"
go "$H/terminate.html"
finish_of terminate 60 && ok "terminate.html ran to its end" || bad "terminate.html did not finish: $(log_lines | grep -a 'WRKTEST terminate\|console' | tail -5)"
mt terminate terminated "after 6 ticks"
mt terminate late "0"
sleep 0.5
C1="$(srvn '^http GET www\.milk\.test /count/term ')"
sleep 1.5
C2="$(srvn '^http GET www\.milk\.test /count/term ')"
[ "$C1" = "$C2" ] && [ "$C1" -ge 1 ] && ok "the server heard the worker ($C1 requests) and then nothing more" || bad "the worker's requests went on or never began ($C1 -> $C2)"
shot workers-04-terminate
fi
if want error; then
echo "-- errors"
go "$H/error.html"
finish_of error 60 && ok "error.html ran to its end" || bad "error.html did not finish: $(log_lines | grep -a 'WRKTEST error\|console' | tail -5)"
mt error message-before-error "about to fail"
mtr error thrown-1 '^Uncaught TypeError.* @ /throws-worker\.js:3$'
mt error thrown-2 "Uncaught RangeError: from a timer @ /throws-worker.js:5"
mtr error syntax '^Uncaught SyntaxError @ /syntax-worker\.js:[0-9]+$'
mt error missing 'error event, message ""'
mt error wrong-type "error event"
mtr error handled '^handled in the worker: .*inside true$'
nomt error handled-leaked
have_console error "\[worker missing\]" 5 && ok "the missing script is explained in the console, tagged with the worker" || bad "nothing about the missing script: $(log_lines | grep -a 'console error' | tail -3)"
shot workers-05-errors
fi
if want many; then
echo "-- more workers than allowed"
go "$H/many.html"
finish_of many 60 && ok "many.html ran to its end" || bad "many.html did not finish"
mt many replies "16"
mt many refused "17"
have_console error "too many workers" 5 && ok "the console says there are too many" || bad "no console line about too many workers"
shot workers-06-many
fi
if want cross; then
echo "-- worker scripts from other origins"
go "$H/cross.html"
finish_of cross 60 && ok "cross.html ran to its end" || bad "cross.html did not finish"
mt cross refused-0 "error event"
mt cross refused-1 "error event"
mtr cross allowed-2 "worker x2 started"
have_console error "cannot be accessed from origin" 5 && ok "the console names the origin problem" || bad "no console line about the origin"
[ "$(srvn 'GET (api\.milk\.test|other\.test) /ping-worker\.js')" = 0 ] && ok "the other origins were never asked for the script" || bad "a cross-origin worker script was requested"
shot workers-07-cross
fi
if want leave; then
echo "-- a page that leaves, a frame that goes"
go "$H/leave.html"
wait_log "$CL leave landed " 40 && ok "the page left" || bad "leave.html did not leave"
sleep 0.5
C1="$(srvn '^http GET www\.milk\.test /count/leave ')"
sleep 1.5
C2="$(srvn '^http GET www\.milk\.test /count/leave ')"
[ "$C1" = "$C2" ] && [ "$C1" -ge 1 ] && ok "the worker was asking the server ($C1 times) and stopped when the page left" || bad "the worker kept going after the page left ($C1 -> $C2)"
go "$H/frame.html"
wait_log "$CL frame removed " 40 && ok "the frame was removed" || bad "frame.html did not remove its frame"
sleep 0.5
C1="$(srvn '^http GET www\.milk\.test /count/inframe ')"
sleep 1.5
C2="$(srvn '^http GET www\.milk\.test /count/inframe ')"
[ "$C1" = "$C2" ] && [ "$C1" -ge 1 ] && ok "the frame's worker ended with its frame ($C1 requests)" || bad "the frame's worker kept going ($C1 -> $C2)"
fi
if want flood; then
echo "-- a lot of messages"
go "$H/flood.html"
finish_of flood 60 && ok "flood.html ran" || bad "flood.html did not finish: $(log_lines | grep -a 'WRKTEST flood\|console' | tail -3)"
mt flood end "ordered=true count=5000"
fi
if want runaway; then
echo "-- a worker that never stops"
go "$H/runaway.html"
wait_log "$CL runaway worker-begins " 40 && ok "the runaway worker started spinning" || bad "the runaway worker never began"
mtw runaway worker-begins spinner
sleep 2
shot workers-08-runaway
T1="$(report_of runaway tick)"
sleep 2
T2="$(report_of runaway tick)"
[ -n "$T1" ] && [ "$T2" != "$T1" ] && ok "the page kept ticking while the worker spun ($T1 -> $T2)" || bad "the page stopped ticking ($T1 -> $T2)"
have_console error "\[worker spinner\].*was stopped" 40 && ok "the runaway worker was stopped, with a console line" || bad "no 'was stopped' line: $(log_lines | grep -a 'console' | tail -4)"
wait_log "$CL runaway good still here: after" 40 && ok "the other worker answered after the runaway was stopped" || bad "the other worker did not answer: $(log_lines | grep -a 'WRKTEST runaway' | tail -3)"
mt runaway good "still here: after"
shot workers-09-runaway-stopped
fi
if want js; then
echo "-- the JavaScript switch"
go "$H/basic.html"
sleep 3
calib
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript off" 5 && ok "the menu turned JavaScript off" || bad "no javascript off"
B1="$(srvn '^http GET www\.milk\.test /basic-worker\.js ')"
go "$H/basic.html"
sleep 4
B2="$(srvn '^http GET www\.milk\.test /basic-worker\.js ')"
[ "$B1" = "$B2" ] && ok "with JavaScript off no worker script was requested" || bad "worker scripts were requested with JavaScript off ($B1 -> $B2)"
shot workers-10-js-off
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript on" 5 && ok "and on again" || bad "no javascript on"
fi
if want after; then
echo "-- after all that"
log_lines | grep -a "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || ok "the Browser stayed up"
go "$H/basic.html"
wait_log "$CL basic end " 40 && ok "and still runs workers" || bad "the Browser didn't recover"
fi
# WORKERS_TEST_SHOTS=DIR keeps the screenshots (the work folder is removed when everything passes).
if [ -n "${WORKERS_TEST_SHOTS:-}" ]; then
  mkdir -p "$WORKERS_TEST_SHOTS" && cp "$T"/workers-*.png "$WORKERS_TEST_SHOTS"/ 2>/dev/null || true
fi
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "workers-test: PASS"
else
  echo "workers-test: FAIL (logs and screenshots in $T)"
  exit 1
fi
