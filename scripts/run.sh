#!/usr/bin/env bash
# Build and run milkOS in qemu.
#   scripts/run.sh            window (cocoa) + serial on this terminal
#   scripts/run.sh headless   no window, serial only (used for tests)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
cargo build ${RELEASE:+--release} -q
ELF="target/aarch64-unknown-none-softfloat/${RELEASE:+release}${RELEASE:-debug}/milkos-kernel"
# Boot a flat arm64 "Image" (boot.S has the header), not the ELF: qemu only hands
# the kernel a device tree pointer (x0) for Image boots.
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
DISPLAY_ARGS=(-device ramfb -display cocoa)
[ "${1:-}" = headless ] && DISPLAY_ARGS=(-display none)
exec qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -serial mon:stdio "${DISPLAY_ARGS[@]}"
