#!/usr/bin/env bash
# Stress the TCP stack's loss recovery: boot GUESTS (default 6) headless at once, each with only
# the network self-test plus its stress mode (fw_cfg opt/milkos/nettest):
#   lossy_rounds=ROUNDS  that many more lossy-loopback transfers after the usual ones, each with
#                        its own drop sequence, losing 1 in 7, 5 or 3 packets (data, ACKs, FIN)
#   jitter=MS            random stalls of up to MS ms with IRQs masked, inside the stack lock and
#                        in the net thread, as a vCPU the host deschedules stalls
# and checks each printed "[net] selftest ok" and the stress summary. Every self-test check runs
# under a watchdog that prints "[net] FAIL <check>: stalled ..." and a dump of the TCP
# connections, the net thread and the run queue when a check runs past 90 s, so a stall shows
# up as a FAIL with evidence, not as silence. Guests run in parallel on purpose: the host load
# is part of the test.
#
#   scripts/net-stress-test.sh [GUESTS [ROUNDS [JITTER_MS]]]      defaults 6 30 30
#
# Offline (restricted slirp, no port forwards), no disks; work files in a fresh mktemp dir,
# removed on a pass (MILK_KEEP=1 keeps them). Only the qemus started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
GUESTS="${1:-6}" ROUNDS="${2:-30}" JITTER="${3:-30}"
LIMIT="${MILK_STRESS_LIMIT:-600}" # seconds per guest
T="$(mktemp -d /tmp/milk-net-stress.XXXXXX)"
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
cp "$ELF.bin" "$T/kernel.bin" # the guests keep booting this one even if the tree is rebuilt
source scripts/netdev.sh; MILK_HTTP_PORT= milk_net_args
QPIDS=()
trap 'for p in ${QPIDS[@]+"${QPIDS[@]}"}; do kill -9 "$p" 2>/dev/null || true; done' EXIT
for g in $(seq "$GUESTS"); do
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -m 512M -kernel "$T/kernel.bin" \
    -display none -monitor none -serial "file:$T/guest$g.log" \
    -global virtio-mmio.force-legacy=false "${NET_ARGS[@]}" \
    -fw_cfg "name=opt/milkos/nettest,string=lossy3=1 lossy_rounds=$ROUNDS jitter=$JITTER" \
    -fw_cfg name=opt/milkos/selftest,string=net 2>/dev/null &
  QPIDS+=($!)
done
echo "net-stress: $GUESTS guests, $ROUNDS stress rounds each, jitter up to $JITTER ms (logs in $T)"
START=$SECONDS
DONE=()
while [ "${#DONE[@]}" -lt "$GUESTS" ] && [ $((SECONDS - START)) -lt "$LIMIT" ]; do
  for g in $(seq "$GUESTS"); do
    [ -n "${DONE[$g]:-}" ] && continue
    if grep -aq '\[net\] selftest' "$T/guest$g.log" 2>/dev/null; then DONE[$g]=$((SECONDS - START)); fi
    kill -0 "${QPIDS[$((g - 1))]}" 2>/dev/null || DONE[$g]="${DONE[$g]:-exit}"
  done
  sleep 1
done
for p in ${QPIDS[@]+"${QPIDS[@]}"}; do kill -9 "$p" 2>/dev/null || true; wait "$p" 2>/dev/null || true; done
QPIDS=()
FAIL=0
for g in $(seq "$GUESTS"); do
  L="$(tr -d '\r' <"$T/guest$g.log")"
  sum="$(echo "$L" | grep -a '^\[net\] tcp lossy stress:' | sed 's/^\[net\] tcp lossy stress: //' || true)"
  if echo "$L" | grep -aq '^\[net\] selftest ok' && [ -n "$sum" ]; then
    echo "  guest $g: ok in ${DONE[$g]} s: $sum"
  else
    FAIL=1
    if [ -z "${DONE[$g]:-}" ]; then
      echo "  guest $g: FAIL, silent: no self-test result in $LIMIT s"
    else
      echo "  guest $g: FAIL"
    fi
    echo "$L" | grep -a '^\[net\]' | grep -aEv '^\[net\] +(/proc|Inter|face)' | tail -25 | sed 's/^/    /'
  fi
done
if [ "$FAIL" = 0 ]; then
  echo "net-stress: PASS ($((SECONDS - START)) s)"
  [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true
else
  echo "net-stress: FAIL (logs in $T)"
  exit 1
fi
