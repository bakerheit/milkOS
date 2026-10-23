#!/usr/bin/env bash
# Run the milkOS test scripts, several at a time, and print one table: name, PASS/FAIL/TIMEOUT,
# seconds, log path, then the total wall time. Exits non-zero if anything failed; the last line
# always says PASS or FAIL.
#
#   scripts/check-all.sh                default set: what gates a push (about 5 minutes at -j 4,
#                                       20 at -j 1)
#   scripts/check-all.sh --smoke        one test per area, for every change (under 2 minutes)
#   scripts/check-all.sh --full         everything, with the long soak/crash/stress runs
#   scripts/check-all.sh --only REGEX   only tests (of the chosen set) whose name matches (egrep)
#   scripts/check-all.sh --list         show the tests, their time limits and modes, run nothing
#   -j N, --jobs N                      run up to N tests at once (default 4; -j 1 = one at a time)
#   --keep                              keep every test's private work directory (default: only failures)
#
# Sets: --smoke ("s" in the table) is part of the default set ("s" + "q"), which is part of
# --full ("s" + "q" + "f"). Nothing leaves the default set to make the smoke tier.
#
# Isolation: each test gets its own directory under /tmp/milk-check-all.XXXXXX with private
# MILK_FS / MILK_DISK / MILK_CRASH_IMG images and TMPDIR, so nothing touches /tmp/milk-*.img and
# no two tests share an image (qemu's image locks never collide). Every 127.0.0.1 port a test
# uses is picked free at run time (net-test's HTTP_PORT comes from here; the others pick their
# own), the two-VM tests link their eth1s over their own pair of UDP ports (no multicast on the
# Mac, so one test's VMs never see another's), and serial / monitor / QMP sockets and
# screenshots live in each test's own mktemp directory. The Mac share is read-only except for
# the tests that write, each into its own subdirectory named after its PID.
# Each test runs in its own process group (scripts/runlimit.py) with a time limit; on a timeout,
# on Ctrl-C, or when it leaves something running, that group, and only that group, is stopped.
#
# Mode "solo": the test runs with nothing else going (the solo tests run first, one at a time,
# then the rest in parallel, longest first by the last run's times in target/check-all-times).
# A default-set test is solo only if it was seen to flake under parallel load. None is today
# (alarm-test's first command typed over a late boot message, and drop-test's mDNS announcement
# going out before the MLDv2 join of ff02::fb, were both fixed at the cause). The --full-only ones are solo because they
# haven't been checked under load (and the benchmarks measure).
# The build happens once, up front. Nothing a test runs may dirty the kernel build (the host unit
# tests build into target/host-tests for that reason), or the tests' own `cargo build`s would
# rebuild it under the tests already running.
# Tests that only measure (treetest, mounttime, cachebench, input-test) pass if they exit 0,
# because they have no assertions of their own.
# Internet guard: no test may boot with the internet on (docs/networking.md, "Internet"). Every
# test runs with MILK_CHECK_ALL=1 (scripts/netdev.sh then refuses `milk_net_args online`) and a
# qemu-system-aarch64 wrapper first in its PATH that refuses to start a VM whose fw_cfg
# opt/milkos/net says internet=on, unless the DNS is a local test server (dns=10.0.2.2:PORT, the
# Mac's 127.0.0.1: web-test). A test that trips either is FAIL(internet), and the run fails.
# The wrapper is checked once at the start (it must refuse an internet=on boot).
set -uo pipefail
# The whole script is one { } block, so bash reads all of it before running any: editing this file
# while a run is going doesn't make that run execute a mix of the old and new text.
{
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

TIER=q; ONLY=""; LIST=0; KEEP=0; JOBS=4
while [ $# -gt 0 ]; do
  case "$1" in
    --full) TIER=f ;;
    --smoke) TIER=s ;;
    --only) ONLY="${2:?--only needs a pattern}"; shift ;;
    --list) LIST=1 ;;
    --keep) KEEP=1 ;;
    -j|--jobs) JOBS="${2:?-j needs a number}"; shift ;;
    -j[0-9]*) JOBS="${1#-j}" ;;
    -h|--help) sed -n '2,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "check-all: unknown option $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done
