#!/usr/bin/env bash
# The Pixel 2 host tools on made-up inputs, no VM and no phone: scripts/android-bootimg.py (the boot image
# header, its sections, the appended device tree, unpack), scripts/dtb_build.py (the device tree writer
# and reader against the Rust writer's golden bytes, the small Pixel 2 tree, the dtbo table listing). The Rust half
# (the same header and tree layouts, the options, the pattern, RAM carving) is in rpikit-unit.
# Prints "pixel2-tools: N passed". If MILK_PIXEL2_STOCK names a folder with `boot.img` and `dtbo.img` that you
# extracted from your own phone, the stock header is also repacked to the identical bytes (the files are
# read, never copied anywhere). See docs/pixel2.md.
set -uo pipefail
cd "$(dirname "$0")/.."
OUT="$(python3 -I scripts/android_tools_test.py 2>&1)"; RC=$?
echo "$OUT" | grep -E "^(FAIL|ERROR|Ran |OK|android-tools|AssertionError|Traceback)|Error" | head -40
if [ "$RC" != 0 ]; then echo "pixel2-tools: FAIL"; exit 1; fi
N="$(echo "$OUT" | sed -n 's/^android-tools: \([0-9]*\) passed/\1/p')"
[ "${N:-0}" -gt 0 ] || { echo "pixel2-tools: FAIL (no tests ran)"; exit 1; }
echo "pixel2-tools: $N passed"
