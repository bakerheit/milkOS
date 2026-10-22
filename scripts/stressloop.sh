#!/usr/bin/env bash
# Parallel file-system stress on SMP, BOOTS times (default 8), each on fresh private
# images in a fresh /tmp directory: a milkfs volume and a FAT32 one. Per boot, with Time
# Machine snapshotting every second meanwhile:
#   fsstress /Volumes/Milk/stress 8 20      (8 workers on 4 CPUs; reflink copies too)
#   fsstress /Volumes/STRESS/stress 4 8     (FAT32)
#   disks dedupe /Volumes/Milk/stress, disks check, sync
# then on the Mac: mkmilkfs.py check (namespace, checksums, shared blocks, space map) and
# fsck_msdos -n (nothing to fix). Only qemus started here are killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
BOOTS="${1:-8}"
T="$(mktemp -d /tmp/milk-stress.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT
FAILS=0
for b in $(seq "$BOOTS"); do
  FS="$T/fs-$b.img"; FAT="$T/fat-$b.img"
  $M mkfs "$FS" --size 64M >/dev/null
  scripts/mkfat.sh "$FAT" 40 STRESS >/dev/null
  SOCK="$T/s$b.sock"; LOG="$T/boot$b.log"
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$FS,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -drive "file=$FAT,if=none,format=raw,id=hd1" -device virtio-blk-device,drive=hd1 2>/dev/null &
  QPID=$!
  P='msh:[^\n]*\$ '
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
    "send:print interval=1s > /System/Kernel/fs/milkfs/auto" "wait:$P" \
    "send:/System/Developer/fsstress /Volumes/Milk/stress 8 20" "wait:$P" \
    "send:/System/Developer/fsstress /Volumes/STRESS/stress 4 8" "wait:$P" \
    "send:disks dedupe /Volumes/Milk/stress" "wait:$P" \
    "send:disks check" "wait:$P" \
    "send:print now > /System/Kernel/fs/milkfs/sync" "wait:$P" \
    "send:print STRESS""-DONE" "wait:STRESS-DONE" "done:$T/done$b" &
  DPID=$!
  for _ in $(seq 3000); do [ -e "$T/done$b" ] && break; sleep 0.2; done
  sleep 2
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
  L="$(tr -d '\r' < "$LOG")"
  # (Kernel messages can land in the middle of an echoed command: match output lines.)
  stress="$(echo "$L" | grep -a "^fsstress:" | sed 's/.*ms, //' | tr '\n' ' ' || true)"
  kcheck="$(echo "$L" | grep -a "blocks checksummed:" | tail -1 | sed 's/.*checksummed: //' || true)"
  dd="$(echo "$L" | grep -a "now share" | sed 's/.*identical files, //; s/, .*//' || true)"
  if $M check "$FS" >"$T/host$b.txt" 2>&1; then host=clean; else host=PROBLEMS; fi
  shared="$(grep -a 'shared by reflink' "$T/host$b.txt" | sed 's/^ *//; s/ data block.*//' || true)"
  if fsck_msdos -n "$FAT" 2>&1 | grep -q "Fix?"; then fat=FIXES; else fat=clean; fi
  echo "boot $b: fsstress ${stress}| dedupe: ${dd:-?} | kernel check: ${kcheck:-?} | host check: $host (${shared:-0} shared blocks) | fsck_msdos: $fat"
  case "$stress" in *"0 error(s) 0 error(s)"*) ;; *) FAILS=$((FAILS + 1)) ;; esac
  [ "$host" = clean ] && [ "$fat" = clean ] && [ "$kcheck" = clean ] || FAILS=$((FAILS + 1))
done
echo
[ $FAILS = 0 ] && echo "stressloop: PASS, $BOOTS boots ($T)" || echo "stressloop: FAIL ($FAILS) ($T)"
exit $FAILS
