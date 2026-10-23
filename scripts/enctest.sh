#!/usr/bin/env bash
# Encrypted milkfs on private images (fresh /tmp directory; only qemus started here are
# killed). The passphrases are test values, obviously fake.
#   scripts/enctest.sh [keys] [lock] [convert]    the parts to run (default: all three, in turn);
#   check-all runs each part as its own test (enctest-keys, -lock, -convert), side by side.
# Part keys: the Mac steps and boot 1. Part lock: boots 3 and 4. Part convert: boots 5 and 6.
# (Opening with the recovery key and a clean scrub of an encrypted volume, once a boot of their
# own, are milkfs host tests now: milkfs/src/tests/crypt.rs.)
#   Mac:    mkfs --encrypt (recovery key to a file), put a file with a canary string;
#           the raw image has neither the canary nor the file name; a wrong passphrase
#           is refused; the recovery key opens it.
#   boot 1: no passphrase given: the volume stays locked (not mounted, home in RAM); a
#           wrong passphrase is refused; `disks unlock` with the right one mounts it and
#           it becomes /Users/milk; files written there; `disks passphrase` changes the
#           passphrase (rewrap only).
#   Mac:    the old passphrase no longer opens it, the new one and the recovery key do;
#           check clean; the canary and the new files' text are still not on the disk.
#   boot 3: lock. Three disks: an encrypted home volume (made on the Mac), a second encrypted one (Vault)
#           and a plain one. `disks lock Milk` is refused (home is on it), the plain one
#           too (not encrypted); Vault is busy while a process works in it or has a file
#           open there (the error names them), then locks: unmounted, listed locked, key
#           wiped; what was written just before is on the disk (Mac check); `disks unlock`
#           mounts it again.
#   boot 4: kernel mkfs --encrypt: a blank disk is listed in /System/Kernel/fs/milkfs/disks,
#           `disks format vda Fresh --encrypt` asks for the passphrase (not echoed, not
#           logged) and shows the recovery key once; the disk is then listed encrypted
#           and locked, `disks unlock` mounts it, files are written; format of the mounted
#           disk is refused. Mac: the passphrase and the shown recovery key open it, check
#           clean, no plaintext on the disk.
#   boot 5: encrypt the home volume in place while it is in use: `disks encrypt` (slowed
#           down) asks for the passphrase; meanwhile info shows the progress, a reflink
#           copy, a rollback and a second encrypt are refused (EBUSY), files written go
#           in; at full speed it finishes. Mac: it is encrypted, the files are there, no
#           plaintext on the disk.
#   boot 6: it is locked at boot; `disks unlock`, then `disks decrypt --wait`. Mac: plain
#           again (no passphrase needed), the files are there.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-enctest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
P1="not-a-real-passphrase"
P2="another-fake-test-passphrase"
printf '%s\n' "$P1" > "$T/p1"; printf '%s\n' "$P2" > "$T/p2"; printf 'wrong-fake-passphrase\n' > "$T/bad"
CANARY="MILKFS-CANARY-PLAINTEXT-7f3a9c"
PARTS="${*:-keys lock convert}"
for p in $PARTS; do case "$p" in keys|lock|convert) ;; *) echo "enctest: no part $p (keys, lock, convert)" >&2; exit 2 ;; esac; done
want() { case " $PARTS " in *" $1 "*) return 0 ;; esac; return 1; }

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
    -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    ${extra[@]+"${extra[@]}"} 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  # A first command that only lets the boot finish (services starting print their banners
  # over the first prompt, so a real command typed there wouldn't sit on its own line).
  local args=("wait:$P" "send:pause 1" "wait:$P")
  for c in "$@"; do
    case "$c" in
      raw:*) args+=("send:${c#raw:}") ;;            # a line typed at a prompt (no shell prompt after)
      wait:*) args+=("$c") ;;
      *) args+=("send:$c" "wait:$P") ;;
    esac
  done
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "${args[@]}" "send:print ENC""-DONE" "wait:ENC-DONE" "done:$T/$name.done" &
  local DPID=$!
  for _ in $(seq 1500); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1.5
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
}

