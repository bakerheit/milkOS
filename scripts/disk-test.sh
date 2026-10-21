#!/usr/bin/env bash
# Disk Utility end to end on a private image: boot a desktop, make snapshots and a clone from the
# shell, open the window and drive it with the pointer: take a snapshot, delete one (confirm
# sheet), roll back (confirm sheet), start a scrub. The shell then checks what the
# volume really did. Screenshots are kept. About 1.5 minutes.
#   scripts/disk-test.sh [workdir]
# Only the qemu started here is killed (by PID). Pointer coordinates are relative to the window's
# position, which is read from /dev/wm.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="${1:-$(mktemp -d /tmp/milk-disktest.XXXXXX)}"
mkdir -p "$T"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/a.img"
rm -f "$IMG"
$M mkfs "$IMG" --size 64M >/dev/null
$M mkdir "$IMG" /home/milk >/dev/null
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -serial "unix:$SOCK,server,nowait" -device ramfb -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
  -qmp "unix:$QMP,server,nowait" >/dev/null 2>&1 &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
n=0
ser() {
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/ser.$n" &
  local d=$!
  for _ in $(seq 400); do [ -e "$T/ser.$n" ] && break; sleep 0.2; done
  kill "$d" 2>/dev/null || true; wait "$d" 2>/dev/null || true
}
q() { python3 scripts/qmp.py "$QMP" "$@"; }
# Output of the command typed as $1 (the last time), up to the next prompt.
out() { tr -d '\r' < "$LOG" | awk -v c="$1" '/^msh:/ { if (on) {on=0}; if (substr($0, index($0, "$ ") + 2) == c) {buf=""; on=1; next} } $0 == c && !on {buf=""; on=1; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'; }

ser "wait:$P"
q move 785 615 click sleep 1
ser "send:snap create first-one" "wait:$P" "send:echo hello > /home/milk/Documents/new.txt" "wait:$P" "send:snap create second-one" "wait:$P" "send:snap clone first-one try1" "wait:$P" "send:open /bin/diskutil" "wait:$P"
sleep 3
ser "send:cat /dev/wm" "wait:$P"
pos=$(tr -d '\r' < "$LOG" | grep -a "window .* Disk Utility" | tail -1 | sed -E 's/.*x[0-9]+\+([0-9]+)\+([0-9]+) .*/\1 \2/')
set -- $pos
BX=$(( $1 + 1 )); BY=$(( $2 + 33 ))
click() { q move $(( BX + $1 )) $(( BY + $2 )) click sleep "${3:-1}"; }
q screendump "$T/overview.png"
click 342 23                       # Snapshots tab
click 313 159                      # the row second-one
q screendump "$T/snapshots.png"
click 820 23                       # Take snapshot (toolbar)
ser "send:snap" "wait:$P"
out "snap" | grep -q "^snap-" && ok "Take snapshot made a snapshot" || bad "no new snapshot: $(out snap)"
click 313 159                      # the row second-one again (taking a snapshot selects the new one)
click 824 286                      # Delete...
q screendump "$T/delete-sheet.png"
click 698 376                      # the Danger button "Delete snapshot"
ser "send:snap" "wait:$P"
out "snap" | grep -q "second-one" && bad "second-one is still there after Delete" || ok "Delete snapshot removed it (after the confirm sheet)"
out "snap" | grep -q "first-one" && ok "the other snapshots are untouched" || bad "first-one vanished"
click 313 129                      # the row first-one
click 440 286                      # Roll back...
q screendump "$T/rollback-sheet.png"
click 722 407                      # the Danger button "Roll back"
ser "send:snap" "wait:$P" "send:ls /home/milk/Documents" "wait:$P"
out "snap" | grep -q "^snap-" && bad "a newer snapshot survived the rollback" || ok "Roll back deleted the snapshots newer than first-one"
out "ls /home/milk/Documents" | grep -q "new.txt" && bad "new.txt survived the rollback" || ok "Roll back undid the change made after the snapshot"
click 506 23                       # Health tab
click 837 23 2                     # Scrub now
q screendump "$T/health.png"
ser "send:cat /sys/fs/milkfs/scrub" "wait:$P"
out "cat /sys/fs/milkfs/scrub" | grep -q "run 1" && ok "Scrub now started a scrub" || bad "no scrub: $(out "cat /sys/fs/milkfs/scrub")"
grep -aqi "panic" "$LOG" && bad "a kernel panic in the serial log" || ok "no panic"
if [ "$FAIL" = 0 ]; then echo "disk-test: PASS (screenshots in $T)"; else echo "disk-test: FAIL (logs and screenshots in $T)"; exit 1; fi
