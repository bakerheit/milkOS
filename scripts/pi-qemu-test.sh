#!/usr/bin/env bash
# The Raspberry Pi 4 kernel (docs/raspberry-pi.md) on qemu's `-M raspi4b`, end to end:
#   boot 1: the shell prompt on the serial console (PL011), four CPUs released through the
#           firmware's spin table, the framebuffer from the mailbox and the desktop on it (a
#           qemu screendump checked for real content), the SD card (emulated EMMC) found,
#           a fresh milkfs on it mounted, commands typed over the serial line, a file written;
#   boot 2: the same SD image again: the file is still there (the card's data survives a reboot);
#   boot 3: a device tree that lists two RAM banks with the GPU's hole between them (the shape of a
#           4 GB or 8 GB board's memory node), loaded unchanged (qemu rewrites memory nodes of a tree
#           given with -dtb, so this one goes in as a raw file where the boot stub looks for it):
#           both banks used, the hole reserved, the framebuffer inside the hole, the desktop up.
#   boot 4: `milk.crashtest=early` on the command line: a panic right after the display is up, with no
#           serial cable in mind: the boot log (not cleared) and the panic text are on the screen, with the
#           green sign-of-life band on top (qemu screendump, pixels checked);
#   boot 5: `milk.crashtest=late`: a panic with the desktop up: the end of the log, the panic text, on a
#           full-screen text console (screendump: the desktop is gone, the console background is there);
#   boots 6, 7: `milk.crashtest=late-off` / `late-reset`: the desktop's Shut Down and Restart on a board with no PSCI
#           (the SoC's power manager): qemu exits by itself / the machine boots again.
#           (raspi4b has exactly 2 GiB, so a layout above 4 GiB cannot run here: the allocator and
#           page tables above 4 GiB are tested on qemu virt, scripts/highmem-test.sh.)
# Offline, private images in a fresh /tmp folder, the qemu started here killed by PID.
# TCG (no hvf for raspi), so a boot takes about 10-30 s.
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
python3 scripts/pi_dtb.py "$T/pi.dtb" --cmdline "milk.sdwrite=1"
python3 scripts/mkmilkfs.py mkfs "$T/sd.img" --size 64M --label Milk >/dev/null
# boot 3's tree: no CPUs (it must fit under the boot stub's own code at 0x300), two banks.
python3 scripts/pi_dtb.py "$T/banks.dtb" --cpus 0 --ram-bytes 0x3b400000 --bank 0x40000000:0x40000000
P='msh:[^\n]*\$ '
QPID=""
DTB_ARGS=(-dtb "$T/pi.dtb")
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

# boot N: start qemu on the card image, drive the serial console with the steps after it.
boot() {
  local n="$1"; shift
  SOCK="$T/s$n.sock"; QMP="$T/q$n.sock"; LOG="$T/serial$n.log"
  : > "$LOG"
  # raspi4b: the SD card attaches with if=sd; no -m (the board's RAM), no hvf.
  qemu-system-aarch64 -M raspi4b -kernel "$T/kernel8.img" "${DTB_ARGS[@]}" -display none -monitor none \
    -serial "unix:$SOCK,server=on,wait=on" -qmp "unix:$QMP,server,nowait" \
    -drive "file=$T/sd.img,if=sd,format=raw" >/dev/null 2>&1 &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/done$n" &
  D=$!
  for _ in $(seq 1500); do [ -e "$T/done$n" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
  # A machine that powers itself off ends the loop above before the script has written its marker.
  for _ in 1 2 3 4 5 6 7 8 9 10; do [ -e "$T/done$n" ] && break; sleep 0.3; done
  [ -e "$T/done$n" ] || bad "boot $n: the serial script did not finish (see $LOG)"
}
stop() {
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  { kill "$D"; wait "$D"; } 2>/dev/null || true
}
out() { # output of the command typed as $1 (the last time), up to the next prompt
  tr -d '\r' < "$LOG" | awk -v c="$1" '/^msh:/ { if (on) {on=0}; if (substr($0, index($0, "$ ") + 2) == c) {buf=""; on=1; next} } $0 == c && !on {buf=""; on=1; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'
}
has() { tr -d '\r' < "$LOG" | grep -Eaq -- "$1"; }
check() { # pattern what
  if has "$1"; then ok "$2: $(tr -d '\r' < "$LOG" | grep -Ea -m1 -- "$1" | cut -c1-110)"; else bad "$2 (no line matching: $1)"; fi
}

# ---- boot 1 ----
boot 1 "wait@300:wm: desktop up" \
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
check 'wm: desktop up' "the desktop"
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

# ---- boot 3: two RAM banks and a hole ----
DTB_ARGS=(-device "loader,file=$T/banks.dtb,addr=0x100,force-raw=on")
boot 3 "wait@300:$P" "wait@300:wm: desktop up" "send:system" "wait:$P"
sleep 1
python3 scripts/qmp.py "$QMP" screendump "$T/banks.png" >/dev/null 2>&1 || true
stop
check '^mem: RAM 0x0\.\.0x80000000 \(1972 MiB, device tree\)' "boot 3: both banks (948 + 1024 MiB)"
check '^mem:   bank 0x0\.\.0x3b400000 ' "boot 3: the first bank"
check '^mem:   bank 0x40000000\.\.0x80000000 ' "boot 3: the second bank"
check '^rpi fb: 1024x768x32 pitch 4096 at 0x3c' "boot 3: the framebuffer in the hole between them"
check 'wm: desktop up' "boot 3: the desktop"
has 'KERNEL PANIC|kernel panic' && bad "boot 3: a kernel panic" || ok "boot 3: no panic"
out system | grep -Eq '^memory +(1\.9|2\.0) GB' && ok "boot 3: system: $(out system | grep '^memory')" || bad "boot 3: system: $(out system)"
[ -s "$T/banks.png" ] && python3 -I scripts/png_check.py "$T/banks.png" >/dev/null 2>&1 && ok "boot 3: a screendump" || bad "boot 3: no screendump"

# ---- boots 4 and 5: what a crash looks like on the screen ----
pixels() { # PNG X,Y ... -> "R,G,B" per point
  python3 -I - "$@" <<'PY'
import sys
sys.path.insert(0, "scripts")
import png_check
w, h, rows = png_check.decode(sys.argv[1])
print(" ".join("%d,%d,%d" % tuple(rows[int(p.split(",")[1])][int(p.split(",")[0])]) for p in sys.argv[2:]))
PY
}
for stage in early late; do
  python3 scripts/pi_dtb.py "$T/crash.dtb" --cmdline "milk.crashtest=$stage"
  DTB_ARGS=(-dtb "$T/crash.dtb")
  n=$([ $stage = early ] && echo 4 || echo 5)
  # No prompt will come: wait for the panic itself, then the screen.
  boot $n "wait@300:KERNEL PANIC"
  sleep 2
  python3 scripts/qmp.py "$QMP" screendump "$T/crash-$stage.png" >/dev/null 2>&1 || true
  stop
  check "^panicked at .*crash-test: a deliberate panic, $stage|crash-test: a deliberate panic, $stage" "boot $n: the deliberate panic ($stage)"
  if [ -s "$T/crash-$stage.png" ]; then
    # Bottom right: the console's background (0x161513), whatever was on the screen before.
    case "$(pixels "$T/crash-$stage.png" 1000,750 1010,740)" in
      "22,21,19 22,21,19") ok "boot $n: the crash screen is a text console (dark background where the desktop was)" ;;
      *) bad "boot $n: the screen is not the text console: $(pixels "$T/crash-$stage.png" 1000,750 1010,740)" ;;
    esac
  else
    bad "boot $n: no screendump"
  fi
