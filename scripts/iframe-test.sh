#!/usr/bin/env bash
# `<iframe>` in the Browser, end to end on a private image, against local test servers only
# (scripts/iframe_test_servers.py on the Mac's 127.0.0.1: a DNS server for a made-up zone and one web
# server answering for www.milk.test, api.milk.test (the same site, another origin) and other.test
# (another site), serving the pages in tests/iframe; the guest is told internet=on with that DNS
# server, like fetch-test, so names work and nothing leaves the Mac). About four minutes. The pages
# report through console.log ("IFRTEST group name value"), which the Browser prints on the serial
# line (a line from a frame carries "[frame name]" after "log"), and the server logs every request
# it gets (with the Referer, the Cookie and the Sec-Fetch headers). Checks:
#   * same.html     a same-origin frame: contentDocument / contentWindow / parent / top / frames /
#                   frameElement both ways, calling functions across, changing the other's DOM
#   * cross.html    frames of another origin and another site: SecurityError for everything but
#                   postMessage, location, closed, frames, parent, top; and from inside
#   * nested.html   frames three deep reach each other; deep.html: the depth cap (levels 1 to 5
#                   are fetched, a sixth never is); many.html: the count cap
#   * srcdoc.html   srcdoc (wins over src), about:blank, data: documents (opaque origin)
#   * xfo.html      X-Frame-Options and CSP frame-ancestors: what is shown and what is blocked, the
#                   frame's `load` event still fires, the tab is never replaced by an error
#   * sandbox.html  the sandbox attribute: scripts, same-origin, forms, top navigation (blocked,
#                   then allowed), modals, popups, a CSP sandbox directive, a sandbox that nests
#   * msg.html      postMessage both ways: origins, targetOrigin, the structured clone
#   * form.html     a form submitting into a frame (GET and POST), the parent untouched
#   * target.html   links with target= (a name, a sibling by name, _parent)
#   * errors.html   404 and 500 pages, refused and unknown hosts, a download, text, a picture,
#                   a redirect, javascript: and file: addresses, a refused port: each inside the frame
#   * cookies.html  a frame on the same site gets the cookie, a third-party frame gets none and
#                   keeps none
#   * history.html  Back and forward step the frames, the parent is not fetched again
#   * scroll.html   the wheel scrolls the frame it is over, then the page; keys and typing go to
#                   the frame with the keyboard; find in page looks inside frames
#   * runaway.html  a frame that never stops: the tab keeps going, Esc stops that frame only
#   * the JavaScript switch (the toolbar's menu): off, no frame script runs
#   scripts/iframe-test.sh [workdir]
# Only the qemu and python processes started here are killed (by PID). The work directory is
# removed when everything passes (unless it was given).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
KEEP="${1:-}"
T="${1:-$(mktemp -d /tmp/milk-ifrtest.XXXXXX)}"
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
(cd "$T" && exec python3 -I "$REPO/scripts/iframe_test_servers.py" "$T/ports" "$REPO/tests/iframe" >"$T/server.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$T/ports" ] && break; sleep 0.1; done
[ -s "$T/ports" ] || { echo "iframe-test: FAIL: the test servers did not start"; exit 1; }
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
# A console line a page printed: "[browser] console LEVEL: IFRTEST ..." from the page, or
# "[browser] console LEVEL [frame NAME]: IFRTEST ..." from a frame.
CL='^\[browser\] console log( \[frame [^]]*\])?: IFRTEST'
report_of() { log_lines | { grep -aE "$CL $1 $2 " || true; } | tail -1 | sed -E "s/$CL $1 $2 //"; }
frame_of() { log_lines | { grep -aE "$CL $1 $2 " || true; } | tail -1 | sed -nE 's/^\[browser\] console log \[frame ([^]]*)\]:.*/\1/p'; }
# mt GROUP NAME WANT: the report equals WANT (an exact string); mtr: matches the regex.
mt() {
  local got; got="$(report_of "$1" "$2")"
  if [ "$got" = "$3" ]; then ok "$1 $2: $3"; else bad "$1 $2: got [$got] wanted [$3]"; fi
}
mtr() {
  local got; got="$(report_of "$1" "$2")"
  if printf '%s' "$got" | grep -aEq -- "$3"; then ok "$1 $2: $got"; else bad "$1 $2: got [$got] wanted /$3/"; fi
}
# nomt GROUP NAME: nobody reported it.
nomt() { if [ -z "$(report_of "$1" "$2")" ]; then ok "$1 $2: never reported"; else bad "$1 $2 was reported: $(report_of "$1" "$2")"; fi; }
# mtf GROUP NAME FRAME: the line came from that frame (its label), "" for the page itself.
mtf() {
  local got; got="$(frame_of "$1" "$2")"
  if [ "$got" = "$3" ]; then ok "$1 $2 came from [${3:-the page}]"; else bad "$1 $2 came from [$got] wanted [$3]"; fi
}
srvn() { grep -acE -- "$1" "$T/server.log" || true; }
# Where the window's body is on the screen: a right click (it does nothing on a page) is logged in
# body coordinates. Sets BX and BY: screen = BX + body x, BY + body y (the page starts 72 down).
calib() {
  q move 512 400 click right
  sleep 0.5
  set -- $(log_lines | sed -n 's/^\[browser\] input down \(-\{0,1\}[0-9]*\) \(-\{0,1\}[0-9]*\) button=2 .*/\1 \2/p' | tail -1)
  if [ -n "${1:-}" ]; then BX=$((512 - $1)); BY=$((400 - $2)); else BX=86; BY=91; bad "no right click logged, guessing the window's place"; fi
}
have_console() { wait_log "^\[browser\] console $1.*$2" "${3:-5}"; }
finish_of() { wait_log "$CL $1 end " "${2:-40}"; }

