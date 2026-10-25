#!/usr/bin/env bash
# Build the Pixel 2 (Snapdragon 835) boot images of stage 1 (docs/pixel2.md): the kernel with feature
# `msm8998` (its own cargo target directory, target/android), the flat arm64 Image, and Android boot
# images that `fastboot boot` can run. Nothing here talks to the phone or writes to any disk: the
# images and a README with the exact commands land in OUTDIR.
#
#   scripts/android-build.sh OUTDIR [--stock-dtb FILE] [--debug]
#
# The device tree the bootloader needs appended to the kernel comes from one of two places:
#   --stock-dtb FILE   the base tree of the phone's own boot image, which you extracted yourself (never
#                      kept in this repository): `milkos-pixel2.img` uses it unchanged, the safest
#                      first try; `milkos-pixel2-minimal.img` uses the small tree scripts/dtb_build.py makes;
#   (no option)        `milkos-pixel2.img` uses the small tree.
# Variants of the same kernel with another command line (docs/pixel2.md, "Options"), for the cases
# the first picture shows something wrong, so no rebuild is needed:
#   -pattern   the colour pattern stays up (msm.hold=inf)
#   -rgbx, -xrgb, -xbgr    the other 32-bit pixel orders (msm.fmt=...)
#   -kick      also kicks the display controller ten times a second (msm.kick=ctl0)
#   -gz        the kernel gzip-compressed (the layout of Image.gz-dtb), in case the bootloader refuses a plain Image
#   -mdp       logs a few display controller registers, read only (msm.mdp=dump)
#   -wdt       pets the SoC's watchdog once a second (msm.wdt=pet), in case the phone resets by itself
# Release profile unless --debug.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
OUT=""; STOCK=""; PROFILE=release
while [ $# -gt 0 ]; do
  case "$1" in
    --stock-dtb) STOCK="${2:?--stock-dtb needs a file}"; shift ;;
    --debug) PROFILE=debug ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "android-build: unknown option $1" >&2; exit 2 ;;
    *) [ -z "$OUT" ] || { echo "android-build: one OUTDIR only" >&2; exit 2; }; OUT="$1" ;;
  esac
  shift
done
[ -n "$OUT" ] || { echo "usage: scripts/android-build.sh OUTDIR [--stock-dtb FILE] [--debug]" >&2; exit 2; }
[ -z "$STOCK" ] || [ -f "$STOCK" ] || { echo "android-build: $STOCK is not a file" >&2; exit 2; }
mkdir -p "$OUT"

cargo build -q $([ "$PROFILE" = release ] && echo --release) --features msm8998 --target-dir target/android
ELF="target/android/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$OUT/Image"
echo "android-build: $OUT/Image ($(wc -c < "$OUT/Image" | tr -d ' ') bytes, $PROFILE)"

python3 -I scripts/dtb_build.py minimal -o "$OUT/minimal.dtb"
if [ -n "$STOCK" ]; then
  python3 -I scripts/dtb_build.py stock -i "$STOCK" -o "$OUT/stock.dtb"
  MAIN_DTB="$OUT/stock.dtb"
  python3 -I scripts/android-bootimg.py --kernel "$OUT/Image" --dtb "$OUT/minimal.dtb" -o "$OUT/milkos-pixel2-minimal.img" >/dev/null
else
  MAIN_DTB="$OUT/minimal.dtb"
