#!/usr/bin/env bash
# disks send / receive, with the backups on the Mac. Private images in a fresh /tmp
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
# A crash in the middle of a receive (the target is then complete or untouched, and receiving
# again works) is a milkfs host test, at many cut points: milkfs/src/tests/send.rs.
# Encrypted streams: s1 is also sent with --encrypt (verified, received on the Mac, a wrong
# passphrase refused in the kernel). With MILK_ENCRYPT=1 every stream is encrypted (the
# default from an encrypted volume; a --plaintext one is checked to be plain), so the
# whole test runs on encrypted streams. The stream passphrase is a fake test value.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-sendtest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
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
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
    -global virtio-mmio.force-legacy=false "${drives[@]}" "${SHARE_ARGS[@]}" ${ENC_QEMU[@]+"${ENC_QEMU[@]}"} 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  local args=("wait:$P" "send:pause 1" "wait:$P") # the first command only lets the boot finish
  for c in "$@"; do args+=("send:$c" "wait:$P"); done
  python3 scripts/serial_drive.py "$SOCK" "$log" "${args[@]}" "send:print SEND""-DONE" "wait:SEND-DONE" "done:$T/$name.done" &
  local DPID=$!
  for _ in $(seq 1500); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1.5
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
D=/Volumes/Milk/data; S=/Volumes/Mac/$SUB
export MILKFS_STREAM_PASSPHRASE="fake-stream-passphrase"
echo "$MILKFS_STREAM_PASSPHRASE" > "$SHAREDIR/$SUB/sp"; echo "wrong-fake-passphrase" > "$SHAREDIR/$SUB/wrong"
SPF=""; EXTRA_SEND="disks info"
if [ "${MILK_ENCRYPT:-0}" = 1 ]; then
  SPF=" --passphrase-file $S/sp"; EXTRA_SEND="disks send s1 --plaintext -o $S/s1-plain.stream"
fi
DISKS="$SRC $BK"
boot b1 "$T/boot1.log" \
  "files copy --reflink $D/big.bin $D/big-copy.bin" \
  "print hello > $D/small.txt" \
  "files create $D/sub/" "print nested > $D/sub/n.txt" "files link small.txt $D/link" \
  "disks snapshot s1" \
  "print more >> $D/small.txt" "files delete --now $D/sub/n.txt" "files move $D/big-copy.bin $D/sub/moved.bin" \
  "print appended >> $D/big.bin" "print brand new > $D/new.txt" "print rewritten > $D/link-target.txt" \
  "disks snapshot s2" \
  "disks send s1 -o $S/s1.stream$SPF" \
  "disks send s2 --from s1 -o $S/s2.stream$SPF" \
  "disks send s2$SPF > $S/s2-full.stream" \
  "disks send s1 --encrypt --passphrase-file $S/sp -o $S/s1-sealed.stream" \
  "$EXTRA_SEND" \
  "disks receive $S/s1.stream --to /Volumes/Backup$SPF" \
  "disks receive $S/s2.stream --to /Volumes/Backup$SPF" \
  "disks receive $S/s2.stream --to /Volumes/Backup$SPF" \
  "files list /Volumes/Backup/data /Volumes/Backup/.snapshots" \
  "print now > /System/Kernel/fs/milkfs/sync"
L="$(tr -d '\r' < "$T/boot1.log")"
echo "$L" | grep -a "^disks: sent\|^disks: received\|^disks receive" | sed 's/^/    /'
[ "$(echo "$L" | grep -ac '^disks: received')" = 2 ] && ok "kernel: received s1 (full) and s2 (incremental) on /Volumes/Backup" || bad "kernel receives"
echo "$L" | grep -aq "receive.*already has a snapshot of that name (EEXIST" && ok "receiving s2 again is refused (EEXIST)" || bad "second receive of s2"