# IFRAME_TEST_ONLY="same cross" runs just those sections (while working on one).
want() { [ -z "${IFRAME_TEST_ONLY:-}" ] || case " $IFRAME_TEST_ONLY " in *" $1 "*) return 0;; *) return 1;; esac; }

ser "wait@120:$PR"
q move 785 615 click sleep 1
ser "send:/Apps/browser/browser --log-input http://$H/same.html &" "wait:$PR"

if want same; then
echo "-- a same-origin frame"
finish_of same 60 && ok "same.html ran to its end" || bad "same.html did not finish: $(log_lines | grep -a 'IFRTEST\|console' | tail -4)"
mt same child-title "The child"
mt same parent-reads-child "I am the child frame"
mt same child-got "hello from the parent"
mt same frames "1 true true 1"
mt same names "f||f"
mt same top-is-self "true true null"
mt same content-window-parent "true"
mt same child-location "/same-child.html"
mt same child-origin-same "true"
mt same child-sees-parent-note "the child changed this paragraph"
mt same child-click "clicked"
mt same-child identity "true true f f true"
mtf same-child identity "f"
mt same-child parent-title "Same-origin frame"
[ "$(srvn '^http GET www\.milk\.test /same-child\.html ')" = 1 ] && ok "the frame was fetched once" || bad "same-child.html was fetched $(srvn '^http GET www\.milk\.test /same-child\.html ') times"
grep -aE '^http GET www\.milk\.test /same-child\.html .*referer=http://'"$(esc_re "$H")"'/same\.html' "$T/server.log" >/dev/null && ok "the request carried the page as its Referer" || bad "no Referer on the frame's request: $(grep -a same-child "$T/server.log" | head -2)"
grep -aE '^http GET www\.milk\.test /same-child\.html .*dest=iframe site=same-origin' "$T/server.log" >/dev/null && ok "and Sec-Fetch-Dest: iframe, Sec-Fetch-Site: same-origin" || bad "no Sec-Fetch headers on the frame's request"
shot iframe-01-same
fi
if want cross; then
echo "-- frames of another origin and another site"
go "$H/cross.html"
finish_of cross 60 && ok "cross.html ran" || bad "cross.html did not finish: $(log_lines | grep -a 'IFRTEST cross\|console' | tail -4)"
for who in api other; do
  mt cross $who-contentDocument "null"
  mt cross $who-window-document "threw SecurityError"
  mt cross $who-location-href "threw SecurityError"
  mt cross $who-location-pathname "threw SecurityError"
  mt cross $who-body "threw SecurityError"
  mt cross $who-set-prop "threw SecurityError"
  mt cross $who-name "threw SecurityError"
  mt cross $who-safe "function false true true true function"
  mt cross $who-length "0"
  mt cross $who-self "true true"
