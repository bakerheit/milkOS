#!/usr/bin/env bash
# Services and the journal, on a private milkfs image, two boots:
#   boot 1: `utest svc` (a crashing unit restarted with doubling backoff, failed
#           dependencies reported, start/stop over /System/Services/Sockets/milkd.sock allowed for root and
#           admins and refused for others), httpd running as a milkd service, `svc`
#           tables; `utest auth` (admin tickets: refused without one, granted with the
#           documented test password `milk`, not inherited, revocable, kept across exec,
#           rate limited, audited); a marker written to the journal, flushed to disk;
#   boot 2: the marker is still in `log` (from the previous boot's file), not in `log -b`.
# Fresh /tmp directory; only the qemus started here are killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-svc.XXXXXX)"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/fs.img"
python3 scripts/mkmilkfs.py mkfs "$IMG" --size 64M --label Milk >/dev/null
MARK="journal-marker-$$-$RANDOM"
P='msh:[^\n]*\$ '

boot() { # name steps...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" "$@" "send:print SVC""-DONE" "wait:SVC-DONE" "done:$T/$name.done" &
  DPID=$!
  for _ in $(seq 900); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
  tr -d '\r' < "$LOG"
}
trap 'kill -9 "${QPID:-}" 2>/dev/null || true' EXIT

L1="$(boot b1 \
  "send:/System/Developer/utest svc" "wait:$P" \
  "send:/System/Developer/utest auth" "wait:$P" \
  "send:svc" "wait:$P" \
  "send:MILK_TABLE=1 svc list" "wait:$P" \
  "send:print $MARK > /dev/journal" "wait:$P" \
  "send:pause 3" "wait:$P" \
  "send:print now > /System/Kernel/fs/milkfs/sync" "wait:$P")"
echo "$L1" > "$T/b1.txt"
echo "$L1" | grep -q "utest: PASS services" && ok "$(echo "$L1" | grep -m1 'utest: PASS services' | cut -c1-200)" || bad "utest svc"
echo "$L1" | grep -q "utest: PASS authorisation" && ok "$(echo "$L1" | grep -m1 '(as milk) utest: PASS authorisation' | cut -c1-200)" || bad "utest auth"
echo "$L1" | grep -Eq "^httpd +running" && ok "httpd runs as a milkd service" || bad "httpd not running under milkd"
echo "$L1" | grep -q "^unit:str	state:str" && ok "svc list as a table" || bad "svc table"

L2="$(boot b2 \
  "send:log | text find $MARK" "wait:$P" \
  "send:log -b | text find --count $MARK" "wait:$P")"
echo "$L2" > "$T/b2.txt"
echo "$L2" | grep -q "user/echo\[[0-9]*\]: $MARK" && ok "the journal survived a reboot: $(echo "$L2" | grep -m1 "user/echo.*$MARK")" || bad "marker not in the journal after reboot"
echo "$L2" | grep -A1 "log -b | text find --count" | grep -qx 0 && ok "log -b shows only this boot" || bad "log -b"
[ "$FAIL" = 0 ] && echo "svc-test: PASS ($T)" || { echo "svc-test: FAIL ($T)"; exit 1; }
