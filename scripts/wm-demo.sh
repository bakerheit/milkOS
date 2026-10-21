#!/usr/bin/env bash
# Boot milkOS headless, run one scripted desktop demo (kernel/src/wm/script.rs), wait for it to
# finish and save the screen as a PNG.
#   scripts/wm-demo.sh DEMO [out.png] [extra qemu args...]
# DEMOs: drag focus minimise restore zoom resize close type notes about launcher launch today
#        spaces space2 control dim toast menu logo movespace files browse shell hover bench tour
#        stty paint paintclose desk desktoday dockpress select termselect minianim openanim
#        shutdown panic, or "none" for the plain desktop. Prefix slow- to run animations 20x
#        slower (e.g. SETTLE=0.3 scripts/wm-demo.sh slow-minianim) to catch one mid-flight.
# Default out: /tmp/milk-DEMO.png; the serial log goes next to it. Only the qemu started here is
# killed (by PID).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
DEMO="${1:?usage: scripts/wm-demo.sh DEMO [out.png]}"
OUT="${2:-/tmp/milk-$DEMO.png}"
shift $(( $# > 2 ? 2 : $# )) || true
SOCK="/tmp/milk-demo.$$.sock"
LOG="${OUT%.png}.serial.log"
cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
rm -f "$SOCK" "$OUT" "$LOG"
# Shared scratch disk: writes discarded, safe with other qemus running (scripts/disk.sh).
source scripts/disk.sh
milk_disk_args shared

qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M -rtc base=localtime \
  -kernel "$KERNEL" -serial "file:$LOG" \
  -device ramfb -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  -fw_cfg "name=opt/milkos/demo,string=$DEMO" "${DISK_ARGS[@]}" \
  -monitor "unix:$SOCK,server,nowait" "$@" &
QPID=$!
trap 'kill "$QPID" 2>/dev/null || true; rm -f "$SOCK"' EXIT

DONE="wm: desktop up"
[ "$DEMO" != none ] && DONE="\[demo\] ${DEMO#slow-} done"
for _ in $(seq 300); do
  grep -q "$DONE" "$LOG" 2>/dev/null && break
  sleep 0.1
done
grep -q "$DONE" "$LOG" || { echo "demo did not finish" >&2; tail -20 "$LOG" >&2; exit 1; }
sleep "${SETTLE:-0.4}"
python3 - "$SOCK" "$OUT" <<'PY'
import socket, sys, time
sock, out = sys.argv[1:3]
s = socket.socket(socket.AF_UNIX)
s.connect(sock)
s.settimeout(1)
def drain():
    data = b""
    try:
        while True:
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
    except socket.timeout:
        pass
    return data
drain()
s.sendall(f"screendump {out} -f png\n".encode())
time.sleep(0.5)
reply = drain().decode(errors="replace")
if "rror" in reply:
    sys.exit("screendump failed: " + reply)
PY
[ -s "$OUT" ] || { echo "no screenshot produced" >&2; exit 1; }
echo "screenshot: $OUT"
echo "serial log: $LOG"
