#!/usr/bin/env bash
# The Browser (/Apps/browser) end to end on a private image, against local test servers only
# (scripts/browser_test_servers.py on the Mac's 127.0.0.1: a DNS server for a made-up zone and
# the test pages; the guest is told internet=on with that DNS server, like web-test, so names
# work and nothing leaves the Mac). About two minutes. Checks, from the serial log, the test
# server's log and screenshots:
#   * the window system's client protocol: Home / End / Page Up / Page Down reach the app, and
#     clicks and the wheel carry the modifier keys (browser --log-input)
#   * a page with an image and links, a page with a stylesheet, a long page: End / Home / Page Down / Space scroll, a
#     #fragment, find in page, zoom, back
#   * forms: a GET form driven by the keyboard (text, checkbox, radio arrows, select,
#     textarea, submit) and a POST sign-in (Post/Redirect/Get) whose cookie comes back
#   * cookies: no third-party cookies (an image from another site gets none and sets none),
#     persistent ones saved in ~/Library/Browser/cookies, session ones not
#   * tabs (Cmd-T, Ctrl-Tab, Ctrl-W, Cmd-click), bookmarks (Cmd-D, the file), downloads to
#     ~/Downloads (a second one gets a new name), error pages, the sandbox it runs in
#   scripts/browser-test.sh [workdir]
# Only the qemu and python processes started here are killed (by PID). The work directory is
# removed when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-brtest.XXXXXX)}"
mkdir -p "$T"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
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
[ -s "$T/ports" ] || { echo "browser-test: FAIL: the test servers did not start"; exit 1; }
eval "$(tr ' ' '\n' <"$T/ports" | sed 's/^/P_/')"
P="$P_http"
H="www.milk.test:$P"
echo "servers: dns 127.0.0.1:$P_dns, http 127.0.0.1:$P (pid $SPID)"
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
# The serial log is qemu's own (logfile=): every line, whether or not a script is connected.
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
# The log with the prompt taken off lines the Browser printed right after one.
log_lines() { clean_log | sed -E 's/^msh:[^$]*\$ //'; }
# Output of the LAST time the command typed as $1 ran, up to the next prompt.
out() { clean_log | awk -v c="$1" 'on && /^msh:/ {on=0} !on && ($0 == c || ($0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c)) {on=1; buf=""; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'; }
# wait_log REGEX [SECS]: until the serial log has a line matching REGEX.
# (grep without -q: with pipefail, -q quitting early would fail the pipe.)
wait_log() { for _ in $(seq $(( ${2:-20} * 10 ))); do log_lines | grep -E -- "$1" >/dev/null && return 0; sleep 0.1; done; return 1; }
# wait_srv REGEX [SECS]: the same for the test server's log.
wait_srv() { for _ in $(seq $(( ${2:-20} * 10 ))); do grep -Eq -- "$1" "$T/servers.log" && return 0; sleep 0.1; done; return 1; }
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
same() { python3 scripts/shotwait.py --same "$T/$1.png" "$T/$2.png"; }
differ() { python3 scripts/shotwait.py --differ "$T/$1.png" "$T/$2.png"; }
# loaded URL-REGEX: the Browser logged a page load of it.
loaded() { wait_log "^\[browser\] $1" "${2:-20}"; }
go() { q key meta_l-l type "$1" key ret; }
esc_re() { printf '%s' "$1" | sed 's/[.[\*^$()+?{|]/\\&/g'; }

