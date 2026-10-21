#!/usr/bin/env bash
# Build and run milkOS in qemu (4 CPUs; MILK_SMP=1 for one).
#   scripts/run.sh            window (cocoa) + serial on this terminal
#   scripts/run.sh headless   no window, serial only (used for tests)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
cargo build ${RELEASE:+--release} -q
PROFILE=debug
[ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
# Boot a flat arm64 "Image" (boot.S has the header), not the ELF: qemu only hands
# the kernel a device tree pointer (x0) for Image boots.
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
DISPLAY_ARGS=(-device ramfb -display cocoa)
[ "${1:-}" = headless ] && DISPLAY_ARGS=(-display none)
# virtio-input keyboard + tablet (absolute pointer); modern virtio-mmio (qemu defaults to legacy).
INPUT_ARGS=(-global virtio-mmio.force-legacy=false
  -device virtio-keyboard-device -device virtio-tablet-device)
# Scratch disk /tmp/milk-disk.img as virtio-blk (/dev/vda), see scripts/disk.sh.
source scripts/disk.sh
milk_disk_args
# eth0: restricted user-mode networking (see scripts/netdev.sh); MILK_HTTP_PORT=8088 to
# reach the guest's web server at http://127.0.0.1:8088/.
source scripts/netdev.sh
milk_net_args
exec qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -rtc base=localtime \
  -kernel "$KERNEL" -serial mon:stdio "${DISPLAY_ARGS[@]}" "${INPUT_ARGS[@]}" "${DISK_ARGS[@]}" \
  "${NET_ARGS[@]}"
