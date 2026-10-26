#!/usr/bin/env bash
# Build the Pixel 2 (Snapdragon 835) boot images (docs/pixel2.md): the whole kernel with feature `msm8998` (its
# own cargo target directory, target/android; user programs, the initramfs and the desktop included), the flat
# arm64 Image, and Android boot images that `fastboot boot` can run. Nothing here talks to the phone or writes to
# any disk: the images and a README with the exact commands land in OUTDIR.
#
#   scripts/android-build.sh OUTDIR [--stock-dtb FILE] [--debug] [--image FILE]
#
# What the real phone showed (docs/pixel2.md): the bootloader accepts a plain uncompressed Image with a device tree
# appended ("Booting OKAY") but does not start it; a GZIP-compressed kernel with the tree appended (the layout of
# `Image.gz-dtb`) is started. So the images are gzip images, and the plain one is only kept as a variant.
#
# The device tree the bootloader needs appended to the kernel comes from one of two places:
#   --stock-dtb FILE   the base tree of the phone's own boot image, which you extracted yourself (never
#                      kept in this repository): `milkos-pixel2.img` uses it unchanged, the one that worked;
#                      `milkos-pixel2-minimal.img` uses the small tree scripts/dtb_build.py makes;
#   (no option)        `milkos-pixel2.img` uses the small tree.
# Variants of the same kernel with another command line (docs/pixel2.md, "Options"), so no rebuild is needed:
#   -plain     the uncompressed Image (accepted but not started by the bootloader: for tests only)
#   -stage1    stops after the log and counts (msm.stage=1): the stage 1 behaviour, no interrupts, no desktop
#   -log30     the boot log stays up for 30 seconds before the desktop (msm.logsecs=30), to photograph it
#   -smp       starts the other seven CPUs through PSCI (msm.smp=1)
#   -turn      the desktop turned a quarter turn, landscape on the portrait glass (msm.rotate=90): turn the phone
#   -desk720   a 720x1280 desktop stretched 1.5 times instead of 540x960 doubled (msm.desk=720x1280): more fits, softer
#   -phystimer ticks on the physical timer instead of the virtual one (msm.timer=phys)
#   -pattern   the colour pattern stays up (msm.hold=inf)
#   -rgbx, -xrgb, -xbgr    the other 32-bit pixel orders (msm.fmt=...)
#   -kick      also kicks the display controller ten times a second (msm.kick=ctl0)
#   -mdp       logs a few display controller registers, read only (msm.mdp=dump)
#   -wdt       pets the SoC's watchdog once a second (msm.wdt=pet), in case the phone resets by itself
# Release profile unless --debug. With --image FILE the kernel is not built: FILE is used as the Image (the script's
# own test does this).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
OUT=""; STOCK=""; PROFILE=release; GIVEN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --stock-dtb) STOCK="${2:?--stock-dtb needs a file}"; shift ;;
    --image) GIVEN="${2:?--image needs a file}"; shift ;;
    --debug) PROFILE=debug ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "android-build: unknown option $1" >&2; exit 2 ;;
    *) [ -z "$OUT" ] || { echo "android-build: one OUTDIR only" >&2; exit 2; }; OUT="$1" ;;
  esac
  shift
done
[ -n "$OUT" ] || { echo "usage: scripts/android-build.sh OUTDIR [--stock-dtb FILE] [--debug] [--image FILE]" >&2; exit 2; }
[ -z "$STOCK" ] || [ -f "$STOCK" ] || { echo "android-build: $STOCK is not a file" >&2; exit 2; }
[ -z "$GIVEN" ] || [ -f "$GIVEN" ] || { echo "android-build: $GIVEN is not a file" >&2; exit 2; }
mkdir -p "$OUT"

if [ -n "$GIVEN" ]; then
  cp "$GIVEN" "$OUT/Image"
  echo "android-build: $OUT/Image ($(wc -c < "$OUT/Image" | tr -d ' ') bytes, given)"
else
  cargo build -q $([ "$PROFILE" = release ] && echo --release) --features msm8998 --target-dir target/android
  ELF="target/android/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
  "$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$OUT/Image"
  echo "android-build: $OUT/Image ($(wc -c < "$OUT/Image" | tr -d ' ') bytes, $PROFILE)"
