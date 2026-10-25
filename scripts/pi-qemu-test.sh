#!/usr/bin/env bash
# The Raspberry Pi 4 kernel (docs/raspberry-pi.md) on qemu's `-M raspi4b`, end to end:
#   boot 1: the shell prompt on the serial console (PL011), four CPUs released through the
#           firmware's spin table, the framebuffer from the mailbox and the desktop on it (a
#           qemu screendump checked for real content), the SD card (emulated EMMC) found,
#           a fresh milkfs on it mounted, commands typed over the serial line, a file written;
#   boot 2: the same SD image again: the file is still there (the card's data survives a reboot);
#   boot 3: a device tree that lists two RAM banks with the GPU's hole between them (the shape of a
#           4 GB or 8 GB board's memory node), loaded unchanged (qemu rewrites memory nodes of a tree
#           given with -dtb, so this one goes in as a raw file where the boot stub looks for it):
#           both banks used, the hole reserved, the framebuffer inside the hole, the desktop up.
#   boot 4: `milk.crashtest=early` on the command line: a panic right after the display is up, with no
#           serial cable in mind: the boot log (not cleared) and the panic text are on the screen, with the
#           green sign-of-life band on top (qemu screendump, pixels checked);
#   boot 5: `milk.crashtest=late`: a panic with the desktop up: the end of the log, the panic text, on a
#           full-screen text console (screendump: the desktop is gone, the console background is there);
#   boots 6, 7: `milk.crashtest=late-off` / `late-reset`: the desktop's Shut Down and Restart on a board with no PSCI
#           (the SoC's power manager): qemu exits by itself / the machine boots again.
#   The ACT LED codes (docs/raspberry-pi.md): qemu's raspi4b does model the GPIO block (GPFSEL4 and the level register
#           GPLEV1 read back what the kernel wrote), so the pin is sampled while booting: boot 1 sees it high, then dark
#           (the start marker and the codes), then high again (solid on); boot 3 (`milk.ledcodes=0`) never sees it dark
#           once it is on; boot 5's crash is decoded from the pin (the whole crash cycle, scripts/led_sample.py + led-decode.py) and compared with the logged code. All boots check the `led: stage N name` lines in order.
#           (raspi4b has exactly 2 GiB, so a layout above 4 GiB cannot run here: the allocator and
#           page tables above 4 GiB are tested on qemu virt, scripts/highmem-test.sh.)
#   boots 8 to 12: the 3.5-inch SPI display (`milk.lcd=...`), against qemu's SPI0 controller with nothing wired to it:
#           8: panel mirroring an HDMI desktop; 9: the panel as the only screen (`milk.hdmi=off`, 1:1 pan view,
#           other controller / rotation / pins / polled touch); 10: a crash with the panel on (no stall, HDMI crash
#           screen as before); 11: bad option values are refused and the defaults used; 12: an unknown controller; 14, 15: `milk.lcd=probe` (every candidate set-up shows its number card, colour flashes and text card, then the boot goes on; 15: its own options).
#           What qemu cannot show: whether any pixel reaches a glass (the host tests of rpikit's panel model prove the
#           byte stream; the real board is the test of the wiring).
#   boots 20 to 25: the framebuffer's size (milk.fbsize=800x480 / 1280x768 / auto / refused / 1920x1080: the log lines, a screendump of the right size, the
#           boot console and crash screen at 800x480) and the touch chip with no SPI panel (milk.touch=xpt2046: pins logged, late start, no invented events).
#   boots 16 to 19: the exception dump (docs/raspberry-pi.md, "Reading a crash"): `milk.crashtest=serror` runs a fake SError (qemu cannot raise a real
#           one) through the real crash path at a checkpoint, `milk.crashtest=dabt` is a real data abort right after a checkpoint, and
#           `milk.serror=off` makes the checkpoints no-ops, `milk.serror=paranoid` adds one after every register write; the ESR decode, FAR/ELR/SPSR, the checkpoint attribution and the register write
#           breadcrumbs are checked in the log, the checkpoint cost is bounded, and boot 1 proves the checkpoints are silent when nothing is pending.
#   boot 20: `milk.pcie=probe` where the tree has no PCIe controller (qemu's raspi4b has none): acknowledged, skipped cleanly, the desktop runs; `milk.crashtest=serror-absorb` runs the SError-survival rules against pretend SErrors.
# Offline, private images in a fresh /tmp folder, the qemu started here killed by PID.
# TCG (no hvf for raspi), so a boot takes about 10-30 s.
#
#   scripts/pi-qemu-test.sh [workdir]      (MILK_KEEP=1 keeps the work folder; MILK_PREBUILT=1 skips the build)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="${1:-$(mktemp -d /tmp/milk-pi.XXXXXX)}"
OWN_WORK=1; [ -n "${1:-}" ] && OWN_WORK=0
mkdir -p "$T"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
if [ "${MILK_PREBUILT:-}" = 1 ]; then
  ELF="target/pi/aarch64-unknown-none-softfloat/debug/milkos-kernel"
  [ -e "$ELF" ] || { echo "pi-qemu-test: MILK_PREBUILT=1 but $ELF is missing"; exit 1; }
  "$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$T/kernel8.img"
else
  scripts/pi-build.sh "$T/kernel8.img" >/dev/null
fi
# What qemu does not supply: a device tree (qemu's raspi4b has none of its own).
python3 scripts/pi_dtb.py "$T/pi.dtb" --cmdline "milk.sdwrite=1 milk.genet=probe"
python3 scripts/mkmilkfs.py mkfs "$T/sd.img" --size 64M --label Milk >/dev/null
# boot 3's tree: no CPUs (it must fit under the boot stub's own code at 0x300), two banks.
python3 scripts/pi_dtb.py "$T/banks.dtb" --cpus 0 --ram-bytes 0x3b400000 --bank 0x40000000:0x40000000 --cmdline "milk.ledcodes=0"
P='msh:[^\n]*\$ '
QPID=""
DTB_ARGS=(-dtb "$T/pi.dtb")
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

pin_level() { # QMP socket -> 0 or 1 (nothing if qemu does not answer yet)
  local v
  v=$(python3 scripts/qmp.py "$1" hmp "xp /1wx 0xfe200038" 2>/dev/null | grep -Eao '0x[0-9a-f]+' | head -1) || true
  [ -n "$v" ] && echo $(( (v >> 10) & 1 ))
  return 0
}

