#!/usr/bin/env bash
# The Browser's memory, end to end on private images, against local test servers only
# (scripts/memory_test_servers.py on the Mac's 127.0.0.1: a DNS server for a made-up zone and one web
# server serving tests/memory plus generated pages, bundles and bodies; the guest is told internet=on with
# that DNS server, like fetch-test, so names work and nothing leaves the Mac). About six minutes.
# Pages, frames and workers report through console.log ("MEMTEST group name value"), which the Browser
# prints on the serial line, and the Browser says what it does about memory on lines starting
# "[browser] memory" (it is run with --log-input). Checks:
#   boot 1 (512 MB)
#   * kernel     /proc/mempressure and MemAvailable (docs/memory.md "Memory pressure")
#   * holds      a worker, a frame and a page each hold 100 MB (and 200 MB) of ordinary objects: they
#                used to be stopped at 32, 16 and 64 MB; the page keeps ticking
#   * bundle     a 3 MB bundle of plain functions loads in a page and in a frame
#   * bigpage    a 40 MB page streams in: its top is on screen before the server has sent it all, a bar
#                says it is too big to show completely, and "Load more" takes more of it
#   * stream     a 200 MB fetch body read piece by piece, the page stopping for six seconds in the
#                middle: the server cannot get ahead, and every byte arrives; .text() and XHR over
#                the memory-derived limit are errors the page can catch
#   * download   100 MB saved to ~/Downloads (it was refused above 64 MB), the Browser's memory flat
#   * eater      a page that eats everything is stopped alone (the console and a bar say so), another
#                tab keeps ticking, the desktop answers, free memory comes back, the kernel never saw
#                memory run out
#   * pressure   five tabs of 70 MB: the ladder drops caches, collects, discards a background tab; the
#                tab comes back from its address when it is opened
#   * churn      frames, workers and tabs made and dropped many times: memory returns to where it was
#   boot 2 (192 MB)
#   * askstop    one page holds more than the machine can spare: "This page uses a lot of memory. Stop
#                it?" with Stop and Wait; Stop ends that page's scripts and the memory comes back
#   boot 3 (1 GB)
#   * big        a worker and a frame each hold 200 MB, three frames hold 150 MB each
#   scripts/memory-test.sh [workdir]      (MEMORY_TEST_ONLY="holds eater" runs only those)
#   MEMORY_TEST_FULL=1 adds a 2 GB boot with 400 MB holders
# Only the qemu and python processes started here are killed (by PID). The work directory is removed
# when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-memtest.XXXXXX)}"
mkdir -p "$T"
M="python3 scripts/mkmilkfs.py"
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
QPID="" SPID=""
stop_vm() {
  [ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true
  [ -n "$QPID" ] && wait "$QPID" 2>/dev/null || true
  QPID=""
}
cleanup() {
  stop_vm
  [ -n "$SPID" ] && kill "$SPID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
rm -f "$T/ports"
(cd "$T" && exec python3 -I "$REPO/scripts/memory_test_servers.py" "$T/ports" "$REPO/tests/memory" >"$T/server.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$T/ports" ] && break; sleep 0.1; done
[ -s "$T/ports" ] || { echo "memory-test: FAIL: the test servers did not start"; exit 1; }
eval "$(tr ' ' '\n' <"$T/ports" | sed 's/^/P_/')"
P="$P_http"
H="www.milk.test:$P"
echo "servers: dns 127.0.0.1:$P_dns, http 127.0.0.1:$P (pid $SPID)"
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
PR='msh:[^\n]*\$ '
n=0
ser() { # steps...: run console steps, wait for them to finish, disconnect
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$T/drive.log" "$@" "done:$T/ser.$n" &
  local d=$!
  for _ in $(seq 900); do [ -e "$T/ser.$n" ] && break; sleep 0.2; done
  kill "$d" 2>/dev/null || true; wait "$d" 2>/dev/null || true
}
q() { python3 scripts/qmp.py "$QMP" "$@"; }
clean_log() { tr -d '\r' < "$LOG"; }
log_lines() { clean_log | sed -E 's/^msh:[^$]*\$ //'; }
out() { clean_log | awk -v c="$1" 'on && /^msh:/ {on=0} !on && ($0 == c || ($0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c)) {on=1; buf=""; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'; }
wait_log() { for _ in $(seq $(( ${2:-20} * 10 ))); do log_lines | grep -aE -- "$1" >/dev/null && return 0; sleep 0.1; done; return 1; }
wait_srv() { for _ in $(seq $(( ${2:-20} * 10 ))); do grep -aEq -- "$1" "$T/server.log" && return 0; sleep 0.1; done; return 1; }
srvn() { grep -acE -- "$1" "$T/server.log" || true; }
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
go() { q key meta_l-l type "$1" key ret; }
newtab() { q key meta_l-t; sleep 0.8; go "$1"; }
CL='^\[browser\] console log( \[(worker|frame) [^]]*\])?: MEMTEST'
# report GROUP NAME: the last "MEMTEST GROUP NAME value" line's value.
report_of() { log_lines | { grep -aE "$CL $1 $2 " || true; } | tail -1 | sed -E "s/$CL $1 $2 //"; }
count_of() { log_lines | { grep -aE "$CL $1 $2( |\$)" || true; } | wc -l | tr -d ' '; }
have() { wait_log "$CL $1 $2 $3" "${4:-30}"; }
want() { [ -z "${MEMORY_TEST_ONLY:-}" ] || case " $MEMORY_TEST_ONLY " in *" $1 "*) return 0;; *) return 1;; esac; }
# shell CMD: type CMD on the console and wait for the prompt, again if the Browser's output
# swallowed the typing (its lines come all the time while a page ticks).
shell() {
  local tries=0 before
  while [ "$tries" -lt 6 ]; do
    before="$(wc -c < "$LOG" | tr -d ' ')"
    ser "send:$1" "wait:$PR"
    if tail -c +$((before + 1)) "$LOG" | tr -d '\r' | grep -aF -- "$1" >/dev/null; then return 0; fi
    tries=$((tries + 1))
  done
  return 1
}
# kb KEY FILE: the number of kB (or the plain number) on line "KEY:" of a /proc file, read in the guest.
kb() {
  shell "text show $2"
  out "text show $2" | sed -n "s/^$1: *\([0-9]*\).*/\1/p" | tail -1
}
memfree() { kb MemFree /proc/meminfo; }
# The Browser's own memory in MB, from `processes list` (the `memory` column, like "54M").
browser_mb() {
  shell "processes list | filter name == browser"
  out "processes list | filter name == browser" | awk '$NF == "browser" { v = $(NF-1); sub(/[MK]$/, "", v); print v }' | tail -1
}
calib() {
  q move 512 400 click right
  sleep 0.5
  set -- $(log_lines | sed -n 's/^\[browser\] input down \(-\{0,1\}[0-9]*\) \(-\{0,1\}[0-9]*\) button=2 .*/\1 \2/p' | tail -1)
  if [ -n "${1:-}" ]; then BX=$((512 - $1)); BY=$((400 - $2)); else BX=86; BY=91; bad "no right click logged, guessing the window's place"; fi
}
# boot MEM: a fresh VM and image, the serial held until we connect.
boot() {
  stop_vm
  : > "$LOG"; n=0
  local img="$T/a.img"
  rm -f "$img" "$SOCK" "$QMP" "$T"/ser.*
  $M mkfs "$img" --size 384M >/dev/null
  $M mkdir "$img" /Users/milk >/dev/null
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m "$1" -rtc base=localtime \
    -kernel "$ELF.bin" -chardev "socket,id=s0,path=$SOCK,server=on,wait=on,logfile=$LOG" -serial chardev:s0 \
    -device virtio-gpu-device -display none \
    -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
    -drive "file=$img,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -netdev user,id=n0 -device virtio-net-device,netdev=n0 \
    -fw_cfg "name=opt/milkos/net,string=internet=on dns=10.0.2.2:$P_dns" \
    -qmp "unix:$QMP,server,nowait" >"$T/qemu.out" 2>&1 &
  QPID=$!
  ser "wait@120:$PR"
  q move 785 615 click sleep 1
}
# launch URL: the Browser, logging what it does (and keeping its window where calib finds it).
launch() {
  shell "/Apps/browser/browser --log-input '$1' &"
  wait_log '^\[browser\] http://' 30 || true
  calib
}

