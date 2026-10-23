#!/usr/bin/env bash
# End-to-end network test: boot milkOS headless with qemu user-mode networking and check
#   - the kernel's boot self-test (DHCP, ping 10.0.2.2, loopback UDP/TCP, DNS policy, plus
#     TCP echo / DNS / refused against the helpers below, all on the Mac's 127.0.0.1),
#   - the network command typed into msh over the serial console: network status, ping,
#     connections, get (host helper, the guest's own servers, a refused port), connect (to
#     the host echo server) and listen (reached from the Mac), name, status --keys,
#   - from the Mac (curl): the userland /System/Services/httpd on guest :80 via 127.0.0.1:$HTTP_PORT,
#     and the kernel's httpd (enabled for the test on guest :8080) via a free port.
#
#   scripts/net-test.sh            HTTP_PORT=8088 by default (override: HTTP_PORT=9099 ...)
#
# Network safety: everything binds to the loopback only (127.0.0.1, and ::1 for the IPv6
# echo server); the guest is told (fw_cfg opt/milkos/nettest) to contact only 10.0.2.2 and
# fec0::2 = the Mac's loopback. Work files go to a
# fresh mktemp dir. Only the qemu and python processes started here are killed, by PID.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO="$PWD"
HTTP_PORT="${HTTP_PORT:-8088}"
# Short path: the serial unix socket lives here (macOS caps those at 104 bytes).
WORK="$(mktemp -d /tmp/milk-net-test.XXXXXX)"
LOG="$WORK/serial.log"
QPID=""
SPID=""
DPID=""
cleanup() {
  [ -n "$DPID" ] && kill "$DPID" 2>/dev/null || true
  [ -n "$QPID" ] && kill "$QPID" 2>/dev/null || true
  [ -n "$SPID" ] && kill "$SPID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT
fail() { echo "net-test: FAIL: $*" >&2; echo "(logs in $WORK)" >&2; exit 1; }

cargo build ${RELEASE:+--release} -q
PROFILE=debug; [ -n "${RELEASE:-}" ] && PROFILE=release
ELF="target/aarch64-unknown-none-softfloat/$PROFILE/milkos-kernel"
KERNEL="$ELF.bin"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$KERNEL"

# Free? (SO_REUSEADDR like qemu's hostfwd, so TIME_WAIT leftovers of a previous run are fine.)
if python3 -c "import socket,sys; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', int(sys.argv[1]))); s.listen()" "$HTTP_PORT" 2>/dev/null; then :; else
  fail "127.0.0.1:$HTTP_PORT is in use (set HTTP_PORT=...)"
fi

# Host helpers (echo + test DNS), from the scratch dir, isolated python.
(cd "$WORK" && exec python3 -I "$REPO/scripts/net_test_servers.py" "$WORK/ports" >"$WORK/servers.log" 2>&1) &
SPID=$!
for _ in $(seq 50); do [ -s "$WORK/ports" ] && break; sleep 0.1; done
[ -s "$WORK/ports" ] || fail "helper servers did not start"
# shellcheck disable=SC1090
eval "$(tr ' ' '\n' <"$WORK/ports" | sed 's/^/P_/')"
CFG="echo=10.0.2.2:$P_echo echo6=[fec0::2]:$P_echo6 dns=10.0.2.2:$P_dns name=echo.milk.test expect=10.0.2.2 refused=10.0.2.2:$P_refused khttpd=8080 lossy3=1"
# Two more free host ports: the kernel httpd (guest :8080) and network listen (guest :7777).
read -r KH_PORT NC_PORT < <(python3 -I -c "
import socket
ss = [socket.socket() for _ in range(2)]
for s in ss: s.bind(('127.0.0.1', 0))
print(*[s.getsockname()[1] for s in ss])")
echo "helpers: echo 127.0.0.1:$P_echo and [::1]:$P_echo6, dns 127.0.0.1:$P_dns, http 127.0.0.1:$P_http, refused port $P_refused (pid $SPID)"
SERIAL="$WORK/serial.sock"

qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  -netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$HTTP_PORT-:80,hostfwd=tcp:127.0.0.1:$KH_PORT-:8080,hostfwd=tcp:127.0.0.1:$NC_PORT-:7777" \
  -device virtio-net-device,netdev=n0 \
  -fw_cfg "name=opt/milkos/nettest,string=$CFG" -fw_cfg name=opt/milkos/selftest,string=net &
QPID=$!
echo "qemu pid $QPID, serial log $LOG"
# Console: log everything; once msh is up, run the network tools.
P='wait:msh:/\$ '
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG" \
  'wait@240:\[net\] selftest' "$P" \
  'send:network status' 'wait:sent +[0-9]+ packets' "$P" \
  'send:network ping --count 3 --interval 200 10.0.2.2' 'wait:rtt min' "$P" \
  'send:network ping -6 -c 2 --interval 200 fec0::2' 'wait:rtt min' "$P" \
  'send:network get --headers http://[::1]/' 'wait:network get: [^\n]* ms' "$P" \
  "send:network get http://10.0.2.2:$P_http/hello.txt" 'wait:network get: [^\n]* ms' "$P" \
  'send:network get -i http://127.0.0.1/' 'wait:network get: [^\n]* ms' "$P" \
  "send:network get 10.0.2.2:$P_refused" 'wait:network get: connect' "$P" \
  "send:network connect 10.0.2.2 $P_echo" 'send:hello through connect' 'wait:hello through connect\r?\n\r?hello through connect' \
  'raw:\x04' "$P" \
  "send:network connect --ipv6 fec0::2 $P_echo6" 'send:hello over IPv6' 'wait:hello over IPv6\r?\n\r?hello over IPv6' \
  'raw:\x04' "$P" \
  'send:network listen 7777' 'wait:listening on tcp' "done:$WORK/nc.listening" 'wait:from the mac' "$P" \
  'send:network connections' 'wait:LISTEN' "$P" \
  'send:network connections --ipv6 --listening' 'wait:LISTEN' "$P" \
  'send:network' 'wait:milk drop' "$P" \
  'send:network status --keys' 'wait:drop_visible=' "$P" \
  'send:network name milkbox' "$P" \
  'send:system info' "$P" \
  'send:text show /proc/net/hostname' "$P" \
  'send:network name bad_name!' "$P" \
  'send:processes watch --plain --frames 2 --every 1' 'wait:COMMAND[^$]*COMMAND' "$P" \
  'send:system uptime' 'wait:15 minutes' "$P" \
  'send:text show /proc/loadavg' "$P" \
  "done:$WORK/drive.done" >"$WORK/drive.log" 2>&1 &
DPID=$!

for _ in $(seq 600); do
  grep -q '\[net\] selftest' "$LOG" 2>/dev/null && break
  kill -0 "$QPID" 2>/dev/null || fail "qemu exited"
  sleep 0.1
done
echo "---- guest ----"
awk '/^\[(net|virtio-net)\]/ { print; inproc = /\/proc\/net/; next } inproc && /^    / { print; next } { inproc = 0 }' "$LOG"
grep -q '\[net\] selftest ok' "$LOG" || fail "guest self-test did not pass"
for want in 'tcp lossy loopback 1/7: 256 KiB intact' 'tcp lossy loopback 1/3: 32 KiB intact' \
  'tcp TIME_WAIT: parked' 'ip fragments: 20000-byte UDP datagram' \
  'ipv6: eth0 fec0::[0-9a-f:]+/64 \(SLAAC, router fe80::2\), ping fec0::2' 'ipv6 extension headers: .* refused \(1\)' \
  'dual-stack: .* V6ONLY ok, udp6 ok' 'tcp echo via \[fec0::2\]:[0-9]+: 512 KiB round trip' \
  'ipv6 fragments: .* overlap poisoned \(1\), header chain split refused, 1 reassembled' \
  'path mtu: IPv6 path 1280 -> tcp mss 1220 .* IPv4 path 1000 -> tcp mss 960' \
  'mld: ff02::1:3 on eth0 dropped before the join, delivered while joined, dropped after leaving'; do
  grep -Eaq "\[net\] $want" "$LOG" || fail "self-test line missing: $want"
done
grep -Ea '\[net\] (tcp lossy|ipv6|ip fragments|tcp TIME_WAIT|dual-stack|tcp echo|path mtu|mld)' "$LOG" | tr -d '\r' | sed 's/^/  /'

echo "---- msh: network tools (socket syscalls from user space) ----"
for _ in $(seq 300); do [ -e "$WORK/nc.listening" ] && break; kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
[ -e "$WORK/nc.listening" ] || { cat "$WORK/drive.log" >&2; fail "msh session stalled before network listen"; }
python3 -I -c "
import socket, sys
s = socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=5)
s.sendall(b'from the mac, to network listen\n')
s.close()" "$NC_PORT" || fail "connect to network listen"
for _ in $(seq 300); do [ -e "$WORK/drive.done" ] && break; kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
[ -e "$WORK/drive.done" ] || { cat "$WORK/drive.log" >&2; fail "msh session did not finish"; }
sleep 0.2
python3 -I scripts/console_clean.py "$LOG" | sed -n '/msh:\/\$ network status/,$p'
grep -q "ipv4       10.0.2.15/24  netmask 255.255.255.0" "$LOG" || fail "network status: eth0 ipv4"
grep -q "3 packets transmitted, 3 received, 0% packet loss" "$LOG" || fail "network ping"
grep -q "hello from the Mac's loopback" "$LOG" || fail "network get from host helper"
[ "$(grep -c 'network get: 200 OK' "$LOG")" -ge 2 ] || fail "network get: expected two 200 OK"
grep -q "network get: connect 10.0.2.2:$P_refused: connection refused" "$LOG" || fail "network get: refused port"
grep -q "network listen: connection from 10.0.2.2:" "$LOG" || fail "network listen: no connection"
# Typed once, echoed back once by the host's [::1] server (the drive waited for both).
[ "$(grep -c 'hello over IPv6' "$LOG")" -ge 2 ] || fail "network connect --ipv6 fec0::2: no echo from the host's [::1] server"
grep -Eq "tcp6 .* \[::\]:80 .* LISTEN" <(tr -d '\r' <"$LOG") || fail "network connections: no [::]:80 listener (httpd, dual-stack)"
grep -Eq "ipv6       fe80::5054:ff:fe12:3456/64  link" "$LOG" || fail "network status: no eth0 link-local ipv6"
grep -Eq "ipv6       fec0::5054:ff:fe12:3456/64  global" "$LOG" || fail "network status: no eth0 SLAAC ipv6"
grep -q "2 packets transmitted, 2 received, 0% packet loss" "$LOG" || fail "network ping -6 fec0::2"
grep -q "bytes from fec0::2: icmp_seq=1" "$LOG" || fail "network ping -6: no reply line from fec0::2"
[ "$(grep -c 'network get: 200 OK' "$LOG")" -ge 3 ] || fail "network get http://[::1]/: expected a third 200 OK"
T0="$WORK/console0.txt"; python3 -I scripts/console_clean.py "$LOG" >"$T0"
grep -q "^iface.eth0.ipv4=10.0.2.15/24$" "$T0" || fail "network status --keys: eth0 ipv4"
grep -q "^iface.eth0.ipv6=fe80::5054:ff:fe12:3456/64,link fec0::5054:ff:fe12:3456/64,global$" "$T0" || fail "network status --keys: eth0 ipv6"
grep -q "^iface.eth0.dhcp=Bound$" "$T0" || fail "network status --keys: eth0 dhcp"
grep -q "^hostname=milk$" "$T0" || fail "network status --keys: hostname"
grep -Eq "^hostname +milk$" "$T0" || fail "network status: hostname"
grep -q "network: the hostname is now milkbox (was milk)" "$T0" || fail "network name milkbox"
grep -Eqx 'name +milkbox' "$T0" && grep -qx 'milkbox' "$T0" || fail "system info / /proc/net/hostname after network name"
grep -q "network name: bad_name! can't be a hostname (1-63 letters, digits and '-')" "$T0" || fail "network name: a bad hostname was not refused"
echo "network status / --keys (interfaces, v4 + v6, DHCP, hostname), network name (system info, /proc/net/hostname), bad name refused: ok"
echo "network status ipv6, network ping -6 fec0::2, network get http://[::1]/ (httpd over IPv6), network connections tcp6 [::]:80: ok"
T="$WORK/console.txt"; python3 -I scripts/console_clean.py "$LOG" >"$T"
grep -Eq '^cpu0 +\[[| ]+\] +[0-9]+%' "$T" || fail "processes watch: no cpu0 bar"
[ "$(grep -c '^processes watch - .*load ' "$T")" -ge 2 ] || fail "processes watch --plain --frames 2: expected two frames"
grep -Eq '^ +1 +0 S .* init$' "$T" || fail "processes watch: init not in the process list"
grep -Eq '^up .* \(since [A-Z][a-z]{2} +[0-9]+ [0-9]{2}:[0-9]{2}\), [0-9]+ processes, load [0-9.]+ [0-9.]+ [0-9.]+ \(1, 5, 15 minutes\)$' "$T" || fail "system uptime: no load average"
grep -Eq '^[0-9]+\.[0-9]{2} [0-9]+\.[0-9]{2} [0-9]+\.[0-9]{2} [0-9]+/[0-9]+ [0-9]+$' "$T" || fail "/proc/loadavg format"
echo "processes watch --plain (2 frames, cpu bars, process list), system uptime load average, /proc/loadavg: ok"

echo "---- host curl -> userland httpd on guest :80 (via 127.0.0.1:$HTTP_PORT) ----"
curl -sS -i --max-time 5 "http://127.0.0.1:$HTTP_PORT/" >"$WORK/index.txt" || fail "curl /"
tr -d '\r' <"$WORK/index.txt"
grep -q "Hello from milkOS userland" "$WORK/index.txt" || fail "index page content"
curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/about.txt" | grep -q "served by /System/Services/httpd" || fail "about.txt"
curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/notes/" | grep -q 'href="hello.txt"' || fail "directory listing"
for t in "/missing 404" "/../etc/motd 403" "/notes 301"; do
  set -- $t
  code=$(curl -sS --path-as-is -o /dev/null -w '%{http_code}' --max-time 5 "http://127.0.0.1:$HTTP_PORT$1")
  [ "$code" = "$2" ] || fail "GET $1: expected $2, got $code"
done
echo "userland httpd: /, /about.txt, /notes/ listing, 404, 403 (..), 301 ok"
curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/status" >"$WORK/status.html" || fail "curl /status"
for want in '<title>milkOS status</title>' '<h2>CPU</h2>' 'cpu0  \[' '<h2>Memory</h2>' '<h2>Recent audit events</h2>' '<b>exec</b> /Commands/'; do
  grep -q "$want" "$WORK/status.html" || { cat "$WORK/status.html"; fail "/status: no $want"; }
done
echo "userland httpd /status: CPU, memory, network, recent audit events ok ($(wc -c <"$WORK/status.html") bytes)"

echo "---- host curl -> kernel httpd on guest :8080 (via 127.0.0.1:$KH_PORT) ----"
HTTP_PORT=$KH_PORT
curl -sS --max-time 20 -o "$WORK/big.bin" -w 'GET /big: %{http_code}, %{size_download} bytes in %{time_total} s (%{speed_download} B/s)\n' \
  "http://127.0.0.1:$HTTP_PORT/big" || fail "curl /big"
python3 -I - "$WORK/big.bin" <<'PY' || fail "/big content mismatch"
import sys
data = open(sys.argv[1], "rb").read()
tail = b" milkOS big page ...............\n"
want = b"".join(b"%08d" % k + tail[:23] + b"\n" for k in range(len(data) // 32))
assert len(data) == 1 << 20, len(data)
assert data == want, "content differs"
print("GET /big: content verified (1 MiB, 32768 numbered lines)")
PY

# Four at once (the server takes 8 connections).
pids=()
for k in 1 2 3 4; do
  curl -sS --max-time 20 -o "$WORK/par$k.bin" "http://127.0.0.1:$HTTP_PORT/big" &
  pids+=($!)
done
ok=1
for p in "${pids[@]}"; do wait "$p" || ok=0; done
for k in 1 2 3 4; do cmp -s "$WORK/big.bin" "$WORK/par$k.bin" || ok=0; done
[ "$ok" = 1 ] || fail "parallel /big downloads"
echo "4 parallel GET /big: all identical"

curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/net" >"$WORK/net.txt" || fail "curl /net"
echo "---- GET /net (guest /proc/net) ----"
cat "$WORK/net.txt"
code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 5 "http://127.0.0.1:$HTTP_PORT/nope")
[ "$code" = 404 ] || fail "expected 404, got $code"
echo "GET /nope (kernel httpd): 404 ok"
echo "---- host helper log ----"
cat "$WORK/servers.log"
cleanup
trap - EXIT
rm -rf "$WORK"
echo "net-test: PASS"
