#!/usr/bin/env bash
# Run the Raspberry Pi 4 kernel on qemu's `-M raspi4b` (docs/raspberry-pi.md): the desktop in a window (cocoa)
# and the serial console (PL011) on this terminal.
#   scripts/pi-run.sh            window + serial here
#   scripts/pi-run.sh headless   serial only (the desktop still runs, unseen)
#   MILK_SD=/tmp/x.img           the emulated SD card (default /tmp/milk-pi-sd.img, a milkfs image made on first use)
#   MILK_CMDLINE='milk.selftest=full'   the kernel command line (milk.<option>=<value>, see docs/raspberry-pi.md)
#   MILK_MONITOR=/tmp/x.sock     a qemu monitor socket
#   RELEASE=1                    the release build
# raspi4b runs on TCG (no hvf), 4 cores, exactly 2 GiB, no network and no USB (qemu does not model them: the Pi's
# Ethernet and USB controller are covered by the code, not by this machine); a keyboard cannot be typed into the window,
# the serial console is the way in. Only the qemu started here is stopped (Ctrl-A X in the serial console, or Ctrl-C).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-pirun.XXXXXX)"
trap 'rm -rf "$T"' EXIT
scripts/pi-build.sh "$T/kernel8.img" >/dev/null
python3 scripts/pi_dtb.py "$T/pi.dtb" --cmdline "${MILK_CMDLINE:-}"
SD="${MILK_SD:-/tmp/milk-pi-sd.img}"
[ -e "$SD" ] || python3 scripts/mkmilkfs.py mkfs "$SD" --size 64M --label Milk >/dev/null
DISPLAY_ARGS=(-display cocoa)
[ "${1:-}" = headless ] && DISPLAY_ARGS=(-display none)
MON=()
[ -n "${MILK_MONITOR:-}" ] && MON=(-monitor "unix:$MILK_MONITOR,server,nowait")
qemu-system-aarch64 -M raspi4b -kernel "$T/kernel8.img" -dtb "$T/pi.dtb" -append "${MILK_CMDLINE:-}" \
  -serial mon:stdio "${DISPLAY_ARGS[@]}" ${MON[@]+"${MON[@]}"} -drive "file=$SD,if=sd,format=raw"