# ---- boot 1: 512 MB -------------------------------------------------------------------------------------
echo "== boot 1: 512 MB"
boot 512M
launch "http://$H/ticker.html"
wait_log '^\[browser\] memory: ' 10 && ok "the Browser reads the machine's memory and says so" || bad "no [browser] memory line"
BASE_FREE="$(memfree)"
echo "  (free memory at the start: $((BASE_FREE / 1024)) MB)"

if want kernel; then
echo "-- the kernel's memory files"
shell "text show /proc/mempressure"
shell "text show /proc/meminfo"
MP="$(out "text show /proc/mempressure")"
echo "$MP" | grep -aE '^level: +normal$' >/dev/null && ok "/proc/mempressure says normal" || bad "mempressure: $MP"
echo "$MP" | grep -aE '^LowMark: +65536 kB$' >/dev/null && echo "$MP" | grep -aE '^CriticalMark: +32768 kB$' >/dev/null && ok "the marks are 1/8 and 1/16 of the 512 MB" || bad "marks: $MP"
echo "$MP" | grep -aE '^AllocFailures: +0$' >/dev/null && ok "no allocation has failed since boot" || bad "alloc failures: $MP"
out "text show /proc/meminfo" | grep -aE '^MemAvailable: +[0-9]+ kB$' >/dev/null && ok "/proc/meminfo has MemAvailable" || bad "no MemAvailable"
fi

