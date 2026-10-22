#!/usr/bin/env bash
# Run every milkOS test script, one after another, and print one table: name, PASS/FAIL/TIMEOUT,
# seconds, log path. Exits non-zero if anything failed.
#
#   scripts/check-all.sh                quick set: the checks that finish in a few minutes
#   scripts/check-all.sh --full         everything, with the long soak/crash/stress runs
#   scripts/check-all.sh --only REGEX   only tests whose name matches (egrep), e.g. --only 'net|pkg'
#   scripts/check-all.sh --list         show the tests and their time limits, run nothing
#   --keep                              keep every test's private work directory (default: only failures)
#
# Tests run serially: some boot two VMs, some bind 127.0.0.1 ports, some use qemu's
# image locking. Each gets its own directory under /tmp/milk-check-all.XXXXXX with private
# MILK_FS / MILK_DISK images (and MILK_CRASH_IMG, TMPDIR), so nothing touches /tmp/milk-*.img.
# net-test gets a free HTTP_PORT. Each test runs in its own process group (scripts/runlimit.py)
# with a time limit; on a timeout that group, and only that group, is killed.
# Tests that only measure (treetest, mounttime, cachebench, input-test) are "smoke": they pass
# if they exit 0, because they have no assertions of their own.
set -uo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

FULL=0; ONLY=""; LIST=0; KEEP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --full) FULL=1 ;;
    --only) ONLY="${2:?--only needs a pattern}"; shift ;;
    --list) LIST=1 ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '2,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "check-all: unknown option $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

# name | tier (q = quick set, f = full only) | time limit in seconds | command
# The quick set also runs in --full, usually with bigger numbers (serial-soak).
TESTS=(
  "serial-soak|q|900|scripts/serial-soak.sh 2 6"
  "net-test|q|600|scripts/net-test.sh"
  "pkg-test|q|600|scripts/pkg-test.sh"
  "svc-test|q|600|scripts/svc-test.sh"
  "driver-crash-test|q|600|scripts/driver-crash-test.sh @WORK@"
  "drop-test|q|600|scripts/drop-test.sh"
  "fscktest|q|600|scripts/fscktest.sh"
  "reflinktest|q|600|scripts/reflinktest.sh"
  "upgradetest|q|600|scripts/upgradetest.sh"
  "scrubtest|q|900|scripts/scrubtest.sh"
  "sendtest|q|600|scripts/sendtest.sh"
  "fattest|q|600|scripts/fattest.sh"
  "dftest|q|600|scripts/dftest.sh"
  "enctest|q|900|scripts/enctest.sh"
  "searchtest|q|900|scripts/searchtest.sh"
  "audio-test|q|600|scripts/audio-test.sh"
  "music-test|q|600|scripts/music-test.sh"
  "shot-test|q|600|scripts/shot-test.sh"
  "timekit-unit|q|300|scripts/timekit-test.sh"
  "sysview-unit|q|300|scripts/sysview-test.sh"
  "diskkit-unit|q|300|scripts/diskkit-test.sh"
  "alarm-test|q|600|scripts/alarm-test.sh"
  "apps-test|q|600|scripts/apps-test.sh"
  "disk-test|q|600|scripts/disk-test.sh"
  "wm-demo-tour|q|300|scripts/wm-demo.sh tour @WORK@/tour.png"
  "flicker-test|q|300|scripts/flicker-test.sh 150"
  "serial-soak-full|f|3600|scripts/serial-soak.sh 5 14"
  "crashtest|f|3600|scripts/crashtest.sh"
  "converttest|f|2400|scripts/converttest.sh"
  "stressloop|f|3600|scripts/stressloop.sh"
  "releasetest|f|1800|scripts/releasetest.sh"
  "treetest|f|1800|scripts/treetest.sh"
  "mounttime|f|900|scripts/mounttime.sh"
  "cachebench|f|900|scripts/cachebench.sh"
  "input-test|f|300|scripts/input-test.sh @WORK@/input.png"
)

selected=()
for t in "${TESTS[@]}"; do
  IFS='|' read -r name tier limit cmd <<<"$t"
  [ "$tier" = f ] && [ "$FULL" = 0 ] && continue
  [ -n "$ONLY" ] && ! echo "$name" | grep -Eq -- "$ONLY" && continue
  selected+=("$t")
done
[ ${#selected[@]} -gt 0 ] || { echo "check-all: no test matches" >&2; exit 2; }

if [ "$LIST" = 1 ]; then
  for t in "${selected[@]}"; do IFS='|' read -r name tier limit cmd <<<"$t"; printf '%-18s %5ss  %s\n' "$name" "$limit" "$cmd"; done
  exit 0
fi

ROOT="$(mktemp -d /tmp/milk-check-all.XXXXXX)"
echo "check-all: ${#selected[@]} tests, one at a time; logs and private images in $ROOT"
others=$(pgrep -f qemu-system-aarch64 | wc -l | tr -d ' ')
[ "$others" != 0 ] && echo "check-all: note, $others other qemu-system-aarch64 process(es) running (not touched; tests use private images, but timing is noisier)"

# One build up front so the first test's time isn't a compile, and a broken build stops here.
echo "check-all: cargo build (debug)"
cargo build -q || { echo "check-all: build failed"; exit 1; }

# A free TCP port on 127.0.0.1 for net-test.
free_port() { python3 -I -c "import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1])"; }

names=(); results=(); secs=(); logs=(); fails=0
for t in "${selected[@]}"; do
  IFS='|' read -r name tier limit cmd <<<"$t"
  W="$ROOT/$name"; mkdir -p "$W"
  cmd="${cmd//@WORK@/$W}"
  log="$W/output.log"
  printf '  %-18s ' "$name"
  start=$SECONDS
  env MILK_FS="$W/fs.img" MILK_DISK="$W/disk.img" MILK_CRASH_IMG="$W/crash.img" \
      HTTP_PORT="$(free_port)" TMPDIR="$W" \
      python3 -I scripts/runlimit.py "$limit" "$log" -- $cmd
  rc=$?
  took=$((SECONDS - start))
  case $rc in
    0)   res=PASS ;;
    124) res=TIMEOUT ;;
    *)   res="FAIL($rc)" ;;
  esac
  # A kernel panic in the serial log is a fail even if the script forgot to look.
  if [ "$res" = PASS ] && grep -rqa "KERNEL PANIC\|kernel panic" "$W" 2>/dev/null; then res="FAIL(panic)"; fi
  [ "$res" != PASS ] && fails=$((fails + 1))
  printf '%-12s %5ss\n' "$res" "$took"
  names+=("$name"); results+=("$res"); secs+=("$took"); logs+=("$W")
  if [ "$res" = PASS ] && [ "$KEEP" = 0 ]; then
    # keep the log, drop the (big, sparse) images
    rm -f "$W"/*.img
  fi
done

echo
printf '%-18s %-12s %6s  %s\n' TEST RESULT SECS LOGS
for i in "${!names[@]}"; do
  printf '%-18s %-12s %6s  %s\n' "${names[$i]}" "${results[$i]}" "${secs[$i]}" "${logs[$i]}/output.log"
done
echo
total=$SECONDS
if [ "$fails" = 0 ]; then echo "check-all: PASS, ${#names[@]} tests in ${total}s ($ROOT)"; exit 0; fi
echo "check-all: FAIL, $fails of ${#names[@]} failed in ${total}s ($ROOT)"
exit 1
