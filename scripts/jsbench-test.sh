#!/usr/bin/env bash
# The JavaScript engine in the guest: boots headless (offline; no disk needed) and runs
# /System/Developer/jsbench on the serial console: Richards- and DeltaBlue-style workloads,
# a ~1 MB JSON round trip, string/regexp, array, async and float work (user/jsbench, sources
# in user/jsengine/bench). Passes on "jsbench: PASS" (every result matched) with no panic,
# and prints each workload's guest time and heap use.
# Fresh /tmp directory, deleted when it passes; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d "${TMPDIR:-/tmp}/milk-jsbench.XXXXXX")"
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
P='msh:[^\n]*\$ '
SOCK="$T/serial.sock"
LOG="$T/serial.log"
qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
  -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
  -global virtio-mmio.force-legacy=false 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:/System/Developer/jsbench" "wait@300:jsbench: (PASS|FAIL)" \
  "send:print JSBENCH""-DONE" "wait:JSBENCH-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 1500); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 0.5
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
python3 -I scripts/console_clean.py "$LOG"
FAIL=0
grep -a "jsbench:" "$LOG" | sed 's/^/  /' || true
[ -e "$T/done" ] || { echo "jsbench-test: FAIL (the session did not finish)"; FAIL=1; }
grep -aq "jsbench: PASS" "$LOG" || { echo "jsbench-test: FAIL (no PASS line)"; FAIL=1; }
if grep -aqi "panic" "$LOG"; then echo "jsbench-test: FAIL (panic)"; grep -ai -m3 panic "$LOG"; FAIL=1; fi
if [ "$FAIL" = 0 ]; then
  rm -rf "$T"
  echo "jsbench-test: PASS"
else
  echo "jsbench-test: FAIL (log: $LOG)"
  exit 1
fi