# boot N: start qemu on the card image, drive the serial console with the steps after it.
boot() {
  local n="$1"; shift
  SOCK="$T/s$n.sock"; QMP="$T/q$n.sock"; LOG="$T/serial$n.log"
  : > "$LOG"
  # raspi4b: the SD card attaches with if=sd; no -m (the board's RAM), no hvf.
  qemu-system-aarch64 -M raspi4b -kernel "$T/kernel8.img" "${DTB_ARGS[@]}" -display none -monitor none \
    -serial "unix:$SOCK,server=on,wait=on" -qmp "unix:$QMP,server,nowait" \
    -drive "file=$T/sd.img,if=sd,format=raw" >/dev/null 2>&1 &
  QPID=$!
  LS=""
  if [ "${LEDCYCLE:-}" = 1 ]; then
    # Decode the crash cycle from the pin while the boot runs (stops at the first complete cycle, or after 200 s).
    python3 -I scripts/led_sample.py "$QMP" 200 "$T/ledcycle$n" > "$T/ledrec$n" &
    LS=$!
  fi
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/done$n" &
  D=$!
  SP=""
  if [ "${SAMPLE:-}" = 1 ]; then
    # The ACT LED pin (GPIO 42: bit 10 of GPLEV1 at 0xfe200038), over and over until the boot is done.
    : > "$T/pin$n"
    ( while [ ! -e "$T/done$n" ] && kill -0 "$QPID" 2>/dev/null; do
        v=$(pin_level "$QMP"); [ -n "$v" ] && printf '%s' "$v" >> "$T/pin$n"
      done ) &
    SP=$!
  fi
  for _ in $(seq 1500); do [ -e "$T/done$n" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
  # A machine that powers itself off ends the loop above before the script has written its marker.
  for _ in 1 2 3 4 5 6 7 8 9 10; do [ -e "$T/done$n" ] && break; sleep 0.3; done
  [ -e "$T/done$n" ] || bad "boot $n: the serial script did not finish (see $LOG)"
}
stop() {
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  { kill "$D"; wait "$D"; } 2>/dev/null || true
  [ -n "${SP:-}" ] && { wait "$SP"; } 2>/dev/null || true
}
led_lines() { # the "led: stage N name" lines of the log, one line: 1:memory 2:mailbox ...
  tr -d '\r' < "$LOG" | sed -n -E 's/^led: stage ([0-9]+) ([a-z]+)$/\1:\2/p; s/^led: done, solid on$/done/p' | tr '\n' ' ' | sed 's/ $//'
}
LED_ORDER='1:memory 2:mailbox 3:framebuffer 4:interrupts 5:sdcard 6:scheduler 7:userspace done'
out() { # output of the command typed as $1 (the last time), up to the next prompt
  tr -d '\r' < "$LOG" | awk -v c="$1" '/^msh:/ { if (on) {on=0}; if (substr($0, index($0, "$ ") + 2) == c) {buf=""; on=1; next} } $0 == c && !on {buf=""; on=1; next} on {buf = buf $0 "\n"} END {printf "%s", buf}'
}
has() { tr -d '\r' < "$LOG" | grep -Eaq -- "$1"; }
check() { # pattern what
  if has "$1"; then ok "$2: $(tr -d '\r' < "$LOG" | grep -Ea -m1 -- "$1" | cut -c1-110)"; else bad "$2 (no line matching: $1)"; fi
}

# ---- boot 1 ----
SAMPLE=1 boot 1 "wait@300:wm: desktop up" \
  "send:system" "wait:$P" \
  "send:disks" "wait:$P" \
  "send:print hello-from-the-pi > /Users/milk/Documents/pi.txt" "wait:$P" \
  "send:disks snapshot pi-test" "wait:$P" \
  "send:text show /Users/milk/Documents/pi.txt" "wait:$P" \
  "send:processes" "wait:$P"
sleep 1
python3 scripts/qmp.py "$QMP" screendump "$T/desktop.png" >/dev/null 2>&1 || true
stop
check '^platform: Raspberry Pi 4 \(BCM2711\) \(device tree: raspberrypi,4-model-b\)' "the Pi board and its device tree"
check '^mem: RAM 0x0\.\.0x[0-9a-f]+ \([0-9]+ MiB, device tree\)' "RAM from the device tree"
check '^\[gic\] v2 up' "the GIC-400"
check '^\[smp\] 4 of 4 CPUs online \(spin table\)' "four CPUs through the spin table"
check '^rpi fb: 1024x768x32 pitch 4096 at ' "the mailbox framebuffer"
check '^display: rpi mailbox framebuffer 1024x768' "the display on it"
check '^\[sdhci\] .*card ready' "the SD card"
check '^milkfs: mmcblk0 mounted at /Volumes/Milk' "milkfs on the SD card"
check '^milklog: not writing a log file: no partition table' "the log writer leaves a milkfs card alone (no FAT, no log file)"
check 'wm: desktop up' "the desktop"
check '^\[genet\] no Ethernet controller in the device tree' "milk.genet=probe on a board without GENET: the driver skips cleanly"
check '^\[genet\] milk.genet is set but there is nothing to probe: skipped' "milk.genet=probe: said so, nothing probed"
has '^\[genet\] probe step|SError' && bad "milk.genet=probe ran a step or took an SError on qemu" || ok "milk.genet=probe: no step ran, no SError"
[ "$(led_lines)" = "$LED_ORDER" ] && ok "LED codes: the milestones in order ($(led_lines))" || bad "LED codes: wrong stages: $(led_lines)"
PIN=$(cat "$T/pin1")
echo "$PIN" | grep -Eq '10+1' && ok "LED pin: high, dark (the start marker and the codes), high again (solid on)" || bad "LED pin never did high-dark-high: ${PIN:0:80}..."
case "$PIN" in *1) ok "LED pin: solid on at the end";; *) bad "LED pin is not on at the end";; esac
has 'KERNEL PANIC|kernel panic' && bad "a kernel panic" || ok "no panic"
check '^serror: checkpoints and write breadcrumbs on' "the SError checkpoints are on by default"
COST=$(tr -d '\r' < "$LOG" | sed -n -E 's/^serror: a checkpoint costs ([0-9]+) ns .*/\1/p' | head -1)
[ -n "$COST" ] && [ "$COST" -lt 20000 ] && ok "a checkpoint costs $COST ns (under qemu's TCG)" || bad "checkpoint cost missing or too high: '$COST'"
has '\*\*\* KERNEL EXCEPTION' && bad "an exception dump in a normal boot" || ok "no exception dump: every checkpoint passed"
out system | grep -Eq '^CPUs +4' && ok "system says 4 CPUs" || bad "system: $(out system)"
out system | grep -Eq '^memory +[0-9]+ MB' && ok "system: $(out system | grep '^memory')" || bad "system has no memory line"
out disks | grep -q 'milkfs.*/Volumes/Milk' && ok "disks lists the card's milkfs" || bad "disks: $(out disks)"
out "text show /Users/milk/Documents/pi.txt" | grep -q 'hello-from-the-pi' && ok "a file written on the card reads back" || bad "no file: $(out 'text show /Users/milk/Documents/pi.txt')"
# The screen: 1024x768, with a desktop's worth of colours in it (not black, not one flat colour).
desktop_shot() { # PNG [W H] -> 0 if it shows a desktop (of 1024x768, or W x H)
  [ -s "$1" ] && python3 -I scripts/png_check.py "$1" >/dev/null 2>&1 && python3 -I - "$1" "${2:-1024}" "${3:-768}" <<'PY'
import struct, sys, zlib
d = open(sys.argv[1], "rb").read()
w, h = struct.unpack(">II", d[16:24])
assert (w, h) == (int(sys.argv[2]), int(sys.argv[3])), (w, h)
i, idat, bpp = 8, b"", 0
while i < len(d):
    n, = struct.unpack(">I", d[i:i+4]); t = d[i+4:i+8]
    if t == b"IHDR": bpp = {2: 3, 6: 4}[d[i+8+9]]
    if t == b"IDAT": idat += d[i+8:i+8+n]
    i += 12 + n
raw = zlib.decompress(idat)
# Rows are filtered; count distinct bytes of the first 20 rows' raw data as a cheap variety check.
colours = len(set(raw[:(1 + w * bpp) * 400]))
assert colours > 60, colours
PY
}
if [ -s "$T/desktop.png" ]; then
  desktop_shot "$T/desktop.png" && ok "the screen shows a desktop" || bad "the screen is blank or flat"
