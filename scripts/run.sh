#!/usr/bin/env bash
# Build and run milkOS in qemu (4 CPUs; MILK_SMP=1 for one).
#   scripts/run.sh            window (cocoa) + serial on this terminal
#   scripts/run.sh headless   no window, serial only (used for tests)
# RELEASE=1: release kernel. MILK_FEATURES="a b": kernel cargo features (kernel/Cargo.toml).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
cargo build ${RELEASE:+--release} ${MILK_FEATURES:+--features "$MILK_FEATURES"} -q
PROFILE=debug
[ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
# Boot a flat arm64 "Image" (boot.S has the header), not the ELF: qemu only hands
# the kernel a device tree pointer (x0) for Image boots.
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
DISPLAY_ARGS=(-device virtio-gpu-device -display cocoa)
[ "${1:-}" = headless ] && DISPLAY_ARGS=(-display none)
# virtio-input keyboard + tablet (absolute pointer); modern virtio-mmio (qemu defaults to legacy).
INPUT_ARGS=(-global virtio-mmio.force-legacy=false
  -device virtio-keyboard-device -device virtio-tablet-device)
# Scratch disk /tmp/milk-disk.img as virtio-blk (/dev/vda), see scripts/disk.sh.
source scripts/disk.sh
milk_disk_args
# Mac shared folder at /Volumes/Mac (read-only unless MILK_SHARE_RW=1), scripts/share.sh.
source scripts/share.sh
milk_share_args
# eth0: restricted user-mode networking (see scripts/netdev.sh); MILK_HTTP_PORT=8088 to
# reach the guest's web server at http://127.0.0.1:8088/. MILK_V6ONLY=1: a v6-only boot
# (no IPv4 in slirp or on the guest's links).
source scripts/netdev.sh
milk_net_args
# Sound (virtio-sound): off by default. MILK_SOUND=1 plays through the Mac's speakers
# (coreaudio); MILK_SOUND_WAV=/tmp/x.wav records to a WAV file instead (silent).
SOUND_ARGS=()
if [ -n "${MILK_SOUND_WAV:-}" ]; then
  SOUND_ARGS=(-audiodev "wav,id=snd0,path=$MILK_SOUND_WAV,out.frequency=48000"
    -device virtio-sound-device,audiodev=snd0)
elif [ "${MILK_SOUND:-}" = 1 ]; then
  SOUND_ARGS=(-audiodev coreaudio,id=snd0 -device virtio-sound-device,audiodev=snd0)
fi
# MILK_MONITOR=/tmp/x.sock adds a qemu monitor on that unix socket (debugging hangs:
# `echo "info registers -a" | nc -U /tmp/x.sock`).
MON_ARGS=()
[ -n "${MILK_MONITOR:-}" ] && MON_ARGS=(-monitor "unix:$MILK_MONITOR,server,nowait")
# MILK_DRIVERS="rng=kernel sound=user": which driver runs each device, the kernel's or the
# user-space one (default: user for both, the kernel's as the fallback; docs/drivers.md).
[ -n "${MILK_DRIVERS:-}" ] && MON_ARGS+=(-fw_cfg "name=opt/milkos/drivers,string=$MILK_DRIVERS")
exec qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -rtc base=localtime \
  -kernel "$KERNEL" -serial mon:stdio ${MON_ARGS[@]+"${MON_ARGS[@]}"} "${DISPLAY_ARGS[@]}" "${INPUT_ARGS[@]}" "${DISK_ARGS[@]}" "${SHARE_ARGS[@]}" \
  "${NET_ARGS[@]}" ${SOUND_ARGS[@]+"${SOUND_ARGS[@]}"}