done
# ---- boots 6 and 7: power off and restart through the power manager ----
python3 scripts/pi_dtb.py "$T/power.dtb" --cmdline "milk.crashtest=late-off"
DTB_ARGS=(-dtb "$T/power.dtb")
boot 6 "wait@300:wm: shutting down"
gone=0
for _ in $(seq 150); do kill -0 "$QPID" 2>/dev/null || { gone=1; break; }; sleep 0.2; done
[ "$gone" = 1 ] && ok "boot 6: Shut Down powered the machine off (qemu exited by itself)" || bad "boot 6: the machine did not power off"
stop
python3 scripts/pi_dtb.py "$T/power.dtb" --cmdline "milk.crashtest=late-reset"
boot 7 "wait@300:wm: restarting" "wait@300:milkOS [0-9][^ ]* .*booting"
stop
check 'wm: restarting \(power manager, full reset\)' "boot 7: Restart reset the machine"
[ "$(tr -d '\r' < "$LOG" | grep -ac 'booting\.\.\.')" -ge 2 ] && ok "boot 7: the kernel booted a second time" || bad "boot 7: no second boot"

# ---- boot 8 (only if MILK_REAL_DTB names a device tree from a real board's boot partition: it is not in the repository) ----
if [ -n "${MILK_REAL_DTB:-}" ] && [ -r "$MILK_REAL_DTB" ]; then
  DTB_ARGS=(-dtb "$MILK_REAL_DTB")
  boot 8 "wait@300:wm: desktop up"
  stop
  check 'platform: Raspberry Pi 4 \(BCM2711\) \(device tree: raspberrypi,4-model-b\)' "boot 8: the real tree is accepted"
  check '^\[smp\] 4 of 4 CPUs online \(spin table\)' "boot 8: four CPUs from the real tree's spin table"
  check '\[genet\] no Ethernet controller' "boot 8: nodes qemu switched off are skipped (GENET)"
  check '\[pcie\] the device tree has no \(enabled\) PCIe' "boot 8: nodes qemu switched off are skipped (PCIe)"
  has 'KERNEL PANIC|kernel panic' && bad "boot 8: a kernel panic" || ok "boot 8: no panic"
fi

# After the early panic the screen kept the boot log and the green band over it (not cleared, no splash).
case "$(pixels "$T/crash-early.png" 500,2 500,5)" in
  "48,208,96 255,255,255") ok "boot 4: the sign-of-life band is on top (green, then a white line)" ;;
  *) bad "boot 4: no sign-of-life band: $(pixels "$T/crash-early.png" 500,2 500,5)" ;;
esac

if [ "$FAIL" = 0 ]; then
  echo "pi-qemu-test: PASS"
  [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ] && { rm -rf "$T" || true; }
else
  echo "pi-qemu-test: FAIL (logs in $T)"; exit 1
fi