ser "wait@120:$PR"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser --log-input http://$H/ &" "wait:$PR"
if loaded "http://$(esc_re "$H")/: 200 OK, [0-9]+ bytes, 'Milk Browser test', 0 redirects" 30; then ok "the home page loaded (200, its title)"; else bad "no home page load in the log"; fi
wait_srv "^http GET /cow\.png host=www\.milk\.test" && ok "the page's image was fetched" || bad "the image wasn't fetched"
# The window's body origin, from /dev/wm (frame + 1, title bar 33).
ser "send:text show /dev/wm" "wait:$PR"
set -- $(out "text show /dev/wm" | grep -a "window .* Milk Browser test" | tail -1 | sed -E 's/.*x[0-9]+\+([0-9]+)\+([0-9]+) .*/\1 \2/')
BX=$(( ${1:-85} + 1 )); BY=$(( ${2:-58} + 33 ))
echo "  (window body at $BX,$BY)"
shot 01-home

go "$H/styled"
loaded "http://$(esc_re "$H")/styled: 200 OK, [0-9]+ bytes, 'Styled page'" && ok "a styled page loaded" || bad "no styled page"
wait_srv "^http GET /site\.css host=www\.milk\.test" && ok "its stylesheet was fetched" || bad "the stylesheet wasn't fetched"
shot 01b-styled 01-home
differ 01-home 01b-styled && ok "the styled page drew" || bad "the styled page looks like the last one"
q key meta_l-bracket_left
loaded "http://$(esc_re "$H")/: 200 OK, [0-9]+ bytes, 'Milk Browser test'" || true
sleep 1

echo "-- the client protocol (keys and modifiers)"
q key home key end key pgup key pgdn
for k in Home End PageUp PageDown; do
  wait_log "^\[browser\] input key $k mods=-$" 5 && ok "$k reaches the app" || bad "$k didn't reach the app"
done
q move $((BX + 600)) $((BY + 400)) hold meta_l click release meta_l
wait_log "^\[browser\] input down 600 400 button=1 mods=meta$" 5 && ok "a click carries Cmd" || bad "no Cmd on the click"
q hold shift scroll -1 release shift
wait_log "^\[browser\] input scroll 1 mods=shift$" 5 && ok "the wheel carries Shift" || bad "no Shift on the wheel"
q move $((BX + 600)) $((BY + 420)) click
wait_log "^\[browser\] input down 600 420 button=1 mods=-$" 5 && ok "a plain click carries no modifiers" || bad "a plain click had modifiers"

echo "-- a long page: scrolling, a fragment, find, zoom"
go "$H/long"
loaded "http://$(esc_re "$H")/long: 200 OK" || bad "/long didn't load"
shot 02-long 01-home
q key end; shot 03-end 02-long
differ 02-long 03-end && ok "End scrolled to the bottom" || bad "End did nothing"
q key home; shot 04-home 03-end
same 02-long 04-home && ok "Home went back to the top" || bad "Home didn't return to the top"
q key pgdn; shot 05-pgdn 04-home
differ 04-home 05-pgdn && ok "Page Down scrolled" || bad "Page Down did nothing"
q key spc; shot 06-space 05-pgdn
differ 05-pgdn 06-space && ok "Space scrolled" || bad "Space did nothing"
q key home sleep 0.5 key meta_l-f type "paragraph 250"; shot 07-find 04-home
differ 04-home 07-find && ok "find in page moved to the match" || bad "find in page did nothing"
q key esc key home sleep 0.5 key meta_l-equal key meta_l-equal; shot 08-zoom 04-home
differ 04-home 08-zoom && ok "Cmd-+ zoomed" || bad "Cmd-+ did nothing"
q key meta_l-0; shot 09-unzoom 08-zoom
same 04-home 09-unzoom && ok "Cmd-0 back to 100%" || bad "Cmd-0 didn't restore the page"
go "$H/long#p200"; shot 10-fragment 04-home
differ 04-home 10-fragment && ok "a #fragment scrolled to its paragraph" || bad "the #fragment did nothing"
q key meta_l-bracket_left; shot 11-back 10-fragment
same 04-home 11-back && ok "Back (Cmd-[) returned to the top of the page" || bad "Back didn't return"

