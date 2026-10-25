#!/usr/bin/env bash
# Make the blank log file the kernel writes its log into (docs/raspberry-pi.md, "Reading the log"): MILKLOG.TXT in
# OUTDIR, one line of text and then spaces. Copy it onto the SD card's boot partition (the FAT one with config.txt)
# with the other boot files. The kernel never creates a file or changes the card's directories: it only overwrites
# sectors of this existing file, in place. Like pi-bootdir.sh, this only ever writes into OUTDIR and refuses a
# mounted volume.
#
#   scripts/pi-logfile.sh OUTDIR [SIZE]     SIZE: 8M (the default), 64K, 32M ... or a number of KiB. 16K to 32M.
#
# The log is a ring that keeps the text of many boots one after the other, newest last; when it is full the oldest
# text is overwritten. 8 MiB holds a few days of an idle board, or hours of a busy one. A smaller file wraps sooner.
#
# Copy it with the Finder (or `cp`) as an ordinary file, uppercase name MILKLOG.TXT. The card's FAT must not be
# defragmented or the file rewritten by anything between this copy and the run (a new copy of the file is fine; the
# kernel looks the file up each boot). To start a new log, copy a fresh blank file over it.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:?usage: scripts/pi-logfile.sh OUTDIR [SIZE]}"
SIZE="${2:-8M}"
case "$(cd "$(dirname "$OUT")" 2>/dev/null && pwd -P)/$(basename "$OUT")" in
  /Volumes/*|/dev/*) echo "pi-logfile: refusing $OUT: it is on a mounted volume or under /dev (write to a scratch folder, copy to the card yourself)" >&2; exit 2 ;;
esac
case "$SIZE" in
  *[Mm]) KIB=$(( ${SIZE%[Mm]} * 1024 )) ;;
  *[Kk]) KIB="${SIZE%[Kk]}" ;;
  *) KIB="$SIZE" ;;
esac
case "$KIB" in ''|*[!0-9]*) echo "pi-logfile: SIZE must be like 8M, 64K or a number of KiB" >&2; exit 2 ;; esac
[ "$KIB" -ge 16 ] && [ "$KIB" -le 32768 ] || { echo "pi-logfile: SIZE must be 16K..32M" >&2; exit 2; }
mkdir -p "$OUT"
python3 -I - "$OUT/MILKLOG.TXT" "$KIB" <<'PY'
import sys
path, kib = sys.argv[1], int(sys.argv[2])
size = kib * 1024
head = b"MILKLOG.TXT: the milkOS log goes here (the kernel overwrites this file in place; nothing below yet).\n"
with open(path, "wb") as f:
    f.write((head + b" " * size)[:size])
PY
echo "pi-logfile: $OUT/MILKLOG.TXT ($((KIB * 1024)) bytes)"