done
mt cross-child parent-document "threw SecurityError"
mt cross-child parent-body "threw SecurityError"
mt cross-child parent-location "threw SecurityError"
mt cross-child top-title "threw SecurityError"
mt cross-child parent-length "2"
mt cross-child parent-frames-0 "object"
mtr cross-child origin "^http://(api\.milk\.test|other\.test):$P$"
wait_log "$CL cross messages " 15 || true
mtr cross messages "^hello from api\.milk\.test@http://api\.milk\.test:$P \| hello from other\.test@http://other\.test:$P$"
shot iframe-02-cross
fi
if want nested; then
echo "-- frames three deep"
go "$H/nested.html"
finish_of nested 60 && ok "nested.html ran" || bad "nested.html did not finish: $(log_lines | grep -a 'IFRTEST nested\|console' | tail -4)"
mt nested depths "1 2 3"
mt nested chain "true true true true"
mt nested reach-down "level C: the innermost frame"
mt nested title-chain "Level A > Level B > Level C"
mt nest-c depth "3 name=c"
mtf nest-c depth "a/b/c"
shot iframe-03-nested
fi
if want srcdoc; then
echo "-- srcdoc, about:blank and data: documents"
go "$H/srcdoc.html"
finish_of srcdoc 60 && ok "srcdoc.html ran" || bad "srcdoc.html did not finish: $(log_lines | grep -a 'IFRTEST srcdoc\|console' | tail -4)"
mt srcdoc content "Hello from srcdoc"
mt srcdoc attribute "kept"
mt srcdoc blank-url "about:blank"
mt srcdoc blank-same-origin "true"
mt srcdoc blank2-built "Built by the parent with innerHTML"
mt srcdoc data-access "null"
mt srcdoc both "srcdoc wins over src"
mt srcdoc-child ran "about:srcdoc true srcdoc and about:blank"
mtf srcdoc-child ran "doc"
mt data-child origin "null"
[ "$(srvn ' /nothing-here ')" = 0 ] && ok "a frame with srcdoc never fetched its src" || bad "the src of a srcdoc frame was fetched"
shot iframe-04-srcdoc
fi
if want xfo; then
echo "-- who may frame what"
go "$H/xfo.html"
finish_of xfo 60 && ok "xfo.html ran" || bad "xfo.html did not finish: $(log_lines | grep -a 'IFRTEST xfo\|console' | tail -4)"
mt xfo shown "csp-both,csp-self,csp-wild,csp-wild,csp-www,csp-www,xfo-allowall,xfo-obsolete,xfo-same"
mt xfo load-events "17 of 17"
have_console error "Refused to display .*xfo/deny.html.* 'X-Frame-Options' to 'DENY'" && ok "the console says why the DENY frame was refused" || bad "no console line for X-Frame-Options: DENY"
have_console error "frame-ancestors 'none'" && ok "and the frame-ancestors one" || bad "no console line for frame-ancestors 'none'"
have_console warn "Invalid 'X-Frame-Options' header encountered: 'allow-from" && ok "an obsolete ALLOW-FROM is noted and ignored" || bad "no note about ALLOW-FROM"
have_console warn "conflicting values" && ok "conflicting X-Frame-Options values are noted" || bad "no note about conflicting values"
[ "$(srvn '^http GET www\.milk\.test /xfo/deny\.html ')" = 1 ] && ok "a blocked frame was still requested (the answer is what blocks it)" || bad "xfo/deny.html requests: $(srvn '^http GET www\.milk\.test /xfo/deny\.html ')"
shot iframe-05-xfo
log_lines | grep -a "tab" >/dev/null || true
fi
if want sandbox; then
echo "-- the sandbox attribute"
go "$H/sandbox.html"
finish_of sandbox 60 && ok "sandbox.html ran" || bad "sandbox.html did not finish: $(log_lines | grep -a 'IFRTEST sandbox\|console' | tail -4)"
shot iframe-06-sandbox
mt sandbox no-script-no-message "true"
mtr sandbox s-scripts-origin "^null / event\.origin null$"
mt sandbox s-scripts-cookie "threw SecurityError"
mt sandbox s-scripts-storage "threw SecurityError"
mt sandbox s-scripts-confirm "false"
mt sandbox s-scripts-open "null"
mt sandbox s-scripts-same-origin-parent "threw SecurityError"
mtr sandbox s-both-origin "^http://$(esc_re "$H") / event\.origin http://$(esc_re "$H")$"
mt sandbox s-both-cookie "readable"
mt sandbox s-both-storage "1"
mt sandbox s-both-parent-doc "string"
mt sandbox s-all-contentDocument "null"
mt sandbox s-same-readable "null"
mt sandbox s-scripts-contentDocument "null"
mtr sandbox-child inner "^http://$(esc_re "$H") cookie=readable storage=1$"
mtf sandbox-child inner "s-nested/inner"
mtr csp-sandbox script-ran "^null$"
have_console error "'allow-popups'" && ok "the console says the popup was blocked" || bad "no console line for the blocked popup"
wait_log "$CL sandbox top-after-blocked " 20 && mt sandbox top-after-blocked "/sandbox.html" || bad "no top-after-blocked report"
have_console error "allow-top-navigation" && ok "a sandbox without allow-top-navigation could not take the tab away" || bad "no console line for the blocked top navigation"
# The sandbox with allow-top-navigation may.
wait_log "$CL landed path " 20 && mt landed path "/landed.html" || bad "the sandbox with allow-top-navigation did not navigate the tab"
# s-forms: its form went to the server from the sandbox with allow-forms, the others never sent one.
[ "$(srvn '^http POST www\.milk\.test /echo\?from=sandbox ')" -ge 1 ] && ok "the sandbox with allow-forms sent its form" || bad "allow-forms: the form was not sent"
[ "$(srvn '^http POST www\.milk\.test /echo\?from=sandbox ')" -le 1 ] && ok "and nobody else did" || bad "forms were sent by $(srvn '^http POST www\.milk\.test /echo\?from=sandbox ') frames"
fi
if want msg; then
echo "-- postMessage"
go "$H/msg.html"
finish_of msg 60 && ok "msg.html ran" || bad "msg.html did not finish: $(log_lines | grep -a 'IFRTEST msg\|console' | tail -4)"
mt msg function-throws "threw DataCloneError"
mt msg symbol-throws "threw DataCloneError"
mt msg dom-node-throws "threw DataCloneError"
mt msg bad-target-throws "threw SyntaxError"
mt msg one-arg "sent"
mt msg options-form "sent"
mt msg async "true"
for id in same api other; do
  mtr msg $id-reply-count "^[0-9]+$"
  mtr msg $id-event-origin "^http://"
  mt msg $id-source-is-frame "true"
  mt msg $id-event "message \"\" 0 true"
