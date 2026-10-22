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
#   - as the ordinary user milk (the desktop's account): alice-as-milk sends bob-as-milk a
#     file through each machine's root drop daemon (which signs for them over
#     /run/drop.sock); it lands in bob's ~/Downloads owned by milk; milk can't read the
#     machine's secret key; milk (an administrator) renames through the daemon, nobody
#     is refused,
#   - IPv6 between the VMs over eth1: alice resolves bob.local over mDNS on ff02::fb
#     (AAAA), pings him with ICMPv6, and sends him a file over IPv6 (TCP between the two
#     eth1 fd6d:696c:6b00:1::/64 addresses),
#   - after shutdown, the host reads both received files out of the images and compares
#     their SHA-256 with the originals, and a second boot of alice's image keeps her
#     identity (same fingerprint: /System/Settings/milk/drop is persistent).
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

# Two different 1 MiB files to send, and each VM's private images. to-bob.bin is half
# random, half a known text (the canary) that must never show up on the wire.
CANARY="MILKDROP-PLAINTEXT-CANARY-$$"
python3 -I -c "
import os, sys
canary = (sys.argv[3] + ' the quick brown cow jumps over the lazy moon\\n').encode()
half = 1 << 19
open(sys.argv[1], 'wb').write(os.urandom(half) + (canary * (half // len(canary) + 1))[:half])
open(sys.argv[2], 'wb').write(os.urandom(1 << 20))" "$WORK/to-bob.bin" "$WORK/to-alice.bin" "$CANARY"
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
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-2}" -m 512M \
    -kernel "$KERNEL" -chardev "socket,id=s0,path=$WORK/$1.sock,server=on,wait=off" -serial chardev:s0 \
    -display none -global virtio-mmio.force-legacy=false \
    -drive "file=$WORK/$1-disk.img,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -drive "file=$WORK/$1-fs.img,if=none,format=raw,id=hd1" -device virtio-blk-device,drive=hd1 \
    -netdev user,id=n0,restrict=on -device virtio-net-device,netdev=n0 \
    -netdev "dgram,id=d0,local.type=inet,local.host=127.0.0.1,local.port=$3,remote.type=inet,remote.host=127.0.0.1,remote.port=$4" \
    -device "virtio-net-device,netdev=d0,mac=$2" \
    -object "filter-dump,id=cap0,netdev=d0,file=$WORK/$1-eth1.pcap" 2>>"$WORK/$1.qemu.err" &
}
boot alice 02:6d:6b:64:0a:0a "$PA" "$PB"
QA=$!
PIDS+=("$QA")
boot bob 02:6d:6b:64:0b:0b "$PB" "$PA"
QB=$!
PIDS+=("$QB")
echo "qemu: alice pid $QA, bob pid $QB"

P='wait:msh:[^\n]*\$ '
DONE_RE='drop: (sent|received|[^\n]*(refused|declined|isn.t|no peer|error|failed|mismatch|timed out|closed|not a Milk Drop|only an administrator|service accounts))'
python3 -I scripts/serial_drive.py "$WORK/alice.sock" "$WORK/alice.log" \
  "$P" 'send:drop name alice' "$P" 'send:netctl hostname alicebox' "$P" 'send:drop id' "$P" 'send:drop selftest' "$P" \
  "done:$WORK/a1" "waitfile:$WORK/b1" \
  'send:drop list' "$P" \
  'send:drop send bob /Volumes/Milk/share/to-bob.bin' "wait:$DONE_RE" "$P" \
  'send:drop recv' 'wait:receiving as' "done:$WORK/a2" \
  'wait:accept\? \[y/N\]' 'send:y' "wait:$DONE_RE" "$P" \
  "waitfile:$WORK/b3" \
  'send:drop send bob /Volumes/Milk/share/to-bob.bin' "wait:$DONE_RE" "$P" \
  'send:drop id --rotate' "$P" \
  'send:drop forget bob' "$P" \
  'send:drop recv -y' 'wait:receiving as' "done:$WORK/a4" "wait:$DONE_RE" "$P" \
  "waitfile:$WORK/bm1" \
  'send:su milk -c "drop send bob /Volumes/Milk/share/to-bob.bin"' "wait:$DONE_RE" "$P" \
  'send:ping -6 -c 2 -i 200 bob.local' 'wait:rtt min|unknown|not resolved|no answer|Request timeout' "$P" \
  "waitfile:$WORK/bv6" \
  'send:drop send bob.local /Volumes/Milk/share/to-bob.bin' "wait:$DONE_RE" "$P" \
  "waitfile:$WORK/bhid" 'send:drop list' "$P" 'send:ping -6 -c 1 -W 500 bob.local' "$P" \
  'send:echo milk | sudo -S -u nobody /Commands/netctl visible off' "$P" "done:$WORK/alist" \
  'send:su milk -c "cat /System/Settings/milk/drop/identity.secret"' "$P" \
  'send:su milk -c "drop name alice"' "$P" \
  'send:echo milk | sudo -S -u nobody /Commands/drop name evil' "$P" \
  'send:drop peers' "$P" \
  'send:files list /Users/milk/Downloads' "$P" \
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
  'send:files move /System/Settings/milk/drop/identity.secret /System/Settings/milk/drop/old.secret' "$P" \
  'send:drop id' "$P" \
  'send:drop recv -y' 'wait:receiving as' "done:$WORK/b3" \
  "wait:$DONE_RE" "$P" "waitfile:$WORK/a4" \
  'send:drop list' "$P" \
  'send:drop send alice /Volumes/Milk/share/to-alice.bin' "wait:$DONE_RE" "$P" \
  'send:su milk -c "drop recv -y"' 'wait:receiving as' "done:$WORK/bm1" "wait:$DONE_RE" "$P" \
  'send:drop recv -y' 'wait:receiving as' "done:$WORK/bv6" "wait:$DONE_RE" "$P" \
  'send:netctl visible off' "$P" 'send:netctl' "$P" "done:$WORK/bhid" "waitfile:$WORK/alist" \
  'send:netctl visible on' "$P" \
  'send:files list -l /Users/milk/Downloads' "$P" \
  "waitfile:$WORK/alice.done" \
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
python3 -I scripts/console_clean.py "$WORK/alice.log" >"$A"
python3 -I scripts/console_clean.py "$WORK/bob.log" >"$B"
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
check "$A" "alice: identity made at first boot" "drop: new identity milk-[0-9a-f]{4} \\(fingerprint [0-9a-f ]{19}\\) in /System/Settings/milk/drop"
check "$A" "init started the Milk Drop daemon" "drop \\(pid [0-9]+\\): Milk Drop on eth1"
check "$A" "alice discovers bob (new, receiving)" "^bob +169\\.254\\.11\\.11 +[0-9a-f ]{19} +yes +new "
check "$A" "first contact: bob's fingerprint shown, TOFU" "drop: bob is new: fingerprint [0-9a-f ]{19} \\(trusted on first use\\)"
check "$B" "bob's prompt names alice, NEW, with her fingerprint" "drop: alice \\(NEW peer, fingerprint [0-9a-f ]{19}; accepting trusts this key from now on\\) wants to send you to-bob\\.bin \\(1024 KiB\\), encrypted\\."
check "$A" "alice's progress reaches 100%" "drop: \\[##########\\] 100%  1024 KiB / 1024 KiB"
check "$B" "bob verified the SHA-256 (the host's hash)" "drop: received /Users/milk/Downloads/to-bob\\.bin \\(1024 KiB\\) from alice, sha256 $SHA_TO_BOB ok"
check "$A" "alice: bob confirmed the hash" "drop: sent to-bob\\.bin to bob: 1024 KiB in [0-9]+ ms"
check "$B" "bob discovers alice (known now)" "^alice +169\\.254\\.10\\.10 +[0-9a-f ]{19} +yes +known "
check "$B" "the other way: bob's sender side knows alice" "drop: alice \\([0-9a-f ]{19}\\) verified"
check "$A" "alice verified bob's file" "drop: received /Users/milk/Downloads/to-alice\\.bin \\(1024 KiB\\) from bob, sha256 $SHA_TO_ALICE ok"
check "$A" "alice's prompt: bob is known (no NEW)" "drop: bob \\(fingerprint [0-9a-f ]{19}\\) wants to send you to-alice\\.bin"
check "$B" "bob: a new identity (same name) after losing the old key" "drop: new identity bob \\(fingerprint"
check "$A" "changed key refused, loudly" "drop: WARNING: bob's KEY HAS CHANGED\\. Refusing to talk to it\\."
check "$A" "the send failed because of it" "drop: bob's key changed; refused"
check "$A" "audit: alice's send" "user\\.drop\\.send +peer=bob fp=[0-9a-f]{16} addr=169\\.254\\.11\\.11 file=to-bob\\.bin size=1048576 sha256=$SHA_TO_BOB enc=x25519-chacha20poly1305 result=ok"
check "$A" "audit: alice's receive" "user\\.drop\\.recv +peer=bob fp=[0-9a-f]{16} addr=169\\.254\\.11\\.11 file=to-alice\\.bin size=1048576 sha256=$SHA_TO_ALICE saved=/Users/milk/Downloads/to-alice\\.bin enc=x25519-chacha20poly1305 result=ok"
check "$A" "audit: the refused key change" "user\\.drop\\.key_changed +role=send name=bob addr=169\\.254\\.11\\.11 trusted_fp=[0-9a-f]{16} offered_fp=[0-9a-f]{16} action=refused"
check "$A" "audit: first-use trust" "user\\.drop\\.trust +name=bob fp=[0-9a-f]{16} role=send"
check "$B" "audit: bob's receive" "user\\.drop\\.recv +peer=alice .* result=ok"
check "$B" "audit: bob's send" "user\\.drop\\.send +peer=alice .* result=ok"
check "$B" "audit: identity creation" "user\\.drop\\.identity +name=milk-[0-9a-f]{4} fp="
check "$B" "bob's receiver saw alice hang up on the changed key" "drop: the peer closed the connection"
# Ctrl-C ends a drop recv blocked in accept (blocking socket calls take signals).
grep -A2 'drop recv -k' "$B" | grep -q '^msh:' || fail "Ctrl-C did not stop drop recv -k"
echo "  ok  Ctrl-C stops a drop recv blocked in accept"
# As milk (not root): both ends through their machine's daemon.
check "$B" "bob as milk received alice-as-milk's file" "drop: received /Users/milk/Downloads/to-bob \\(1\\)\\.bin \\(1024 KiB\\) from alice, sha256 $SHA_TO_BOB ok"
[ "$(grep -Ec "drop: sent to-bob\.bin to bob: 1024 KiB" "$A")" -ge 2 ] || fail "alice as milk did not send (only root's send in the log)"
echo "  ok  alice as milk sent to bob (signed by alice's daemon)"
check "$B" "the file milk received is milk's" "^-[rwx-]{9} +[0-9]+ milk +[a-z]+ +1048576 .*to-bob \\(1\\)\\.bin$"
check "$B" "root's receive into milk's Downloads is milk's too" "^file +[rwx-]{9} +milk +[a-z]+ +1048576 .*to-bob\\.bin$"
grep -A1 'su milk -c "cat /System/Settings/milk/drop/identity.secret"' "$A" | grep -q "ed25519-seed" && fail "milk can read the machine's secret key"
echo "  ok  milk can't read /System/Settings/milk/drop/identity.secret"
check "$A" "milk (admin) renames through the daemon" "drop: this machine is now alice \\(was alice\\)"
check "$A" "audit: the daemon's rename names who asked" "user\\.drop\\.rename +old=alice new=alice by uid=501 user=milk"
check "$A" "nobody may not rename the machine" "drop: name: only an administrator may change this machine's identity"
check "$A" "audit: the refusal" "user\\.drop\\.refused +uid=65534 user=nobody request=name reason=not-an-administrator"
check "$A" "userland crypto vectors (rt::crypto)" "drop: selftest ok"
check "$A" "X25519 vectors in userland" "drop: x25519 +8/8 vectors"
check "$A" "a fresh key is from the kernel CSPRNG" "^rng: +kernel CSPRNG"
check "$A" "the kernel CSPRNG was seeded from virtio-rng" "\\[rng\\] ChaCha20 CSPRNG seeded from virtio-rng \\(64 bytes\\).*: ok$"
check "$A" "the kernel ran every crypto vector" "\\[crypto\\] test vectors: .*x25519 8/8.*: ok"
check "$A" "alice rotated her key" "drop: the old key signed the new one"
check "$B" "bob sees alice's rotation in drop list" "^alice +169\\.254\\.10\\.10 +[0-9a-f ]{19} +yes +rotated "
check "$B" "bob accepts the rotated key (signed by the old one)" "drop: alice rotated its key \\(signed by its old key [0-9a-f ]{19}\\): now [0-9a-f ]{19}"
check "$A" "drop forget: bob's new key is first contact again" "drop: forgot bob; its next key will be trusted on first use"
check "$A" "alice receives over the rotated key" "drop: received /Users/milk/Downloads/to-alice \\(1\\)\\.bin \\(1024 KiB\\) from bob, sha256 $SHA_TO_ALICE ok"
check "$B" "audit: the accepted rotation" "user\\.drop\\.key_rotated +role=send name=alice old_fp=[0-9a-f]{16} new_fp=[0-9a-f]{16} action=accepted"
check "$A" "audit: the rotation" "user\\.drop\\.rotate +name=alice old_fp=[0-9a-f]{16} new_fp=[0-9a-f]{16}"
# IPv6 between the two VMs over eth1 (EUI-64 of 02:6d:6b:64:0b:0b = 6d:6bff:fe64:b0b).
A6='fd6d:696c:6b00:1:6d:6bff:fe64:a0a'
B6='fd6d:696c:6b00:1:6d:6bff:fe64:b0b'
check "$A" "ping -6 bob.local: AAAA over ff02::fb, echo over eth1" "bytes from $B6: icmp_seq=1"
check "$A" "ping -6 bob.local: both replies" "2 packets transmitted, 2 received, 0% packet loss"
check "$B" "bob's receiver listens on eth1's IPv6 address too" "drop: receiving as bob .* and \[$B6\]:[0-9]+"
check "$B" "bob got alice's file over IPv6" "drop: received /Users/milk/Downloads/to-bob \(2\)\.bin \(1024 KiB\) from alice, sha256 $SHA_TO_BOB ok"
check "$A" "audit: alice's send over IPv6" "user\.drop\.send +peer=bob fp=[0-9a-f]{16} addr=$B6 file=to-bob\.bin size=1048576 sha256=$SHA_TO_BOB .*result=ok"
check "$B" "audit: bob's receive over IPv6" "user\.drop\.recv +peer=alice fp=[0-9a-f]{16} addr=$A6 .*result=ok"
# Settings > Network through netctl: Milk Drop visibility and the hostname.
check "$B" "netctl hides bob" "netctl: Milk Drop is now hidden on the VM link"
check "$B" "netctl show says so" "^milk drop +hidden \\(no announcements, no \\.local answers\\)"
check "$B" "netctl show names bob.local" "^local name +bob\\.local \\(the Milk Drop name\\)"
check "$A" "hidden bob: drop list finds nobody" "drop: nobody answered on eth1"
check "$A" "hidden bob: bob.local does not resolve" "ping: bob\\.local: no answer on the link"
check "$A" "nobody may not change visibility" "netctl: visible: visible: only an administrator may change this machine's identity"
check "$B" "netctl shows bob again" "netctl: Milk Drop is now visible on the VM link"
check "$A" "netctl sets the hostname" "netctl: hostname is now alicebox \\(was milk\\)"
# The wire (qemu filter-dump on eth1, both VMs): the hello and discovery are visible, the
# file and its name are not.
for vm in alice bob; do
  [ -s "$WORK/$vm-eth1.pcap" ] || fail "no capture from $vm's eth1"
  grep -aq "MILKDROP/2 HELLO recv" "$WORK/$vm-eth1.pcap" || fail "$vm capture: no plaintext hello (the capture isn't working)"
  grep -aq "ANNOUNCE name=" "$WORK/$vm-eth1.pcap" || fail "$vm capture: no discovery datagrams"
  grep -aq "$CANARY" "$WORK/$vm-eth1.pcap" && fail "$vm capture: the canary text went over the wire in plaintext"
  grep -aq "to-bob.bin" "$WORK/$vm-eth1.pcap" && fail "$vm capture: the file name went over the wire in plaintext"
  grep -aq "sha256=" "$WORK/$vm-eth1.pcap" && fail "$vm capture: the offer went over the wire in plaintext"
done
# IPv6 on the wire: discovery announcements to [ff02::fb]:5359, and MLDv2 reports that
# join ff02::fb (to ff02::16, hop limit 1, router alert) before any of it.
python3 -I scripts/pcap_v6.py "$WORK/alice-eth1.pcap" || fail "alice's capture: IPv6 discovery / MLD (see above)"
echo "  ok  alice announces over ff02::fb too, after an MLDv2 report joining it"
echo "  ok  eth1 captures (filter-dump): hello + discovery visible; canary text, file name and offer never in plaintext ($(wc -c <"$WORK/alice-eth1.pcap" | tr -d ' ') bytes captured on alice)"
FP_A=$(sed -n 's/^drop:   new fingerprint: *//p' "$A" | head -1)
[ -n "$FP_A" ] || fail "no fingerprint in alice's drop id"

echo "---- on disk (mkmilkfs.py, after shutdown) ----"
for vm in alice bob; do
  python3 -I scripts/mkmilkfs.py check "$WORK/$vm-fs.img" --quiet || fail "$vm's image doesn't check"
done
GOT_B=$(python3 -I scripts/mkmilkfs.py cat "$WORK/bob-fs.img" /Users/milk/Downloads/to-bob.bin | shasum -a 256 | cut -d' ' -f1)
GOT_A=$(python3 -I scripts/mkmilkfs.py cat "$WORK/alice-fs.img" /Users/milk/Downloads/to-alice.bin | shasum -a 256 | cut -d' ' -f1)
[ "$GOT_B" = "$SHA_TO_BOB" ] || fail "bob's to-bob.bin on disk: $GOT_B, sent $SHA_TO_BOB"
[ "$GOT_A" = "$SHA_TO_ALICE" ] || fail "alice's to-alice.bin on disk: $GOT_A, sent $SHA_TO_ALICE"
echo "  ok  both received files on disk match the host's originals (SHA-256)"
python3 -I scripts/mkmilkfs.py cat "$WORK/alice-fs.img" /System/Settings/milk/drop/known_peers | grep -q "^bob " || fail "alice's known_peers has no bob on disk"
echo "  ok  alice's trusted peers are on her disk (/System/Settings/milk/drop/known_peers)"

echo "---- second boot of alice's image (alone) ----"
rm -f "$WORK/alice.sock" "$WORK/alice2.done"
boot alice 02:6d:6b:64:0a:0a "$PA" "$PB"
QA=$!
PIDS+=("$QA")
python3 -I scripts/serial_drive.py "$WORK/alice.sock" "$WORK/alice2.log" \
  "$P" 'send:sleep 1' "$P" 'send:drop id' "$P" 'send:drop peers' "$P" 'send:uname -n' "$P" "done:$WORK/alice2.done" >"$WORK/alice2.drive" 2>&1 &
DA=$!
PIDS+=("$DA")
for _ in $(seq 600); do [ -e "$WORK/alice2.done" ] && break; sleep 0.1; done
[ -e "$WORK/alice2.done" ] || fail "second boot timed out"
kill "$DA" 2>/dev/null || true
kill "$QA"
wait "$QA" 2>/dev/null || true
A2="$WORK/alice2.txt"
python3 -I scripts/console_clean.py "$WORK/alice2.log" | sed -n '/msh:[^ ]*\$ drop id/,$p' >"$A2"
cat "$A2"; echo
check "$A2" "after reboot: same name" "^name: +alice$"
check "$A2" "after reboot: same fingerprint ($FP_A)" "^fingerprint: +$FP_A$"
check "$A2" "after reboot: bob still trusted" "^bob +[0-9a-f ]{19}"
check "$A2" "after reboot: the hostname netctl set (uname -n)" "^alicebox$"
grep -aq "\[net\] hostname alicebox (from /System/Settings/milk/net/hostname)" "$WORK/alice2.log" || fail "after reboot: the kernel did not apply /System/Settings/milk/net/hostname"
echo "  ok  after reboot: the kernel applied /System/Settings/milk/net/hostname (persistent)"

echo "---- a machine with a pre-CSPRNG (legacy) identity: carol ----"
# A fresh image whose /System/Settings/milk/drop holds an old-format secret (no rng= tag, as made by
# the weak generator): the daemon must rotate it at boot, signed by the old key.
LEGACY_SEED=$(python3 -I -c "print('42' * 32)")
python3 -I scripts/mkmilkfs.py mkfs "$WORK/carol-fs.img" --size 64M --label Milk >/dev/null
python3 -I scripts/mkmilkfs.py mkdir "$WORK/carol-fs.img" /System/Settings/milk/drop 700 >/dev/null
printf 'ed25519-seed %s\n# Milk Drop identity of this machine: never copy it anywhere.\n' "$LEGACY_SEED" >"$WORK/legacy.secret"
printf 'carol\n' >"$WORK/legacy.name"
python3 -I scripts/mkmilkfs.py put "$WORK/carol-fs.img" /System/Settings/milk/drop/identity.secret "$WORK/legacy.secret" >/dev/null
python3 -I scripts/mkmilkfs.py put "$WORK/carol-fs.img" /System/Settings/milk/drop/name "$WORK/legacy.name" >/dev/null
dd if=/dev/zero of="$WORK/carol-disk.img" bs=1m count=0 seek=64 2>/dev/null
read -r PC PD < <(python3 -I -c "
import socket
ss = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(2)]
for s in ss: s.bind(('127.0.0.1', 0))
print(*[s.getsockname()[1] for s in ss])")
boot carol 02:6d:6b:64:0c:0c "$PC" "$PD"
QC=$!
PIDS+=("$QC")
python3 -I scripts/serial_drive.py "$WORK/carol.sock" "$WORK/carol.log" \
  "$P" 'send:sleep 1' "$P" 'send:drop id' "$P" 'send:audit --event user.drop' "$P" \
  'send:echo now > /sys/fs/milkfs/sync' "$P" "done:$WORK/carol.done" >"$WORK/carol.drive" 2>&1 &
DC=$!
PIDS+=("$DC")
for _ in $(seq 600); do [ -e "$WORK/carol.done" ] && break; sleep 0.1; done
[ -e "$WORK/carol.done" ] || fail "carol's boot timed out"
sleep 1.5
kill "$DC" 2>/dev/null || true
kill "$QC"
wait "$QC" 2>/dev/null || true
C="$WORK/carol.txt"
python3 -I scripts/console_clean.py "$WORK/carol.log" >"$C"
grep -E "^drop: carol|^name:|^fingerprint:|^rotated:|^rng:|user\.drop\.rotate" "$C"
check "$C" "the daemon rotated the legacy identity at boot" "drop: carol's identity came from the old weak RNG: rotated it \\([0-9a-f ]{19} -> [0-9a-f ]{19}, signed by the old key\\)"
check "$C" "drop id: rotated, now from the kernel CSPRNG" "^rotated: +from [0-9a-f ]{19} \\(signed by it\\)"
check "$C" "drop id: the new key's RNG" "^rng: +kernel CSPRNG"
check "$C" "audit: rotation because of the legacy RNG" "user\\.drop\\.rotate +name=carol old_fp=[0-9a-f]{16} new_fp=[0-9a-f]{16} key=[0-9a-f]{64} reason=legacy-rng"
python3 -I scripts/mkmilkfs.py cat "$WORK/carol-fs.img" /System/Settings/milk/drop/identity.secret >"$WORK/carol.secret"
grep -q "rng=kernel-csprng" "$WORK/carol.secret" || fail "carol's secret on disk isn't tagged rng=kernel-csprng"
grep -q "$LEGACY_SEED" "$WORK/carol.secret" && fail "carol's legacy seed is still on disk"
python3 -I scripts/mkmilkfs.py cat "$WORK/carol-fs.img" /System/Settings/milk/drop/rotation | grep -q "^ed25519-rotate " || fail "no rotation statement on carol's disk"
echo "  ok  on disk: the legacy seed is gone, the new one is tagged, the rotation statement is kept"

cleanup
trap - EXIT
rm -rf "$WORK"
echo "drop-test: PASS"
