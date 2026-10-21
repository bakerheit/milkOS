#!/usr/bin/env bash
# Alarms, timers and calendar alerts on a private image (fresh /tmp directory; only the qemu
# started here is killed). One boot rings all three through alarmd (init starts it), a second
# boot checks the data survived. About two and a half minutes.
#   boot 1: alarm add in 5s / timer 6s / cal add in 5s --alert 0, plus a weekday alarm and a
#           repeating event that must NOT ring. After ~80 s the log has one line per ring, the
#           "once" alarm is switched off, the finished timer is gone.
#   boot 2: the weekday alarm and the repeating event are still there (milkfs home folder).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-alarmtest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/a.img"
$M mkfs "$IMG" --size 64M >/dev/null
$M mkdir "$IMG" /home/milk >/dev/null
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT
boot() { # name commands...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  local args=("wait:$P")
  for c in "$@"; do args+=("send:$c" "wait:$P"); done
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "${args[@]}" "send:echo ALARM""-DONE" "wait:ALARM-DONE" "done:$T/$name.done" &
  local DPID=$!
  for _ in $(seq 1500); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
}
# Output of the command typed as $2 in log $1, up to the next prompt.
out() { tr -d '\r' < "$1" | awk -v c="$2" 'on && /^msh:/ {on=0; done=1} !done && $0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c {on=1; next} on'; }
LOGF=/home/milk/.cache/milk/alarmd.log
boot b1 "ps | grep alarmd" \
  "alarm add 03:15 weekdays pills" "alarm add in 5s ring-test" "timer 6s tea-test" \
  "cal add in 5s standup-test --alert 0" "cal add tomorrow 12:00 lunch-test --repeat weekly" \
  "alarm list" "timer list" "cal list 3" "cal" \
  "sleep 30" "sleep 30" "sleep 20" \
  "cat $LOGF" "alarm list" "timer list" "cal list 3" \
  "echo now > /sys/fs/milkfs/sync" "sleep 3"
L="$T/b1.log"
out "$L" "ps | grep alarmd" | grep -q "alarmd" && ok "alarmd is running (started by init)" || bad "alarmd not running"
out "$L" "alarm list" | head -3 | sed 's/^/    /'
out "$L" "cat $LOGF" | sed 's/^/    log: /'
out "$L" "cat $LOGF" | grep -q "timer: Timer done / tea-test" && ok "the timer rang" || bad "no timer ring in the log"
out "$L" "cat $LOGF" | grep -q "alarm: Alarm / ring-test" && ok "the alarm rang" || bad "no alarm ring in the log"
out "$L" "cat $LOGF" | grep -q "event: standup-test / Today " && ok "the calendar alert rang" || bad "no event alert in the log"
[ "$(out "$L" "cat $LOGF" | grep -c "pills\|lunch-test")" = 0 ] && ok "the weekday alarm and tomorrow's event did not ring" || bad "something rang that should not have"
# The second "alarm list" / "timer list" is the one typed after the sleeps.
after() { tr -d '\r' < "$L" | awk -v c="$1" '/^msh:/ { on=0; if (substr($0, index($0, "$ ") + 2) == c && ++k == 2) on=1; next } on'; }
after "alarm list" | grep "ring-test" | grep -q "off" && ok "the once alarm switched itself off" || bad "ring-test alarm is not off: $(after "alarm list" | grep ring-test)"
after "alarm list" | grep "pills" | grep -q "03:15  Weekdays" && ok "the weekday alarm is untouched" || bad "pills alarm changed"
after "timer list" | grep -q "no timers" && ok "the finished timer was removed" || bad "timer still listed: $(after "timer list")"
out "$L" "cal" | grep -q '\*' && ok "the month view marks today's event" || bad "month view has no * mark"
boot b2 "alarm list" "cal list 3" "cat $LOGF"
L="$T/b2.log"
out "$L" "alarm list" | grep -q "pills" && ok "the alarm survived a reboot" || bad "alarm lost after reboot"
out "$L" "cal list 3" | grep -q "lunch-test" && ok "the repeating event survived a reboot" || bad "event lost after reboot"
out "$L" "cat $LOGF" | grep -q "tea-test" && ok "the ring log survived a reboot" || bad "ring log lost after reboot"
if [ "$FAIL" = 0 ]; then echo "alarm-test: PASS"; rm -rf "$T"; else echo "alarm-test: FAIL (logs in $T)"; exit 1; fi