if want keys; then
IMG="$T/e.img"
$M mkfs "$IMG" --size 64M --encrypt --passphrase-file "$T/p1" --recovery-out "$T/recovery" >/dev/null
REC="$(cat "$T/recovery")"
echo "$CANARY and some text around it" > "$T/canary.txt"
$M put "$IMG" /docs/canary-file.txt "$T/canary.txt" --passphrase-file "$T/p1" >/dev/null
grep -aq "$CANARY" "$IMG" && bad "the canary is on the disk in plaintext" || ok "raw image: no canary string"
grep -aq "canary-file" "$IMG" && bad "a file name is on the disk in plaintext" || ok "raw image: no file name"
$M check "$IMG" --passphrase-file "$T/bad" >/dev/null 2>&1 && bad "a wrong passphrase opened it" || ok "Mac: a wrong passphrase is refused"
[ "$($M cat "$IMG" /docs/canary-file.txt --recovery "$REC")" = "$CANARY and some text around it" ] && ok "Mac: the recovery key opens it" || bad "recovery key on the Mac"

boot b1 - \
  "text show /System/Kernel/fs/milkfs/unlock" "files list /Volumes" \
  "raw:disks unlock" "wait:Passphrase" "raw:wrong-fake-passphrase" "wait:try again" "wait:Passphrase" "raw:$P1" "wait:unlocked" \
  "wait:msh:" \
  "text show /Users/milk/../milk/README.txt" "text show /Volumes/Milk/docs/canary-file.txt" \
  "print written inside milkOS: $CANARY-2 > /Users/milk/inside.txt" \
  "raw:disks passphrase" "wait:Current" "raw:$P1" "wait:New passphrase" "raw:$P2" "wait:again" "raw:$P2" "wait:changed" \
  "wait:msh:" "print now > /System/Kernel/fs/milkfs/sync"
L="$(tr -d '\r' < "$T/b1.log")"
echo "$L" | grep -aq "vda (Milk) is encrypted and locked" && ok "boot 1: the volume is locked at boot" || bad "not reported locked"
echo "$L" | grep -a -A2 "text show /System/Kernel/fs/milkfs/unlock" | grep -aq "vda Milk locked" && ok "/System/Kernel/fs/milkfs/unlock lists it" || bad "unlock file"
echo "$L" | awk '/\$ files list \/Volumes$/{on=1;next} on && /^msh:/{exit} on' | grep -aq "Milk" && bad "mounted while locked" || ok "not mounted while locked"
echo "$L" | grep -aq "wrong passphrase or recovery key" && ok "a wrong passphrase is refused" || bad "wrong passphrase accepted?"
echo "$L" | grep -aq "disks: Milk unlocked" && ok "disks unlock mounts it" || bad "disks unlock"
echo "$L" | grep -aq "$P1" && bad "the passphrase was echoed" || ok "the passphrase isn't echoed or logged"
echo "$L" | grep -aq "milkfs: /Users/milk is /Volumes/Milk/Users/milk" && ok "it becomes /Users/milk" || bad "home not on the unlocked volume"
echo "$L" | grep -aq "^$CANARY and some text around it" && ok "the Mac-written file reads back" || bad "canary file"
echo "$L" | grep -aq "disks: passphrase changed" && ok "disks passphrase rewraps the key" || bad "disks passphrase"

