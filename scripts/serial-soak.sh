#!/usr/bin/env bash
# Serial console soak: N boots, each typing M short commands over the UART one second
# apart (`echo tickK`, a burst of bytes per line, the way a person or a script types).
# Every boot must print every tickK back, without the kernel's slow fallback poll ever
# having to rescue bytes (that would mean an RX interrupt was lost). Guards the PL011 RX path (lost RX interrupt)
# and the tty-uart feeder's wakeups.
#
#   scripts/serial-soak.sh [boots] [commands]      (default 5 14; MILK_SMP=1 for one CPU)
#
# Private images (MILK_FS / MILK_DISK default to /tmp/serial-soak-*.img). On a failed
# boot the UART registers and all CPUs' registers are dumped into its log via a qemu
# monitor socket. Only the qemu processes started here are killed (by PID).
set -uo pipefail
cd "$(dirname "$0")/.."
BOOTS="${1:-5}"
CMDS="${2:-14}"
export MILK_FS="${MILK_FS:-/tmp/serial-soak-fs.img}"
export MILK_DISK="${MILK_DISK:-/tmp/serial-soak-disk.img}"
SMP="${MILK_SMP:-4}"
LOGS="$(mktemp -d /tmp/serial-soak.XXXXXX)"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build -q || exit 1

pass=0
fail=0
for b in $(seq 1 "$BOOTS"); do
  log="$LOGS/boot$b.log"
  mon="$LOGS/mon$b.sock"
  : > "$log"
  # Typist: wait for the shell prompt, then one command a second.
  (
    for _ in $(seq 1 600); do
      grep -q 'msh:/' "$log" 2>/dev/null && break
      sleep 0.1
    done
    sleep 1
    for k in $(seq 1 "$CMDS"); do
      printf 'echo tick%s\n' "$k"
      sleep 1
    done
    sleep 30 # keep stdin open until we're killed
  ) | MILK_MONITOR="$mon" MILK_SMP="$SMP" scripts/run.sh headless > "$log" 2>&1 &
  qpid=$!
  # Wait for the last tick (or give up 20 s after the typist should be done).
  ok=0
  for _ in $(seq 1 $((600 + CMDS * 10 + 200))); do
    if tr -d '\r' < "$log" | grep -qx "tick$CMDS"; then
      ok=1
      break
    fi
    kill -0 "$qpid" 2>/dev/null || break
    sleep 0.1
  done
  got=$(tr -d '\r' < "$log" | grep -cx 'tick[0-9]*')
  # The kernel's 1 s fallback poll must never have been needed (it would hide a lost
  # RX interrupt): uart::poll prints this line when it rescues bytes.
  rescued=$(grep -c 'recovered by poll' "$log")
  if [ "$ok" = 1 ] && [ "$got" = "$CMDS" ] && [ "$rescued" = 0 ]; then
    pass=$((pass + 1))
    echo "boot $b: ok ($got/$CMDS)"
  else
    fail=$((fail + 1))
    echo "boot $b: FAIL ($got/$CMDS ticks, fallback rescues: $rescued; log $log)"
    if [ -S "$mon" ]; then
      {
        echo "==== monitor dump ===="
        # UARTFR (0x18), UARTRIS (0x3c), UARTMIS (0x40), UARTIMSC (0x38)
        printf 'xp /1wx 0x09000018\nxp /1wx 0x0900003c\nxp /1wx 0x09000040\nxp /1wx 0x09000038\ninfo registers -a\n' |
          nc -U -w 2 "$mon"
      } >> "$log" 2>&1
    fi
  fi
  kill "$qpid" 2>/dev/null
  wait "$qpid" 2>/dev/null
done
echo "serial-soak (smp $SMP): $pass passed, $fail failed. Logs: $LOGS"
[ "$fail" = 0 ]
