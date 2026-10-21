#!/usr/bin/env bash
# milkfs crash test. Formats a fresh image in /tmp, then repeatedly:
#   kill rounds:     boot the writer workload, SIGKILL qemu at a random moment
#   powercut rounds: boot the writer with an in-kernel power cut after N random sectors
#                    (the request in flight is torn, possibly a superblock), the kernel halts
# and after every crash:
#   1. scripts/mkmilkfs.py check on the raw image (host fsck: superblock, every checksum,
#      namespace)
#   2. a crash-check boot: mounts, verifies every file in the live tree and all snapshots
#      is complete and self-consistent, deletes the snapshots, checks space accounting;
#      for power cuts also that the mounted generation is the last committed one or the
#      one that was in flight.
#
#   superblock rounds: cut during a superblock write after T of its 8 sectors
#                    (T=1..7 torn: must mount the previous generation; T=8 landed but the
#                    commit never returned: must mount the new one)
#
#   scripts/crashtest.sh [kill-rounds] [powercut-rounds] [superblock-rounds]  (default 4 4 4)
# Only qemu processes started here are killed (by PID). Images and logs go to /tmp.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
KILLS="${1:-4}"
CUTS="${2:-4}"
SBCUTS="${3:-4}"
IMG="${MILK_CRASH_IMG:-/tmp/milk-crash.img}"
LOGS="$(mktemp -d /tmp/milk-crash.XXXXXX)"

cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
source scripts/enc.sh
milk_enc_setup "$LOGS"
python3 scripts/mkmilkfs.py mkfs "$IMG" --size 64M --label Milk --force ${ENC_MKFS[@]+"${ENC_MKFS[@]}"} >/dev/null

QPID=""
cleanup() { [ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true; }
trap cleanup EXIT

boot() { # mode log
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "file:$2" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -fw_cfg "name=opt/milkos/test,string=$1" ${ENC_QEMU[@]+"${ENC_QEMU[@]}"} 2>/dev/null &
  QPID=$!
}
stop() { kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""; }
wait_for() { # pattern log seconds
  local n=$(($3 * 10))
  for _ in $(seq "$n"); do
    grep -q "$1" "$2" 2>/dev/null && return 0
    sleep 0.1
  done
  return 1
}

PASS=0
FAIL=0
verify() { # round-name [expected-committed-generation] [exact]
  local name="$1" want="${2:-}" exact="${3:-}"
  local host="$LOGS/$name.host.txt" klog="$LOGS/$name.check.log"
  local ok=1
  if ! python3 scripts/mkmilkfs.py check "$IMG" --quiet >"$host" 2>&1; then
    ok=0
    echo "  host check FAILED:"; sed 's/^/    /' "$host"
  fi
  boot crash-check "$klog"
  if ! wait_for "CRASHCHECK" "$klog" 60; then
    ok=0
    echo "  check boot gave no verdict"; tail -5 "$klog" | sed 's/^/    /'
  fi
  stop
  local line
  line="$(grep -a "CRASHCHECK" "$klog" | tail -1 || true)"
  grep -a "^milkfs: .* mounted" "$klog" | sed 's/^/  /' || true
  echo "  $line"
  case "$line" in *"CRASHCHECK PASS"*) ;; *) ok=0 ;; esac
  if [ -n "$want" ]; then
    local got
    got="$(echo "$line" | sed -n 's/.*mounted generation \([0-9]*\).*/\1/p')"
    if [ -n "$exact" ] && [ "$got" != "$exact" ]; then
      ok=0
      echo "  mounted generation $got, expected exactly $exact"
    elif [ "$got" != "$want" ] && [ "$got" != "$((want + 1))" ]; then
      ok=0
      echo "  mounted generation $got, expected $want (last committed) or $((want + 1)) (in flight)"
    else
      echo "  generation $got: $([ "$got" = "$want" ] && echo "the last committed one (in-flight commit lost)" || echo "the in-flight commit had landed")"
    fi
  fi
  if [ $ok = 1 ]; then PASS=$((PASS + 1)); echo "  => PASS"; else FAIL=$((FAIL + 1)); echo "  => FAIL"; fi
}

for ((r = 1; r <= KILLS; r++)); do
  log="$LOGS/kill-$r.log"
  boot crash-writer "$log"
  wait_for "crash-writer: started" "$log" 60 || { echo "writer did not start"; tail "$log"; exit 1; }
  delay="$((RANDOM % 2500 + 300))"
  sleep "$((delay / 1000)).$(printf '%03d' $((delay % 1000)))"
  stop
  echo "kill round $r: SIGKILL after $delay ms of writing ($(grep -ac 'crash-writer: iteration' "$log") progress lines)"
  verify "kill-$r"
done

for ((r = 1; r <= CUTS; r++)); do
  log="$LOGS/cut-$r.log"
  n="$((RANDOM % 4000 + 1))"
  boot "powercut=$n" "$log"
  if ! wait_for "POWERCUT" "$log" 90; then
    echo "powercut round $r: no power cut seen"; tail -5 "$log"; stop; FAIL=$((FAIL + 1)); continue
  fi
  stop
  cut="$(grep -a POWERCUT "$log" | tail -1)"
  echo "powercut round $r (budget $n sectors): $cut"
  want="$(echo "$cut" | sed -n 's/.*last committed generation \([0-9]*\).*/\1/p')"
  verify "cut-$r" "$want"
done

TEARS=(8 1 4 7 0 8 3 6)
for ((r = 1; r <= SBCUTS; r++)); do
  log="$LOGS/sb-$r.log"
  k="$((RANDOM % 40 + 2))"
  t="${TEARS[$(((r - 1) % ${#TEARS[@]}))]}"
  boot "powercut-sb=$k:$t" "$log"
  if ! wait_for "POWERCUT" "$log" 90; then
    echo "superblock round $r: no power cut seen"; tail -5 "$log"; stop; FAIL=$((FAIL + 1)); continue
  fi
  stop
  cut="$(grep -a POWERCUT "$log" | tail -1)"
  want="$(echo "$cut" | sed -n 's/.*last committed generation \([0-9]*\).*/\1/p')"
  exact="$want"
  [ "$t" = 8 ] && exact="$((want + 1))"
  echo "superblock round $r (superblock write $k, $t/8 sectors land): $cut"
  verify "sb-$r" "$want" "$exact"
done

echo
echo "crashtest: $PASS passed, $FAIL failed ($KILLS kill, $CUTS power-cut, $SBCUTS superblock rounds). Logs: $LOGS"
python3 scripts/mkmilkfs.py check "$IMG"
[ "$FAIL" = 0 ]
