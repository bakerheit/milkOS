#!/usr/bin/env bash
# The Pixel 2 kernel (feature msm8998, docs/pixel2.md) run on qemu `virt` with a GICv3 and ramfb as the
# screen: the same stage 1 code, linked for virt (feature msm-virt-test). What it can show and what not:
# the phone's SoC registers do not exist here, so nothing proves the display controller, the real
# framebuffer's pixel order or the bootloader's loading rules. What it does prove is everything that
# does not depend on them:
#   pattern     boot with `msm.hold=inf`: the colour test pattern in the screendump (blocks, labels, frame, the diagonals),
#               and again with `msm.fmt=rgbx`: red and blue swapped, as qemu reads the buffer B,G,R
#   console     the boot log on the screen after the pattern, the green band, the counter on the last line moving
#   crashes     `msm.crashtest=abort` (a data abort), `panic` and `early` (a panic before the console): the crash
#               screens, with the text in the screendump
#   relocation  the same image entered 1 MiB above and 448 KiB below the address it is linked for, and 128 KiB above
#               and below it so that the old and the new place overlap (the Image header says another text_offset):
#               boot.S moves it, the tree with it, and the boot goes on
#   splash      a tree like the phone's (`-dtb`): reserved regions with `no-map` (sentinel words written into them are
#               untouched, the RAM banks go round them), the framebuffer from `splash_region` drawn on before memory is
#               set up (qemu's memory is read back with the monitor: pixel words at known places), also as rgb565 with a
#               longer stride
#   degrade     no framebuffer at all, bad options, a kick or a register dump with no display controller in the tree, no
#               watchdog node: logged, the boot goes on; the watchdog read and pet against a node the tree puts at an
#               address with no device behind it
# Offline, private work folder, the qemu started here killed by PID.
#
#   scripts/pixel2-qemu-test.sh [workdir]      (MILK_KEEP=1 keeps the work folder; it builds its own three kernels, in target/android-*)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="${1:-$(mktemp -d /tmp/milk-pixel2.XXXXXX)}"
OWN_WORK=1; [ -n "${1:-}" ] && OWN_WORK=0
mkdir -p "$T"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
OC="$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)"
build() { # name target-dir [env...]  features come from $FEATURES
  local name="$1" dir="$2"; shift 2
  env "$@" cargo build -q --features "$FEATURES" --target-dir "$dir"
  "$OC" -O binary "$dir/aarch64-unknown-none-softfloat/debug/milkos-kernel" "$T/$name.bin"
}
FEATURES=msm-virt-test build virt target/android-virt
FEATURES=msm-reloc-test build up target/android-reloc-up MILK_TEXT_OFFSET=0x180000
FEATURES=msm-reloc-test build down target/android-reloc-down MILK_TEXT_OFFSET=0x10000
FEATURES=msm-reloc-test build upnear target/android-reloc-upnear MILK_TEXT_OFFSET=0xa0000
FEATURES=msm-reloc-test build downnear target/android-reloc-downnear MILK_TEXT_OFFSET=0x60000
python3 -I scripts/dtb_build.py virt-test -o "$T/phone.dtb" >/dev/null
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

# boot NAME KERNEL "cmdline" [extra qemu args...]: start qemu, leave it running (stop kills it)
boot() {
  local name="$1" kernel="$2" append="$3"; shift 3
  LOG="$T/$name.log"; SOCK="$T/$name.sock"; : > "$LOG"
  qemu-system-aarch64 -M virt,gic-version=3 -cpu cortex-a72 -m 1G -kernel "$T/$kernel.bin" -append "$append" \
    -display none -monitor none -serial "file:$LOG" -qmp "unix:$SOCK,server,nowait" "$@" >"$T/$name.qemu" 2>&1 &
  QPID=$!
}
stop() { kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""; }
wait_for() { # regex seconds -> 0 when the log has it
  local i
  for i in $(seq $(( $2 * 5 ))); do grep -Eaq -- "$1" "$LOG" && return 0; kill -0 "$QPID" 2>/dev/null || return 1; sleep 0.2; done
  return 1
}
has() { tr -d '\r' < "$LOG" | grep -Eaq -- "$1"; }
check() { # regex what
  if has "$1"; then ok "$2: $(tr -d '\r' < "$LOG" | grep -Ea -m1 -- "$1" | cut -c1-100)"; else bad "$2 (no line matching: $1; log $LOG)"; fi
}
nopanic() { if has 'KERNEL PANIC|KERNEL EXCEPTION'; then bad "$1: a crash in the log ($LOG)"; else ok "$1: no crash"; fi; }
shot() { python3 scripts/qmp.py "$SOCK" screendump "$T/$1.ppm" >/dev/null; }
mem() { # address count -> the words, one line
  python3 scripts/qmp.py "$SOCK" hmp "xp /${2}wx $1" | grep -Eao '0x[0-9a-f]{8}' | tr '\n' ' '
}
verify() { if python3 -I scripts/pixel2_check.py "$@" >"$T/check.out" 2>&1; then sed 's/^  ok:/  ok:/' "$T/check.out" | head -3; ok "screen check passed: $*" ; else cat "$T/check.out"; bad "screen check: $*"; fi; }

