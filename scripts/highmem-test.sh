#!/usr/bin/env bash
# RAM above 4 GiB, end to end on qemu virt with 6 GiB (RAM is 0x4000_0000..0x1_c000_0000): the device
# tree's RAM range is taken whole, the page tables map it, the frame allocator hands out frames above
# 4 GiB, they read back what was written through the linear map, and the DMA allocator stays under
# the board's limit (kernel/src/memtest.rs, "high memory ok"). The same code runs on an 8 GB Raspberry
# Pi (docs/raspberry-pi.md: raspi4b itself has exactly 2 GiB, so the Pi image cannot be run with more).
# Offline, a private work folder, the qemu started here killed by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh
T="$(mktemp -d /tmp/milk-highmem.XXXXXX)"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$T/kernel.bin"
SOCK="$T/s.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 6G \
  -kernel "$T/kernel.bin" -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
  -global virtio-mmio.force-legacy=false >/dev/null 2>&1 &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
python3 scripts/serial_drive.py "$SOCK" "$LOG" 'wait@240:msh:[^\n]*\$ ' "send:system" 'wait:msh:[^\n]*\$ ' "done:$T/done" &
DPID=$!
for _ in $(seq 1200); do [ -e "$T/done" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
L="$(tr -d '\r' < "$LOG")"
has() { echo "$L" | grep -Eaq -- "$1"; }
[ -e "$T/done" ] && ok "booted to the shell with 6 GiB" || bad "no shell prompt (see $LOG)"
has '^mem: RAM 0x40000000\.\.0x1c0000000 \(6144 MiB, device tree\)' && ok "RAM 6144 MiB from the device tree, to 0x1c0000000" || bad "RAM line: $(echo "$L" | grep -a '^mem: RAM')"
has '^selftest: high memory ok \(RAM to 0x1c0000000; 3 frames at 0x1[0-9a-f]{8} ' && ok "frames above 4 GiB: written, read back, freed" || bad "no 'high memory ok' line: $(echo "$L" | grep -a 'high memory')"
has '^memory +[56]\.[0-9] GB' && ok "system sees it: $(echo "$L" | grep -a '^memory ')" || bad "system: $(echo "$L" | grep -a '^memory')"
has 'KERNEL PANIC|kernel panic' && bad "a kernel panic" || ok "no panic"
if [ "$FAIL" = 0 ]; then echo "highmem-test: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; else echo "highmem-test: FAIL (log $LOG)"; exit 1; fi
