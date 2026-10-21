#!/usr/bin/env bash
# Milk Drop end-to-end: TWO milkOS VMs (alice and bob), each on its own private disk
# images, eth1 of each linked to the other with qemu `-netdev dgram` over 127.0.0.1 UDP
# (eth0 is a restricted slirp, so neither VM can reach anything else). Then:
#   - both come up with link-local eth1 addresses and a fresh Ed25519 identity,
#   - alice discovers bob (`drop list`), sends him a 1 MiB file; bob accepts at the prompt
#     (first contact: fingerprint shown, trusted on first use),
#   - bob sends alice a different 1 MiB file the other way,
#   - bob gets a new key under the same name: alice refuses to send, loudly,
#   - audit records (user.drop.*) on both,
#   - after shutdown, the host reads both received files out of the images and compares
#     their SHA-256 with the originals, and a second boot of alice's image keeps her
#     identity (same fingerprint: /etc/milk/drop is persistent).
#
# Network safety: the only sockets on the Mac are qemu's two UDP ports on 127.0.0.1; no
# internet, no host servers. Work files in a mktemp dir; only the qemus and drivers
# started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
WORK="$(mktemp -d /tmp/milk-drop-test.XXXXXX)"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "drop-test: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

# Two different 1 MiB files to send, and each VM's private images.
python3 -I -c "
import os, sys
for p in sys.argv[1:]:
    open(p, 'wb').write(os.urandom(1 << 20))" "$WORK/to-bob.bin" "$WORK/to-alice.bin"
for vm in alice bob; do
  python3 -I scripts/mkmilkfs.py mkfs "$WORK/$vm-fs.img" --size 64M --label Milk >/dev/null
  python3 -I scripts/mkmilkfs.py mkdir "$WORK/$vm-fs.img" /share >/dev/null
  dd if=/dev/zero of="$WORK/$vm-disk.img" bs=1m count=0 seek=64 2>/dev/null
  printf 'milkOS scratch disk (created by scripts/disk.sh)\n' | dd of="$WORK/$vm-disk.img" conv=notrunc 2>/dev/null
done
python3 -I scripts/mkmilkfs.py put "$WORK/alice-fs.img" /share/to-bob.bin "$WORK/to-bob.bin" >/dev/null
python3 -I scripts/mkmilkfs.py put "$WORK/bob-fs.img" /share/to-alice.bin "$WORK/to-alice.bin" >/dev/null
SHA_TO_BOB=$(shasum -a 256 "$WORK/to-bob.bin" | cut -d' ' -f1)
SHA_TO_ALICE=$(shasum -a 256 "$WORK/to-alice.bin" | cut -d' ' -f1)
echo "files: to-bob.bin $SHA_TO_BOB, to-alice.bin $SHA_TO_ALICE (1 MiB each)"

# The wire between the two eth1s: two free UDP ports on 127.0.0.1.
read -r PA PB < <(python3 -I -c "
import socket
ss = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(2)]
for s in ss: s.bind(('127.0.0.1', 0))
print(*[s.getsockname()[1] for s in ss])")
echo "eth1 link: alice 127.0.0.1:$PA <-> bob 127.0.0.1:$PB (qemu -netdev dgram)"

# boot NAME MAC LOCAL_PORT REMOTE_PORT: eth0 = restricted slirp, eth1 = the dgram link.
boot() {
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-2}" -m 512M \
    -kernel "$KERNEL" -chardev "socket,id=s0,path=$WORK/$1.sock,server=on,wait=off" -serial chardev:s0 \
    -display none -global virtio-mmio.force-legacy=false \
    -drive "file=$WORK/$1-disk.img,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -drive "file=$WORK/$1-fs.img,if=none,format=raw,id=hd1" -device virtio-blk-device,drive=hd1 \
    -netdev user,id=n0,restrict=on -device virtio-net-device,netdev=n0 \
    -netdev "dgram,id=d0,local.type=inet,local.host=127.0.0.1,local.port=$3,remote.type=inet,remote.host=127.0.0.1,remote.port=$4" \
    -device "virtio-net-device,netdev=d0,mac=$2" 2>>"$WORK/$1.qemu.err" &
}
boot alice 02:6d:6b:64:0a:0a "$PA" "$PB"
QA=$!
PIDS+=("$QA")
boot bob 02:6d:6b:64:0b:0b "$PB" "$PA"
QB=$!
PIDS+=("$QB")
echo "qemu: alice pid $QA, bob pid $QB"

