#!/usr/bin/env bash
# milkfs format upgrade: a v0 image (one-block extents, made on the Mac) is mounted by
# this kernel, read, partly rewritten (so a file mixes v0 extents with a v1 run, and a
# small file becomes inline), then checked on the Mac: v1, clean, every byte right.
# The same image has the old folder layout (home/milk, Applications, etc/milk/drop and
# etc/milk/net): the boot moves it to the new one (Users/milk, Apps, System/Settings/milk,
# fs/layout.rs), and a second boot moves nothing.
# Fresh /tmp directory; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="$(mktemp -d /tmp/milk-upgrade.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"

IMG="$T/v0.img"
head -c 300000 /dev/urandom > "$T/data.bin"
printf 'small file from the v0 days\n' > "$T/small.txt"
$M mkfs "$IMG" --size 64M --v0 >/dev/null
$M put "$IMG" /docs/data.bin "$T/data.bin" --v0 >/dev/null
$M put "$IMG" /docs/small.txt "$T/small.txt" --v0 >/dev/null
# The old layout, as a disk from before the plain-English folders has it.
printf 'written before the move\n' > "$T/old-note.txt"
printf 'format 1\nname hello\nversion 1.0.0\ndescription an installed package\nexec hello\nfiles hello\npayload-sha256 -\n' > "$T/manifest"
printf 'repo http://10.0.2.2:8765/\nfile hello-1.0.0.milkpkg\nsha256 -\n' > "$T/origin"
printf '#!hello\n' > "$T/hello"
printf 'old-machine\n' > "$T/name"
printf 'oldbox\n' > "$T/hostname"
$M put "$IMG" /home/milk/Documents/old-note.txt "$T/old-note.txt" --v0 >/dev/null
$M put "$IMG" /Applications/hello/manifest "$T/manifest" --v0 >/dev/null
$M put "$IMG" /Applications/hello/.origin "$T/origin" --v0 >/dev/null
$M put "$IMG" /Applications/hello/hello "$T/hello" --v0 >/dev/null
$M put "$IMG" /etc/milk/drop/name "$T/name" --v0 >/dev/null
$M put "$IMG" /etc/milk/net/hostname "$T/hostname" --v0 >/dev/null
$M check "$IMG" | grep -q "milkfs v0" && ok "v0 image with v0 extents" || bad "v0 image"

SOCK="$T/serial.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
D=/Volumes/Milk/docs
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
  "send:text show $D/small.txt" "wait:$P" \
  "send:files copy $D/data.bin $D/copy.bin" "wait:$P" \
  "send:print appended by milkOS >> $D/data.bin" "wait:$P" \
  "send:print new small > $D/small.txt" "wait:$P" \
  "send:text show /Users/milk/Documents/old-note.txt" "wait:$P" \
  "send:files list /Volumes/Milk" "wait:$P" \
  "send:files list /Users/milk/Sites" "wait:$P" \
  "send:text show /System/Settings/milk/drop/name" "wait:$P" \
  "send:system info" "wait:$P" \
  "send:apps list" "wait:$P" \
  "send:print now > /System/Kernel/fs/milkfs/sync" "wait:$P" \
  "send:print UPGRADE""-DONE" "wait:UPGRADE-DONE" "done:$T/done" &
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

echo "-- old folder layout"
L="$(tr -d '\r' < "$LOG")"
echo "$L" | grep -a "layout: moved" | sed 's/^/  /'
echo "$L" | grep -aq "layout: moved /Volumes/Milk to the new folder layout" && ok "the boot moved the old layout" || bad "no layout move logged"
echo "$L" | grep -aq "milkfs: /Users/milk is /Volumes/Milk/Users/milk (persistent)" && ok "the moved home is /Users/milk" || bad "home bind"
echo "$L" | grep -aq "^written before the move" && ok "a file from the old home reads at /Users/milk" || bad "old home file"
echo "$L" | grep -a -A3 'files list /Users/milk/Sites$' | grep -aq "index.html" && ok "the moved home got ~/Sites" || bad "no ~/Sites"
echo "$L" | grep -aq "^old-machine$" && ok "Milk Drop's settings are at /System/Settings/milk/drop" || bad "drop settings"
echo "$L" | grep -Eaq "^name +oldbox$" && ok "the hostname moved with /System/Settings/milk/net" || bad "hostname"
echo "$L" | grep -aEq "^hello +1\.0\.0" && ok "the installed package is in /Apps (apps list)" || bad "package in /Apps"
$M ls "$IMG" / > "$T/root.txt"
grep -Eq " (home|Applications|etc)$" "$T/root.txt" && { cat "$T/root.txt"; bad "old folders left on the volume"; } || ok "no home, Applications or etc left on the volume"
$M ls "$IMG" /Apps > "$T/apps.txt"
grep -q " hello$" "$T/apps.txt" && grep -q " store$" "$T/apps.txt" && ok "Apps has the package and the built-in apps" || { cat "$T/apps.txt"; bad "Apps on disk"; }
[ "$($M cat "$IMG" /Users/milk/Documents/old-note.txt)" = "written before the move" ] && ok "Mac: the note is at /Users/milk" || bad "Mac: note"
[ "$($M cat "$IMG" /System/Settings/milk/drop/name)" = "old-machine" ] && ok "Mac: drop name at /System/Settings/milk/drop" || bad "Mac: drop name"
$M check "$IMG" > "$T/check2.txt" 2>&1 && ok "host check clean after the move" || { bad "host check after move"; cat "$T/check2.txt"; }

echo "-- second boot: nothing left to move"
LOG2="$T/serial2.log"; rm -f "$SOCK" "$T/done"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
QPID=$!
python3 scripts/serial_drive.py "$SOCK" "$LOG2" "wait:$P" "send:text show /Users/milk/Documents/old-note.txt" "wait:$P" "done:$T/done" &
DPID=$!
for _ in $(seq 600); do [ -e "$T/done" ] && break; sleep 0.2; done
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
L2="$(tr -d '\r' < "$LOG2")"
echo "$L2" | grep -aq "^written before the move" && ok "second boot: the home is still there" || bad "second boot: home"
echo "$L2" | grep -aq "layout: moved" && bad "second boot moved something again" || ok "second boot: no move (idempotent)"
echo "$L2" | grep -aq "built-in app(s) copied" && bad "second boot copied the built-in apps again" || ok "second boot: built-in apps unchanged, not rewritten"
echo
if [ $FAIL = 0 ]; then echo "upgradetest: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; else echo "upgradetest: FAIL ($T)"; fi
exit $FAIL
