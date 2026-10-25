#!/usr/bin/env bash
# USB hubs end to end on qemu virt: an xHCI controller (`qemu-xhci`) with a `usb-hub` on its port 1 and a
# `usb-kbd` and a `usb-mouse` behind the hub, nothing directly on the controller. This is the Raspberry Pi 4's shape:
# its four sockets are behind a hub inside the VL805. The kernel must find the hub on the root port, read its
# descriptor, power and poll its ports, and address the keyboard and the mouse through it (route string
# and parent hub in the slot context); keys and pointer events typed through QMP then reach /proc/input.
# Then, over QMP: a second hub plugged into the first one's port 3 (a nested hub) with a keyboard behind it,
# a keyboard unplugged while a key is held (released), the nested hub unplugged with its keyboard, the whole first
# hub unplugged with everything on it, and a hub and keyboard plugged in again (slots are reused, nothing leaks, no
# panic). qemu's hub is a full speed one, so the translator fields of a low/full speed device behind a *high speed* hub
# (the Pi's) are covered by the host tests (rpikit usbhub) and not by this.
# Offline, private work folder, the qemu started here killed by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh
T="$(mktemp -d /tmp/milk-usbhub.XXXXXX)"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
[ "${MILK_PREBUILT:-}" = 1 ] || cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$T/kernel.bin"
SOCK="$T/s.sock"; QMP="$T/q.sock"; LOG="$T/serial.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 4 -m 512M \
  -kernel "$T/kernel.bin" -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" -qmp "unix:$QMP,server,nowait" \
  -global virtio-mmio.force-legacy=false \
  -device qemu-xhci,id=xhci -device usb-hub,bus=xhci.0,port=1,id=hub1 \
  -device usb-kbd,id=kbd,bus=xhci.0,port=1.1 -device usb-mouse,id=mouse,bus=xhci.0,port=1.2 >/dev/null 2>&1 &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
