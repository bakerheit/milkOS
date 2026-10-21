#!/usr/bin/env bash
# milkfs format upgrade: a v0 image (one-block extents, made on the Mac) is mounted by
# this kernel, read, partly rewritten (so a file mixes v0 extents with a v1 run, and a
# small file becomes inline), then checked on the Mac: v1, clean, every byte right.
# Fresh /tmp directory; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-upgrade.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"

IMG="$T/v0.img"
head -c 300000 /dev/urandom > "$T/data.bin"
printf 'small file from the v0 days\n' > "$T/small.txt"
$M mkfs "$IMG" --size 64M --v0 >/dev/null
$M put "$IMG" /docs/data.bin "$T/data.bin" --v0 >/dev/null
$M put "$IMG" /docs/small.txt "$T/small.txt" --v0 >/dev/null
$M check "$IMG" | grep -q "milkfs v0" && ok "v0 image with v0 extents" || bad "v0 image"

SOCK="$T/serial.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
D=/Volumes/Milk/docs
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:cat $D/small.txt" "wait:$P" \
  "send:cp $D/data.bin $D/copy.bin" "wait:$P" \
  "send:echo appended by milkOS >> $D/data.bin" "wait:$P" \
  "send:echo new small > $D/small.txt" "wait:$P" \
  "send:echo now > /sys/fs/milkfs/sync" "wait:$P" \
  "send:echo UPGRADE""-DONE" "wait:UPGRADE-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 600); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 2   # let the flusher commit (1 s)
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
tr -d '\r' < "$LOG" | grep -a "format v0 volume" | sed 's/^/  /'
tr -d '\r' < "$LOG" | grep -aq "small file from the v0 days" && ok "kernel reads v0 extents" || bad "kernel read of v0 data"
$M check "$IMG" > "$T/check.txt" 2>&1 && ok "host check clean" || { bad "host check"; cat "$T/check.txt"; }
grep -q "milkfs v1" "$T/check.txt" && ok "image is v1 now" || bad "image version"
{ cat "$T/data.bin"; printf 'appended by milkOS\n'; } > "$T/data.want"
$M cat "$IMG" /docs/data.bin > "$T/data.got"
cmp -s "$T/data.got" "$T/data.want" && ok "appended file (v0 extents + a v1 run) is byte-exact" || bad "appended file"
$M cat "$IMG" /docs/copy.bin > "$T/copy.got"
cmp -s "$T/copy.got" "$T/data.bin" && ok "copy (all v1 runs) is byte-exact" || bad "copy"
[ "$($M cat "$IMG" /docs/small.txt)" = "new small" ] && ok "rewritten small file (inline now)" || bad "small file"
echo
[ $FAIL = 0 ] && echo "upgradetest: PASS ($T)" || echo "upgradetest: FAIL ($T)"
exit $FAIL