P='wait:msh:[^\n]*\$ '
DONE_RE='drop: (sent|received|[^\n]*(refused|declined|isn.t|no peer|error|failed|mismatch|timed out|closed))'
python3 -I scripts/serial_drive.py "$WORK/alice.sock" "$WORK/alice.log" \
  "$P" 'send:drop name alice' "$P" 'send:drop id' "$P" "done:$WORK/a1" "waitfile:$WORK/b1" \
  'send:drop list' "$P" \
  'send:drop send bob /Volumes/Milk/share/to-bob.bin' "wait:$DONE_RE" "$P" \
  'send:drop recv' 'wait:receiving as' "done:$WORK/a2" \
  'wait:accept\? \[y/N\]' 'send:y' "wait:$DONE_RE" "$P" \
  "waitfile:$WORK/b3" \
  'send:drop send bob /Volumes/Milk/share/to-bob.bin' "wait:$DONE_RE" "$P" \
  'send:drop peers' "$P" \
  'send:ls /home/milk/Downloads' "$P" \
  'send:audit --event user.drop' "$P" \
  'send:echo now > /sys/fs/milkfs/sync' "$P" \
  "done:$WORK/alice.done" >"$WORK/alice.drive" 2>&1 &
DA=$!
PIDS+=("$DA")
python3 -I scripts/serial_drive.py "$WORK/bob.sock" "$WORK/bob.log" \
  "$P" 'send:drop name bob' "$P" "waitfile:$WORK/a1" \
  'send:drop recv' 'wait:receiving as' "done:$WORK/b1" \
  'wait:accept\? \[y/N\]' 'send:y' "wait:$DONE_RE" "$P" \
  "waitfile:$WORK/a2" \
  'send:drop list' "$P" \
  'send:drop send alice /Volumes/Milk/share/to-alice.bin' "wait:$DONE_RE" "$P" \
  'send:mv /etc/milk/drop/identity.secret /etc/milk/drop/old.secret' "$P" \
  'send:drop id' "$P" \
  'send:drop recv -y' 'wait:receiving as' "done:$WORK/b3" \
  "wait:$DONE_RE" "$P" "waitfile:$WORK/alice.done" \
  'send:drop recv -k' 'wait:receiving as' 'raw:\x03' "$P" \
  'send:audit --event user.drop' "$P" \
  'send:echo now > /sys/fs/milkfs/sync' "$P" \
  "done:$WORK/bob.done" >"$WORK/bob.drive" 2>&1 &
DB=$!
PIDS+=("$DB")

for _ in $(seq 1800); do
  [ -e "$WORK/alice.done" ] && [ -e "$WORK/bob.done" ] && break
  kill -0 "$DA" 2>/dev/null || [ -e "$WORK/alice.done" ] || { cat "$WORK/alice.drive" >&2; tail -30 "$WORK/alice.log" >&2; fail "alice's session failed"; }
  kill -0 "$DB" 2>/dev/null || [ -e "$WORK/bob.done" ] || { cat "$WORK/bob.drive" >&2; tail -30 "$WORK/bob.log" >&2; fail "bob's session failed"; }
  sleep 0.1
done
[ -e "$WORK/alice.done" ] && [ -e "$WORK/bob.done" ] || fail "sessions timed out"
sleep 1.5 # the sync
kill "$DA" "$DB" 2>/dev/null || true
kill "$QA" "$QB"
wait "$QA" "$QB" 2>/dev/null || true

