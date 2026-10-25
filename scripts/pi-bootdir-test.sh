#!/usr/bin/env bash
# scripts/pi-bootdir.sh and its --display option (docs/raspberry-pi.md, "HDMI displays"): the files it writes for an
# ordinary monitor (auto, the default) and for the 4-inch 800x480 HDMI panel (hdmi800x480), what each must and must not
# contain, and the refusals (an unknown --display, a folder on a mounted volume). The kernel is not built: a stand-in
# file goes in as the image (MILK_KERNEL_IMG). Offline, nothing outside a fresh /tmp folder is written.
#
#   scripts/pi-bootdir-test.sh      (MILK_KEEP=1 keeps the work folder)
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/grepq.sh # grep -q that reads all its input (pipefail-safe)
T="$(mktemp -d /tmp/milk-bootdir.XXXXXX)"
trap '[ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T"' EXIT
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
printf 'not a kernel, a stand-in\n' > "$T/img"
export MILK_KERNEL_IMG="$T/img"
# lines: the file's lines that are not comments or empty
active() { grep -Ev '^[[:space:]]*(#|$)' "$1"; }
has() { grep -Eq -- "$2" "$1"; }

run() { # name args... -> OUT=$T/name
  local n="$1"; shift
  scripts/pi-bootdir.sh "$T/$n" "$@" > "$T/$n.log" 2>&1
}

# ---- the default and --display auto: no forced video mode, the SPI panel's switch on, as before ----
for form in "default:" "auto:--display auto" "auto2:--display=auto"; do
  n="${form%%:*}"; args="${form#*:}"
  # shellcheck disable=SC2086
  if run "$n" $args; then ok "$n: pi-bootdir.sh $args succeeded"; else bad "$n: failed: $(tail -3 "$T/$n.log")"; continue; fi
  D="$T/$n"
  for f in milkos8.img config.milkos.txt cmdline.milkos.txt MILKLOG.TXT; do [ -s "$D/$f" ] && ok "$n: $f written" || bad "$n: $f missing"; done
  active "$D/config.milkos.txt" | grep -Fxq 'arm_64bit=1' && active "$D/config.milkos.txt" | grep -Fxq 'kernel=milkos8.img' && ok "$n: arm_64bit and the kernel are set" || bad "$n: arm_64bit/kernel lines missing"
  active "$D/config.milkos.txt" | grep -Eq '^(hdmi_group|hdmi_mode|hdmi_cvt|hdmi_timings|hdmi_drive|hdmi_pixel_freq_limit|config_hdmi_boost|display_hdmi_rotate)=' && bad "$n: a video mode line is active (auto must not force one)" || ok "$n: no video mode forced"
  for l in hdmi_force_hotplug=1 disable_overscan=1 max_framebuffers=1; do active "$D/config.milkos.txt" | grep -Fxq "$l" && ok "$n: $l" || bad "$n: $l missing"; done
  active "$D/config.milkos.txt" | grep -Eq 'dtoverlay=.*vc4|vc4-(f)?kms' && bad "$n: a KMS overlay line is active" || ok "$n: no KMS overlay"
  grep -Fxq 'milk.fbscale=1 milk.lcd=ili9486' "$D/cmdline.milkos.txt" && ok "$n: cmdline keeps the 3.5-inch SPI display's option" || bad "$n: cmdline: $(cat "$D/cmdline.milkos.txt")"
  grep -q 'milk.touch=xpt2046' "$D/cmdline.milkos.txt" && bad "$n: cmdline has the HDMI panel's touch option" || ok "$n: cmdline has no milk.touch=xpt2046"
  [ "$(wc -l < "$D/cmdline.milkos.txt" | tr -d ' ')" = 1 ] && ok "$n: cmdline is one line" || bad "$n: cmdline is not one line"
done
# the default and auto are the same files
cmp -s "$T/default/config.milkos.txt" "$T/auto/config.milkos.txt" && cmp -s "$T/default/cmdline.milkos.txt" "$T/auto/cmdline.milkos.txt" && ok "default == --display auto" || bad "default differs from --display auto"

# ---- hdmi800x480 ----
for form in "hdmi:--display hdmi800x480" "hdmi2:--display=hdmi800x480"; do
  n="${form%%:*}"; args="${form#*:}"
  # shellcheck disable=SC2086
  if run "$n" $args; then ok "$n: pi-bootdir.sh $args succeeded"; else bad "$n: failed: $(tail -3 "$T/$n.log")"; continue; fi
  D="$T/$n"; C="$D/config.milkos.txt"
  for l in 'hdmi_group=2' 'hdmi_mode=16' 'hdmi_drive=2' hdmi_force_hotplug=1 disable_overscan=1 max_framebuffers=1 arm_64bit=1 kernel=milkos8.img; do
    active "$C" | grep -Fxq "$l" && ok "$n: $l" || bad "$n: '$l' is not an active line"
  done
  grep -Eq '^# =+ this display: the 4-inch 800x480 HDMI panel' "$C" && ok "$n: the 'this display' block is marked" || bad "$n: no marked 'this display' block"
  # the block comes after the common lines, and the common lines are before it
  [ "$(grep -n '^kernel=' "$C" | head -1 | cut -d: -f1)" -lt "$(grep -n 'this display:' "$C" | head -1 | cut -d: -f1)" ] && ok "$n: common lines first, the display's block after" || bad "$n: block order"
  # the alternatives are there, commented out, in the order: custom CVT, 720p, 800x600, 640x480
  python3 -I - "$C" <<'PY' && ok "$n: the alternative modes follow in order (CVT 800x480, 720p, 800x600, 640x480), commented out" || bad "$n: alternative modes missing or out of order"