case "$JOBS" in ''|*[!0-9]*|0) echo "check-all: -j wants a number of at least 1 (got $JOBS)" >&2; exit 2 ;; esac

# name | set (s = smoke, q = default, f = full only) | time limit in seconds | mode (par/solo) | command
# The default set also runs in --full, sometimes with bigger numbers (serial-soak).
TESTS=(
  "public-check|s|120|solo|scripts/public-check.sh"
  "serial-soak|q|900|par|scripts/serial-soak.sh 2 6"
  "net-test|s|600|par|scripts/net-test.sh"
  "web-test|q|600|par|scripts/web-test.sh @WORK@"
  "pkg-test|s|600|par|scripts/pkg-test.sh"
  "svc-test|s|600|par|scripts/svc-test.sh"
  "driver-crash-test|q|600|par|scripts/driver-crash-test.sh @WORK@"
  "drop-test|q|600|par|scripts/drop-test.sh"
  "fscktest|s|600|par|scripts/fscktest.sh"
  "reflinktest|q|600|par|scripts/reflinktest.sh"
  "upgradetest|q|600|par|scripts/upgradetest.sh"
  "scrubtest|q|900|par|scripts/scrubtest.sh"
  "sendtest|q|600|par|scripts/sendtest.sh"
  "fattest|q|600|par|scripts/fattest.sh"
  "dftest|q|600|par|scripts/dftest.sh"
  "enctest|q|900|par|scripts/enctest.sh"
  "searchtest|q|900|par|scripts/searchtest.sh"
  "audio-test|q|600|par|scripts/audio-test.sh"
  "music-test|q|600|par|scripts/music-test.sh"
  "shot-test|q|600|par|scripts/shot-test.sh"
  "timekit-unit|s|300|par|scripts/timekit-test.sh"
  "sysview-unit|s|300|par|scripts/sysview-test.sh"
  "diskkit-unit|s|300|par|scripts/diskkit-test.sh"
  "filekit-unit|s|300|par|scripts/filekit-test.sh"
  "textkit-unit|s|300|par|scripts/textkit-test.sh"
  "textbuf-unit|s|300|par|scripts/textbuf-test.sh"
  "heapkit-unit|s|300|par|scripts/heapkit-test.sh"
  "syskit-unit|s|300|par|scripts/syskit-test.sh"
  "cmdkit-unit|s|300|par|scripts/cmdkit-test.sh"
  "imgdec-unit|s|300|par|scripts/imgdec-test.sh"
  "milkfs-unit|s|300|par|scripts/milkfs-test.sh"
  "milkweb-unit|s|300|par|scripts/milkweb-test.sh"
  "files-test|q|600|par|scripts/files-test.sh"
  "systools-test|q|600|par|scripts/systools-test.sh"
  "net-disks-apps-test|q|600|par|scripts/net-disks-apps-test.sh"
  "text-test|q|600|par|scripts/text-test.sh"
  "alarm-test|q|600|par|scripts/alarm-test.sh"
  "apps-test|s|600|par|scripts/apps-test.sh"
  "texteditor-test|q|600|par|scripts/texteditor-test.sh"
  "preview-test|q|600|par|scripts/preview-test.sh"
  "disk-test|q|600|par|scripts/disk-test.sh"
  "wm-demo-tour|s|300|par|scripts/wm-demo.sh tour @WORK@/tour.png"
  "flicker-test|q|300|par|scripts/flicker-test.sh 150"
  "leak-test|q|600|par|scripts/leak-test.sh"
  "serial-soak-full|f|3600|solo|scripts/serial-soak.sh 5 14"
  "crashtest|f|3600|solo|scripts/crashtest.sh"
  "converttest|f|2400|solo|scripts/converttest.sh"
  "stressloop|f|3600|solo|scripts/stressloop.sh"
  "releasetest|f|1800|solo|scripts/releasetest.sh"
  "treetest|f|1800|solo|scripts/treetest.sh"
  "mounttime|f|900|solo|scripts/mounttime.sh"
  "cachebench|f|900|solo|scripts/cachebench.sh"
  "input-test|f|300|solo|scripts/input-test.sh @WORK@/input.png"
)

