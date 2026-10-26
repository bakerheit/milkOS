#!/usr/bin/env bash
# Try one boot image on the Pixel 2 and say what happened (docs/pixel2.md, "Running the experiment"):
#
#   scripts/android-try.sh [options] IMAGE
#
#   --wait SECS    how long to wait after `fastboot boot` before asking the bootloader again (default 20)
#   --no-reboot    the phone is already in a FRESH bootloader session (powered on with power + volume down,
#                  or rebooted into the bootloader a moment ago): do not reboot it first
#   --serial S     the phone's serial number, when more than one device is connected
#   --yes          do not ask before booting
#   ADB=... FASTBOOT=...   (environment) the tools to run, default `adb` and `fastboot`
#   ANDROID_TRY_SETTLE, ANDROID_TRY_POLLS, ANDROID_TRY_TICK, ANDROID_TRY_ASK   (environment, for the script's own
#                  tests) seconds to wait after the reboot (3), how many polls for the phone to show (20), seconds
#                  between polls (1), seconds to wait for an answer from the bootloader (6)
#
# What it does, in this order:
#   1. looks at IMAGE (an Android boot image: scripts/android-bootimg.py --unpack) and refuses a file that is not one;
#   2. gets a FRESH bootloader session: `fastboot reboot bootloader` if the phone is in fastboot already, else
#      `adb reboot bootloader` if Android is running. Why: on this phone `fastboot boot` works only for the FIRST
#      boot of a bootloader session; a second one in the same session fails with "Failed to load/authenticate boot
#      image: Load Error" (seen on the real phone). Every try therefore starts with a reboot into the bootloader;
#   3. waits (up to 20 s) for fastboot to list the phone, then runs `fastboot boot IMAGE` (loads it into RAM and
#      jumps: nothing is written to the phone);
#   4. waits --wait seconds and asks the bootloader again (`fastboot devices`, `fastboot getvar product`):
#        it still answers   -> the image was accepted ("Booting OKAY") but NOT started: the bootloader is still
#                              there. (This is what the plain, uncompressed image did.) Exit status 3.
#        it does not answer -> the phone jumped: the USB device is gone because the bootloader is no longer
#                              running. Look at the screen: that is milkOS (or its crash screen). Exit status 0.
#
# Exit status: 0 jumped, 2 refused or a tool problem, 3 loaded but not started, 4 no phone found.
# Nothing here flashes, erases or unlocks anything. To get back to Android: hold the power button 10 to 15 seconds.
set -uo pipefail
ADB="${ADB:-adb}"; FASTBOOT="${FASTBOOT:-fastboot}"
SETTLE="${ANDROID_TRY_SETTLE:-3}"; POLLS="${ANDROID_TRY_POLLS:-20}"; TICK="${ANDROID_TRY_TICK:-1}"; ASK="${ANDROID_TRY_ASK:-6}"
WAIT=20; REBOOT=1; SERIAL=""; YES=0; IMG=""
usage() { sed -n '2,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; }
while [ $# -gt 0 ]; do
  case "$1" in
    --wait) WAIT="${2:?--wait needs seconds}"; shift ;;
    --no-reboot) REBOOT=0 ;;
    --serial) SERIAL="${2:?--serial needs a serial number}"; shift ;;
    --yes) YES=1 ;;
    -h|--help) usage; exit 0 ;;
    -*) echo "android-try: unknown option $1" >&2; exit 2 ;;
    *) [ -z "$IMG" ] || { echo "android-try: one IMAGE only" >&2; exit 2; }; IMG="$1" ;;
  esac
  shift
done
[ -n "$IMG" ] || { echo "usage: scripts/android-try.sh [--wait SECS] [--no-reboot] [--serial S] [--yes] IMAGE" >&2; exit 2; }
case "$WAIT" in ''|*[!0-9]*) echo "android-try: --wait wants whole seconds" >&2; exit 2 ;; esac
[ -f "$IMG" ] || { echo "android-try: $IMG is not a file" >&2; exit 2; }
HERE="$(cd "$(dirname "$0")" && pwd)"
if ! python3 -I "$HERE/android-bootimg.py" --unpack "$IMG" >/dev/null 2>&1; then
  echo "android-try: $IMG is not an Android boot image" >&2; exit 2
fi
TMP="$(mktemp -d "${TMPDIR:-/tmp}/android-try.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
FB=("$FASTBOOT"); AD=("$ADB")
[ -z "$SERIAL" ] || { FB+=(-s "$SERIAL"); AD+=(-s "$SERIAL"); }

