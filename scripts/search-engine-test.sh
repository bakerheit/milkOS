#!/usr/bin/env bash
# The Browser's search engine choice end to end on a private image, against local test servers
# only (scripts/browser_test_servers.py on the Mac's 127.0.0.1: a DNS server for a made-up zone
# and the test pages; every search engine's name is NXDOMAIN there, so a search ends in an
# error page, and the test reads the address the Browser went to from its log line). About
# two minutes. Checks:
#   * no settings file: bare words go to DuckDuckGo (Lite), the default
#   * `search = KEY` in ~/.config/milk/settings, written while the Browser runs: each of the six
#     engines gets its own address for the next search, with no restart; an unknown key or a
#     junk line goes back to the default; addresses and milk: pages ignore the choice
#   * unusual words (a space, +, &, #, quotes) are percent-encoded inside the query
#   * Settings > Browser: the choice is drawn, a click on Google writes `search = google` to
#     the settings file (and keeps the other lines), and the Browser's next search uses it
#   * the choice survives a reboot (the file is on the disk image) and the Browser reads it
#   * nothing panics or crashes
#   scripts/search-engine-test.sh [workdir]
# Only the qemu and python processes started here are killed (by PID). The work directory is
# removed when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-srtest.XXXXXX)}"
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
(cd "$T" && exec python3 -I "$REPO/scripts/browser_test_servers.py" "$T/ports" >"$T/servers.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$T/ports" ] && break; sleep 0.1; done
[ -s "$T/ports" ] || { echo "search-engine-test: FAIL: the test servers did not start"; exit 1; }
eval "$(tr ' ' '\n' <"$T/ports" | sed 's/^/P_/')"
P="$P_http"
H="www.milk.test:$P"
echo "servers: dns 127.0.0.1:$P_dns, http 127.0.0.1:$P (pid $SPID)"
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
BOOT=0
boot() { # one boot of the image; its own serial log, socket and QMP socket
  BOOT=$((BOOT + 1))
  SOCK="$T/s$BOOT.sock"; QMP="$T/q$BOOT.sock"; LOG="$T/serial$BOOT.log"
  : > "$LOG"
  # The serial log is qemu's own (logfile=): every line, whether or not a script is connected.
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
    -kernel "$ELF.bin" -chardev "socket,id=s0,path=$SOCK,server=on,wait=off,logfile=$LOG" -serial chardev:s0 \
    -device virtio-gpu-device -display none \
    -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -netdev user,id=n0 -device virtio-net-device,netdev=n0 \
    -fw_cfg "name=opt/milkos/net,string=internet=on dns=10.0.2.2:$P_dns" \
    -qmp "unix:$QMP,server,nowait" >"$T/qemu$BOOT.out" 2>&1 &
  QPID=$!
}
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
# The log with the prompt taken off lines the Browser printed right after one.
log_lines() { clean_log | sed -E 's/^msh:[^$]*\$ //'; }
# Output of the LAST time the command typed as $1 ran, up to the next prompt.
out() { clean_log | awk -v c="$1" 'on && /^msh:/ {on=0} !on && ($0 == c || ($0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c)) {on=1; buf=""; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'; }
# wait_log REGEX [SECS]: until the serial log has a line matching REGEX.
wait_log() { for _ in $(seq $(( ${2:-20} * 10 ))); do log_lines | grep -E -- "$1" >/dev/null && return 0; sleep 0.1; done; return 1; }
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
differ() { python3 scripts/shotwait.py --differ "$T/$1.png" "$T/$2.png"; }
esc_re() { printf '%s' "$1" | sed 's/[.[\*^$()+?{|]/\\&/g'; }
go() { q key meta_l-l type "$1" key ret; }
# How many times the Browser logged a load of an address matching URL-REGEX (the line is
# "[browser] ADDRESS: ..."; here an error, as the test DNS doesn't know the engines).
count() { log_lines | grep -c -E "^\[browser\] $1: " || true; }
# search WORDS URL-REGEX LABEL: type WORDS in the address bar; the Browser must go to the address.
search() {
  local before; before=$(count "$2")
  go "$1"
  for _ in $(seq 100); do [ "$(count "$2")" -gt "$before" ] && { ok "$3"; return 0; }; sleep 0.1; done
  bad "$3 (wanted $2; the Browser logged: $(log_lines | grep -a '^\[browser\] http' | tail -2 | tr '\n' ' '))"
  return 1
}
# setting LINES...: write the settings file from the console (while the Browser runs).
setting() {
  local steps=() op=">"
  for l in "$@"; do steps+=("send:print '$l' $op ~/.config/milk/settings" "wait:$PR"); op=">>"; done
  ser "${steps[@]}"
}

boot
wait_log "^wm: desktop up" 120 || bad "the desktop didn't come up"
ser "send:files create ~/.config/milk/" "wait:$PR"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser --log-input http://$H/ &" "wait:$PR"
if wait_log "^\[browser\] http://$(esc_re "$H")/: 200 OK" 30; then ok "the Browser is up"; else bad "no home page load in the log"; fi

echo "-- the default"
search "milk cows" 'https://lite\.duckduckgo\.com/lite/\?q=milk\+cows' "no settings file: DuckDuckGo (Lite)"

