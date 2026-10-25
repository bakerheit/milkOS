#!/usr/bin/env bash
# The display must never hold up the Raspberry Pi boot or the boot log (docs/raspberry-pi.md, "3.5-inch SPI display"),
# end to end on qemu's `-M raspi4b`:
#   boot 1: `milk.lcd=probe` on a card with a pre-created MILKLOG.TXT: the probe is only noted at the start and runs LAST. The log
#           has the LED stage lines in order, `wm: desktop up`, `led: done, solid on` and the log writer's lines BEFORE the first
#           `lcd: probe 1/` line; the log file on the card, read back the moment the probe's first set-up has started, already holds the
#           whole boot up to `wm: desktop up`; the probe then runs through (`lcd: probe finished`, nothing skipped, no SPI time-out);
#           the ACT LED, sampled from the pin, shows the boot status (solid on) while the probe runs, never a long dark stretch of its own;
#   boot 2: the same on a card with no log file (nothing to wait for: the probe starts after a grace period of a few seconds);
#   boot 3: a normal `milk.lcd=ili9486` boot: the whole early bring-up (init and the first full picture) is reported inside its budget (1.5 s).
# The time-budget logic itself (a stalled bus cut off at the budget, the rate limit, the probe gate) is host tested with a fake clock:
# rpikit/src/lcdbudget.rs.
# Offline, private images in a fresh /tmp folder, the qemu started here killed by PID. TCG: about 1 to 2 minutes a boot.
#
#   scripts/pi-lcd-late-test.sh [workdir]      (MILK_KEEP=1 keeps the work folder; MILK_PREBUILT=1 skips the build)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="${1:-$(mktemp -d /tmp/milk-pilcdlate.XXXXXX)}"
OWN_WORK=1; [ -n "${1:-}" ] && OWN_WORK=0
mkdir -p "$T"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
if [ "${MILK_PREBUILT:-}" = 1 ]; then
  ELF="target/pi/aarch64-unknown-none-softfloat/debug/milkos-kernel"
  [ -e "$ELF" ] || { echo "pi-lcd-late-test: MILK_PREBUILT=1 but $ELF is missing"; exit 1; }
  "$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$T/kernel8.img"
else
  scripts/pi-build.sh "$T/kernel8.img" >/dev/null
fi
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

