#!/usr/bin/env bash
# Flicker/tearing test for client windows: boot headless (virtio-gpu, private disk images), open
# Activity Monitor from Skim, then press and drag inside its window while sampling the screen
# many times over QMP. Each sample must show a complete frame: the toolbar band and the summary
# band at the bottom of the window must both be there. A sample with the window body cleared
# but not yet redrawn is a torn frame. Prints the counts; PASS only if none was torn.
#   scripts/flicker-test.sh [samples]    (default 150)
# Only the qemu started here is killed (by PID). Work files: a fresh mktemp dir.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
SAMPLES="${1:-150}"
WORK="$(mktemp -d /tmp/milk-flicker.XXXXXX)"
SOCK="$WORK/q.sock"
LOG="$WORK/serial.log"
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
export MILK_FS="$WORK/fs.img" MILK_DISK="$WORK/disk.img"
source scripts/disk.sh
milk_disk_args
source scripts/netdev.sh
MILK_HTTP_PORT= milk_net_args

qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
  -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$KERNEL" -serial "file:$LOG" \
  -device virtio-gpu-device -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  "${DISK_ARGS[@]}" "${NET_ARGS[@]}" \
  -qmp "unix:$SOCK,server,nowait" &
QPID=$!
trap 'kill "$QPID" 2>/dev/null || true' EXIT

for _ in $(seq 600); do
  grep -q 'wm: desktop up' "$LOG" 2>/dev/null && break
  sleep 0.1
done
grep -q 'wm: desktop up' "$LOG" || { echo "flicker-test: the desktop did not come up" >&2; tail -20 "$LOG" >&2; exit 1; }
sleep 2
RC=0
python3 -I scripts/flicker_probe.py "$SOCK" "$WORK" "$SAMPLES" || RC=$?
[ "$RC" = 0 ] && [ -z "${FLICKER_KEEP:-}" ] && rm -rf "$WORK" || echo "flicker-test: files kept in $WORK"
exit "$RC"
