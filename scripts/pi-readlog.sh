#!/usr/bin/env bash
# Read the milkOS SD card log (MILKLOG.TXT, copied off the card or read from the mounted card):
#
#   scripts/pi-readlog.sh PATH                 the whole log, every boot in order
#   scripts/pi-readlog.sh PATH --last          just the newest boot
#   scripts/pi-readlog.sh PATH --boot N        just boot N
#   scripts/pi-readlog.sh PATH --list          one line per boot: where it starts, lines, uptime, build, how it ended
#                                              (clean shutdown, restart, crash, or a power cut / reset)
#   scripts/pi-readlog.sh PATH --tail 200      only the last 200 lines (of the whole log, or of --last / --boot N)
#
# The log goes to stdout, notes and warnings (the ring wrapped, damaged sectors, how the newest boot ended) to stderr:
#   scripts/pi-readlog.sh /Volumes/BOOT/MILKLOG.TXT --last > last-boot.log
# Read only. Exit status 0: read and whole; 1: read, with a warning (wrapped, damage, an old format 1 file without its
# end marker); 2: not a milkOS log / unreadable. The format is described in rpikit/src/milklog.rs.
set -euo pipefail
[ $# -ge 1 ] || { echo "usage: scripts/pi-readlog.sh PATH [--all | --last | --boot N | --list] [--tail N]" >&2; exit 2; }
# No cd: PATH is relative to where this was run from.
exec python3 -I "$(dirname "$0")/pi_readlog.py" "$@"
