#!/usr/bin/env bash
# Tests of scripts/led-decode.py, the reader of the Raspberry Pi's crash code on the ACT LED: typed forms,
# a pulse list (as measured from a phone video, with jitter) against the documented pattern, the symbol
# lookup. Prints "led-decode: N passed". The Rust side of the same pattern is rpikit-unit.
set -uo pipefail
cd "$(dirname "$0")/.."
ELF=""
for f in target/pi/aarch64-unknown-none-softfloat/release/milkos-kernel target/pi/aarch64-unknown-none-softfloat/debug/milkos-kernel; do
  [ -f "$f" ] && ELF="$f" && break
done
OUT="$(LED_DECODE_ELF="$ELF" PYTHONDONTWRITEBYTECODE=1 python3 -I scripts/led_decode_test.py 2>&1)"; RC=$?
echo "$OUT"
[ "$RC" = 0 ] || { echo "led-decode: FAIL"; exit 1; }
N="$(echo "$OUT" | sed -n 's/^Ran \([0-9]*\) tests.*/\1/p')"
[ "${N:-0}" -gt 0 ] || { echo "led-decode: FAIL (no tests ran)"; exit 1; }
echo "led-decode: $N passed"
