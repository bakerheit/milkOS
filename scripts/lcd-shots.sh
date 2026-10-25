#!/usr/bin/env bash
# Regenerate the 3.5-inch SPI display's screenshots in docs/screenshots/ (docs/raspberry-pi.md): the host
# tests draw the boot log, the crash screen and the desktop through the model of the panel
# (rpikit/src/lcdtests.rs, MILK_LCD_SHOTS=dir makes them write PNGs); this recompresses the ones the
# document shows. No hardware involved.
#
#   scripts/lcd-shots.sh [OUTDIR]       OUTDIR defaults to docs/screenshots
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
OUT="${1:-docs/screenshots}"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/milk-lcd-shots.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
HOST="$(rustc -vV | sed -n 's/^host: //p')"
MILK_LCD_SHOTS="$TMP" cargo test -q -p rpikit --lib --target "$HOST" --target-dir target/host-tests lcdtests >/dev/null
for n in boot-log crash-screen desktop desktop-pan probe-number probe-card; do
  python3 -I scripts/png_pack.py "$TMP/raspberry-pi-lcd-$n.png" "$OUT/raspberry-pi-lcd-$n.png"
done
ls -l "$OUT"/raspberry-pi-lcd-*.png
