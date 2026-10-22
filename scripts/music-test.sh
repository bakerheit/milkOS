#!/usr/bin/env bash
# The Music app, SILENT: qemu's wav audiodev records what the guest plays (never coreaudio).
#   1. `music` demo: open Music from the dock, Space plays Morning Pasture; the recording
#      must match rootfs/System/Music/morning-pasture.wav sample for sample (after
#      the boot chime), at least 2.5 s of it.
#   2. `musicpause` demo: play, pause 1.5 s later; the recording must hold the start of the
#      tune and stop within ~0.6 s of the pause (what was already queued in the mixer).
# scripts/music-test.sh [workdir]   (screenshots and recordings stay there; default mktemp)
set -euo pipefail
cd "$(dirname "$0")/.."
WORK="${1:-$(mktemp -d /tmp/milk-music-test.XXXXXX)}"
mkdir -p "$WORK"
SRC=rootfs/System/Music/morning-pasture.wav
ok=1
MILK_SOUND_WAV="$WORK/music.wav" scripts/wm-demo.sh music "$WORK/music.png" >/dev/null
python3 -I scripts/music_check.py "$WORK/music.wav" "$SRC" --min-secs 2.5 || ok=0
MILK_SOUND_WAV="$WORK/musicpause.wav" scripts/wm-demo.sh musicpause "$WORK/musicpause.png" >/dev/null
python3 -I scripts/music_check.py "$WORK/musicpause.wav" "$SRC" --min-secs 1.2 --max-secs 2.4 || ok=0
echo "(screenshots and recordings in $WORK)"
[ "$ok" = 1 ] && echo "music-test: PASS" || { echo "music-test: FAIL"; exit 1; }
