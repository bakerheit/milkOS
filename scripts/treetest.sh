#!/usr/bin/env bash
# milkfs B+tree shape under mass create/delete: boots a fresh private image, has the
# kernel create N empty files (default 50000) in one directory, delete 99 of every 100,
# then the rest (/System/Kernel/fs/milkfs/treetest), and prints the live tree's height, node count,
# fill and underfull nodes after each step. Then the host check. Fresh /tmp directory;
# only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
N="${1:-50000}"
T="$(mktemp -d /tmp/milk-treetest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/tree.img"
$M mkfs "$IMG" --size 256M >/dev/null
SOCK="$T/serial.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:print /Volumes/Milk/tt $N > /System/Kernel/fs/milkfs/treetest" "wait:$P" \
  "send:print TREE""-DONE" "wait:TREE-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 3000); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 2
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
tr -d '\r' < "$LOG" | grep -a "milkfs treetest:" | sed 's/^milkfs treetest: /  /'
$M check "$IMG" | tail -1 | sed 's/^/host /'
echo "(logs: $T)"
