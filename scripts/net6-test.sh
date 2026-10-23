#!/usr/bin/env bash
# The v6-only boot: proves nothing in the network stack or the tools secretly depends on
# IPv4. Two milkOS VMs, alice and bob, both booted with IPv4 off (fw_cfg opt/milkos/net
# v6only=1); slirp itself runs without IPv4 (ipv4=off). eth1 links them over qemu dgram.
#   - alice's boot self-test: no IPv4 on eth0/eth1, IPv4 to 10.0.2.2 refused, every
#     loopback and IPv6 test, a TCP echo over IPv6 to the Mac's ::1 (as fec0::2);
#   - alice's tools over IPv6 only: network status / status --keys (IPv4 off), network ping
#     -6 to slirp, network get from the Mac's ::1 and from her own httpd ([::1] and
#     localhost), network connect -6 to the Mac's echo server, network connections, and
#     network ping 10.0.2.2 failing;
#   - Milk Drop between the two over IPv6 alone: discovery over ff02::fb, bob.local by
#     AAAA, a file sent by drop name, received and checked.
#
# Network safety: the host helpers bind ::1 and 127.0.0.1 only; alice is told to contact
# only fec0::2 (the Mac's loopback through slirp); bob's slirp is restricted; eth1 is two
# UDP ports on 127.0.0.1. Work files in a mktemp dir; only the qemus, drivers and helpers
# started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
WORK="$(mktemp -d /tmp/milk-net6-test.XXXXXX)"
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "net6-test: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

[ "${MILK_PREBUILT:-}" = 1 ] || cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

(cd "$WORK" && exec python3 -I "$REPO/scripts/net_test_servers.py" "$WORK/ports" >"$WORK/servers.log" 2>&1) &
PIDS+=($!)
for _ in $(seq 50); do [ -s "$WORK/ports" ] && break; sleep 0.1; done
[ -s "$WORK/ports" ] || fail "helper servers did not start"
# shellcheck disable=SC1090
eval "$(tr ' ' '\n' <"$WORK/ports" | sed 's/^/P_/')"
echo "helpers: echo [::1]:$P_echo6, http [::1]:$P_http6"
read -r PA PB < <(python3 -I -c "
import socket
ss = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(2)]
for s in ss: s.bind(('127.0.0.1', 0))
print(*[s.getsockname()[1] for s in ss])")

# boot NAME MAC LOCAL_PORT REMOTE_PORT SLIRP_OPTS [EXTRA_FWCFG...]
boot() {
  local name=$1 mac=$2 lp=$3 rp=$4 slirp=$5
  shift 5
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp 2 -m 512M \
    -kernel "$KERNEL" -chardev "socket,id=s0,path=$WORK/$name.sock,server=on,wait=on" -serial chardev:s0 \
    -display none -global virtio-mmio.force-legacy=false \
    -netdev "user,id=n0,ipv4=off$slirp" -device virtio-net-device,netdev=n0 \
    -netdev "dgram,id=d0,local.type=inet,local.host=127.0.0.1,local.port=$lp,remote.type=inet,remote.host=127.0.0.1,remote.port=$rp" \
    -device "virtio-net-device,netdev=d0,mac=$mac" \
    -fw_cfg "name=opt/milkos/net,string=v6only=1" -fw_cfg name=opt/milkos/selftest,string=net "$@" 2>>"$WORK/$name.qemu.err" &
  PIDS+=($!)
}
boot alice 02:6d:6b:64:0a:0a "$PA" "$PB" "" -fw_cfg "name=opt/milkos/nettest,string=echo6=[fec0::2]:$P_echo6"
boot bob 02:6d:6b:64:0b:0b "$PB" "$PA" ",restrict=on"
B6='fd6d:696c:6b00:1:6d:6bff:fe64:b0b'

P='wait:msh:[^\n]*\$ '
DONE_RE='drop: (sent|received|[^\n]*(refused|declined|isn.t|no peer|error|failed|mismatch|timed out|closed))'
python3 -I scripts/serial_drive.py "$WORK/bob.sock" "$WORK/bob.log" \
  'wait@240:\[net\] selftest' "$P" 'send:drop name bob' "$P" \
  'send:drop recv -y' 'wait:receiving as' "done:$WORK/b1" "wait:$DONE_RE" "$P" \
  'send:files list /Users/milk/Downloads' "$P" "done:$WORK/bob.done" >"$WORK/bob.drive" 2>&1 &
PIDS+=($!)
python3 -I scripts/serial_drive.py "$WORK/alice.sock" "$WORK/alice.log" \
  'wait@240:\[net\] selftest' "$P" 'send:drop name alice' "$P" \
  'send:network status' 'wait:sent +[0-9]+ packets' "$P" \
  'send:network status --keys' 'wait:drop_visible=' "$P" \
  'send:network ping -6 --count 2 --interval 200 fec0::2' 'wait:rtt min|packet loss' "$P" \
  'send:network ping --count 1 --timeout 500 10.0.2.2' "$P" \
  "send:network get http://[fec0::2]:$P_http6/hello.txt" 'wait:network get: [^\n]*' "$P" \
  'send:network get http://[::1]/about.txt' 'wait:network get: [^\n]*' "$P" \
  'send:network get http://localhost/about.txt' 'wait:network get: [^\n]*' "$P" \
  "send:network connect -6 fec0::2 $P_echo6" 'send:hello over IPv6 only' 'wait:hello over IPv6 only\r?\n\r?hello over IPv6 only' \
  'raw:\x04' "$P" \
  'send:network connections --listening' "$P" \
  "waitfile:$WORK/b1" \
  'send:drop list' "$P" \
  'send:network ping -6 -c 2 -i 200 bob.local' 'wait:rtt min|packet loss|no answer' "$P" \
  'send:print milk drop over ipv6 only > /tmp/v6.txt' "$P" \
  'send:drop send bob /tmp/v6.txt' "wait:$DONE_RE" "$P" \
  "done:$WORK/alice.done" >"$WORK/alice.drive" 2>&1 &
