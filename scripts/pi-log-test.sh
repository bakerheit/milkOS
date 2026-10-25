#!/usr/bin/env bash
# The boot log in a file on the SD card (kernel/src/rpi/logfile.rs, docs/raspberry-pi.md, "The boot log in a file"),
# end to end on qemu's `-M raspi4b`. The card is an image with an MBR and a FAT32 partition holding a pre-created
# MILKLOG.TXT (scripts/pi_fat32.py), booted WITHOUT milk.sdwrite=1 (the log writer has its own narrow path):
#   boot 1: a card whose log file is split into 64 pieces: the boot log ends up in the file, with its header (version,
#           build), the diagnostic lines (boot, cpu, dtb, mbox, sdhci), the LED stage lines, and the end marker;
#           scripts/pi-readlog.sh says the marker is there; the LED shows the "log saved" signal once everything is on the card;
#           and EVERY OTHER SECTOR OF THE IMAGE IS UNCHANGED (the kernel wrote only inside the file's own clusters);
#   boot 2: the same card again with milk.crashtest=late: the second boot's log replaces the first (another boot number)
#           and ends with the crash dump and the same crash code the LED cycle carries; nothing outside the file changed;
#   boots 3 to 10: cards the writer must refuse and leave alone (a loop in the file's chain, a chain that is too short, too
#           long, outside the volume or through a free cluster, no log file, no partition table, a file under 4 KiB):
#           the image is byte for byte what it was, and the log says why.
# Offline, private images in a fresh /tmp folder, the qemu started here killed by PID. TCG: about 15 s a boot.
#
#   scripts/pi-log-test.sh [workdir]      (MILK_KEEP=1 keeps the work folder; MILK_PREBUILT=1 skips the build)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="${1:-$(mktemp -d /tmp/milk-pilog.XXXXXX)}"
mkdir -p "$T"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
if [ "${MILK_PREBUILT:-}" = 1 ]; then
  ELF="target/pi/aarch64-unknown-none-softfloat/debug/milkos-kernel"
  [ -e "$ELF" ] || { echo "pi-log-test: MILK_PREBUILT=1 but $ELF is missing"; exit 1; }
  "$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$T/kernel8.img"
else
  scripts/pi-build.sh "$T/kernel8.img" >/dev/null
fi
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

