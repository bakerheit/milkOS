#!/usr/bin/env bash
# The Raspberry Pi 4 kernel (docs/raspberry-pi.md) on qemu's `-M raspi4b`, end to end:
#   boot 1: the shell prompt on the serial console (PL011), four CPUs released through the
#           firmware's spin table, the framebuffer from the mailbox and the desktop on it (a
#           qemu screendump checked for real content), the SD card (emulated EMMC) found,
#           a fresh milkfs on it mounted, commands typed over the serial line, a file written;
#   boot 2: the same SD image again: the file is still there (the card's data survives a reboot).
# Offline, private images in a fresh /tmp folder, the qemu started here killed by PID.
# TCG (no hvf for raspi), so a boot takes about 10-20 s.
#
#   scripts/pi-qemu-test.sh [workdir]      (MILK_KEEP=1 keeps the work folder; MILK_PREBUILT=1 skips the build)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="${1:-$(mktemp -d /tmp/milk-pi.XXXXXX)}"
OWN_WORK=1; [ -n "${1:-}" ] && OWN_WORK=0
mkdir -p "$T"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
if [ "${MILK_PREBUILT:-}" = 1 ]; then
  ELF="target/pi/aarch64-unknown-none-softfloat/debug/milkos-kernel"
  [ -e "$ELF" ] || { echo "pi-qemu-test: MILK_PREBUILT=1 but $ELF is missing"; exit 1; }
  "$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$T/kernel8.img"
else
  scripts/pi-build.sh "$T/kernel8.img" >/dev/null
fi
# What qemu does not supply: a device tree (qemu's raspi4b has none of its own).
python3 scripts/pi_dtb.py "$T/pi.dtb" --cmdline ""
python3 scripts/mkmilkfs.py mkfs "$T/sd.img" --size 64M --label Milk >/dev/null
P='msh:[^\n]*\$ '
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

# boot N: start qemu on the card image, drive the serial console with the steps after it.
boot() {
  local n="$1"; shift
  SOCK="$T/s$n.sock"; QMP="$T/q$n.sock"; LOG="$T/serial$n.log"
  : > "$LOG"
  # raspi4b: the SD card attaches with if=sd; no -m (the board's RAM), no hvf.
  qemu-system-aarch64 -M raspi4b -kernel "$T/kernel8.img" -dtb "$T/pi.dtb" -display none -monitor none \
    -serial "unix:$SOCK,server=on,wait=on" -qmp "unix:$QMP,server,nowait" \
    -drive "file=$T/sd.img,if=sd,format=raw" >/dev/null 2>&1 &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/done$n" &
  D=$!
  for _ in $(seq 1500); do [ -e "$T/done$n" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
  [ -e "$T/done$n" ] || bad "boot $n: the serial script did not finish (see $LOG)"
}
stop() {
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$D" 2>/dev/null || true
}
out() { # output of the command typed as $1 (the last time), up to the next prompt
  tr -d '\r' < "$LOG" | awk -v c="$1" '/^msh:/ { if (on) {on=0}; if (substr($0, index($0, "$ ") + 2) == c) {buf=""; on=1; next} } $0 == c && !on {buf=""; on=1; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'
}
has() { tr -d '\r' < "$LOG" | grep -Eaq -- "$1"; }
check() { # pattern what
  if has "$1"; then ok "$2: $(tr -d '\r' < "$LOG" | grep -Ea -m1 -- "$1" | cut -c1-110)"; else bad "$2 (no line matching: $1)"; fi
}

# ---- boot 1 ----
boot 1 "wait@300:$P" \
  "send:system" "wait:$P" \
  "send:disks" "wait:$P" \
  "send:print hello-from-the-pi > /Users/milk/Documents/pi.txt" "wait:$P" \
  "send:disks snapshot pi-test" "wait:$P" \
  "send:text show /Users/milk/Documents/pi.txt" "wait:$P" \
  "send:processes" "wait:$P"
sleep 1
python3 scripts/qmp.py "$QMP" screendump "$T/desktop.png" >/dev/null 2>&1 || true
stop
check '^platform: Raspberry Pi 4 \(BCM2711\) \(device tree: raspberrypi,4-model-b\)' "the Pi board and its device tree"
check '^mem: RAM 0x0\.\.0x[0-9a-f]+ \([0-9]+ MiB, device tree\)' "RAM from the device tree"
check '^\[gic\] v2 up' "the GIC-400"
check '^\[smp\] 4 of 4 CPUs online \(spin table\)' "four CPUs through the spin table"
check '^rpi fb: 1024x768x32 pitch 4096 at ' "the mailbox framebuffer"
check '^display: rpi mailbox framebuffer 1024x768' "the display on it"
check '^\[sdhci\] .*card ready' "the SD card"
check '^milkfs: mmcblk0 mounted at /Volumes/Milk' "milkfs on the SD card"
check '^wm: desktop up' "the desktop"
has 'KERNEL PANIC|kernel panic' && bad "a kernel panic" || ok "no panic"
out system | grep -Eq '^CPUs +4' && ok "system says 4 CPUs" || bad "system: $(out system)"
out system | grep -Eq '^memory +[0-9]+ MB' && ok "system: $(out system | grep '^memory')" || bad "system has no memory line"
out disks | grep -q 'milkfs.*/Volumes/Milk' && ok "disks lists the card's milkfs" || bad "disks: $(out disks)"
out "text show /Users/milk/Documents/pi.txt" | grep -q 'hello-from-the-pi' && ok "a file written on the card reads back" || bad "no file: $(out 'text show /Users/milk/Documents/pi.txt')"
# The screen: 1024x768, with a desktop's worth of colours in it (not black, not one flat colour).
if [ -s "$T/desktop.png" ] && python3 -I scripts/png_check.py "$T/desktop.png" >/dev/null 2>&1; then
  python3 -I - "$T/desktop.png" <<'PY' && ok "the screen shows a desktop" || bad "the screen is blank or flat"
import struct, sys, zlib
d = open(sys.argv[1], "rb").read()
w, h = struct.unpack(">II", d[16:24])
assert (w, h) == (1024, 768), (w, h)
i, idat, bpp = 8, b"", 0
while i < len(d):
    n, = struct.unpack(">I", d[i:i+4]); t = d[i+4:i+8]
    if t == b"IHDR": bpp = {2: 3, 6: 4}[d[i+8+9]]
    if t == b"IDAT": idat += d[i+8:i+8+n]
    i += 12 + n
raw = zlib.decompress(idat)
# Rows are filtered; count distinct bytes of the first 20 rows' raw data as a cheap variety check.
colours = len(set(raw[:(1 + w * bpp) * 400]))
assert colours > 60, colours
PY
else
  bad "no screendump of the desktop"
fi

# ---- boot 2: the same card ----
boot 2 "wait@300:$P" "send:text show /Users/milk/Documents/pi.txt" "wait:$P"
stop
# (The echo of the typed command can be interleaved with kernel log lines; the file's text is
# on a line of its own.)
tr -d '\r' < "$LOG" | grep -q '^hello-from-the-pi$' && ok "boot 2: the file is still on the card" || bad "boot 2: the file is gone (see $LOG)"
check '^milkfs: mmcblk0 mounted' "boot 2: the card mounts again"

if [ "$FAIL" = 0 ]; then
  echo "pi-qemu-test: PASS"
  [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ] && { rm -rf "$T" || true; }
else
  echo "pi-qemu-test: FAIL (logs in $T)"; exit 1
fi
