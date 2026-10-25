#!/usr/bin/env bash
# The SD card log (kernel/src/rpi/logfile.rs, docs/raspberry-pi.md, "Reading the log") on a card whose log file is split
# into many pieces, and on cards that must be refused, end to end on qemu's `-M raspi4b`. The card is an image with an MBR
# and a FAT32 partition holding a pre-created MILKLOG.TXT (scripts/pi_fat32.py), booted WITHOUT milk.sdwrite=1 (the log
# writer has its own narrow path). The multi-boot story (three boots in a row, a wrap) is scripts/pi-log-multiboot-test.sh.
#   boot 1: a card whose 256 KiB log file is split into 64 pieces: the log ends up in the file with its boot banner (version,
#           build, git hash), the diagnostic lines (boot, cpu, dtb, mbox, sdhci), the LED stage lines, the tagged
#           lines of the audit log and the services, and a system report; the lines that went to the serial port are all in
#           the file, in the same order; scripts/pi-readlog.sh reads it; the LED shows the "log saved" signal once everything is
#           on the card; and EVERY OTHER SECTOR OF THE IMAGE IS UNCHANGED (the kernel wrote only inside the file's own clusters);
#   boot 2: the same card again with milk.crashtest=late: this boot is appended after the first (boot 2, the first boot's
#           text is still there) and ends with the crash dump, the same crash code the LED cycle carries and the end line;
#           nothing outside the file changed;
#   boots 3 to 12: cards the writer must refuse and leave alone (a loop in the file's chain, a chain that is too short, too
#           long, outside the volume or through a free cluster, no log file, no partition table, a file under 4 KiB, a file that
#           holds a format 1 log, a file that holds someone's text): the image is byte for byte what it was, and the log says why.
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
    # The LED is sampled until the test says enough ($T/pinstop$n): the log saved signal only starts some seconds after "solid on".
    ( while [ ! -e "$T/pinstop$n" ] && kill -0 "$QPID" 2>/dev/null; do
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

# The file's text without the uptime stamps (what the serial port printed, plus tagged lines of its own).
unstamp() { sed -E 's/^\[ *[0-9]+\.[0-9]{3}\] //'; }

# ---- boot 1: a fragmented log file ----
python3 -I scripts/pi_fat32.py "$T/card.img" --fragment --layout "$T/card.json" >/dev/null
cp "$T/card.img" "$T/card0.img"
SAMPLE=1 boot 1 "$T/card.img" "milk.fbscale=1" "wait@300:led: done, solid on"
DONE_AT=$(wc -c < "$T/pin1")
# Wait (up to a minute) for the LED to show what the log writer says: solid on, then the two-blink "log saved" signal.
for _ in $(seq 30); do
  AFTER=$(tail -c +"$((DONE_AT + 1))" "$T/pin1")
  case "$AFTER" in *1111*0000*1*) break ;; esac
  sleep 2
done
sleep 2
touch "$T/pinstop1"
extract "$T/card.img" "$T/card.json" "$T/log1.bin"
stop
set +e; scripts/pi-readlog.sh "$T/log1.bin" 2> "$T/log1.verdict" | unstamp > "$T/log1.txt"; RC=${PIPESTATUS[0]}; set -e
[ "$RC" = 0 ] && ok "boot 1: pi-readlog: $(cut -c1-140 "$T/log1.verdict" | head -1)" || bad "boot 1: pi-readlog exit $RC: $(cat "$T/log1.verdict")"
python3 -I -c "
import json; l=json.load(open('$T/card.json')); print(len(l['log_extents']))" | grep -q '^64$' && ok "boot 1: the card's log file is in 64 pieces" || bad "boot 1: the log file is not fragmented as intended"
check '^milklog: MILKLOG.TXT found: .* in 64 piece' "boot 1: the writer found the fragmented file"
for pat in '^=== milkOS boot 1 === milkOS [^ ]+ .*\| build [^ ]+ \| git [^ ]+ \| log format 2' '^milkOS .* booting' '^boot: entered at EL2' '^cpu: MIDR 0x[0-9a-f]+' '^dtb: .*compatible' '^dtb: kernel command line: ' '^mbox: the firmware answers through bus alias' '^mbox: board revision ' '^mbox: clock arm ' '^\[sdhci\] .*card ready' '^\[sdhci\] .*base clock [0-9]+ Hz \(' '^led: stage 1 memory$' '^led: stage 2 mailbox$' '^led: stage 5 sdcard$' '^led: stage 7 ' '^led: done, solid on$' 'wm: desktop up' '^\[audit\] [0-9]+ pid [0-9]+ .*(exec|fs.mount)' '^\[journal milkd/[0-9]+\] ' '^\[tty1\] msh' '^\[report\] ==== system report 1, boot 1, uptime' '^\[report\] -- /proc/meminfo' '^\[report\] -- /proc/threads' '^\[report\] -- processes' '^\[report\] temperature: ' '^\[report\] milklog: writing to the log file'; do
  if grep -Eaq -- "$pat" "$T/log1.txt"; then ok "boot 1: the file has: $(grep -Ea -m1 -- "$pat" "$T/log1.txt" | cut -c1-100)"; else bad "boot 1: the file has no line matching $pat"; fi