# boot N IMAGE CMDLINE STEPS...: start qemu on the card image, drive the serial console with the steps.
boot() {
  local n="$1" img="$2" cmdline="$3"; shift 3
  python3 scripts/pi_dtb.py "$T/b$n.dtb" --cmdline "$cmdline"
  SOCK="$T/s$n.sock"; QMP="$T/q$n.sock"; LOG="$T/serial$n.log"
  : > "$LOG"
  qemu-system-aarch64 -M raspi4b -kernel "$T/kernel8.img" -dtb "$T/b$n.dtb" -display none -monitor none \
    -serial "unix:$SOCK,server=on,wait=on" -qmp "unix:$QMP,server,nowait" \
    -drive "file=$img,if=sd,format=raw" >/dev/null 2>&1 &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/done$n" &
  D=$!
  SP=""
  if [ "${SAMPLE:-}" = 1 ]; then
    : > "$T/pin$n"
    ( while [ ! -e "$T/done$n" ] && kill -0 "$QPID" 2>/dev/null; do
        v=$(python3 scripts/qmp.py "$QMP" hmp "xp /1wx 0xfe200038" 2>/dev/null | grep -Eao '0x[0-9a-f]+' | head -1) || true
        [ -n "$v" ] && printf '%s' $(( (v >> 10) & 1 )) >> "$T/pin$n"
      done ) &
    SP=$!
  fi
  for _ in $(seq 1500); do [ -e "$T/done$n" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
  [ -e "$T/done$n" ] || bad "boot $n: the serial script did not finish (see $LOG)"
}
stop() {
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  { kill "$D"; wait "$D"; } 2>/dev/null || true
  [ -n "${SP:-}" ] && { wait "$SP"; } 2>/dev/null || true
}
has() { tr -d '\r' < "$LOG" | grep -Eaq -- "$1"; }
check() { if has "$1"; then ok "$2: $(tr -d '\r' < "$LOG" | grep -Ea -m1 -- "$1" | cut -c1-110)"; else bad "$2 (no line matching: $1)"; fi; }

# The log file's bytes out of an image, by the layout file: file.bin.
extract() { python3 -I - "$1" "$2" "$3" <<'PY'
import json, sys
img, layout, out = sys.argv[1:4]
lay = json.load(open(layout))
data = open(img, "rb").read()
buf = b"".join(data[l * 512:(l + lay_n) * 512] for l, lay_n in lay["log_extents"])
open(out, "wb").write(buf[:lay["log_size"]])
PY
}
# How many sectors differ between two images outside the log file / inside it: "OUTSIDE INSIDE".
diffcount() { python3 -I - "$1" "$2" "$3" <<'PY'
import json, sys
a, b, layout = sys.argv[1:4]
lay = json.load(open(layout))
inside = set()
for l, n in lay["log_extents"]:
    inside.update(range(l, l + n))
x, y = open(a, "rb").read(), open(b, "rb").read()
assert len(x) == len(y), (len(x), len(y))
out_n = in_n = 0
for s in range(len(x) // 512):
    if x[s * 512:(s + 1) * 512] != y[s * 512:(s + 1) * 512]:
        if s in inside: in_n += 1
        else: out_n += 1
print(out_n, in_n)
PY
}

# ---- the Mac-side reader on made-up files ----
python3 -I - "$T" <<'PY'
import sys
t = sys.argv[1]
head = b"MILKLOG 1 boot=0000abcd\nmilkOS test\n--------\nline one\nline two\n"
mk = lambda boot, fl, n: b"\n<<milklog end boot=%s flush=%d bytes=%d>>\n" % (boot, fl, n)
open(t + "/r-blank.txt", "wb").write(b"MILKLOG.TXT: not written yet\n" + b" " * 3000)
open(t + "/r-good.txt", "wb").write(head[:-1] + mk(b"0000abcd", 3, len(head) - 1) + b" " * 3000)
open(t + "/r-cut.txt", "wb").write(head + b"line thr" + b" " * 3000)
open(t + "/r-stale.txt", "wb").write(head + b"line thr" + b" " * 100 + mk(b"00001111", 9, 5) + b" " * 100)
PY
set +e
scripts/pi-readlog.sh "$T/r-blank.txt" >/dev/null 2>&1; [ $? = 2 ] && ok "pi-readlog: a blank file is 'not written by a kernel' (exit 2)" || bad "pi-readlog: blank file"
scripts/pi-readlog.sh "$T/r-good.txt" > "$T/r-good.out" 2>/dev/null; RC=$?
[ $RC = 0 ] && grep -q '^line two$' "$T/r-good.out" && ! grep -q 'milklog end' "$T/r-good.out" && ok "pi-readlog: text without padding or marker, exit 0 with the marker" || bad "pi-readlog: good file (exit $RC)"
scripts/pi-readlog.sh "$T/r-cut.txt" > "$T/r-cut.out" 2>/dev/null; RC=$?
[ $RC = 1 ] && grep -q '^line thr$' "$T/r-cut.out" && ok "pi-readlog: no marker -> the text up to the padding, exit 1" || bad "pi-readlog: cut file (exit $RC)"
scripts/pi-readlog.sh "$T/r-stale.txt" >/dev/null 2>&1; [ $? = 1 ] && ok "pi-readlog: a marker from another boot does not count (exit 1)" || bad "pi-readlog: stale marker"
set -e

# ---- boot 1: a fragmented log file ----
python3 -I scripts/pi_fat32.py "$T/card.img" --fragment --layout "$T/card.json" >/dev/null
cp "$T/card.img" "$T/card0.img"
SAMPLE=1 boot 1 "$T/card.img" "milk.fbscale=1" "wait@300:led: done, solid on"
sleep 8
extract "$T/card.img" "$T/card.json" "$T/log1.bin"
stop
set +e; scripts/pi-readlog.sh "$T/log1.bin" > "$T/log1.txt" 2> "$T/log1.verdict"; RC=$?; set -e
[ "$RC" = 0 ] && ok "boot 1: pi-readlog: $(cat "$T/log1.verdict" | cut -c1-120)" || bad "boot 1: pi-readlog exit $RC: $(cat "$T/log1.verdict")"
python3 -I -c "
import json; l=json.load(open('$T/card.json')); print(len(l['log_extents']))" | grep -q '^64$' && ok "boot 1: the card's log file is in 64 pieces" || bad "boot 1: the log file is not fragmented as intended"
check '^milklog: MILKLOG.TXT found: .* in 64 piece' "boot 1: the writer found the fragmented file"
for pat in '^MILKLOG 1 boot=[0-9a-f]{8}$' '^milkOS .*\(build ' '^milkOS .* booting' '^boot: entered at EL2' '^cpu: MIDR 0x[0-9a-f]+' '^dtb: .*compatible' '^dtb: kernel command line: ' '^mbox: the firmware answers through bus alias' '^mbox: board revision ' '^mbox: clock arm ' '^\[sdhci\] .*card ready' '^\[sdhci\] .*base clock [0-9]+ Hz \(' '^led: stage 1 memory$' '^led: stage 2 mailbox$' '^led: stage 5 sdcard$' '^led: stage 7 userspace$' '^led: done, solid on$' 'wm: desktop up'; do
  if grep -Eaq -- "$pat" "$T/log1.txt"; then ok "boot 1: the file has: $(grep -Ea -m1 -- "$pat" "$T/log1.txt" | cut -c1-100)"; else bad "boot 1: the file has no line matching $pat"; fi
done
# The file is the serial log, from the first line to the last it had saved by then.
python3 -I - "$T/log1.txt" "$T/serial1.log" <<'PY' && ok "boot 1: the file's text is the serial log (the same lines in the same order)" || bad "boot 1: the file and the serial log disagree"
import re, sys
f = [l.rstrip("\r") for l in open(sys.argv[1], errors="replace").read().split("\n")]
s = [l.rstrip("\r") for l in open(sys.argv[2], errors="replace").read().split("\n")]
# Drop the file's own header (up to the rule) and compare the next 40 lines with the serial log from the same place.
i = f.index("--------") + 1
first = f[i]
j = s.index(first)
n = 0
for a, b in zip(f[i:], s[j:]):
    if a != b:
        if "milklog:" in a or "milklog:" in b or "sdhci" in a:
            continue
        print("differs:", repr(a), repr(b)); sys.exit(1)
    n += 1
assert n > 100, n
PY
read OUT IN <<<"$(diffcount "$T/card0.img" "$T/card.img" "$T/card.json")"
[ "$OUT" = 0 ] && ok "boot 1: every sector outside the log file is unchanged" || bad "boot 1: $OUT sectors outside the log file changed"
[ "$IN" -gt 10 ] && ok "boot 1: $IN sectors of the log file were written" || bad "boot 1: only $IN log sectors written"
PIN=$(cat "$T/pin1")
# After the desktop is up the LED shows solid on and then, once everything is saved, the two-blink signal: dark again.
TAIL="${PIN: -60}"
case "$TAIL" in *0*1*) ok "boot 1: the LED shows the log saved signal (dark again after solid on: ...${TAIL: -30})";; *) bad "boot 1: no log saved signal on the LED: ...${TAIL}";; esac
ID1=$(head -1 "$T/log1.txt")