if want holds; then
echo "-- a worker, a frame and a page hold 100 and 200 MB (once stopped at 32, 16 and 64 MB)"
go "$H/worker.html?mb=100"
have worker held '100MB' 40 && ok "a worker held 100 MB of objects" || bad "worker 100: $(log_lines | grep -aE 'MEMTEST worker|stopped|ran out' | tail -3)"
have worker message pong 20 && ok "and still answered a message" || bad "no pong"
T1="$(count_of worker tick)"; sleep 2; T2="$(count_of worker tick)"
[ "$T2" -gt "$T1" ] && ok "the page went on ticking" || bad "the page stopped ticking ($T1 -> $T2)"
go "$H/frame.html?mb=100"
have frame held '100MB' 40 && ok "a frame held 100 MB" || bad "frame 100: $(log_lines | grep -aE 'MEMTEST frame|stopped|ran out' | tail -3)"
go "$H/page.html?mb=150&kind=mixed"
have page held '150MB' 40 && ok "a page held 150 MB of objects, strings and typed arrays" || bad "page 150: $(log_lines | grep -aE 'MEMTEST page|stopped|ran out' | tail -3)"
go "$H/worker.html?mb=200&kind=strings"
have worker held '200MB' 60 && ok "a worker held 200 MB of strings" || bad "worker 200: $(log_lines | grep -aE 'MEMTEST worker|stopped|ran out' | tail -3)"
go "$H/worker.html?mb=150&kind=objects&sync=1"
have worker held '150MB' 60 && ok "a worker held 150 MB built in one task that never returned to the window (its limit grew while it ran)" || bad "worker sync 150: $(log_lines | grep -aE 'MEMTEST worker|stopped|ran out' | tail -3)"
go "$H/frame.html?mb=200&kind=objects"
have frame held '200MB' 60 && ok "a frame held 200 MB of objects" || bad "frame 200: $(log_lines | grep -aE 'MEMTEST frame|stopped|ran out' | tail -3)"
log_lines | grep -aE '^\[browser\] console error.*(ran out of memory|Terminated)' >/dev/null && bad "something was stopped for memory: $(log_lines | grep -aE 'ran out of memory' | tail -2)" || ok "nothing was stopped for memory"
log_lines | grep -aE '^\[browser\] memory: .*\[[0-9a-f]+:(Worker|Frame|Page) ' >/dev/null && ok "the log shows each realm's heap and its limit" || bad "no per-realm numbers"
shot memory-01-holds
fi