selected=()
for t in "${TESTS[@]}"; do
  IFS='|' read -r name tier limit mode cmd <<<"$t"
  case "$TIER$tier" in
    ss|qs|qq|f?) ;;
    *) continue ;;
  esac
  [ -n "$ONLY" ] && ! echo "$name" | grep -Eq -- "$ONLY" && continue
  selected+=("$t")
done
[ ${#selected[@]} -gt 0 ] || { echo "check-all: no test matches" >&2; exit 2; }

if [ "$LIST" = 1 ]; then
  for t in "${selected[@]}"; do
    IFS='|' read -r name tier limit mode cmd <<<"$t"
    printf '%-18s %-5s %-4s %5ss  %s\n' "$name" "$tier" "$mode" "$limit" "$cmd"
  done
  exit 0
fi

# Last run's seconds per test ("name secs" lines), for running the long ones first.
TIMES=target/check-all-times
last_secs() { awk -v n="$1" '$1 == n {s = $2} END {print s + 0}' "$TIMES" 2>/dev/null || echo 0; }

# Run order: solo tests first (table order), then the rest, longest first.
n=${#selected[@]}
order=()
for i in $(seq 0 $((n - 1))); do
  IFS='|' read -r name tier limit mode cmd <<<"${selected[$i]}"
  [ "$mode" = solo ] && order+=("$i")
done
while read -r _ i; do order+=("$i"); done < <(
  for i in $(seq 0 $((n - 1))); do
    IFS='|' read -r name tier limit mode cmd <<<"${selected[$i]}"
    [ "$mode" = solo ] && continue
    printf '%s %s\n' "$(last_secs "$name")" "$i"
  done | sort -s -k1,1nr)

ROOT="$(mktemp -d /tmp/milk-check-all.XXXXXX)"
echo "check-all: $n tests, up to $JOBS at a time; logs and private images in $ROOT"
# Count by process name (pgrep -f would also match shells whose command line mentions qemu).
others=$(pgrep -x qemu-system-aarch64 | wc -l | tr -d ' ')
[ "$others" != 0 ] && echo "check-all: note, $others other qemu-system-aarch64 process(es) running (not touched; tests use private images, but timing is noisier)"

# The internet guard: a qemu wrapper in $ROOT/.guard, first in every test's PATH (above).
GUARD="$ROOT/.guard"
mkdir -p "$GUARD"
REAL_QEMU="$(command -v qemu-system-aarch64 || true)"
[ -n "$REAL_QEMU" ] || { echo "check-all: FAIL, qemu-system-aarch64 isn't installed"; exit 1; }
cat >"$GUARD/qemu-system-aarch64" <<GUARDEOF
#!/bin/bash
# check-all's internet guard (scripts/check-all.sh): no test boots with the internet on.
for a in "\$@"; do
  case "\$a" in
    *name=opt/milkos/net,file=*) bad="\$a" ;;
    *name=opt/milkos/net,*internet=on*)
      case "\$a" in
        *" dns=10.0.2.2:"[0-9]*|*"=dns=10.0.2.2:"[0-9]*) ;; # a local test DNS server (web-test)
        *) bad="\$a" ;;
      esac ;;
  esac
done
if [ -n "\${bad:-}" ]; then
  msg="check-all: INTERNET GUARD: a test tried to boot with the internet on (-fw_cfg \$bad); tests stay offline"
  echo "\$msg" >&2
  echo "\$msg" >>"$GUARD/tripped"
  [ -n "\${TMPDIR:-}" ] && echo "\$msg" >>"\$TMPDIR/.internet-guard"
  exit 97