done
# Every line of the serial log is in the file, in the same order (the file adds tagged lines of its own).
python3 -I - "$T/log1.txt" "$T/serial1.log" <<'PY' && ok "boot 1: the file's console lines are the serial log (the same lines in the same order)" || bad "boot 1: the file and the serial log disagree"
import re, sys
f = open(sys.argv[1], errors="replace").read().split("\n")
s = [l.rstrip("\r") for l in open(sys.argv[2], errors="replace").read().split("\n")]
tagged = re.compile(r"^(\[(report|milklog|tty[0-9]|pty[0-9])\] |\[audit\] [0-9]+ pid |\[journal [^\]]*/[0-9]+\] )")
own = ("milklog:", "[sdhci]")  # printed while the log is being written: where they fall depends on the flush
f = [l for l in f if l and not tagged.match(l) and not l.startswith(("=== milkOS boot", "previous boot")) and not any(o in l for o in own)]
s = [l for l in s if l and not any(o in l for o in own)]
# A shell prompt waiting for input is a line of its own in the file when the next output comes more than a moment later
# (the log writer ends a partial line after 1.5 s of silence): join it to what follows, on both sides.
def join_prompts(ls):
    out = []
    for l in ls:
        if out and out[-1].endswith("$ "):
            out[-1] += l
        else:
            out.append(l)
    return out
f, s = join_prompts(f), join_prompts(s)
j = s.index(f[0])
f = f[:-1]  # the last line may be cut where the last flush ended
s = s[j:]
assert len(f) > 100, len(f)
# Two CPUs print at the same time (the first user program says hello while the kernel is still logging), so a few
# lines come out of the serial port mixed in another way than they are in the file. Everything else is the same lines in
# the same order: the lines that are unique on both sides keep their order, and at most 3 lines of the file are not in the
# serial log at all.
from collections import Counter
cs, cf = Counter(s), Counter(f)
uniq = [l for l in f if cf[l] == 1 and cs.get(l, 0) == 1]
assert len(uniq) > 100, len(uniq)
at = {l: i for i, l in enumerate(s)}
order = [at[l] for l in uniq]
if order != sorted(order):
    k = next(i for i in range(1, len(order)) if order[i] < order[i - 1])
    print("order differs at", repr(uniq[k - 1]), repr(uniq[k])); sys.exit(1)
missing = [l for l in f if cs.get(l, 0) == 0]
if len(missing) > 3:
    print("not in the serial log:", missing[:5]); sys.exit(1)
PY
read OUT IN <<<"$(diffcount "$T/card0.img" "$T/card.img" "$T/card.json")"
[ "$OUT" = 0 ] && ok "boot 1: every sector outside the log file is unchanged" || bad "boot 1: $OUT sectors outside the log file changed"
[ "$IN" -gt 10 ] && ok "boot 1: $IN sectors of the log file were written" || bad "boot 1: only $IN log sectors written"
# After "led: done, solid on" the LED is solid on and then, once everything is saved and nothing new came for two seconds, it shows
# the two-blink signal: dark again, with a blip. (The pin is sampled over QMP, a few times a second: it sees the dark, and a blip now and then.)
AFTER=$(tail -c +"$((DONE_AT + 1))" "$T/pin1")
case "$AFTER" in *1111*0000*1*) ok "boot 1: the LED shows solid on and then the log saved signal (dark again, with blips): $(echo "$AFTER" | tr -s 01 | cut -c1-60)";; *) bad "boot 1: no log saved signal on the LED after solid on: $AFTER";; esac
cp "$T/log1.txt" "$T/log1.first.txt"

