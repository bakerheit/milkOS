#!/usr/bin/env bash
# Boot milkOS headless with ramfb + virtio keyboard/tablet, drive input over QMP, save the screen.
#   scripts/input-test.sh [out.png] [qmp.py commands...]
# Default out: /tmp/milk-input.png. Without commands it types a line, moves the pointer and clicks
# (see DEFAULT below). Waits for the boot self-test to finish before sending anything.
# Only the qemu started here is killed (by PID); nothing else is touched.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
OUT="${1:-/tmp/milk-input.png}"
shift $(( $# > 0 ? 1 : 0 ))
SOCK="/tmp/milk-qmp.$$.sock"
LOG="${OUT%.png}.serial.log"
cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
KERNEL="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
rm -f "$SOCK" "$OUT" "$LOG"

qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -serial "file:$LOG" \
  -device ramfb -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  -qmp "unix:$SOCK,server,nowait" &
QPID=$!
trap 'kill "$QPID" 2>/dev/null || true; rm -f "$SOCK"' EXIT

for _ in $(seq 100); do
  grep -q '\[selftest\] ok' "$LOG" 2>/dev/null && break
  sleep 0.1
done
grep -q '\[selftest\] ok' "$LOG" || { echo "boot did not reach selftest ok" >&2; cat "$LOG" >&2; exit 1; }

DEFAULT=(type 'Hello, milkOS! ~(1+2)=3' key ret type 'caps:' key caps_lock type 'abc'
  key caps_lock type ' ctrl-c:' key ctrl-c type ' bs:xy' key backspace key ret
  move 700 500 click left scroll 2 move 300 600)
[ $# -eq 0 ] && set -- "${DEFAULT[@]}"
python3 scripts/qmp.py "$SOCK" "$@" sleep 0.3 screendump "$OUT"

[ -s "$OUT" ] || { echo "no screenshot produced" >&2; exit 1; }
echo "screenshot: $OUT"
echo "serial log: $LOG"