$M check "$IMG" --passphrase-file "$T/p1" >/dev/null 2>&1 && bad "the old passphrase still opens it" || ok "Mac: the old passphrase no longer opens it"
$M check "$IMG" --passphrase-file "$T/p2" > "$T/check.txt" 2>&1 && ok "Mac: the new passphrase opens it, check clean" || { bad "new passphrase / check"; cat "$T/check.txt"; }
[ "$($M cat "$IMG" /Users/milk/inside.txt --recovery "$REC")" = "written inside milkOS: $CANARY-2" ] && ok "Mac: the recovery key still opens it; the file written in milkOS is there" || bad "recovery after passwd"
grep -aq "$CANARY" "$IMG" && bad "plaintext on the disk after milkOS wrote" || ok "raw image after milkOS wrote: no canary, no plaintext"
fi

if want lock; then
# The home volume for boot 3: encrypted with P2 (so the same passphrase opens it and the Vault).
IMG="$T/home3.img"
$M mkfs "$IMG" --size 64M --encrypt --passphrase-file "$T/p2" --recovery-out "$T/recovery3" >/dev/null
VAULT="$T/vault.img" PLAIN="$T/plain.img"
$M mkfs "$VAULT" --size 32M --label Vault --encrypt --passphrase-file "$T/p2" --recovery-out "$T/vault-recovery" >/dev/null
$M mkfs "$PLAIN" --size 16M --label Plain >/dev/null
EXTRA_DISKS="$VAULT $PLAIN" boot b3 "$P2" \
  "text show /System/Kernel/fs/milkfs/info" "text show /System/Kernel/fs/milkfs/lock" \
  "disks lock Milk" "disks lock Plain" \
  "print kept across the lock > /Volumes/Vault/notes.txt" \
  "go /Volumes/Vault" "disks lock Vault" "go /" \
  "pause 4 < /Volumes/Vault/notes.txt &" "disks lock Vault" "text show /System/Kernel/fs/milkfs/error" "pause 5" \
  "disks lock Vault" "files list /Volumes" "text show /Volumes/Vault/notes.txt" "text show /System/Kernel/fs/milkfs/lock" \
  "raw:disks unlock" "wait:Passphrase" "raw:$P2" "wait:unlocked" "wait:msh:" \
  "text show /Volumes/Vault/notes.txt" "print second > /Volumes/Vault/two.txt" "disks lock"
L="$(tr -d '\r' < "$T/b3.log")"
I="$(echo "$L" | awk '/\$ text show \/System\/Kernel\/fs\/milkfs\/info$/{on=1;next} on && /^msh:/{exit} on')"
echo "$I" | grep -q "^encryption:  on$" && echo "$I" | grep -q "^locked:      no$" && echo "$I" | grep -q "^cipher:      xchacha20-siv" \
  && echo "$I" | grep -q "^kdf:         scrypt N=2^16 r=8 p=4$" && echo "$I" | grep -q "^key slots:   1 passphrase, 1 recovery$" \
  && ok "info: encryption on, unlocked, cipher, KDF, key slots" || bad "info encryption lines: $(echo "$I" | tail -6)"
