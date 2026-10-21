#!/usr/bin/env bash
# Background scrub on a private image (fresh /tmp directory; only qemus started here are
# killed):
#   boot 1: a snapshot s1, a file deleted after it (so its blocks are only in s1); a
#           scrub of the clean volume finds nothing; the rate limit holds (8 MB/s);
#           fsstress with and without a (16 MB/s) scrub running alongside: the
#           scrub is low priority and rate limited, so normal work barely notices.
#   Mac:    flip one bit in a live file's data and one in the snapshot-only file's.
#   boot 2: the scrub reports both, with the paths (and the snapshot); reading the live
#           file gives EIO; the errors are in the audit log.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-scrubtest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/s.img"
source scripts/enc.sh
milk_enc_setup "$T"
$M mkfs "$IMG" --size 128M ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
head -c 300000 /dev/urandom > "$T/b.bin"; head -c 100000 /dev/urandom > "$T/old.bin"
head -c 40000000 /dev/urandom > "$T/bulk.bin"
$M put "$IMG" /docs/b.bin "$T/b.bin" >/dev/null
$M put "$IMG" /docs/old.bin "$T/old.bin" >/dev/null
$M put "$IMG" /bulk/bulk.bin "$T/bulk.bin" >/dev/null
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT
boot() { # name commands...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 ${ENC_QEMU[@]+"${ENC_QEMU[@]}"} 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  local args=("wait:$P")
  for c in "$@"; do args+=("send:$c" "wait:$P"); done
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "${args[@]}" "send:echo SCRUB""-DONE" "wait:SCRUB-DONE" "done:$T/$name.done" &
  local DPID=$!
  for _ in $(seq 3000); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1.5
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
}
boot b1 \
  "snap create s1" "rm /Volumes/Milk/docs/old.bin" "echo now > /sys/fs/milkfs/sync" \
  "snap scrub rate=0" "snap scrub" \
  "snap scrub rate=8" "snap scrub" \
  "fsstress /Volumes/Milk/st 4 10" \
  "snap scrub rate=16" "snap scrub start" "fsstress /Volumes/Milk/st2 4 10" "snap scrub status" "snap scrub rate=32"
L="$(tr -d '\r' < "$T/b1.log")"
echo "$L" | grep -a "^scrub: \[run [12]\] done" | sort -u | sed 's/^/    /'
echo "$L" | grep -a -m1 "^scrub: \[run 1\] done" | grep -q " 0 error" && ok "clean volume: no errors" || bad "clean scrub"
r8=$(echo "$L" | grep -a -m1 "^scrub: \[run 2\] done" | sed 's/.* ms, \([0-9.]*\) MB\/s.*/\1/')
python3 -c "import sys; sys.exit(0 if 6.0 <= float('${r8:-0}') <= 8.5 else 1)" && ok "rate limit 8 MB/s holds (${r8} MB/s)" || bad "rate limit (${r8:-?} MB/s)"
f1=$(echo "$L" | grep -a "^fsstress:.*/st:" | sed 's/.* in \([0-9]*\) ms.*/\1/')
f2=$(echo "$L" | grep -a "^fsstress:.*/st2:" | sed 's/.* in \([0-9]*\) ms.*/\1/')
echo "    fsstress alone: ${f1:-?} ms; with a 16 MB/s scrub running: ${f2:-?} ms"
echo "$L" | grep -a -A1 "snap scrub status" | grep -aq "running" && ok "the scrub was still running when fsstress finished (they overlapped)" || bad "no overlap: scrub finished first"
echo "$L" | grep -a "^fsstress:" | grep -q " 0 error" && ok "fsstress clean" || bad "fsstress"
$M corrupt "$IMG" flip /docs/b.bin | sed 's/^/    /'
$M corrupt "$IMG" flip /docs/old.bin --snapshot s1 | sed 's/^/    /'
boot b2 "snap scrub" "cat /Volumes/Milk/docs/b.bin > /dev/null" "audit --event milkfs.scrub"
L="$(tr -d '\r' < "$T/b2.log")"
echo "$L" | grep -a "^error: block" | sed 's/^/    /'
echo "$L" | grep -a "^error: block .*data of /Volumes/Milk/docs/b.bin at offset 0 (live" | grep -aq "snapshot s1" && ok "live file reported with its path (and the snapshots sharing the block)" || bad "live file not reported"
echo "$L" | grep -a "^error: block .*data of /Volumes/Milk/docs/old.bin at offset 0 (snapshot" | grep -aq "snapshot s1" && ok "snapshot-only file reported with its path and the snapshots that have it" || bad "snapshot file not reported"
[ "$(echo "$L" | grep -ac '^error: block')" = 2 ] && ok "exactly two errors" || bad "error count"
echo "$L" | grep -a -A1 "cat /Volumes/Milk/docs/b.bin" | grep -aq "checksum mismatch in block" && ok "reading the damaged file fails its checksum (EIO)" || bad "read of damaged file"
[ "$(echo "$L" | grep -a "milkfs.scrub.error" | grep -vc "audit --event")" -ge 2 ] && ok "both in the audit log" || bad "audit log"
echo
[ $FAIL = 0 ] && echo "scrubtest: PASS ($T)" || echo "scrubtest: FAIL ($T)"
exit $FAIL