A="$WORK/alice.txt"
B="$WORK/bob.txt"
tr -d '\r' <"$WORK/alice.log" >"$A"
tr -d '\r' <"$WORK/bob.log" >"$B"
echo "---- alice ----"
sed -n '/msh:[^ ]*\$ drop name alice/,$p' "$A"
echo "---- bob ----"
sed -n '/msh:[^ ]*\$ drop name bob/,$p' "$B"
echo "---- checks ----"
check() { # file label regex
  if grep -Eq -- "$3" "$1"; then echo "  ok  $2"; else fail "$2 (no /$3/ in $(basename "$1"))"; fi
}
check "$A" "alice: eth1 link-local from her MAC" "\\[net\\] eth1: link-local address 169\\.254\\.10\\.10/16"
check "$B" "bob: eth1 link-local from his MAC" "\\[net\\] eth1: link-local address 169\\.254\\.11\\.11/16"
check "$A" "eth0 is still slirp + DHCP" "\\[net\\] dhcp: 10\\.0\\.2\\.15"
check "$A" "alice: identity made at first boot" "drop: new identity milk-[0-9a-f]{4} \\(fingerprint [0-9a-f ]{19}\\) in /etc/milk/drop"
check "$A" "init started the Milk Drop daemon" "drop \\(pid [0-9]+\\): Milk Drop on eth1"
check "$A" "alice discovers bob (new, receiving)" "^bob +169\\.254\\.11\\.11 +[0-9a-f ]{19} +yes +new "
check "$A" "first contact: bob's fingerprint shown, TOFU" "drop: bob is new: fingerprint [0-9a-f ]{19} \\(trusted on first use\\)"
check "$B" "bob's prompt names alice, NEW, with her fingerprint" "drop: alice \\(NEW peer, fingerprint [0-9a-f ]{19}; accepting trusts this key from now on\\) wants to send you to-bob\\.bin \\(1024 KiB\\)\\."
check "$A" "alice's progress reaches 100%" "drop: \\[##########\\] 100%  1024 KiB / 1024 KiB"
check "$B" "bob verified the SHA-256 (the host's hash)" "drop: received /home/milk/Downloads/to-bob\\.bin \\(1024 KiB\\) from alice, sha256 $SHA_TO_BOB ok"
check "$A" "alice: bob confirmed the hash" "drop: sent to-bob\\.bin to bob: 1024 KiB in [0-9]+ ms"
check "$B" "bob discovers alice (known now)" "^alice +169\\.254\\.10\\.10 +[0-9a-f ]{19} +yes +known "
check "$B" "the other way: bob's sender side knows alice" "drop: alice \\([0-9a-f ]{19}\\) verified"
check "$A" "alice verified bob's file" "drop: received /home/milk/Downloads/to-alice\\.bin \\(1024 KiB\\) from bob, sha256 $SHA_TO_ALICE ok"
check "$A" "alice's prompt: bob is known (no NEW)" "drop: bob \\(fingerprint [0-9a-f ]{19}\\) wants to send you to-alice\\.bin"
check "$B" "bob: a new identity (same name) after losing the old key" "drop: new identity bob \\(fingerprint"
check "$A" "changed key refused, loudly" "drop: WARNING: bob's KEY HAS CHANGED\\. Refusing to talk to it\\."
check "$A" "the send failed because of it" "drop: bob's key changed; refused"
check "$A" "audit: alice's send" "user\\.drop\\.send +peer=bob fp=[0-9a-f]{16} addr=169\\.254\\.11\\.11 file=to-bob\\.bin size=1048576 sha256=$SHA_TO_BOB result=ok"
check "$A" "audit: alice's receive" "user\\.drop\\.recv +peer=bob fp=[0-9a-f]{16} addr=169\\.254\\.11\\.11 file=to-alice\\.bin size=1048576 sha256=$SHA_TO_ALICE saved=/home/milk/Downloads/to-alice\\.bin result=ok"
check "$A" "audit: the refused key change" "user\\.drop\\.key_changed +role=send name=bob addr=169\\.254\\.11\\.11 trusted_fp=[0-9a-f]{16} offered_fp=[0-9a-f]{16} action=refused"
check "$A" "audit: first-use trust" "user\\.drop\\.trust +name=bob fp=[0-9a-f]{16} role=send"
check "$B" "audit: bob's receive" "user\\.drop\\.recv +peer=alice .* result=ok"
check "$B" "audit: bob's send" "user\\.drop\\.send +peer=alice .* result=ok"
check "$B" "audit: identity creation" "user\\.drop\\.identity +name=milk-[0-9a-f]{4} fp="
check "$B" "bob's receiver saw alice hang up on the changed key" "drop: the peer closed the connection"
# Ctrl-C ends a drop recv blocked in accept (blocking socket calls take signals).
grep -A2 'drop recv -k' "$B" | grep -q '^msh:' || fail "Ctrl-C did not stop drop recv -k"
echo "  ok  Ctrl-C stops a drop recv blocked in accept"
FP_A=$(sed -n 's/^fingerprint: *//p' "$A" | head -1)
[ -n "$FP_A" ] || fail "no fingerprint in alice's drop id"

