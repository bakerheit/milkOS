#!/usr/bin/env bash
# Assemble the files a Raspberry Pi 4's SD card boot partition needs to start milkOS, into a folder
# of your choosing (docs/raspberry-pi.md, "Putting it on a card"). This script only ever writes into
# that folder; it refuses a folder on a mounted volume or under /dev, so it cannot touch a card. Copying
# the files onto the card is yours to do.
#
#   scripts/pi-bootdir.sh OUTDIR [--display hdmi800x480|auto]    a release build (smaller and faster than debug)
#   DEBUG=1 scripts/pi-bootdir.sh OUTDIR ...                     the debug profile
#
# --display says which screen the files are for (docs/raspberry-pi.md, "HDMI displays"):
#   auto         (the default) an HDMI monitor or television that tells the firmware what it can show (EDID):
#                no video mode is forced. cmdline.milkos.txt also turns on the 3.5-inch SPI display
#                (milk.lcd=ili9486), which does nothing when no such display is wired.
#   hdmi800x480  the 4-inch 800x480 HDMI panel with an SPI touch chip (HDMI Display-C, XPT2046): config.milkos.txt
#                has a "this display" block that forces the standard 1024x768 mode (the panel's scaler shows it
#                at 800x480), cmdline.milkos.txt asks for a 5:3 1280x768 framebuffer so the picture is not
#                squashed and turns on the touch chip (milk.touch=xpt2046); no SPI panel driver.
#
# OUTDIR gets: milkos8.img (the kernel the firmware loads), config.milkos.txt and cmdline.milkos.txt
# (what to put in config.txt / cmdline.txt, or use instead of them: edit them on the card to change the
# options), and MILKLOG.TXT (the blank 8 MiB file the kernel writes its log into, every boot after the
# last: copy it to the card with the rest; scripts/pi-logfile.sh makes it alone, in another size if you
# like, scripts/pi-readlog.sh reads it back).
# MILK_KERNEL_IMG=FILE uses that file as the kernel image instead of building one (for the tests of this script).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
USAGE="usage: scripts/pi-bootdir.sh OUTDIR [--display hdmi800x480|auto]"
OUT=""
DISPLAY_KIND=auto
while [ $# -gt 0 ]; do
  case "$1" in
    --display) [ $# -ge 2 ] || { echo "pi-bootdir: --display needs a value (hdmi800x480 or auto). $USAGE" >&2; exit 2; }; DISPLAY_KIND="$2"; shift 2 ;;
    --display=*) DISPLAY_KIND="${1#--display=}"; shift ;;
    -h|--help) echo "$USAGE"; exit 0 ;;
    -*) echo "pi-bootdir: unknown option $1. $USAGE" >&2; exit 2 ;;
    *) [ -z "$OUT" ] || { echo "pi-bootdir: more than one OUTDIR. $USAGE" >&2; exit 2; }; OUT="$1"; shift ;;
  esac
done
[ -n "$OUT" ] || { echo "$USAGE" >&2; exit 2; }
case "$DISPLAY_KIND" in
  auto|hdmi800x480) ;;
  *) echo "pi-bootdir: --display must be hdmi800x480 or auto, not '$DISPLAY_KIND'" >&2; exit 2 ;;