else
  bad "no screendump of the desktop"
fi

# ---- boot 2: the same card ----
boot 2 "wait@300:$P" "send:text show /Users/milk/Documents/pi.txt" "wait:$P"
stop
# (The echo of the typed command can be interleaved with kernel log lines; the file's text is
# on a line of its own.)
tr -d '\r' < "$LOG" | grep -q '^hello-from-the-pi$' && ok "boot 2: the file is still on the card" || bad "boot 2: the file is gone (see $LOG)"
check '^milkfs: mmcblk0 mounted' "boot 2: the card mounts again"

# ---- boot 3: two RAM banks and a hole ----
DTB_ARGS=(-device "loader,file=$T/banks.dtb,addr=0x100,force-raw=on")
SAMPLE=1 boot 3 "wait@300:$P" "seen@300:wm: desktop up" "send:system" "wait:$P"
sleep 1
python3 scripts/qmp.py "$QMP" screendump "$T/banks.png" >/dev/null 2>&1 || true
stop
check '^mem: RAM 0x0\.\.0x80000000 \(1972 MiB, device tree\)' "boot 3: both banks (948 + 1024 MiB)"
check '^mem:   bank 0x0\.\.0x3b400000 ' "boot 3: the first bank"
check '^mem:   bank 0x40000000\.\.0x80000000 ' "boot 3: the second bank"
check '^rpi fb: 1024x768x32 pitch 4096 at 0x3c' "boot 3: the framebuffer in the hole between them"
check 'wm: desktop up' "boot 3: the desktop"
[ "$(led_lines)" = "$LED_ORDER" ] && ok "boot 3: LED stages still logged with milk.ledcodes=0" || bad "boot 3: wrong LED stages: $(led_lines)"
PIN=$(cat "$T/pin3")
echo "$PIN" | grep -Eq '^0*1+$' && ok "boot 3: milk.ledcodes=0: the LED goes on and stays on (no codes)" || bad "boot 3: the LED went dark with ledcodes off: ${PIN:0:80}..."
has 'KERNEL PANIC|kernel panic' && bad "boot 3: a kernel panic" || ok "boot 3: no panic"
out system | grep -Eq '^memory +(1\.9|2\.0) GB' && ok "boot 3: system: $(out system | grep '^memory')" || bad "boot 3: system: $(out system)"
[ -s "$T/banks.png" ] && python3 -I scripts/png_check.py "$T/banks.png" >/dev/null 2>&1 && ok "boot 3: a screendump" || bad "boot 3: no screendump"

# ---- boots 4 and 5: what a crash looks like on the screen ----
pixels() { # PNG X,Y ... -> "R,G,B" per point
  python3 -I - "$@" <<'PY'
import sys
sys.path.insert(0, "scripts")
import png_check
w, h, rows = png_check.decode(sys.argv[1])
print(" ".join("%d,%d,%d" % tuple(rows[int(p.split(",")[1])][int(p.split(",")[0])]) for p in sys.argv[2:]))
PY
}
for stage in early late; do
  python3 scripts/pi_dtb.py "$T/crash.dtb" --cmdline "milk.crashtest=$stage"
  DTB_ARGS=(-dtb "$T/crash.dtb")
  n=$([ $stage = early ] && echo 4 || echo 5)
  # No prompt will come: wait for the panic itself, then the screen.
  LEDCYCLE=$([ $stage = late ] && echo 1 || echo 0) boot $n "wait@300:KERNEL PANIC"
  if [ $stage = late ]; then
    # The crash cycle (docs/raspberry-pi.md): decoded from the pin by scripts/led_sample.py with the decoder of
    # scripts/led-decode.py, and compared with the plain-hex values the kernel logs.
    if wait "$LS" 2>/dev/null; then
      REC=$(cat "$T/ledrec5")
      CODE=$(tr -d '\r' < "$LOG" | sed -n -E 's/^led: crash code (stage=[0-9]+ ec=0o[0-7]+ pc=0o[0-7]+ far=0o[0-7]+) .*/\1/p' | head -1)
      [ -n "$CODE" ] && [ "$REC" = "$CODE" ] && ok "boot 5: the LED crash cycle on the pin decodes to the logged code ($REC)" || bad "boot 5: LED cycle '$REC' != logged '$CODE'"
      case "$REC" in "stage=8 ec=0o77 "*) ok "boot 5: stage 8 (desktop up), kind 0o77 (panic)";; *) bad "boot 5: wrong stage/kind: $REC";; esac
      LINE=$(python3 -I scripts/led-decode.py $REC | grep -a 'panicked at' | tr -s ' ')
      case "$LINE" in *"panicked at kernel/src/selftest.rs:"*) ok "boot 5: the code names the panic's file and line ($LINE)";; *) bad "boot 5: the code does not name selftest.rs: $LINE";; esac
    else
      bad "boot 5: no complete LED crash cycle seen on the pin"
    fi
  fi
  # (qemu takes one QMP client at a time: the LED sampler above has let go by now.)
  sleep 2
  python3 scripts/qmp.py "$QMP" screendump "$T/crash-$stage.png" >/dev/null 2>&1 || true
  stop
  check "^panicked at .*crash-test: a deliberate panic, $stage|crash-test: a deliberate panic, $stage" "boot $n: the deliberate panic ($stage)"
  if [ -s "$T/crash-$stage.png" ]; then
    # Bottom right: the console's background (0x161513), whatever was on the screen before.
    case "$(pixels "$T/crash-$stage.png" 1000,750 1010,740)" in
      "22,21,19 22,21,19") ok "boot $n: the crash screen is a text console (dark background where the desktop was)" ;;
      *) bad "boot $n: the screen is not the text console: $(pixels "$T/crash-$stage.png" 1000,750 1010,740)" ;;
    esac
  else
    bad "boot $n: no screendump"
  fi