echo "---- on disk (mkmilkfs.py, after shutdown) ----"
for vm in alice bob; do
  python3 -I scripts/mkmilkfs.py check "$WORK/$vm-fs.img" --quiet || fail "$vm's image doesn't check"
done
GOT_B=$(python3 -I scripts/mkmilkfs.py cat "$WORK/bob-fs.img" /home/milk/Downloads/to-bob.bin | shasum -a 256 | cut -d' ' -f1)
GOT_A=$(python3 -I scripts/mkmilkfs.py cat "$WORK/alice-fs.img" /home/milk/Downloads/to-alice.bin | shasum -a 256 | cut -d' ' -f1)
[ "$GOT_B" = "$SHA_TO_BOB" ] || fail "bob's to-bob.bin on disk: $GOT_B, sent $SHA_TO_BOB"
[ "$GOT_A" = "$SHA_TO_ALICE" ] || fail "alice's to-alice.bin on disk: $GOT_A, sent $SHA_TO_ALICE"
echo "  ok  both received files on disk match the host's originals (SHA-256)"
python3 -I scripts/mkmilkfs.py cat "$WORK/alice-fs.img" /etc/milk/drop/known_peers | grep -q "^bob " || fail "alice's known_peers has no bob on disk"
echo "  ok  alice's trusted peers are on her disk (/etc/milk/drop/known_peers)"

echo "---- second boot of alice's image (alone) ----"
rm -f "$WORK/alice.sock" "$WORK/alice2.done"
boot alice 02:6d:6b:64:0a:0a "$PA" "$PB"
QA=$!
PIDS+=("$QA")
python3 -I scripts/serial_drive.py "$WORK/alice.sock" "$WORK/alice2.log" \
  "$P" 'send:drop id' "$P" 'send:drop peers' "$P" "done:$WORK/alice2.done" >"$WORK/alice2.drive" 2>&1 &
DA=$!
PIDS+=("$DA")
for _ in $(seq 600); do [ -e "$WORK/alice2.done" ] && break; sleep 0.1; done
[ -e "$WORK/alice2.done" ] || fail "second boot timed out"
kill "$DA" 2>/dev/null || true
kill "$QA"
wait "$QA" 2>/dev/null || true
A2="$WORK/alice2.txt"
tr -d '\r' <"$WORK/alice2.log" | sed -n '/msh:[^ ]*\$ drop id/,$p' >"$A2"
cat "$A2"; echo
check "$A2" "after reboot: same name" "^name: +alice$"
check "$A2" "after reboot: same fingerprint ($FP_A)" "^fingerprint: +$FP_A$"
check "$A2" "after reboot: bob still trusted" "^bob +[0-9a-f ]{19}"

cleanup
trap - EXIT
rm -rf "$WORK"
echo "drop-test: PASS"