fi

python3 -I scripts/dtb_build.py minimal -o "$OUT/minimal.dtb"
if [ -n "$STOCK" ]; then
  python3 -I scripts/dtb_build.py stock -i "$STOCK" -o "$OUT/stock.dtb"
  MAIN_DTB="$OUT/stock.dtb"
else
  MAIN_DTB="$OUT/minimal.dtb"
fi
# img NAME DTB COMPRESS(gzip|none) [extra command line words]
img() {
  local name="$1" dtb="$2" comp="$3"; shift 3
  local args=(--kernel "$OUT/Image" --dtb "$dtb" -o "$OUT/$name.img")
  [ "$comp" != gzip ] || args+=(--compress gzip)
  [ $# -eq 0 ] || args+=(--add-cmdline "$*")
  python3 -I scripts/android-bootimg.py "${args[@]}" >/dev/null
  echo "android-build: $OUT/$name.img ($(wc -c < "$OUT/$name.img" | tr -d ' ') bytes, $([ "$comp" = gzip ] && echo gzip || echo plain) kernel)"
}
img milkos-pixel2 "$MAIN_DTB" gzip
img milkos-pixel2-plain "$MAIN_DTB" none
[ -z "$STOCK" ] || img milkos-pixel2-minimal "$OUT/minimal.dtb" gzip
img milkos-pixel2-stage1 "$MAIN_DTB" gzip msm.stage=1
img milkos-pixel2-log30 "$MAIN_DTB" gzip msm.logsecs=30
img milkos-pixel2-smp "$MAIN_DTB" gzip msm.smp=1
img milkos-pixel2-turn "$MAIN_DTB" gzip msm.rotate=90
img milkos-pixel2-desk720 "$MAIN_DTB" gzip msm.desk=720x1280
img milkos-pixel2-phystimer "$MAIN_DTB" gzip msm.timer=phys
img milkos-pixel2-pattern "$MAIN_DTB" gzip msm.hold=inf
img milkos-pixel2-rgbx "$MAIN_DTB" gzip msm.fmt=rgbx
img milkos-pixel2-xrgb "$MAIN_DTB" gzip msm.fmt=xrgb
img milkos-pixel2-xbgr "$MAIN_DTB" gzip msm.fmt=xbgr
img milkos-pixel2-kick "$MAIN_DTB" gzip msm.kick=ctl0 msm.hold=3
img milkos-pixel2-mdp "$MAIN_DTB" gzip msm.mdp=dump msm.hold=3
img milkos-pixel2-wdt "$MAIN_DTB" gzip msm.wdt=pet
python3 -I scripts/android-bootimg.py --unpack "$OUT/milkos-pixel2.img" | sed 's/^/  /'

cat > "$OUT/README.txt" <<'README'
milkOS on the Pixel 2: the whole kernel and the desktop on the phone's screen (display only).
Nothing is flashed: `fastboot boot` loads the image into RAM and runs it; the phone's own system is
untouched, and a restart brings Android back. The details are in docs/pixel2.md.

Needs the bootloader unlocked (it is) and fastboot and adb (platform-tools).

THE ONE RULE ABOUT FASTBOOT: `fastboot boot` works only for the FIRST boot of a bootloader session.
After one `fastboot boot` (successful or not) the next one in the same session fails with
"Failed to load/authenticate boot image: Load Error". So every try starts from a fresh session:
  adb reboot bootloader            (from Android)          or: fastboot reboot bootloader     (from the bootloader)
                                   or: power off, hold power + volume down
and then, at once:
  fastboot boot milkos-pixel2.img

The helper does exactly that, and then tells you what happened:
  scripts/android-try.sh milkos-pixel2.img
It reboots into a fresh bootloader session (through adb or fastboot), waits for fastboot, runs `fastboot boot`,
waits 20 seconds and asks the bootloader again.

HOW TO TELL WHETHER THE IMAGE WAS STARTED (the discriminator). After `Booting OKAY`:
  * `fastboot getvar product` STILL ANSWERS and the phone is still listed by `fastboot devices`: the bootloader
    accepted the image and did NOT start it (the screen is the bootloader's, or goes black). The plain
    (uncompressed) image does this. Nothing of ours ran.
  * `fastboot getvar product` no longer answers and the USB device has vanished: the bootloader jumped. What is
    on the screen is milkOS (or its crash screen). Photograph it.
The images here are GZIP images (the kernel compressed the way Image.gz-dtb is): that is the format the
bootloader started on the real phone. milkos-pixel2-plain.img is the same kernel uncompressed, kept only for tests.

What you should see on the phone, in this order:
  1. the usual bootloader warning screen for an unlocked phone (a few seconds): that is the stock
     bootloader, not milkOS;
  2. four big colour blocks with black borders, labelled RED, GREEN, BLUE, WHITE (top left, top
     right, bottom left, bottom right), a white line round the very edge of the screen, a black
     diagonal from the top left corner to the bottom right and a white one the other way. Held for
     about 5 seconds;
  3. the boot log as dark text with a green band along the top edge: the kernel, the device tree,
     memory (all of it, above 4 GiB too), the CPU, the interrupt controller (read only first, then
     brought up: lines starting "[gic]" and "irq:"), the timer, the file system, the scheduler, user
     space. The last line counts "alive N s" and a white marker moves along the band;
  4. after 5 seconds of the log with the counter (msm.logsecs=), the DESKTOP, 540 x 960 of its own pixels shown
     doubled on the 1080 x 1920 glass: a menu bar with a clock that ticks (a frozen phone shows a frozen
     clock), a window "Welcome to milkOS", the dock along the bottom edge. There is no touch and no USB yet:
     nothing on the phone can be operated.
If stage 2 cannot go on (the interrupt controller or the timer does not answer), the log stays with the
reason ("stage 2 STOPPED: ...") and keeps counting: photograph it.
Photograph step 2, the log, and the desktop.

Getting back: hold the power button for about 10 to 15 seconds until the phone restarts; it boots
Android again. (If the phone is still in the bootloader, `fastboot reboot` does the same.)

If something is off, try the variants of the same image (the same kernel, another command line; all gzip):
  milkos-pixel2-minimal.img   the small device tree made by scripts/dtb_build.py instead of the stock one (only
                              built with --stock-dtb; whether the bootloader accepts it was never validated: its
                              first "failure" was a second boot in one bootloader session)
  milkos-pixel2-stage1.img    stops after the log and counts: no interrupts, no desktop (what stage 1 did, with the big kernel
                              behind it). Try it before the default image to tell "the bigger image is started" apart from
                              "stage 2 works"
  milkos-pixel2-log30.img     the boot log stays up 30 seconds before the desktop, to photograph it in peace
  milkos-pixel2-smp.img       also starts the other seven CPUs through PSCI. Untried on the phone: use it after
                              the plain image has shown the desktop
  milkos-pixel2-turn.img      the desktop turned a quarter turn: landscape (960 x 540 of its pixels) on the portrait
                              glass; turn the phone onto its side
  milkos-pixel2-desk720.img   a 720 x 1280 desktop stretched 1.5 times instead of 540 x 960 doubled: more of every window
                              fits, the text is a little softer
  milkos-pixel2-phystimer.img ticks on the physical timer instead of the virtual one (the image tries the virtual
                              one first and says in the log which one it used and why)
  milkos-pixel2-pattern.img   the colour pattern stays up (no text), to look at it in peace
  milkos-pixel2-rgbx.img      pixel order R,G,B,x instead of B,G,R,x  (use if red and blue are swapped)
  milkos-pixel2-xrgb.img      x,R,G,B
  milkos-pixel2-xbgr.img      x,B,G,R
  milkos-pixel2-kick.img      also pokes the display controller ten times a second (use if the screen
                              stays on the bootloader's picture while the image runs: a panel that
                              needs a refresh command; read docs/pixel2.md first, it writes a register)
  milkos-pixel2-mdp.img       reads (never writes) some display controller registers into the log
  milkos-pixel2-wdt.img       pets the SoC's watchdog once a second (one register write). The watchdog was found
                              switched off on the real phone, so this should not be needed
  milkos-pixel2-plain.img     the uncompressed kernel: the bootloader accepts it and does not start it
If the image does not start at all (the phone returns to the bootloader, or restarts into Android by
itself), say what the bootloader screen or `fastboot` printed.
README
echo "android-build: $OUT/README.txt"