magic() { head -c 8 "$SHAREDIR/$SUB/$1.stream"; }
inner() { grep -aq MILKSTRM "$SHAREDIR/$SUB/$1.stream"; }
[ "$(magic s1-sealed)" = MILKSTRX ] && ! inner s1-sealed && ok "send --encrypt: an encrypted stream (envelope first, no plaintext header inside)" || bad "s1-sealed not encrypted"
if [ "${MILK_ENCRYPT:-0}" = 1 ]; then
  [ "$(magic s1)" = MILKSTRX ] && [ "$(magic s2)" = MILKSTRX ] && [ "$(magic s2-full)" = MILKSTRX ] && ! inner s2-full \
    && ok "from an encrypted volume every stream is encrypted by default" || bad "default streams from an encrypted volume"
  inner s1-plain && [ "$(magic s1-plain)" != MILKSTRX ] && ok "--plaintext opts out" || bad "--plaintext"
else
  inner s1 && ok "from a plain volume streams stay plaintext by default" || bad "plain default"
fi
echo "$L" | grep -aq "^disks: sent s1, encrypted to $S/s1-sealed.stream" && ok "disks reports the stream encrypted" || bad "send report"
MILKFS_STREAM_PASSPHRASE=wrong-fake-passphrase $M verify-stream "$SHAREDIR/$SUB/s1-sealed.stream" > "$T/wrong.verify" 2>&1 \
  && bad "Mac: a wrong stream passphrase opened it" || ok "Mac: $(sed 's/^.*BAD/BAD/' "$T/wrong.verify")"

for s in s1 s2 s2-full s1-sealed; do
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
H3="$T/host3.img"; $M mkfs "$H3" --size 64M ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
$M receive "$H3" "$SHAREDIR/$SUB/s1-sealed.stream" >/dev/null && ok "host: received the --encrypt stream" || bad "host receive of the encrypted stream"

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
$M tree "$H3" --snapshot s1 | diff -q - "$T/src-s1.tree" >/dev/null && ok "s1 through an encrypted stream on the Mac is identical" || bad "s1 via encrypted stream differs"
$M tree "$H2" --snapshot s2 > "$T/host2-s2.tree"
diff -q "$T/src-s2.tree" "$T/host2-s2.tree" >/dev/null && ok "full s2 received on the Mac is identical too" || bad "full s2 differs"
for img in "$SRC" "$BK" "$H" "$H2"; do
  $M check "$img" > "$T/check.txt" 2>&1 && ok "check clean: $(basename "$img")$(grep -a 'shared by reflink' "$T/check.txt" | sed 's/^ */, /')" || { bad "check $(basename "$img")"; cat "$T/check.txt"; }
done

F="$T/fresh.img"; $M mkfs "$F" --size 32M --label Fresh ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null
DISKS="$T/src.img $F"
boot b2 "$T/boot2.log" \
  "disks receive $S/s2.stream --to /Volumes/Fresh$SPF" \
  "disks receive $S/bad.stream --to /Volumes/Fresh$SPF" \
  "disks receive $S/short.stream --to /Volumes/Fresh$SPF" \
  "disks receive $S/s1-sealed.stream --to /Volumes/Fresh --passphrase-file $S/wrong" \
  "files list /Volumes/Fresh"
L="$(tr -d '\r' < "$T/boot2.log")"
echo "$L" | grep -a "^disks receive" | sed 's/^/    /'
echo "$L" | grep -aq "receive.*base snapshot isn't on the target" && ok "kernel refuses an incremental whose base is missing" || bad "kernel: missing base"
[ "$(echo "$L" | grep -ac 'receive.*damaged or cut short (nothing was changed) (EIO')" = 2 ] && ok "kernel refuses the damaged and the truncated stream" || bad "kernel: bad streams"
echo "$L" | grep -aq "disks receive: wrong passphrase for the stream (EACCES" && ok "kernel refuses a wrong stream passphrase" || bad "kernel: wrong stream passphrase"
$M check "$F" > "$T/fresh.txt" 2>&1 && grep -q " 0 snapshot" "$T/fresh.txt" && [ "$($M tree "$F" | wc -l | tr -d ' ')" = 0 ] \
  && ok "the target is still empty and clean" || { bad "target after refused receives"; cat "$T/fresh.txt"; }

echo
if [ $FAIL = 0 ]; then echo "sendtest: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; else echo "sendtest: FAIL ($T)"; fi
exit $FAIL
