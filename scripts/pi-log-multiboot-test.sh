#!/usr/bin/env bash
# The SD card log across boots (kernel/src/rpi/logfile.rs, docs/raspberry-pi.md, "Reading the log"), end to end on qemu's
# `-M raspi4b`. The card is an image with an MBR and a FAT32 partition holding a pre-created, blank MILKLOG.TXT
# (scripts/pi_fat32.py), booted WITHOUT milk.sdwrite=1.
#
# Part A, an 8 MiB log file, three boots in a row on the same card:
#   boot 1: an ordinary boot; at the shell prompt `logsync` saves the log and says where it stands; then the power is cut
#           (qemu killed): no end line;
#   boot 2: `milk.crashtest=late`, a deliberate crash: the file gets the crash dump, the crash code the LED cycle carries and
#           the end line;
#   boot 3: `milk.crashtest=late-off`, the desktop's Shut Down through the power manager: the file gets "ended: shutdown".
#   Then: all three boots are in the file, in order, each with its banner (version, build, git hash) and the line saying how
#   the one before it ended; boot 1's text is exactly what it was after boot 1, the serial log of each boot is in its
#   section; the system report, the audit lines and the services' journal lines are there; `scripts/pi-readlog.sh --list`
#   says: power cut, crash, shutdown; the header and the sectors say the same end as a scan of the ring; and EVERY OTHER
#   SECTOR OF THE CARD IMAGE IS UNCHANGED, the sectors of the log file after the end of the text included.
# Part B, a 64 KiB log file, three boots: the ring wraps, the oldest text is overwritten, the newest boot is whole, the cursor
#   (the header's, and the one found by scanning the sectors) is consistent, the reader says the ring wrapped.
# Part C, `milk.logstorm=20000`: 20000 lines printed as fast as the console takes them: the rate cap lets a burst through, drops the
#   rest, writes "N lines dropped" and counts every line (in the file plus dropped = printed); the system stays up.
# Offline, private images in a fresh /tmp folder, the qemu started here killed by PID. TCG: about 15 s a boot.
#
#   scripts/pi-log-multiboot-test.sh [workdir]      (MILK_KEEP=1 keeps the work folder; MILK_PREBUILT=1 skips the build)
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="${1:-$(mktemp -d /tmp/milk-pilogmb.XXXXXX)}"
mkdir -p "$T"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
if [ "${MILK_PREBUILT:-}" = 1 ]; then
  ELF="target/pi/aarch64-unknown-none-softfloat/debug/milkos-kernel"
  [ -e "$ELF" ] || { echo "pi-log-multiboot-test: MILK_PREBUILT=1 but $ELF is missing"; exit 1; }
  "$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$T/kernel8.img"
else
  scripts/pi-build.sh "$T/kernel8.img" >/dev/null
fi
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '

