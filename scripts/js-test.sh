#!/usr/bin/env bash
# JavaScript in the Browser, end to end on a private image, against a local test server only
# (scripts/js_test_server.py on the Mac's 127.0.0.1, serving the pages in tests/js; the guest
# reaches it as http://10.0.2.2:PORT/, no DNS, no internet). About a minute and a half.
# The pages report through console.log ("JSTEST name what"), which the Browser prints on the
# serial line as "[browser] console log: ..."; the test clicks and types over QMP and takes
# a screenshot of each:
#   * tabs.html      a tabs widget: a click on a tab shows its panel (classList, dataset)
#   * todo.html      a list built by script; Enter in the field adds an item (submit
#                    cancelled), a click on an item crosses it out
#   * form.html      form validation in script that blocks the submit, then lets it through
#   * timer.html     setInterval / setTimeout with arguments, promise and queueMicrotask order
#   * raf.html       a requestAnimationFrame animation moving a box
#   * innerhtml.html a table built with innerHTML, insertAdjacentHTML, outerHTML; noscript
#                    hidden while scripts are on
#   * order.html     blocking, defer, module (deferred, in order with them), data-block scripts, document.write,
#                    DOMContentLoaded and load in order, document.currentScript
#   * storage.html   localStorage across a reload (kept in ~/Library/Browser/storage),
#                    document.cookie without the HttpOnly cookie
#   * runaway.html   while(true) {}: the "page is slow" bar comes up, Esc stops it, the
#                    Browser goes on
#   * bigscript.html a generated 3 MB bundle (thousands of functions, closures, object and array
#                    literals) loads and runs; bigframe.html the same, 0.5 MB, in an iframe
#   * mc.html        MessageChannel between a page and its frame: structured copies in order,
#                    a port handed on through a port, 500 queued messages in order while a
#                    timer keeps running
#   * webapis.html   MutationObserver (after the script, before timers), IntersectionObserver
#                    and ResizeObserver first reports, DOMParser, Range, TreeWalker,
#                    structuredClone, scheduler, BroadcastChannel, getComputedStyle
#   * the JavaScript switch (the toolbar's menu): off, the noscript text shows, on again
#   scripts/js-test.sh [workdir]
# Only the qemu and python processes started here are killed (by PID). The work directory is
# removed when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-jstest.XXXXXX)}"
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
(cd "$T" && exec python3 -I "$REPO/scripts/js_test_server.py" "$T/ports" "$REPO/tests/js" >"$T/server.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$T/ports" ] && break; sleep 0.1; done
[ -s "$T/ports" ] || { echo "js-test: FAIL: the test server did not start"; exit 1; }
eval "$(tr ' ' '\n' <"$T/ports" | sed 's/^/P_/')"
P="$P_http"
H="10.0.2.2:$P"
echo "server: http 127.0.0.1:$P (pid $SPID)"
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -chardev "socket,id=s0,path=$SOCK,server=on,wait=off,logfile=$LOG" -serial chardev:s0 \
  -device virtio-gpu-device -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
  -netdev user,id=n0 -device virtio-net-device,netdev=n0 \
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
out() { clean_log | awk -v c="$1" 'on && /^msh:/ {on=0} !on && ($0 == c || ($0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c)) {on=1; buf=""; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'; }
# wait_log REGEX [SECS]: until the serial log has a line matching REGEX.
wait_log() { for _ in $(seq $(( ${2:-20} * 10 ))); do log_lines | grep -E -- "$1" >/dev/null && return 0; sleep 0.1; done; return 1; }
wait_srv() { for _ in $(seq $(( ${2:-20} * 10 ))); do grep -Eq -- "$1" "$T/server.log" && return 0; sleep 0.1; done; return 1; }
# A page's report: wait for "JSTEST NAME REGEX".
js() { wait_log "^\[browser\] console log: JSTEST $1 $2" "${3:-20}"; }
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
differ() { python3 scripts/shotwait.py --differ "$T/$1.png" "$T/$2.png"; }
esc_re() { printf '%s' "$1" | sed 's/[.[\*^$()+?{|]/\\&/g'; }
loaded() { wait_log "^\[browser\] $1" "${2:-20}"; }
go() { q key meta_l-l type "$1" key ret; }
# The page area starts this far below the window body's top (the tab strip and toolbar).
TOP=72
# click_at ID: click the element a page reported with "JSTEST pos ID X Y" (its last report).
click_at() {
  set -- $(log_lines | sed -n "s/^\[browser\] console log: JSTEST pos $1 \(-\{0,1\}[0-9]*\) \(-\{0,1\}[0-9]*\)$/\1 \2/p" | tail -1)
  if [ -z "${1:-}" ]; then bad "no position reported for an element"; return; fi
  q move $((BX + $1)) $((BY + TOP + $2)) click
}

