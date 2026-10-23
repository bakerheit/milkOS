#!/usr/bin/env bash
# Test the release bundle the way someone else would get it: build it (release.sh),
# unzip dist/milkOS-<V>.zip into a fresh /tmp directory outside the repo, and run its
# milkOS.command --offline (tests never use the internet) with a PATH that has no cargo/rustup:
#   1. --headless: boots to msh; the bundled milkfs disk is mounted and is /Users/milk
#      (not new: made by release.sh); /Apps is there; version is right; the share
#      is readable but not writable; a note saved to ~ is written out
#   2. --headless again: the note is still there (persistent)
#   3. windowed (cocoa), once, briefly: a screendump through the monitor socket, then
#      that qemu is killed by its exact PID
# Only qemus started here are killed (by PID).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$(pwd)"
V="$(tr -d '[:space:]' < VERSION)"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
scripts/release.sh >/dev/null
T="$(mktemp -d /tmp/milk-release.XXXXXX)"
ditto -x -k "dist/milkOS-$V.zip" "$T"
B="$T/milkOS-$V"
case "$B" in "$REPO"*) echo "bundle copy is inside the repo" >&2; exit 1 ;; esac
[ -x "$B/milkOS.command" ] && ok "unzipped to $B, milkOS.command is executable" || bad "milkOS.command mode"
grep -rIl -e "$REPO" -e rustup -e cargo "$B" >/dev/null 2>&1 && bad "bundle mentions the build machine" || ok "nothing in the bundle refers to the repo, cargo or rustup"
# check-all's internet guard (a qemu wrapper) stays first, if there is one.
CLEANPATH="${MILK_NET_GUARD_DIR:+$MILK_NET_GUARD_DIR:}/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin"
# Started from / with a clean environment; every step execs (env, bash -c, the
# .command), so $! is qemu itself. (Not a shell function: backgrounding one forks.)
GO='cd / && exec "$0" "$@"'
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT

headless() { # name commands...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  env -i HOME="$HOME" PATH="$CLEANPATH" /bin/bash -c "$GO" "$B/milkOS.command" \
    --offline --headless --serial "unix:$SOCK,server,nowait" >"$T/$name.out" 2>&1 &
  QPID=$!
  local P='msh:[^\n]*\$ '
  local args=("wait:$P")
  for c in "$@"; do args+=("send:$c" "wait:$P"); done
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "${args[@]}" "send:print REL""-DONE" "wait:REL-DONE" "done:$T/$name.done" &
  local DPID=$!
  for _ in $(seq 900); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1.5
  ps -p "$QPID" -o comm= | grep -q qemu || echo "  (note: PID $QPID is not qemu)"
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
}
headless h1 "system info" "text show /proc/version" "files list /Users/milk" "files list /Apps" "text show /Volumes/Mac/README.txt" \
  "print nope > /Volumes/Mac/x.txt" "print saved by the release test > /Users/milk/note.txt" "print now > /System/Kernel/fs/milkfs/sync"
L="$(tr -d '\r' < "$T/h1.log")"
[ -e "$T/h1.done" ] && ok "headless: boots to msh" || { bad "headless boot"; tail -5 "$T/h1.out"; }
echo "$L" | grep -aq "milkfs: vd[a-z] mounted at /Volumes/Milk " && ok "the bundled Milk disk is mounted at /Volumes/Milk" || bad "Milk disk not mounted"
echo "$L" | grep -aq "milkfs: /Users/milk is /Volumes/Milk/Users/milk (persistent)" && ok "/Users/milk is on it (the skeleton made by release.sh, not new)" || bad "home not on the Milk disk"
echo "$L" | grep -a -A2 "files list /Users/milk$" | grep -aq "README.txt" && ok "home skeleton present" || bad "home skeleton"
echo "$L" | grep -aq "files list /Apps$" && ! echo "$L" | grep -a -A1 "files list /Apps$" | grep -aq "doesn't exist" && ok "/Apps present" || bad "/Apps"
echo "$L" | grep -Eaq "^system +milkOS $V$" && echo "$L" | grep -aq "^milkOS $V (aarch64" && ok "version $V in system info and /proc/version" || bad "version strings"
echo "$L" | grep -aq "Files you put in this folder show up inside milkOS" && ok "share readable" || bad "share not readable"
echo "$L" | grep -a -A1 "print nope > /Volumes/Mac/x.txt" | grep -aqi "read-only\|EROFS\|denied\|EACCES" && [ ! -e "$B/share/x.txt" ] && ok "share is read-only" || bad "share writable?"
headless h2 "text show /Users/milk/note.txt"
L="$(tr -d '\r' < "$T/h2.log")"
echo "$L" | grep -aq "^saved by the release test" && ok "second boot: the note saved in ~ is still there" || bad "note not persistent"
python3 scripts/mkmilkfs.py check "$B/disks/milk.img" --quiet >/dev/null && ok "the bundle's milk.img checks clean on the Mac" || bad "milk.img check"

# Windowed, once: wait for the shell, let the desktop draw, grab the screen, kill by PID.
MON="$T/mon.sock"
env -i HOME="$HOME" PATH="$CLEANPATH" /bin/bash -c "$GO" "$B/milkOS.command" \
  --offline --serial "file:$T/w.log" --monitor "unix:$MON,server,nowait" >"$T/w.out" 2>&1 &
QEMU_PID=$!
QPID=$QEMU_PID
for _ in $(seq 600); do grep -aq "msh:" "$T/w.log" 2>/dev/null && break; sleep 0.1; done
sleep 5
python3 - "$MON" "$T/screen.png" <<'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.settimeout(1)
def drain():
    d = b""
    try:
        while True:
            c = s.recv(4096)
            if not c: break
            d += c
    except socket.timeout:
        pass
    return d
drain(); s.sendall(f"screendump {sys.argv[2]} -f png\n".encode()); time.sleep(1); drain()
PY
if ps -p "$QEMU_PID" -o comm= | grep -q qemu; then
  kill "$QEMU_PID"; ok "windowed qemu (PID $QEMU_PID, exactly the one started) killed"
else
  bad "PID $QEMU_PID isn't qemu"
fi
wait "$QEMU_PID" 2>/dev/null || true; QPID=""
grep -aq "msh:" "$T/w.log" && ok "windowed: boots to msh" || bad "windowed boot"
if [ -s "$T/screen.png" ]; then
  dims="$(sips -g pixelWidth -g pixelHeight "$T/screen.png" 2>/dev/null | awk '/pixel/{print $2}' | paste -sd x -)"
  ok "windowed: screendump $T/screen.png ($dims)"
else bad "no screendump"; fi
rm -f "$B/disks/"*.img
echo
[ $FAIL = 0 ] && echo "releasetest: PASS ($T)" || echo "releasetest: FAIL ($T)"
exit $FAIL