if want bundle; then
echo "-- a 3 MB bundle of plain functions"
go "$H/bundle.html?mb=3"
have bundle page-loaded 3 60 && ok "the bundle loaded in the page" || bad "bundle in page: $(log_lines | grep -aE 'MEMTEST bundle|SyntaxError|too large' | tail -3)"
have bundle frame-loaded 3 60 && ok "and in a frame" || bad "bundle in frame: $(log_lines | grep -aE 'MEMTEST bundle|SyntaxError|too large' | tail -3)"
log_lines | grep -aE 'MEMTEST bundle loaded [0-9]+ functions, ran' | wc -l | tr -d ' ' | { read c; [ "$c" -ge 2 ] && ok "its functions ran (the page's and the frame's)" || bad "the bundle ran $c times"; }
log_lines | grep -a "too large to fit in memory" >/dev/null && bad "the engine said the code was too large" || ok "no out-of-memory refusals"
fi

if want bigpage; then
echo "-- a 40 MB page"
S0="$(srvn '^ +[0-9.]+ big done')"
go "$H/big.html?mb=40&gap=40"
wait_log '^\[browser\] streaming http://www\.milk\.test:[0-9]+/big\.html\?mb=40&gap=40: showing the top after' 40 && ok "the top of the page was shown while the rest came" || bad "no preview: $(log_lines | grep -aE 'streaming|big' | tail -3)"
[ "$(srvn '^ +[0-9.]+ big done')" = "$S0" ] && ok "(the server had not finished sending it yet)" || bad "the server had already sent it all when the top was shown"
shot memory-02-big-top
# (The Browser takes the page up to what this machine allows for one page and hangs up there;
# on a roomier machine the server sends it all.)
wait_srv '^ +[0-9.]+ big (done|aborted)' 60 && ok "the page came until the Browser had enough ($(grep -aE 'big (done|aborted)' "$T/server.log" | tail -1 | sed 's/^ *[0-9.]* //'))" || bad "big page not taken: $(tail -3 "$T/server.log")"
wait_log '^\[browser\] console warn: This page is too big to show completely' 40 && ok "the console says the page is too big to show completely" || bad "no too-big line: $(log_lines | grep -a 'console' | tail -3)"
log_lines | grep -aE '^\[browser\] parsed [0-9]+ KB in [0-9]+ ms' | tail -1 | sed 's/^/  (/;s/$/)/'
shot memory-03-big-bar
# A page that is too big only a little (about 360,000 nodes against the 200,000 this machine takes
# at first): Load more takes it whole.
W0="$(log_lines | grep -ac '^\[browser\] console warn: This page is too big to show completely')"
go "$H/big.html?mb=12&gap=2"
for _ in $(seq 120); do [ "$(log_lines | grep -ac '^\[browser\] console warn: This page is too big to show completely')" -gt "$W0" ] && break; sleep 0.5; done
[ "$(log_lines | grep -ac '^\[browser\] console warn: This page is too big to show completely')" -gt "$W0" ] && ok "a 12 MB page is cut too (more nodes than 200,000)" || bad "no bar for the 12 MB page"
shot memory-03b-small-bar
S1="$(srvn '^ *[0-9.]+ big done 12 MB')"
W1="$(log_lines | grep -ac '^\[browser\] console warn: This page is too big to show completely')"
# "Load more" is the first button of the bar over the foot of the page.
q move $((BX + 738)) $((BY + 558)) click
for _ in $(seq 120); do [ "$(srvn '^ *[0-9.]+ big done 12 MB')" -gt "$S1" ] && break; sleep 0.5; done
[ "$(srvn '^ *[0-9.]+ big done 12 MB')" -gt "$S1" ] && ok "Load more fetched the page again" || bad "Load more did nothing ($(srvn '^ *[0-9.]+ big done 12 MB') sends)"
sleep 4
[ "$(log_lines | grep -ac '^\[browser\] console warn: This page is too big to show completely')" = "$W1" ] && ok "with four times the room it is shown completely (no second warning)" || bad "the warning came again"
[ "$(log_lines | grep -ac '^\[browser\] parsed 12288 KB')" -ge 2 ] && ok "(parsed twice: $(log_lines | grep -a '^\[browser\] parsed 12288 KB' | tail -1))" || bad "the 12 MB page was not parsed twice"
shot memory-04-big-more
fi

