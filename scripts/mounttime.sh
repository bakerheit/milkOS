#!/usr/bin/env bash
# milkfs mount time on a volume with N files (default 20000), made on the Mac with
# `mkmilkfs.py populate`. Four boots of a private image in a fresh /tmp directory:
#   1. first mount of the host-made image (no space map: full scan), then one write + sync
#   2. mount after a kernel commit (space map, if the kernel writes one)
#   3. mount after `mkmilkfs.py put` wrote to the image on the Mac (host commits drop
#      the map: full scan again)
#   4. mount after the kernel committed again (space map again)
# Prints each boot's mount line and the host check. Only qemus started here are killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
N="${1:-20000}"
T="$(mktemp -d /tmp/milk-mounttime.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/big.img"
$M mkfs "$IMG" --size 256M >/dev/null
$M populate "$IMG" /data "$N" >/dev/null
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

boot() { # n: boot, write a file, sync, print the mount line
  local SOCK="$T/s$1.sock" LOG="$T/boot$1.log"
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
    "send:echo boot $1 > /Volumes/Milk/data/boot$1.txt" "wait:$P" \
    "send:echo now > /System/Kernel/fs/milkfs/sync" "wait:$P" \
    "send:echo MOUNT""-DONE" "wait:MOUNT-DONE" "done:$T/done$1" &
  local DPID=$!
  for _ in $(seq 900); do [ -e "$T/done$1" ] && break; sleep 0.2; done
  sleep 1.5
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
  echo "boot $1: $(tr -d '\r' < "$LOG" | grep -a -m1 'milkfs: vda mounted' | sed 's/^milkfs: //')"
  tr -d '\r' < "$LOG" | grep -a "milkfs: vda: space map" | sed 's/^/        /' || true
}

echo "== $N files"
boot 1
boot 2
echo "written on the Mac" > "$T/mac.txt"
$M put "$IMG" /data/mac.txt "$T/mac.txt" >/dev/null
boot 3
boot 4
$M check "$IMG" | tail -1 | sed 's/^/host /'
echo "(logs: $T)"
