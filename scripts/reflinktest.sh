#!/usr/bin/env bash
# Reflink copies and dedupe from userspace on a private image: `cp --reflink` (FICLONE),
# `--reflink=auto` falling back to a copy on tmpfs, `--reflink` refusing there, writing
# into a copy, `dedupe -n` / `dedupe` of plain copies, `snap check`; then on the Mac:
# check clean (shared-block table, space map), v2, every file byte-exact.
# Fresh /tmp directory; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-reflink.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/r.img"
head -c 2000000 /dev/urandom > "$T/big.bin"
$M mkfs "$IMG" --size 64M >/dev/null
$M put "$IMG" /d/big.bin "$T/big.bin" >/dev/null
SOCK="$T/serial.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
D=/Volumes/Milk/d
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:cat /sys/fs/milkfs/info" "wait:$P" \
  "send:cp --reflink $D/big.bin $D/copy.bin" "wait:$P" \
  "send:cat /sys/fs/milkfs/info" "wait:$P" \
  "send:echo patched >> $D/copy.bin" "wait:$P" \
  "send:cp --reflink=auto $D/big.bin /tmp/auto.bin" "wait:$P" \
  "send:cp --reflink $D/big.bin /tmp/always.bin" "wait:$P" \
  "send:cp /tmp/auto.bin $D/dup1.bin" "wait:$P" \
  "send:cp /tmp/auto.bin $D/dup2.bin" "wait:$P" \
  "send:echo now > /sys/fs/milkfs/sync" "wait:$P" \
  "send:cat /sys/fs/milkfs/info" "wait:$P" \
  "send:dedupe -n $D" "wait:$P" \
  "send:dedupe $D" "wait:$P" \
  "send:dedupe $D" "wait:$P" \
  "send:cat /sys/fs/milkfs/info" "wait:$P" \
  "send:snap check" "wait:$P" \
  "send:echo now > /sys/fs/milkfs/sync" "wait:$P" \
  "send:echo REFLINK""-DONE" "wait:REFLINK-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 900); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 2
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
L="$(tr -d '\r' < "$LOG")"
# Free blocks as printed by the N-th `cat /sys/fs/milkfs/info` typed at the shell.
free() { echo "$L" | awk '/^msh:.*\$ cat \/sys\/fs\/milkfs\/info/{want=1} want && /^blocks:/{print; want=0}' |
  sed -n "${1}p" | sed 's/.*total, \([0-9]*\) free.*/\1/'; }
f1=$(free 1); f2=$(free 2); f3=$(free 3); f4=$(free 4)
[ -n "$f1" ] && [ $((f1 - f2)) -lt 10 ] && ok "cp --reflink of 2 MB used $((f1 - f2)) blocks (metadata only)" || bad "reflink space ($f1 -> $f2)"
echo "$L" | grep -aq "can't reflink" && ok "cp --reflink to tmpfs refuses: $(echo "$L" | grep -a -m1 "can't reflink")" || bad "--reflink to tmpfs"
echo "$L" | grep -a "could share\|now share\|already shared\|shared, \|would be shared" | sed 's/^/    /'
[ -n "$f3" ] && [ $((f4 - f3)) -gt 900 ] && ok "dedupe freed $((f4 - f3)) blocks (two 2 MB copies)" || bad "dedupe space ($f3 -> $f4)"
echo "$L" | grep -a -A2 "snap check$" | grep -aq "checksummed: clean" && ok "kernel check clean" || bad "kernel check"
$M check "$IMG" > "$T/check.txt" 2>&1 && ok "host check clean: $(grep -a 'shared by reflink' "$T/check.txt" | sed 's/^ *//')" || { bad "host check"; cat "$T/check.txt"; }
grep -q "milkfs v2" "$T/check.txt" && ok "image is v2 (shared blocks)" || bad "version"
{ cat "$T/big.bin"; printf 'patched\n'; } > "$T/copy.want"
cmp -s <($M cat "$IMG" /d/copy.bin) "$T/copy.want" && ok "written copy is byte-exact" || bad "copy.bin"
for f in big.bin dup1.bin dup2.bin; do
  cmp -s <($M cat "$IMG" /d/$f) "$T/big.bin" && ok "$f is byte-exact" || bad "$f"
done
echo
[ $FAIL = 0 ] && echo "reflinktest: PASS ($T)" || echo "reflinktest: FAIL ($T)"
exit $FAIL