echo "-- each engine, picked while the Browser runs"
setting "theme = whole" "search = duckduckgo-html"
search "milk cows" 'https://html\.duckduckgo\.com/html/\?q=milk\+cows' "search = duckduckgo-html"
setting "search = brave"
search "milk cows" 'https://search\.brave\.com/search\?q=milk\+cows' "search = brave"
setting "search = startpage"
search "milk cows" 'https://www\.startpage\.com/sp/search\?query=milk\+cows' "search = startpage"
setting "search = bing"
search "milk cows" 'https://www\.bing\.com/search\?q=milk\+cows' "search = bing"
search "a+b & c#d \"q\"" 'https://www\.bing\.com/search\?q=a%2Bb\+%26\+c%23d\+%22q%22' "+, &, # and quotes are encoded inside the query"
setting "search = google"
search "milk cows" 'https://www\.google\.com/search\?q=milk\+cows&udm=14' "search = google"
setting "search = duckduckgo-lite"
search "milk cows" 'https://lite\.duckduckgo\.com/lite/\?q=milk\+cows' "search = duckduckgo-lite"
setting "search = duckduckgo"
search "two words" 'https://lite\.duckduckgo\.com/lite/\?q=two\+words' "search = duckduckgo means the Lite page"

echo "-- bad settings, addresses and pages"
setting "search = altavista"
search "milk cows" 'https://lite\.duckduckgo\.com/lite/\?q=milk\+cows' "an unknown engine: the default"
setting "search = bing" "search = nonsense"
search "milk cows" 'https://lite\.duckduckgo\.com/lite/\?q=milk\+cows' "the last line counts, and it is unknown: the default"
setting "search ==== brave ====" "====" "search"
search "milk cows" 'https://lite\.duckduckgo\.com/lite/\?q=milk\+cows' "junk lines: the default"
setting "search = brave"
before=$(log_lines | grep -c "^\[browser\] http://$(esc_re "$H")/styled: 200" || true)
go "$H/styled"
for _ in $(seq 100); do [ "$(log_lines | grep -c "^\[browser\] http://$(esc_re "$H")/styled: 200" || true)" -gt "$before" ] && break; sleep 0.1; done
[ "$(log_lines | grep -c "^\[browser\] http://$(esc_re "$H")/styled: 200" || true)" -gt "$before" ] && ok "an address opens as itself with an engine picked" || bad "an address was taken for a search"
b0=$(count 'https://search\.brave\.com/.*')
go "milk:bookmarks"; sleep 1
go "milk:newtab"; sleep 1
go "localhost:1/x"; sleep 1
[ "$(count 'https://search\.brave\.com/.*')" = "$b0" ] && ok "milk: pages and addresses never become searches" || bad "an address or milk: page was searched for"

echo "-- Settings > Browser"
# The dock's gear (Settings), its sidebar's Browser, then the Google card (1024x768 desktop).
q move 487 727 click sleep 1.5
q move 241 365 click sleep 0.8
shot 02-settings-brave
q move 679 415 click sleep 0.8
shot 03-settings-google 02-settings-brave
differ 02-settings-brave 03-settings-google && ok "Settings > Browser drew, and the click on Google changed it" || bad "the Settings page didn't change on the click"
ser "send:text show ~/.config/milk/settings" "wait:$PR"
S="$(out "text show ~/.config/milk/settings")"
echo "$S" | grep -a "^search = google$" >/dev/null && ok "the click wrote 'search = google' to ~/.config/milk/settings" || bad "the settings file: $S"
echo "$S" | grep -a "^theme = whole$" >/dev/null && echo "$S" | grep -a "^trash = " >/dev/null && ok "it kept the other settings" || bad "the other settings are gone: $S"
q move 199 97 click sleep 1 # the close light
search "milk cows" 'https://www\.google\.com/search\?q=milk\+cows&udm=14' "the Browser's next search went to Google, no restart"
q move 487 727 click sleep 1.5
q move 241 365 click sleep 0.8
q move 467 287 click sleep 0.8
ser "send:text show ~/.config/milk/settings" "wait:$PR"
out "text show ~/.config/milk/settings" | grep -a "^search = duckduckgo-lite$" >/dev/null && ok "clicking DuckDuckGo (Lite) writes it back" || bad "the settings file: $(out "text show ~/.config/milk/settings")"
q move 679 415 click sleep 0.8 # Google again, for the reboot
q move 199 97 click sleep 1

echo "-- after a reboot"
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
boot
wait_log "^wm: desktop up" 120 || bad "the desktop didn't come up after the reboot"
ser "send:text show ~/.config/milk/settings" "wait:$PR"
out "text show ~/.config/milk/settings" | grep -a "^search = google$" >/dev/null && ok "the choice is still in the settings file" || bad "after the reboot: $(out "text show ~/.config/milk/settings")"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser http://$H/ &" "wait:$PR"
wait_log "^\[browser\] http://$(esc_re "$H")/: 200 OK" 30 || bad "no home page load after the reboot"
search "milk cows" 'https://www\.google\.com/search\?q=milk\+cows&udm=14' "the Browser searches with Google after the reboot"
# Settings shows the choice after the reboot too.
q move 487 727 click sleep 1.5
q move 241 365 click sleep 0.8
shot 04-settings-after-reboot
q move 199 97 click sleep 1
q key meta_l-t sleep 1
shot 05-newtab-google 04-settings-after-reboot

clean_log | grep -i "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || true
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "search-engine-test: PASS"
else
  echo "search-engine-test: FAIL (logs and screenshots in $T)"
  exit 1
fi