echo "$L" | grep -a -A4 "text show /System/Kernel/fs/milkfs/lock" | grep -aq "Milk unlocked at=/Volumes/Milk (home: can't be locked)" && ok "lock file: home volume marked" || bad "lock file listing"
echo "$L" | grep -aq "disks lock Milk: .*holds /Users/milk.*you can't lock the volume you're logged into.*(EBUSY" && ok "locking the home volume: EBUSY, says why" || bad "home lock refusal"
echo "$L" | grep -aq "disks lock Plain: .*isn't encrypted.*(EINVAL" && ok "locking a plain volume: EINVAL, says why" || bad "plain lock refusal"
echo "$L" | grep -aq "disks lock Vault: .*is in use: pid [0-9]* (msh): working directory.*(EBUSY" && ok "busy (cwd): names the shell" || bad "cwd busy: $(echo "$L" | grep -a 'disks lock Vault' | head -1)"
echo "$L" | grep -aq "disks lock Vault: .*is in use: pid [0-9]* (pause): 1 open file (EBUSY" && ok "busy (open file): names the process" || bad "open-file busy: $(echo "$L" | grep -a 'disks lock Vault' | sed -n 2p)"
echo "$L" | grep -aq "^lock: EBUSY: .*(pause): 1 open file" && ok "/System/Kernel/fs/milkfs/error has the reason" || bad "error file"
echo "$L" | grep -aq "milkfs: vd. (Vault) locked, /Volumes/Vault unmounted, key wiped$" && ok "Vault locked, key wiped" || bad "lock log"
echo "$L" | awk '/\$ files list \/Volumes$/{on=1;next} on && /^msh:/{exit} on' | grep -aq "Vault" && bad "still mounted after lock" || ok "unmounted after lock"
echo "$L" | grep -aq "text: /Volumes/Vault/notes.txt doesn.t exist" && ok "its files are gone until unlock" || bad "file readable while locked?"
echo "$L" | grep -aq "^vd. Vault locked" && ok "listed as locked" || bad "not listed locked"
[ "$(echo "$L" | grep -ac '^kept across the lock')" -ge 1 ] && echo "$L" | grep -aq "disks: Vault unlocked" && ok "unlock after lock: the file is back" || bad "unlock after lock"
[ "$(echo "$L" | grep -ac "milkfs: vd. (Vault) locked, /Volumes/Vault unmounted")" = 2 ] && ok "disks lock (no argument) locks every lockable volume" || bad "disks lock without argument"
$M check "$VAULT" --passphrase-file "$T/p2" > "$T/vcheck.txt" 2>&1 && ok "Mac: Vault checks clean after the locks" || { bad "Vault check"; cat "$T/vcheck.txt"; }
[ "$($M cat "$VAULT" /two.txt --passphrase-file "$T/p2")" = "second" ] && ok "Mac: written just before \`disks lock\`: on the disk" || bad "lock didn't sync"
grep -aq "kept across the lock" "$VAULT" && bad "Vault plaintext on the disk" || ok "Vault raw image: no plaintext"

BLANK="$T/blank.img"; P3="third-fake-test-passphrase"
dd if=/dev/zero of="$BLANK" bs=1m count=24 2>/dev/null
IMG="$BLANK" EXTRA_DISKS="" boot b4 - \
  "text show /System/Kernel/fs/milkfs/disks" \
  "raw:disks format vda Fresh --encrypt" "wait:Passphrase for the new" "raw:$P3" "wait:Again" "raw:$P3" "wait:recovery key: " \
  "wait:msh:" "text show /System/Kernel/fs/milkfs/disks" \
  "raw:disks unlock vda" "wait:Passphrase" "raw:$P3" "wait:unlocked" "wait:msh:" \
  "print made in milkOS: $CANARY-3 > /Volumes/Fresh/fresh.txt" "disks format vda Again --force" \
  "text show /System/Kernel/fs/milkfs/disks" "print now > /System/Kernel/fs/milkfs/sync"
