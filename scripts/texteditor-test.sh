#!/usr/bin/env bash
# The Text Editor end to end on a private image: boot with a desktop, open a Rust file with the
# editor, type into it, save with Cmd-S and read the file back; find and undo; a binary file and
# a long prose file (screenshots in the work directory). About a minute.
#   scripts/texteditor-test.sh [workdir]
# Only the qemu started here is killed (by PID). Coordinates are for the 1024x768 desktop and the
# window's position read from /dev/wm.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="${1:-$(mktemp -d /tmp/milk-edtest.XXXXXX)}"
OWN_WORK=1; [ -n "${1:-}" ] && OWN_WORK=0 # remove it on a pass only if this script made it
mkdir -p "$T"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/a.img"
rm -f "$IMG"
$M mkfs "$IMG" --size 64M >/dev/null
$M mkdir "$IMG" /Users/milk >/dev/null
cat > "$T/sample.rs" <<'RS'
// A small file.
fn main() {
    let name = "milk";
    let n = 42; /* answer */
    println!("hello {name} {n}");
}
RS
python3 - "$T" <<'PY'
import sys
t = sys.argv[1]
open(f"{t}/prose.txt", "w").write(("The quick brown fox jumps over the lazy dog. " * 12) + "\n" + "Second paragraph.\n")
open(f"{t}/bin.dat", "wb").write(b"\x00\x01\x02binary\xff\xfe\x00data" * 10)
PY
for f in sample.rs prose.txt bin.dat; do $M put "$IMG" "/Users/milk/$f" "$T/$f" >/dev/null; done
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -serial "unix:$SOCK,server=on,wait=on" -device virtio-gpu-device -display none \
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
# Output of the LAST time the command typed as $1 ran, up to the next prompt.
out() { tr -d '\r' < "$LOG" | awk -v c="$1" 'on && /^msh:/ {on=0} !on && ($0 == c || ($0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c)) {on=1; buf=""; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'; }
# Screenshots wait for the screen (scripts/shotwait.py) instead of sleeping: shot NAME [REF] takes
# $T/NAME.png once the picture has settled and, with REF, differs from $T/REF.png; same / differ
# compare two of them ignoring the menu bar, the caret and the pointer. A shot that times out
# still leaves its picture, and the check that follows then fails.
shot() { python3 scripts/shotwait.py "$QMP" "$T/$1.png" ${2:+--differ-from "$T/$2.png"} || true; }
differ() { python3 scripts/shotwait.py --differ "$T/$1.png" "$T/$2.png"; }
# wait_window PATTERN: poll /dev/wm until a window matching PATTERN is listed (up to ~30 s).
wait_window() {
  for _ in $(seq 30); do
    ser "send:text show /dev/wm" "wait:$P"
    out "text show /dev/wm" | grep -aq "window .* $1" && return 0
    sleep 1
  done
  return 1
}
# wait_gone PATTERN: the same, until no window matches.
wait_gone() {
  for _ in $(seq 30); do
    ser "send:text show /dev/wm" "wait:$P"
    out "text show /dev/wm" | grep -aq "window .* $1" || return 0
    sleep 1
  done
  return 1
}
# file_has WORD / file_lacks WORD: poll the saved file (up to ~20 s) until it has / lacks WORD.
file_poll() { # has|lacks WORD
  for _ in $(seq 20); do
    ser "send:text show /Users/milk/sample.rs" "wait:$P"
    if out "text show /Users/milk/sample.rs" | grep -q "$2"; then [ "$1" = has ] && return 0; else [ "$1" = lacks ] && return 0; fi
    sleep 1
  done
  return 1
}
# The window's position (outer rect) from /dev/wm: "window ... WxH+X+Y".
winpos() { out "text show /dev/wm" | grep -a "window .* $1" | tail -1 | sed -E 's/.*x[0-9]+\+([0-9]+)\+([0-9]+) .*/\1 \2/'; }

ser "wait:$P"
q move 785 615 click sleep 1
ser "send:/Apps/texteditor/texteditor /Users/milk/sample.rs &" "wait:$P"
wait_window "sample.rs" && ok "the editor window (sample.rs) is on the desktop" || bad "no editor window"
set -- $(winpos "sample.rs")
X=$(( ${1:-0} + 1 )); Y=$(( ${2:-0} + 33 ))   # body origin
shot rust
# Click at the end of the file's last line and type; the editor starts with the caret at the top.
q move $((X + 300)) $((Y + 100)) click sleep 0.5 type 'ZZ'; shot typed
q key meta_l-s
file_poll has ZZ && ok "typing then Cmd-S wrote the change to the file" || bad "the saved file has no ZZ: $(out 'text show /Users/milk/sample.rs' | head -3)"
# Undo and save again: back to the original.
q key meta_l-z; shot undone
q key meta_l-s
file_poll lacks ZZ && ok "Cmd-Z then Cmd-S restored the file" || bad "undo then save still has ZZ"
# Find: the bar opens, the match is highlighted.
q key meta_l-f sleep 0.3 type 'println'; shot find undone
differ undone find && ok "the find bar changed the window" || bad "the find bar did nothing"
q key esc sleep 0.3
q key meta_l-q
wait_gone "sample.rs" && ok "Cmd-Q closed the clean editor" || bad "Cmd-Q left the window open"
shot quit
# A long prose file: wrapped. A binary file: the sheet.
ser "send:/Apps/texteditor/texteditor /Users/milk/prose.txt &" "wait:$P"
wait_window "prose.txt" || bad "no window for prose.txt"
shot prose quit
ser "send:/Apps/texteditor/texteditor /Users/milk/bin.dat &" "wait:$P"
wait_window "bin.dat" || bad "no window for bin.dat"
shot binary prose
differ rust prose && differ prose binary && ok "prose and binary files drew differently" || bad "screens did not change"
ser "send:text show /dev/wm" "wait:$P"
grep -aqi "panic" "$LOG" && bad "a kernel panic in the serial log" || ok "no panic"
if [ "$FAIL" = 0 ] && [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ]; then rm -rf "$T" || true; echo "texteditor-test: PASS"
elif [ "$FAIL" = 0 ]; then echo "texteditor-test: PASS (screenshots in $T)"; else echo "texteditor-test: FAIL (logs and screenshots in $T)"; exit 1; fi
