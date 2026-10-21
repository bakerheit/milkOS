#!/usr/bin/env bash
# Boot milkOS headless with ramfb, wait, and save the screen as a PNG (via the qemu monitor).
#   scripts/screenshot.sh [out.png] [seconds-to-wait] [extra qemu args...]
# Defaults: /tmp/milk-screen.png after 4 s (the boot splash lasts ~1 s). Prints the serial log
# path too. To see the splash itself, pass a short wait like 0.5.
# Only the qemu started here is killed (by PID); nothing else is touched.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
OUT="${1:-/tmp/milk-screen.png}"
WAIT="${2:-4}"
shift $(( $# > 2 ? 2 : $# )) || true
SOCK="/tmp/milk-display.$$.sock"
LOG="${OUT%.png}.serial.log"
cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
# Flat arm64 Image so qemu passes the device tree (see run.sh).
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
rm -f "$SOCK" "$OUT"
# Shared scratch disk: writes discarded, safe with other qemus running (scripts/disk.sh).
source scripts/disk.sh
milk_disk_args shared

qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M -rtc base=localtime \
  -kernel "$KERNEL" -serial "file:$LOG" \
  -device ramfb -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  "${DISK_ARGS[@]}" \
  -monitor "unix:$SOCK,server,nowait" "$@" &
QPID=$!
trap 'kill "$QPID" 2>/dev/null || true; rm -f "$SOCK"' EXIT

for _ in $(seq 50); do [ -S "$SOCK" ] && break; sleep 0.1; done
sleep "$WAIT"
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
