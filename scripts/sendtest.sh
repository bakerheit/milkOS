#!/usr/bin/env bash
# snap send / receive, with the backups on the Mac. Private images in a fresh /tmp
# directory; the Mac share is attached read-write for this run only and our directory in
# it (share/.vfs-sendtest-*) is removed at the end. Only qemus started here are killed.
#   boot 1: source volume (with a file put there on the Mac) + an empty "Backup" volume.
#           Make files (a reflink copy, a symlink, a subdirectory), snapshot s1, change
#           things (append, rewrite, delete, rename, new file), snapshot s2; send s1 (full),
#           s2 incremental from s1, s2 full to stdout, all into /Volumes/Mac; receive s1
#           then s2 into /Volumes/Backup; receiving s2 again is refused.
#   Mac:    verify-stream all three; receive s1 + s2 into a fresh image and s2-full into
#           another; damaged and truncated copies are refused and change nothing; the trees
#           of s1 / s2 on the source, the Backup volume and both host images are identical;
#           mkmilkfs.py check clean everywhere.
#   boot 2: an empty volume: the incremental stream is refused (base missing), the damaged
#           and truncated ones too, and the volume stays empty.
#   boot 3: qemu is killed in the middle of receiving the full s2 (20 MB); the target is
#           then either complete or untouched, and (boot 4) receiving again works.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-sendtest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
source scripts/share.sh
MILK_SHARE_RW=1 milk_share_args
SHAREDIR="$(git rev-parse --path-format=absolute --git-common-dir)/../share"
SHAREDIR="$(cd "$SHAREDIR" && pwd)"
# Leftovers of runs that died without cleaning up (their process is gone).
for d in "$SHAREDIR"/.vfs-sendtest-*; do
  [ -d "$d" ] || continue
  kill -0 "${d##*-}" 2>/dev/null || rm -rf "$d"
done
SUB=".vfs-sendtest-$$"
mkdir -p "$SHAREDIR/$SUB"
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null; rm -rf "$SHAREDIR/$SUB"' EXIT

boot() { # name log commands...
  local name="$1" log="$2"; shift 2
  local SOCK="$T/$name.sock" drives=()
  for img in $DISKS; do
    drives+=(-drive "file=$img,if=none,format=raw,id=d${#drives[@]}" -device "virtio-blk-device,drive=d${#drives[@]}")
  done
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -global virtio-mmio.force-legacy=false "${drives[@]}" "${SHARE_ARGS[@]}" ${ENC_QEMU[@]+"${ENC_QEMU[@]}"} 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  local args=("wait:$P")
  for c in "$@"; do args+=("send:$c" "wait:$P"); done
  python3 scripts/serial_drive.py "$SOCK" "$log" "${args[@]}" "send:echo SEND""-DONE" "wait:SEND-DONE" "done:$T/$name.done" &
  local DPID=$!
  if [ -n "${KILL_ON:-}" ]; then
    # Kill qemu KILL_AFTER seconds after the command matching KILL_ON was typed.
    for _ in $(seq 1500); do grep -aq "$KILL_ON" "$log" 2>/dev/null && break; sleep 0.05; done
    sleep "${KILL_AFTER:-0.3}"
  else
    for _ in $(seq 1500); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
    sleep 1.5
  fi
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
}