fi
exec "$REAL_QEMU" "\$@"
GUARDEOF
chmod 755 "$GUARD/qemu-system-aarch64"
if "$GUARD/qemu-system-aarch64" -fw_cfg "name=opt/milkos/net,string=internet=on" >/dev/null 2>&1 \
  || [ ! -s "$GUARD/tripped" ]; then
  echo "check-all: FAIL, the internet guard doesn't refuse an internet=on boot"; exit 1
fi
rm -f "$GUARD/tripped"
echo "check-all: internet guard on (qemu wrapper in $GUARD, MILK_CHECK_ALL=1)"

# One build up front so no test's time is a compile, a broken build stops here, and the tests'
# own `cargo build`s are no-ops (nothing in a test rebuilds the kernel under another test).
echo "check-all: cargo build (debug)"
cargo build -q || { echo; echo "check-all: FAIL, the build failed"; exit 1; }

# A free TCP port on 127.0.0.1 for net-test.
free_port() { python3 -I -c "import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1])"; }

# Per test (index into selected): pid of its runlimit.py, result, seconds.
pid=(); res=(); took=()
for i in $(seq 0 $((n - 1))); do pid[$i]=""; res[$i]=""; took[$i]=""; done
running=(); done_count=0; fails=0; WALL0=$SECONDS

start_test() { # index
  local i="$1" name tier limit mode cmd W
  IFS='|' read -r name tier limit mode cmd <<<"${selected[$i]}"
  W="$ROOT/$name"; mkdir -p "$W"
  cmd="${cmd//@WORK@/$W}"
  [ "$mode" = solo ] && echo "  ($name runs solo: nothing else running)"
  env MILK_FS="$W/fs.img" MILK_DISK="$W/disk.img" MILK_CRASH_IMG="$W/crash.img" \
      HTTP_PORT="$(free_port)" TMPDIR="$W" \
      MILK_CHECK_ALL=1 MILK_OFFLINE=1 MILK_NET_GUARD_DIR="$GUARD" PATH="$GUARD:$PATH" \
      python3 -I scripts/runlimit.py "$limit" "$W/output.log" --status "$W/.status" -- $cmd &
  pid[$i]=$!
  running+=("$i")
}

