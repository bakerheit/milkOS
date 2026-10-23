#!/usr/bin/env bash
# Boot + serial console soak. N boots; each must
#   1. pass the boot self-tests (`[sched-test] PASS`, `[proc-test] PASS`: the scheduler
#      self-test is a good wakeup/IPI stress on its own),
#   2. reach the shell prompt,
#   3. echo back M short commands typed over the UART one second apart (`print tickK`, a
#      burst of bytes per line, the way a person or a script types),
# without the kernel's slow UART fallback poll ever having to rescue bytes (that would
# mean an RX interrupt was lost). Guards the PL011 RX path, the tty-uart feeder's
# wakeups and the scheduler's wakeups during boot.
#
#   scripts/serial-soak.sh [boots] [commands]   (default 5 14; 0 commands = boot tests only)
#   MILK_SMP=1 for one CPU, RELEASE=1 for the release kernel (as run.sh).
#   MILK_FEATURES=smp-join-delay: secondaries take 200 ms between coming online and
#   joining the scheduler, so the self-tests run while CPUs join (the window of the
#   release-boot crash: an IPI/tick on a CPU that hadn't joined jumped to PC 0).
#
# Private images (MILK_FS / MILK_DISK default to /tmp/serial-soak-*.img). On a failed
# boot the UART registers and every CPU's registers (symbolized) are appended to its log
# via a qemu monitor socket. Only the qemu processes started here are killed (by PID).
set -uo pipefail
cd "$(dirname "$0")/.."
BOOTS="${1:-5}"
CMDS="${2:-14}"
export MILK_FS="${MILK_FS:-/tmp/serial-soak-fs.img}"
export MILK_DISK="${MILK_DISK:-/tmp/serial-soak-disk.img}"
SMP="${MILK_SMP:-4}"
LOGS="$(mktemp -d /tmp/serial-soak.XXXXXX)"
PROFILE=debug
[ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build -q ${RELEASE:+--release} ${MILK_FEATURES:+--features "$MILK_FEATURES"} || exit 1

# has PATTERN: the log (CRs stripped) has a line matching the fixed string / regex.
has() { tr -d '\r' < "$log" | grep -q "$@"; }

# wait_for SECONDS grep-args...: poll the log until it matches; false on timeout/exit.
wait_for() {
  local secs=$1
  shift
  for _ in $(seq 1 $((secs * 10))); do
    has "$@" && return 0
    kill -0 "$qpid" 2>/dev/null || return 1
    sleep 0.1
  done
  return 1
}

dump() {
  [ -S "$mon" ] || return
  {
    echo "==== monitor dump ===="
    # UARTFR (0x18), UARTRIS (0x3c), UARTMIS (0x40), UARTIMSC (0x38)
    printf 'xp /1wx 0x09000018\nxp /1wx 0x0900003c\nxp /1wx 0x09000040\nxp /1wx 0x09000038\ninfo registers -a\n' |
      nc -U -w 2 "$mon"
  } >> "$log" 2>&1
  if command -v aarch64-elf-addr2line > /dev/null; then
    echo "==== CPU PCs ====" >> "$log"
    tr -d '\r' < "$log" | grep -o 'PC=[0-9a-f]*' | sed 's/PC=/0x/' |
      while read -r pc; do
        echo "$pc $(aarch64-elf-addr2line -fCe "$ELF" "$pc" | head -1)"
      done >> "$log"
  fi
}

pass=0
fail=0
for b in $(seq 1 "$BOOTS"); do
  log="$LOGS/boot$b.log"
  mon="$LOGS/mon$b.sock"
  : > "$log"
  # Typist: wait for the shell prompt, then one command a second.
  (
    for _ in $(seq 1 900); do
      grep -q 'msh:/' "$log" 2>/dev/null && break
      sleep 0.1
    done
    sleep 1
    for k in $(seq 1 "$CMDS"); do
      printf 'print tick%s\n' "$k"
      sleep 1
    done
    sleep 60 # keep stdin open until we're killed
  ) | MILK_OFFLINE=1 MILK_MONITOR="$mon" MILK_SMP="$SMP" scripts/run.sh headless > "$log" 2>&1 &
  qpid=$!
  stage=""
  if ! wait_for 60 -F '[sched-test] PASS'; then
    stage="sched-test"
  elif ! wait_for 60 -F '[proc-test] PASS'; then
    stage="proc-test"
  elif ! wait_for 60 -F 'msh:/'; then
    stage="shell prompt"
  elif [ "$CMDS" -gt 0 ] && ! wait_for $((CMDS * 2 + 20)) -x "tick$CMDS"; then
    stage="typed commands"
  fi
  got=$(tr -d '\r' < "$log" | grep -cx 'tick[0-9]*')
  # The kernel's 1 s fallback poll must never have been needed (it would hide a lost
  # RX interrupt): uart::poll prints this line when it rescues bytes.
  rescued=$(grep -c 'recovered by poll' "$log")
  bad=$(grep -c 'PANIC\|EXCEPTION' "$log")
  if [ -z "$stage" ] && [ "$got" = "$CMDS" ] && [ "$rescued" = 0 ] && [ "$bad" = 0 ]; then
    pass=$((pass + 1))
    echo "boot $b: ok (self-tests, $got/$CMDS commands)"
  else
    fail=$((fail + 1))
    echo "boot $b: FAIL (stuck at: ${stage:-none}; $got/$CMDS commands; fallback rescues $rescued; panics $bad; log $log)"
    dump
  fi
  kill "$qpid" 2>/dev/null
  wait "$qpid" 2>/dev/null
done
echo "serial-soak ($PROFILE${MILK_FEATURES:+ +$MILK_FEATURES}, smp $SMP, $BOOTS boots x $CMDS commands): $pass passed, $fail failed. Logs: $LOGS"
[ "$fail" = 0 ]