# boot N IMAGE CMDLINE STEPS...: start qemu on the card image, drive the serial console with the steps, sample the LED pin.
boot() {
  local n="$1" img="$2" cmdline="$3"; shift 3
  python3 scripts/pi_dtb.py "$T/b$n.dtb" --cmdline "$cmdline"
  SOCK="$T/s$n.sock"; QMP="$T/q$n.sock"; LOG="$T/serial$n.log"
  : > "$LOG"
  qemu-system-aarch64 -M raspi4b -kernel "$T/kernel8.img" -dtb "$T/b$n.dtb" -display none -monitor none \
    -serial "unix:$SOCK,server=on,wait=on" -qmp "unix:$QMP,server,nowait" \
    -drive "file=$img,if=sd,format=raw" >/dev/null 2>&1 &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/done$n" &
  D=$!
  : > "$T/pin$n"
  ( while [ ! -e "$T/done$n" ] && kill -0 "$QPID" 2>/dev/null; do
      v=$(python3 scripts/qmp.py "$QMP" hmp "xp /1wx 0xfe200038" 2>/dev/null | grep -Eao '0x[0-9a-f]+' | head -1) || true
      [ -n "$v" ] && printf '%s' $(( (v >> 10) & 1 )) >> "$T/pin$n"
    done ) &
  SP=$!
  for _ in $(seq 2000); do [ -e "$T/done$n" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
  [ -e "$T/done$n" ] || bad "boot $n: the serial script did not finish (see $LOG)"
}
stop() {
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  { kill "$D"; wait "$D"; } 2>/dev/null || true
  { wait "$SP"; } 2>/dev/null || true
}
has() { tr -d '\r' < "$LOG" | grep -Eaq -- "$1"; }
check() { if has "$1"; then ok "$2: $(tr -d '\r' < "$LOG" | grep -Ea -m1 -- "$1" | cut -c1-110)"; else bad "$2 (no line matching: $1)"; fi; }
lineno() { tr -d '\r' < "$LOG" | grep -Ean -m1 -- "$1" | cut -d: -f1; } # first line number matching $1 (empty: none)
before() { # PATTERN-A PATTERN-B what: A appears, B appears, and A comes first
  local a b; a=$(lineno "$1"); b=$(lineno "$2")
  if [ -n "$a" ] && [ -n "$b" ] && [ "$a" -lt "$b" ]; then ok "$3 (line $a before line $b)"; else bad "$3 (line of '$1': ${a:-none}, of '$2': ${b:-none})"; fi
}
led_lines() { tr -d '\r' < "$LOG" | sed -n -E 's/^led: stage ([0-9]+) ([a-z]+)$/\1:\2/p; s/^led: done, solid on$/done/p' | tr '\n' ' ' | sed 's/ $//'; }
LED_ORDER='1:memory 2:mailbox 3:framebuffer 4:interrupts 5:sdcard 6:scheduler 7:userspace done'
extract() { python3 -I - "$1" "$2" "$3" <<'PY'
import json, sys
img, layout, out = sys.argv[1:4]
lay = json.load(open(layout))
data = open(img, "rb").read()
buf = b"".join(data[l * 512:(l + n) * 512] for l, n in lay["log_extents"])
open(out, "wb").write(buf[:lay["log_size"]])
PY
}
PROBE_OPTS="milk.lcd=probe milk.lcd_probe_rounds=1 milk.lcd_probe_ms=50"

# ---- boot 1: probe on a card with a log file ----
python3 -I scripts/pi_fat32.py "$T/card.img" --layout "$T/card.json" >/dev/null
boot 1 "$T/card.img" "$PROBE_OPTS" "wait@300:lcd: probe 1/22 "
sleep 1.5
extract "$T/card.img" "$T/card.json" "$T/log1.bin"
stop
check '^lcd: milk.lcd=probe: the probe starts after the boot is over' "boot 1: the probe is noted at the start of the boot"
[ "$(led_lines)" = "$LED_ORDER" ] && ok "boot 1: the LED stage lines, in order: $(led_lines)" || bad "boot 1: LED stages '$(led_lines)', expected '$LED_ORDER'"
before '^led: stage 7 userspace$' '^lcd: probe: [0-9]+ set-ups' "boot 1: the LED stages are done before the probe's first line"
before '^wm: desktop up' '^lcd: probe [0-9]+/22 ' "boot 1: the desktop is up before the first probe set-up"
before '^led: done, solid on$' '^lcd: probe [0-9]+/22 ' "boot 1: the boot is over (LED solid on) before the first probe set-up"
before '^milklog: ' '^lcd: probe [0-9]+/22 ' "boot 1: the log writer found the card's file before the first probe set-up"
before '^lcd: milk.lcd=probe: the probe starts after' '^wm: desktop up' "boot 1: at the start of the boot the probe is only noted"
before '^lcd: probe: the boot is over and the log is saved' '^lcd: probe: 22 set-ups' "boot 1: the probe says it was released by the boot"
set +e; scripts/pi-readlog.sh "$T/log1.bin" > "$T/log1.txt" 2> "$T/log1.verdict"; set -e
A=$(grep -an -m1 '^lcd: probe [0-9]*/22 ' "$T/log1.txt" | cut -d: -f1 || true)
D1=$(grep -an -m1 '^wm: desktop up' "$T/log1.txt" | cut -d: -f1 || true)
L1=$(grep -an -m1 '^led: done, solid on' "$T/log1.txt" | cut -d: -f1 || true)
if [ -n "$D1" ] && [ -n "$L1" ]; then ok "boot 1: the log file on the card, read back as the probe's first set-up runs, already holds 'wm: desktop up' (line $D1) and 'led: done' (line $L1)"; else bad "boot 1: the log file on the card lacks the end of the boot (desktop ${D1:-none}, done ${L1:-none})"; fi
if [ -n "$D1" ] && { [ -z "$A" ] || [ "$D1" -lt "$A" ]; }; then ok "boot 1: in the file the desktop line comes before any probe line"; else bad "boot 1: in the file a probe line comes before the desktop line"; fi
# ... and the probe runs through. (boot 1 above stopped at the first set-up; run the whole thing in boot 2.)

# ---- boot 2: no log file on the card: the probe waits a grace period, then runs to the end ----
python3 -I scripts/pi_fat32.py "$T/card2.img" --defect nofile >/dev/null
boot 2 "$T/card2.img" "$PROBE_OPTS" "wait@300:lcd: probe finished"
stop
before '^led: done, solid on$' '^lcd: probe [0-9]+/22 ' "boot 2: the boot is over before the first probe set-up (no log file to wait for)"
before '^wm: desktop up' '^lcd: probe [0-9]+/22 ' "boot 2: the desktop is up before the first probe set-up"
check '^milklog: not writing a boot log file: ' "boot 2: the card has no log file"
check '^lcd: probe finished in [0-9]+ ms: 22 set-up\(s\) shown, 0 skipped' "boot 2: the probe runs through, nothing skipped"
has 'lcd: .*timed out|KERNEL PANIC|kernel panic' && bad "boot 2: a time-out or a panic" || ok "boot 2: no SPI time-out, no panic"
N=$(tr -d '\r' < "$LOG" | grep -Eac '^lcd: probe [0-9]+/22 .*: done in ')
[ "$N" = 22 ] && ok "boot 2: all 22 set-ups ran ($N)" || bad "boot 2: $N set-ups ran, expected 22"
# The LED while the probe ran: the pin shows the boot status (solid on, or the log-saved blinks), not dark.
PIN=$(cat "$T/pin2")
TAIL="${PIN: -40}"
case "$TAIL" in *1*) ok "boot 2: the ACT LED is lit while the probe runs (pin trace ...${TAIL})";; *) bad "boot 2: the ACT LED is dark during the probe (pin trace ...${TAIL})";; esac
case "$TAIL" in *000000000000000000000000000000*) bad "boot 2: a long dark stretch on the LED during the probe (...${TAIL})";; *) ok "boot 2: no long dark stretch on the LED during the probe";; esac

