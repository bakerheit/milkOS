#!/usr/bin/env bash
# Unit tests for user/imgdec on the Mac itself (Preview's decoders: PNG with every colour type,
# depth and filter plus Adam7 (fixtures from scripts/gen_png_fixtures.py) and the kernel's own
# screenshot encoder, PPM/PGM/PBM, BMP, scaling, and truncated / mutated / hostile files).
# Prints "imgdec-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" imgdec imgdec-unit