# A finished test: read its status, check for panics, print its line.
finish_test() { # index
  local i="$1" name tier limit mode cmd W rc secs r
  IFS='|' read -r name tier limit mode cmd <<<"${selected[$i]}"
  W="$ROOT/$name"
  wait "${pid[$i]}" 2>/dev/null
  rc=$?
  secs=""
  [ -s "$W/.status" ] && read -r rc secs <"$W/.status"
  [ -n "$secs" ] || secs="?"
  case $rc in
    0)   r=PASS ;;
    124) r=TIMEOUT ;;
    *)   r="FAIL($rc)" ;;
  esac
  # A kernel panic in the serial log is a fail even if the script forgot to look.
  if [ "$r" = PASS ] && grep -rqa "KERNEL PANIC\|kernel panic" "$W" 2>/dev/null; then r="FAIL(panic)"; fi
  # The internet guard tripped (the wrapper or netdev.sh): a fail, whatever the script said.
  if [ -s "$W/.internet-guard" ] || grep -qa "INTERNET GUARD" "$W/output.log" 2>/dev/null; then
    r="FAIL(internet)"
    grep -ah "INTERNET GUARD" "$W/.internet-guard" "$W/output.log" 2>/dev/null | head -2 | sed 's/^/    /'
  fi
  [ "$r" != PASS ] && fails=$((fails + 1))
  res[$i]="$r"; took[$i]="$secs"
  done_count=$((done_count + 1))
  printf '  %-18s %-12s %5ss   [%d/%d done, %ds wall]\n' "$name" "$r" "$secs" "$done_count" "$n" $((SECONDS - WALL0))
  if [ "$r" = PASS ] && [ "$KEEP" = 0 ]; then
    rm -f "$W"/*.img # keep the log, drop the (big, sparse) images
  fi
}

# Wait until at least one running test has finished; finish those.
reap() {
  local still i
  while :; do
    still=()
    for i in ${running[@]+"${running[@]}"}; do
      if [ -e "$ROOT/${selected[$i]%%|*}/.status" ] || ! kill -0 "${pid[$i]}" 2>/dev/null; then
        finish_test "$i"
      else
        still+=("$i")
      fi
    done
    if [ ${#still[@]} -lt ${#running[@]} ]; then running=(${still[@]+"${still[@]}"}); return; fi
    sleep 0.5
  done
}

# Ctrl-C / TERM: stop every running test's process group (runlimit.py does that on a signal),
# then report what finished.
INTERRUPTED=0
on_signal() {
  INTERRUPTED=1
  local i
  for i in ${running[@]+"${running[@]}"}; do kill -TERM "${pid[$i]}" 2>/dev/null; done
  for i in ${running[@]+"${running[@]}"}; do wait "${pid[$i]}" 2>/dev/null; done
}
trap on_signal INT TERM HUP

for i in "${order[@]}"; do
  [ "$INTERRUPTED" = 1 ] && break
  IFS='|' read -r name tier limit mode cmd <<<"${selected[$i]}"
  if [ "$mode" = solo ] || [ "$JOBS" = 1 ]; then
    while [ ${#running[@]} -gt 0 ] && [ "$INTERRUPTED" = 0 ]; do reap; done
    [ "$INTERRUPTED" = 1 ] && break
    start_test "$i"
    while [ ${#running[@]} -gt 0 ] && [ "$INTERRUPTED" = 0 ]; do reap; done
  else
    while [ ${#running[@]} -ge "$JOBS" ] && [ "$INTERRUPTED" = 0 ]; do reap; done
    [ "$INTERRUPTED" = 1 ] && break
    start_test "$i"
  fi
done
while [ ${#running[@]} -gt 0 ] && [ "$INTERRUPTED" = 0 ]; do reap; done
[ "$INTERRUPTED" = 1 ] && for i in ${running[@]+"${running[@]}"}; do finish_test "$i"; done
WALL=$((SECONDS - WALL0))

# Remember the passing tests' seconds for next time's order.
if [ "$INTERRUPTED" = 0 ]; then
  {
    for i in $(seq 0 $((n - 1))); do
      [ "${res[$i]}" = PASS ] && [ "${took[$i]}" != "?" ] && printf '%s %s\n' "${selected[$i]%%|*}" "${took[$i]}"
    done
    cat "$TIMES" 2>/dev/null || true
  } | awk '!seen[$1]++' >"$TIMES.new" 2>/dev/null && mv "$TIMES.new" "$TIMES"
fi

echo
printf '%-18s %-12s %6s  %s\n' TEST RESULT SECS LOGS
sum=0; ran=0
for i in $(seq 0 $((n - 1))); do
  name="${selected[$i]%%|*}"
  r="${res[$i]}"; s="${took[$i]}"
  [ -n "$r" ] || { r="NOT RUN"; s="-"; }
  [ "$r" = "NOT RUN" ] && [ "$INTERRUPTED" = 1 ] && fails=$((fails + 1))
  case "$s" in ''|*[!0-9]*) ;; *) sum=$((sum + s)); ran=$((ran + 1)) ;; esac
  printf '%-18s %-12s %6s  %s\n' "$name" "$r" "$s" "$ROOT/$name/output.log"
done
echo
echo "check-all: wall time ${WALL}s for $n tests at -j $JOBS (${sum}s of test time); total ${SECONDS}s with the build"
if [ "$INTERRUPTED" = 1 ]; then echo "check-all: FAIL, interrupted ($ROOT)"; exit 130; fi
if [ -s "$GUARD/tripped" ]; then
  echo "check-all: the INTERNET GUARD tripped (a test tried to boot with the internet on):"
  sed 's/^/  /' "$GUARD/tripped"
  [ "$fails" = 0 ] && fails=1
fi
if [ "$fails" = 0 ]; then echo "check-all: PASS, $n tests in ${WALL}s ($ROOT)"; exit 0; fi
echo "check-all: FAIL, $fails of $n failed in ${WALL}s ($ROOT)"
exit 1
}
