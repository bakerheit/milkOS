#!/usr/bin/env bash
# TCP throughput, three paths, with scripts/../user/tcpbench:
#   guest -> host   alice's `tcpbench send` to a sink on the Mac's 127.0.0.1 (via slirp)
#   host -> guest   alice's `tcpbench recv` from a source on the Mac's 127.0.0.1
#   guest -> guest  alice -> bob over eth1 (qemu -netdev dgram on 127.0.0.1)
#   scripts/net-bench.sh [MIB]     (default 32 MiB per run; RELEASE=1 for a release kernel)
# Network safety: host servers bind 127.0.0.1 only and are started from the mktemp dir;
# the guests are told only 10.0.2.2 (the Mac's loopback through slirp) and each other.
# Only the qemu, driver and helper processes started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
MIB="${1:-32}"
WORK="$(mktemp -d /tmp/milk-net-bench.XXXXXX)"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "net-bench: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

(cd "$WORK" && exec python3 -I "$REPO/scripts/net_bench_host.py" "$WORK/ports" "$MIB" >"$WORK/host.log" 2>&1) &
PIDS+=("$!")
for _ in $(seq 50); do [ -s "$WORK/ports" ] && break; sleep 0.1; done
[ -s "$WORK/ports" ] || fail "host helper did not start"
eval "$(tr ' ' '\n' <"$WORK/ports" | sed 's/^/H_/')"
read -r PA PB < <(python3 -I -c "
import socket
ss = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(2)]
for s in ss: s.bind(('127.0.0.1', 0))
print(*[s.getsockname()[1] for s in ss])")

boot() { # name mac lport rport netdev
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
    -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M \
    -kernel "$KERNEL" -chardev "socket,id=s0,path=$WORK/$1.sock,server=on,wait=off" -serial chardev:s0 \
    -display none -global virtio-mmio.force-legacy=false \
    -netdev "$5" -device virtio-net-device,netdev=n0 \
    -netdev "dgram,id=d0,local.type=inet,local.host=127.0.0.1,local.port=$3,remote.type=inet,remote.host=127.0.0.1,remote.port=$4" \
    -device "virtio-net-device,netdev=d0,mac=$2" 2>>"$WORK/$1.qemu.err" &
}
boot alice 02:6d:6b:64:0a:0a "$PA" "$PB" "user,id=n0"
PIDS+=("$!")
boot bob 02:6d:6b:64:0b:0b "$PB" "$PA" "user,id=n0,restrict=on"
PIDS+=("$!")

P='wait:msh:[^\n]*\$ '
DONE='wait:tcpbench: [^\n]*(MiB/s|: )'
python3 -I scripts/serial_drive.py "$WORK/bob.sock" "$WORK/bob.log" \
  "$P" 'send:sleep 1' "$P" 'send:/System/Developer/tcpbench sink 5001' "done:$WORK/b1" "$DONE" "$P" \
  "done:$WORK/bob.done" >"$WORK/bob.drive" 2>&1 &
PIDS+=("$!")
python3 -I scripts/serial_drive.py "$WORK/alice.sock" "$WORK/alice.log" \
  "$P" 'send:sleep 1' "$P" \
  "send:/System/Developer/tcpbench send 10.0.2.2:$H_sink $MIB" "$DONE" "$P" \
  "send:/System/Developer/tcpbench recv 10.0.2.2:$H_source" "$DONE" "$P" \
  "waitfile:$WORK/b1" 'send:sleep 1' "$P" \
  "send:/System/Developer/tcpbench send 169.254.11.11:5001 $MIB" "$DONE" "$P" \
  'send:cat /proc/net/snmp' "$P" \
  "done:$WORK/alice.done" >"$WORK/alice.drive" 2>&1 &
PIDS+=("$!")
for _ in $(seq 1800); do
  [ -e "$WORK/alice.done" ] && [ -e "$WORK/bob.done" ] && break
  sleep 0.1
done
[ -e "$WORK/alice.done" ] && [ -e "$WORK/bob.done" ] || { tail -20 "$WORK/alice.log" >&2; fail "timed out"; }
A="$WORK/alice.txt"; B="$WORK/bob.txt"
python3 -I scripts/console_clean.py "$WORK/alice.log" >"$A"
python3 -I scripts/console_clean.py "$WORK/bob.log" >"$B"
rate() { grep -E "^tcpbench: $1 " "$2" | sed -n "${3:-1}p" | sed -E 's/.* = ([0-9.]+) MiB\/s.*/\1/'; }
UP=$(rate send "$A" 1); DOWN=$(rate recv "$A" 1); G2G=$(rate send "$A" 2)
[ -n "$UP" ] && [ -n "$DOWN" ] && [ -n "$G2G" ] || { grep tcpbench "$A" "$B"; fail "a run didn't finish"; }
grep -E "^(Tcp|Ip):" "$A" | tail -4
printf '%-16s %8s MiB/s\n' "guest -> host" "$UP" "host -> guest" "$DOWN" "guest -> guest" "$G2G"
printf 'net-bench: %s MiB per run, %s kernel, smp %s\n' "$MIB" "$PROFILE" "${MILK_SMP:-4}"
cleanup
trap - EXIT
rm -rf "$WORK"