if want stream; then
echo "-- a 200 MB fetch body, read piece by piece"
go "$H/fetch-stream.html?mb=200&pause=6000"
have stream paused 262144 40 && ok "the page read 256 KB and stopped reading" || bad "stream did not begin: $(log_lines | grep -aE 'MEMTEST stream|console error' | tail -3)"
sleep 3
if grep -aE '^ +[0-9.]+ stream done' "$T/server.log" >/dev/null; then bad "the server finished sending 200 MB to a page that had stopped reading"; else ok "the server could not get ahead of the page (no end after three seconds of the page not reading)"; fi
have stream end '209715200 bytes in' 90 && ok "all 200 MB arrived once the page read on" || bad "stream end: $(log_lines | grep -aE 'MEMTEST stream|console error' | tail -3)"
wait_srv '^ +[0-9.]+ stream done 200 MB' 10 && ok "the server's send ended after the page's reading did" || bad "no server end"
MB="$(browser_mb)"
[ -n "$MB" ] && [ "$MB" -lt 120 ] && ok "the Browser held ${MB} MB, not 200" || bad "the Browser holds ${MB:-?} MB after streaming 200 MB"
go "$H/fetch-text.html?mb=60"
have text error 'TypeError: The response body is bigger than this' 40 && ok ".text()-like reads over the memory limit are an error the page can catch" || bad "text 60: $(log_lines | grep -aE 'MEMTEST text' | tail -2)"
go "$H/fetch-text.html?mb=20"
have text loaded 20971520 40 && ok "20 MB read whole is fine" || bad "text 20: $(log_lines | grep -aE 'MEMTEST text' | tail -2)"
go "$H/fetch-text.html?mb=60&xhr=1"
have text xhr-error 'network error' 40 && log_lines | grep -aE 'console error: GET .*failed: the response is bigger than the limit \([0-9]+ KiB\)' >/dev/null && ok "an XMLHttpRequest over the limit fails, and the console says what the limit is" || bad "xhr: $(log_lines | grep -aE 'MEMTEST text|console error' | tail -2)"
fi

if want download; then
echo "-- a 100 MB download"
go "$H/dl?mb=100"
wait_log '^\[browser\] saved /Users/milk/Downloads/big\.bin \(104857600 bytes\)' 120 && ok "100 MB went to ~/Downloads (the limit used to be 64 MB, held in memory)" || bad "no download: $(log_lines | grep -aE 'saved|download' | tail -3)"
MB="$(browser_mb)"
[ -n "$MB" ] && [ "$MB" -lt 120 ] && ok "the Browser held ${MB} MB: the file never sat in memory" || bad "Browser holds ${MB:-?} MB after the download"
shell "files list /Users/milk/Downloads"
out "files list /Users/milk/Downloads" | grep -a "big.bin" >/dev/null && ok "the file is there" || bad "files list: $(out 'files list /Users/milk/Downloads')"
fi

if want eater; then
echo "-- a page that eats everything, with another tab ticking and the desktop in use"
go "$H/ticker.html?id=2"
have ticker2 begin ticking 20 || true
BEFORE_FREE="$(memfree)"
newtab "$H/eat.html"
have eat begin eating 20 && ok "the eater began" || bad "eat did not begin"
wait_log '^\[browser\] console error: This page ran out of memory: it needed more than the [0-9]+ MB the Browser could give it' 90 && ok "the eater was stopped, with a plain message on the console" || bad "the eater was never stopped: $(log_lines | grep -aE 'MEMTEST eat|memory' | tail -4)"
wait_log '^\[browser\] console warn: The page.s scripts were stopped \(out of memory\)' 5 && ok "(and the page's scripts are marked stopped)" || bad "no stop line"
shot memory-05-eater
q key ctrl-tab; sleep 2
T1="$(count_of ticker2 tick)"; sleep 3; T2="$(count_of ticker2 tick)"
[ "$T2" -gt "$T1" ] && ok "the other tab kept ticking ($T1 -> $T2)" || bad "the other tab stopped ($T1 -> $T2)"
shell "system memory"
out "system memory" | grep -aE '^[0-9]+ MB of memory' >/dev/null && ok "the desktop answered a command while all that went on" || bad "system memory: $(out 'system memory')"
sleep 2
AFTER_FREE="$(memfree)"
[ "$AFTER_FREE" -ge $((BEFORE_FREE - 30 * 1024)) ] && ok "the eater's memory came back (free: $((BEFORE_FREE / 1024)) MB before, $((AFTER_FREE / 1024)) MB after)" || bad "free memory $((BEFORE_FREE / 1024)) MB -> $((AFTER_FREE / 1024)) MB"
MIN="$(kb MinFree /proc/mempressure)"
[ -n "$MIN" ] && [ "$MIN" -gt 65536 ] && ok "free memory never fell under the kernel's low mark (lowest: $((MIN / 1024)) MB)" || bad "MinFree ${MIN:-?} kB"
AF="$(kb AllocFailures /proc/mempressure)"
[ "$AF" = 0 ] && ok "the kernel never failed an allocation" || bad "AllocFailures $AF"
log_lines | grep -a "panic" >/dev/null && bad "a panic" || ok "no panic"
fi