done
# ---- boots 6 and 7: power off and restart through the power manager ----
python3 scripts/pi_dtb.py "$T/power.dtb" --cmdline "milk.crashtest=late-off"
DTB_ARGS=(-dtb "$T/power.dtb")
boot 6 "wait@300:wm: shutting down"
gone=0
for _ in $(seq 150); do kill -0 "$QPID" 2>/dev/null || { gone=1; break; }; sleep 0.2; done
[ "$gone" = 1 ] && ok "boot 6: Shut Down powered the machine off (qemu exited by itself)" || bad "boot 6: the machine did not power off"
stop
python3 scripts/pi_dtb.py "$T/power.dtb" --cmdline "milk.crashtest=late-reset"
boot 7 "wait@300:wm: restarting" "wait@300:milkOS [0-9][^ ]* .*booting"
stop
check 'wm: restarting \(power manager, full reset\)' "boot 7: Restart reset the machine"
[ "$(tr -d '\r' < "$LOG" | grep -ac 'booting\.\.\.')" -ge 2 ] && ok "boot 7: the kernel booted a second time" || bad "boot 7: no second boot"

# ---- boots 8 to 12: the 3.5-inch SPI display (docs/raspberry-pi.md). qemu models the SPI0 controller (the bytes
# go nowhere: no panel, no touch chip is attached), so these boots run the whole driver against nothing: the
# bring-up, the boot log, the desktop view, the touch polling, the crash screen; none of it may stall or panic. ----
python3 scripts/pi_dtb.py "$T/lcd.dtb" --cmdline "milk.sdwrite=1 milk.lcd_budget_ms=10000 milk.lcd=ili9486"
DTB_ARGS=(-dtb "$T/lcd.dtb")
boot 8 "wait@300:wm: desktop up" "send:system" "wait:$P" "send:text show /proc/input" "wait:$P"
sleep 1
python3 scripts/qmp.py "$QMP" screendump "$T/lcd8.png" >/dev/null 2>&1 || true
stop
check '^lcd: ili9486 480x320 rot 90 bgr, SPI0 CE0 [0-9]+ kHz .*DC=GPIO24 RST=GPIO25, touch XPT2046 on CE1, pen IRQ GPIO17' "boot 8: the panel's options and pins"
check '^lcd: panel initialised' "boot 8: the panel is brought up"
check '^lcd: boot log on the panel' "boot 8: the boot log goes to the panel"
check '^rpi fb: 1024x768x32 pitch 4096 at ' "boot 8: HDMI is still the primary screen"
check 'lcd: the desktop \(1024x768\) is on the panel 480x320, scaled down' "boot 8: the panel mirrors the desktop"
check 'lcd: first desktop update on the panel' "boot 8: a desktop update reaches the panel"
has 'KERNEL PANIC|kernel panic|lcd: .*timed out' && bad "boot 8: a panic or SPI time-out" || ok "boot 8: no panic, no SPI time-out"
out system | grep -Eq '^CPUs +4' && ok "boot 8: the system runs (system says 4 CPUs)" || bad "boot 8: system: $(out system)"
# The touch chip is polled all along and nothing is attached: no phantom touch (no pointer move, no button).
out "text show /proc/input" | tr -d ' ' | grep -Eq '^buttons0$' && out "text show /proc/input" | tr -d ' ' | grep -Eq '^moves0$' && ok "boot 8: touch polling invents no pointer events" || bad "boot 8: /proc/input: $(out 'text show /proc/input' | tr '\n' ' ')"
desktop_shot "$T/lcd8.png" && ok "boot 8: the HDMI screen shows the desktop" || bad "boot 8: the HDMI screen is blank or flat"

python3 scripts/pi_dtb.py "$T/lcd.dtb" --cmdline "milk.lcd_budget_ms=10000 milk.lcd=ili9488 milk.lcd_rot=270 milk.hdmi=off milk.lcd_view=pan milk.touch_irq=-1 milk.lcd_dc=22 milk.lcd_rst=27 milk.lcd_hz=8000000"
boot 9 "wait@300:wm: desktop up" "send:system" "wait:$P" "send:text show /proc/input" "wait:$P"
stop
check '^lcd: ili9488 480x320 rot 270 bgr, SPI0 CE0 [0-9]+ kHz .*DC=GPIO22 RST=GPIO27, touch XPT2046 on CE1, polled' "boot 9: other controller, rotation, pins, polled touch"
check '^display: the SPI panel is the screen, the desktop draws 1024x768 in RAM' "boot 9: milk.hdmi=off makes the panel the screen"
has '^rpi fb: ' && bad "boot 9: the HDMI framebuffer was used" || ok "boot 9: no HDMI framebuffer"
check 'lcd: the desktop \(1024x768\) is on the panel 480x320, 1:1' "boot 9: the desktop runs on the panel, 1:1 view"
check 'lcd: first desktop update on the panel' "boot 9: a desktop update reaches the panel"
check 'wm: desktop up' "boot 9: the desktop"
has 'KERNEL PANIC|kernel panic|lcd: .*timed out' && bad "boot 9: a panic or SPI time-out" || ok "boot 9: no panic, no SPI time-out"
out system | grep -Eq '^CPUs +4' && ok "boot 9: the system runs on the panel alone" || bad "boot 9: system: $(out system)"
out "text show /proc/input" | tr -d ' ' | grep -Eq '^buttons0$' && out "text show /proc/input" | tr -d ' ' | grep -Eq '^moves0$' && ok "boot 9: touch polling invents no pointer events" || bad "boot 9: /proc/input: $(out 'text show /proc/input' | tr '\n' ' ')"

