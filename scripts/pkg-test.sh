#!/usr/bin/env bash
# End-to-end test of the `milk` package manager, on PRIVATE disk images (fresh milkfs in a
# mktemp dir; the shared /tmp/milk-*.img are never touched).
#   1. build packages/ into a repo dir and serve it with python3 http.server on 127.0.0.1
#      (a free port; the guest reaches it as 10.0.2.2:PORT)
#   2. boot, and in msh: milk update / search / info, install fortune, run it, list, remove;
#      try snake in a clone, run it from the clone, check the live /Apps and /Commands
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
scripts/milk-repo.sh build "$WORK/repo-all" >"$WORK/repo.log"
# Served at first: the three original packages. fortune 1.1.0 and games join later (the
# upgrade / dependency phase), from this pristine signed copy.
mkdir -p "$WORK/repo"
for f in fortune-1.0.0 snake-1.0.0 moo2048-1.0.0; do cp "$WORK/repo-all/$f.milkpkg" "$WORK/repo-all/$f.milkpkg.sig" "$WORK/repo/"; done
python3 -I scripts/mkpkg.py index "$WORK/repo" --key scripts/milk-test-key.secret >>"$WORK/repo.log"
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
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none -global virtio-mmio.force-legacy=false \
  "${DISK_ARGS[@]}" \
  -netdev user,id=n0 -device virtio-net-device,netdev=n0 2>"$WORK/qemu.err" &
QPID=$!
P='wait:msh:[^\n]*\$ '
CL=/Volumes/Milk-try-snake
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG" \
  "$P" "send:print http://10.0.2.2:$PORT/ > /System/Settings/milk/repos" "$P" \
  'send:milk update' 'wait:index updated' "$P" \
  'send:milk search' "$P" \
  'send:milk info fortune' "$P" \
  'send:milk install fortune' 'wait:milk: (installed|[^\n]*error|[^\n]*mismatch)' "$P" \
  'send:fortune -n 1' "$P" \
  'send:fortune' "$P" \
  'send:milk list' "$P" \
  'send:milk remove fortune' "$P" \
  'send:files list /Apps/fortune' "$P" \
  'send:files list /Apps' "$P" \
  'send:milk remove store' "$P" \
  'send:milk try snake' 'wait:milk: (then|[^\n]*error)' "$P" \
  "send:$CL/Apps/snake/snake --demo 400" 'wait:snake: demo over' "$P" \
  'send:milk run-try snake --demo 300' 'wait:(snake: demo over|milk: [^\n]*(error|not|denied))' "$P" \
  'send:files list /Apps/snake' "$P" \
  'send:files list /Commands/snake' "$P" \
  "send:files list $CL/Apps" "$P" \
  'send:text show /System/Kernel/fs/milkfs/clones' "$P" \
  'send:milk discard snake' 'wait:discarded' "$P" \
  'send:text show /System/Kernel/fs/milkfs/clones' "$P" \
  'send:files list /Volumes' "$P" \
  'send:milk try moo2048' 'wait:milk: (then|[^\n]*error)' "$P" \
  'send:milk run-try moo2048 --selftest' 'wait:(moo2048: selftest|milk: )' "$P" \
  "send:files list /Volumes/Milk-try-moo2048/tmp" "$P" \
  'send:milk keep moo2048' 'wait:milk: (kept|[^\n]*error)' "$P" \
  'send:files list /Volumes/Milk/tmp' "$P" \
  'send:moo2048 --selftest' 'wait:moo2048: selftest' "$P" \
  'send:milk list' "$P" \
  'send:milk selftest' 'wait:milk: selftest' "$P" \
  'send:milk keys' "$P" \
  "done:$WORK/phase1" "waitfile:$WORK/tampered1" \
  'send:milk install fortune' 'wait:milk: [^\n]*(refused|installed)' "$P" \
  'send:milk install snake' 'wait:milk: [^\n]*(refused|installed)' "$P" \
  'send:milk --insecure install snake' 'wait:milk: [^\n]*(installed|refused|error)' "$P" \
  'send:milk remove snake' "$P" \
  "done:$WORK/phase2" "waitfile:$WORK/tampered2" \
  'send:milk update' 'wait:milk: [^\n]*(refused|updated)' "$P" \
  'send:audit --event user.pkg.verify_fail' "$P" \
  "done:$WORK/phase3" "waitfile:$WORK/restored" \
  'send:milk update' 'wait:milk: [^\n]*(updated|refused)' "$P" \
  'send:milk install fortune=1.0.0' 'wait:milk: (installed|[^\n]*refused|[^\n]*error)' "$P" \
  'send:milk search fortune' "$P" \
  'send:milk upgrade fortune' 'wait:milk: (then|[^\n]*failed|[^\n]*error)' "$P" \
  'send:/Volumes/Milk-try-upgrade/Apps/fortune/fortune -n 15' "$P" \
  'send:milk run-try fortune -n 15' "$P" \
  'send:audit --event sandbox' "$P" \
  'send:milk discard upgrade' 'wait:discarded' "$P" \
  'send:milk info fortune' "$P" \
  'send:milk upgrade -y' 'wait:milk: (kept|[^\n]*failed|[^\n]*error)' "$P" \
  'send:fortune -n 15' "$P" \
  'send:milk install games' 'wait:milk: (installed games|[^\n]*error|[^\n]*refused)' "$P" \
  'send:milk remove snake' "$P" \
  'send:milk info games' "$P" \
  'send:milk remove games' "$P" \
  'send:milk autoremove' 'wait:autoremove: ' "$P" \
  'send:milk list' "$P" \
  'send:milk list | filter version > 1.0' "$P" \
  'send:milk search | filter status == available | select name version' "$P" \
  'send:milk list | to-json' "$P" \
  'send:audit --event user.pkg' "$P" \
  'send:audit --event milkfs' "$P" \
  'send:audit verify' "$P" \
  'send:print now > /System/Kernel/fs/milkfs/sync' "$P" \
  "done:$WORK/drive.done" >"$WORK/drive.log" 2>&1 &
