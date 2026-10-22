#!/usr/bin/env bash
# milkfs check/repair test: damage private images on purpose, repair on the host and in
# the kernel, verify. Everything in a fresh /tmp directory; only qemus started here are
# killed (by PID).
#   repairable kinds:   orphan dangling nlink parent sb smap bref
#   unrepairable kinds: data node (must be reported, never "repaired")
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-fscktest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }

cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

boot() { # image mode log [sock]
  local fw=() ser=(-serial "file:$3")
  [ "$2" != normal ] && fw=(-fw_cfg "name=opt/milkos/test,string=$2")
  [ -n "${4:-}" ] && ser=(-serial "unix:$4,server,nowait")
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M \
    -kernel "$ELF.bin" -display none -monitor none "${ser[@]}" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$1,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    ${fw[@]+"${fw[@]}"} 2>/dev/null &
  QPID=$!
}
stop() { kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""; }
wait_for() { for _ in $(seq $(($3 * 10))); do grep -aq "$1" "$2" 2>/dev/null && return 0; sleep 0.1; done; return 1; }

echo "== base image: a few seconds of the crash workload (files + snapshots)"
BASE="$T/base.img"
$M mkfs "$BASE" --size 64M >/dev/null
boot "$BASE" crash-writer "$T/base.log"
wait_for "crash-writer: iteration" "$T/base.log" 60 && sleep 2
stop
$M check "$BASE" --quiet >/dev/null && ok "base image is clean" || { bad "base image"; exit 1; }

for kind in orphan dangling nlink parent sb smap bref; do
  echo "== $kind"
  # host repair
  H="$T/$kind-host.img"; cp "$BASE" "$H"
  $M corrupt "$H" "$kind" >/dev/null
  if [ "$kind" = sb ]; then
    # A torn slot is what an interrupted commit leaves: a note, not a failed check.
    { $M check "$H" >"$T/$kind-host.check1" 2>&1 && grep -q "note: superblock slot" "$T/$kind-host.check1"; } \
      && ok "host check notes it: $(grep -m1 'note:' "$T/$kind-host.check1" | sed 's/^ *//')" || bad "host check of a torn slot"
  elif $M check "$H" >"$T/$kind-host.check1" 2>&1; then bad "host check missed $kind"; else
    ok "host check finds it: $(grep -m1 PROBLEM "$T/$kind-host.check1" | sed 's/^ *//')"; fi
  $M check "$H" --repair >"$T/$kind-host.repair" 2>&1 && ok "host repair: $(grep -m1 repaired: "$T/$kind-host.repair" | sed 's/^ *//')" || bad "host repair of $kind"
  { $M check "$H" >"$T/$kind-host.check2" 2>&1 && ! grep -q "note:" "$T/$kind-host.check2"; } && ok "host check clean after repair" || bad "host check after repair"
  boot "$H" crash-check "$T/$kind-host.boot"
  wait_for "CRASHCHECK" "$T/$kind-host.boot" 60 || true
  stop
  grep -aq "CRASHCHECK PASS" "$T/$kind-host.boot" && ok "kernel mounts it and all files verify" || { bad "kernel after host repair"; grep -a "CRASHCHECK\|milkfs" "$T/$kind-host.boot" | head -5; }
  # kernel repair
  K="$T/$kind-kernel.img"; cp "$BASE" "$K"
  $M corrupt "$K" "$kind" >/dev/null
  SOCK="$T/$kind.sock"; LOG="$T/$kind-kernel.log"
  boot "$K" normal "$LOG" "$SOCK"
  P='msh:[^\n]*\$ '
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" \
    "send:disks check" "wait:$P" "send:disks check --repair" "wait:$P" \
    "send:disks check" "wait:$P" "send:echo FSCK""-DONE" "wait:FSCK-DONE" "done:$T/$kind.done" &
  DPID=$!
  for _ in $(seq 600); do [ -e "$T/$kind.done" ] && break; sleep 0.2; done
  sleep 0.5; stop; kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
  L="$(tr -d '\r' < "$LOG")"
  if [ "$kind" = smap ]; then
    echo "$L" | grep -aq "vda: space map chunk unreadable: rebuilding" && ok "mount notices the damaged space map and rebuilds it" || bad "mount didn't notice the space map"
  elif [ "$kind" = bref ]; then
    # Mount doesn't count references; `disks check` does.
    echo "$L" | awk '/disks check$/{n++} n==1' | grep -aq "shared-block table" && ok "kernel check finds it (mount leaves it alone)" || bad "kernel check missed $kind"
  elif [ "$kind" != sb ]; then
    echo "$L" | grep -aq "check FOUND PROBLEMS" && ok "mount reports it (and doesn't repair)" || bad "mount didn't report $kind"
  fi
  if echo "$L" | grep -a -A3 'disks check --repair$' | grep -aq "repaired:"; then
    ok "kernel repair: $(echo "$L" | grep -a -m1 'repaired:' | sed 's/^ *//')"
  elif [ "$kind" = sb ] && echo "$L" | awk '/disks check$/{n++} n==1' | grep -aq "checksummed: clean"; then
    ok "kernel: the damaged slot is where the next commit goes, so the boot's first commit already rewrote it"
  elif [ "$kind" = smap ] && echo "$L" | awk '/disks check$/{n++} n==1' | grep -aq "checksummed: clean"; then
    ok "kernel: mount rebuilt the map and the next commit wrote a good one"
  else
    bad "kernel repair of $kind"
  fi
  echo "$L" | awk '/disks check$/{n++} n==2' | grep -aq "checksummed: clean" && ok "kernel check clean after repair" || bad "kernel check after repair"
  $M check "$K" >/dev/null 2>&1 && ok "host check clean after kernel repair" || bad "host check after kernel repair"
done

for kind in data node; do
  echo "== $kind (not repairable)"
  H="$T/$kind.img"; cp "$BASE" "$H"
  $M corrupt "$H" "$kind" >/dev/null
  $M check "$H" >/dev/null 2>&1 && bad "host check missed $kind" || ok "host check finds it"
  cp "$H" "$T/$kind.before"
  rc=0; $M check "$H" --repair >"$T/$kind.repair" 2>&1 || rc=$?
  { [ $rc = 2 ] && grep -q "cannot repair" "$T/$kind.repair"; } && ok "host repair refuses: $(grep -m1 'cannot repair' "$T/$kind.repair" | sed 's/^ *//')" || bad "host repair of $kind (rc $rc)"
  cmp -s "$H" "$T/$kind.before" && ok "image left as it was" || bad "repair changed a $kind-damaged image"
done

echo
[ $FAIL = 0 ] && echo "fscktest: PASS ($T)" || echo "fscktest: FAIL ($T)"
exit $FAIL