done
mtr msg same-reply "^n=42 arr=\[1,\"two\",\[3\]\] nested=\{\"a\":\{\"b\":true\}\} nul=null undef=true/undefined d=true:86400000 re=true:a\+b/gi m=true:1 st=true:2 buf=true:3:3 f=1\.5 \| origin=http://$(esc_re "$H") source-is-parent=true self-origin=http://$(esc_re "$H")$"
mtr msg api-reply "origin=http://$(esc_re "$H") source-is-parent=true self-origin=http://api\.milk\.test:$P"
mtr msg other-reply "origin=http://$(esc_re "$H") source-is-parent=true self-origin=http://other\.test:$P"
mt msg cycle-reply "cycle:true | origin=http://$H source-is-parent=true self-origin=http://$H"
mt msg wrong-target-delivered "0"
shot iframe-07-msg
fi
if want form; then
echo "-- a form that submits into a frame"
go "$H/form.html"
finish_of form 60 && ok "form.html ran" || bad "form.html did not finish: $(log_lines | grep -a 'IFRTEST form\|console' | tail -4)"
mt form echoes "2"
mtr form echo-1 "method=GET query=\[\('q', 'cheese'\), \('via', 'get'\)\] form=\[\] .*referer=http://$(esc_re "$H")/form\.html"
mtr form echo-2 "method=POST query=\[\('from', 'post'\)\] form=\[\('field', 'a b&c'\), \('via', 'post'\)\] type=application/x-www-form-urlencoded"
mt form frame-loads "3"
mtr form parent-untouched "^the parent page, untouched /form\.html$"
[ "$(srvn '^http GET www\.milk\.test /form\.html ')" = 1 ] && ok "the page itself was fetched once" || bad "form.html was fetched $(srvn '^http GET www\.milk\.test /form\.html ') times"
shot iframe-08-form
fi
if want target; then
echo "-- links with targets"
go "$H/target.html"
wait_log "$CL target top-path " 40 && ok "target.html ran" || bad "target.html did not finish: $(log_lines | grep -a 'IFRTEST target\|console' | tail -4)"
mt target named-path "/target-page.html?by=sibling"
mt target inner-path "/target-page.html?by=inner"
mt target top-path "/target.html"
have_console log "IFRTEST target-page landed \?by=name in frame named" && ok "the named link landed in the named frame" || bad "target=named did not land in its frame"
have_console log "IFRTEST target-page landed \?by=inner in frame inner" && ok "a link in a frame found a frame inside it by name" || bad "target=inner did not land"
wait_log "IFRTEST target-page landed \?by=parent in the top page" 20 && ok "target=_parent from a top-level frame replaced the page" || bad "target=_parent did not go to the top page"
shot iframe-09-target
fi
if want errors; then
echo "-- frames that go wrong"
go "$H/errors.html"
finish_of errors 80 && ok "errors.html ran" || bad "errors.html did not finish: $(log_lines | grep -a 'IFRTEST errors\|console' | tail -4)"
mt errors loaded "download,e404,e500,file,js,late,nxdomain,pic,port,redir,refused,text"
mt errors e404-text "Status 404"
mt errors text-body "just some plain text"
mt errors redir-path "/same-child.html"
mt errors top-path "/errors.html"
[ "$(srvn '^http GET www\.milk\.test /status/404 ')" = 1 ] && ok "a 404 frame shows the server's own page" || bad "status/404 requests: $(srvn '^http GET www\.milk\.test /status/404 ')"
have_console error "javascript:" && ok "the console refuses javascript:" || bad "no console line for the javascript: frame"
[ "$(srvn ':22/')" = 0 ] && ok "a refused port was never connected to" || bad "port 22 was requested"
shot iframe-10-errors
fi
if want cookies; then
echo "-- cookies"
go "$H/cookies.html"
finish_of cookies 60 && ok "cookies.html ran" || bad "cookies.html did not finish: $(log_lines | grep -a 'IFRTEST cookies\|console' | tail -4)"
mtr cookies same-origin-frame "sid=m1"
mtr cookies same-site-frame "sid=m1"
mt cookies third-party-frame "None"
mtr cookies document-cookie "sid=m1"
grep -aE '^http GET other\.test /cookie/who cookie=None' "$T/server.log" >/dev/null && ok "the third-party frame's request carried no cookie" || bad "the third-party frame sent cookies"
grep -aE '^http GET other\.test /cookie/set ' "$T/server.log" >/dev/null && ok "a third-party frame is fetched, though" || bad "the third-party /cookie/set was never requested"
shot iframe-11-cookies
fi
if want history; then
echo "-- Back and forward with frames"
go "$H/history.html"
wait_log "IFRTEST history clicked all" 40 && ok "history.html clicked its links" || bad "history.html did not finish: $(log_lines | grep -a 'IFRTEST history\|console' | tail -4)"
HP="$(srvn '^http GET www\.milk\.test /history\.html ')"
L1="$(srvn '^http GET www\.milk\.test /count/left ')"
q key meta_l-bracket_left
sleep 1.5
wait_srv "^http GET www\.milk\.test /count/right " 5
q key meta_l-bracket_left
sleep 1.5
q key meta_l-bracket_left
sleep 1.5
[ "$(srvn '^http GET www\.milk\.test /history\.html ')" = "$HP" ] && ok "going back in the frames never fetched the page again" || bad "history.html was fetched again by Back"
[ "$(srvn '^http GET www\.milk\.test /count/right ')" -ge 2 ] && ok "Back reloaded the frame it had been in" || bad "the right frame was not reloaded by Back"
q key meta_l-bracket_right
sleep 1.5
[ "$(srvn '^http GET www\.milk\.test /count/left2 ')" -ge 2 ] && ok "Forward went to the frame's next page" || bad "Forward did not reload left2"
shot iframe-12-history
fi
if want scroll; then
echo "-- scrolling, typing and find in frames"
go "$H/scroll.html"
wait_log "$CL scroll message" 5 || true
sleep 2
calib
# The tall frame is at (20, 40) in the page and 420 x 160: in the window body that is y = 72 + 40.
q move $((BX + 200)) $((BY + 72 + 110)) click
q scroll -3
wait_log "IFRTEST scroll message scrolled:" 10 && ok "the wheel scrolled the frame it was over" || bad "the wheel did not scroll the frame"
log_lines | grep -a "IFRTEST scroll parent-scrolled" >/dev/null && bad "the page scrolled too" || ok "and the page stayed where it was"
shot iframe-13-scrolled-frame
# Click the frame with the input, Tab to the field, type.
q move $((BX + 300)) $((BY + 72 + 280)) click
q key tab
q type "hello"
wait_log "IFRTEST input-child value=hello" 10 && ok "typing went to the field in the frame" || bad "the field in the frame got no input: $(log_lines | grep -a 'input-child' | tail -3)"
log_lines | grep -aE "^\[browser\] console log \[frame in\]: IFRTEST input-child value=hello$" >/dev/null && ok "and the line came from the frame called in" || bad "no frame label on the frame's console line"
shot iframe-14-typed
# The wheel in the page's own area scrolls the page.
q move $((BX + 700)) $((BY + 72 + 450)) click
q scroll -5
wait_log "IFRTEST scroll parent-scrolled" 10 && ok "the wheel over the page scrolled the page" || bad "the wheel over the page did nothing"
go "$H/find.html"
sleep 3
q key meta_l-f type quince
sleep 1.5
wait_log "find \"quince\": 4 matches" 10 && ok "find counted the match in the page and the three in the frames" || bad "find: $(log_lines | grep -a 'find ' | tail -2)"
q key ret
sleep 1
shot iframe-15-find
fi
if want runaway; then
echo "-- a frame that never stops"
go "$H/runaway.html"
wait_log "$CL runaway-child begin" 40 && ok "runaway.html started" || bad "runaway.html did not start"
wait_log "this page is slow" 40 && ok "the slow bar came up" || bad "no slow bar for the runaway frame"
q key esc
wait_log "stopped the page's scripts" 10 && ok "Esc stopped it" || bad "Esc did not stop the script"
sleep 1.5
T1="$(report_of runaway tick)"
sleep 2
T2="$(report_of runaway tick)"
[ -n "$T1" ] && [ "$T2" != "$T1" ] && ok "the page's own timer kept ticking ($T1 -> $T2)" || bad "the page stopped ticking ($T1 -> $T2)"
shot iframe-16-runaway
fi
if want deep; then
echo "-- frames deeper than the cap, and more than the cap"
go "$H/deep.html"
wait_log "IFRTEST deep level-5 shown" 40 && ok "deep.html reached level 5" || bad "deep.html did not reach level 5"
sleep 2
for l in 1 2 3 4 5; do
  [ "$(srvn "^http GET www\.milk\.test /deep\?n=$l ")" = 1 ] && ok "level $l was fetched once" || bad "level $l fetched $(srvn "^http GET www\.milk\.test /deep\?n=$l ") times"
