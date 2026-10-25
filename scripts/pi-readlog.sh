#!/usr/bin/env bash
# Print the boot log out of MILKLOG.TXT (copied off the SD card, or read from the mounted card) without the padding,
# and say whether the kernel finished saving it (the end marker). The log goes to stdout, the verdict to stderr:
#
#   scripts/pi-readlog.sh PATH-TO-MILKLOG.TXT > boot.log
#
# Read only. Exit status 0: marker there; 1: log without marker (cut off); 2: not a milkOS log / unreadable.
set -euo pipefail
cd "$(dirname "$0")/.."
[ $# -eq 1 ] || { echo "usage: scripts/pi-readlog.sh PATH-TO-MILKLOG.TXT" >&2; exit 2; }
exec python3 -I scripts/pi_readlog.py "$1"
