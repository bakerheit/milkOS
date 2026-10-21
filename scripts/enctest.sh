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
#   boot 3: lock. Three disks: the encrypted home volume, a second encrypted one (Vault)
#           and a plain one. `snap lock Milk` is refused (home is on it), the plain one
#           too (not encrypted); Vault is busy while a process works in it or has a file
#           open there (the error names them), then locks: unmounted, listed locked, key
#           wiped; what was written just before is on the disk (Mac check); `snap unlock`
#           mounts it again.
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
  local SOCK="$T/$name.sock" LOG="$T/$name.log" extra=() i=1
  [ "$fw" != - ] && extra=(-fw_cfg "name=opt/milkos/passphrase,string=$fw")
  for d in ${EXTRA_DISKS:-}; do
    extra+=(-drive "file=$d,if=none,format=raw,id=hd$i" -device "virtio-blk-device,drive=hd$i"); i=$((i+1))
  done
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
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

VAULT="$T/vault.img" PLAIN="$T/plain.img"
$M mkfs "$VAULT" --size 32M --label Vault --encrypt --passphrase-file "$T/p2" --recovery-out "$T/vault-recovery" >/dev/null
$M mkfs "$PLAIN" --size 16M --label Plain >/dev/null
EXTRA_DISKS="$VAULT $PLAIN" boot b3 "$P2" \
  "cat /sys/fs/milkfs/info" "cat /sys/fs/milkfs/lock" \
  "snap lock Milk" "snap lock Plain" \
  "echo kept across the lock > /Volumes/Vault/notes.txt" \
  "cd /Volumes/Vault" "snap lock Vault" "cd /" \
  "sleep 4 < /Volumes/Vault/notes.txt &" "snap lock Vault" "cat /sys/fs/milkfs/error" "sleep 5" \
  "snap lock Vault" "ls /Volumes" "cat /Volumes/Vault/notes.txt" "cat /sys/fs/milkfs/lock" \
  "raw:snap unlock" "wait:Passphrase" "raw:$P2" "wait:unlocked" "wait:msh:" \
  "cat /Volumes/Vault/notes.txt" "echo second > /Volumes/Vault/two.txt" "snap lock"
L="$(tr -d '\r' < "$T/b3.log")"
I="$(echo "$L" | awk '/\$ cat \/sys\/fs\/milkfs\/info$/{on=1;next} on && /^msh:/{exit} on')"
echo "$I" | grep -q "^encryption:  on$" && echo "$I" | grep -q "^locked:      no$" && echo "$I" | grep -q "^cipher:      xchacha20-siv" \
  && echo "$I" | grep -q "^kdf:         scrypt N=2^16 r=8 p=4$" && echo "$I" | grep -q "^key slots:   1 passphrase, 1 recovery$" \
  && ok "info: encryption on, unlocked, cipher, KDF, key slots" || bad "info encryption lines: $(echo "$I" | tail -6)"
echo "$L" | grep -a -A4 "cat /sys/fs/milkfs/lock" | grep -aq "Milk unlocked at=/Volumes/Milk (home: can't be locked)" && ok "lock file: home volume marked" || bad "lock file listing"
echo "$L" | grep -aq "snap: lock Milk: .*holds /home/milk.*you can't lock the volume you're logged into.*(EBUSY" && ok "locking the home volume: EBUSY, says why" || bad "home lock refusal"
echo "$L" | grep -aq "snap: lock Plain: .*isn't encrypted.*(EINVAL" && ok "locking a plain volume: EINVAL, says why" || bad "plain lock refusal"
echo "$L" | grep -aq "snap: lock Vault: .*is in use: pid [0-9]* (msh): working directory.*(EBUSY" && ok "busy (cwd): names the shell" || bad "cwd busy: $(echo "$L" | grep -a 'snap: lock Vault' | head -1)"
echo "$L" | grep -aq "snap: lock Vault: .*is in use: pid [0-9]* (sleep): 1 open file (EBUSY" && ok "busy (open file): names the process" || bad "open-file busy: $(echo "$L" | grep -a 'snap: lock Vault' | sed -n 2p)"
echo "$L" | grep -aq "^lock: EBUSY: .*(sleep): 1 open file" && ok "/sys/fs/milkfs/error has the reason" || bad "error file"
echo "$L" | grep -aq "milkfs: vd. (Vault) locked, /Volumes/Vault unmounted, key wiped$" && ok "Vault locked, key wiped" || bad "lock log"
echo "$L" | awk '/\$ ls \/Volumes$/{on=1;next} on && /^msh:/{exit} on' | grep -aq "Vault" && bad "still mounted after lock" || ok "unmounted after lock"
echo "$L" | grep -aq "cat: /Volumes/Vault/notes.txt: " && ok "its files are gone until unlock" || bad "file readable while locked?"
echo "$L" | grep -aq "^vd. Vault locked" && ok "listed as locked" || bad "not listed locked"
[ "$(echo "$L" | grep -ac '^kept across the lock')" -ge 1 ] && echo "$L" | grep -aq "snap: Vault unlocked" && ok "unlock after lock: the file is back" || bad "unlock after lock"
[ "$(echo "$L" | grep -ac "milkfs: vd. (Vault) locked, /Volumes/Vault unmounted")" = 2 ] && ok "snap lock (no argument) locks every lockable volume" || bad "snap lock without argument"
$M check "$VAULT" --passphrase-file "$T/p2" > "$T/vcheck.txt" 2>&1 && ok "Mac: Vault checks clean after the locks" || { bad "Vault check"; cat "$T/vcheck.txt"; }
[ "$($M cat "$VAULT" /two.txt --passphrase-file "$T/p2")" = "second" ] && ok "Mac: written just before \`snap lock\`: on the disk" || bad "lock didn't sync"
grep -aq "kept across the lock" "$VAULT" && bad "Vault plaintext on the disk" || ok "Vault raw image: no plaintext"

echo
[ $FAIL = 0 ] && echo "enctest: PASS ($T)" || echo "enctest: FAIL ($T)"
exit $FAIL
