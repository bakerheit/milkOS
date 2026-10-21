#!/bin/bash
# milkOS @VERSION@: double-click in Finder to start it (or run ./milkOS.command).
#
#   ./milkOS.command              a window with the milkOS desktop; this Terminal window
#                                 shows the console (msh shell + kernel messages)
#   ./milkOS.command --sound      also play sound through the Mac's speakers
#   ./milkOS.command --headless   no window, console only (for tests and SSH sessions)
#   --serial SPEC / --monitor SPEC   pass qemu -serial / -monitor (for test scripts)
#
# Everything it uses is in this folder: kernel.bin, disks/milk.img (your files; it keeps
# what you save), disks/scratch.img, share/ (visible in milkOS as /Volumes/Mac,
# read-only). Needs QEMU (brew install qemu). Network: milkOS can't reach the Mac or the
# internet (qemu "restrict" mode). To quit: close the window, or type Ctrl-A then X here.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE" || exit 1

HEADLESS=0
SOUND=0
SERIAL="mon:stdio"
MONITOR=()
say() { printf '%s\n' "$*" >&2; }
pause() { [ -t 0 ] && [ "$HEADLESS" = 0 ] && { printf 'Press Return to close this window. '; read -r _; }; }
while [ $# -gt 0 ]; do
  case "$1" in
    --headless) HEADLESS=1 ;;
    --sound) SOUND=1 ;;
    --serial) SERIAL="${2:?--serial needs a value}"; shift ;;
    --monitor) MONITOR=(-monitor "${2:?--monitor needs a value}"); shift ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) say "milkOS: unknown option $1 (try --help)"; exit 2 ;;
  esac
  shift
done

QEMU=""
for q in "$(command -v qemu-system-aarch64 2>/dev/null)" /opt/homebrew/bin/qemu-system-aarch64 \
         /usr/local/bin/qemu-system-aarch64 /opt/local/bin/qemu-system-aarch64; do
  if [ -n "$q" ] && [ -x "$q" ]; then QEMU="$q"; break; fi
done
if [ -z "$QEMU" ]; then
  say "milkOS needs QEMU, which isn't installed."
  say "Install Homebrew (https://brew.sh), then in Terminal run:  brew install qemu"
  say "and double-click milkOS.command again."
  pause
  exit 1
fi
for f in kernel.bin disks/milk.img disks/scratch.img; do
  if [ ! -f "$f" ]; then
    say "milkOS: $HERE/$f is missing (unpack the whole milkOS folder again)."
    pause
    exit 1
  fi
done
mkdir -p share

if [ "$(uname -m)" = arm64 ]; then
  ACCEL=(-accel hvf -cpu host)
else
  say "milkOS: this Mac isn't Apple Silicon; running in (slow, untested) emulation."
  ACCEL=(-accel tcg -cpu cortex-a72)
fi
if [ "$HEADLESS" = 1 ]; then
  DISPLAY_ARGS=(-display none)
else
  DISPLAY_ARGS=(-device ramfb -display cocoa)
fi
SOUND_ARGS=()
[ "$SOUND" = 1 ] && SOUND_ARGS=(-audiodev coreaudio,id=snd0 -device virtio-sound-device,audiodev=snd0)

[ "$HEADLESS" = 0 ] && say "Starting milkOS @VERSION@ (a window opens; this one is its console)..."
exec "$QEMU" -name "milkOS @VERSION@" -M virt "${ACCEL[@]}" -smp 4 -m 512M -rtc base=localtime \
  -kernel kernel.bin -serial "$SERIAL" "${MONITOR[@]+"${MONITOR[@]}"}" "${DISPLAY_ARGS[@]}" \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  -drive file=disks/scratch.img,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 \
  -drive file=disks/milk.img,if=none,format=raw,id=hd1 -device virtio-blk-device,drive=hd1 \
  -fsdev local,id=fs0,path=share,security_model=mapped-xattr,readonly=on \
  -device virtio-9p-device,fsdev=fs0,mount_tag=mac -fw_cfg name=opt/milkos/share-rw,string=0 \
  -netdev user,id=n0,restrict=on -device virtio-net-device,netdev=n0 \
  "${SOUND_ARGS[@]+"${SOUND_ARGS[@]}"}"