n=0
step() { # the serial driver for one batch of steps
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/done.$n" &
  local d=$!
  for _ in $(seq 600); do [ -e "$T/done.$n" ] && break; sleep 0.2; done
  [ -e "$T/done.$n" ] || bad "serial step $n did not finish"
  { kill "$d"; wait "$d"; } 2>/dev/null || true
}
ev() { python3 scripts/qmp.py "$QMP" raw "$1" >/dev/null; }
# Listen on the serial line for REGEX (output is only kept while a driver is connected), then run the QMP commands given;
# true if the line came within 30 s.
plug() {
  local re="$1"; shift
  n=$((n + 1))
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait@30:$re" "done:$T/done.$n" &
  local d=$!
  sleep 0.5
  local j
  for j in "$@"; do ev "$j"; sleep 0.3; done
  for _ in $(seq 400); do [ -e "$T/done.$n" ] && break; sleep 0.1; done
  local r=1
  [ -e "$T/done.$n" ] && r=0
  { kill "$d"; wait "$d"; } 2>/dev/null || true
  return $r
}
add() { echo "{\"execute\":\"device_add\",\"arguments\":{\"driver\":\"$1\",\"bus\":\"xhci.0\",\"port\":\"$2\",\"id\":\"$3\"}}"; }
del() { echo "{\"execute\":\"device_del\",\"arguments\":{\"id\":\"$1\"}}"; }
key() { echo "{\"execute\":\"input-send-event\",\"arguments\":{\"events\":[{\"type\":\"key\",\"data\":{\"down\":$2,\"key\":{\"type\":\"qcode\",\"data\":\"$1\"}}}]}}"; }
V=0
stat() { # V = the value of one counter of /proc/input, read through the shell (not in a subshell: step counts)
  step "send:text show /proc/input" "wait:$P"
  V=$(tr -d '\r' < "$LOG" | awk -v k="$1" '$1 == k {v = $2} END {print v + 0}')
}
has() { tr -d '\r' < "$LOG" | grep -Eaq "$1"; }
step "wait@240:$P" "wait@60:usb. port [0-9]+\.2: .*mouse"
L="$(tr -d '\r' < "$LOG")"
# The number the kernel gives the root port qemu calls 1 (qemu-xhci numbers its USB 2 ports after the USB 3 ones).
R="$(echo "$L" | sed -n 's/.*\[usb\] hub [0-9a-f:]* on port \([0-9]*\): .*/\1/p' | head -1)"
[ -n "$R" ] && ok "the hub is on the controller's root port $R" || { bad "no hub line"; R=5; }
echo "$L" | grep -Eaq '^\[usb\] xHCI [0-9]\.[0-9a-f]+: [0-9]+ ports' && ok "the controller started" || bad "the controller did not start"
echo "$L" | grep -Eaq '\[usb\] hub [0-9a-f]{4}:[0-9a-f]{4} on port [0-9]+: [0-9]+ ports' && ok "the hub on a root port was found: $(echo "$L" | grep -Ea -m1 'usb. hub [0-9a-f]+:')" || bad "no hub line"
echo "$L" | grep -Eaq "\[usb\] port $R\.1: .*keyboard" && ok "keyboard behind the hub: $(echo "$L" | grep -Ea -m1 "usb. port $R\.1: .*keyboard")" || bad "no keyboard behind the hub"
echo "$L" | grep -Eaq "\[usb\] port $R\.2: .*mouse" && ok "mouse behind the hub: $(echo "$L" | grep -Ea -m1 "usb. port $R\.2: .*mouse")" || bad "no mouse behind the hub"
echo "$L" | grep -Eaq "\[usb\] port $R\.1: hid keyboard attached" && ok "hid keyboard attached" || bad "no 'hid keyboard attached'"
echo "$L" | grep -Eaq "\[usb\] port $R: .*(keyboard|mouse)" && bad "something was attached directly to the root port" || ok "nothing on the root port itself"
stat keys; K0=$V; stat moves; M0=$V; stat buttons; B0=$V
python3 scripts/qmp.py "$QMP" type 'Hello' || bad "qmp type failed"
sleep 1
stat keys; K1=$V
[ $((K1 - K0)) -ge 12 ] && ok "typed 'Hello' on the keyboard behind the hub: $((K1 - K0)) key events" || bad "keys: $K0 -> $K1 (wanted at least 12)"
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"rel","data":{"axis":"x","value":40}},{"type":"rel","data":{"axis":"y","value":-20}}]}}'
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"btn","data":{"down":true,"button":"left"}}]}}'
sleep 0.3
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"btn","data":{"down":false,"button":"left"}}]}}'
sleep 1
stat moves; M1=$V; stat buttons; B1=$V
[ "$M1" -gt "$M0" ] && ok "the mouse behind the hub moved the pointer ($M0 -> $M1 moves)" || bad "no pointer moves: $M0 -> $M1"
[ $((B1 - B0)) -ge 2 ] && ok "a click: $((B1 - B0)) button events" || bad "buttons: $B0 -> $B1"
# A key down on the keyboard, then it is unplugged (it is the only one, so the key is certainly its): the key is released.
ev "$(key z true)"
sleep 0.5
stat keys; K2=$V
plug "usb. port $R\.1: device removed" "$(del kbd)" && ok "unplugging the keyboard was noticed" || bad "no 'device removed' for the keyboard"
stat keys; K3=$V
[ $((K3 - K2)) -ge 1 ] && ok "its held key was released ($((K3 - K2)) event)" || bad "the held key stayed down: $K2 -> $K3"
# Hot plug: a second hub in the first one's port 3 (nested), a keyboard in the second one's port 1.
plug "usb. port $R\.3\.1: .*keyboard" "$(add usb-hub 1.3 hub2)" "$(add usb-kbd 1.3.1 kbd2)" && ok "a keyboard behind a nested hub was enumerated" || bad "no keyboard behind the nested hub"
has "\[usb\] hub [0-9a-f:]+ on port $R\.3: [0-9]+ ports" && ok "the nested hub (route string 3, then 1) was found: $(tr -d '\r' < "$LOG" | grep -Ea -m1 "usb. hub [0-9a-f:]+ on port $R\.3")" || bad "no line for the nested hub"
# The keyboard in the nested hub is the only one left.
python3 scripts/qmp.py "$QMP" type 'ok' || bad "qmp type failed"
sleep 1
stat keys; K4=$V
[ $((K4 - K3)) -ge 4 ] && ok "the keyboard behind two hubs works: $((K4 - K3)) key events" || bad "keys after: $K3 -> $K4"
# A keyboard plugged in where the first was: the port is reused.
plug "usb. port $R\.1: .*keyboard" "$(add usb-kbd 1.1 kbd3)" && ok "a keyboard plugged into the freed port was enumerated again" || bad "no keyboard on the reused port"
# The nested hub goes with its keyboard.
plug "usb. port $R\.3: hub removed" "$(del hub2)" && ok "the nested hub was removed" || bad "no 'hub removed' for the nested hub"
has "usb. port $R\.3\.1: device removed" && ok "the keyboard behind it was removed first" || bad "no removal line for the keyboard behind the nested hub"
# The whole first hub, with the mouse and the keyboard on it.
plug "usb. port $R: hub removed" "$(del hub1)" && ok "the first hub was removed with everything on it" || bad "no 'hub removed' for the first hub"
has "usb. port $R\.2: device removed" && ok "the mouse behind it was removed" || bad "no removal line for the mouse"
# And a new hub with a keyboard: the slots and the port are free again.
plug "usb. port $R\.4: .*keyboard" "$(add usb-hub 1 hub4)" "$(add usb-kbd 1.4 kbd4)" && ok "a new hub with a keyboard was enumerated after all that" || bad "no keyboard behind the new hub"
python3 scripts/qmp.py "$QMP" type 'again' || bad "qmp type failed"
sleep 1
stat keys; K5=$V
[ $((K5 - K4)) -ge 8 ] && ok "and it types: $((K5 - K4)) key events" || bad "keys after the re-plug: $K4 -> $K5"
tr -d '\r' < "$LOG" | grep -Eaq 'KERNEL PANIC|kernel panic' && bad "a kernel panic" || ok "no panic"
tr -d '\r' < "$LOG" | grep -Eaq 'does not answer|keeps failing|did not confirm|giving up|ignoring the port' && bad "the driver gave up on something: $(tr -d '\r' < "$LOG" | grep -Ea -m1 'does not answer|keeps failing|did not confirm|giving up|ignoring the port')" || ok "no hub or port was given up on"
if [ "$FAIL" = 0 ]; then echo "usb-hub-test: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; else echo "usb-hub-test: FAIL (log $LOG)"; exit 1; fi