ser "wait@120:$PR"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser --log-input http://$H/tabs.html &" "wait:$PR"
loaded "http://$(esc_re "$H")/tabs.html: 200 OK, [0-9]+ bytes, 'Tabs widget'" 30 && ok "the tabs page loaded" || bad "no tabs page"
# Where the window's body is: a right click (it does nothing on a page) is logged in body
# coordinates.
q move 512 400 click right
set -- $(log_lines | sed -n 's/^\[browser\] input down \(-\{0,1\}[0-9]*\) \(-\{0,1\}[0-9]*\) button=2 .*/\1 \2/p' | tail -1)
if [ -n "${1:-}" ]; then BX=$((512 - $1)); BY=$((400 - $2)); else BX=86; BY=91; bad "no right click logged, guessing the window's place"; fi
echo "  (window body at $BX,$BY)"

echo "-- a tabs widget"
wait_srv "^http GET /common\.js" && ok "the external script was fetched" || bad "common.js wasn't fetched"
js tabs "ready" && ok "its scripts ran (DOMContentLoaded)" || bad "no ready report: $(log_lines | grep -a 'console' | tail -3)"
shot js-01-tabs
click_at t2
js tabs "shown p2 Cheese is made from milk" && ok "a click on a tab showed its panel" || bad "the tab click: $(log_lines | grep -a 'JSTEST tabs' | tail -1)"
shot js-02-tabs-clicked js-01-tabs
differ js-01-tabs js-02-tabs-clicked && ok "the page redrew with the other panel" || bad "the page looks the same after the tab click"

echo "-- a todo list built by script"
go "$H/todo.html"
js todo "ready 3" && ok "three items built with createElement" || bad "the todo list: $(log_lines | grep -a 'JSTEST todo' | tail -1)"
shot js-03-todo
click_at what
q type "Make cheese" key ret
js todo "items 4 last Make cheese" && ok "Enter added an item (the submit was cancelled, no navigation)" || bad "adding an item: $(log_lines | grep -a 'JSTEST todo' | tail -1)"
wait_srv "^http GET /todo\.html\?" 2 && bad "the form was submitted to the server" || ok "nothing was sent to the server"
click_at item0
js todo "toggled 0 done" && ok "a click on an item crossed it out" || bad "toggling an item"
shot js-04-todo-added js-03-todo
differ js-03-todo js-04-todo-added && ok "the list redrew" || bad "the list looks the same"

echo "-- form validation that blocks the submit"
go "$H/form.html"
js form "ready" || bad "no form page"
click_at email
q type "not an address" key ret
js form "blocked 1 'not an address'" && ok "a bad address: the submit was blocked" || bad "the bad address went through: $(log_lines | grep -a 'JSTEST form' | tail -1)"
shot js-05-form-blocked
wait_srv "^thanks" 2 && bad "the blocked form reached the server" || ok "nothing reached the server"
for _ in $(seq 14); do q key backspace; done
# (Built with printf so no e-mail-shaped string is in the repo.)
ADDR="milk$(printf '\100')cow.test"
q type "$ADDR" key ret
js form "sending $(esc_re "$ADDR")" && ok "a good address: the script let it through" || bad "the good address: $(log_lines | grep -a 'JSTEST form' | tail -1)"
wait_srv "^thanks email=$(esc_re "$ADDR")$" && ok "the form arrived (GET /thanks?email=...)" || bad "no /thanks request: $(tail -2 "$T/server.log")"
loaded "http://$(esc_re "$H")/thanks\?email=milk%40cow\.test: 200 OK" && ok "the thanks page loaded" || bad "no thanks page"