source scripts/enc.sh
milk_enc_setup "$T"
SRC="$T/src.img"; BK="$T/backup.img"
$M mkfs "$SRC" --size 64M ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
$M mkfs "$BK" --size 64M --label Backup ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
head -c 1500000 /dev/urandom > "$T/big.bin"
$M put "$SRC" /data/big.bin "$T/big.bin" >/dev/null
head -c 20000000 /dev/urandom > "$T/bulk.bin"
$M put "$SRC" /bulk/bulk.bin "$T/bulk.bin" >/dev/null
D=/Volumes/Milk/data; S=/Volumes/Mac/$SUB
DISKS="$SRC $BK"
boot b1 "$T/boot1.log" \
  "cp --reflink $D/big.bin $D/big-copy.bin" \
  "echo hello > $D/small.txt" \
  "mkdir $D/sub" "echo nested > $D/sub/n.txt" "ln -s small.txt $D/link" \
  "snap create s1" \
  "echo more >> $D/small.txt" "rm $D/sub/n.txt" "mv $D/big-copy.bin $D/sub/moved.bin" \
  "echo appended >> $D/big.bin" "echo brand new > $D/new.txt" "echo rewritten > $D/link-target.txt" \
  "snap create s2" \
  "snap send s1 -o $S/s1.stream" \
  "snap send s2 --from s1 -o $S/s2.stream" \
  "snap send s2 > $S/s2-full.stream" \
  "snap receive $S/s1.stream --to /Volumes/Backup" \
  "snap receive $S/s2.stream --to /Volumes/Backup" \
  "snap receive $S/s2.stream --to /Volumes/Backup" \
  "ls /Volumes/Backup/data /Volumes/Backup/.snapshots" \
  "echo now > /sys/fs/milkfs/sync"
L="$(tr -d '\r' < "$T/boot1.log")"
echo "$L" | grep -a "^snap: sent\|^snap: received\|^snap: receive" | sed 's/^/    /'
[ "$(echo "$L" | grep -ac '^snap: received')" = 2 ] && ok "kernel: received s1 (full) and s2 (incremental) on /Volumes/Backup" || bad "kernel receives"
echo "$L" | grep -aq "receive.*that name is taken" && ok "receiving s2 again is refused (EEXIST)" || bad "second receive of s2"

for s in s1 s2 s2-full; do
  if $M verify-stream "$SHAREDIR/$SUB/$s.stream" > "$T/$s.verify"; then
    ok "verify-stream $s: $(sed -n 2p "$T/$s.verify" | sed 's/^ *//')"
  else bad "verify-stream $s: $(cat "$T/$s.verify")"; fi
done

H="$T/host.img"; H2="$T/host2.img"
$M mkfs "$H" --size 64M ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null; $M mkfs "$H2" --size 64M ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
$M receive "$H" "$SHAREDIR/$SUB/s2.stream" > "$T/h0.txt" 2>&1 && bad "host receive of an incremental without its base" || ok "host: incremental without its base refused: $(tail -1 "$T/h0.txt")"
$M receive "$H" "$SHAREDIR/$SUB/s1.stream" >/dev/null && $M receive "$H" "$SHAREDIR/$SUB/s2.stream" >/dev/null \
  && ok "host: received s1 + s2 into a fresh image" || bad "host receive"
$M receive "$H2" "$SHAREDIR/$SUB/s2-full.stream" >/dev/null && ok "host: received the full s2 into another" || bad "host receive full s2"