# bounded SECS CMD...: run CMD, output in $TMP/out, killed (by its PID: it is ours) after SECS seconds. 124 on a timeout.
bounded() {
  local secs="$1" pid i; shift
  "$@" >"$TMP/out" 2>&1 </dev/null & pid=$!
  for ((i = 0; i < secs * 10; i++)); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
  if kill -0 "$pid" 2>/dev/null; then kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; return 124; fi
  wait "$pid"
}
# Is a phone listed by fastboot right now? (`fastboot devices` returns at once, with nothing when there is none.)
listed() { bounded 8 "${FB[@]}" devices && grep -q 'fastboot' "$TMP/out"; }
# Does the bootloader answer a query? (`fastboot getvar` waits for ever for a device that is not there.)
answers() { bounded "$ASK" "${FB[@]}" getvar product && grep -q 'product' "$TMP/out"; }

echo "android-try: image $IMG ($(wc -c < "$IMG" | tr -d ' ') bytes)"
if [ "$YES" != 1 ] && [ -t 0 ]; then
  read -r -p "android-try: boot it on the phone now (nothing is flashed or erased)? [y/N] " a
  case "$a" in y|Y|yes) ;; *) echo "android-try: not booted"; exit 2 ;; esac
fi

if [ "$REBOOT" = 1 ]; then
  if listed; then
    echo "android-try: the phone is in fastboot: rebooting into a fresh bootloader session"
    bounded 20 "${FB[@]}" reboot bootloader || { echo "android-try: fastboot reboot bootloader failed:"; sed 's/^/  /' "$TMP/out"; exit 2; }
  elif bounded 10 "${AD[@]}" get-state && grep -q '^device' "$TMP/out"; then
    echo "android-try: Android is running: adb reboot bootloader"
    bounded 20 "${AD[@]}" reboot bootloader || { echo "android-try: adb reboot bootloader failed:"; sed 's/^/  /' "$TMP/out"; exit 2; }
  else
    echo "android-try: no phone found through fastboot or adb." >&2
    echo "  Connect it, unlock the screen and allow USB debugging, or power it off and hold power + volume down" >&2
    echo "  until the bootloader screen shows, then run this again with --no-reboot." >&2
    exit 4
  fi
  sleep "$SETTLE"
else
  echo "android-try: --no-reboot: assuming this is a fresh bootloader session"
fi

n=0
until listed; do
  n=$((n + 1))
  if [ "$n" -ge "$POLLS" ]; then
    echo "android-try: fastboot does not list the phone after $POLLS polls." >&2
    echo "  Is the cable in, is the phone showing the bootloader screen (hold power + volume down)?" >&2
    exit 4
  fi
  sleep "$TICK"
done
echo "android-try: fastboot lists the phone: $(tr -s '\t ' ' ' < "$TMP/out" | head -1)"
if bounded 8 "${FB[@]}" getvar unlocked && ! grep -q 'unlocked: *yes' "$TMP/out"; then
  echo "android-try: the bootloader says it is NOT unlocked ($(grep -i unlocked "$TMP/out" | head -1)): fastboot boot will be refused" >&2
fi

echo "android-try: fastboot boot $IMG"
bounded 120 "${FB[@]}" boot "$IMG"; rc=$?
sed 's/^/  fastboot: /' "$TMP/out"
if [ "$rc" != 0 ] || grep -qi 'FAILED\|error' "$TMP/out"; then
  echo "android-try: RESULT: the bootloader REFUSED the image (fastboot exit $rc)."
  if grep -qi 'Load Error' "$TMP/out"; then
    echo "  \"Load Error\" is what a second boot in one bootloader session gives. This script reboots into a fresh"
    echo "  session first, so look at what happened in between (a cable, a phone that was not in the bootloader)."
  fi
  exit 2
fi

echo "android-try: loaded; waiting $WAIT s, then asking the bootloader again"
sleep "$WAIT"
if answers || listed; then
  echo "android-try: RESULT: the bootloader STILL ANSWERS ($(tr -s '\n\t ' ' ' < "$TMP/out" | cut -c1-80))."
  echo "  The image was accepted but not started: the screen is the bootloader's, not milkOS's."
  echo "  (The plain uncompressed image does this; the gzip images are the ones to use.)"
  exit 3
fi
echo "android-try: RESULT: the phone no longer answers fastboot and has left the USB bus: the image JUMPED."
echo "  What is on the screen is milkOS (or its crash screen). Photograph it."
echo "  To get back to Android: hold the power button for 10 to 15 seconds."
exit 0