python3 scripts/pi_dtb.py "$T/lcd.dtb" --cmdline "milk.lcd_budget_ms=10000 milk.lcd=st7796 milk.lcd_rot=0 milk.crashtest=late"
boot 10 "wait@300:KERNEL PANIC"
sleep 2
python3 scripts/qmp.py "$QMP" screendump "$T/lcd10.png" >/dev/null 2>&1 || true
stop
check '^lcd: st7796 320x480 rot 0 bgr' "boot 10: portrait st7796"
check 'lcd: crash screen sent to the panel' "boot 10: the crash screen was drawn on the panel (no stall)"
if [ -s "$T/lcd10.png" ]; then
  case "$(pixels "$T/lcd10.png" 1000,750 1010,740)" in
    "22,21,19 22,21,19") ok "boot 10: the HDMI crash screen is still the text console" ;;
    *) bad "boot 10: the HDMI screen is not the text console: $(pixels "$T/lcd10.png" 1000,750 1010,740)" ;;
  esac
else
  bad "boot 10: no screendump"
fi

python3 scripts/pi_dtb.py "$T/lcd.dtb" --cmdline "milk.lcd_budget_ms=10000 milk.lcd=ili9486 milk.lcd_dc=99 milk.lcd_rot=45 milk.touch_cal=1,2,3 milk.lcd_hz=0 milk.lcd_madctl=999"
boot 11 "wait@300:wm: desktop up"
stop
check '^lcd: milk.lcd_dc=99 is not a header GPIO' "boot 11: a bad pin is refused, the default used"
check '^lcd: milk.lcd_rot must be 0, 90, 180 or 270' "boot 11: a bad rotation is refused"
check '^lcd: milk.touch_cal needs six numbers' "boot 11: a bad calibration is refused"
check 'DC=GPIO24 ' "boot 11: the default pin was used"
check 'wm: desktop up' "boot 11: the desktop comes up anyway"

python3 scripts/pi_dtb.py "$T/lcd.dtb" --cmdline "milk.lcd=ili9999"
boot 12 "wait@300:wm: desktop up"
stop
check '^lcd: milk.lcd must be ili9486, ili9486p, ili9486p18, ili9488, st7796, ili9481, r61581, hx8357, ili9341 or st7789' "boot 12: an unknown controller is refused"
has '^lcd: panel initialised' && bad "boot 12: a panel was brought up" || ok "boot 12: no panel"
check 'wm: desktop up' "boot 12: the desktop comes up anyway"

# ---- boot 14: milk.lcd=probe, for a display whose controller is unknown: every set-up shows its number card, four colour
# flashes and its text card, then (the probe runs last, after the desktop is up: scripts/pi-lcd-late-test.sh) it finishes. The holds are trimmed to 50 ms (the init waits of 21 set-ups remain). ----
python3 scripts/pi_dtb.py "$T/lcd.dtb" --cmdline "milk.lcd=probe milk.lcd_probe_rounds=1 milk.lcd_probe_ms=50"
boot 14 "wait@600:wm: desktop up" "wait@600:lcd: probe finished"
stop
check '^lcd: probe: 22 set-ups from 1 of 22, 1 round\(s\), text card 50 ms, number card 50 ms, colour flashes 20 ms, pin sweeps on \(D/C GPIO24 reset GPIO25 .*SPI core clock [0-9]+ MHz, (from the firmware|fallback, no firmware answer)\)' "boot 14: probe mode starts, says the clock and where it came from"
N=$(tr -d '\r' < "$LOG" | grep -Eac '^lcd: probe [0-9]+/22 [a-z0-9]+ [0-9]+ MHz mode [03] DC[0-9]+ RST[0-9]+: start ')
[ "$N" = 22 ] && ok "boot 14: all 22 set-ups started ($N lines)" || bad "boot 14: $N set-up start lines, expected 22"
N=$(tr -d '\r' < "$LOG" | grep -Eac '^lcd: probe [0-9]+/22 [a-z0-9]+ [0-9]+ MHz mode [03] DC[0-9]+ RST[0-9]+: done in [0-9]+ ms \(text card [0-9]+ bytes in [0-9]+ ms')
[ "$N" = 22 ] && ok "boot 14: all 22 set-ups ran through (init, number card, flashes, text card: $N done lines)" || bad "boot 14: $N done lines, expected 22"
check '^lcd: probe 1/22 st7796 4 MHz mode 0 DC24 RST25: start \(round 1/1\): milk.lcd=st7796 milk.lcd_hz=4000000; SPI mode 0 divider [0-9]+ = [0-9]+ kHz of core [0-9]+ MHz \(' "boot 14: the first set-up (the 4-inch kits' ST7796S), with its divider and clock"
check '^lcd: probe 2/22 st7796 16 MHz mode 0 DC24 RST25: start ' "boot 14: the second: the same chip at 16 MHz"
check '^lcd: probe 11/22 st7789 4 MHz mode 0 DC24 RST25: start ' "boot 14: the eleventh: the last controller at the slow clock"
check '^lcd: probe 15/22 ili9486 32 MHz mode 0 ' "boot 14: a 32 MHz set-up"
check '^lcd: probe 16/22 ili9486 4 MHz mode 3 DC24 RST25: start .*milk.lcd_spi_mode=3' "boot 14: an SPI mode 3 set-up"
check '^lcd: probe 19/22 ili9486 4 MHz mode 0 DC22 RST27: start .*milk.lcd_dc=22 milk.lcd_rst=27;.*backlight high: GPIO18$' "boot 14: other pins (27 is the reset, so only 18 is the backlight)"
check '^lcd: probe 22/22 ili9486p 4 MHz mode 0 DC25 RST24: start .*milk.lcd_dc=25 milk.lcd_rst=24' "boot 14: the last set-up"
check '^lcd: probe 1/22 .*backlight high: GPIO18 GPIO27$' "boot 14: the free backlight pins are driven high"
check '^lcd: probe finished in [0-9]+ ms: 22 set-up\(s\) shown, 0 skipped' "boot 14: probe ends, nothing skipped"
has 'lcd: probe [0-9]+/22 .*: skipped after|lcd: .*timed out' && bad "boot 14: a set-up was skipped or timed out" || ok "boot 14: nothing skipped, no SPI time-out"
has '^lcd: panel initialised' && bad "boot 14: a panel was left running" || ok "boot 14: no panel after the probe"
has 'KERNEL PANIC|kernel panic' && bad "boot 14: a kernel panic" || ok "boot 14: no panic"
check 'wm: desktop up' "boot 14: the boot goes on after the probe"