# Damaged / truncated copies.
cp "$SHAREDIR/$SUB/s1.stream" "$T/bad.stream"
python3 - "$T/bad.stream" <<'PY'
import sys; p = sys.argv[1]; b = bytearray(open(p, "rb").read()); b[len(b) // 2] ^= 0x40; open(p, "wb").write(b)
PY
head -c $(( $(wc -c < "$SHAREDIR/$SUB/s1.stream") / 2 )) "$SHAREDIR/$SUB/s1.stream" > "$T/short.stream"
for s in bad short; do
  $M verify-stream "$T/$s.stream" > "$T/$s.verify" && bad "verify-stream missed the $s stream" || ok "verify-stream: $(cat "$T/$s.verify" | sed 's/^.*BAD/BAD/')"
  E="$T/empty-$s.img"; $M mkfs "$E" --size 16M ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null; cp "$E" "$E.before"
  $M receive "$E" "$T/$s.stream" >/dev/null 2>&1 && bad "host received the $s stream" || ok "host refuses the $s stream"
  cmp -s "$E" "$E.before" && ok "  and the image is untouched" || bad "  $s stream changed the image"
done
cp "$T/bad.stream" "$T/short.stream" "$SHAREDIR/$SUB/"

for s in s1 s2; do
  $M tree "$SRC" --snapshot $s > "$T/src-$s.tree"
  for img in backup host; do
    $M tree "$T/$img.img" --snapshot $s > "$T/$img-$s.tree"
    diff -q "$T/src-$s.tree" "$T/$img-$s.tree" >/dev/null && ok "$s on the $img image is identical to the source ($(wc -l < "$T/src-$s.tree" | tr -d ' ') paths)" || { bad "$s differs on $img"; diff "$T/src-$s.tree" "$T/$img-$s.tree" | head; }
  done
done
$M tree "$H2" --snapshot s2 > "$T/host2-s2.tree"
diff -q "$T/src-s2.tree" "$T/host2-s2.tree" >/dev/null && ok "full s2 received on the Mac is identical too" || bad "full s2 differs"
for img in "$SRC" "$BK" "$H" "$H2"; do
  $M check "$img" > "$T/check.txt" 2>&1 && ok "check clean: $(basename "$img")$(grep -a 'shared by reflink' "$T/check.txt" | sed 's/^ */, /')" || { bad "check $(basename "$img")"; cat "$T/check.txt"; }
done

F="$T/fresh.img"; $M mkfs "$F" --size 32M --label Fresh ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
DISKS="$T/src.img $F"
boot b2 "$T/boot2.log" \
  "snap receive $S/s2.stream --to /Volumes/Fresh" \
  "snap receive $S/bad.stream --to /Volumes/Fresh" \
  "snap receive $S/short.stream --to /Volumes/Fresh" \
  "ls /Volumes/Fresh"
L="$(tr -d '\r' < "$T/boot2.log")"
echo "$L" | grep -a "^snap: receive" | sed 's/^/    /'
echo "$L" | grep -aq "receive.*base snapshot isn't on the target" && ok "kernel refuses an incremental whose base is missing" || bad "kernel: missing base"
[ "$(echo "$L" | grep -ac 'receive.*damaged stream')" = 2 ] && ok "kernel refuses the damaged and the truncated stream" || bad "kernel: bad streams"
$M check "$F" > "$T/fresh.txt" 2>&1 && grep -q " 0 snapshot" "$T/fresh.txt" && [ "$($M tree "$F" | wc -l | tr -d ' ')" = 0 ] \
  && ok "the target is still empty and clean" || { bad "target after refused receives"; cat "$T/fresh.txt"; }

C="$T/crash.img"; $M mkfs "$C" --size 64M --label Crash ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
DISKS="$T/src.img $C"
KILL_ON="snap receive $S/s2-full" KILL_AFTER=0.4 boot b3 "$T/boot3.log" \
  "snap receive $S/s2-full.stream --to /Volumes/Crash"
$M check "$C" > "$T/crash1.txt" 2>&1 && ok "after a kill mid-receive the target checks clean" || { bad "target after a kill mid-receive"; cat "$T/crash1.txt"; }
if [ "$($M tree "$C" | wc -l | tr -d ' ')" = 0 ]; then
  ok "  and is untouched (the receive hadn't finished; $(grep -a -c 'clone' "$T/crash1.txt" | tr -d ' ') hidden clone record left)"
elif $M tree "$C" --snapshot s2 2>/dev/null | diff -q - "$T/src-s2.tree" >/dev/null; then
  ok "  and is complete (the receive finished before the kill)"
else bad "  partly received"; fi
boot b4 "$T/boot4.log" "snap receive $S/s2-full.stream --to /Volumes/Crash" "echo now > /sys/fs/milkfs/sync"
$M tree "$C" --snapshot s2 > "$T/crash-s2.tree" 2>/dev/null
if diff -q "$T/crash-s2.tree" "$T/src-s2.tree" >/dev/null && $M check "$C" > "$T/crash2.txt" 2>&1 && ! grep -q "clone" "$T/crash2.txt"; then
  ok "receiving again works (leftover clone discarded): s2 identical, check clean"
else bad "receive after the kill"; tr -d '\r' < "$T/boot4.log" | grep -a "snap:" | head -3; cat "$T/crash2.txt"; fi
echo
[ $FAIL = 0 ] && echo "sendtest: PASS ($T)" || echo "sendtest: FAIL ($T)"
exit $FAIL
