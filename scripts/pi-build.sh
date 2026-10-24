#!/usr/bin/env bash
# Build the Raspberry Pi 4 kernel image (docs/raspberry-pi.md): the same sources as the qemu
# virt kernel, feature `raspi`, which links the image for the Pi firmware's load address
# (0x80000). Builds into target/pi (its own cargo target directory, so the virt build in
# target/ is not rebuilt back and forth) and writes the flat image the firmware loads.
#
#   scripts/pi-build.sh [OUT]       OUT defaults to target/pi/kernel8.img
#   RELEASE=1 scripts/pi-build.sh   the release profile
#
# kernel8.img goes on the boot partition of the SD card next to the firmware's own files
# (config.txt: arm_64bit=1, kernel=kernel8.img); that step is the person's, not this script's:
# nothing here writes to any disk or card. qemu runs the same file: scripts/pi-run.sh.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
PROFILE=debug
[ -n "${RELEASE:-}" ] && PROFILE=release
cargo build -q ${RELEASE:+--release} --features raspi --target-dir target/pi
ELF="target/pi/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
OUT="${1:-target/pi/kernel8.img}"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$OUT"
echo "pi-build: $OUT ($(wc -c < "$OUT" | tr -d ' ') bytes)"