PIDS+=($!)
for _ in $(seq 1800); do
  [ -e "$WORK/alice.done" ] && [ -e "$WORK/bob.done" ] && break
  sleep 0.1
done
[ -e "$WORK/alice.done" ] || { tail -20 "$WORK/alice.drive" >&2; fail "alice's session did not finish"; }
[ -e "$WORK/bob.done" ] || { tail -20 "$WORK/bob.drive" >&2; fail "bob's session did not finish"; }
A="$WORK/alice.txt"; B="$WORK/bob.txt"
python3 -I scripts/console_clean.py "$WORK/alice.log" >"$A"
python3 -I scripts/console_clean.py "$WORK/bob.log" >"$B"
echo "---- alice ----"; sed -n '/msh:[^ ]*\$ drop name alice/,$p' "$A"
echo "---- bob ----"; sed -n '/msh:[^ ]*\$ drop name bob/,$p' "$B"
echo "---- checks ----"
check() { # file label regex
  if grep -Eq -- "$3" "$1"; then echo "  ok  $2"; else fail "$2 (no /$3/ in $(basename "$1"))"; fi
}
check "$A" "the kernel booted v6-only" "\\[net\\] v6-only boot: no IPv4 on eth0 or eth1"
check "$A" "self-test: IPv4 really off, loopback IPv4 still fine" "\\[net\\] v6-only: eth0 0\\.0\\.0\\.0, eth1 0\\.0\\.0\\.0 \\(no IPv4\\), connect 10\\.0\\.2\\.2: ENETUNREACH, IPv4 loopback ok"
check "$A" "self-test: SLAAC from slirp without IPv4" "\\[net\\] ipv6: eth0 fec0::[0-9a-f:]+/64 \\(SLAAC, router fe80::2\\), ping fec0::2"
check "$A" "self-test: TCP echo to the Mac's ::1" "\\[net\\] tcp echo via \\[fec0::2\\]:[0-9]+: 512 KiB round trip"
check "$A" "self-test: everything else passed" "\\[net\\] selftest ok"
grep -q "\\[net\\] FAIL" "$A" && fail "a self-test failed: $(grep '\[net\] FAIL' "$A" | head -1)"
check "$A" "network status: no IPv4 on eth0, its IPv6 addresses" "ipv6       fec0::5054:ff:fe12:3456/64  global"
check "$A" "network status: IPv4 shown as off" "^  ipv4       off \\(IPv6 only\\)"
check "$A" "network status --keys: IPv4 off on eth0 and eth1" "^iface\\.eth1\\.ipv4=off$"
check "$A" "network status --keys: no DHCP on eth0" "^iface\\.eth0\\.ipv4=off$"
check "$A" "network ping -6 to slirp" "2 packets transmitted, 2 received, 0% packet loss"
check "$A" "IPv4 ping refused" "network ping: couldn't send: .*(unreachable|101)"
check "$A" "network get from the Mac's ::1" "hello from the Mac's loopback, via qemu slirp, to milkOS network get"
check "$A" "network get from her own httpd over [::1]" "network get: 200 OK, [0-9]+ bytes from \\[::1\\]:80"
[ "$(grep -c 'served by /System/Services/httpd' "$A")" -ge 2 ] || fail "network get localhost: no page (localhost must reach ::1)"
echo "  ok  network get http://localhost/ reaches httpd over ::1"
[ "$(grep -c 'hello over IPv6 only' "$A")" -ge 2 ] || fail "network connect -6: no echo from the Mac's [::1] server"
echo "  ok  network connect -6 to the Mac's echo server"
check "$A" "httpd listens on [::]:80" "tcp6 .* \\[::\\]:80 .* LISTEN"
check "$A" "drop list finds bob over IPv6 alone" "^bob +$B6 +[0-9a-f ]{19} +yes +new "
check "$A" "network ping -6 bob.local (AAAA over ff02::fb)" "bytes from $B6: icmp_seq=1"
check "$A" "drop send bob over IPv6" "drop: sent v6\\.txt to bob"
check "$B" "bob's receiver: only the IPv6 address" "drop: receiving as bob \\(fingerprint [0-9a-f ]{19}\\) on \\[$B6\\]:5360;"
check "$B" "bob got the file" "drop: received /Users/milk/Downloads/v6\\.txt"
cleanup
trap - EXIT
[ "${MILK_KEEP:-}" = 1 ] || rm -rf "$WORK" || true
echo "net6-test: PASS"
