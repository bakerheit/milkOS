#!/usr/bin/env bash
# The Pixel 2 kernel (feature msm8998, docs/pixel2.md) run on qemu `virt` with a GICv3 and ramfb as the
# screen: the same code, linked for virt (feature msm-virt-test): stage 1 (`msm.stage=1`: the pattern, the console, the
# crash screens, the relocation, the trees), then stage 2 (the whole kernel and the desktop). What it can show and what not:
# the phone's SoC registers do not exist here, so nothing proves the display controller, the real
# framebuffer's pixel order or the bootloader's loading rules. What it does prove is everything that
# does not depend on them:
#   pattern     boot with `msm.hold=inf`: the colour test pattern in the screendump (blocks, labels, frame, the diagonals),
#               and again with `msm.fmt=rgbx`: red and blue swapped, as qemu reads the buffer B,G,R
#   console     the boot log on the screen after the pattern, the green band, the counter on the last line moving
#   crashes     `msm.crashtest=abort` (a data abort), `panic`, `early` (a panic before the console) and `pre` (a panic
#               before memory is set up, text drawn straight into the framebuffer): the crash screens, with the text in the
#               screendump or the framebuffer's memory
#   relocation  the same image entered 1 MiB above and 448 KiB below the address it is linked for, and 128 KiB above
#               and below it so that the old and the new place overlap (the Image header says another text_offset):
#               boot.S moves it, the tree with it, and the boot goes on
#   splash      a tree like the phone's (`-dtb`): reserved regions with `no-map` (sentinel words written into them are
#               untouched, the RAM banks go round them), the framebuffer from `splash_region` drawn on before memory is
#               set up (qemu's memory is read back with the monitor: pixel words at known places), also as rgb565 with a
#               longer stride
#   desktop     the default boot all the way: GICv3 up, the timer tried and chosen, the scheduler, user space, the shell,
#               the desktop on the 1080x1920 "glass" doubled from 540x960 (screendumps: pixel doubling, the clock with
#               seconds changing), on 6 GiB so that RAM above 4 GiB is used; the other desktop sizes and the quarter turn;
#               four CPUs through PSCI (`msm.smp=1`); the physical timer (`msm.timer=phys`); a tree that names the
#               wrong timer interrupt (the kernel finds the right one by trying it); a GICv2 machine, where stage 2
#               stops with its reason on the screen and the counter keeps going; the phone-shaped tree all the way to
#               the desktop with sentinel words in its reserved regions untouched
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
# The five kernels (the whole kernel each, the relocation ones with another text_offset), two at a time (the host is
# shared with other tests). The first cargo that needs the user programs builds them; the other waits for its lock.
( FEATURES=msm-virt-test build virt target/android-virt ) & BP1=$!
( FEATURES=msm-reloc-test build up target/android-reloc-up MILK_TEXT_OFFSET=0x180000 ) & BP2=$!
for bp in "$BP1" "$BP2"; do
  wait "$bp" || { echo "pixel2-qemu-test: a kernel did not build"; exit 1; }
done
( FEATURES=msm-reloc-test build down target/android-reloc-down MILK_TEXT_OFFSET=0x10000 ) & BP3=$!
( FEATURES=msm-reloc-test build upnear target/android-reloc-upnear MILK_TEXT_OFFSET=0xa0000 ) & BP4=$!
for bp in "$BP3" "$BP4"; do
  wait "$bp" || { echo "pixel2-qemu-test: a kernel did not build"; exit 1; }
