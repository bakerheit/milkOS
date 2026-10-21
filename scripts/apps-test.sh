#!/usr/bin/env bash
# Calendar and Clock windows end to end on a private image: boot with a desktop, open each app,
# click through the views (screenshots in the work directory), make an event through the
# window and check the file it wrote with the `cal` tool. About one minute.
#   scripts/apps-test.sh [workdir]
# Only the qemu started here is killed (by PID). Pointer coordinates assume the 1024x768 desktop
# and the windows' first position (the first window opens at the top left of the cascade).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="${1:-$(mktemp -d /tmp/milk-appstest.XXXXXX)}"
mkdir -p "$T"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/a.img"
rm -f "$IMG"
$M mkfs "$IMG" --size 64M >/dev/null
$M mkdir "$IMG" /home/milk >/dev/null
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
: > "$LOG"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M -rtc base=localtime \
  -kernel "$ELF.bin" -serial "unix:$SOCK,server,nowait" -device ramfb -display none \
  -global virtio-mmio.force-legacy=false -device virtio-keyboard-device -device virtio-tablet-device \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
  -qmp "unix:$QMP,server,nowait" >/dev/null 2>&1 &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
n=0
ser() { # steps...: run console steps, wait for them to finish, disconnect
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/ser.$n" &
  local d=$!
  for _ in $(seq 400); do [ -e "$T/ser.$n" ] && break; sleep 0.2; done
  kill "$d" 2>/dev/null || true; wait "$d" 2>/dev/null || true
}
q() { python3 scripts/qmp.py "$QMP" "$@"; }
# Output of the command typed as $1, up to the next prompt (the echoed command may or may not
# follow a prompt on the same line, because the window system logs between them).
out() { tr -d '\r' < "$LOG" | awk -v c="$1" 'on && /^msh:/ {on=0} !on && ($0 == c || ($0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c)) {on=1; next} on'; }
differ() { ! cmp -s "$1" "$2"; }

ser "wait:$P"
q move 785 615 click sleep 1
ser "send:cal add today 09:00 Seeded event --calendar Work" "wait:$P" "send:open /bin/calendar" "wait:$P"
sleep 3
q screendump "$T/cal-month.png"
q move 333 92 click sleep 1 screendump "$T/cal-week.png"
q move 277 92 click sleep 1 screendump "$T/cal-day.png"
q move 461 92 click sleep 1 screendump "$T/cal-year.png"
q move 398 92 click sleep 1 screendump "$T/cal-month2.png"
differ "$T/cal-month.png" "$T/cal-week.png" && differ "$T/cal-week.png" "$T/cal-day.png" && differ "$T/cal-day.png" "$T/cal-year.png" && ok "Day, Week, Month and Year each draw something different" || bad "the view buttons changed nothing"
# A new event through the window: the New event button, a title, Return.
q move 152 662 click sleep 1 type 'From the window' sleep 0.3 screendump "$T/cal-sheet.png" key ret sleep 1.5
ser "send:cal list 2" "wait:$P" "send:cat /dev/wm" "wait:$P"
out "cal list 2" | grep -q "Seeded event" && ok "the seeded event is in the calendar file" || bad "seeded event missing"
out "cal list 2" | grep -q "From the window" && ok "an event made in the window reached the file" || bad "the window's event did not reach the file: $(out "cal list 2")"
out "cat /dev/wm" | grep -q "Calendar" && ok "the Calendar window is on the desktop" || bad "no Calendar window in /dev/wm"
ser "send:open /bin/clock" "wait:$P"
sleep 3
q screendump "$T/clock.png"
ser "send:cat /dev/wm" "wait:$P"
tr -d '\r' < "$LOG" | grep -a "window .* Clock" >/dev/null && ok "the Clock window is on the desktop" || bad "no Clock window"
[ "$(stat -f %z "$T/clock.png")" -gt 10000 ] && ok "Clock drew (screenshot $(stat -f %z "$T/clock.png") bytes)" || bad "Clock screenshot is empty"
ser "send:open /bin/activity" "wait:$P"
sleep 3
q move 483 140 click sleep 2 screendump "$T/activity-security.png"
ser "send:cat /dev/wm" "wait:$P"
tr -d '\r' < "$LOG" | grep -a "window .* Activity Monitor" >/dev/null && ok "the Activity Monitor window is on the desktop" || bad "no Activity Monitor window"
differ "$T/clock.png" "$T/activity-security.png" && ok "Activity Monitor drew its own screen" || bad "Activity Monitor screenshot matches Clock"
grep -aqi "panic" "$LOG" && bad "a kernel panic in the serial log" || ok "no panic"
if [ "$FAIL" = 0 ]; then echo "apps-test: PASS (screenshots in $T)"; else echo "apps-test: FAIL (logs and screenshots in $T)"; exit 1; fi
