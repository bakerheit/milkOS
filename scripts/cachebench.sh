#!/usr/bin/env bash
# Benchmarks on real virtio disks (private images). Page cache: a 4 MiB file on milkfs (/Users/milk, private
# image) and on FAT32 (scripts/mkfat.sh image), each read twice with the page cache off,
# then on (/sys/fs/pagecache/bench). milkfs create/read/delete of 300 x 1000 B,
# 100 x 20 KB and 4 x 1 MiB files (/sys/fs/milkfs/bench). Everything in a fresh /tmp directory; the file comes
# in through the Mac shared folder (read-only). Only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-cachebench.XXXXXX)"
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
scripts/mkfat.sh "$T/fat.img" 64 MILKFAT >/dev/null
export MILK_FS="$T/fs.img" MILK_DISK="$T/disk.img" MILK_FAT="$T/fat.img"
unset MILK_SHARE_RW
source scripts/disk.sh; milk_disk_args
source scripts/share.sh; milk_share_args
SHARE_DIR="$(cd "$(git rev-parse --git-common-dir)/.." && pwd)/share"
SUB="cachebench-$$"; mkdir -p "$SHARE_DIR/$SUB"
head -c 4194304 /dev/urandom > "$SHARE_DIR/$SUB/big.bin"
SOCK="$T/serial.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -global virtio-mmio.force-legacy=false "${DISK_ARGS[@]}" "${SHARE_ARGS[@]}" 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true; rm -rf "$SHARE_DIR/$SUB"' EXIT
P='msh:[^\n]*\$ '
B=/sys/fs/pagecache
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:cp /Volumes/Mac/$SUB/big.bin /Users/milk/big.bin" "wait:$P" \
  "send:cp /Volumes/Mac/$SUB/big.bin /Volumes/MILKFAT/big.bin" "wait:$P" \
  "send:echo 0 > $B/enabled" "wait:$P" \
  "send:echo /Users/milk/big.bin > $B/bench" "wait:$P" \
  "send:echo /Volumes/MILKFAT/big.bin > $B/bench" "wait:$P" \
  "send:echo 1 > $B/enabled" "wait:$P" \
  "send:echo /Users/milk/big.bin > $B/bench" "wait:$P" \
  "send:echo /Volumes/MILKFAT/big.bin > $B/bench" "wait:$P" \
  "send:cat $B/stats" "wait:$P" \
  "send:echo /Users/milk/b1 300 1000 > /sys/fs/milkfs/bench" "wait:$P" \
  "send:echo /Users/milk/b2 100 20000 > /sys/fs/milkfs/bench" "wait:$P" \
  "send:echo /Users/milk/b3 4 1048576 > /sys/fs/milkfs/bench" "wait:$P" \
  "send:echo CACHEBENCH""-DONE" "wait:CACHEBENCH-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 900); do [ -e "$T/done" ] && break; sleep 0.2; done
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
tr -d '\r' < "$LOG" | grep -a "pagecache bench:" | sed 's/^pagecache bench: /  /'
tr -d '\r' < "$LOG" | grep -a "pages (" | sed 's/^/  stats: /'
tr -d '\r' < "$LOG" | grep -a "^milkfs bench:" | sed 's/^milkfs bench: /  milkfs create: /'
[ -e "$T/done" ] || { echo "cachebench: session did not finish"; exit 1; }
