#!/usr/bin/env bash
# End-to-end test of the `milk` package manager, on PRIVATE disk images (fresh milkfs in a
# mktemp dir; the shared /tmp/milk-*.img are never touched).
#   1. build packages/ into a repo dir and serve it with python3 http.server on 127.0.0.1
#      (a free port; the guest reaches it as 10.0.2.2:PORT)
#   2. boot, and in msh: milk update / search / info, install fortune, run it, list, remove;
#      try snake in a clone, run it from the clone, check the live /Applications and /bin
#      never saw it, discard; try moo2048 and keep it (promote), run its self-test;
#      audit trail (user.pkg.* + milkfs.*) and `audit verify`
#   3. after shutdown, look inside the image with mkmilkfs.py: moo2048 is on disk, fortune
#      and snake are not, no clone is left.
# Only the qemu, python and helper processes started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
WORK="$(mktemp -d /tmp/milk-pkg-test.XXXXXX)"
LOG="$WORK/serial.log"
QPID=""
DPID=""
HPID=""
cleanup() {
  [ -n "$DPID" ] && kill "$DPID" 2>/dev/null || true
  [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true
  [ -n "$HPID" ] && kill "$HPID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "pkg-test: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

# Repo + server (127.0.0.1 only, free port).
scripts/milk-repo.sh build "$WORK/repo" >"$WORK/repo.log"
PORT=$(python3 -I -c "import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1])")
(cd "$WORK" && exec python3 -I -m http.server --bind 127.0.0.1 --directory "$WORK/repo" "$PORT" >"$WORK/http.log" 2>&1) &
HPID=$!
for _ in $(seq 50); do curl -sf "http://127.0.0.1:$PORT/index.txt" >/dev/null 2>&1 && break; sleep 0.1; done
echo "repo: $(grep -vc '^#' "$WORK/repo/index.txt") packages at http://127.0.0.1:$PORT/ (pid $HPID)"

# Private disks: a fresh milkfs volume and a scratch disk.
python3 -I scripts/mkmilkfs.py mkfs "$WORK/fs.img" --size 64M --label Milk >/dev/null
dd if=/dev/zero of="$WORK/disk.img" bs=1m count=0 seek=64 2>/dev/null
printf 'milkOS scratch disk (created by scripts/disk.sh)\n' | dd of="$WORK/disk.img" conv=notrunc 2>/dev/null
# shellcheck disable=SC1091
source scripts/disk.sh
MILK_FS="$WORK/fs.img" MILK_DISK="$WORK/disk.img" milk_disk_args

SERIAL="$WORK/serial.sock"
qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none -global virtio-mmio.force-legacy=false \
  "${DISK_ARGS[@]}" \
  -netdev user,id=n0 -device virtio-net-device,netdev=n0 2>"$WORK/qemu.err" &
QPID=$!
P='wait:msh:[^\n]*\$ '
CL=/Volumes/Milk-try-snake
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG" \
  "$P" "send:echo http://10.0.2.2:$PORT/ > /etc/milk/repos" "$P" \
  'send:milk update' 'wait:index updated' "$P" \
  'send:milk search' "$P" \
  'send:milk info fortune' "$P" \
  'send:milk install fortune' 'wait:milk: (installed|[^\n]*error|[^\n]*mismatch)' "$P" \
  'send:fortune -n 1' "$P" \
  'send:fortune' "$P" \
  'send:milk list' "$P" \
  'send:milk remove fortune' "$P" \
  'send:ls /Applications' "$P" \
  'send:milk try snake' 'wait:milk: (then|[^\n]*error)' "$P" \
  "send:$CL/Applications/snake/bin/snake --demo 400" 'wait:snake: demo over' "$P" \
  'send:ls /Applications' "$P" \
  'send:ls /bin/snake' "$P" \
  "send:ls $CL/Applications" "$P" \
  'send:cat /sys/fs/milkfs/clones' "$P" \
  'send:milk discard snake' 'wait:discarded' "$P" \
  'send:cat /sys/fs/milkfs/clones' "$P" \
  'send:ls /Volumes' "$P" \
  'send:milk try moo2048' 'wait:milk: (then|[^\n]*error)' "$P" \
  'send:milk keep moo2048' 'wait:milk: (kept|[^\n]*error)' "$P" \
  'send:moo2048 --selftest' 'wait:moo2048: selftest' "$P" \
  'send:milk list' "$P" \
  'send:audit --event user.pkg' "$P" \
  'send:audit --event milkfs' "$P" \
  'send:audit verify' "$P" \
  'send:echo now > /sys/fs/milkfs/sync' "$P" \
  "done:$WORK/drive.done" >"$WORK/drive.log" 2>&1 &
DPID=$!
for _ in $(seq 1200); do
  [ -e "$WORK/drive.done" ] && break
  kill -0 "$QPID" 2>/dev/null || fail "qemu exited"
  kill -0 "$DPID" 2>/dev/null || { cat "$WORK/drive.log" >&2; tail -30 "$LOG" >&2; fail "console session failed"; }
  sleep 0.1
done
[ -e "$WORK/drive.done" ] || { tail -30 "$LOG" >&2; fail "console session timed out"; }
sleep 2 # milkfs commits within a second; the audit thread flushes every second
kill "$DPID" 2>/dev/null || true
kill "$QPID"
wait "$QPID" 2>/dev/null || true
QPID=""
DPID=""

S="$WORK/session.txt"
sed -n '/msh:[^ ]*\$ echo http/,$p' "$LOG" | tr -d '\r' >"$S"
echo "---- msh ----"
cat "$S"
echo "---- checks ----"
check() { if grep -Eq -- "$2" "$S"; then echo "  ok  $1"; else fail "$1 (no /$2/)"; fi; }
check "update: 3 packages from the repo" "milk: index updated, 3 packages"
check "search lists snake" "^snake +1.0.0 "
check "info shows the sha256" "^sha256: +[0-9a-f]{64}"
check "install verified the package sha256" "milk: sha256 [0-9a-f]{16}\.\.\. ok"
check "install verified the payload sha256" "milk: payload sha256 [0-9a-f]{16}\.\.\. ok \(2 files\)"
check "install linked /bin/fortune" "/bin/fortune -> /Applications/fortune/bin/fortune"
check "fortune runs (quote 1)" "There is no use crying over spilt milk"
check "fortune runs (random)" "-- milkOS fortune \(12 quotes\)"
check "remove" "milk: removed fortune 1.0.0"
check "try: installed into the clone only" "snake 1.0.0 is installed in clone try-snake only"
check "snake runs from the clone" "snake: demo over after [0-9]+ ticks, length [0-9]+"
check "live /bin has no snake while trying" "ls: /bin/snake: "
check "the clone has snake" "^snake$"
check "clone listed while trying" "^try-snake of=milk-try-snake at=/Volumes/Milk-try-snake"
check "discard" "milk: discarded clone try-snake"
check "keep (promote)" "milk: kept moo2048 1.0.0: clone try-moo2048 is now the live system"
check "kept package runs from /bin" "moo2048: selftest ok"
check "audit: install" "user.pkg.install +name=fortune version=1.0.0 sha256="
check "audit: remove" "user.pkg.remove +name=fortune"
check "audit: try" "user.pkg.try +name=snake"
check "audit: discard" "user.pkg.discard +name=snake"
check "audit: keep" "user.pkg.keep +name=moo2048"
check "audit: kernel saw the clone" "/bin/milk +milkfs.clone +snap=milk-try-snake name=try-snake result=ok"
check "audit: kernel saw the discard" "milkfs.clone.discard +name=try-snake result=ok"
check "audit: kernel saw the promote" "milkfs.clone.promote +name=try-moo2048 kept_as=before-try-moo2048 result=ok"
check "audit verify: RAM ring OK" "audit verify: /proc/audit: OK"
check "audit verify: persisted log OK" "audit verify: /Volumes/Milk/var/log/audit.log: OK"
# Both `ls /Applications` (after remove, and while snake sat in the clone) print nothing
# (the index cache is a dot directory): the next line is the prompt.
if grep -A1 '\$ ls /Applications$' "$S" | grep -v -- '--' | grep -v '\$ ls /Applications$' | grep -qv '^msh:'; then
  fail "/Applications not empty after remove / while trying"
fi
echo "  ok  live /Applications empty after remove and while snake was only in the clone"

echo "---- on disk (mkmilkfs.py, after shutdown) ----"
python3 -I scripts/mkmilkfs.py check "$WORK/fs.img" | tail -3
python3 -I scripts/mkmilkfs.py ls "$WORK/fs.img" /Applications | tee "$WORK/apps.txt"
grep -q " moo2048$" "$WORK/apps.txt" || fail "moo2048 not on disk after keep"
grep -q " fortune$" "$WORK/apps.txt" && fail "fortune still on disk after remove"
grep -q " snake$" "$WORK/apps.txt" && fail "snake on the live volume after discard"
python3 -I scripts/mkmilkfs.py ls "$WORK/fs.img" / | grep -q "try-snake" && fail "clone left behind"
python3 -I scripts/mkmilkfs.py cat "$WORK/fs.img" /var/log/audit.log | grep -c "user.pkg" | sed 's/^/audit.log on disk: user.pkg records: /'
echo "  ok  disk: moo2048 kept, fortune removed, snake never reached the live volume"
echo "---- second boot of the same image ----"
rm -f "$SERIAL" "$WORK/drive.done"
LOG2="$WORK/serial2.log"
qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none -global virtio-mmio.force-legacy=false \
  "${DISK_ARGS[@]}" -netdev user,id=n0,restrict=on -device virtio-net-device,netdev=n0 2>>"$WORK/qemu.err" &
QPID=$!
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG2" \
  "$P" 'send:moo2048 --selftest' 'wait:moo2048: selftest' "$P" \
  'send:milk list' "$P" 'send:audit verify' 'wait:audit.log: [A-Z]+' "$P" \
  "done:$WORK/drive.done" >"$WORK/drive2.log" 2>&1 &
DPID=$!
for _ in $(seq 600); do
  [ -e "$WORK/drive.done" ] && break
  kill -0 "$DPID" 2>/dev/null || { cat "$WORK/drive2.log" >&2; fail "second boot session failed"; }
  sleep 0.1
done
[ -e "$WORK/drive.done" ] || fail "second boot session timed out"
kill "$DPID" 2>/dev/null || true
kill "$QPID"
wait "$QPID" 2>/dev/null || true
QPID=""
DPID=""
S="$WORK/session2.txt"
sed -n '/msh:[^ ]*\$ moo2048/,$p' "$LOG2" | tr -d '\r' >"$S"
cat "$S"
check "after reboot: init relinked /bin/moo2048 and it runs" "moo2048: selftest ok"
check "after reboot: still installed" "^moo2048 +1.0.0"
check "after reboot: audit chain OK across boots" "audit verify: /Volumes/Milk/var/log/audit.log: OK"

echo "---- repo server log ----"
cat "$WORK/http.log"

cleanup
trap - EXIT
rm -rf "$WORK"
echo "pkg-test: PASS"