# ---- the pattern ----
boot pattern virt "msm.hold=inf" -device ramfb
wait_for '^pattern: 1080x1920 bgrx stride 4320' 60 && ok "pattern drawn (qemu ramfb, 1080x1920 bgrx)" || bad "no pattern line ($LOG)"
sleep 0.5; shot pattern; stop
verify pattern "$T/pattern.ppm" bgrx
nopanic pattern

boot rgbx virt "msm.hold=inf msm.fmt=rgbx" -device ramfb
wait_for '^pattern: 1080x1920 rgbx' 60 && ok "pattern drawn as rgbx" || bad "no rgbx pattern line ($LOG)"
sleep 0.5; shot rgbx; stop
verify pattern "$T/rgbx.ppm" rgbx

# ---- the console and the counter ----
boot console virt "msm.hold=1" -device ramfb
if wait_for '^idle: ' 60; then
  sleep 1.5; shot c1; sleep 2.2; shot c2
  ok "boot reached the idle loop"
else bad "never reached the idle loop ($LOG)"; fi
stop
verify console "$T/c1.ppm" "$T/c2.ppm"
nopanic console
check '^milkOS .*: Pixel 2 build on qemu virt' "banner"
check '^cpu: EL1 at entry, MIDR 0x[0-9a-f]+, MPIDR' "cpu line"
check '^image: .*entered right there' "image placement"
check '^dtb: model linux,dummy-virt' "tree read"
check '^cmdline: msm.hold=1' "command line"
check '^timer: CNTFRQ [0-9]+ Hz, polled' "timer"
check '^screen: 1080x1920 XRGB in RAM \(qemu ramfb\)' "screen"
check '^gic: arch rev 3 \(3 = GICv3\)' "GICv3 identified"
check '^gic:  \[0\] aff 0x00000000 cpu# 0 last <- this cpu' "this cpu's redistributor"
check '^ramoops: no region in the tree' "ramoops"
check '^pattern: ' "pattern line replayed"

# ---- the crash screens ----
boot abort virt "msm.hold=0 msm.crashtest=abort" -device ramfb
wait_for '\*\*\* halted \*\*\*' 60 && ok "exception dump printed" || bad "no exception dump ($LOG)"
sleep 1; shot abort; stop
check 'data abort: translation fault' "abort decoded"
verify text "$T/abort.ppm" 1000 1900 3000

boot cpanic virt "msm.hold=0 msm.crashtest=panic" -device ramfb
wait_for 'KERNEL PANIC' 60 && ok "panic printed" || bad "no panic ($LOG)"
sleep 1; shot cpanic; stop
verify text "$T/cpanic.ppm" 700 1500 1000
check '^panicked at kernel/src/msm/stage1.rs' "panic location in the log"

boot early virt "msm.hold=0 msm.crashtest=early" -device ramfb
wait_for 'KERNEL PANIC' 60 && ok "early panic printed" || bad "no early panic ($LOG)"
sleep 1; shot early; stop
verify red "$T/early.ppm" 0 1920 40
verify text "$T/early.ppm" 0 700 2000

# ---- relocation ----
for r in up:0x40180000 down:0x40010000 upnear:0x400a0000 downnear:0x40060000; do
  name="${r%%:*}"; at="${r##*:}"
  boot "reloc-$name" "$name" "msm.hold=0" -device ramfb
  wait_for '^idle: ' 60 && ok "relocation $name: booted" || bad "relocation $name: never idle ($LOG)"
  sleep 1; shot "reloc-$name"; stop
  check "^image: 0x40080000\\.\\.0x[0-9a-f]+, entered at $at" "entered at $at"
  check '^image: boot.S moved it' "moved"
  nopanic "reloc-$name"
  verify text "$T/reloc-$name.ppm" 0 700 3000
done

# ---- a tree like the phone's ----
SENT=(-device loader,addr=0x41000000,data=0xdeadbeef,data-len=4 -device loader,addr=0x410ffffc,data=0xcafef00d,data-len=4
      -device loader,addr=0x42000000,data=0x0badf00d,data-len=4 -device loader,addr=0x421ffffc,data=0x12345678,data-len=4)
