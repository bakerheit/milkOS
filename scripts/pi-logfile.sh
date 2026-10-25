#!/usr/bin/env bash
# Make the blank log file the kernel saves its boot log into (docs/raspberry-pi.md, "The boot log in a file"):
# MILKLOG.TXT in OUTDIR, a header line and then spaces. Copy it onto the SD card's boot partition (the FAT one
# with config.txt) with the other boot files. The kernel never creates a file or changes the card's
# directories: it only overwrites the data sectors of this existing file, in place. Like pi-bootdir.sh, this only
# ever writes into OUTDIR and refuses a mounted volume.
#
#   scripts/pi-logfile.sh OUTDIR [KIB]      KIB defaults to 256 (a boot is well under 100 KiB)
#
# Copy it with the Finder (or `cp`) as an ordinary file, uppercase name MILKLOG.TXT. The card's FAT must not be
# defragmented or the file rewritten by anything between this copy and the run (a new copy of the file is fine; the
# kernel looks the file up each boot).
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:?usage: scripts/pi-logfile.sh OUTDIR [KIB]}"
KIB="${2:-256}"
case "$(cd "$(dirname "$OUT")" 2>/dev/null && pwd -P)/$(basename "$OUT")" in
  /Volumes/*|/dev/*) echo "pi-logfile: refusing $OUT: it is on a mounted volume or under /dev (write to a scratch folder, copy to the card yourself)" >&2; exit 2 ;;
esac
case "$KIB" in ''|*[!0-9]*) echo "pi-logfile: KIB must be a number" >&2; exit 2 ;; esac
[ "$KIB" -ge 16 ] && [ "$KIB" -le 4096 ] || { echo "pi-logfile: KIB must be 16..4096" >&2; exit 2; }
mkdir -p "$OUT"
python3 -I - "$OUT/MILKLOG.TXT" "$KIB" <<'PY'
import sys
path, kib = sys.argv[1], int(sys.argv[2])
size = kib * 1024
head = b"MILKLOG.TXT: the milkOS boot log goes here (the kernel overwrites this file in place; nothing below yet).\n"
with open(path, "wb") as f:
    f.write((head + b" " * size)[:size])
PY
echo "pi-logfile: $OUT/MILKLOG.TXT ($((KIB * 1024)) bytes)"