# ---- boot 2: the same card, with a crash ----
cp "$T/card.img" "$T/card1.img"
boot 2 "$T/card.img" "milk.crashtest=late" "wait@300:KERNEL PANIC"
sleep 6
extract "$T/card.img" "$T/card.json" "$T/log2.bin"
stop
set +e; scripts/pi-readlog.sh "$T/log2.bin" --last 2> "$T/log2.verdict" | unstamp > "$T/log2.txt"; RC=${PIPESTATUS[0]}; set -e
[ "$RC" = 0 ] && ok "boot 2: pi-readlog: $(cut -c1-140 "$T/log2.verdict" | head -1)" || bad "boot 2: pi-readlog exit $RC: $(cat "$T/log2.verdict")"
head -1 "$T/log2.txt" | grep -Eaq '^=== milkOS boot 2 === ' && ok "boot 2: a new boot number: $(head -1 "$T/log2.txt" | cut -c1-60)" || bad "boot 2: the last boot is not boot 2: $(head -1 "$T/log2.txt" | cut -c1-80)"
sed -n 2p "$T/log2.txt" | grep -Eaq '^previous boot 1: no end recorded' && ok "boot 2: the banner says boot 1 ended in a power cut / without an end line" || bad "boot 2: the banner does not say how boot 1 ended: $(sed -n 2p "$T/log2.txt")"
CODE=$(tr -d '\r' < "$LOG" | sed -n -E 's/^led: crash code (stage=[0-9]+ ec=0o[0-7]+ pc=0o[0-7]+ far=0o[0-7]+) .*/\1/p' | head -1)
[ -n "$CODE" ] && grep -aFq "led: crash code $CODE" "$T/log2.txt" && ok "boot 2: the file carries the crash code the LED cycle shows ($CODE)" || bad "boot 2: crash code '$CODE' is not in the file"
grep -aq 'KERNEL PANIC' "$T/log2.txt" && ok "boot 2: the file has the panic" || bad "boot 2: no panic text in the file"
grep -aq 'crash-test: a deliberate panic, late' "$T/log2.txt" && ok "boot 2: the file has the panic message" || bad "boot 2: no panic message in the file"
grep -aq '^=== milkOS boot 2 ended: crash ===$' "$T/log2.txt" && ok "boot 2: the file ends the boot with its crash end line" || bad "boot 2: no crash end line"
grep -aq 'led: stage 5 sdcard' "$T/log2.txt" && grep -aq 'led: done, solid on' "$T/log2.txt" && ok "boot 2: the file has the boot up to the panic" || bad "boot 2: the boot before the panic is missing"
scripts/pi-readlog.sh "$T/log2.bin" --list 2>/dev/null | grep -Eaq '^ +1 .*no end line' && scripts/pi-readlog.sh "$T/log2.bin" --list 2>/dev/null | grep -Eaq '^ +2 .* crash' && ok "boot 2: --list shows boot 1 without an end and boot 2 as a crash" || bad "boot 2: --list: $(scripts/pi-readlog.sh "$T/log2.bin" --list 2>&1 | tail -4)"
scripts/pi-readlog.sh "$T/log2.bin" --boot 1 2>/dev/null | unstamp > "$T/log2.boot1.txt"
python3 -I - "$T/log1.first.txt" "$T/log2.boot1.txt" <<'PY' && ok "boot 2: boot 1's text is still in the file, as it was" || bad "boot 2: boot 1's text changed"
import sys
a = open(sys.argv[1], errors="replace").read().strip("\n").split("\n")
b = open(sys.argv[2], errors="replace").read().strip("\n").split("\n")
assert len(b) >= len(a) - 1, (len(a), len(b))
assert a[:-1] == b[: len(a) - 1], "boot 1 differs"
PY
read OUT IN <<<"$(diffcount "$T/card1.img" "$T/card.img" "$T/card.json")"
[ "$OUT" = 0 ] && ok "boot 2: every sector outside the log file is unchanged" || bad "boot 2: $OUT sectors outside the log file changed"

# ---- boots 3 to 12: cards the writer must leave alone ----
N=3
for defect in loop short long range free nofile nombr tiny v1 foreign; do
  python3 -I scripts/pi_fat32.py "$T/bad-$defect.img" --defect "$defect" --layout "$T/bad-$defect.json" >/dev/null
  cp "$T/bad-$defect.img" "$T/bad-$defect.orig"
  boot $N "$T/bad-$defect.img" "milk.ledcodes=0" "wait@300:led: (stage 7 |done, solid on)"
  sleep 3
  stop
  if cmp -s "$T/bad-$defect.img" "$T/bad-$defect.orig"; then ok "boot $N ($defect): the card is byte for byte unchanged"; else bad "boot $N ($defect): the card CHANGED"; fi
  has '^milklog: not writing (a|the) log file: ' && ok "boot $N ($defect): $(tr -d '\r' < "$LOG" | grep -Ea -m1 '^milklog: not writing' | cut -c1-120)" || bad "boot $N ($defect): no refusal line in the log"
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