echo "-- timers and microtasks"
go "$H/timer.html"
js timer "microtask first" && ok "a promise reaction ran" || bad "no promise reaction"
js timer "queued microtask" && ok "queueMicrotask ran" || bad "no queueMicrotask"
js timer "timeout milk!" && ok "setTimeout passed its extra arguments" || bad "the timeout"
js timer "five in-time" && ok "setInterval ran five times, 100 ms apart, then stopped" || bad "the interval: $(log_lines | grep -a 'JSTEST timer' | tail -1)"
ORDER="$(log_lines | { grep -a 'JSTEST timer' || true; } | tail -4 | sed 's/.*JSTEST timer //' | tr '\n' ',')"
[ "$ORDER" = "microtask first,queued microtask,timeout milk!,five in-time," ] && ok "in order: microtasks, the timeout, the interval" || bad "timer order: $ORDER"
shot js-06-timer

echo "-- requestAnimationFrame"
go "$H/raf.html"
js raf "done 50 moved yes" 20 && ok "50 animation frames moved the box" || bad "the animation: $(log_lines | grep -a 'JSTEST raf' | tail -1)"
shot js-07-raf

echo "-- innerHTML"
go "$H/innerhtml.html"
js innerhtml "rows 4 cells 6 total 16 ns hidden" && ok "a table built with innerHTML (noscript hidden)" || bad "innerHTML: $(log_lines | grep -a 'JSTEST innerhtml' | tail -1)"
js innerhtml 'outer <p id="after">Added after\.</p>' && ok "insertAdjacentHTML and outerHTML" || bad "insertAdjacentHTML: $(log_lines | grep -a 'JSTEST innerhtml' | tail -1)"
shot js-08-innerhtml

echo "-- script order"
go "$H/order.html"
js order "inline1 loading, ext b loading, inline2, ext a interactive, module, DOMContentLoaded, load complete \| written yes" && ok "blocking, defer, a module (deferred, after ext a), DOMContentLoaded, load (the template not run), document.write" || bad "order: $(log_lines | grep -a 'JSTEST order' | tail -1)"

echo "-- storage and cookies"
go "$H/storage.html"
js storage "visits 1 cookie set httponly hidden" && ok "localStorage, document.cookie (the HttpOnly cookie stays hidden)" || bad "storage: $(log_lines | grep -a 'JSTEST storage' | tail -1)"
q key meta_l-r
js storage "visits 2 cookie set httponly hidden" && ok "localStorage kept across a reload" || bad "storage after reload: $(log_lines | grep -a 'JSTEST storage' | tail -1)"
wait_srv "^http GET /storage\.html cookie=.*jsc=1" 5 && ok "the cookie a script set went to the server" || bad "the script's cookie wasn't sent: $(grep 'GET /storage' "$T/server.log" | tail -1)"

echo "-- a runaway script"
go "$H/runaway.html"
js runaway "starting" || bad "no runaway page"
wait_log "^\[browser\] this page is slow" 20 && ok "the slow bar came up" || bad "no slow bar"
shot js-09-slow
q key esc
wait_log "^\[browser\] stopped the page's scripts" 10 && ok "Esc stopped the script" || bad "the script wasn't stopped"
wait_log "^\[browser\] console warn: The page's scripts were stopped" 5 && ok "the console says so" || bad "no console note"
sleep 1
go "$H/tabs.html"
js tabs "ready" 20 && ok "the Browser goes on (another page's scripts run)" || bad "the Browser didn't recover"
log_lines | grep -a "JSTEST runaway never" >/dev/null && bad "a timer of the stopped page ran" || ok "the stopped page's timers are gone"

echo "-- a big script"
go "$H/bigscript.html?mb=3"
js bigscript "ok [0-9]+ [0-9]+/[0-9]+ mb 3" 60 && ok "a generated 3 MB bundle loaded and every 50th function returned the right value" || bad "the big script: $(log_lines | grep -aE 'JSTEST bigscript|SyntaxError|too large' | tail -2)"
log_lines | grep -a "JSTEST bigtime" | tail -1 | sed 's/.*JSTEST bigtime /  (3 MB, ms to load and run: /;s/$/)/'
shot js-12-bigscript
clean_log | grep -a "too large to fit in memory" >/dev/null && bad "the engine said the code was too large" || ok "no out-of-memory refusals"
go "$H/bigframe.html"
js bigframe "ok [0-9]+" 60 && ok "the same kind of bundle (0.5 MB) ran in an iframe, under the smaller frame limits" || bad "the big script in a frame: $(log_lines | grep -aE 'JSTEST bigframe|frame.*Error' | tail -2)"
shot js-13-bigframe