boot splash virt "msm.hold=inf" -dtb "$T/phone.dtb" -device ramfb "${SENT[@]}"
if wait_for '^pattern: 1080x1920 bgrx stride 4320 drawn at 0x50000000\+0x2400000' 60; then ok "pattern drawn into the tree's splash_region"; else bad "no pattern line for the splash region ($LOG)"; fi
sleep 0.5
check '^mem:   bank 0x40000000\.\.0x41000000' "bank 1 stops at the first reserved region"
check '^mem:   bank 0x41100000\.\.0x42000000' "bank 2 goes round it"
check '^mem:   bank 0x42200000\.\.0x80000000' "bank 3 starts after the second"
[ "$(mem 0x41000000 1)" = "0xdeadbeef " ] && [ "$(mem 0x410ffffc 1)" = "0xcafef00d " ] && [ "$(mem 0x42000000 1)" = "0x0badf00d " ] && [ "$(mem 0x421ffffc 1)" = "0x12345678 " ] \
  && ok "the sentinel words in the reserved regions are untouched" || bad "a reserved region was written to"
# bgrx, stride 4320: the frame is white, red block at (100,600), the black diagonal on white at (1012,1800).
[ "$(mem 0x50000000 2)" = "0xffffffff 0xffffffff " ] && ok "frame row written" || bad "frame row: $(mem 0x50000000 2)"
[ "$(mem $(( 0x50000000 + 600*4320 + 100*4 )) 1)" = "0xffff0000 " ] && ok "red block pixel" || bad "red pixel: $(mem $(( 0x50000000 + 600*4320 + 100*4 )) 1)"
[ "$(mem $(( 0x50000000 + 1800*4320 + 1012*4 )) 1)" = "0xff000000 " ] && ok "black diagonal pixel" || bad "diagonal pixel: $(mem $(( 0x50000000 + 1800*4320 + 1012*4 )) 1)"
nopanic splash
stop

boot splash565 virt "msm.hold=inf msm.fmt=rgb565 msm.stride=2400" -dtb "$T/phone.dtb" -device ramfb
wait_for '^pattern: 1080x1920 rgb565 stride 2400 drawn at 0x50000000' 60 && ok "rgb565 pattern drawn" || bad "no rgb565 pattern line ($LOG)"
sleep 0.5
[ "$(mem $(( 0x50000000 + 600*2400 + 200 )) 1)" = "0xf800f800 " ] && ok "red block as rgb565 (stride 2400)" || bad "rgb565 red: $(mem $(( 0x50000000 + 600*2400 + 200 )) 1)"
[ "$(mem $(( 0x50000000 + 1500*2400 + 200 )) 1)" = "0x001f001f " ] && ok "blue block as rgb565" || bad "rgb565 blue: $(mem $(( 0x50000000 + 1500*2400 + 200 )) 1)"
[ "$(mem $(( 0x50000000 + 300*2400 + 2000 )) 1)" = "0x07e007e0 " ] && ok "green block as rgb565" || bad "rgb565 green: $(mem $(( 0x50000000 + 300*2400 + 2000 )) 1)"
stop

# ---- degrade safely ----
boot nofb virt "msm.hold=1 msm.kick=ctl0 msm.mdp=dump msm.fmt=zzz msm.size=big msm.psci=maybe"
wait_for '^idle: ' 60 && ok "no framebuffer: the boot still reaches idle" || bad "no framebuffer: never idle ($LOG)"
sleep 1; stop
check '^screen: no framebuffer' "no screen said so"
check '^option: msm.fmt: not one of' "bad fmt reported"
check '^option: msm.size: want WIDTHxHEIGHT' "bad size reported"
check '^option: msm.psci: want 1 or 0' "bad psci reported"
check '^kick: no usable ctl0 in the tree' "kick without a display controller"
check '^mdp: no qcom,mdss_mdp node with a reg in the tree' "mdp dump without a display controller"
check '^wdt: no qcom,msm-watchdog in the tree' "no watchdog node, said so"
nopanic nofb

# A tree with no splash region at all: ramfb stands in (the phone build would use Google's address).
python3 -I scripts/dtb_build.py virt-test --no-splash -o "$T/nosplash.dtb" >/dev/null
boot nosplash virt "msm.hold=1 msm.wdt=pet msm.mdp=dump" -dtb "$T/nosplash.dtb" -device ramfb
wait_for '^idle: ' 60 && ok "a tree without splash_region: boot reaches idle" || bad "no-splash tree: never idle ($LOG)"
sleep 1; shot nosplash; stop
check '^screen: 1080x1920 XRGB in RAM' "falls back to ramfb"
check '^ram: tree lists 1 range\(s\); 2 reserved regions \(3 MiB\) taken out' "reserved regions counted"
check '^wdt: 0xa003e00: EN 0x[0-9a-f]+ STS 0x[0-9a-f]+' "watchdog registers read from the node the tree names"
check '^wdt: petting it once a second' "watchdog pet announced"
check '^mdp: no qcom,mdss_mdp node' "no display controller node"
nopanic nosplash
verify text "$T/nosplash.ppm" 0 700 3000

if [ "$FAIL" = 0 ]; then echo "pixel2-qemu-test: PASS"; [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ] && rm -rf "$T" || true; else echo "pixel2-qemu-test: FAIL (work folder $T)"; exit 1; fi
