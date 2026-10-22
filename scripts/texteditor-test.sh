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
T="${1:-$(mktemp -d /tmp/milk-edtest.XXXXXX)}"
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
  -kernel "$ELF.bin" -serial "unix:$SOCK,server,nowait" -device virtio-gpu-device -display none \
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
differ() { ! cmp -s "$1" "$2"; }
# The window's position (outer rect) from /dev/wm: "window ... WxH+X+Y".
winpos() { out "cat /dev/wm" | grep -a "window .* $1" | tail -1 | sed -E 's/.*x[0-9]+\+([0-9]+)\+([0-9]+) .*/\1 \2/'; }

ser "wait:$P"
q move 785 615 click sleep 1
ser "send:/Apps/texteditor/texteditor /Users/milk/sample.rs &" "wait:$P"
sleep 3
ser "send:cat /dev/wm" "wait:$P"
set -- $(winpos "sample.rs")
[ -n "${1:-}" ] && ok "the editor window (sample.rs) is on the desktop" || { bad "no editor window"; }
X=$(( ${1:-0} + 1 )); Y=$(( ${2:-0} + 33 ))   # body origin
q screendump "$T/rust.png"
# Click at the end of the file's last line and type; the editor starts with the caret at the top.
q move $((X + 300)) $((Y + 100)) click sleep 0.5 type 'ZZ' sleep 0.5 screendump "$T/typed.png"
q key meta_l-s sleep 1
ser "send:cat /Users/milk/sample.rs" "wait:$P"
out "cat /Users/milk/sample.rs" | grep -q "ZZ" && ok "typing then Cmd-S wrote the change to the file" || bad "the saved file has no ZZ: $(out 'cat /Users/milk/sample.rs' | head -3)"
# Undo and save again: back to the original.
q key meta_l-z sleep 0.5 screendump "$T/undone.png" key meta_l-s sleep 1
ser "send:cat /Users/milk/sample.rs" "wait:$P"
out "cat /Users/milk/sample.rs" | grep -q "ZZ" && bad "undo then save still has ZZ" || ok "Cmd-Z then Cmd-S restored the file"
# Find: the bar opens, the match is highlighted.
q key meta_l-f sleep 0.5 type 'println' sleep 0.5 screendump "$T/find.png"
differ "$T/typed.png" "$T/find.png" && ok "the find bar changed the window" || bad "the find bar did nothing"
q key esc sleep 0.3
q key meta_l-q sleep 1 screendump "$T/quit.png"
ser "send:cat /dev/wm" "wait:$P"
out "cat /dev/wm" | grep -a "window .* sample.rs" >/dev/null && bad "Cmd-Q left the window open" || ok "Cmd-Q closed the clean editor"
# A long prose file: wrapped. A binary file: the sheet.
ser "send:/Apps/texteditor/texteditor /Users/milk/prose.txt &" "wait:$P"
sleep 3
q screendump "$T/prose.png"
ser "send:/Apps/texteditor/texteditor /Users/milk/bin.dat &" "wait:$P"
sleep 3
q screendump "$T/binary.png"
differ "$T/rust.png" "$T/prose.png" && differ "$T/prose.png" "$T/binary.png" && ok "prose and binary files drew differently" || bad "screens did not change"
ser "send:cat /dev/wm" "wait:$P"
grep -aqi "panic" "$LOG" && bad "a kernel panic in the serial log" || ok "no panic"
if [ "$FAIL" = 0 ]; then echo "texteditor-test: PASS (screenshots in $T)"; else echo "texteditor-test: FAIL (logs and screenshots in $T)"; exit 1; fi