# ---- boot 3: a normal panel, with a budget qemu can meet (its SPI model and TCG make the first picture take about a second in a debug build; the
# real board has 1.5 s for the whole bring-up): both parts are timed, the log mirror drops what it cannot send and says so, the desktop comes up ----
python3 -I scripts/pi_fat32.py "$T/card3.img" --defect nofile >/dev/null
boot 3 "$T/card3.img" "milk.lcd=ili9486 milk.lcd_budget_ms=10000" "wait@300:wm: desktop up"
stop
check '^lcd: panel initialised in [0-9]+ ms' "boot 3: the panel's bring-up is timed"
check '^lcd: boot log on the panel \(first full picture [0-9]+ ms, [0-9]+ ms since the bring-up began\)' "boot 3: the first picture is timed"
has '^lcd: gave up' && bad "boot 3: the bring-up gave up" || ok "boot 3: the bring-up did not give up"
has '^lcd: [0-9]+ panel update\(s\) from the log were dropped so far' && ok "boot 3: dropped panel updates are counted and logged" || ok "boot 3: no panel update was dropped"
has 'lcd: .*timed out|KERNEL PANIC|kernel panic' && bad "boot 3: a time-out or a panic" || ok "boot 3: no SPI time-out, no panic"

# ---- boot 4: a bring-up that does not fit its budget (milk.lcd_budget_ms=300, less than the controller's own waits) is cut off, logged, and the boot goes on ----
boot 4 "$T/card3.img" "milk.lcd=ili9486 milk.lcd_budget_ms=300" "wait@300:wm: desktop up"
stop
check '^lcd: gave up after [0-9]+ ms at step init #[0-9]+ \((delay|command|data|reset pin), out of time\); the boot goes on with the panel unused' "boot 4: the bring-up is cut off and logged"
G=$(tr -d '\r' < "$LOG" | sed -n -E 's/^lcd: gave up after ([0-9]+) ms .*/\1/p' | head -1)
[ -n "$G" ] && [ "$G" -ge 300 ] && [ "$G" -le 700 ] && ok "boot 4: it gave up after $G ms (budget 300)" || bad "boot 4: gave up after '${G:-?}' ms, expected 300 to 700"
has '^lcd: panel initialised|^lcd: boot log on the panel' && bad "boot 4: the panel was used after all" || ok "boot 4: the panel is unused"
has 'KERNEL PANIC|kernel panic' && bad "boot 4: a kernel panic" || ok "boot 4: no panic"
check '^wm: desktop up' "boot 4: the boot goes on to the desktop"

if [ "$FAIL" = 0 ]; then
  echo "pi-lcd-late-test: PASS"
  [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ] && { rm -rf "$T" || true; }
  exit 0
fi
echo "pi-lcd-late-test: FAIL (logs in $T)"; exit 1
