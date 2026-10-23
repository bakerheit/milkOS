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
OWN_WORK=1; [ -n "${1:-}" ] && OWN_WORK=0 # remove it on a pass only if this script made it
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
# A second, encrypted disk ("Vault"), unlocked at boot with a throwaway test passphrase (the
# kernel's test-only fw_cfg hook), so the window can lock it and unlock it again.
VAULT="$T/vault.img"; PW="vault-test-passphrase-not-real"
printf '%s\n' "$PW" > "$T/pw"
rm -f "$VAULT"
$M mkfs "$VAULT" --size 32M --label Vault --encrypt --passphrase-file "$T/pw" --recovery-out "$T/vault-recovery" >/dev/null
$M mkdir "$IMG" /Users/milk >/dev/null
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -serial "unix:$SOCK,server=on,wait=on" -device virtio-gpu-device -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
  -drive "file=$VAULT,if=none,format=raw,id=hd1" -device virtio-blk-device,drive=hd1 \
  -fw_cfg "name=opt/milkos/passphrase,string=$PW" \
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
ser "send:disks snapshot first-one" "wait:$P" "send:print hello > /Users/milk/Documents/new.txt" "wait:$P" "send:disks snapshot second-one" "wait:$P" "send:disks clone first-one try1" "wait:$P" "send:open diskutil" "wait:$P"
sleep 3
ser "send:text show /dev/wm" "wait:$P"
pos=$(tr -d '\r' < "$LOG" | grep -a "window .* Disk Utility" | tail -1 | sed -E 's/.*x[0-9]+\+([0-9]+)\+([0-9]+) .*/\1 \2/')
set -- $pos
BX=$(( $1 + 1 )); BY=$(( $2 + 33 ))
click() { q move $(( BX + $1 )) $(( BY + $2 )) click sleep "${3:-1}"; }
q screendump "$T/overview.png"
click 342 23                       # Snapshots tab
click 313 159                      # the row second-one
q screendump "$T/snapshots.png"
click 820 23                       # Take snapshot (toolbar)
ser "send:disks snapshots" "wait:$P"
out "disks snapshots" | grep -q "^snap-" && ok "Take snapshot made a snapshot" || bad "no new snapshot: $(out "disks snapshots")"
click 313 159                      # the row second-one again (taking a snapshot selects the new one)
click 824 286                      # Delete...
q screendump "$T/delete-sheet.png"
click 698 376                      # the Danger button "Delete snapshot"
# The first destructive control needs an administrator's ticket: the desktop's trusted
# password panel asks (the password field has the focus); the demo account's well-known
# default password, then Return. The ticket lasts 5 minutes, so the rest needs no panel.
sleep 1
q screendump "$T/auth-panel.png"
q type "milk" sleep 0.3 screendump "$T/auth-panel-typed.png" key ret sleep 2
ser "send:disks snapshots" "wait:$P"
out "disks snapshots" | grep -q "second-one" && bad "second-one is still there after Delete" || ok "Delete snapshot removed it (after the confirm sheet)"
out "disks snapshots" | grep -q "first-one" && ok "the other snapshots are untouched" || bad "first-one vanished"
click 313 129                      # the row first-one
click 440 286                      # Roll back...
q screendump "$T/rollback-sheet.png"
click 722 407                      # the Danger button "Roll back"
ser "send:disks snapshots" "wait:$P" "send:files list /Users/milk/Documents" "wait:$P"
out "disks snapshots" | grep -q "^snap-" && bad "a newer snapshot survived the rollback" || ok "Roll back deleted the snapshots newer than first-one"
out "files list /Users/milk/Documents" | grep -q "new.txt" && bad "new.txt survived the rollback" || ok "Roll back undid the change made after the snapshot"
click 506 23                       # Health tab
click 837 23 2                     # Scrub now
q screendump "$T/health.png"
ser "send:text show /System/Kernel/fs/milkfs/scrub" "wait:$P"
out "text show /System/Kernel/fs/milkfs/scrub" | grep -q "scrub: \[run [0-9]" && ok "Scrub now started a scrub" || bad "no scrub: $(out "text show /System/Kernel/fs/milkfs/scrub")"
# The Vault: lock it from the window (it is not the volume the session lives on), see it listed as
# locked, unlock it again with the passphrase.
ser "send:text show /System/Kernel/fs/milkfs/lock" "wait:$P"
out "text show /System/Kernel/fs/milkfs/lock" | grep -q "Vault unlocked" && ok "the Vault was unlocked at boot" || bad "no unlocked Vault: $(out "text show /System/Kernel/fs/milkfs/lock")"
click 100 163                      # the Vault row in the sidebar
q screendump "$T/vault.png"
# While a program holds a file on it, Lock now is refused and the sheet names the program.
ser "send:print x > /Volumes/Vault/x.txt" "wait:$P" "send:pause 12 < /Volumes/Vault/x.txt &" "wait:$P"
click 258 226 2                    # Lock now (refused)
q screendump "$T/busy.png"
q key ret sleep 0.5
ser "send:text show /System/Kernel/fs/milkfs/lock" "wait:$P"
out "text show /System/Kernel/fs/milkfs/lock" | grep -q "Vault unlocked" && ok "a busy Vault was not locked" || bad "the Vault was locked while in use"
ser "send:pause 13" "wait:$P"
click 258 226 3                    # Lock now
ser "send:text show /System/Kernel/fs/milkfs/lock" "wait:$P"
out "text show /System/Kernel/fs/milkfs/lock" | grep -q "Vault locked" && ok "Lock now locked the Vault" || bad "Vault not locked: $(out "text show /System/Kernel/fs/milkfs/lock")"
click 100 163                      # the locked Vault row
click 278 212                      # Unlock...
q type "wrong-passphrase" key ret sleep 3 screendump "$T/wrong.png"
q key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace key backspace
q type "$PW" key ret sleep 4
ser "send:text show /System/Kernel/fs/milkfs/lock" "wait:$P"
out "text show /System/Kernel/fs/milkfs/lock" | grep -q "Vault unlocked" && ok "Unlock with the passphrase mounted the Vault again" || bad "Vault still locked: $(out "text show /System/Kernel/fs/milkfs/lock")"
grep -aqi "panic" "$LOG" && bad "a kernel panic in the serial log" || ok "no panic"
if [ "$FAIL" = 0 ] && [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ]; then rm -rf "$T" || true; echo "disk-test: PASS"
elif [ "$FAIL" = 0 ]; then echo "disk-test: PASS (screenshots in $T)"; else echo "disk-test: FAIL (logs and screenshots in $T)"; exit 1; fi