DPID=$!
flip() { # flip one byte in the middle of a file (keeps its size)
  python3 -I -c "
import sys
p = sys.argv[1]
b = bytearray(open(p, 'rb').read())
b[len(b) // 2] ^= 0x01
open(p, 'wb').write(b)" "$1"
}
for _ in $(seq 1200); do
  [ -e "$WORK/drive.done" ] && break
  if [ -e "$WORK/phase1" ] && [ ! -e "$WORK/tampered1" ]; then
    flip "$WORK/repo/fortune-1.0.0.milkpkg" # signature (and sha256) no longer match
    rm -f "$WORK/repo/snake-1.0.0.milkpkg.sig" # unsigned
    touch "$WORK/tampered1"
  fi
  if [ -e "$WORK/phase3" ] && [ ! -e "$WORK/restored" ]; then
    rm -f "$WORK/repo"/*
    cp "$WORK/repo-all"/* "$WORK/repo/" # pristine and complete: 5 packages, all signed
    touch "$WORK/restored"
  fi
  if [ -e "$WORK/phase2" ] && [ ! -e "$WORK/tampered2" ]; then
    flip "$WORK/repo/index.txt" # the index signature no longer matches
    touch "$WORK/tampered2"
  fi
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
# From the first command on. Asynchronous kernel lines (timemachine's boot snapshot, the
# audit log re-attaching after a promote) can land inside a command's echo:
# console_clean.py puts them back on their own lines first.
python3 -I scripts/console_clean.py "$LOG" | sed -n '/print http:\/\/10.0.2.2:/,$p' >"$S"
echo "---- msh ----"
cat "$S"
echo "---- checks ----"
check() { if grep -Eq -- "$2" "$S"; then echo "  ok  $1"; else fail "$1 (no /$2/)"; fi; }
check "update: 3 packages from the repo" "milk: index updated, 3 packages"
check "search lists snake" "^snake +1.0.0 "
check "info shows the sha256" "^sha256: +[0-9a-f]{64}"
check "install verified the package sha256" "milk: sha256 [0-9a-f]{16}\.\.\. ok"
check "install verified the payload sha256" "milk: payload sha256 [0-9a-f]{16}\.\.\. ok \(2 files\)"
check "install linked /Commands/fortune" "/Commands/fortune -> /Apps/fortune/fortune"
check "fortune runs (quote 1)" "There is no use crying over spilt milk"
check "fortune runs (random)" "-- milkOS fortune \(12 quotes\)"
check "remove" "milk: removed fortune 1.0.0"
check "removed from /Apps" "files: /Apps/fortune doesn't exist"
check "built-in apps are in /Apps" "(^| )store/( |$)"
check "milk leaves built-in apps alone" "milk: store is a built-in app, not a package"
check "try: installed into the clone only" "snake 1.0.0 is installed in clone try-snake only"
check "snake runs from the clone" "snake: demo over after [0-9]+ ticks, length [0-9]+"
check "live /Commands has no snake while trying" "files: /Commands/snake doesn't exist"
check "live /Apps has no snake while trying" "files: /Apps/snake doesn't exist"
check "the clone has snake" "(^| )snake/( |$)"
check "clone listed while trying" "^try-snake of=milk-try-snake at=/Volumes/Milk-try-snake"
[ "$(grep -c "snake: demo over after" "$S")" -ge 2 ] || fail "run-try snake did not run"
echo "  ok  run-try snake: runs with its clone as / (chroot), sandboxed"
check "run-try made a scratch dir in the clone" "^run-moo2048/$"
check "keep dropped the run-try scratch dir" "files: /Volumes/Milk/tmp doesn't exist"
check "audit: run-try (chroot + sandbox)" "user.pkg.run_try +name=snake version=1.0.0 clone=try-snake root=/Volumes/Milk-try-snake exec=/Apps/snake/snake sandbox=fs=/Apps:r,fs=/Volumes/Milk-try-snake/Apps:r,fs=/tmp/run-snake:rw"
check "audit: run-try entered the sandbox" "sandbox.enter .*net=local"
check "discard" "milk: discarded clone try-snake"
check "keep (promote)" "milk: kept moo2048 1.0.0: clone try-moo2048 is now the live system"
check "kept package runs from /Commands" "moo2048: selftest ok"
check "milk selftest: RFC 8032 + FIPS vectors" "ed25519 16/16 RFC 8032 checks"
check "update checked the index signature" "index.txt: signature ok \(key milk-test\)"
check "install checked the package signature" "fortune-1.0.0.milkpkg: signature ok \(key milk-test\)"
check "tampered package refused" "milk: fortune-1.0.0.milkpkg: BAD signature \(key milk-test\); refused"
check "unsigned package refused" "milk: snake-1.0.0.milkpkg: unsigned \(no .sig: [^)]*\); refused"
check "--insecure warns and installs" "WARNING: snake-1.0.0.milkpkg: unsigned"
check "tampered index refused" "index.txt: BAD signature \(key milk-test\); refused"
check "audit: verify failure (tampered package)" "user.pkg.verify_fail +what=fortune-1.0.0.milkpkg [^ ]* reason=BAD signature \(key milk-test\) insecure=false"
check "audit: verify failure (unsigned, refused)" "user.pkg.verify_fail +what=snake-1.0.0.milkpkg [^ ]* reason=unsigned .* insecure=false"
check "audit: verify failure (unsigned, --insecure)" "user.pkg.verify_fail +what=snake-1.0.0.milkpkg [^ ]* reason=unsigned .* insecure=true"
check "audit: verify failure (tampered index)" "user.pkg.verify_fail +what=http://10.0.2.2:[0-9]+/index.txt [^ ]* reason=BAD signature"
check "milk selftest: version compare" "milk: version compare 10/10 checks"
check "restored repo: 5 packages" "milk: index updated, 5 packages"
check "pinned install of an older version" "milk: installed fortune 1.0.0 in /Apps/fortune"
check "search offers the upgrade" "^fortune +1.1.0 .*\\[installed 1.0.0, upgrade available\\]"
check "upgrade plan" "milk: upgrade fortune 1.0.0 -> 1.1.0"
check "upgrade went into a clone" "milk: upgraded 1 package\\(s\\) in clone try-upgrade; the live system is untouched"
check "the clone runs the new version" "If an upgrade sours, discard the clone"
# run-try in the upgrade clone: /Apps/fortune inside the jail is the clone's 1.1.0
# (the live one is 1.0.0 and has no 15th quote), so the new quote proves the root.
fq=$(awk '/milk run-try fortune -n 15$/{f=1; next} f && /^msh:/{exit} f' "$S")
echo "$fq" | grep -q "If an upgrade sours" || { echo "$fq"; fail "run-try fortune did not see the clone as /"; }
echo "  ok  run-try fortune -n 15 sees the clone's 1.1.0 (the clone is its /)"
check "discarded upgrade" "discarded clone try-upgrade; the live system never saw the upgrade"
check "live still 1.0.0 after discard" "installed: +1.0.0 in /Apps/fortune"
check "upgrade -y kept" "milk: kept the upgrade: clone try-upgrade is now the live system"
check "dependency announced" "milk: games needs snake"
check "dependency installed as such" "milk: installed snake 1.0.0 in /Apps/snake \\(as a dependency\\)"
check "remove refused while needed" "milk: snake is needed by games"
check "info lists depends" "depends: +snake moo2048>=1.0"
check "autoremove took the orphan" "milk: autoremove: 1 package\\(s\\) removed"
check "audit: upgrade" "user.pkg.upgrade +packages=fortune:1.0.0->1.1.0 clone=try-upgrade"
check "audit: upgrade kept" "user.pkg.keep +name=upgrade clone=try-upgrade now=.*fortune:1.1.0"
check "audit: dependency install" "user.pkg.install +name=snake version=1.0.0 .* auto=true"
check "audit: autoremove" "user.pkg.remove +name=snake version=1.0.0 reason=autoremove"
# Dependencies first: snake is installed before games.
sl=$(grep -n "milk: installed snake 1.0.0 in /Apps/snake (as a dependency)" "$S" | head -1 | cut -d: -f1)
gl=$(grep -n "milk: installed games 1.0.0" "$S" | head -1 | cut -d: -f1)
[ -n "$sl" ] && [ -n "$gl" ] && [ "$sl" -lt "$gl" ] || fail "dependency order (snake line $sl, games line $gl)"
echo "  ok  install order: snake (dependency) before games"
# The live fortune is 1.1.0 at the end: its 15th quote is new in 1.1.
[ "$(grep -c "If an upgrade sours" "$S")" -ge 2 ] || fail "live fortune is not 1.1.0 after upgrade -y"
echo "  ok  live fortune runs 1.1.0 after upgrade -y"
# Structured output: `filter version > 1.0` compares versions (1.0.0 is not > 1.0).
awk '/milk list \| filter version > 1.0$/{f=1; next} f && /^msh:/{exit} f' "$S" >"$WORK/filter.txt"
grep -Eq '^fortune +1\.1\.0 +no ' "$WORK/filter.txt" || { cat "$WORK/filter.txt"; fail "milk list | filter: fortune 1.1.0 missing"; }
grep -q '^moo2048' "$WORK/filter.txt" && { cat "$WORK/filter.txt"; fail "milk list | filter version > 1.0 kept moo2048 1.0.0"; }
echo "  ok  milk list | filter version > 1.0 (fortune 1.1.0 in, moo2048 1.0.0 out)"
awk '/filter status == available \| select name version$/{f=1; next} f && /^msh:/{exit} f' "$S" >"$WORK/avail.txt"
grep -Eq '^games +1\.0\.0$' "$WORK/avail.txt" && grep -Eq '^snake +1\.0\.0$' "$WORK/avail.txt" || { cat "$WORK/avail.txt"; fail "milk search | filter status == available"; }
echo "  ok  milk search | filter status == available | select name version (games, snake)"
check "milk list | to-json" '"name": ?"moo2048", ?"version": ?"1.0.0"'
check "audit: install" "user.pkg.install +name=fortune version=1.0.0 sha256="
check "audit: remove" "user.pkg.remove +name=fortune"
check "audit: try" "user.pkg.try +name=snake"
check "audit: discard" "user.pkg.discard +name=snake"
check "audit: keep" "user.pkg.keep +name=moo2048"
check "audit: kernel saw the clone" "/Commands/milk +milkfs.clone +snap=milk-try-snake name=try-snake result=ok"
check "audit: kernel saw the discard" "milkfs.clone.discard +name=try-snake result=ok"
check "audit: kernel saw the promote" "milkfs.clone.promote +name=try-moo2048 kept_as=before-try-moo2048 result=ok"
check "audit verify: RAM ring OK" "audit verify: /proc/audit: OK"
check "audit verify: persisted log OK" "audit verify: /Volumes/Milk/var/log/audit.log: OK"
# `milk list` never shows the built-in apps (they have no .origin): after keep, only moo2048.
awk '/milk keep moo2048$/{k=1} k && /\$ milk list$/{f=1; next} f && /^msh:/{exit} f' "$S" >"$WORK/list1.txt"
grep -q '^moo2048' "$WORK/list1.txt" && ! grep -Eq '^(store|calendar|clock) ' "$WORK/list1.txt" \
  || { cat "$WORK/list1.txt"; fail "milk list shows built-in apps"; }
echo "  ok  milk list: packages only (moo2048), not the built-in apps"

echo "---- on disk (mkmilkfs.py, after shutdown) ----"
python3 -I scripts/mkmilkfs.py check "$WORK/fs.img" | tail -3
python3 -I scripts/mkmilkfs.py ls "$WORK/fs.img" /Apps | tee "$WORK/apps.txt"
grep -q " moo2048$" "$WORK/apps.txt" || fail "moo2048 not on disk after keep"
grep -q " store$" "$WORK/apps.txt" || fail "the built-in apps are not in the volume's Apps folder"
grep -q " fortune$" "$WORK/apps.txt" || fail "fortune (1.1.0) not on disk"
python3 -I scripts/mkmilkfs.py cat "$WORK/fs.img" /Apps/fortune/manifest | grep -q "^version 1.1.0$" || fail "fortune on disk is not 1.1.0"
grep -q " snake$" "$WORK/apps.txt" && fail "snake on disk after autoremove"
grep -q " games$" "$WORK/apps.txt" && fail "games on disk after remove"
python3 -I scripts/mkmilkfs.py ls "$WORK/fs.img" / | grep -q "try-snake" && fail "clone left behind"
python3 -I scripts/mkmilkfs.py cat "$WORK/fs.img" /var/log/audit.log | grep -c "user.pkg" | sed 's/^/audit.log on disk: user.pkg records: /'
echo "  ok  disk: moo2048 kept, fortune upgraded to 1.1.0, games removed, snake autoremoved, built-in apps there"
echo "---- second boot of the same image ----"
rm -f "$SERIAL" "$WORK/drive.done"
LOG2="$WORK/serial2.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none -global virtio-mmio.force-legacy=false \
  "${DISK_ARGS[@]}" -netdev user,id=n0,restrict=on -device virtio-net-device,netdev=n0 2>>"$WORK/qemu.err" &
QPID=$!
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG2" \
  "$P" 'send:pause 1' "$P" 'send:moo2048 --selftest' 'wait:moo2048: selftest' "$P" \
  'send:milk list' "$P" 'send:fortune -n 15' "$P" 'send:audit verify' 'wait:audit.log: [A-Z]+' "$P" \
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
python3 -I scripts/console_clean.py "$LOG2" | sed -n '/msh:[^ ]*\$ moo2048/,$p' >"$S"
cat "$S"
check "after reboot: init relinked /Commands/moo2048 and it runs" "moo2048: selftest ok"
check "after reboot: still installed" "^moo2048 +1.0.0"
check "after reboot: fortune is 1.1.0" "^fortune +1.1.0"
check "after reboot: audit chain OK across boots" "audit verify: /Volumes/Milk/var/log/audit.log: OK"

echo "---- repo server log ----"
cat "$WORK/http.log"

cleanup
trap - EXIT
rm -rf "$WORK"
echo "pkg-test: PASS"