done
FEATURES=msm-reloc-test build downnear target/android-reloc-downnear MILK_TEXT_OFFSET=0x60000 || { echo "pixel2-qemu-test: a kernel did not build"; exit 1; }
python3 -I scripts/dtb_build.py virt-test -o "$T/phone.dtb" >/dev/null
QPID=""
# Every qemu started here has its PID in $T/NAME.pid; the trap kills those and nothing else.
reap_all() { local f; for f in "$T"/*.pid; do [ -f "$f" ] && kill -9 "$(cat "$f")" 2>/dev/null || true; done; }
trap reap_all EXIT

# boot NAME KERNEL "cmdline" [extra qemu args...]: start qemu, leave it running (stop kills it). BOOT_MEM, BOOT_GIC and
# BOOT_SMP change the machine (default 1G, GICv3, one CPU). Several can run at once: `use NAME` selects one again.
BOOT_MEM=1G; BOOT_GIC=3; BOOT_SMP=1
boot() {
  local name="$1" kernel="$2" append="$3"; shift 3
  LOG="$T/$name.log"; SOCK="$T/$name.sock"; : > "$LOG"
  qemu-system-aarch64 -M virt,gic-version="$BOOT_GIC" -cpu cortex-a72 -smp "$BOOT_SMP" -m "$BOOT_MEM" -kernel "$T/$kernel.bin" -append "$append" \
    -display none -monitor none -serial "file:$LOG" -qmp "unix:$SOCK,server,nowait" "$@" >"$T/$name.qemu" 2>&1 &
  QPID=$!
  echo "$QPID" > "$T/$name.pid"
}
use() { LOG="$T/$1.log"; SOCK="$T/$1.sock"; QPID="$(cat "$T/$1.pid")"; }
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
# settle NAME CHECK ARGS...: take a screendump of the running machine and run the check on it (@PPM@ stands for the file),
# again and again for up to 10 s until it passes: a loaded host may not have drawn the screen yet.
settle() {
  local name="$1" i; shift
  for i in $(seq 10); do
    shot "$name"
    if python3 -I scripts/pixel2_check.py "${@//@PPM@/$T/$name.ppm}" >"$T/check.out" 2>&1; then ok "screen check passed: $*"; return 0; fi
    sleep 1
  done
  cat "$T/check.out"; bad "screen check: $*"
}
verify() { if python3 -I scripts/pixel2_check.py "$@" >"$T/check.out" 2>&1; then sed 's/^  ok:/  ok:/' "$T/check.out" | head -3; ok "screen check passed: $*" ; else cat "$T/check.out"; bad "screen check: $*"; fi; }

# ---- stage 2: the whole kernel and the desktop. Started first, all at once (each takes a while), checked last. ----
python3 -I scripts/dtb_build.py virt-test --timer 13,14,3,10 -o "$T/badtimer.dtb" >/dev/null
SENT=(-device loader,addr=0x4c000000,data=0xdeadbeef,data-len=4 -device loader,addr=0x4c0ffffc,data=0xcafef00d,data-len=4
      -device loader,addr=0x4d000000,data=0x0badf00d,data-len=4 -device loader,addr=0x4d1ffffc,data=0x12345678,data-len=4)
S2="msm.hold=0 msm.logsecs=2"
# qemu's own tree, 6 GiB (RAM above 4 GiB is used), the desktop doubled from 540x960.
BOOT_MEM=6G boot desk virt "$S2" -device ramfb
BOOT_MEM=2G boot turn virt "$S2 msm.rotate=90" -device ramfb
BOOT_MEM=2G boot big virt "$S2 msm.desk=720x1280" -device ramfb
BOOT_MEM=2G BOOT_SMP=4 boot smp virt "$S2 msm.smp=1" -device ramfb
# (Four machines at once, then four more: the host is shared with other tests.)

# ---- the pattern ----
boot pattern virt "msm.stage=1 msm.hold=inf" -device ramfb
wait_for '^pattern: 1080x1920 bgrx stride 4320' 60 && ok "pattern drawn (qemu ramfb, 1080x1920 bgrx)" || bad "no pattern line ($LOG)"
sleep 0.5; shot pattern; stop
verify pattern "$T/pattern.ppm" bgrx
nopanic pattern

boot rgbx virt "msm.stage=1 msm.hold=inf msm.fmt=rgbx" -device ramfb
wait_for '^pattern: 1080x1920 rgbx' 60 && ok "pattern drawn as rgbx" || bad "no rgbx pattern line ($LOG)"
sleep 0.5; shot rgbx; stop
verify pattern "$T/rgbx.ppm" rgbx

# ---- the console and the counter ----
boot console virt "msm.stage=1 msm.hold=1" -device ramfb
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
check '^cmdline: msm.stage=1 msm.hold=1' "command line"
check '^timer: CNTFRQ [0-9]+ Hz, polled' "timer"
check '^screen: 1080x1920 XRGB in RAM \(qemu ramfb\)' "screen"
check '^gic: arch rev 3 \(3 = GICv3\)' "GICv3 identified"
check '^gic:  \[0\] aff 0x00000000 cpu# 0 last <- this cpu' "this cpu's redistributor"
check '^ramoops: no region in the tree' "ramoops"
check '^pattern: ' "pattern line replayed"

# ---- the crash screens ----
boot abort virt "msm.stage=1 msm.hold=0 msm.crashtest=abort" -device ramfb
wait_for '\*\*\* halted \*\*\*' 60 && ok "exception dump printed" || bad "no exception dump ($LOG)"
settle abort text @PPM@ 1000 1900 3000; stop
check 'data abort: translation fault' "abort decoded"

boot cpanic virt "msm.stage=1 msm.hold=0 msm.crashtest=panic" -device ramfb
wait_for 'KERNEL PANIC' 60 && ok "panic printed" || bad "no panic ($LOG)"
settle cpanic text @PPM@ 700 1500 1000; stop
check '^panicked at kernel/src/msm/stage1.rs' "panic location in the log"

boot early virt "msm.stage=1 msm.hold=0 msm.crashtest=early" -device ramfb
wait_for 'KERNEL PANIC' 60 && ok "early panic printed" || bad "no early panic ($LOG)"
settle early red @PPM@ 0 1920 40
settle early text @PPM@ 0 700 2000; stop

# A crash before memory is set up: the log is drawn glyph by glyph into the framebuffer the tree names.
boot pre virt "msm.stage=1 msm.hold=0 msm.crashtest=pre" -dtb "$T/phone.dtb" -device ramfb
wait_for 'KERNEL PANIC' 60 && ok "pre-memory panic printed" || bad "no pre-memory panic ($LOG)"
# The panic line reaches the serial log before the glyphs are drawn: look until they are there (up to 30 s).
n=0
for _ in $(seq 60); do
  words="$(python3 scripts/qmp.py "$SOCK" hmp "xp /1080wx $(( 0x50000000 + 20*4320 ))" | grep -Eao '0x[0-9a-f]{8}')"
  n=$(echo "$words" | grep -c '0xffece6d8' || true)
  [ "$n" -gt 20 ] && break
  sleep 0.5
done
[ "$n" -gt 20 ] && ok "text pixels drawn straight into the framebuffer ($n light words in one row)" || bad "no text in the framebuffer row ($n)"
echo "$words" | grep -q '0xff161513' && ok "background cleared" || bad "background not drawn"
has 'mem: RAM' && bad "memory was already set up" || ok "crashed before mem::init"
stop

# ---- relocation ----
for r in up:0x40180000 down:0x40010000 upnear:0x400a0000 downnear:0x40060000; do
  name="${r%%:*}"; at="${r##*:}"
  boot "reloc-$name" "$name" "msm.stage=1 msm.hold=0" -device ramfb
  wait_for '^idle: ' 60 && ok "relocation $name: booted" || bad "relocation $name: never idle ($LOG)"
  settle "reloc-$name" text @PPM@ 0 700 3000; stop
  check "^image: 0x40080000\\.\\.0x[0-9a-f]+, entered at $at" "entered at $at"
  check '^image: boot.S moved it' "moved"
  nopanic "reloc-$name"
done

# ---- a tree like the phone's ----
boot splash virt "msm.stage=1 msm.hold=inf" -dtb "$T/phone.dtb" -device ramfb "${SENT[@]}"
if wait_for '^pattern: 1080x1920 bgrx stride 4320 drawn at 0x50000000\+0x2400000' 60; then ok "pattern drawn into the tree's splash_region"; else bad "no pattern line for the splash region ($LOG)"; fi
sleep 0.5
check '^mem:   bank 0x40000000\.\.0x4c000000' "bank 1 stops at the first reserved region"
check '^mem:   bank 0x4c100000\.\.0x4d000000' "bank 2 goes round it"
check '^mem:   bank 0x4d200000\.\.0x80000000' "bank 3 starts after the second"
[ "$(mem 0x4c000000 1)" = "0xdeadbeef " ] && [ "$(mem 0x4c0ffffc 1)" = "0xcafef00d " ] && [ "$(mem 0x4d000000 1)" = "0x0badf00d " ] && [ "$(mem 0x4d1ffffc 1)" = "0x12345678 " ] \
  && ok "the sentinel words in the reserved regions are untouched" || bad "a reserved region was written to"
# bgrx, stride 4320: the frame is white, red block at (100,600), the black diagonal on white at (1012,1800).
[ "$(mem 0x50000000 2)" = "0xffffffff 0xffffffff " ] && ok "frame row written" || bad "frame row: $(mem 0x50000000 2)"
[ "$(mem $(( 0x50000000 + 600*4320 + 100*4 )) 1)" = "0xffff0000 " ] && ok "red block pixel" || bad "red pixel: $(mem $(( 0x50000000 + 600*4320 + 100*4 )) 1)"
[ "$(mem $(( 0x50000000 + 1800*4320 + 1012*4 )) 1)" = "0xff000000 " ] && ok "black diagonal pixel" || bad "diagonal pixel: $(mem $(( 0x50000000 + 1800*4320 + 1012*4 )) 1)"
nopanic splash
stop

boot splash565 virt "msm.stage=1 msm.hold=inf msm.fmt=rgb565 msm.stride=2400" -dtb "$T/phone.dtb" -device ramfb
wait_for '^pattern: 1080x1920 rgb565 stride 2400 drawn at 0x50000000' 60 && ok "rgb565 pattern drawn" || bad "no rgb565 pattern line ($LOG)"
sleep 0.5
[ "$(mem $(( 0x50000000 + 600*2400 + 200 )) 1)" = "0xf800f800 " ] && ok "red block as rgb565 (stride 2400)" || bad "rgb565 red: $(mem $(( 0x50000000 + 600*2400 + 200 )) 1)"
[ "$(mem $(( 0x50000000 + 1500*2400 + 200 )) 1)" = "0x001f001f " ] && ok "blue block as rgb565" || bad "rgb565 blue: $(mem $(( 0x50000000 + 1500*2400 + 200 )) 1)"
[ "$(mem $(( 0x50000000 + 300*2400 + 2000 )) 1)" = "0x07e007e0 " ] && ok "green block as rgb565" || bad "rgb565 green: $(mem $(( 0x50000000 + 300*2400 + 2000 )) 1)"
stop

# ---- degrade safely ----
boot nofb virt "msm.stage=1 msm.hold=1 msm.kick=ctl0 msm.mdp=dump msm.fmt=zzz msm.size=big msm.psci=maybe"
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
boot nosplash virt "msm.stage=1 msm.hold=1 msm.wdt=pet msm.mdp=dump" -dtb "$T/nosplash.dtb" -device ramfb
wait_for '^idle: ' 60 && ok "a tree without splash_region: boot reaches idle" || bad "no-splash tree: never idle ($LOG)"
settle nosplash text @PPM@ 0 700 3000; stop
check '^screen: 1080x1920 XRGB in RAM' "falls back to ramfb"
check '^ram: tree lists 1 range\(s\); 2 reserved regions \(3 MiB\) taken out' "reserved regions counted"
check '^wdt: 0xa003e00: EN 0x[0-9a-f]+ STS 0x[0-9a-f]+' "watchdog registers read from the node the tree names"
check '^wdt: petting it once a second' "watchdog pet announced"
check '^mdp: no qcom,mdss_mdp node' "no display controller node"
nopanic nosplash

# ---- stage 2: the results ----
# What every boot to the desktop has, whatever the machine around it.
desktop_up() { # NAME seconds-to-wait
  use "$1"
  if wait_for 'wm: desktop up in' "$2"; then ok "$1: the desktop came up"; else bad "$1: no desktop (log $LOG)"; fi
  check '\[gic\] v3 up: 288 interrupt IDs' "$1: GICv3 up"
  check '\[sched\] up' "$1: scheduler"
  check '\[proc\] up' "$1: user space"
  check 'desktop: starting at' "$1: the desktop thread started"
  nopanic "$1"
}
# The four words at 0x4c000000 and 0x4d000000 written before the kernel started are still there: the whole boot, the desktop
# included, never touched what the tree reserves.
sentinels() {
  [ "$(mem 0x4c000000 1)" = "0xdeadbeef " ] && [ "$(mem 0x4c0ffffc 1)" = "0xcafef00d " ] && [ "$(mem 0x4d000000 1)" = "0x0badf00d " ] && [ "$(mem 0x4d1ffffc 1)" = "0x12345678 " ] \
    && ok "$1: the sentinel words in the reserved regions are untouched by the whole boot" || bad "$1: a reserved region was written to"
}

desktop_up desk 180
sleep 3; shot desk1; sleep 2.3; shot desk2
check '^mem: RAM 0x40000000\.\.0x1c0000000 \(6144 MiB' "desk: all of 6 GiB, above 4 GiB too"
check '^selftest: high memory ok \(RAM to 0x1c0000000' "desk: frames above 4 GiB written and read back"
check '^irq: timer interrupts from the tree: secure 29 physical 30 virtual 27 hypervisor 26' "desk: the timer interrupts come from the tree"
check '^irq: virtual timer, INTID 27: condition met, private interrupts pending in the GIC 0x08000000' "desk: the virtual timer was tried and the GIC saw INTID 27"
check '^irq: scheduler tick on the virtual timer, INTID 27' "desk: the tick is the virtual timer"
check '^\[timer\] CNTFRQ [0-9]+ Hz, tick 100 Hz \(INTID 27\)' "desk: 100 Hz tick"
check '^\[smp\] one CPU' "desk: one CPU by default"
check '^present: desktop 540x960 -> turn 0 clockwise -> 540x960 -> stretch x2\.000 y2\.000 -> glass 1080x1920 bgrx stride 4320' "desk: the desktop is doubled onto the glass"
check '^stage 2: the log stays up for 2 s' "desk: the boot log is held"
verify desktop "$T/desk1.ppm" "$T/desk2.ppm"
stop

desktop_up turn 180
sleep 3; shot turn1
check '^present: desktop 960x540 -> turn 90 clockwise -> 540x960 -> stretch x2\.000 y2\.000' "turn: a landscape desktop turned onto the glass"
verify turned "$T/turn1.ppm"
stop

desktop_up big 180
sleep 3; shot big1; sleep 2.3; shot big2
check '^present: desktop 720x1280 -> turn 0 clockwise -> 720x1280 -> stretch x1\.500 y1\.500' "big: 720x1280 stretched 1.5 times"
verify desktop "$T/big1.ppm" "$T/big2.ppm" free
stop

desktop_up smp 240
check '^\[smp\] 4 of 4 CPUs online \(PSCI' "smp: four CPUs through PSCI"
check '^\[smp\] cpu 3 up' "smp: cpu 3 came up"
stop

# The second four.
BOOT_MEM=2G boot phys virt "$S2 msm.timer=phys" -device ramfb
# A tree whose virtual timer interrupt is wrong (PPI 3): the kernel finds the right one by trying the timer. With the
# sentinel words in the reserved regions: the whole boot, the desktop included, leaves them alone.
BOOT_MEM=2G boot badtimer virt "$S2" -dtb "$T/badtimer.dtb" -device ramfb "${SENT[@]}"
# The phone-shaped tree (the framebuffer is its splash region, not ramfb): to the desktop, sentinels untouched.
BOOT_MEM=2G boot phone virt "$S2" -dtb "$T/phone.dtb" -device ramfb "${SENT[@]}"
# A GICv2 machine: stage 2 cannot go on, says why on the screen and keeps counting.
BOOT_MEM=1G BOOT_GIC=2 boot gicv2 virt "msm.hold=0" -device ramfb

desktop_up phys 180
check '^irq: physical timer, INTID 30: condition met' "phys: the physical timer was tried"
check '^irq: scheduler tick on the physical timer, INTID 30' "phys: the tick is the physical timer"
stop

desktop_up badtimer 180
check '^irq: timer interrupts from the tree: secure 29 physical 30 virtual 19 hypervisor 26' "badtimer: the tree's wrong virtual interrupt"
check '^irq: virtual timer, INTID 19: condition met, private interrupts pending in the GIC 0x08000000' "badtimer: it fired, and the GIC showed INTID 27"
check '^irq: scheduler tick on the virtual timer, INTID 27' "badtimer: the kernel used what the GIC showed"
sentinels badtimer
stop

desktop_up phone 180
sleep 2
check '^mem:   bank 0x40000000\.\.0x4c000000' "phone: bank 1 stops at the first reserved region"
check '^present: desktop 540x960 -> turn 0 clockwise -> 540x960 -> stretch x2\.000 y2\.000' "phone: the desktop through the splash region"
sentinels phone
# The glass is the tree's splash region (bgrx, stride 4320): the menu bar's cream at the top left, a wallpaper word further down.
top="$(mem $(( 0x50000000 + 10*4320 + 10*4 )) 1)"; wall="$(mem $(( 0x50000000 + 1500*4320 + 100*4 )) 1)"
if python3 -I -c "import sys; w=int(sys.argv[1],16); sys.exit(0 if min((w>>16)&255,(w>>8)&255,w&255) > 0xc0 else 1)" "$top" \
   && [ "$wall" != "0xff161513 " ] && [ "$wall" != "0x00000000 " ]; then ok "phone: the desktop is in the splash region (menu bar $top, wallpaper $wall)"; else bad "phone: the splash region does not hold the desktop ($top / $wall)"; fi
stop

use gicv2
if wait_for '^stage 2 STOPPED' 120; then ok "gicv2: stage 2 stopped"; else bad "gicv2: no stop line (log $LOG)"; fi
check '^\[gic\] GICv3 setup failed: the tree has no arm,gic-v3 node' "gicv2: the reason is in the log"
check '^stage 2 STOPPED: the tree has no arm,gic-v3 node' "gicv2: the reason is on the screen"
check '^staying in stage 1: no interrupts, counting' "gicv2: it says what it does"
wait_for '^idle: ' 30 || true
sleep 1.5; shot gicv2a; sleep 2.2; shot gicv2b; stop
verify console "$T/gicv2a.ppm" "$T/gicv2b.ppm"
has 'KERNEL PANIC|KERNEL EXCEPTION' && bad "gicv2: a crash" || ok "gicv2: no crash"

if [ "$FAIL" = 0 ]; then echo "pixel2-qemu-test: PASS"; [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ] && rm -rf "$T" || true; else echo "pixel2-qemu-test: FAIL (work folder $T)"; exit 1; fi