# ---- boot 15: the probe's own options: start at set-up 15, the pin sweeps off (no pin round, no backlight pins), three rounds' worth of nothing else ----
python3 scripts/pi_dtb.py "$T/lcd.dtb" --cmdline "milk.lcd=probe milk.lcd_probe_ms=50 milk.lcd_probe_from=16 milk.lcd_probe_pins=0 milk.lcd_spi_mode=7"
boot 15 "wait@300:wm: desktop up" "wait@300:lcd: probe finished"
stop
check '^lcd: probe: 3 set-ups from 16 of 18, 1 round\(s\), .*pin sweeps off ' "boot 15: from=16 with the pin round left out leaves three set-ups"
check '^lcd: milk.lcd_spi_mode must be 0, 1, 2 or 3, using 0' "boot 15: a bad SPI mode is refused"
N=$(tr -d '\r' < "$LOG" | grep -Eac '^lcd: probe [0-9]+/18 .*: start ')
[ "$N" = 3 ] && ok "boot 15: three set-ups ran" || bad "boot 15: $N set-ups ran, expected 3"
check '^lcd: probe 16/18 ili9486 4 MHz mode 3 DC24 RST25: start .*backlight high: none by the probe' "boot 15: with the sweeps off the probe drives no backlight pin"
check '^lcd: probe finished in [0-9]+ ms: 3 set-up\(s\) shown, 0 skipped' "boot 15: probe ends"
check 'wm: desktop up' "boot 15: the boot goes on after the probe"

# ---- boots 16 to 19: the exception dump (ESR decode, checkpoints, register write breadcrumbs). qemu cannot raise an SError, so boot 16 sends a
# fake one through the real crash path (kernel/src/serror.rs `simulate`); boot 17 is a real data abort; boot 18 has the checkpoints off. ----
python3 scripts/pi_dtb.py "$T/dump.dtb" --cmdline "milk.crashtest=serror"
DTB_ARGS=(-dtb "$T/dump.dtb")
boot 16 "wait@300:\*\*\* halted \*\*\*"
sleep 1
stop
check '^\*\*\* KERNEL EXCEPTION: SError \*\*\*' "boot 16: the SError dump"
check '^  vector 7 \(current EL, SP_ELx\), cpu 0, thread ' "boot 16: vector, cpu and thread"
check '^  ESR_EL1 = 0xbe000000  EC=0x2f \(SError\) IL=1 ISS=0x0000000' "boot 16: ESR_EL1 in full with the class name"
check '^  IDS=0 IESB=0 AET=0 \(uncontainable\) EA=0 DFSC=0x00 \(uncategorized\)' "boot 16: the SError syndrome fields"
check '^  this CPU has no RAS extension' "boot 16: the dump says the syndrome is not architected here"
check '^  ELR_EL1 = 0x[0-9a-f]{16}  \(where the CPU was, not the culprit\)' "boot 16: ELR_EL1, with the warning"
check '^  FAR_EL1 = 0x[0-9a-f]{16}$' "boot 16: FAR_EL1"
check '^  SPSR_EL1 = 0x000003c5  EL1h .*DAIF=' "boot 16: SPSR_EL1 decoded"
check 'SCTLR_EL1 = 0x[0-9a-f]+  ISR_EL1 = 0x[0-9a-f]+ \(pending now: .*ID_AA64PFR0_EL1 = 0x[0-9a-f]+ \(RAS: none\)' "boot 16: SCTLR, ISR (pending bits), the RAS field"
check "^  SError at checkpoint 'crashtest-serror' on cpu 0" "boot 16: the checkpoint that took it"
check "^  the access that caused it happened after checkpoint 'crashtest-before' passed and before 'crashtest-serror' " "boot 16: the window the culprit is in"
check '^    0x[0-9a-f]+ genet\+0x4018 <- 0x00000003$' "boot 16: a register write breadcrumb, with the block's name"
check '^  last 8 register writes on cpu 0 \(oldest first\):' "boot 16: the list of writes"
check "^  where: SError AT checkpoint 'crashtest-serror', after 'crashtest-before'" "boot 16: the closing summary names the checkpoints"
check '^  summary: SError, cpu 0, thread .*ESR 0xbe000000 \(EC 0x2f SError\)' "boot 16: the one-line summary"
check '^led: crash code stage=[0-9] ec=0o57 ' "boot 16: the LED code is still the SError's (ec 0o57)"
# The summary is the last thing before 'halted': the crash screen shows the end of the log.
tr -d '\r' < "$LOG" | grep -a -A3 '^  where: SError AT' | grep -aq '^\*\*\* halted' && ok "boot 16: the summary is the last thing in the dump" || bad "boot 16: lines between the summary and 'halted'"

python3 scripts/pi_dtb.py "$T/dump.dtb" --cmdline "milk.crashtest=dabt"
boot 17 "wait@300:\*\*\* halted \*\*\*"
sleep 1
stop
check '^\*\*\* KERNEL EXCEPTION: synchronous exception \*\*\*' "boot 17: the data abort dump"
check '^  ESR_EL1 = 0x96000044  EC=0x25 \(data abort, same EL\) IL=1 ISS=0x0000044' "boot 17: ESR_EL1 in full"
check '^  data abort: translation fault, level 0 on write' "boot 17: the fault and the direction"
check '^  DFSC=0x04 WnR=1 CM=0 S1PTW=0 EA=0 FnV=0' "boot 17: DFSC, WnR and the other flags"
check '^  FAR_EL1 = 0xfffff000dead0010  \(address accessed\)' "boot 17: the address the store hit"
check '^  ELR_EL1 = 0x[0-9a-f]{16}  \(faulting pc\)' "boot 17: ELR_EL1"
check "^  last SError checkpoint that passed on cpu 0: 'crashtest-dabt'" "boot 17: the last checkpoint that passed"
check '^led: crash code stage=[0-9] ec=0o45 ' "boot 17: the LED code is the data abort's (ec 0o45)"

