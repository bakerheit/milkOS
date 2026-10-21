#!/usr/bin/env bash
# End-to-end network test: boot milkOS headless with qemu user-mode networking and check
#   - the kernel's boot self-test (DHCP, ping 10.0.2.2, loopback UDP/TCP, DNS policy, plus
#     TCP echo / DNS / refused against the helpers below, all on the Mac's 127.0.0.1),
#   - the network tools typed into msh over the serial console: ifconfig, ping, netstat,
#     fetch (host helper, the guest's own servers, a refused port), nc (to the host echo
#     server, and nc -l reached from the Mac),
#   - from the Mac (curl): the userland /bin/httpd on guest :80 via 127.0.0.1:$HTTP_PORT,
#     and the kernel's httpd (enabled for the test on guest :8080) via a free port.
#
#   scripts/net-test.sh            HTTP_PORT=8088 by default (override: HTTP_PORT=9099 ...)
#
# Network safety: everything binds to 127.0.0.1 only; the guest is told (fw_cfg
# opt/milkos/nettest) to contact only 10.0.2.2 = the Mac's loopback. Work files go to a
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
CFG="echo=10.0.2.2:$P_echo dns=10.0.2.2:$P_dns name=echo.milk.test expect=10.0.2.2 refused=10.0.2.2:$P_refused khttpd=8080 lossy3=1"
# Two more free host ports: the kernel httpd (guest :8080) and nc -l (guest :7777).
read -r KH_PORT NC_PORT < <(python3 -I -c "
import socket
ss = [socket.socket() for _ in range(2)]
for s in ss: s.bind(('127.0.0.1', 0))
print(*[s.getsockname()[1] for s in ss])")
echo "helpers: echo 127.0.0.1:$P_echo, dns 127.0.0.1:$P_dns, http 127.0.0.1:$P_http, refused port $P_refused (pid $SPID)"
SERIAL="$WORK/serial.sock"

qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  -netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$HTTP_PORT-:80,hostfwd=tcp:127.0.0.1:$KH_PORT-:8080,hostfwd=tcp:127.0.0.1:$NC_PORT-:7777" \
  -device virtio-net-device,netdev=n0 \
  -fw_cfg "name=opt/milkos/nettest,string=$CFG" &
QPID=$!
echo "qemu pid $QPID, serial log $LOG"
# Console: log everything; once msh is up, run the network tools.
P='wait:msh:/\$ '
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG" \
  'wait@240:\[net\] selftest' "$P" \
  'send:ifconfig' 'wait:RX packets' "$P" \
  'send:ping -c 3 -i 200 10.0.2.2' 'wait:rtt min' "$P" \
  'send:ping -6 -c 2 -i 200 fec0::2' 'wait:rtt min' "$P" \
  'send:fetch -i http://[::1]/' 'wait:fetch: [^\n]* ms' "$P" \
  "send:fetch http://10.0.2.2:$P_http/hello.txt" 'wait:fetch: [^\n]* ms' "$P" \
  'send:fetch -i http://127.0.0.1/' 'wait:fetch: [^\n]* ms' "$P" \
  "send:fetch 10.0.2.2:$P_refused" 'wait:fetch: connect' "$P" \
  "send:nc 10.0.2.2 $P_echo" 'send:hello through nc' 'wait:hello through nc\r?\n\r?hello through nc' \
  'raw:\x04' "$P" \
  'send:nc -l 7777' 'wait:listening on tcp' "done:$WORK/nc.listening" 'wait:from the mac' "$P" \
  'send:netstat' 'wait:LISTEN' "$P" \
  'send:netstat -6 -l' 'wait:LISTEN' "$P" \
  'send:top -b -n 2 -d 1' 'wait:COMMAND[^$]*COMMAND' "$P" \
  'send:uptime' 'wait:load average' "$P" \
  'send:cat /proc/loadavg' "$P" \
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
  'dual-stack: .* V6ONLY ok, udp6 ok'; do
  grep -Eaq "\[net\] $want" "$LOG" || fail "self-test line missing: $want"
done
grep -Ea '\[net\] (tcp lossy|ipv6|ip fragments|tcp TIME_WAIT|dual-stack)' "$LOG" | tr -d '\r' | sed 's/^/  /'

echo "---- msh: network tools (socket syscalls from user space) ----"
for _ in $(seq 300); do [ -e "$WORK/nc.listening" ] && break; kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
[ -e "$WORK/nc.listening" ] || { cat "$WORK/drive.log" >&2; fail "msh session stalled before nc -l"; }
python3 -I -c "
import socket, sys
s = socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=5)
s.sendall(b'from the mac, to nc -l\n')
s.close()" "$NC_PORT" || fail "connect to nc -l"
for _ in $(seq 300); do [ -e "$WORK/drive.done" ] && break; kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
[ -e "$WORK/drive.done" ] || { cat "$WORK/drive.log" >&2; fail "msh session did not finish"; }
sleep 0.2
python3 -I scripts/console_clean.py "$LOG" | sed -n '/msh:\/\$ ifconfig/,$p'
grep -q "inet 10.0.2.15  netmask 255.255.255.0" "$LOG" || fail "ifconfig"
grep -q "3 packets transmitted, 3 received, 0% packet loss" "$LOG" || fail "ping"
grep -q "hello from the Mac's loopback" "$LOG" || fail "fetch from host helper"
[ "$(grep -c 'fetch: 200 OK' "$LOG")" -ge 2 ] || fail "fetch: expected two 200 OK"
grep -q "fetch: connect 10.0.2.2:$P_refused: connection refused" "$LOG" || fail "fetch: refused port"
grep -q "nc: connection from 10.0.2.2:" "$LOG" || fail "nc -l: no connection"
grep -Eq "tcp6 .* \[::\]:80 .* LISTEN" <(tr -d '\r' <"$LOG") || fail "netstat: no [::]:80 listener (httpd, dual-stack)"
grep -Eq "inet6 fe80::5054:ff:fe12:3456  prefixlen 64  scopeid 0x20<link>" "$LOG" || fail "ifconfig: no eth0 link-local inet6"
grep -Eq "inet6 fec0::5054:ff:fe12:3456  prefixlen 64  scopeid 0x0<global>" "$LOG" || fail "ifconfig: no eth0 SLAAC inet6"
grep -q "2 packets transmitted, 2 received, 0% packet loss" "$LOG" || fail "ping -6 fec0::2"
grep -q "bytes from fec0::2: icmp_seq=1" "$LOG" || fail "ping -6: no reply line from fec0::2"
[ "$(grep -c 'fetch: 200 OK' "$LOG")" -ge 3 ] || fail "fetch http://[::1]/: expected a third 200 OK"
echo "ifconfig inet6, ping -6 fec0::2, fetch http://[::1]/ (httpd over IPv6), netstat tcp6 [::]:80: ok"
T="$WORK/console.txt"; python3 -I scripts/console_clean.py "$LOG" >"$T"
grep -Eq '^cpu0 +\[[| ]+\] +[0-9]+%' "$T" || fail "top: no cpu0 bar"
[ "$(grep -c '^top - .*load ' "$T")" -ge 2 ] || fail "top -b -n 2: expected two frames"
grep -Eq '^ +1 +0 S .* init$' "$T" || fail "top: init not in the process list"
grep -Eq 'up [0-9:]+, [0-9]+ processes, load average: [0-9.]+, [0-9.]+, [0-9.]+' "$T" || fail "uptime: no load average"
grep -Eq '^[0-9]+\.[0-9]{2} [0-9]+\.[0-9]{2} [0-9]+\.[0-9]{2} [0-9]+/[0-9]+ [0-9]+$' "$T" || fail "/proc/loadavg format"
echo "top -b (2 frames, cpu bars, process list), uptime load average, /proc/loadavg: ok"

