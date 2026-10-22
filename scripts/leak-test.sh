#!/usr/bin/env bash
# Memory leak test for long-running user processes: boot with a desktop on private images, open
# Activity Monitor, start `top -b -d 1` in the background, then keep milkd and alarmd busy (svc
# and alarm commands) for SECS seconds. `ps` is read after a warm-up and again at the end; the
# resident size of top, milkd, alarmd and activity must not grow by more than LIMIT_KB (the
# allocator returns freed memory, so a steady loop stays flat; plus an eighth of the process's
# size, for the batching of what it gives back). Every other process's growth is
# printed too, for information. About SECS + 40 seconds.
#   scripts/leak-test.sh [SECS] [LIMIT_KB]      (defaults 60 and 128)
# Only the qemu started here is killed (by PID). Work files: a fresh mktemp dir.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
SECS="${1:-60}"
LIMIT_KB="${2:-128}"
T="$(mktemp -d /tmp/milk-leaktest.XXXXXX)"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
export MILK_FS="${MILK_FS:-$T/fs.img}" MILK_DISK="${MILK_DISK:-$T/disk.img}"
source scripts/disk.sh
milk_disk_args
source scripts/netdev.sh
MILK_HTTP_PORT= milk_net_args
SOCK="$T/s.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
  -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -serial "unix:$SOCK,server,nowait" -device virtio-gpu-device -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  "${DISK_ARGS[@]}" "${NET_ARGS[@]}" >/dev/null 2>&1 &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
n=0
ser() { # steps...: run console steps, wait for them to finish, disconnect
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/ser.$n" &
  local d=$!
  for _ in $(seq 1500); do [ -e "$T/ser.$n" ] && break; sleep 0.2; done
  kill "$d" 2>/dev/null || true; wait "$d" 2>/dev/null || true
  [ -e "$T/ser.$n" ] || { echo "leak-test: the console stopped answering (step $n)"; tail -20 "$LOG"; exit 1; }
}
# `ps` output as "name rssK" lines (the first line of each PID: the process, not its other
# threads), from the part of the log between markers $1 and $2.
psrss() {
  tr -d '\r' < "$LOG" | awk -v a="$1" -v b="$2" '$0 ~ a {on=1; next} $0 ~ b {on=0} on && $1 ~ /^[0-9]+$/ && !seen[$1]++ {print $NF, $(NF-1)}' | sed 's/K$//'
}

ser "wait:$P" "send:sleep 1" "wait:$P" "send:open activity" "wait:$P"
ser "send:top -b -n $((SECS + 60)) -d 1 > /dev/null &" "wait:$P" "send:sleep 10" "wait:$P"
ser "send:echo LEAK""-A" "wait:$P" "send:ps" "wait:$P" "send:echo LEAK""-A-END" "wait:$P"
# Keep milkd and alarmd answering while top and Activity Monitor refresh every second.
t0=$SECONDS
while [ $((SECONDS - t0)) -lt "$SECS" ]; do
  ser "send:svc list > /dev/null" "wait:$P" "send:svc status alarmd > /dev/null" "wait:$P" \
    "send:alarm list > /dev/null" "wait:$P" "send:sleep 4" "wait:$P"
done
ser "send:echo LEAK""-B" "wait:$P" "send:ps" "wait:$P" "send:echo LEAK""-B-END" "wait:$P"
psrss '^LEAK-A$' '^LEAK-A-END$' | sort > "$T/a.txt"
psrss '^LEAK-B$' '^LEAK-B-END$' | sort > "$T/b.txt"
echo "  resident size after the warm-up and $((SECONDS - t0)) s later (KiB):"
join "$T/a.txt" "$T/b.txt" | awk '{printf "    %-12s %7d -> %7d  (%+d)\n", $1, $2, $3, $3 - $2}'
for p in top milkd alarmd activity; do
  a=$(awk -v p="$p" '$1 == p {print $2; exit}' "$T/a.txt")
  b=$(awk -v p="$p" '$1 == p {print $2; exit}' "$T/b.txt")
  if [ -z "$a" ] || [ -z "$b" ]; then bad "$p was not running at both samples"; continue; fi
  # Free memory goes back to the kernel in batches (after 256 KiB of frees), so a process with
  # a big per-frame churn wobbles by a few hundred KiB: allow an eighth of its size on top.
  lim=$((LIMIT_KB + a / 8))
  if [ $((b - a)) -le "$lim" ]; then ok "$p stayed flat (${a}K -> ${b}K, limit +${lim}K)"; else bad "$p grew from ${a}K to ${b}K in $((SECONDS - t0)) s (limit +${lim}K)"; fi
done
grep -aqi "kernel panic" "$LOG" && bad "a kernel panic in the serial log" || true
if [ "$FAIL" = 0 ]; then echo "leak-test: PASS"; rm -rf "$T"; else echo "leak-test: FAIL (log in $T)"; exit 1; fi