# ---- boot 2: the same card, with a crash ----
cp "$T/card.img" "$T/card1.img"
boot 2 "$T/card.img" "milk.crashtest=late" "wait@300:KERNEL PANIC"
sleep 6
extract "$T/card.img" "$T/card.json" "$T/log2.bin"
stop
set +e; scripts/pi-readlog.sh "$T/log2.bin" > "$T/log2.txt" 2> "$T/log2.verdict"; RC=$?; set -e
[ "$RC" = 0 ] && ok "boot 2: pi-readlog: $(cut -c1-120 "$T/log2.verdict")" || bad "boot 2: pi-readlog exit $RC: $(cat "$T/log2.verdict")"
[ "$(head -1 "$T/log2.txt")" != "$ID1" ] && ok "boot 2: a new boot number ($(head -1 "$T/log2.txt")), not the first boot's" || bad "boot 2: same boot number as boot 1"
CODE=$(tr -d '\r' < "$LOG" | sed -n -E 's/^led: crash code (stage=[0-9]+ ec=0o[0-7]+ pc=0o[0-7]+ far=0o[0-7]+) .*/\1/p' | head -1)
[ -n "$CODE" ] && grep -aFq "led: crash code $CODE" "$T/log2.txt" && ok "boot 2: the file carries the crash code the LED cycle shows ($CODE)" || bad "boot 2: crash code '$CODE' is not in the file"
grep -aq 'KERNEL PANIC' "$T/log2.txt" && ok "boot 2: the file has the panic" || bad "boot 2: no panic text in the file"
grep -aq 'crash-test: a deliberate panic, late' "$T/log2.txt" && ok "boot 2: the file has the panic message" || bad "boot 2: no panic message in the file"
grep -aq 'led: stage ' "$T/log2.txt" && grep -aq 'wm: desktop up' "$T/log2.txt" && ok "boot 2: the file has the whole boot before the panic" || bad "boot 2: the boot before the panic is missing"
read OUT IN <<<"$(diffcount "$T/card1.img" "$T/card.img" "$T/card.json")"
[ "$OUT" = 0 ] && ok "boot 2: every sector outside the log file is unchanged" || bad "boot 2: $OUT sectors outside the log file changed"

# ---- boots 3 to 10: cards the writer must leave alone ----
N=3
for defect in loop short long range free nofile nombr tiny; do
  python3 -I scripts/pi_fat32.py "$T/bad-$defect.img" --defect "$defect" --layout "$T/bad-$defect.json" >/dev/null
  cp "$T/bad-$defect.img" "$T/bad-$defect.orig"
  boot $N "$T/bad-$defect.img" "milk.ledcodes=0" "wait@300:led: stage 7 userspace"
  sleep 3
  stop
  if cmp -s "$T/bad-$defect.img" "$T/bad-$defect.orig"; then ok "boot $N ($defect): the card is byte for byte unchanged"; else bad "boot $N ($defect): the card CHANGED"; fi
  has '^milklog: not writing a boot log file: ' && ok "boot $N ($defect): $(tr -d '\r' < "$LOG" | grep -Ea -m1 '^milklog: not writing' | cut -c1-120)" || bad "boot $N ($defect): no refusal line in the log"
  has 'KERNEL PANIC|kernel panic' && bad "boot $N ($defect): a kernel panic" || true
  N=$((N + 1))
done

if [ "$FAIL" = 0 ]; then
  echo "pi-log-test: all passed"
  [ -z "${MILK_KEEP:-}" ] && [ "${T#/tmp/milk-pilog.}" != "$T" ] && rm -rf "$T"
  exit 0
fi
echo "pi-log-test: FAILED (work folder kept: $T)"
exit 1
