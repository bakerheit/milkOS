#!/usr/bin/env bash
# User-space drivers crash test (docs/drivers.md), SILENT (qemu's wav audiodev records):
#   boot 1 (default policy: rng and sound are user drivers): rngd and soundd hold their
#     devices; a 5 s 440 Hz tone plays; 1.5 s in, both drivers are killed with SIGKILL
#     (`svc kill`). Checks:
#       - the kernel never crashes,
#       - milkd restarts each within a second (journal, monotonic ms) and it takes its
#         device back (/proc/drivers: claims=2, releases=1),
#       - the tone plays on to its end: the kernel driver bridges the gap, the longest
#         silence inside it is < 300 ms, and soundd is playing again at the end,
#       - the CSPRNG's source is rngd again;
#   boot 2 (`opt/milkos/drivers` = rng=kernel sound=kernel): no user drivers (their units
#     are skipped), the kernel drivers play and seed.
#   scripts/driver-crash-test.sh [workdir]
# Only the qemus and drivers started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
WORK="${1:-$(mktemp -d /tmp/milk-driver-crash.XXXXXX)}"
mkdir -p "$WORK"
QPID=""
DPID=""
cleanup() {
  [ -n "$DPID" ] && kill "$DPID" 2>/dev/null || true
  [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "driver-crash-test: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }
ok() { echo "  ok  $*"; }

cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"
P='wait:msh:/\$ '

# boot NAME WAV [fw_cfg string] -- steps...
boot() {
  local name="$1" wav="$2" drivers="$3"
  shift 4
  local extra=()
  [ -n "$drivers" ] && extra=(-fw_cfg "name=opt/milkos/drivers,string=$drivers")
  rm -f "$WORK/$name.sock" "$WORK/$name.done"
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
    -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M \
    -kernel "$KERNEL" -chardev "socket,id=s0,path=$WORK/$name.sock,server=on,wait=off" -serial chardev:s0 \
    -display none -global virtio-mmio.force-legacy=false \
    -audiodev "wav,id=snd0,path=$wav,out.frequency=48000" -device virtio-sound-device,audiodev=snd0 \
    ${extra[@]+"${extra[@]}"} 2>"$WORK/$name.qemu.err" &
  QPID=$!
  python3 -I "$REPO/scripts/serial_drive.py" "$WORK/$name.sock" "$WORK/$name.log" \
    "$P" "$@" "done:$WORK/$name.done" >"$WORK/$name.drive" 2>&1 &
  DPID=$!
  for _ in $(seq 900); do
    [ -e "$WORK/$name.done" ] && break
    kill -0 "$QPID" 2>/dev/null || fail "$name: qemu exited"
    kill -0 "$DPID" 2>/dev/null || { cat "$WORK/$name.drive" >&2; tail -30 "$WORK/$name.log" >&2; fail "$name: console session failed"; }
    sleep 0.1
  done
  [ -e "$WORK/$name.done" ] || fail "$name: timed out"
  kill "$DPID" 2>/dev/null || true
  kill "$QPID" # graceful: qemu writes the WAV header on exit
  wait "$QPID" 2>/dev/null || true
  QPID=""
  DPID=""
  python3 -I scripts/console_clean.py "$WORK/$name.log" >"$WORK/$name.txt"
}

echo "---- boot 1: kill rngd and soundd mid-use ----"
boot b1 "$WORK/b1.wav" "" -- \
  'send:pause 2' "$P" \
  'send:text show /proc/drivers' "$P" \
  'send:play tone 440 5 &' "$P" \
  'send:pause 1.5' "$P" \
  'send:svc kill soundd' "$P" \
  'send:svc kill rngd' "$P" \
  'send:pause 1.5' "$P" \
  'send:text show /proc/drivers' "$P" \
  'send:text show /proc/rng' "$P" \
  'send:pause 3.5' "$P" \
  'send:text show /proc/audio' 'wait:device:[^\n]*\n' "$P" \
  'send:text show /dev/journal' "$P"
B1="$WORK/b1.txt"
sed -n '/msh:\/\$ text show \/proc\/drivers/,/msh:\/\$ text show \/dev\/journal/p' "$B1"
grep -Eqi "panic|kernel exception|\[exc\]" "$B1" && fail "the kernel crashed (see $B1)"
ok "the kernel kept running (no crash in the log)"
python3 -I - "$B1" <<'PY' || fail "boot 1 checks"
import re, sys
text = open(sys.argv[1], errors="replace").read()
def must(desc, cond):
    if not cond:
        sys.exit(f"missing: {desc}")
    print(f"  ok  {desc}")
blocks = text.split("msh:/$ text show /proc/drivers")
before, after = blocks[1], blocks[2]
must("before: soundd and rngd hold their devices",
     re.search(r"^sound policy=user .*owner=pid \d+ \(/System/Services/soundd\) claims=1", before, re.M)
     and re.search(r"^rng policy=user .*owner=pid \d+ \(/System/Services/rngd\) claims=1", before, re.M))
must("after: both taken back by restarted drivers (claims=2, releases=1)",
     re.search(r"^sound policy=user .*owner=pid \d+ \(/System/Services/soundd\) claims=2 releases=1", after, re.M)
     and re.search(r"^rng policy=user .*owner=pid \d+ \(/System/Services/rngd\) claims=2 releases=1", after, re.M))
must("the kernel driver covered the gap", "[udrv] sound released by pid" in text and "kernel driver takes over" in text)
must("the CSPRNG is fed by rngd again", re.search(r"source=rngd\+jitter .*user_driver_bytes=(\d+)", after)
     and int(re.search(r"user_driver_bytes=(\d+)", after).group(1)) >= 128)
must("soundd is playing at the end", "device: user-space driver (soundd)" in blocks[-1])
# The journal: seq boot mono_ms wall pid uid unit level message.
j = [l.split("\t") for l in text.split("msh:/$ text show /dev/journal", 1)[1].splitlines()]
j = [f for f in j if len(f) == 9]
for unit, ready in (("soundd", "is ours"), ("rngd", "is ours")):
    ev = [(int(f[2]), f[8]) for f in j if f[6] == unit]
    died = [t for t, m in ev if "killed by SIGKILL" in m]
    must(f"{unit}: milkd saw the SIGKILL", died)
    back = [t for t, m in ev if ready in m and t > died[0]]
    must(f"{unit}: running again after it", back)
    ms = back[0] - died[0]
    must(f"{unit}: restarted and driving its device {ms} ms after the kill (< 1000)", ms < 1000)
PY
python3 -I - "$WORK/b1.wav" "$REPO/scripts" <<'PY' || fail "the tone did not survive the driver crash"
import json, subprocess, sys
wav, scripts = sys.argv[1], sys.argv[2]
res = json.loads(subprocess.check_output([sys.executable, "-I", f"{scripts}/wav_check.py", wav, "--json"]))
runs = res["runs"]
near = lambda r: not r.get("silent") and abs(r["freq"] - 440) <= 440 * 0.03
idx = [k for k, r in enumerate(runs) if near(r)]
assert idx, f"no 440 Hz in the recording: {runs}"
first, last = idx[0], idx[-1]
span = runs[last]["start"] + runs[last]["dur"] - runs[first]["start"]
tone = sum(runs[k]["dur"] for k in idx)
gaps = [runs[k]["dur"] for k in range(first, last + 1) if runs[k].get("silent")]
gap = max(gaps, default=0.0)
print(f"  440 Hz from {runs[first]['start']:.3f} s for {span:.3f} s ({tone:.3f} s of tone), longest gap {gap * 1000:.0f} ms")
assert span >= 4.8, "the tone stopped early"
assert tone >= 4.6, "too much of the tone is missing"
assert gap < 0.3, "the gap is too long"
print("  ok  the tone played to its end across the crash")
PY

echo "---- boot 2: drivers=rng=kernel sound=kernel ----"
boot b2 "$WORK/b2.wav" "rng=kernel sound=kernel" -- \
  'send:pause 1.5' "$P" \
  'send:text show /proc/drivers' "$P" \
  'send:svc status soundd' "$P" \
  'send:svc status rngd' "$P" \
  'send:play tone 660 0.5' "$P" \
  'send:text show /proc/audio' 'wait:device:[^\n]*\n' "$P" \
  'send:text show /proc/rng' "$P"
B2="$WORK/b2.txt"
sed -n '/msh:\/\$ text show \/proc\/drivers/,$p' "$B2"
grep -Eq "^rng policy=kernel .*owner=kernel claims=0" "$B2" || fail "boot 2: rng isn't the kernel's"
grep -Eq "^sound policy=kernel .*owner=kernel claims=0" "$B2" || fail "boot 2: sound isn't the kernel's"
[ "$(grep -c 'state: skipped (/proc/drivers does not contain' "$B2")" -ge 2 ] || fail "boot 2: the driver units should be skipped"
grep -Eq "^device: (running|stopped)" "$B2" || fail "boot 2: the kernel driver isn't playing"
grep -q "source=virtio-rng+jitter" "$B2" || fail "boot 2: the kernel rng driver isn't the source"
ok "boot 2: kernel drivers only (units skipped, /proc/drivers policy=kernel)"
python3 -I "$REPO/scripts/wav_check.py" "$WORK/b2.wav" --json | python3 -I -c "
import json, sys
r = json.load(sys.stdin)['runs']
assert any(not x.get('silent') and abs(x['freq'] - 660) < 20 and x['dur'] >= 0.4 for x in r), r
print('  ok  boot 2: the kernel driver played the 660 Hz tone')" || fail "boot 2: no tone"
echo "driver-crash-test: PASS ($WORK)"