if want pressure; then
echo "-- five tabs of 70 MB: caches, garbage, then a background tab goes"
for i in 1 2 3 4 5; do
  [ "$i" = 1 ] && go "$H/page.html?mb=70&t=$i" || newtab "$H/page.html?mb=70&t=$i"
  sleep 3
done
wait_log '^\[browser\] memory is Tight: DropCaches' 40 && ok "short of room, the Browser dropped caches first" || bad "no DropCaches: $(log_lines | grep -aE 'memory is|discard' | tail -3)"
wait_log '^\[browser\] memory is Tight: Collect' 20 && ok "then collected garbage" || bad "no Collect"
wait_log '^\[browser\] discarded tab [0-9]' 30 && ok "then discarded a background tab" || bad "no discard: $(log_lines | grep -aE 'memory is|discard|ran out' | tail -4)"
shot memory-06-pressure
D="$(log_lines | sed -n 's/^\[browser\] discarded tab \([0-9]*\) .*/\1/p' | tail -1)"
if [ -n "$D" ]; then
  q key meta_l-"$D"
  wait_log "^\[browser\] restoring discarded tab $D from http://$(printf '%s' "$H" | sed 's/[.[\*^$]/\\&/g')/page\.html" 20 && ok "opening it fetched its page again" || bad "no restore of tab $D"
  have page held '70MB' 30 && ok "and the page ran again" || bad "restored page did not run"
fi
log_lines | grep -aE '^\[browser\] console error: This page ran out of memory' | wc -l | tr -d ' ' | { read c; [ "$c" -le 1 ] && ok "at most the eater's stop: nothing was stopped for memory in this test ($c)" || bad "$c pages ran out of memory"; }
for i in 1 2 3 4 5 6; do q key ctrl-w; sleep 0.4; done
fi

if want churn; then
echo "-- frames, workers and tabs made and dropped"
go "milk:newtab"
sleep 3
F0="$(memfree)"; B0="$(browser_mb)"
go "$H/churn.html?rounds=6"
have churn end 6 120 && ok "six rounds of six frames and four workers, each holding memory, ended" || bad "churn: $(log_lines | grep -aE 'MEMTEST churn|ran out|stopped' | tail -3)"
go "milk:newtab"
sleep 6
F1="$(memfree)"; B1="$(browser_mb)"
[ "$F1" -ge $((F0 - 20 * 1024)) ] && ok "free memory is back where it was (${F0} -> ${F1} kB; Browser ${B0} -> ${B1} MB)" || bad "free memory ${F0} -> ${F1} kB; Browser ${B0} -> ${B1} MB"
for r in 1 2 3 4 5 6 7 8; do newtab "$H/page.html?mb=15&t=$r"; sleep 1; done
for r in 1 2 3 4 5 6 7 8; do q key ctrl-w; sleep 0.5; done
sleep 5
F2="$(memfree)"
[ "$F2" -ge $((F0 - 20 * 1024)) ] && ok "and again after opening and closing eight tabs (${F2} kB)" || bad "free memory after the tabs: ${F2} kB (was ${F0})"
log_lines | grep -aE 'MEMTEST (worker|frame|page) (held|begin)' >/dev/null && ok "(the hogs did run)" || bad "no hog ran"
fi