fi
img() { # name extra-command-line-words
  local name="$1"; shift
  if [ $# -gt 0 ]; then
    python3 -I scripts/android-bootimg.py --kernel "$OUT/Image" --dtb "$MAIN_DTB" --add-cmdline "$*" -o "$OUT/$name.img" >/dev/null
  else
    python3 -I scripts/android-bootimg.py --kernel "$OUT/Image" --dtb "$MAIN_DTB" -o "$OUT/$name.img" >/dev/null
  fi
  echo "android-build: $OUT/$name.img"
}
img milkos-pixel2
python3 -I scripts/android-bootimg.py --kernel "$OUT/Image" --dtb "$MAIN_DTB" --compress gzip -o "$OUT/milkos-pixel2-gz.img" >/dev/null
echo "android-build: $OUT/milkos-pixel2-gz.img"
img milkos-pixel2-pattern msm.hold=inf
img milkos-pixel2-rgbx msm.fmt=rgbx
img milkos-pixel2-xrgb msm.fmt=xrgb
img milkos-pixel2-xbgr msm.fmt=xbgr
img milkos-pixel2-kick msm.kick=ctl0 msm.hold=3
img milkos-pixel2-mdp msm.mdp=dump msm.hold=3
img milkos-pixel2-wdt msm.wdt=pet
python3 -I scripts/android-bootimg.py --unpack "$OUT/milkos-pixel2.img" | sed 's/^/  /'

cat > "$OUT/README.txt" <<'README'
milkOS on the Pixel 2, stage 1: a boot image that draws on the screen.
Nothing is flashed: `fastboot boot` loads the image into RAM and runs it; the phone's own system is
untouched, and a restart brings Android back. The details are in docs/pixel2.md.

Needs the bootloader unlocked (it is) and fastboot (platform-tools).

  adb reboot bootloader                    (or: power off, then hold power + volume down)
  fastboot devices                         (the phone should be listed)
  fastboot boot milkos-pixel2.img

What you should see, in this order:
  1. the usual bootloader warning screen for an unlocked phone (a few seconds): that is the stock
     bootloader, not milkOS;
  2. four big colour blocks with black borders, labelled RED, GREEN, BLUE, WHITE (top left, top
     right, bottom left, bottom right), a white line round the very edge of the screen, a black
     diagonal from the top left corner to the bottom right and a white one the other way. Held for
     about 5 seconds. Tell what you see: do the labels match the colours? Is the picture the right
     way up, complete, are the diagonals straight lines? Is it shifted or sheared?
  3. then dark text on the screen, the boot log (kernel, device tree, memory, CPU, interrupt
     controller, screen), with a green band along the top edge;
  4. then the log stops and the last line of the screen counts: "alive N s". A white marker moves along
     the green band. If those stop, the phone has frozen.
Photograph step 2 and step 4 (and the whole log, if you can read it).

Getting back: hold the power button for about 10 to 15 seconds until the phone restarts; it boots
Android again. (If the phone is still in the bootloader, `fastboot reboot` does the same.)

If something is off, try the variants of the same image (the same kernel, another command line):
  milkos-pixel2-gz.img        the same, with the kernel gzip-compressed the way Image.gz-dtb is (use if the plain
                              one is refused: the phone goes back to the bootloader or restarts at once)
  milkos-pixel2-pattern.img   the colour pattern stays up (no text), to look at it in peace
  milkos-pixel2-rgbx.img      pixel order R,G,B,x instead of B,G,R,x  (use if red and blue are swapped)
  milkos-pixel2-xrgb.img      x,R,G,B
  milkos-pixel2-xbgr.img      x,B,G,R
  milkos-pixel2-kick.img      also pokes the display controller ten times a second (use if the screen
                              stays on the bootloader's picture while the image runs: a panel that
                              needs a refresh command; read docs/pixel2.md first, it writes a register)
  milkos-pixel2-mdp.img       reads (never writes) some display controller registers into the log, to tell
                              how the panel is driven (docs/pixel2.md, "If the screen does not change")
  milkos-pixel2-wdt.img       pets the SoC's watchdog once a second (one register write). Nobody else does while
                              this image runs: if the phone restarts by itself some 15 to 30 seconds after the
                              counter started, that is probably the watchdog, not a crash (the last number
                              you saw on the counter says how long it ran); this variant keeps it alive.
If the image does not start at all (the phone returns to the bootloader, or restarts into Android by
itself), say what the bootloader screen or `fastboot` printed.
README
echo "android-build: $OUT/README.txt"
