#!/usr/bin/env bash
# The system font in user space, end to end on a private image: the kernel puts its font
# files in /System/Fonts, /dev/wm says which family the desktop uses, an appkit app (Preview)
# draws a Greek, Polish, Cyrillic and symbol file name in it, and switching the font (`font
# KEY` to /dev/wm, what Settings does) changes the app on its next redraw and back again.
# Screenshots in the work directory, which is removed when everything passes (unless given).
#   scripts/fonts-test.sh [workdir]
# Only the qemu started here is killed (by PID).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="${1:-}"
OWN=0
[ -n "$T" ] || { T="$(mktemp -d /tmp/milk-fonttest.XXXXXX)"; OWN=1; }
mkdir -p "$T"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/a.img"
rm -f "$IMG"
$M mkfs "$IMG" --size 64M >/dev/null
$M mkdir "$IMG" /Users/milk >/dev/null
$M mkdir "$IMG" /Users/milk/Pictures >/dev/null
NAME="Ωμέγα Łódź Жук ★.png"
$M put "$IMG" "/Users/milk/Pictures/$NAME" user/imgdec/tests/fixtures/checker.png >/dev/null
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -serial "unix:$SOCK,server=on,wait=on" -fw_cfg name=opt/milkos/selftest,string=timer -device virtio-gpu-device -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
  -qmp "unix:$QMP,server,nowait" >/dev/null 2>&1 &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
n=0
ser() { # steps...: run console steps, wait for them to finish, disconnect
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/ser.$n" &
  local d=$!
  for _ in $(seq 400); do [ -e "$T/ser.$n" ] && break; sleep 0.2; done
  kill "$d" 2>/dev/null || true; wait "$d" 2>/dev/null || true
}
q() { python3 scripts/qmp.py "$QMP" "$@"; }
out() { tr -d '\r' < "$LOG" | awk -v c="$1" 'on && /^msh:/ {on=0} !on && ($0 == c || ($0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c)) {on=1; buf=""; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'; }
# Screenshots wait for the screen (scripts/shotwait.py) instead of sleeping: shot NAME [REF] takes
# $T/NAME.png once the picture has settled and, with REF, differs from $T/REF.png; same / differ
# compare two of them ignoring the menu bar, the caret and the pointer. A shot that times out
# still leaves its picture, and the check that follows then fails.
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
same() { python3 scripts/shotwait.py --same "$T/$1.png" "$T/$2.png"; }
differ() { python3 scripts/shotwait.py --differ "$T/$1.png" "$T/$2.png"; }

ser "wait:$P"
q move 785 615 click sleep 1
ser "send:files list /System/Fonts" "wait:$P"
n=$(out "files list /System/Fonts" | grep -o "[a-z]*\.font" | sort -u | wc -l | tr -d ' ')
[ "$n" = 6 ] && ok "the kernel put 6 font files in /System/Fonts" || bad "/System/Fonts has $n font files"
ser "send:text show /dev/wm" "wait:$P"
out "text show /dev/wm" | grep -aq "^font figtree" && ok "/dev/wm says the desktop font is figtree" || bad "no font line in /dev/wm"
wait_window() {
  for _ in $(seq 30); do
    ser "send:text show /dev/wm" "wait:$P"
    out "text show /dev/wm" | grep -aq "window .* $1" && return 0
    sleep 1
  done
  return 1
}
# A glob: the console line stays ASCII.
ser "send:/Apps/preview/preview /Users/milk/Pictures/*.png &" "wait:$P"
wait_window "Preview\|checker\|png" && ok "Preview is open" || bad "no Preview window"
shot 1-figtree
# The same window in Fira Sans after a redraw (any key redraws Preview), then back.
# The desktop logs the switch on the console: let that line pass before reading.
ser "send:print font fira > /dev/wm" "wait:wm: font Fira" "send:pause 1" "wait:$P"
ser "send:text show /dev/wm" "wait:$P"
out "text show /dev/wm" | grep -aq "^font fira" && ok "font fira took" || bad "/dev/wm did not switch to fira"
sleep 1.2
q key esc; shot 2-fira 1-figtree
differ 1-figtree 2-fira && ok "the app redrew in the new font" || bad "the app did not follow the font change"
ser "send:print font figtree > /dev/wm" "wait:wm: font Figtree" "send:pause 1" "wait:$P"
sleep 1.2
q key esc; shot 3-back 2-fira
same 1-figtree 3-back && ok "switching back gives the same picture" || bad "switching back did not restore the look"
grep -aq "fonts: 6 font files" "$LOG" && ok "boot log: fonts installed" || bad "no 'fonts:' line in the boot log"
grep -aq "system fonts: 5 families ok" "$LOG" && ok "boot self-test: the fonts are ok" || bad "font self-test line missing"
grep -aqi "panic" "$LOG" && bad "a kernel panic in the serial log" || ok "no panic"
if [ "$FAIL" = 0 ]; then
  if [ "$OWN" = 1 ]; then kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; rm -rf "$T"; echo "fonts-test: PASS"; else echo "fonts-test: PASS (screenshots in $T)"; fi
else
  echo "fonts-test: FAIL (logs and screenshots in $T)"; exit 1
fi
