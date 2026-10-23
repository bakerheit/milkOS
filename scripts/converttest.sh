#!/usr/bin/env bash
# Crash test for encrypting (and decrypting) a milkfs volume in place. Private images in a
# fresh /tmp directory; only the qemus started here are killed (by PID); the passphrase is
# a test value, obviously fake.
#   base:     a plain volume filled by the crash-writer workload (files, reflink copies,
#             deduped copies, snapshots, a promoted clone): its trees (live and every
#             snapshot) are recorded on the Mac.
#   encrypt:  kill rounds (SIGKILL at a random moment of a slowed-down conversion) and
#             power-cut rounds (the disk stops after N random sectors, the request in flight
#             torn). After every crash:
#               1. the Mac's check is clean (reading blocks in either form), and the trees
#                  are exactly the base's: a crash never changes what's in the volume;
#               2. a crash-check boot: every file in the live tree and the snapshots is
#                  complete, `check` is clean, space is exact after deleting the snapshots;
#               3. (on a copy) the conversion resumes and finishes: the volume is then
#                  encrypted, the Mac's check is clean, the trees are still the base's, and
#                  the raw image holds none of the workload's plaintext.
#   decrypt:  the same from a finished encrypted volume back to plain (kill and power-cut
#             rounds); at the end the Mac reads it without any passphrase.
#   scripts/converttest.sh [kill-rounds] [cut-rounds]   (default 3 3 each way)
set -uo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
KILLS="${1:-3}"
CUTS="${2:-3}"
T="$(mktemp -d /tmp/milk-converttest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
export MILKFS_PASSPHRASE="not-a-real-passphrase"
PASS=0
FAIL=0
MIXED=0  # crashes that left blocks in both forms (mid-tree, not after the rewriting)
ok() { echo "  ok: $*"; PASS=$((PASS + 1)); }
bad() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }
cargo build -q || exit 1
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"

QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT
boot() { # image mode log
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "file:$3" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$1,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -fw_cfg "name=opt/milkos/test,string=$2" -fw_cfg "name=opt/milkos/passphrase,string=$MILKFS_PASSPHRASE" 2>/dev/null &
  QPID=$!
}
stop() { kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""; }
wait_for() { # pattern log seconds
  for _ in $(seq $(($3 * 10))); do grep -aq "$1" "$2" 2>/dev/null && return 0; sleep 0.1; done
  return 1
}
snaps() { python3 - "$1" <<'PY'
import importlib.util, sys
spec = importlib.util.spec_from_file_location("m", "scripts/mkmilkfs.py")
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(" ".join(s["name"] for s in m.Image(sys.argv[1]).superblock()["snaps"]))
PY
}
trees() { # image dir: the live tree and every snapshot/clone's
  mkdir -p "$2"; $M tree "$1" > "$2/live"
  for s in $(snaps "$1"); do $M tree "$1" --snapshot "$s" > "$2/$s"; done
}
same_trees() { # dir-a dir-b
  [ "$(ls "$1")" = "$(ls "$2")" ] || return 1
  for f in "$1"/*; do cmp -s "$f" "$2/$(basename "$f")" || return 1; done
}
state() { $M check "$1" 2>&1 | grep -a "encryption:" | sed 's/^ *//'; }

echo "== base: a plain volume the crash writer filled"
BASE="$T/base.img"
$M mkfs "$BASE" --size 64M --label Milk >/dev/null
boot "$BASE" crash-writer "$T/writer.log"
wait_for "crash-writer: started" "$T/writer.log" 60 || bad "writer didn't start"
sleep 8; stop
$M check "$BASE" > "$T/base.check" 2>&1 && ok "base: check clean, $(grep -c 'snapshot ' "$T/base.check") snapshot(s)" || { bad "base check"; cat "$T/base.check"; }
trees "$BASE" "$T/base.trees"
WORDS=$(grep -ac "milkcrash i=" "$BASE")
[ "$WORDS" -gt 0 ] && ok "base: the workload's plaintext is on the disk ($WORDS hits)" || bad "no plaintext in the base?"

# round name image mode crashkind(kill|cut) target(encrypt|decrypt)
round() {
  local name="$1" img="$2" mode="$3" kind="$4" dir="$5" log="$T/$1.log"
  boot "$img" "$mode" "$log"
  if [ "$kind" = kill ]; then
    wait_for "convert: started\|convert: resuming" "$log" 60 || { bad "$name: didn't start"; stop; return; }
    sleep "$(python3 -c "import random; print(round(random.uniform(0.2, ${KILL_MAX:-9}), 2))")"
    stop
    if grep -aq "CONVERT DONE" "$log"; then echo "  ($name: finished before the kill)"; fi
  else
    wait_for "POWERCUT\|CONVERT DONE\|CONVERT FAIL" "$log" 120; sleep 0.3; stop
    grep -a "POWERCUT" "$log" | sed 's/^/    /'
  fi
  grep -a "CONVERT FAIL\|panic" "$log" && bad "$name: $(grep -a 'CONVERT FAIL\|panic' "$log" | head -1)"
  # 1. the Mac: clean, unchanged
  if $M check "$img" > "$T/$name.check" 2>&1; then ok "$name: Mac check clean ($(state "$img"))"; else bad "$name: Mac check"; cat "$T/$name.check"; fi
  grep -aq "read sealed" "$T/$name.check" && ! grep -aq " 0 block(s) read sealed\| 0 plain" "$T/$name.check" && MIXED=$((MIXED + 1))
  trees "$img" "$T/$name.trees"
  same_trees "$T/base.trees" "$T/$name.trees" && ok "$name: every tree is the base's" || { bad "$name: trees differ"; diff -r "$T/base.trees" "$T/$name.trees" | head -5; }
  # 3. on a copy: resume to the end
  cp "$img" "$img.resumed"
  boot "$img.resumed" "convert=$dir:0" "$T/$name-resume.log"
  wait_for "CONVERT DONE\|CONVERT FAIL" "$T/$name-resume.log" 180; sleep 0.3; stop
  grep -aq "CONVERT DONE" "$T/$name-resume.log" && ok "$name: resumed and finished" || bad "$name: resume: $(grep -a 'CONVERT\|milkfs: convert' "$T/$name-resume.log" | tail -2)"
  local want=on; [ "$dir" = decrypt ] && want=off
  if [ "$dir" = decrypt ]; then
    MILKFS_PASSPHRASE= $M check "$img.resumed" > "$T/$name-done.check" 2>&1 && grep -q "encryption: $want" "$T/$name-done.check" \
      && ok "$name: now plain: the Mac reads it with no passphrase, check clean" || { bad "$name: after decrypting"; cat "$T/$name-done.check"; }
  else
    $M check "$img.resumed" > "$T/$name-done.check" 2>&1 && grep -q "encryption: $want" "$T/$name-done.check" \
      && ok "$name: now encrypted, check clean" || { bad "$name: after encrypting"; cat "$T/$name-done.check"; }
    [ "$(grep -ac "milkcrash i=" "$img.resumed")" = 0 ] && ok "$name: no plaintext left on the disk" || bad "$name: plaintext left ($(grep -ac "milkcrash i=" "$img.resumed") hits)"
  fi
  trees "$img.resumed" "$T/$name-done.trees"
  same_trees "$T/base.trees" "$T/$name-done.trees" && ok "$name: and still the same trees" || bad "$name: trees changed by finishing"
  # 2. the crash-check boot on what the crash left
  boot "$img" crash-check "$T/$name-check.log"
  wait_for "CRASHCHECK" "$T/$name-check.log" 120; sleep 0.3; stop
  grep -aq "CRASHCHECK PASS" "$T/$name-check.log" && ok "$name: crash-check passes" || bad "$name: $(grep -a 'CRASHCHECK' "$T/$name-check.log" | tail -3)"
}

echo "== encrypt"
for i in $(seq "$KILLS"); do
  cp "$BASE" "$T/ek$i.img"
  round "encrypt-kill-$i" "$T/ek$i.img" "convert=encrypt:256" kill encrypt
done
for i in $(seq "$CUTS"); do
  cp "$BASE" "$T/ec$i.img"
  N=$(python3 -c "import random; print(random.randint(50, 9000))")
  echo "  (power cut after $N sectors)"
  round "encrypt-cut-$i" "$T/ec$i.img" "convert-cut=encrypt:$N" cut encrypt
done

echo "== decrypt"
ENC="$T/encrypted.img"
cp "$BASE" "$ENC"
boot "$ENC" "convert=encrypt:0" "$T/full-encrypt.log"
wait_for "CONVERT DONE\|CONVERT FAIL" "$T/full-encrypt.log" 180; sleep 0.3; stop
grep -aq "CONVERT DONE" "$T/full-encrypt.log" && ok "a fully encrypted copy to start from" || bad "full encrypt"
for i in $(seq "$KILLS"); do
  cp "$ENC" "$T/dk$i.img"
  round "decrypt-kill-$i" "$T/dk$i.img" "convert=decrypt:256" kill decrypt
done
for i in $(seq "$CUTS"); do
  cp "$ENC" "$T/dc$i.img"
  N=$(python3 -c "import random; print(random.randint(50, 6000))")
  echo "  (power cut after $N sectors)"
  round "decrypt-cut-$i" "$T/dc$i.img" "convert-cut=decrypt:$N" cut decrypt
done
[ "$MIXED" -gt 0 ] && ok "$MIXED crash(es) hit in the middle of the rewriting (blocks in both forms)" || bad "no crash landed mid-way (timing?)"
echo
echo "converttest: $PASS passed, $FAIL failed ($T)"
[ "$FAIL" = 0 ] || exit 1
[ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true