echo "---- host curl -> userland httpd on guest :80 (via 127.0.0.1:$HTTP_PORT) ----"
curl -sS -i --max-time 5 "http://127.0.0.1:$HTTP_PORT/" >"$WORK/index.txt" || fail "curl /"
tr -d '\r' <"$WORK/index.txt"
grep -q "Hello from milkOS userland" "$WORK/index.txt" || fail "index page content"
curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/about.txt" | grep -q "served by /bin/httpd" || fail "about.txt"
curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/notes/" | grep -q 'href="hello.txt"' || fail "directory listing"
for t in "/missing 404" "/../etc/motd 403" "/notes 301"; do
  set -- $t
  code=$(curl -sS --path-as-is -o /dev/null -w '%{http_code}' --max-time 5 "http://127.0.0.1:$HTTP_PORT$1")
  [ "$code" = "$2" ] || fail "GET $1: expected $2, got $code"
done
echo "userland httpd: /, /about.txt, /notes/ listing, 404, 403 (..), 301 ok"
curl -sS --max-time 5 "http://127.0.0.1:$HTTP_PORT/status" >"$WORK/status.html" || fail "curl /status"
for want in '<title>milkOS status</title>' '<h2>CPU</h2>' 'cpu0  \[' '<h2>Memory</h2>' '<h2>Recent audit events</h2>' '<b>exec</b> /bin/'; do
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
