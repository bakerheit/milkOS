#!/usr/bin/env bash
# Reflink copies and dedupe from userspace on a private image: `cp --reflink` (FICLONE),
# `--reflink=auto` falling back to a copy on tmpfs, `--reflink` refusing there, writing
# into a copy, `dedupe -n` / `dedupe` of plain copies, `snap check`; then on the Mac:
# check clean (shared-block table, space map), v2, every file byte-exact. Built-in files
# (read in place from the kernel image, copied on write): copied to the disk with
# --reflink=auto (a plain copy, byte-exact, runs), --reflink refused, appended to, deleted,
# and the RAM they take (RootFiles) before and after.
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
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
D=/Volumes/Milk/d
# A second for milkd's service lines to finish (one landing inside the first command's echo
# hides it from free() below).
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:pause 1" "wait:$P" \
  "send:text show /System/Kernel/fs/milkfs/info" "wait:$P" \
  "send:files copy --reflink $D/big.bin $D/copy.bin" "wait:$P" \
  "send:text show /System/Kernel/fs/milkfs/info" "wait:$P" \
  "send:print patched >> $D/copy.bin" "wait:$P" \
  "send:files copy --reflink=auto $D/big.bin /tmp/auto.bin" "wait:$P" \
  "send:files copy --reflink $D/big.bin /tmp/always.bin" "wait:$P" \
  "send:files copy /tmp/auto.bin $D/dup1.bin" "wait:$P" \
  "send:files copy /tmp/auto.bin $D/dup2.bin" "wait:$P" \
  "send:print now > /System/Kernel/fs/milkfs/sync" "wait:$P" \
  "send:text show /System/Kernel/fs/milkfs/info" "wait:$P" \
  "send:dedupe -n $D" "wait:$P" \
  "send:dedupe $D" "wait:$P" \
  "send:dedupe $D" "wait:$P" \
  "send:text show /System/Kernel/fs/milkfs/info" "wait:$P" \
  "send:snap check" "wait:$P" \
  "send:text find RootFiles /proc/meminfo" "wait:$P" \
  "send:files copy --reflink=auto /Commands/cowsay $D/cowsay.auto" "wait:$P" \
  "send:files copy --reflink /Commands/cowsay $D/cowsay.always" "wait:$P" \
  "send:$D/cowsay.auto copied-cow" "wait:$P" \
  "send:print appended >> /Commands/cowsay" "wait:$P" \
  "send:cowsay written-cow" "wait:$P" \
  "send:text find RootFiles /proc/meminfo" "wait:$P" \
  "send:files copy /Commands/cowsay $D/cowsay.written" "wait:$P" \
  "send:files delete --now /Commands/cowsay" "wait:$P" \
  "send:files info /Commands/cowsay" "wait:$P" \
  "send:text find RootFiles /proc/meminfo" "wait:$P" \
  "send:print now > /System/Kernel/fs/milkfs/sync" "wait:$P" \
  "send:print REFLINK""-DONE" "wait:REFLINK-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 900); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 2
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
L="$(tr -d '\r' < "$LOG")"
# Free blocks as printed by the N-th `text show /System/Kernel/fs/milkfs/info` typed at the shell.
free() { echo "$L" | awk '/^msh:.*\$ text show \/System\/Kernel\/fs\/milkfs\/info/{want=1} want && /^blocks:/{print; want=0}' |
  sed -n "${1}p" | sed 's/.*total, \([0-9]*\) free.*/\1/'; }
f1=$(free 1); f2=$(free 2); f3=$(free 3); f4=$(free 4)
[ -n "$f1" ] && [ $((f1 - f2)) -lt 10 ] && ok "files copy --reflink of 2 MB used $((f1 - f2)) blocks (metadata only)" || bad "reflink space ($f1 -> $f2)"
echo "$L" | grep -aq "can't reflink" && ok "files copy --reflink to tmpfs refuses: $(echo "$L" | grep -a -m1 "can't reflink")" || bad "--reflink to tmpfs"
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
# Built-in files.
COW_SRC="target/user/aarch64-unknown-none-softfloat/release/cowsay"
cmp -s <($M cat "$IMG" /d/cowsay.auto) "$COW_SRC" && ok "built-in /Commands/cowsay copied to the disk (--reflink=auto) byte-exact" || bad "cowsay.auto"
$M cat "$IMG" /d/cowsay.always >/dev/null 2>&1 && bad "--reflink from a built-in (tmpfs) file made a copy" || ok "--reflink from a built-in file refuses"
echo "$L" | grep -aq "copied-cow" && ok "the copied built-in program runs" || bad "cowsay.auto did not run"
echo "$L" | grep -aq "written-cow" && ok "a built-in program still runs after a write to it (its own copy now)" || bad "cowsay after the write"
cmp -s <($M cat "$IMG" /d/cowsay.written) <(cat "$COW_SRC"; echo appended) && ok "the written built-in file reads as the original plus the append" || bad "cowsay.written"
# RootFiles as printed by the N-th `text find RootFiles /proc/meminfo` typed at the shell (it
# includes the 2 MB /tmp/auto.bin from above).
rf() { echo "$L" | awk '/^msh:.*\$ text find RootFiles/{want=1; next} want && /^RootFiles:/{print $2; want=0}' | sed -n "${1}p"; }
r1=$(rf 1); r2=$(rf 2); r3=$(rf 3)
csz=$(stat -f %z "$COW_SRC")
[ -n "$r1" ] && [ "$r1" -lt $((2000000 / 1024 + 1024)) ] && ok "built-in files take no RAM of their own: RootFiles ${r1} kB, of which /tmp/auto.bin is 1953" || bad "RootFiles before: $r1 kB"
[ -n "$r2" ] && [ $((r2 - r1)) -ge $((csz / 1024)) ] && [ $((r2 - r1)) -le $((csz / 1024 + 8)) ] && ok "writing to a built-in file copies it: RootFiles $r1 -> $r2 kB (the file is $((csz / 1024)) kB)" || bad "RootFiles after the write: $r1 -> $r2 (file $csz bytes)"
[ -n "$r3" ] && [ "$r3" -le "$r1" ] && ok "deleting it frees its pages: RootFiles $r3 kB" || bad "RootFiles after delete: $r3"
echo "$L" | grep -a -A1 "files info /Commands/cowsay" | grep -aqi "no such\|not found\|doesn't exist" && ok "the deleted built-in file is gone" || bad "cowsay still there: $(echo "$L" | grep -a -A1 "files info /Commands/cowsay")"
echo
[ $FAIL = 0 ] && echo "reflinktest: PASS ($T)" || echo "reflinktest: FAIL ($T)"
exit $FAIL
