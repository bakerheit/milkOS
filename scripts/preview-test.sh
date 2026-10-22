#!/usr/bin/env bash
# Preview end to end on a private image: boot with a desktop, open a PNG from a folder of
# pictures (PNG, PPM, BMP, a damaged PNG and a PDF), step through them, zoom, and look at the PDF
# and error cards (screenshots in the work directory; the checks compare screens and read
# /dev/wm). About a minute.
#   scripts/preview-test.sh [workdir]
# Only the qemu started here is killed (by PID). Coordinates are for the 1024x768 desktop and the
# window's position read from /dev/wm.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="${1:-$(mktemp -d /tmp/milk-pvtest.XXXXXX)}"
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
FIX=user/imgdec/tests/fixtures
python3 - "$T" <<'PY'
import struct, sys, zlib
t = sys.argv[1]
# A 400x300 PPM gradient and a 64x48 BMP; a damaged PNG; a one-page PDF.
w, h = 400, 300
open(f"{t}/b-gradient.ppm", "wb").write(b"P6\n%d %d\n255\n" % (w, h) + bytes(v for y in range(h) for x in range(w) for v in (x * 255 // w, y * 255 // h, 128)))
w, h = 64, 48
rows = b"".join(bytes(v for x in range(w) for v in (200, x * 4 % 256, y * 5 % 256)) + b"" for y in range(h - 1, -1, -1))
open(f"{t}/c-small.bmp", "wb").write(b"BM" + struct.pack("<IHHI", 54 + len(rows), 0, 0, 54) + struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(rows), 0, 0, 0, 0) + rows)
good = open("user/imgdec/tests/fixtures/checker.png", "rb").read()
open(f"{t}/d-damaged.png", "wb").write(good[:60] + bytes([good[60] ^ 0xFF]) + good[61:])
content = b"BT /F1 12 Tf 72 700 Td (Hello from a PDF) Tj 0 -14 Td (second line) Tj ET"
pdf = (b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n"
       b"3 0 obj << /Type /Page /Parent 2 0 R /Contents 4 0 R >> endobj\n4 0 obj << /Length " + str(len(content)).encode() + b" >>\nstream\n"
       + content + b"\nendstream\nendobj\ntrailer << /Root 1 0 R >>\n%%EOF\n")
open(f"{t}/e-doc.pdf", "wb").write(pdf)
PY
$M put "$IMG" /Users/milk/Pictures/a-checker.png "$FIX/checker.png" >/dev/null
$M put "$IMG" /Users/milk/Pictures/b-gradient.ppm "$T/b-gradient.ppm" >/dev/null
$M put "$IMG" /Users/milk/Pictures/c-small.bmp "$T/c-small.bmp" >/dev/null
$M put "$IMG" /Users/milk/Pictures/d-damaged.png "$T/d-damaged.png" >/dev/null
$M put "$IMG" /Users/milk/Pictures/e-doc.pdf "$T/e-doc.pdf" >/dev/null
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
# wait_window PATTERN: poll /dev/wm until a window matching PATTERN is listed (up to ~30 s).
wait_window() {
  for _ in $(seq 30); do
    ser "send:text show /dev/wm" "wait:$P"
    out "text show /dev/wm" | grep -aq "window .* $1" && return 0
    sleep 1
  done
  return 1
}
ser "send:/Apps/preview/preview /Users/milk/Pictures/a-checker.png &" "wait:$P"
wait_window "a-checker.png\|Preview" && ok "the Preview window is on the desktop" || bad "no Preview window"
shot 1-png
q key right; shot 2-ppm 1-png
q key right; shot 3-bmp 2-ppm
q key right; shot 4-damaged 3-bmp
q key right; shot 5-pdf 4-damaged
q key right; shot 6-wrapped 5-pdf
differ 1-png 2-ppm && differ 2-ppm 3-bmp && differ 3-bmp 4-damaged && differ 4-damaged 5-pdf && ok "each picture, the damaged file and the PDF drew a different screen" || bad "stepping through the folder did not change the screen"
same 1-png 6-wrapped && ok "the strip wraps from the last file back to the first" || bad "stepping past the last file did not return to the first"
# On the gradient (400 x 300): big enough to zoom past the window and pan.
q key right; shot 7-grad 6-wrapped
q key meta_l-i; shot 7b-info 7-grad
differ 7-grad 7b-info && ok "Cmd-I opened the info panel" || bad "Cmd-I did nothing"
q key esc
q key meta_l-equal sleep 0.3 key meta_l-equal sleep 0.3 key meta_l-equal sleep 0.3 key meta_l-equal; shot 8-200 7-grad
differ 7-grad 8-200 && ok "Cmd-+ stepped the zoom up to 200%" || bad "Cmd-+ did nothing"
# Rotate the picture (Cmd-R) and pan it with a drag.
q key meta_l-r; shot 9-rotated 8-200
differ 8-200 9-rotated && ok "Cmd-R rotated the picture" || bad "Cmd-R did nothing"
q move 600 400 button left down move 560 380 move 520 360 button left up; shot 10-panned 9-rotated
differ 9-rotated 10-panned && ok "dragging panned the picture" || bad "the drag did not move the picture"
# The PDF's text view: fit, three files on to the PDF, then the Show text button.
q key meta_l-0 sleep 0.3 key right sleep 0.3 key right sleep 0.3 key right; shot 5b-pdf 10-panned
ser "send:text show /dev/wm" "wait:$P"
set -- $(out "text show /dev/wm" | grep -a "window .* Preview" | tail -1 | sed -E 's/.*x[0-9]+\+([0-9]+)\+([0-9]+) .*/\1 \2/')
q move $(( ${1:-0} + 346 )) $(( ${2:-0} + 411 )) click; shot 11-text 5b-pdf
differ 5b-pdf 11-text && ok "Show text opened the text view" || bad "Show text did nothing"
grep -aqi "panic" "$LOG" && bad "a kernel panic in the serial log" || ok "no panic"
if [ "$FAIL" = 0 ]; then echo "preview-test: PASS (screenshots in $T)"; else echo "preview-test: FAIL (logs and screenshots in $T)"; exit 1; fi
