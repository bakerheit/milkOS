#!/usr/bin/env bash
# The JavaScript engine in the guest: boots headless (offline; no disk needed) and runs
# /System/Developer/jsbench on the serial console: Richards- and DeltaBlue-style workloads,
# a ~1 MB JSON round trip, string/regexp, array, async and float work (user/jsbench, sources
# in user/jsengine/bench). Passes on "jsbench: PASS" (every result matched) with no panic,
# and prints each workload's guest time and heap use. Then `jsbench big` loads and runs
# generated bundles of 3 MB (page limits: 64 MB heap) and 1 MB (frame limits: 16 MB heap) in
# the shapes real minified code has (plain functions, modules, closures, big literals) and
# passes on "jsbench-big: PASS" with a line each for time and peak memory.
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
  "send:/System/Developer/jsbench big modules 3 50 page" "wait@300:jsbench-big: (PASS|FAIL)" \
  "send:/System/Developer/jsbench big closures 3 50 page" "wait@300:jsbench-big: (PASS|FAIL)" \
  "send:/System/Developer/jsbench big funcs 1 50 frame" "wait@300:jsbench-big: (PASS|FAIL)" \
  "send:/System/Developer/jsbench big literals 1 1 frame" "wait@300:jsbench-big: (PASS|FAIL)" \
  "send:print JSBENCH""-DONE" "wait:JSBENCH-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 1500); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 0.5
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
python3 -I scripts/console_clean.py "$LOG"
FAIL=0
grep -a "jsbench:\|jsbench-big:" "$LOG" | sed 's/^/  /' || true
[ -e "$T/done" ] || { echo "jsbench-test: FAIL (the session did not finish)"; FAIL=1; }
grep -aq "jsbench: PASS" "$LOG" || { echo "jsbench-test: FAIL (no PASS line)"; FAIL=1; }
# Four `jsbench big` runs: each prints one PASS, and a case that was refused or stopped for
# the wrong reason would print FAIL.
[ "$(grep -ac "jsbench-big: PASS" "$LOG")" = 4 ] || { echo "jsbench-test: FAIL (the big-script runs did not all pass)"; FAIL=1; }
grep -aq "jsbench-big: FAIL" "$LOG" && { echo "jsbench-test: FAIL (a big-script run failed)"; FAIL=1; }
grep -a "jsbench-big:.*refused" "$LOG" | grep -av "huge" >/dev/null && { echo "jsbench-test: FAIL (a bundle was refused)"; FAIL=1; }
if grep -aqi "panic" "$LOG"; then echo "jsbench-test: FAIL (panic)"; grep -ai -m3 panic "$LOG"; FAIL=1; fi
if [ "$FAIL" = 0 ]; then
  rm -rf "$T"
  echo "jsbench-test: PASS"
else
  echo "jsbench-test: FAIL (log: $LOG)"
  exit 1
fi