# boot N IMAGE CMDLINE STEPS...: start qemu on the card image, drive the serial console with the steps.
boot() {
  local n="$1" img="$2" cmdline="$3"; shift 3
  python3 scripts/pi_dtb.py "$T/b$n.dtb" --cmdline "$cmdline"
  SOCK="$T/s$n.sock"; LOG="$T/serial$n.log"
  : > "$LOG"
  qemu-system-aarch64 -M raspi4b -kernel "$T/kernel8.img" -dtb "$T/b$n.dtb" -display none -monitor none \
    -serial "unix:$SOCK,server=on,wait=on" \
    -drive "file=$img,if=sd,format=raw" >/dev/null 2>&1 &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "$@" "done:$T/done$n" &
  D=$!
  for _ in $(seq 1500); do [ -e "$T/done$n" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
  # A machine that powers itself off ends the loop above before the script has written its marker.
  for _ in 1 2 3 4 5 6 7 8 9 10; do [ -e "$T/done$n" ] && break; sleep 0.3; done
  [ -e "$T/done$n" ] || bad "boot $n: the serial script did not finish (see $LOG)"
}
stop() {
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  { kill "$D"; wait "$D"; } 2>/dev/null || true
}
# Did qemu exit by itself (the machine powered off)?
exited() { for _ in $(seq 100); do kill -0 "$QPID" 2>/dev/null || return 0; sleep 0.2; done; return 1; }
has() { tr -d '\r' < "$LOG" | grep -Eaq -- "$1"; }
check() { if has "$1"; then ok "$2: $(tr -d '\r' < "$LOG" | grep -Ea -m1 -- "$1" | cut -c1-110)"; else bad "$2 (no line matching: $1)"; fi; }
unstamp() { sed -E 's/^\[ *[0-9]+\.[0-9]{3}\] //'; }

# The log file's bytes out of an image, by the layout file.
extract() { python3 -I - "$1" "$2" "$3" <<'PY'
import json, sys
img, layout, out = sys.argv[1:4]
lay = json.load(open(layout))
data = open(img, "rb").read()
buf = b"".join(data[l * 512:(l + n) * 512] for l, n in lay["log_extents"])
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
# The log file's text for boot N, uptime stamps removed.
boot_text() { scripts/pi-readlog.sh "$1" --boot "$2" 2>/dev/null | unstamp; }
# The serial log of one boot is in the file's section of that boot, line for line.
same_lines() { python3 -I - "$1" "$2" <<'PY'
import re, sys
f = open(sys.argv[1], errors="replace").read().split("\n")
s = [l.rstrip("\r") for l in open(sys.argv[2], errors="replace").read().split("\n")]
tagged = re.compile(r"^(\[(report|milklog|tty[0-9]|pty[0-9])\] |\[audit\] [0-9]+ pid |\[journal [^\]]*/[0-9]+\] )")
own = ("milklog:", "[sdhci]")
f = [l for l in f if l and not tagged.match(l) and not l.startswith(("=== milkOS boot", "previous boot", "[milk")) and not any(o in l for o in own)]
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
}

# ================= Part A: 8 MiB, three boots =================
python3 -I scripts/pi_fat32.py "$T/card.img" --log-size 8M --layout "$T/card.json" >/dev/null
cp "$T/card.img" "$T/card0.img"

# ---- boot 1: an ordinary boot, logsync, then the power is cut ----
boot 1 "$T/card.img" "milk.fbscale=1" "wait@300:led: done, solid on" "send:logsync" "wait@120:logsync: (saved|NOT saved)"
check '^logsync: saved$' "boot 1: logsync saved the log"
check '^milklog: writing to the log file on the card$' "boot 1: logsync printed where the log stands"
check '^milklog: boot 1 \(this one\); cursor [0-9]+ bytes of text ever written \(lap 0\); [0-9]+ bytes logged but not yet on the card$' "boot 1: logsync says the boot and the cursor"
sleep 2
extract "$T/card.img" "$T/card.json" "$T/f1.bin"
stop
boot_text "$T/f1.bin" 1 > "$T/b1.first.txt"
[ "$(scripts/pi-readlog.sh "$T/f1.bin" --list 2>/dev/null | grep -Ec '^ +[0-9]+ ')" = 1 ] && ok "boot 1: one boot in the file" || bad "boot 1: --list: $(scripts/pi-readlog.sh "$T/f1.bin" --list 2>&1)"
scripts/pi-readlog.sh "$T/f1.bin" --list 2>/dev/null | grep -Eaq '^ +1 .*no end line' && ok "boot 1: it has no end line (the power is still on)" || bad "boot 1: --list says: $(scripts/pi-readlog.sh "$T/f1.bin" --list 2>&1 | tail -2)"
same_lines "$T/b1.first.txt" "$T/serial1.log" && ok "boot 1: the serial log is in the file, line for line" || bad "boot 1: the file and the serial log disagree"
for pat in '^=== milkOS boot 1 === milkOS [^ ]+ .*\| build [^ ]+ \| git [^ ]+ \| log format 2, file 8388608 bytes \| uptime [0-9]+ ms when the log was opened' '^boot: entered at EL2' '^led: stage 5 sdcard$' '^led: done, solid on$' '^\[report\] ==== system report 1, boot 1, uptime' '^\[report\] -- /proc/meminfo' '^\[report\] -- /proc/interrupts' '^\[report\] -- /proc/mounts' '^\[report\] -- /proc/net/dev' '^\[report\] -- /dev' '^\[report\] -- hardware found at boot' '^\[report\] -- firmware' '^\[report\] temperature: ' '^\[report\] -- SD card controller' '^\[audit\] [0-9]+ pid [0-9]+ .*exec' '^\[journal [a-z]+/[0-9]+\] ' '^\[tty1\] msh' '^logsync$'; do
  if grep -Eaq -- "$pat" "$T/b1.first.txt"; then ok "boot 1: the file has: $(grep -Ea -m1 -- "$pat" "$T/b1.first.txt" | cut -c1-100)"; else bad "boot 1: the file has no line matching $pat"; fi
done

# ---- boot 2: a crash ----
cp "$T/card.img" "$T/card1.img"
boot 2 "$T/card.img" "milk.crashtest=late" "wait@300:KERNEL PANIC"
sleep 6
extract "$T/card.img" "$T/card.json" "$T/f2.bin"
stop
CODE=$(tr -d '\r' < "$LOG" | sed -n -E 's/^led: crash code (stage=[0-9]+ ec=0o[0-7]+ pc=0o[0-7]+ far=0o[0-7]+) .*/\1/p' | head -1)
boot_text "$T/f2.bin" 2 > "$T/b2.txt"
[ -n "$CODE" ] && grep -aFq "led: crash code $CODE" "$T/b2.txt" && ok "boot 2: the file carries the crash code the LED cycle shows ($CODE)" || bad "boot 2: crash code '$CODE' is not in the file"
grep -aq 'crash-test: a deliberate panic, late' "$T/b2.txt" && grep -aq '^\*\*\* KERNEL PANIC \*\*\*$' "$T/b2.txt" && ok "boot 2: the file has the panic" || bad "boot 2: no panic in the file"
grep -aq '^=== milkOS boot 2 ended: crash ===$' "$T/b2.txt" && ok "boot 2: the file has the crash end line" || bad "boot 2: no crash end line"
sed -n 2p "$T/b2.txt" | grep -Eaq '^previous boot 1: no end recorded' && ok "boot 2: its banner says boot 1 was cut off" || bad "boot 2: banner: $(sed -n 2p "$T/b2.txt")"
same_lines "$T/b2.txt" "$T/serial2.log" && ok "boot 2: the serial log is in the file, line for line" || bad "boot 2: the file and the serial log disagree"
read OUT IN <<<"$(diffcount "$T/card1.img" "$T/card.img" "$T/card.json")"
[ "$OUT" = 0 ] && ok "boot 2: every sector outside the log file is unchanged" || bad "boot 2: $OUT sectors outside the log file changed"

# ---- boot 3: a clean shutdown through the power manager ----
cp "$T/card.img" "$T/card2.img"
boot 3 "$T/card.img" "milk.crashtest=late-off" "wait@300:wm: shutting down"
exited && ok "boot 3: qemu exited by itself (the machine powered off)" || bad "boot 3: the machine did not power off"
sleep 1
extract "$T/card.img" "$T/card.json" "$T/f3.bin"
stop
boot_text "$T/f3.bin" 3 > "$T/b3.txt"
grep -aq '^wm: shutting down (power manager, halt)$' "$T/b3.txt" && ok "boot 3: the file has the shutdown line" || bad "boot 3: no shutdown line"
tail -1 "$T/b3.txt" | grep -aq '^=== milkOS boot 3 ended: shutdown ===$' && ok "boot 3: the last line of the boot is the shutdown end line" || bad "boot 3: last line: $(tail -1 "$T/b3.txt")"
sed -n 2p "$T/b3.txt" | grep -Eaq '^previous boot 2: crash$' && ok "boot 3: its banner says boot 2 crashed" || bad "boot 3: banner: $(sed -n 2p "$T/b3.txt")"
same_lines "$T/b3.txt" "$T/serial3.log" && ok "boot 3: the serial log is in the file, line for line" || bad "boot 3: the file and the serial log disagree"

# ---- all three boots together ----
scripts/pi-readlog.sh "$T/f3.bin" --list > "$T/list.txt" 2> "$T/list.err" || true
[ "$(grep -Ec '^ +[0-9]+ ' "$T/list.txt")" = 3 ] && ok "all: three boots in the file" || bad "all: --list: $(cat "$T/list.txt")"
grep -Eaq '^ +1 .*no end line' "$T/list.txt" && grep -Eaq '^ +2 .* crash' "$T/list.txt" && grep -Eaq '^ +3 .* shutdown' "$T/list.txt" && ok "all: --list says: power cut, crash, shutdown" || bad "all: --list: $(cat "$T/list.txt")"
python3 -I - "$T/list.txt" <<'PY' && ok "all: the boots are in order and have uptimes" || bad "all: list order"
import re, sys
rows = [l.split() for l in open(sys.argv[1]) if re.match(r"^ +[0-9]+ ", l)]
assert [int(r[0]) for r in rows] == [1, 2, 3], rows
starts = [int(r[1]) for r in rows]
assert starts == sorted(starts) and len(set(starts)) == 3, starts
PY
scripts/pi-readlog.sh "$T/f3.bin" 2> "$T/all.err" | unstamp > "$T/all.txt"
python3 -I - "$T/all.txt" <<'PY' && ok "all: the banners of boots 1, 2 and 3 are in the file in that order" || bad "all: banners"
import re, sys
t = open(sys.argv[1], errors="replace").read()
pos = [t.index(f"=== milkOS boot {n} === ") for n in (1, 2, 3)]
assert pos == sorted(pos), pos
PY
# Boot 1 is what it was after boot 1: the later boots did not touch its text.
boot_text "$T/f3.bin" 1 > "$T/b1.late.txt"
python3 -I - "$T/b1.first.txt" "$T/b1.late.txt" <<'PY' && ok "all: boot 1's text is exactly what it was after boot 1" || bad "all: boot 1's text changed"
import sys
def lines(path):
    ls = open(path, errors="replace").read().rstrip("\n").split("\n")
    # The shell prompt waiting for input is written as a line of its own a moment after the last output: it may or may not
    # have made it into the first copy, which was taken a second before the power was cut.
    while ls and ls[-1].startswith("msh:") and ls[-1].rstrip().endswith("$"):
        ls.pop()
    return ls
a, b = lines(sys.argv[1]), lines(sys.argv[2])
assert a == b, (len(a), len(b))
PY
read OUT IN <<<"$(diffcount "$T/card0.img" "$T/card.img" "$T/card.json")"
[ "$OUT" = 0 ] && ok "all: every sector outside the log file is unchanged by the three boots" || bad "all: $OUT sectors outside the log file changed"
# The cursor: the header, and the sectors, say the same end; the sectors after the end are still the blank file's.
python3 -I - "$T/f3.bin" "$T/card0.img" "$T/card.json" "$PWD/scripts/pi_readlog.py" <<'PY' && ok "all: the header's cursor and a scan of the ring find the same end; the file after it is untouched" || bad "all: cursor"
import importlib.util, json, sys
f, orig, layout, reader = sys.argv[1:5]
spec = importlib.util.spec_from_file_location("pi_readlog", reader)
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
data = open(f, "rb").read()
log = m.Log(data)
end1, how1, hdr = log.recover()
assert how1 == "from the header", how1
d2 = bytearray(data); d2[:1024] = b"\0" * 1024
end2, how2, _ = m.Log(bytes(d2)).recover()
assert end1 == end2, (end1, end2)
# The header's index: three boots, the last two ended as crash and shutdown.
assert [(b[0], b[1]) for b in hdr["boots"]] == [(1, 0), (2, 3), (3, 1)], hdr["boots"]
lay = json.load(open(layout))
card0 = open(orig, "rb").read()
blank = b"".join(card0[l * 512:(l + n) * 512] for l, n in lay["log_extents"])[: lay["log_size"]]
used = 2 + -(-end1 // 500)
assert data[used * 512:] == blank[used * 512:], "the file changed after the end of the text"
assert end1 > 20000, end1
PY

# ================= Part B: 64 KiB, the ring wraps =================
python3 -I scripts/pi_fat32.py "$T/small.img" --log-size 64K --layout "$T/small.json" >/dev/null
cp "$T/small.img" "$T/small0.img"
for n in 4 5 6; do
  boot $n "$T/small.img" "milk.ledcodes=0" "wait@300:led: done, solid on"
  sleep 4
  stop
done
extract "$T/small.img" "$T/small.json" "$T/w.bin"
set +e; scripts/pi-readlog.sh "$T/w.bin" 2> "$T/w.err" | unstamp > "$T/w.txt"; RC=${PIPESTATUS[0]}; set -e
[ "$RC" = 1 ] && grep -aq 'the ring has wrapped' "$T/w.err" && ok "wrap: the reader says the ring wrapped: $(grep -a 'wrapped' "$T/w.err" | cut -c1-120)" || bad "wrap: reader exit $RC: $(cat "$T/w.err")"
grep -aq '^=== milkOS boot 3 === ' "$T/w.txt" && ok "wrap: the newest boot's banner is in the file" || bad "wrap: no boot 3 banner"
grep -aq '^=== milkOS boot 1 === ' "$T/w.txt" && bad "wrap: boot 1's banner survived (the oldest text should be gone)" || ok "wrap: boot 1's banner was overwritten, the oldest text is gone"
grep -aq '^\[milklog\] the ring is full: from here the oldest text' "$T/w.txt" && ok "wrap: the log says where the ring went round" || bad "wrap: no ring-full marker line"
scripts/pi-readlog.sh "$T/w.bin" --last 2>/dev/null | unstamp > "$T/w.last.txt" || true
head -1 "$T/w.last.txt" | grep -Eaq '^=== milkOS boot 3 === ' && grep -aq '^\[report\] ==== end of system report 1 ====$' "$T/w.last.txt" && ok "wrap: the newest boot is whole, from its banner to its system report" || bad "wrap: the newest boot is cut: $(head -1 "$T/w.last.txt" | cut -c1-60)"
python3 -I - "$T/w.bin" "$T/small0.img" "$T/small.json" "$PWD/scripts/pi_readlog.py" <<'PY' && ok "wrap: every ring sector is real, laps are consistent, header and scan agree on the end" || bad "wrap: ring consistency"
import importlib.util, json, sys
f, orig, layout, reader = sys.argv[1:5]
spec = importlib.util.spec_from_file_location("pi_readlog", reader)
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
data = open(f, "rb").read()
log = m.Log(data)
end1, how1, hdr = log.recover()
assert how1 == "from the header", how1
d2 = bytearray(data); d2[:1024] = b"\0" * 1024
end2, how2, _ = m.Log(bytes(d2)).recover()
assert end1 == end2, (end1, end2)
ring = log.ring
end_abs = -(-end1 // 500)
assert end_abs > ring, "it should have wrapped"
bad = 0
for a in range(end_abs - ring, end_abs):
    info = m.open_sector(log.slot(a))
    if not info or info[0] != a:
        bad += 1
assert bad == 0, f"{bad} ring sectors are not the ones the cursor says"
assert [b[0] for b in hdr["boots"]] == [1, 2, 3], hdr["boots"]
PY
read OUT IN <<<"$(diffcount "$T/small0.img" "$T/small.img" "$T/small.json")"
[ "$OUT" = 0 ] && ok "wrap: every sector outside the log file is unchanged" || bad "wrap: $OUT sectors outside the log file changed"

# ================= Part C: a log storm hits the rate cap =================
python3 -I scripts/pi_fat32.py "$T/storm.img" --log-size 8M --layout "$T/storm.json" >/dev/null
cp "$T/storm.img" "$T/storm0.img"
boot 7 "$T/storm.img" "milk.ledcodes=0 milk.logstorm=20000" "wait@300:milklog: storm: done, 20000 lines printed" "send:logsync" "wait@120:logsync: (saved|NOT saved)"
sleep 3
extract "$T/storm.img" "$T/storm.json" "$T/s.bin"
stop
check '^logsync: saved$' "storm: the system is alive after 20000 lines and logsync saved the log"
scripts/pi-readlog.sh "$T/s.bin" 2> "$T/s.err" | unstamp > "$T/s.txt" || true
python3 -I - "$T/s.txt" "$LOG" <<'PY' && ok "storm: the cap let a burst through, dropped the rest, and counted every line it dropped" || bad "storm: accounting"
import re, sys
f = open(sys.argv[1], errors="replace").read().split("\n")
serial = open(sys.argv[2], errors="replace").read()
stormlines = sum(1 for l in f if l.startswith("storm line "))
capped = sum(int(m.group(1)) for l in f for m in [re.match(r"^\[milklog: ([0-9]+) lines dropped by the rate cap \(25 lines/s, 4096 bytes/s on average\)\]$", l)] if m)
full = sum(int(m.group(1)) for l in f for m in [re.match(r"^\[milklog: ([0-9]+) lines lost: the ring waiting for the card was full\]$", l)] if m)
print("in the file:", stormlines, "storm lines; dropped by the cap:", capped, "; lost, ring full:", full)
assert 1500 <= stormlines <= 5000, stormlines          # the burst, plus what the sustained rate allowed meanwhile (25 a second, longer on a loaded machine)
assert capped > 15000, capped
total = stormlines + capped + full
assert 20000 <= total <= 20040, total                  # every storm line is in the file or counted (a few other lines may be counted too)
m = re.search(r"milklog: ([0-9]+) lines taken, ([0-9]+) dropped by the rate cap", serial)
assert m and int(m.group(2)) > 15000, m and m.groups()
PY
grep -aq 'lines dropped by the rate cap (25 lines/s, 4096 bytes/s on average)' "$T/s.txt" && ok "storm: the file says how many lines were dropped" || bad "storm: no dropped-lines marker"
grep -aq '^=== milkOS boot 1 === ' "$T/s.txt" && grep -aq '^\[report\] ==== end of system report 1 ====$' "$T/s.txt" && ok "storm: the boot before the storm, its system report included, is whole" || bad "storm: the start of the boot is missing"
read OUT IN <<<"$(diffcount "$T/storm0.img" "$T/storm.img" "$T/storm.json")"
[ "$OUT" = 0 ] && ok "storm: every sector outside the log file is unchanged" || bad "storm: $OUT sectors outside the log file changed"

if [ "$FAIL" = 0 ]; then
  echo "pi-log-multiboot-test: all passed"
  [ -z "${MILK_KEEP:-}" ] && [ "${T#/tmp/milk-pilogmb.}" != "$T" ] && rm -rf "$T"
  exit 0
fi
echo "pi-log-multiboot-test: FAILED (work folder kept: $T)"
exit 1
