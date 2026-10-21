#!/usr/bin/env bash
# Encrypted milkfs on private images (fresh /tmp directory; only qemus started here are
# killed). The passphrases are test values, obviously fake.
#   Mac:    mkfs --encrypt (recovery key to a file), put a file with a canary string;
#           the raw image has neither the canary nor the file name; a wrong passphrase
#           is refused; the recovery key opens it.
#   boot 1: no passphrase given: the volume stays locked (not mounted, home in RAM); a
#           wrong passphrase is refused; `snap unlock` with the right one mounts it and
#           it becomes /home/milk; files written there; `snap passwd` changes the
#           passphrase (rewrap only).
#   Mac:    the old passphrase no longer opens it, the new one and the recovery key do;
#           check clean; the canary and the new files' text are still not on the disk.
#   boot 2: unlocked with the recovery key; the files are there; scrub is clean.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-enctest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
P1="not-a-real-passphrase"
P2="another-fake-test-passphrase"
printf '%s\n' "$P1" > "$T/p1"; printf '%s\n' "$P2" > "$T/p2"; printf 'wrong-fake-passphrase\n' > "$T/bad"
CANARY="MILKFS-CANARY-PLAINTEXT-7f3a9c"
IMG="$T/e.img"
$M mkfs "$IMG" --size 64M --encrypt --passphrase-file "$T/p1" --recovery-out "$T/recovery" >/dev/null
REC="$(cat "$T/recovery")"
echo "$CANARY and some text around it" > "$T/canary.txt"
$M put "$IMG" /docs/canary-file.txt "$T/canary.txt" --passphrase-file "$T/p1" >/dev/null
grep -aq "$CANARY" "$IMG" && bad "the canary is on the disk in plaintext" || ok "raw image: no canary string"
grep -aq "canary-file" "$IMG" && bad "a file name is on the disk in plaintext" || ok "raw image: no file name"
$M check "$IMG" --passphrase-file "$T/bad" >/dev/null 2>&1 && bad "a wrong passphrase opened it" || ok "Mac: a wrong passphrase is refused"
[ "$($M cat "$IMG" /docs/canary-file.txt --recovery "$REC")" = "$CANARY and some text around it" ] && ok "Mac: the recovery key opens it" || bad "recovery key on the Mac"
cp "$IMG" "$T/plain-check.img"

QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT
boot() { # name [fw_cfg passphrase or -] commands...
  local name="$1" fw="$2"; shift 2
  local SOCK="$T/$name.sock" LOG="$T/$name.log" extra=()
  [ "$fw" != - ] && extra=(-fw_cfg "name=opt/milkos/passphrase,string=$fw")
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    ${extra[@]+"${extra[@]}"} 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  local args=("wait:$P")
  for c in "$@"; do
    case "$c" in
      raw:*) args+=("send:${c#raw:}") ;;            # a line typed at a prompt (no shell prompt after)
      wait:*) args+=("$c") ;;
      *) args+=("send:$c" "wait:$P") ;;
    esac
  done
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "${args[@]}" "send:echo ENC""-DONE" "wait:ENC-DONE" "done:$T/$name.done" &
  local DPID=$!
  for _ in $(seq 1500); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1.5
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
}
boot b1 - \
  "cat /sys/fs/milkfs/unlock" "ls /Volumes" \
  "raw:snap unlock" "wait:Passphrase" "raw:wrong-fake-passphrase" "wait:try again" "wait:Passphrase" "raw:$P1" "wait:unlocked" \
  "wait:msh:" \
  "cat /home/milk/../milk/README.txt" "cat /Volumes/Milk/docs/canary-file.txt" \
  "echo written inside milkOS: $CANARY-2 > /home/milk/inside.txt" \
  "raw:snap passwd" "wait:Current" "raw:$P1" "wait:New passphrase" "raw:$P2" "wait:again" "raw:$P2" "wait:changed" \
  "wait:msh:" "echo now > /sys/fs/milkfs/sync"
L="$(tr -d '\r' < "$T/b1.log")"
echo "$L" | grep -aq "vda (Milk) is encrypted and locked" && ok "boot 1: the volume is locked at boot" || bad "not reported locked"
echo "$L" | grep -a -A2 "cat /sys/fs/milkfs/unlock" | grep -aq "vda Milk locked" && ok "/sys/fs/milkfs/unlock lists it" || bad "unlock file"
echo "$L" | awk '/\$ ls \/Volumes$/{on=1;next} on && /^msh:/{exit} on' | grep -aq "Milk" && bad "mounted while locked" || ok "not mounted while locked"
echo "$L" | grep -aq "wrong passphrase or recovery key" && ok "a wrong passphrase is refused" || bad "wrong passphrase accepted?"
echo "$L" | grep -aq "snap: Milk unlocked" && ok "snap unlock mounts it" || bad "snap unlock"
echo "$L" | grep -aq "$P1" && bad "the passphrase was echoed" || ok "the passphrase isn't echoed or logged"
echo "$L" | grep -aq "milkfs: /home/milk is /Volumes/Milk/home/milk" && ok "it becomes /home/milk" || bad "home not on the unlocked volume"
echo "$L" | grep -aq "^$CANARY and some text around it" && ok "the Mac-written file reads back" || bad "canary file"
echo "$L" | grep -aq "snap: passphrase changed" && ok "snap passwd rewraps the key" || bad "snap passwd"

$M check "$IMG" --passphrase-file "$T/p1" >/dev/null 2>&1 && bad "the old passphrase still opens it" || ok "Mac: the old passphrase no longer opens it"
$M check "$IMG" --passphrase-file "$T/p2" > "$T/check.txt" 2>&1 && ok "Mac: the new passphrase opens it, check clean" || { bad "new passphrase / check"; cat "$T/check.txt"; }
[ "$($M cat "$IMG" /home/milk/inside.txt --recovery "$REC")" = "written inside milkOS: $CANARY-2" ] && ok "Mac: the recovery key still opens it; the file written in milkOS is there" || bad "recovery after passwd"
grep -aq "$CANARY" "$IMG" && bad "plaintext on the disk after milkOS wrote" || ok "raw image after milkOS wrote: no canary, no plaintext"

boot b2 "$REC" "cat /home/milk/inside.txt" "snap scrub"
L="$(tr -d '\r' < "$T/b2.log")"
echo "$L" | grep -aq "^written inside milkOS: $CANARY-2" && ok "boot 2 (recovery key): the file is there" || bad "boot 2 file"
echo "$L" | grep -a "^scrub: \[run" | grep -q " 0 error" && ok "scrub of the encrypted volume: clean" || bad "scrub: $(echo "$L" | grep -a '^scrub:' | head -2)"
echo
[ $FAIL = 0 ] && echo "enctest: PASS ($T)" || echo "enctest: FAIL ($T)"
exit $FAIL