python3 scripts/pi_dtb.py "$T/dump.dtb" --cmdline "milk.serror=off milk.crashtest=serror"
boot 18 "wait@300:\*\*\* halted \*\*\*"
sleep 1
stop
check '^serror: checkpoints and write breadcrumbs off \(milk.serror=off\)' "boot 18: milk.serror=off is acknowledged"
check '^  SError checkpoints are off \(milk.serror=off\): no attribution' "boot 18: the dump says there is no attribution"
has "SError at checkpoint|register writes on cpu|serror: a checkpoint costs" && bad "boot 18: checkpoints ran with milk.serror=off" || ok "boot 18: no checkpoints, no breadcrumbs, no timing"
check '^  ESR_EL1 = 0xbe000000  EC=0x2f \(SError\)' "boot 18: the ESR is decoded regardless"

# boot 19: milk.serror=paranoid, a checkpoint after every register write of the drivers: the whole early boot runs through it, then the fake SError
# (taken at 'mmio-write' this time, so the dump names the register of the newest write).
python3 scripts/pi_dtb.py "$T/dump.dtb" --cmdline "milk.serror=paranoid milk.crashtest=serror"
boot 19 "wait@300:\*\*\* halted \*\*\*"
sleep 1
stop
check '^serror: paranoid: checkpoints and write breadcrumbs on, and a checkpoint after every register write' "boot 19: paranoid mode is acknowledged"
check '^\*\*\* KERNEL EXCEPTION: SError \*\*\*' "boot 19: the dump comes out of paranoid mode (the early boot ran through a checkpoint per register write)"
check '^\[sdhci\] .*card ready' "boot 19: the SD card was brought up with a checkpoint after every register write"
check "^  SError at checkpoint 'crashtest-serror' on cpu 0" "boot 19: the fake SError"

# ---- boots 20 to 25: the framebuffer's size (docs/raspberry-pi.md, "HDMI displays"). qemu's firmware says its display is 640x480
# (no monitor behind it); pi_dtb.py pins milk.fbsize=1024x768 for the boots above, these name their own. ----
dsize() { # PNG -> "WxH"
  python3 -I -c 'import struct,sys; d=open(sys.argv[1],"rb").read(); print("%dx%d" % struct.unpack(">II", d[16:24]))' "$1"
}
# boot 20: the 4-inch panel's own size, forced, with its touch chip and no SPI panel: the desktop at 800x480, the touch driver late and quiet.
python3 scripts/pi_dtb.py "$T/fb.dtb" --cmdline "milk.fbsize=800x480 milk.touch=xpt2046"
DTB_ARGS=(-dtb "$T/fb.dtb")
boot 20 "wait@300:wm: desktop up" "wait@300:touch: running" "send:system" "wait:$P" "send:text show /proc/input" "wait:$P"
python3 scripts/qmp.py "$QMP" screendump "$T/fb800.png" >/dev/null 2>&1 || true
stop
check '^mbox: display size the firmware has: 640x480 \(get physical width/height, tag 0x40003\)' "boot 20: the firmware's display size is logged, and where it came from"
check '^rpi fb: display size 640x480 \(firmware, physical size\), framebuffer 800x480: forced by milk.fbsize' "boot 20: milk.fbsize=800x480 is used, and the log says why"
check '^rpi fb: 800x480x32 pitch 3200 at ' "boot 20: the framebuffer is 800x480"
check '^display: rpi mailbox framebuffer 800x480 XRGB8888' "boot 20: the boot log console follows the size"
check 'wm: desktop up' "boot 20: the desktop"
check '^touch: XPT2046 on SPI0 CE1 \(GPIO 7\), MISO GPIO 9, MOSI GPIO 10, SCLK GPIO 11, .*pen IRQ GPIO 17; the display.s pins .* are not touched' "boot 20: milk.touch=xpt2046: the pins used are logged, the display's are not touched"
check '^touch: running, map raw -> 800x480 pixels \(default\): milk.touch_cal=14152,0,-43,0,8484,-25 \(rot 0\)' "boot 20: the touch map is for the actual framebuffer size"
has '^lcd: ' && bad "boot 20: the SPI panel driver said something with no milk.lcd" || ok "boot 20: no SPI panel driver output"
[ "$(tr -d '\r' < "$LOG" | grep -an '^led: done, solid on' | head -1 | cut -d: -f1)" -lt "$(tr -d '\r' < "$LOG" | grep -an '^touch: XPT2046' | head -1 | cut -d: -f1)" ] && ok "boot 20: the touch thread started after the boot was over" || bad "boot 20: the touch thread did not wait for the end of the boot"
has 'KERNEL PANIC|kernel panic|touch: .*(timed out|does not answer)' && bad "boot 20: a panic or a touch time-out" || ok "boot 20: no panic, no time-out"
out system | grep -Eq '^CPUs +4' && ok "boot 20: the system runs" || bad "boot 20: system: $(out system)"
out "text show /proc/input" | tr -d ' ' | grep -Eq '^buttons0$' && out "text show /proc/input" | tr -d ' ' | grep -Eq '^moves0$' && ok "boot 20: nothing is attached to the touch chip: no pointer events are invented" || bad "boot 20: /proc/input: $(out 'text show /proc/input' | tr '\n' ' ')"
desktop_shot "$T/fb800.png" 800 480 && ok "boot 20: the screendump is an 800x480 desktop" || bad "boot 20: the screen is not an 800x480 desktop ($(dsize "$T/fb800.png"))"

# boot 21: 1280x768, the 5:3 size for the panel behind a 1024x768 firmware mode, with the touch map for it, and a touch calibration given on the line.
python3 scripts/pi_dtb.py "$T/fb.dtb" --cmdline "milk.fbsize=1280x768 milk.touch=xpt2046 milk.touch_rot=90 milk.touch_irq=-1"
boot 21 "wait@300:wm: desktop up" "wait@300:touch: running"
python3 scripts/qmp.py "$QMP" screendump "$T/fb1280.png" >/dev/null 2>&1 || true
stop
check '^rpi fb: 1280x768x32 pitch 5120 at ' "boot 21: the framebuffer is 1280x768"
check '^touch: .*pen IRQ none \(polled\)' "boot 21: milk.touch_irq=-1 polls"
check '^touch: running, map raw -> 1280x768 pixels \(default\): .* \(rot 90\)' "boot 21: the touch map follows the framebuffer, and milk.touch_rot=90"
desktop_shot "$T/fb1280.png" 1280 768 && ok "boot 21: the screendump is a 1280x768 desktop" || bad "boot 21: the screen is not a 1280x768 desktop ($(dsize "$T/fb1280.png"))"