echo "-- forms"
go "$H/form"
loaded "http://$(esc_re "$H")/form: 200 OK" || bad "/form didn't load"
shot 12-form
# Tab order: q, milk, cream, S, M, L, drink, note, Search, Reset, user, pw, Sign in.
q key tab type "milk cows" key tab key spc key tab key spc key tab key right key right key tab key down key tab
q type "two" key ret type "lines"
shot 13-form-filled 12-form
differ 12-form 13-form-filled && ok "typing filled the form" || bad "the form looks the same"
q key tab key spc
if wait_srv '^echo q=milk cows&cream=yes&size=l&drink=latte&note=two'; then ok "GET form sent: text, checkboxes, radio, select, textarea"; else bad "the GET form's query was wrong: $(grep '^echo' "$T/servers.log" | tail -1)"; fi
loaded "http://$(esc_re "$H")/echo\?q=milk\+cows&cream=yes&size=l&drink=latte&note=two%0D%0Alines: 200 OK" && ok "the echo page loaded (the textarea's newline sent as CR LF)" || bad "no echo page"
q key meta_l-bracket_left
loaded "http://$(esc_re "$H")/form: 200 OK" || true
sleep 1
q key tab key tab key tab key tab key tab key tab key tab key tab key tab key tab key tab type "milk" key tab type "s3cret pw" key ret
wait_srv '^post /login user=milk&pw=s3cret\+pw type=application/x-www-form-urlencoded$' && ok "POST form sent its body" || bad "no POST body: $(grep '^post' "$T/servers.log" | tail -1)"
wait_srv '^http GET /account host=www\.milk\.test cookie=session=abc123; theme=dark$' && ok "after the 303 the session cookie came back (and the Domain=tracker.test one was refused)" || bad "cookies after sign-in: $(grep 'GET /account' "$T/servers.log" | tail -1)"
loaded "http://$(esc_re "$H")/account: 200 OK, [0-9]+ bytes, 'Account', 1 redirects" && ok "Post/Redirect/Get landed on /account" || bad "no /account page"

echo "-- third-party cookies"
go "tracker.test:$P/track"
loaded "http://tracker\.test:$P/track: 200 OK" || bad "tracker.test didn't load"
go "$H/cookies"
loaded "http://$(esc_re "$H")/cookies: 200 OK" || bad "/cookies didn't load"
wait_srv "^http GET /pixel\.png host=tracker\.test cookie=None$" && ok "the third-party image got no cookie" || bad "the tracker's image got a cookie: $(grep 'pixel' "$T/servers.log" | tail -1)"
go "tracker.test:$P/track"
sleep 2
[ "$(grep -c '^http GET /track host=tracker.test cookie=track=1$' "$T/servers.log")" = 1 ] && ok "first party, tracker.test keeps its own cookie (and not the image's)" || bad "tracker.test's own cookie: $(grep 'GET /track' "$T/servers.log" | tail -1)"

