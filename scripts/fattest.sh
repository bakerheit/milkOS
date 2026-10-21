#!/usr/bin/env bash
# FAT32 round trip between macOS and milkOS (everything in a fresh /tmp directory).
#  1. scripts/mkfat.sh makes the image; the Mac mounts it and writes test files (long names,
#     spaces, non-ASCII, nested dirs, a 300 KB random file, 8.3 names, an empty file).
#  2. milkOS boots with it (private milkfs/scratch images, share read-write), reads every
#     file back out through /Volumes/Mac for a byte-exact compare, then writes: a long-name
#     directory and file, a copied binary, a rename to a long name, a delete, a dir move.
#  3. The Mac attaches the image read-only, checks all of that, runs fsck_msdos, detaches.
# Only the qemu started here is killed (by PID). Exit status 0 = everything matched.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-fattest.XXXXXX)"
IMG="$T/fat.img"
FAIL=0
bad() { echo "  FAIL: $*"; FAIL=1; }
ok() { echo "  ok: $*"; }

cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"

echo "== 1. Mac: make FAT32 image and write test files"
scripts/mkfat.sh "$IMG" 64 MILKFAT
MNT="$T/mnt"; mkdir "$MNT"
DEV="$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -mountpoint "$MNT" "$IMG" | awk 'NR==1{print $1}')"
REF="$T/ref"; mkdir -p "$REF/Projects/milkOS notes"
printf 'upper case 8.3\n' > "$REF/CAPS.TXT"
printf 'lower case 8.3\n' > "$REF/readme.txt"
printf 'a long file name with spaces\n' > "$REF/A Long File Name With Spaces.txt"
printf 'non-ASCII name\n' > "$REF/Résumé (draft) v2.markdown"
printf 'deep in a long-named dir\n' > "$REF/Projects/milkOS notes/ideas for later.md"
head -c 300000 /dev/urandom > "$REF/data.bin"
: > "$REF/empty"
cp -X -R "$REF/." "$MNT/"
sync
hdiutil detach "$DEV" >/dev/null
fsck_msdos -n "$IMG" >/dev/null 2>&1 && ok "fsck_msdos clean after the Mac wrote" || bad "fsck_msdos after the Mac wrote"

echo "== 2. milkOS: read, copy out, write"
SHARE_SUB="fattest-$$"
export MILK_FS="$T/fs.img" MILK_DISK="$T/disk.img" MILK_FAT="$IMG" MILK_SHARE_RW=1
source scripts/disk.sh; milk_disk_args
source scripts/share.sh; milk_share_args
SHARE_DIR="$(cd "$(git rev-parse --git-common-dir)/.." && pwd)/share"
X="$SHARE_DIR/$SHARE_SUB"; mkdir -p "$X"
head -c 200000 /dev/urandom > "$X/payload.bin"
SOCK="$T/serial.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -global virtio-mmio.force-legacy=false "${DISK_ARGS[@]}" "${SHARE_ARGS[@]}" 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true; rm -rf "$X"' EXIT
F=/Volumes/MILKFAT; O="/Volumes/Mac/$SHARE_SUB"
P='msh:[^\n]*\$ '
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:ls -l $F" "wait:$P" \
  "send:cat \"$F/A Long File Name With Spaces.txt\"" "wait:$P" \
  "send:cp \"$F/A Long File Name With Spaces.txt\" $O/long.txt" "wait:$P" \
  "send:cp $F/R*markdown $O/unicode.txt" "wait:$P" \
  "send:cp \"$F/Projects/milkOS notes/ideas for later.md\" $O/deep.txt" "wait:$P" \
  "send:cp $F/data.bin $O/data.bin" "wait:$P" \
  "send:cp $F/readme.txt $O/readme.txt" "wait:$P" \
  "send:cp $F/CAPS.TXT $O/caps.txt" "wait:$P" \
  "send:mkdir \"$F/Made In milkOS\"" "wait:$P" \
  "send:echo hello from milkOS > \"$F/Made In milkOS/a pretty long file name from milkOS.txt\"" "wait:$P" \
  "send:cp $O/payload.bin \"$F/Made In milkOS/Copied Binary Payload.bin\"" "wait:$P" \
  "send:mv $F/readme.txt \"$F/Renamed To A Long Name.txt\"" "wait:$P" \
  "send:rm $F/CAPS.TXT" "wait:$P" \
  "send:mv \"$F/Projects/milkOS notes\" \"$F/Projects/Notes Moved\"" "wait:$P" \
  "send:cp \"$F/Made In milkOS/Copied Binary Payload.bin\" $O/readback.bin" "wait:$P" \
  "send:cp \"$F/Renamed To A Long Name.txt\" $O/renamed.txt" "wait:$P" \
  "send:ls -l \"$F\" \"$F/Made In milkOS\" \"$F/Projects\"" "wait:$P" \
  "send:echo FATTEST""-DONE" "wait:FATTEST-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 900); do [ -e "$T/done" ] && break; sleep 0.2; done