# boot 22: a crash at 800x480: the boot log console and the crash screen fill the panel, the sign-of-life band is as wide as it.
python3 scripts/pi_dtb.py "$T/fb.dtb" --cmdline "milk.fbsize=800x480 milk.crashtest=early"
boot 22 "wait@300:KERNEL PANIC"
sleep 2
python3 scripts/qmp.py "$QMP" screendump "$T/fbcrash.png" >/dev/null 2>&1 || true
stop
if [ -s "$T/fbcrash.png" ] && [ "$(dsize "$T/fbcrash.png")" = 800x480 ]; then
  case "$(pixels "$T/fbcrash.png" 400,2 400,5 795,2 795,5)" in
    "48,208,96 255,255,255 48,208,96 255,255,255") ok "boot 22: an 800x480 screen, the sign-of-life band across all of it" ;;
    *) bad "boot 22: no band across the 800x480 screen: $(pixels "$T/fbcrash.png" 400,2 400,5 795,2 795,5)" ;;
  esac
  case "$(pixels "$T/fbcrash.png" 795,478 790,470)" in
    "22,21,19 22,21,19") ok "boot 22: the text console fills the bottom right corner of the 800x480 screen" ;;
    *) bad "boot 22: the console does not fill the screen: $(pixels "$T/fbcrash.png" 795,478 790,470)" ;;
  esac
else
  bad "boot 22: no 800x480 screendump ($(dsize "$T/fbcrash.png" 2>/dev/null))"
fi

# boot 23: milk.fbsize=auto: the firmware's own display size (qemu says 640x480), the smallest the desktop is made for.
python3 scripts/pi_dtb.py "$T/fb.dtb" --cmdline "milk.fbsize=auto"
boot 23 "wait@300:wm: desktop up"
python3 scripts/qmp.py "$QMP" screendump "$T/fbauto.png" >/dev/null 2>&1 || true
stop
check "^rpi fb: display size 640x480 \\(firmware, physical size\\), framebuffer 640x480: the firmware's display mode" "boot 23: milk.fbsize=auto follows the firmware"
desktop_shot "$T/fbauto.png" 640 480 && ok "boot 23: the screendump is a 640x480 desktop" || bad "boot 23: not a 640x480 desktop ($(dsize "$T/fbauto.png"))"

# boot 24: a value that cannot be used is said so and the firmware's size is followed.
python3 scripts/pi_dtb.py "$T/fb.dtb" --cmdline "milk.fbsize=99999x1"
boot 24 "wait@300:wm: desktop up"
stop
check '^rpi fb: milk.fbsize is outside 640x400..4096x2160, using auto' "boot 24: a size out of range is refused"
check '^rpi fb: 640x480x32 pitch 2560 at ' "boot 24: and the firmware's size is used"

# boot 25: 1920x1080 (a television): the desktop at that size.
python3 scripts/pi_dtb.py "$T/fb.dtb" --cmdline "milk.fbsize=1920x1080"
boot 25 "wait@300:wm: desktop up"
python3 scripts/qmp.py "$QMP" screendump "$T/fb1080.png" >/dev/null 2>&1 || true
stop
check '^rpi fb: 1920x1080x32 pitch 7680 at ' "boot 25: the framebuffer is 1920x1080"
desktop_shot "$T/fb1080.png" 1920 1080 && ok "boot 25: the screendump is a 1920x1080 desktop" || bad "boot 25: not a 1920x1080 desktop ($(dsize "$T/fb1080.png"))"

# boot 26: milk.pcie=probe on a board with no PCIe controller in its tree (qemu's raspi4b): the probe mode is acknowledged, the root complex is
# not touched (an access would be a bus error here), USB is skipped with its reason, and the desktop comes up and keeps running.
python3 scripts/pi_dtb.py "$T/pcie.dtb" --cmdline "milk.pcie=probe milk.crashtest=serror-absorb"
DTB_ARGS=(-dtb "$T/pcie.dtb")
boot 26 "wait@300:wm: desktop up" \
  "send:system" "wait:$P"
stop
check '^\[pcie\] milk.pcie=probe: a checkpoint after every register access' "boot 26: the probe mode is acknowledged"
check '^\[pcie\] the device tree has no \(enabled\) PCIe controller' "boot 26: no controller in the tree: skipped cleanly"
has '^pcie: step ' && bad "boot 26: a PCIe step ran without a controller" || ok "boot 26: no step ran"
has 'KERNEL EXCEPTION|KERNEL PANIC|kernel panic' && bad "boot 26: an exception or a panic" || ok "boot 26: no exception, no panic"
check 'wm: desktop up' "boot 26: the desktop is up"
check '^serror: absorb self-test: ok ' "boot 26: an SError is survived at a pcie-* checkpoint (and under tolerate), refused elsewhere"

# ---- boot 13 (only if MILK_REAL_DTB names a device tree from a real board's boot partition: it is not in the repository) ----
if [ -n "${MILK_REAL_DTB:-}" ] && [ -r "$MILK_REAL_DTB" ]; then
  DTB_ARGS=(-dtb "$MILK_REAL_DTB")
  boot 13 "wait@300:wm: desktop up"
  stop
  check 'platform: Raspberry Pi 4 \(BCM2711\) \(device tree: raspberrypi,4-model-b\)' "boot 13: the real tree is accepted"
  check '^\[smp\] 4 of 4 CPUs online \(spin table\)' "boot 13: four CPUs from the real tree's spin table"
  check '\[genet\] no Ethernet controller' "boot 13: nodes qemu switched off are skipped (GENET)"
  check '\[pcie\] the device tree has no \(enabled\) PCIe' "boot 13: nodes qemu switched off are skipped (PCIe)"
  has 'KERNEL PANIC|kernel panic' && bad "boot 13: a kernel panic" || ok "boot 13: no panic"
fi

# After the early panic the screen kept the boot log and the green band over it (not cleared, no splash).
case "$(pixels "$T/crash-early.png" 500,2 500,5)" in
  "48,208,96 255,255,255") ok "boot 4: the sign-of-life band is on top (green, then a white line)" ;;
  *) bad "boot 4: no sign-of-life band: $(pixels "$T/crash-early.png" 500,2 500,5)" ;;
esac

if [ "$FAIL" = 0 ]; then
  echo "pi-qemu-test: PASS"
  [ "$OWN_WORK" = 1 ] && [ "${MILK_KEEP:-}" != 1 ] && { rm -rf "$T" || true; }
else
  echo "pi-qemu-test: FAIL (logs in $T)"; exit 1
fi