# ---- boot 2: 192 MB ----------------------------------------------------------------------------------------
if want askstop; then
echo "== boot 2: 192 MB"
boot 192M
launch "http://$H/page.html?mb=36"
have page held '36MB' 40 && ok "a page holds 36 MB on a machine with little to spare" || bad "page did not hold: $(log_lines | grep -aE 'MEMTEST page|ran out' | tail -3)"
wait_log '^\[browser\] memory: asking to stop tab 1' 30 && ok "the Browser asked whether to stop it" || bad "no ask: $(log_lines | grep -aE 'memory' | tail -4)"
shot memory-07-ask
# "Stop" is the first of the bar's two buttons (the bar is over the foot of the page).
q move $((BX + 744)) $((BY + 558)) click
wait_log "^\[browser\] console warn: You stopped this page.s scripts because it used a lot of memory" 10 && ok "Stop ended that page's scripts, and the console says so" || bad "no stop line: $(log_lines | grep -aE 'console|memory' | tail -3)"
sleep 3
shell "system memory"
out "system memory" | grep -aE '^[0-9]+ MB of memory' >/dev/null && ok "and the machine is fine" || bad "system memory: $(out 'system memory')"
shot memory-08-stopped
echo "-- the same churn on a machine with little room: pages, frames and workers are stopped, nothing is left behind"
go "milk:newtab"
sleep 3
G0="$(memfree)"
go "$H/churn.html?rounds=4"
have churn end 4 150 && ok "four rounds of churn ran to their end under pressure" || bad "churn on 192 MB: $(log_lines | grep -aE 'MEMTEST churn|memory is' | tail -3)"
go "milk:newtab"
sleep 6
G1="$(memfree)"
[ "$G1" -ge $((G0 - 12 * 1024)) ] && ok "free memory is back where it was (${G0} -> ${G1} kB)" || bad "free memory ${G0} -> ${G1} kB"
AF="$(kb AllocFailures /proc/mempressure)"
[ "$AF" = 0 ] && ok "the kernel never failed an allocation" || bad "AllocFailures $AF"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || ok "the Browser stayed up"
fi

# ---- boot 3: 1 GB -----------------------------------------------------------------------------------------------
if want big; then
echo "== boot 3: 1 GB"
boot 1G
launch "http://$H/ticker.html"
go "$H/worker.html?mb=200"
have worker held '200MB' 60 && ok "a worker held 200 MB" || bad "worker 200 on 1 GB: $(log_lines | grep -aE 'MEMTEST worker|ran out' | tail -3)"
go "$H/frame.html?mb=200"
have frame held '200MB' 60 && ok "a frame held 200 MB" || bad "frame 200 on 1 GB"
go "$H/frames.html?mb=150&n=3"
sleep 12
[ "$(log_lines | grep -ac 'MEMTEST frame held 150MB')" -ge 3 ] && ok "three frames held 150 MB each (450 MB in one tab)" || bad "frames: $(log_lines | grep -aE 'MEMTEST frame (held|begin)|ran out' | tail -4)"
log_lines | grep -aE '^\[browser\] console error.*ran out of memory' >/dev/null && bad "something ran out of memory on a 1 GB machine" || ok "nothing ran out of memory"
fi

if [ "${MEMORY_TEST_FULL:-}" = 1 ] && want huge; then
echo "== boot 4: 2 GB"
boot 2G
launch "http://$H/ticker.html"
go "$H/worker.html?mb=400"
have worker held '400MB' 120 && ok "a worker held 400 MB" || bad "worker 400 on 2 GB: $(log_lines | grep -aE 'MEMTEST worker|ran out' | tail -3)"
fi

echo "-- after all that"
log_lines | grep -a "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || ok "the Browser stayed up"
# MEMORY_TEST_SHOTS=DIR keeps the screenshots (the work folder is removed when everything passes).
if [ -n "${MEMORY_TEST_SHOTS:-}" ]; then
  mkdir -p "$MEMORY_TEST_SHOTS" && cp "$T"/memory-*.png "$MEMORY_TEST_SHOTS"/ 2>/dev/null || true
fi
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "memory-test: PASS"
else
  echo "memory-test: FAIL (logs and screenshots in $T)"
  exit 1
fi