done
[ "$(srvn '^http GET www\.milk\.test /deep\?n=6 ')" = 0 ] && ok "level 6 was never fetched" || bad "level 6 was fetched"
have_console error "nested at most 5 deep" && ok "the console says the frame was too deep" || have_console warn "nested more than 5 deep" && ok "(the engine says so)" || bad "no console line about the depth"
shot iframe-17-deep
go "$H/many.html"
finish_of many 60 && ok "many.html ran" || bad "many.html did not finish"
mt many made "45"
mtr many frames-length "^32$"
shot iframe-18-many
fi
if want js; then
echo "-- the JavaScript switch"
go "$H/same.html"
sleep 3
calib
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript off" 5 && ok "the menu turned JavaScript off" || bad "no javascript off"
BEFORE="$(log_lines | grep -ac 'IFRTEST same-child identity')"
go "$H/same.html"
sleep 4
AFTER="$(log_lines | grep -ac 'IFRTEST same-child identity')"
[ "$BEFORE" = "$AFTER" ] && ok "with JavaScript off no frame script ran" || bad "frame scripts ran with JavaScript off ($BEFORE -> $AFTER)"
[ "$(srvn '^http GET www\.milk\.test /same-child\.html ')" -ge 3 ] && ok "(the frame was still fetched and shown)" || bad "the frame was not fetched with JavaScript off"
shot iframe-19-js-off
q move $((BX + 874)) $((BY + 52)) click sleep 0.5 move $((BX + 700)) $((BY + 422)) click
wait_log "^\[browser\] javascript on" 5 && ok "and on again" || bad "no javascript on"
fi
if want after; then
echo "-- after all that"
log_lines | grep -a "panic" >/dev/null && bad "a panic in the serial log" || ok "no panic"
clean_log | grep "browser.* quit unexpectedly" >/dev/null && bad "the Browser crashed" || ok "the Browser stayed up"
go "$H/same.html"
wait_log "$CL same end " 40 && ok "and still loads pages with frames" || bad "the Browser didn't recover"
fi
# IFRAME_TEST_SHOTS=DIR keeps the screenshots (the work folder is removed when everything passes).
if [ -n "${IFRAME_TEST_SHOTS:-}" ]; then
  mkdir -p "$IFRAME_TEST_SHOTS" && cp "$T"/iframe-*.png "$IFRAME_TEST_SHOTS"/ 2>/dev/null || true
fi
if [ "$FAIL" = 0 ]; then
  cleanup
  trap - EXIT
  [ -z "$KEEP" ] && rm -rf "$T"
  echo "iframe-test: PASS"
else
  echo "iframe-test: FAIL (logs and screenshots in $T)"
  exit 1
fi
