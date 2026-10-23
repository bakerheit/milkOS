#!/usr/bin/env bash
# Boot the desktop on fresh PRIVATE disk images, type into it through QMP, save a PNG.
#   scripts/desk-shot.sh OUT.png [qmp.py commands...]
# e.g.  scripts/desk-shot.sh /tmp/x.png type 'open http://127.0.0.1/status' key ret sleep 3
# The commands are scripts/qmp.py's (type, key, move, click, sleep, screendump, ...); a
# final screendump to OUT.png is added. The Terminal window has the keyboard focus at boot.
# MILK_REPO=1: also build the toy package repo and serve it on 127.0.0.1:8765 (the guest's
# default /System/Settings/milk/repos URL, http://10.0.2.2:8765/) with an unrestricted user netdev;
# otherwise the netdev is restricted. The serial log goes next to OUT. Only the qemu and
# python processes started here are killed, by PID; a fresh image boots into the Welcome
# tour, which is closed first ("Get started") unless DESK_WELCOME=1; the images live in a mktemp dir that
# is removed at the end.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
OUT="${1:?usage: scripts/desk-shot.sh OUT.png [qmp.py commands...]}"
shift
LOG="${OUT%.png}.serial.log"
WORK="$(mktemp -d /tmp/milk-shot.XXXXXX)"
QPID=""
HPID=""
cleanup() {
  [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true
  [ -n "$HPID" ] && kill "$HPID" 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

[ "${MILK_PREBUILT:-}" = 1 ] || cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

python3 -I scripts/mkmilkfs.py mkfs "$WORK/fs.img" --size 64M --label Milk >/dev/null
dd if=/dev/zero of="$WORK/disk.img" bs=1m count=0 seek=64 2>/dev/null
# shellcheck disable=SC1091
source scripts/disk.sh
MILK_FS="$WORK/fs.img" MILK_DISK="$WORK/disk.img" milk_disk_args

NET=(-netdev user,id=n0,restrict=on -device virtio-net-device,netdev=n0)
if [ -n "${MILK_REPO:-}" ]; then
  scripts/milk-repo.sh build "$WORK/repo" >"$WORK/repo.log"
  (cd "$WORK" && exec python3 -I -m http.server --bind 127.0.0.1 --directory "$WORK/repo" 8765 >"$WORK/http.log" 2>&1) &
  HPID=$!
  NET=(-netdev user,id=n0 -device virtio-net-device,netdev=n0)
fi

rm -f "$OUT" "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -rtc base=localtime \
  -kernel "$KERNEL" -serial "file:$LOG" -device virtio-gpu-device -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  "${DISK_ARGS[@]}" "${NET[@]}" -qmp "unix:$WORK/qmp.sock,server,nowait" 2>"$WORK/qemu.err" &
QPID=$!
for _ in $(seq 200); do grep -q "wm: desktop up" "$LOG" 2>/dev/null && break; sleep 0.1; done
grep -q "wm: desktop up" "$LOG" || { tail -20 "$LOG" >&2; echo "desk-shot: desktop did not come up" >&2; exit 1; }
sleep 1.5
PRE=()
if [ -z "${DESK_WELCOME:-}" ]; then
  PRE=(move 785 615 click sleep 0.6)
fi
python3 -I scripts/qmp.py "$WORK/qmp.sock" "${PRE[@]}" "$@" screendump "$OUT"
[ -s "$OUT" ] || { echo "desk-shot: no screenshot" >&2; exit 1; }
echo "screenshot: $OUT"
echo "serial log: $LOG"
