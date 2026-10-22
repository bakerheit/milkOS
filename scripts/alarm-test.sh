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
$M mkdir "$IMG" /Users/milk >/dev/null
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
  # A first command that only lets the boot finish: a service starting late (seen with other
  # tests running: "[udrv] rng ... claimed") prints over the first prompt, and a command typed
  # there isn't on a prompt line of its own, so out() can't find its output.
  local args=("wait:$P" "send:sleep 1" "wait:$P")
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
LOGF=/Users/milk/.cache/milk/alarmd.log
boot b1 "processes list | grep alarmd" \
  "alarm add 03:15 weekdays pills" "alarm add in 5s ring-test" "timer 6s tea-test" \
  "cal add in 5s standup-test --alert 0" "cal add tomorrow 12:00 lunch-test --repeat weekly" \
  "remind add in 5s call-the-plumber" "remind add tomorrow stretch --repeat weekly" "remind add undated-thing" "remind add Pick up parcel friday at 5pm" \
  "alarm snooze 1 30" "remind snooze 3 15" "grep undated /Users/milk/Library/Reminders/Personal.rem" \
  "alarm list" "timer list" "cal list 3" "cal" "remind list" \
  "sleep 30" "sleep 30" "sleep 20" \
  "cat $LOGF" "alarm list" "timer list" "cal list 3" "remind done 2" "remind done 3" "remind list" \
  "echo now > /System/Kernel/fs/milkfs/sync" "sleep 3"
L="$T/b1.log"
out "$L" "processes list | grep alarmd" | grep -q "alarmd" && ok "alarmd is running (started by init)" || bad "alarmd not running"
out "$L" "alarm list" | head -3 | sed 's/^/    /'
out "$L" "cat $LOGF" | sed 's/^/    log: /'
[ "$(out "$L" "alarm list" | grep -c pills)" = 2 ] && ok "alarm snooze made a one-off copy of the alarm" || bad "alarm snooze: $(out "$L" "alarm list")"
out "$L" "grep undated /Users/milk/Library/Reminders/Personal.rem" | grep -q "@" && ok "remind snooze stored a snooze time" || bad "remind snooze: $(out "$L" "grep undated /Users/milk/Library/Reminders/Personal.rem")"
out "$L" "cat $LOGF" | grep -q "timer: Timer done / tea-test" && ok "the timer rang" || bad "no timer ring in the log"
out "$L" "cat $LOGF" | grep -q "alarm: Alarm / ring-test" && ok "the alarm rang" || bad "no alarm ring in the log"
out "$L" "cat $LOGF" | grep -q "event: standup-test / Today " && ok "the calendar alert rang" || bad "no event alert in the log"
out "$L" "cat $LOGF" | grep -q "reminder: call-the-plumber / Personal" && ok "the reminder rang" || bad "no reminder ring in the log"
out "$L" "remind done 2" | grep -q "next due" && ok "completing a repeating reminder moves it on" || bad "repeating reminder did not move on: $(out "$L" "remind done 2")"
[ "$(out "$L" "cat $LOGF" | grep -c "pills\|lunch-test\|stretch\|undated")" = 0 ] && ok "the weekday alarm and tomorrow's event did not ring" || bad "something rang that should not have"
# The second "alarm list" / "timer list" is the one typed after the sleeps.
after() { tr -d '\r' < "$L" | awk -v c="$1" '/^msh:/ { on=0; if (substr($0, index($0, "$ ") + 2) == c && ++k == 2) on=1; next } on'; }
after "alarm list" | grep "ring-test" | grep -q "off" && ok "the once alarm switched itself off" || bad "ring-test alarm is not off: $(after "alarm list" | grep ring-test)"
after "alarm list" | grep "pills" | grep -q "03:15  Weekdays" && ok "the weekday alarm is untouched" || bad "pills alarm changed"
after "timer list" | grep -q "no timers" && ok "the finished timer was removed" || bad "timer still listed: $(after "timer list")"
out "$L" "cal" | grep -q '\*' && ok "the month view marks today's event" || bad "month view has no * mark"
boot b2 "alarm list" "cal list 3" "cat $LOGF" "remind list"
L="$T/b2.log"
out "$L" "alarm list" | grep -q "pills" && ok "the alarm survived a reboot" || bad "alarm lost after reboot"
out "$L" "cal list 3" | grep -q "lunch-test" && ok "the repeating event survived a reboot" || bad "event lost after reboot"
out "$L" "cat $LOGF" | grep -q "tea-test" && ok "the ring log survived a reboot" || bad "ring log lost after reboot"
out "$L" "remind list" | grep -q "stretch" && ok "the repeating reminder survived a reboot" || bad "reminder lost after reboot"
out "$L" "remind list" | grep -q "undated-thing" && bad "a completed reminder is still open" || ok "completed reminders stay closed"
if [ "$FAIL" = 0 ]; then echo "alarm-test: PASS"; rm -rf "$T"; else echo "alarm-test: FAIL (logs in $T)"; exit 1; fi