esac
case "$(cd "$(dirname "$OUT")" 2>/dev/null && pwd -P)/$(basename "$OUT")" in
  /Volumes/*|/dev/*) echo "pi-bootdir: refusing $OUT: it is on a mounted volume or under /dev (write to a scratch folder, copy to the card yourself)" >&2; exit 2 ;;
esac
mkdir -p "$OUT"
if [ -n "${MILK_KERNEL_IMG:-}" ]; then
  cp "$MILK_KERNEL_IMG" "$OUT/milkos8.img"
else
  [ -n "${DEBUG:-}" ] && unset RELEASE || export RELEASE=1
  scripts/pi-build.sh "$OUT/milkos8.img" >/dev/null
fi
# ---- config.milkos.txt: the same for every display, then the block for the display chosen ----
cat > "$OUT/config.milkos.txt" <<'CFG'
# milkOS on a Raspberry Pi 4 (docs/raspberry-pi.md). Replace the file's contents with this (keep a copy
# of the old config.txt to boot the other system again), or add these lines to an [all] section.
arm_64bit=1
kernel=milkos8.img
# A screen even if the monitor is slow to answer, no overscan border, one framebuffer, and NO KMS overlay (no
# dtoverlay=vc4-kms-v3d / vc4-fkms-v3d line): milkOS draws on the firmware's own framebuffer.
hdmi_force_hotplug=1
disable_overscan=1
max_framebuffers=1
# The serial console on GPIO 14/15 (the mini UART), 115200 8N1, for anyone who has a cable.
enable_uart=1
CFG
if [ "$DISPLAY_KIND" = hdmi800x480 ]; then
  cat >> "$OUT/config.milkos.txt" <<'CFG'

# ======================= this display: the 4-inch 800x480 HDMI panel (HDMI Display-C) =======================
# The panel is an HDMI input (micro-HDMI on the Pi to the full-size HDMI socket on its edge; power for the
# panel on its micro-USB; its GPIO socket carries 5 V and the touch chip's SPI lines). Its scaler board
# takes the standard 1024x768 mode and shows it scaled to 800x480 (the custom 800x480 timing below was
# refused by a real panel: "Not Support"), and it has no EDID the firmware can use, so the mode is forced.
# The picture's proportions come from the kernel's framebuffer size: cmdline.txt asks for 1280x768 (5:3, the
# panel's own shape), which the firmware scales to this mode and the panel scales to 800x480.
# If the picture is not right, change ONE thing, reboot, look again (docs/raspberry-pi.md, "HDMI displays").
hdmi_group=2
# 2 = the "computer monitor" (DMT) family of modes.
hdmi_mode=16
# 16 = 1024x768 at 60 Hz (DMT).
hdmi_drive=2
# 2 = HDMI with audio (1 = plain DVI signal, no audio over the link).

# ---- other modes to try, in this order, if the panel says "Not Support" or shows nothing (remove the # of ONE
# ---- group of lines, and comment out the three mode lines above: hdmi_group, hdmi_mode, hdmi_drive) ----
# The custom 800x480 timing (width height refresh-Hz aspect(6 = 15:9) margins interlace reduced-blanking). Some panels
# refuse it. With it, ask for the panel's own size in cmdline.txt too: milk.fbsize=800x480 (or leave it out: the firmware
# says 800x480 and the kernel follows).
#hdmi_group=2
#hdmi_mode=87
#hdmi_cvt=800 480 60 6 0 0 0
#hdmi_drive=1
# 720p (1280x720), which nearly every scaler board accepts:
#hdmi_group=1
#hdmi_mode=4
#hdmi_drive=2
# 800x600 at 60 Hz (DMT):
#hdmi_group=2
#hdmi_mode=9
#hdmi_drive=2
# 640x480 at 60 Hz (CEA), the mode every HDMI display has to take:
#hdmi_group=1
#hdmi_mode=1
#hdmi_drive=2

# ---- picture problems (remove the # of ONE line) ----
# Flicker, dropouts, a blank or "no signal" panel with a cable that is long or poor: a stronger signal (1..11, the default is 5):
#config_hdmi_boost=7
# ... or refuse any mode with a pixel clock above 40 MHz so the firmware picks a gentler one:
#hdmi_pixel_freq_limit=40000000
# Explicit timings instead of a mode (the usual 800x480 panel: h-active 800, front 40, sync 48, back 88; v-active 480,
# front 13, sync 3, back 32; 60 Hz, 32 MHz pixel clock). Fields: h_active_pixels h_sync_polarity h_front_porch
# h_sync_pulse h_back_porch v_active_lines v_sync_polarity v_front_porch v_sync_pulse v_back_porch
# v_sync_offset_a v_sync_offset_b pixel_rep frame_rate interlaced pixel_freq aspect_ratio (use with hdmi_group=2 hdmi_mode=87):
#hdmi_timings=800 0 40 48 88 480 0 13 3 32 0 0 0 60 0 32000000 3
# Picture shifted by a few pixels or cut at an edge: let the firmware keep a border and move it (set disable_overscan=0 above first):
#overscan_left=8
#overscan_right=8
#overscan_top=8
#overscan_bottom=8
# Picture upside down or sideways (the glass is fitted the other way up): the firmware can turn the HDMI output (0 none,
# 1 = 90, 2 = 180, 3 = 270 degrees; documented for the firmware's own framebuffer, which is what milkOS uses, but not
# verified on a Pi 4 here: if it does nothing, the panel has to be turned by hand). The touch needs the same turn:
# milk.touch_rot=180 in cmdline.txt for 2 (90 / 270 for the quarter turns), or milk.touch_cal=ask.
#display_hdmi_rotate=2
# ==============================================================================================================
CFG
fi
# ---- cmdline.milkos.txt: one line (the firmware reads cmdline.txt as one line) ----
if [ "$DISPLAY_KIND" = hdmi800x480 ]; then
  # milk.touch=xpt2046 reads the panel's touch chip on SPI0 (docs/raspberry-pi.md, "HDMI displays"). No milk.lcd: that is the driver
  # for SPI panels, and this panel is not one; with it nothing on the header's display pins is touched. milk.fbsize=1280x768 is the
  # panel's own shape (5:3) at a size the desktop is made for: the firmware shows it in the 1024x768 mode of config.milkos.txt (the
  # firmware cannot say what shape the panel behind its scaler has), and the panel squeezes that to 800x480 without distorting it.
  # With the custom 800x480 mode instead, use milk.fbsize=800x480 or leave the option out (the firmware reports 800x480).
  cat > "$OUT/cmdline.milkos.txt" <<'CMD'
milk.fbscale=1 milk.fbsize=1280x768 milk.touch=xpt2046
CMD
else
  # milk.lcd=ili9486 turns on the 3.5-inch SPI display on the GPIO header (docs/raspberry-pi.md, "3.5-inch SPI display"): the boot log,
  # crash screen, desktop and touch. Without the option nothing on the header is touched; with it and no display there, the bytes just go
  # nowhere and the boot is not held up. If the panel is another controller: milk.lcd=ili9488 or milk.lcd=st7796.
  cat > "$OUT/cmdline.milkos.txt" <<'CMD'
milk.fbscale=1 milk.lcd=ili9486
CMD
fi
scripts/pi-logfile.sh "$OUT" 8M >/dev/null
echo "pi-bootdir: $(ls -1 "$OUT" | tr '\n' ' ')in $OUT (display: $DISPLAY_KIND)"
ls -la "$OUT"