sleep 1
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
[ -e "$T/done" ] || { bad "milkOS session did not finish"; tail -20 "$LOG"; }
grep -a "^fat: " "$LOG" | sed 's/^/  /'
echo "  milkOS sees, after its writes:"
tr -d '\r' < "$LOG" | sed -n '/^msh:[^$]*\$ ls -l "\/Volumes\/MILKFAT" /,/FATTEST-DONE/p' | grep -v FATTEST | sed 's/^/  | /'

echo "== check what milkOS read (byte-exact via the share)"
for pair in "long.txt:A Long File Name With Spaces.txt" "unicode.txt:Résumé (draft) v2.markdown" \
            "deep.txt:Projects/milkOS notes/ideas for later.md" "data.bin:data.bin" \
            "readme.txt:readme.txt" "caps.txt:CAPS.TXT"; do
  got="${pair%%:*}"; want="${pair#*:}"
  cmp -s "$X/$got" "$REF/$want" && ok "milkOS read \"$want\"" || bad "milkOS read \"$want\""
done

cmp -s "$X/readback.bin" "$X/payload.bin" && ok "milkOS reads back what it wrote (page cache)" || bad "milkOS read-back after write"
cmp -s "$X/renamed.txt" "$REF/readme.txt" && ok "milkOS reads a renamed file" || bad "milkOS read after rename"

echo "== 3. Mac: attach read-only and check what milkOS wrote"
fsck_msdos -n "$IMG" >/dev/null 2>&1 && ok "fsck_msdos clean after milkOS wrote" || { bad "fsck_msdos after milkOS wrote"; fsck_msdos -n "$IMG" || true; }
DEV="$(hdiutil attach -readonly -imagekey diskimage-class=CRawDiskImage -mountpoint "$MNT" "$IMG" | awk 'NR==1{print $1}')"
[ "$(cat "$MNT/Made In milkOS/a pretty long file name from milkOS.txt")" = "hello from milkOS" ] \
  && ok "long-name file written by milkOS" || bad "long-name file written by milkOS"
cmp -s "$MNT/Made In milkOS/Copied Binary Payload.bin" "$X/payload.bin" \
  && ok "200 KB binary copied by milkOS" || bad "binary copied by milkOS"
cmp -s "$MNT/Renamed To A Long Name.txt" "$REF/readme.txt" && ok "rename to a long name" || bad "rename to a long name"
[ ! -e "$MNT/CAPS.TXT" ] && ok "delete" || bad "delete"
cmp -s "$MNT/Projects/Notes Moved/ideas for later.md" "$REF/Projects/milkOS notes/ideas for later.md" \
  && [ ! -e "$MNT/Projects/milkOS notes" ] && ok "directory rename" || bad "directory rename"
cmp -s "$MNT/data.bin" "$REF/data.bin" && ok "untouched file intact" || bad "untouched file"
echo "  Mac sees:"; (cd "$MNT" && find . -path ./.fseventsd -prune -o -print | grep -v '^\./\._' | sort | sed 's/^/    /')
hdiutil detach "$DEV" >/dev/null && ok "detached"

echo
if [ $FAIL = 0 ]; then echo "fattest: PASS ($T)"; else echo "fattest: FAIL ($T)"; fi
exit $FAIL
