#!/usr/bin/env bash
# USB input end to end on qemu virt: an xHCI controller on the PCI host bridge (`qemu-xhci`) with a
# `usb-kbd` and a `usb-mouse`, no virtio input devices at all. The kernel finds the controller through
# PCI (BARs placed, bus mastering on), starts it, enumerates both devices (descriptors, boot protocol,
# interrupt endpoints) and turns their reports into input events. Keys and pointer events typed
# through qemu's QMP reach /proc/input's counters; unplugging the keyboard (device_del) releases it.
# The same code drives the VL805 on a Raspberry Pi 4 (docs/raspberry-pi.md); the PCIe root complex is the
# only part this cannot reach.
# Offline, private work folder, the qemu started here killed by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh
T="$(mktemp -d /tmp/milk-usb.XXXXXX)"
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
  -device qemu-xhci,id=xhci -device usb-kbd,id=kbd,bus=xhci.0 -device usb-mouse,id=mouse,bus=xhci.0 >/dev/null 2>&1 &
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
V=0
stat() { # V = the value of one counter of /proc/input, read through the shell (not in a subshell: step counts)
  step "send:text show /proc/input" "wait:$P"
  V=$(tr -d '\r' < "$LOG" | awk -v k="$1" '$1 == k {v = $2} END {print v + 0}')
}
step "wait@240:$P" "wait@60:usb. port [0-9]+: .*mouse"
L="$(tr -d '\r' < "$LOG")"
echo "$L" | grep -Eaq '^\[pci\] .* \(xHCI USB controller\)' && ok "PCI found the xHCI controller: $(echo "$L" | grep -Ea -m1 'xHCI USB controller')" || bad "no xHCI controller on the PCI bus"
echo "$L" | grep -Eaq '^\[usb\] xHCI [0-9]\.[0-9a-f]+: [0-9]+ ports' && ok "the controller started: $(echo "$L" | grep -Ea -m1 '^\[usb\] xHCI')" || bad "the controller did not start"
echo "$L" | grep -Eaq '\[usb\] port [0-9]+: .*keyboard' && ok "keyboard enumerated: $(echo "$L" | grep -Ea -m1 'usb. port [0-9]+: .*keyboard')" || bad "no keyboard"
echo "$L" | grep -Eaq '\[usb\] port [0-9]+: .*mouse' && ok "mouse enumerated: $(echo "$L" | grep -Ea -m1 'usb. port [0-9]+: .*mouse')" || bad "no mouse"
stat keys; K0=$V; stat moves; M0=$V; stat buttons; B0=$V
# Keys: "Hello" is 5 presses and 5 releases (plus shift for the capital).
python3 scripts/qmp.py "$QMP" type 'Hello' || bad "qmp type failed"
sleep 1
stat keys; K1=$V
[ $((K1 - K0)) -ge 12 ] && ok "typed 'Hello' on the USB keyboard: $((K1 - K0)) key events" || bad "keys: $K0 -> $K1 (wanted at least 12)"
# Pointer: relative motion and a left click.
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"rel","data":{"axis":"x","value":40}},{"type":"rel","data":{"axis":"y","value":-20}}]}}'
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"btn","data":{"down":true,"button":"left"}}]}}'
sleep 0.3
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"btn","data":{"down":false,"button":"left"}}]}}'
sleep 1
stat moves; M1=$V; stat buttons; B1=$V
[ "$M1" -gt "$M0" ] && ok "the USB mouse moved the pointer ($M0 -> $M1 moves)" || bad "no pointer moves: $M0 -> $M1"
[ $((B1 - B0)) -ge 2 ] && ok "a click: $((B1 - B0)) button events" || bad "buttons: $B0 -> $B1"
# Hold a key: it repeats (qemu sends the press once; the repeat is ours).
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"key","data":{"down":true,"key":{"type":"qcode","data":"x"}}}]}}'
sleep 1.5
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"key","data":{"down":false,"key":{"type":"qcode","data":"x"}}}]}}'
sleep 0.5
stat repeats; R=$V
[ "$R" -ge 10 ] && ok "a held key auto-repeats ($R repeats in 1.5 s)" || bad "no key repeat: $R"
# Unplug the keyboard while a key is down: the key is released.
ev '{"execute":"input-send-event","arguments":{"events":[{"type":"key","data":{"down":true,"key":{"type":"qcode","data":"z"}}}]}}'
sleep 0.5
stat keys; K2=$V
# (Serial output is only kept while a driver is connected: listen first, then pull the plug.)
n=$((n + 1))
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait@30:usb. port [0-9]+: device removed" "done:$T/done.$n" &
D=$!
sleep 0.5
ev '{"execute":"device_del","arguments":{"id":"kbd"}}'
for _ in $(seq 400); do [ -e "$T/done.$n" ] && break; sleep 0.1; done
{ kill "$D"; wait "$D"; } 2>/dev/null || true
stat keys; K3=$V
[ -e "$T/done.$n" ] && ok "unplugging the keyboard was noticed" || bad "no 'device removed' line"
[ $((K3 - K2)) -ge 1 ] && ok "its held key was released ($((K3 - K2)) event)" || bad "the held key stayed down: $K2 -> $K3"
tr -d '\r' < "$LOG" | grep -Eaq 'KERNEL PANIC|kernel panic' && bad "a kernel panic" || ok "no panic"
if [ "$FAIL" = 0 ]; then echo "usb-test: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; else echo "usb-test: FAIL (log $LOG)"; exit 1; fi