import sys
t = open(sys.argv[1]).read().split("\n")
want = ["#hdmi_cvt=800 480 60 6 0 0 0", "#hdmi_group=1\n#hdmi_mode=4", "#hdmi_group=2\n#hdmi_mode=9", "#hdmi_group=1\n#hdmi_mode=1"]
text = "\n".join(t)
pos = [text.find(w) for w in want]
assert all(p >= 0 for p in pos) and pos == sorted(pos), pos
PY
  for v in '#config_hdmi_boost=' '#hdmi_pixel_freq_limit=' '#hdmi_timings=800 ' '#display_hdmi_rotate=2' '#overscan_left='; do
    grep -Fq -- "$v" "$C" && ok "$n: commented variant $v" || bad "$n: variant $v missing"
  done
  active "$C" | grep -Eq '^(hdmi_timings|config_hdmi_boost|hdmi_pixel_freq_limit|display_hdmi_rotate|overscan_|hdmi_cvt)' && bad "$n: a variant is active" || ok "$n: every variant is commented out"
  [ "$(active "$C" | grep -c '^hdmi_mode=')" = 1 ] && ok "$n: exactly one hdmi_mode line active" || bad "$n: hdmi_mode count"
  active "$C" | grep -Eq 'dtoverlay=.*vc4|vc4-(f)?kms' && bad "$n: a KMS overlay line is active" || ok "$n: no KMS overlay"
  grep -Fxq 'milk.fbscale=1 milk.fbsize=1280x768 milk.touch=xpt2046' "$D/cmdline.milkos.txt" && ok "$n: cmdline: the 5:3 framebuffer and the panel's touch chip" || bad "$n: cmdline: $(cat "$D/cmdline.milkos.txt")"
  grep -Eq 'milk\.lcd=' "$D/cmdline.milkos.txt" && bad "$n: cmdline names an SPI panel (this one is not)" || ok "$n: cmdline has no milk.lcd="
  [ "$(wc -l < "$D/cmdline.milkos.txt" | tr -d ' ')" = 1 ] && ok "$n: cmdline is one line" || bad "$n: cmdline is not one line"
  # no personal names or paths in what a user copies onto a card
  grep -Eiq '/Users/|/home/' "$C" "$D/cmdline.milkos.txt" && bad "$n: a personal path in the files" || ok "$n: no personal paths in the files"
done
cmp -s "$T/hdmi/config.milkos.txt" "$T/hdmi2/config.milkos.txt" && ok "--display X == --display=X" || bad "--display forms differ"
# the common part of the two outputs is the same
head -c "$(wc -c < "$T/auto/config.milkos.txt")" "$T/hdmi/config.milkos.txt" | cmp -s - "$T/auto/config.milkos.txt" && ok "the lines every display gets are the same in both (auto is the start of hdmi800x480's file)" || bad "the common lines differ between auto and hdmi800x480"

# ---- refusals ----
if scripts/pi-bootdir.sh "$T/bad" --display lcd35 > "$T/bad.log" 2>&1; then bad "an unknown --display was accepted"; else
  grep -q 'must be hdmi800x480 or auto' "$T/bad.log" && [ ! -e "$T/bad" ] && ok "an unknown --display is refused, nothing written" || bad "unknown --display: $(cat "$T/bad.log")"
fi
if scripts/pi-bootdir.sh "$T/bad2" --display > "$T/bad2.log" 2>&1; then bad "--display without a value was accepted"; else ok "--display without a value is refused"; fi
if scripts/pi-bootdir.sh "$T/bad3" --nonsense > "$T/bad3.log" 2>&1; then bad "an unknown option was accepted"; else ok "an unknown option is refused"; fi
if scripts/pi-bootdir.sh > "$T/bad4.log" 2>&1; then bad "no OUTDIR was accepted"; else ok "no OUTDIR is refused"; fi
if scripts/pi-bootdir.sh /Volumes/milk-refuse-me --display hdmi800x480 > "$T/bad5.log" 2>&1; then bad "a folder on /Volumes was accepted"; else
  grep -q 'refusing' "$T/bad5.log" && ok "a folder on a mounted volume is refused" || bad "volume refusal: $(cat "$T/bad5.log")"
fi
[ "$FAIL" = 0 ] && echo "pi-bootdir-test: PASS" || { echo "pi-bootdir-test: FAIL"; exit 1; }