L="$(tr -d '\r' < "$T/b4.log")"
D4() { echo "$L" | awk -v n="$1" '/(^|\$ )text show \/System\/Kernel\/fs\/milkfs\/disks$/{c++; on=(c==n); next} on && /^msh:/{on=0} on'; }
D4 1 | grep -q "^vda 25165824 rw other$" && ok "disks: the blank disk (24 MiB, no milkfs)" || bad "disks before mkfs: $(D4 1)"
echo "$L" | grep -aq "^disks: vda formatted as an encrypted milkfs volume" && ok "disks format --encrypt formats it" || bad "disks format --encrypt"
REC4="$(echo "$L" | sed -n 's/^disks: recovery key: //p' | head -1)"
echo "$REC4" | grep -Eq '^([0-9A-F]{4}-){7}[0-9A-F]{4}$' && ok "the recovery key is shown once ($(echo "$L" | grep -ac 'recovery key: ') time)" || bad "recovery key shown: $REC4"
D4 2 | grep -q "^vda 25165824 rw milkfs encrypted locked Fresh$" && ok "disks: now milkfs, encrypted, locked" || bad "disks after mkfs: $(D4 2)"
echo "$L" | grep -aq "disks: Fresh unlocked" && ok "disks unlock mounts the new volume" || bad "unlock the new volume"
echo "$L" | grep -aq "disks format vda: vda is mounted: lock or unmount it first (EBUSY" && ok "format of the mounted disk: EBUSY" || bad "format of a mounted disk"
D4 3 | grep -q "^vda 25165824 rw milkfs encrypted mounted=/Volumes/Fresh Fresh$" && ok "disks: mounted" || bad "disks mounted: $(D4 3)"
echo "$L" | grep -aq "$P3" && bad "the new passphrase was echoed or logged" || ok "the new passphrase isn't echoed or logged"
printf '%s\n' "$P3" > "$T/p3"
[ "$($M cat "$BLANK" /fresh.txt --passphrase-file "$T/p3")" = "made in milkOS: $CANARY-3" ] && ok "Mac: the passphrase opens it, the file is there" || bad "Mac open with the passphrase"
[ -n "$REC4" ] && [ "$($M cat "$BLANK" /fresh.txt --recovery "$REC4")" = "made in milkOS: $CANARY-3" ] && ok "Mac: the shown recovery key opens it" || bad "Mac open with the recovery key"
$M check "$BLANK" --passphrase-file "$T/p3" > "$T/check4.txt" 2>&1 && ok "Mac: check clean" || { bad "check of the kernel-made volume"; cat "$T/check4.txt"; }
grep -aq "$CANARY-3" "$BLANK" && bad "plaintext on the kernel-made encrypted disk" || ok "no plaintext on the kernel-made disk"
fi

if want convert; then
PL="$T/inplace.img"; P5="fourth-fake-test-passphrase"
$M mkfs "$PL" --size 32M >/dev/null
echo "$CANARY-5 written on the Mac" > "$T/c5.txt"
head -c 6000000 /dev/urandom > "$T/r5.bin"
$M put "$PL" /docs/c5.txt "$T/c5.txt" >/dev/null && $M put "$PL" /docs/r5.bin "$T/r5.bin" >/dev/null
IMG="$PL" EXTRA_DISKS="" boot b5 - \
  "disks snapshot s5" \
  "raw:disks encrypt --no-recovery --rate 1" "wait:New passphrase" "raw:$P5" "wait:Again" "raw:$P5" "wait:started" "wait:msh:" \
  "text show /System/Kernel/fs/milkfs/info" "files copy --reflink /Volumes/Milk/docs/r5.bin /Volumes/Milk/docs/copy.bin" \
  "disks rollback s5" \
  "raw:disks encrypt --no-recovery" "wait:New passphrase" "raw:$P5" "wait:Again" "raw:$P5" "wait:msh:" \
  "print made while encrypting: $CANARY-6 > /Volumes/Milk/docs/during.txt" \
  "print rate=0 > /System/Kernel/fs/milkfs/encrypt" "pause 6" "text show /System/Kernel/fs/milkfs/encrypt" "text show /System/Kernel/fs/milkfs/info"
L="$(tr -d '\r' < "$T/b5.log")"
echo "$L" | grep -q "^encryption:  converting$" && echo "$L" | grep -Eq "^conversion:  encrypt [0-9]+% \([0-9]+/[0-9]+ blocks\), " \
  && ok "info while encrypting: converting, with progress ($(echo "$L" | sed -n 's/^conversion:  //p' | head -1))" || bad "info while converting"