echo "-- tabs, bookmarks, downloads, errors"
go "$H/"
loaded "http://$(esc_re "$H")/: 200 OK" || true
sleep 1
q key meta_l-d sleep 5.5
shot 14-home
# Cmd-click "forms" (page 606,86 = body 606,158): it opens in a new tab, this one stays.
before=$(log_lines | grep -c "^\[browser\] http://$(esc_re "$H")/form: 200" || true)
q move $((BX + 606)) $((BY + 158)) hold meta_l click release meta_l
sleep 2
after=$(log_lines | grep -c "^\[browser\] http://$(esc_re "$H")/form: 200" || true)
[ "$after" -gt "$before" ] && ok "Cmd-click loaded the link in a new tab" || bad "Cmd-click didn't load the link"
shot 15-two-tabs 14-home
differ 14-home 15-two-tabs && ok "a second tab is on the strip" || bad "no second tab"
q key ctrl-tab; shot 16-tab2 15-two-tabs
differ 15-two-tabs 16-tab2 && ok "Ctrl-Tab switched tabs" || bad "Ctrl-Tab didn't switch"
q key ctrl-w sleep 5; shot 17-closed 16-tab2
same 14-home 17-closed && ok "Ctrl-W closed the tab" || bad "Cmd-W didn't go back to one tab"
q key meta_l-t type "$H/missing" key ret
loaded "http://$(esc_re "$H")/missing: 404 Not Found, [0-9]+ bytes, 'Not found'" && ok "a 404 page from a new tab (Cmd-T)" || bad "no 404 page"
go "www.milk.test:1/"
loaded "http://www\.milk\.test:1/: connect 10\.0\.2\.2:1: connection refused" && ok "an error page for a refused connection" || bad "no connection error"
go "nowhere.milk.test/"
loaded "http://nowhere\.milk\.test/: nowhere\.milk\.test: there's no such host" && ok "an error page for an unknown host" || bad "no unknown-host error"
go "milk cows"
loaded "https://lite\.duckduckgo\.com/lite/\?q=milk\+cows: " && ok "bare words became a search (refused here by the test DNS)" || bad "bare words weren't searched for"
go "$H/file.zip"
wait_log "^\[browser\] saved /Users/milk/Downloads/file\.zip \(204800 bytes\) from http://$(esc_re "$H")/file\.zip$" 30 && ok "a download went to ~/Downloads" || bad "no download"
go "$H/file.zip"
wait_log "^\[browser\] saved /Users/milk/Downloads/file \(2\)\.zip \(204800 bytes\)" 30 && ok "a second download got a new name" || bad "the second download"
q key meta_l-q
sleep 1.5
ser "send:files list /Users/milk/Downloads" "wait:$PR" "send:text show /Users/milk/Library/Browser/bookmarks" "wait:$PR" "send:text show /Users/milk/Library/Browser/cookies" "wait:$PR"
out "files list /Users/milk/Downloads" | grep "file (2).zip" >/dev/null && ok "both files are in ~/Downloads" || bad "files list ~/Downloads: $(out "files list /Users/milk/Downloads")"
out "text show /Users/milk/Library/Browser/bookmarks" | grep "^http://$(esc_re "$H")/	Milk Browser test$" >/dev/null && ok "Cmd-D saved the bookmark" || bad "bookmarks file: $(out "text show /Users/milk/Library/Browser/bookmarks")"
C="$(out "text show /Users/milk/Library/Browser/cookies")"
echo "$C" | grep "	session	abc123$" >/dev/null && echo "$C" | grep "	track	1$" >/dev/null && ok "persistent cookies were saved" || bad "cookies file: $C"
echo "$C" | grep -E "	(theme|sess|pixel|evil)	" >/dev/null && bad "a session, third-party or refused cookie was saved" || ok "no session or third-party cookie was saved"

echo "-- the sandbox"
ser "send:/Apps/browser/browser http://$H/ &" "wait:$PR"
loaded "http://$(esc_re "$H")/: 200 OK" || true
BP=$(out "/Apps/browser/browser http://$H/ &" | sed -n 's/^\[[0-9]*\] \([0-9]*\).*/\1/p' | tail -1)
ser "send:text show /proc/${BP:-0}/status" "wait:$PR"
CAPS="$(out "text show /proc/${BP:-0}/status" | grep -a '^Caps:' || true)"
echo "  $CAPS"
echo "$CAPS" | grep "net=all" >/dev/null && echo "$CAPS" | grep "spawn=0" >/dev/null && echo "$CAPS" | grep "fs=/Users/milk/Library/Browser:rw" >/dev/null && ok "it runs sandboxed (network, its own files, no spawning)" || bad "the Browser's sandbox: ${CAPS:-no Caps line (pid ${BP:-?})}"
clean_log | grep -i "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || true
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "browser-test: PASS"
else
  echo "browser-test: FAIL (logs and screenshots in $T)"
  exit 1
fi

