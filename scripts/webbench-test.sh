#!/usr/bin/env bash
# The web engine in the guest: boots headless (offline; no disk needed) and runs
# /System/Developer/webbench on the serial console, which generates the ~1 MB
# Wikipedia-like page and renders it plus the fixture pages (user/webbench). Passes on
# "webbench: PASS" with no panic, and prints the guest's parse / layout times and heap use.
# Fresh /tmp directory, deleted when it passes; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d "${TMPDIR:-/tmp}/milk-webbench.XXXXXX")"
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
P='msh:[^\n]*\$ '
SOCK="$T/serial.sock"
LOG="$T/serial.log"
qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
  -global virtio-mmio.force-legacy=false 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:/System/Developer/webbench" "wait@240:webbench: (PASS|FAIL)" \
  "send:print WEBBENCH""-DONE" "wait:WEBBENCH-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 1500); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 0.5
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
python3 -I scripts/console_clean.py "$LOG"
FAIL=0
grep -a "webbench:" "$LOG" | sed 's/^/  /' || true
[ -e "$T/done" ] || { echo "webbench-test: FAIL (the session did not finish)"; FAIL=1; }
grep -aq "webbench: PASS" "$LOG" || { echo "webbench-test: FAIL (no PASS line)"; FAIL=1; }
if grep -aqi "panic" "$LOG"; then echo "webbench-test: FAIL (panic)"; grep -ai -m3 panic "$LOG"; FAIL=1; fi
if [ "$FAIL" = 0 ]; then
  rm -rf "$T"
  echo "webbench-test: PASS"
else
  echo "webbench-test: FAIL (log: $LOG)"
  exit 1
fi
