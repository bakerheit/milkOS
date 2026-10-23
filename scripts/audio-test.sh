#!/usr/bin/env bash
# End-to-end sound test, SILENT: qemu's wav audiodev records everything the guest plays
# to a file (never coreaudio). Boots milkOS with virtio-sound, lets init play the boot
# chime, then types into msh over the serial console:
#   play tone 440 0.5 | play tone 1000 0.25 | play tone 660 1.2 0.25 & + play tone 440 0.4
#   (two writers: the mixer) | play system notify (kernel play_system) |
#   play /System/Sounds/error.wav (WAV file) | text show /proc/audio
# and checks the recording with scripts/wav_check.py: every sound present, in order, at the
# right pitch and length, the mix containing both tones, nothing clipped.
#   scripts/audio-test.sh [out.wav]     (default: a mktemp dir, removed on PASS)
# Only the qemu and python processes started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
WORK="$(mktemp -d /tmp/milk-audio-test.XXXXXX)"
WAV="${1:-$WORK/out.wav}"
LOG="$WORK/serial.log"
QPID=""
DPID=""
cleanup() {
  [ -n "$DPID" ] && kill "$DPID" 2>/dev/null || true
  [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "audio-test: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

[ "${MILK_PREBUILT:-}" = 1 ] || cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

SERIAL="$WORK/serial.sock"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=on" -serial chardev:s0 \
  -display none -global virtio-mmio.force-legacy=false \
  -audiodev "wav,id=snd0,path=$WAV,out.frequency=48000" -device virtio-sound-device,audiodev=snd0 \
  2>"$WORK/qemu.err" &
QPID=$!
P='wait:msh:/\$ '
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG" \
  "$P" 'send:pause 1.2' "$P" \
  'send:play tone 440 0.5' "$P" \
  'send:play tone 1000 0.25' "$P" \
  'send:play tone 660 1.2 0.25 &' "$P" 'send:play tone 440 0.4 0.25' "$P" \
  'send:pause 1' "$P" \
  'send:play system notify' "$P" 'send:pause 0.6' "$P" \
  'send:play /System/Sounds/error.wav' "$P" \
  'send:text show /proc/audio' 'wait:device:[^\n]*\n' "$P" \
  "done:$WORK/drive.done" >"$WORK/drive.log" 2>&1 &
DPID=$!
for _ in $(seq 600); do
  [ -e "$WORK/drive.done" ] && break
  kill -0 "$QPID" 2>/dev/null || fail "qemu exited"
  kill -0 "$DPID" 2>/dev/null || { cat "$WORK/drive.log" >&2; fail "console session failed"; }
  sleep 0.1
done
[ -e "$WORK/drive.done" ] || fail "console session timed out"
echo "---- guest ----"
grep -a '^\[virtio-snd\]\|^\[audio\]' "$LOG" | tr -d '\r'
python3 -I scripts/console_clean.py "$LOG" | sed -n '/msh:\/\$ play tone 440 0.5/,$p'
# Graceful stop: qemu writes the WAV header sizes on exit.
kill "$DPID" 2>/dev/null || true
kill "$QPID"
wait "$QPID" 2>/dev/null || true
QPID=""
grep -q "clipped samples 0" "$LOG" || fail "/proc/audio: clipping"

echo "---- recording ----"
python3 -I "$REPO/scripts/wav_check.py" "$WAV" --probe 440,660,784,1046.5
python3 -I - "$WAV" "$REPO/scripts" <<'PY' || fail "recording does not match"
import json, subprocess, sys
wav, scripts = sys.argv[1], sys.argv[2]
res = json.loads(subprocess.check_output(
    [sys.executable, "-I", f"{scripts}/wav_check.py", wav, "--probe", "440,660,784,1046.5", "--json"]))
assert res["rate"] == 48000 and res["channels"] == 2, res
runs = [r for r in res["runs"] if not r.get("silent")]
pos = 0
def near(f, want, tol=0.03):
    return abs(f - want) <= want * tol
def find(desc, pred):
    global pos
    for k in range(pos, len(runs)):
        if pred(runs[k]):
            pos = k + 1
            print(f"  ok  {desc}: {runs[k]['start']:.3f} s, {runs[k]['dur']:.3f} s, ~{runs[k]['freq']} Hz")
            return runs[k]
    sys.exit(f"missing: {desc} (after run {pos})")
find("boot chime, first note G5", lambda r: near(r["freq"], 784) and r["dur"] >= 0.1)
find("boot chime, second note C6", lambda r: near(r["freq"], 1046.5) and r["dur"] >= 0.4)
find("tone 440 Hz 0.5 s", lambda r: near(r["freq"], 440) and 0.44 <= r["dur"] <= 0.56)
find("tone 1000 Hz 0.25 s", lambda r: near(r["freq"], 1000) and 0.2 <= r["dur"] <= 0.3)
m = find("two writers: a 660 Hz run with 440 Hz inside it", lambda r: near(r["freq"], 660) and r["probe"]["440.0"] > -30)
# The 660 Hz tone may be split into several runs where the mix changes (both tones, then
# 660 alone): take the run and the 660 Hz runs right after it as one span.
end = m["start"] + m["dur"]
while pos < len(runs) and near(runs[pos]["freq"], 660) and abs(runs[pos]["start"] - end) < 0.05:
    end = runs[pos]["start"] + runs[pos]["dur"]
    pos += 1
# Inside that span, 100 ms windows: some must hold both tones, and the end 660 alone.
sys.path.insert(0, scripts)
import wav_check
rate, _, mono = wav_check.load(wav)
a, n = int(m["start"] * rate), int(0.1 * rate)
wins = []
for k in range(a, int(end * rate) - n + 1, n):
    seg = mono[k:k + n]
    wins.append((round((k - a) / rate, 1), wav_check.goertzel(seg, rate, 440), wav_check.goertzel(seg, rate, 660)))
both = [w for w in wins if w[1] > -16 and w[2] > -16]
alone = [w for w in wins if w[1] < -40 and w[2] > -16]
print(f"  mix windows (offset s, 440 dB, 660 dB): {[(t, round(x), round(y)) for t, x, y in wins]}")
assert len(both) >= 3, "the two writers were not mixed"
assert alone and alone[-1][0] > both[-1][0], "660 Hz should go on alone after the 440 writer"
print(f"  ok  mixed: {len(both)} x 100 ms with both tones at about -12 dB, then 660 Hz alone")
find("notify (system sound), A5", lambda r: near(r["freq"], 880))
find("notify, E6", lambda r: near(r["freq"], 1318.5))
find("error.wav, G4", lambda r: near(r["freq"], 392))
find("error.wav, Eb4", lambda r: near(r["freq"], 311.1))
print(f"recording: {res['seconds']} s, peak {res['peak_dbfs']} dBFS, everything in order")
PY
cleanup
trap - EXIT
[ -z "${1:-}" ] && [ "${MILK_KEEP:-}" != 1 ] && rm -rf "$WORK" || true
echo "audio-test: PASS"
