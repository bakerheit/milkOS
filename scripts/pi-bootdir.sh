#!/usr/bin/env bash
# Assemble the files a Raspberry Pi 4's SD card boot partition needs to start milkOS, into a folder
# of your choosing (docs/raspberry-pi.md, "Putting it on a card"). This script only ever writes into
# that folder; it refuses a folder on a mounted volume or under /dev, so it cannot touch a card. Copying
# the files onto the card is yours to do.
#
#   scripts/pi-bootdir.sh OUTDIR            a release build (smaller and faster than debug)
#   DEBUG=1 scripts/pi-bootdir.sh OUTDIR    the debug profile
#
# OUTDIR gets: milkos8.img (the kernel the firmware loads), config.milkos.txt and cmdline.milkos.txt
# (what to put in config.txt / cmdline.txt, or use instead of them).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
OUT="${1:?usage: scripts/pi-bootdir.sh OUTDIR}"
case "$(cd "$(dirname "$OUT")" 2>/dev/null && pwd -P)/$(basename "$OUT")" in
  /Volumes/*|/dev/*) echo "pi-bootdir: refusing $OUT: it is on a mounted volume or under /dev (write to a scratch folder, copy to the card yourself)" >&2; exit 2 ;;
esac
mkdir -p "$OUT"
[ -n "${DEBUG:-}" ] && unset RELEASE || export RELEASE=1
scripts/pi-build.sh "$OUT/milkos8.img" >/dev/null
cat > "$OUT/config.milkos.txt" <<'CFG'
# milkOS on a Raspberry Pi 4 (docs/raspberry-pi.md). Replace the file's contents with this (keep a copy
# of the old config.txt to boot the other system again), or add these lines to an [all] section.
arm_64bit=1
kernel=milkos8.img
# A screen even if the monitor is slow to answer, no overscan border, one framebuffer, no KMS overlay:
# milkOS draws on the firmware's framebuffer.
hdmi_force_hotplug=1
disable_overscan=1
max_framebuffers=1
# The serial console on GPIO 14/15 (the mini UART), 115200 8N1, for anyone who has a cable.
enable_uart=1
CFG
cat > "$OUT/cmdline.milkos.txt" <<'CMD'
milk.fbscale=1
CMD
echo "pi-bootdir: $(ls -1 "$OUT" | tr '\n' ' ')in $OUT"
ls -la "$OUT"