echo "$L" | grep -aq "files: can't reflink .*busy\|files: .*EBUSY" && ok "a reflink copy is refused while converting" || bad "reflink during conversion: $(echo "$L" | grep -a '^files:' | head -1)"
echo "$L" | grep -aq "disks rollback s5: the volume is being converted" && ok "rollback is refused while converting" || bad "rollback during conversion"
echo "$L" | grep -aq "disks encrypt: it is being converted already (EBUSY" && ok "a second encrypt is refused (EBUSY)" || bad "second encrypt: $(echo "$L" | grep -a 'disks encrypt' | tail -1)"
echo "$L" | grep -aq "milkfs: vda (Milk) at /Volumes/Milk: encrypted in place, done\|milkfs: encryption done" && ok "it finished (in the background, the volume in use)" || bad "didn't finish: $(echo "$L" | grep -a 'convert\|in place' | tail -2)"
echo "$L" | grep "^encryption:  " | tail -1 | grep -q "^encryption:  on$" && ok "info afterwards: encryption on" || bad "info after"
printf '%s\n' "$P5" > "$T/p5"
[ "$($M cat "$PL" /docs/during.txt --passphrase-file "$T/p5")" = "made while encrypting: $CANARY-6" ] \
  && [ "$($M cat "$PL" /docs/c5.txt --passphrase-file "$T/p5")" = "$CANARY-5 written on the Mac" ] \
  && $M cat "$PL" /docs/r5.bin --passphrase-file "$T/p5" | cmp -s - "$T/r5.bin" \
  && ok "Mac: encrypted now, every file there (also the one written meanwhile)" || bad "Mac after in-place encryption"
$M check "$PL" --passphrase-file "$T/p5" > "$T/check5.txt" 2>&1 && grep -q "encryption: on" "$T/check5.txt" && ok "Mac: check clean, encryption on" || { bad "check after in-place encryption"; cat "$T/check5.txt"; }
grep -aq "$CANARY" "$PL" && bad "plaintext left on the disk after encrypting in place" || ok "no plaintext left on the disk (free space cleared too)"
MILKFS_PASSPHRASE= $M check "$PL" > /dev/null 2>&1 < /dev/null && bad "Mac opened it without a passphrase" || ok "Mac: no passphrase, no entry"

IMG="$PL" EXTRA_DISKS="" boot b6 - \
  "raw:disks unlock" "wait:Passphrase" "raw:$P5" "wait:unlocked" "wait:msh:" \
  "text show /Volumes/Milk/docs/during.txt" \
  "raw:disks decrypt --wait" "wait:Passphrase" "raw:$P5" "wait:decrypted in place" "wait:msh:" \
  "text show /System/Kernel/fs/milkfs/info"
L="$(tr -d '\r' < "$T/b6.log")"
echo "$L" | grep -aq "vda (Milk) is encrypted and locked" && ok "boot 6: locked at boot" || bad "boot 6 not locked"
echo "$L" | grep -aq "^made while encrypting: $CANARY-6" && ok "unlocked: the file is there" || bad "boot 6 file"
echo "$L" | grep -aq "disks: vda (Milk) at /Volumes/Milk: decrypted in place, done" && ok "disks decrypt --wait: done" || bad "decrypt: $(echo "$L" | grep -a 'disks' | tail -2)"
echo "$L" | awk '/\$ text show \/System\/Kernel\/fs\/milkfs\/info$/{on=1; next} on && /^msh:/{on=0} on' | grep -q "^encryption:  off$" && ok "info: encryption off" || bad "info after decrypt"
MILKFS_PASSPHRASE= $M check "$PL" > "$T/check6.txt" 2>&1 < /dev/null && grep -q "encryption: off" "$T/check6.txt" \
  && [ "$(MILKFS_PASSPHRASE= $M cat "$PL" /docs/during.txt < /dev/null)" = "made while encrypting: $CANARY-6" ] \
  && ok "Mac: plain again, readable with no passphrase, check clean" || { bad "Mac after decrypt"; cat "$T/check6.txt"; }
fi

echo
if [ $FAIL = 0 ]; then echo "enctest: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; else echo "enctest: FAIL ($T)"; fi
exit $FAIL