echo "-- MessageChannel between frames"
go "$H/mc.html"
js mc "posted, port2 is object" 30 && ok "the page posted to the frame (port2 handed over)" || bad "mc page: $(log_lines | grep -a 'JSTEST mc' | tail -1)"
js mc 'echoes \[\{"n":1,"list":2,"date":0,"map":"a"\},"two",\[3\]\]' 30 && ok "three messages came back in order, copied (Date and Map survived)" || bad "mc echoes: $(log_lines | grep -a 'JSTEST mc' | tail -2)"
js mc "transferred port answered from the frame" 30 && ok "a port handed on through a port works in the frame" || bad "mc transferred port"
js mc "bulk 500 in order, timers ran yes" 40 && ok "500 queued messages arrived in order and timers kept running" || bad "mc bulk: $(log_lines | grep -a 'JSTEST mc' | tail -2)"
shot js-14-mc

echo "-- web APIs"
go "$H/webapis.html"
js webapis "mutations attributes,childList" 30 && ok "MutationObserver reported the attribute and child changes" || bad "mutation observer: $(log_lines | grep -a 'JSTEST webapis' | tail -2)"
js webapis "order promise>mo2>timer" 10 && ok "records come after the promise job and before the timer" || bad "order: $(log_lines | grep -a 'JSTEST webapis order' | tail -1)"
js webapis "intersection true:1" 20 && ok "IntersectionObserver reported the box as visible" || bad "intersection observer"
js webapis "resize 120x40" 20 && ok "ResizeObserver reported the box's size" || bad "resize observer"
js webapis "parsed 2 UL" && ok "DOMParser built a document" || bad "domparser"
js webapis "range \[box\]" && ok "a Range over the box reads its text" || bad "range"
js webapis "walked yes" && ok "TreeWalker walked the page" || bad "treewalker"
js webapis "clone 5 1" && ok "structuredClone copied a Date and a Set" || bad "structuredClone"
js webapis "task 42" && ok "scheduler.postTask ran" || bad "scheduler"
js webapis "broadcast hi" && ok "BroadcastChannel delivered between two channels" || bad "broadcast"
js webapis "style 120px block" && ok "getComputedStyle(...).getPropertyValue works" || bad "computed style"
shot js-15-webapis

echo "-- the JavaScript switch"
go "$H/innerhtml.html"
js innerhtml "rows 4" || true
shot js-10-js-on
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript off" 5 && ok "the menu turned JavaScript off" || bad "no javascript off"
loaded "http://$(esc_re "$H")/innerhtml.html: 200 OK" 10 || true
sleep 1
shot js-11-js-off js-10-js-on
differ js-10-js-on js-11-js-off && ok "without scripts the page looks different (no table, the noscript text)" || bad "the page looks the same with JavaScript off"
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript on" 5 && ok "and on again" || bad "no javascript on"
ser "send:text show /Users/milk/Library/Browser/settings" "wait:$PR" "send:files list /Users/milk/Library/Browser/storage" "wait:$PR"
out "text show /Users/milk/Library/Browser/settings" | grep "javascript=on" >/dev/null && ok "the switch is kept in ~/Library/Browser/settings" || bad "settings: $(out "text show /Users/milk/Library/Browser/settings")"
out "files list /Users/milk/Library/Browser/storage" | grep "http%3A%2F%2F10.0.2.2%3A$P" >/dev/null && ok "localStorage is kept per origin in ~/Library/Browser/storage" || bad "storage folder: $(out "files list /Users/milk/Library/Browser/storage")"
clean_log | grep -i "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || true
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "js-test: PASS"
else
  echo "js-test: FAIL (logs and screenshots in $T)"
  exit 1
fi
