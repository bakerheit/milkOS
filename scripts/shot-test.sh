#!/usr/bin/env bash
# Screenshots end to end, on private disk images (never the shared ones):
#   1. `shot` demo: Cmd+Shift+3 saves ~/Pictures/Screenshot ....png on the milkfs disk;
#   2. `shotregion`: Cmd+Shift+4 + a drag saves a 385x301 region.
# After each boot the PNG is pulled out of the image with mkmilkfs.py, checked with
# png_check.py (CRCs, zlib, filters; a decoder independent of ours) and by macOS's own
# `sips`, and compared with qemu's screendump of the same screen. Skipped: the toast
# (top right; the screendump has it, the screenshot doesn't), the menu bar's live cpu meter and
# clock, and the Terminal's text (the
# kernel log scrolls on, e.g. with the screenshot's own log line); only the cursor (12x19,
# in the screendump, not in the screenshot) may differ elsewhere. Sound goes to a WAV
# (never the speakers).
#   scripts/shot-test.sh [workdir]
set -euo pipefail
cd "$(dirname "$0")/.."
WORK="${1:-$(mktemp -d /tmp/milk-shot-test.XXXXXX)}"
mkdir -p "$WORK"
ok=1
for demo in shot shotregion; do
  rm -f "$WORK/fs.img" "$WORK/disk.img"
  WM_DISK_RW=1 MILK_FS="$WORK/fs.img" MILK_DISK="$WORK/disk.img" MILK_SOUND_WAV="$WORK/$demo.wav" \
    scripts/wm-demo.sh "$demo" "$WORK/$demo-qemu.png" >/dev/null
  name=$(python3 -I scripts/mkmilkfs.py ls "$WORK/fs.img" /home/milk/Pictures | awk '/Screenshot/ { $1=$2=$3=""; sub(/^ +/, ""); print; exit }')
  [ -n "$name" ] || { echo "$demo: FAIL no screenshot in ~/Pictures"; ok=0; continue; }
  python3 -I scripts/mkmilkfs.py cat "$WORK/fs.img" "/home/milk/Pictures/$name" >"$WORK/$demo.png"
  sips -g pixelWidth -g pixelHeight "$WORK/$demo.png" | tail -2 | tr '\n' ' '; echo
  at=0,0; [ "$demo" = shotregion ] && at=300,90
  out=$(python3 -I scripts/png_check.py "$WORK/$demo.png" "$WORK/$demo-qemu.png" --at "$at" \
    --skip 690,30,334,110 --skip 441,304,518,255 --skip 860,0,164,30)
  echo "$out" | grep -v '^DIFF'
  read -r _ diff total < <(echo "$out" | grep '^DIFF')
  # Allowed: the cursor (12x19) and, full screen, the toast (~280x70 top right).
  limit=240
  if [ "$diff" -le "$limit" ]; then echo "$demo: PASS ($diff px differ, limit $limit)"; else echo "$demo: FAIL ($diff px differ)"; ok=0; fi
done
echo "(files in $WORK)"
[ "$ok" = 1 ] && echo "shot-test: PASS" || { echo "shot-test: FAIL"; exit 1; }
