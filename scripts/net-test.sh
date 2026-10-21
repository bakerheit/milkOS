#!/usr/bin/env bash
# End-to-end network test: boot milkOS headless with qemu user-mode networking and check
#   - the kernel's boot self-test (DHCP, ping 10.0.2.2, loopback UDP/TCP, DNS policy, plus
#     TCP echo / DNS / refused against the helpers below, all on the Mac's 127.0.0.1),
#   - the guest's HTTP server on :80, from the Mac via hostfwd 127.0.0.1:$HTTP_PORT (curl),
#   - the socket syscalls: /bin/fetch typed into msh over the serial console, against a
#     host HTTP helper (10.0.2.2:PORT), the guest's own server, and a refused port.
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
CFG="echo=10.0.2.2:$P_echo dns=10.0.2.2:$P_dns name=echo.milk.test expect=10.0.2.2 refused=10.0.2.2:$P_refused"
echo "helpers: echo 127.0.0.1:$P_echo, dns 127.0.0.1:$P_dns, http 127.0.0.1:$P_http, refused port $P_refused (pid $SPID)"
SERIAL="$WORK/serial.sock"

qemu-system-aarch64 -M virt -accel hvf -cpu host -m 512M \
  -kernel "$KERNEL" -chardev "socket,id=s0,path=$SERIAL,server=on,wait=off" -serial chardev:s0 \
  -display none \
  -global virtio-mmio.force-legacy=false \
  -device virtio-keyboard-device -device virtio-tablet-device \
  -netdev "user,id=n0,hostfwd=tcp:127.0.0.1:$HTTP_PORT-:80" -device virtio-net-device,netdev=n0 \
  -fw_cfg "name=opt/milkos/nettest,string=$CFG" &
QPID=$!
echo "qemu pid $QPID, serial log $LOG"
# Console: log everything; once msh is up, run fetch three times.
python3 -I "$REPO/scripts/serial_drive.py" "$SERIAL" "$LOG" \
  'wait:\[net\] selftest' 'wait:msh:/\$ ' \
  "send:fetch http://10.0.2.2:$P_http/hello.txt" 'wait:fetch: [^\n]* ms' 'wait:msh:/\$ ' \
  'send:fetch -i http://127.0.0.1/' 'wait:fetch: [^\n]* ms' 'wait:msh:/\$ ' \
  "send:fetch 10.0.2.2:$P_refused" 'wait:fetch: connect' \
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

echo "---- msh: fetch (socket syscalls from user space) ----"
for _ in $(seq 300); do [ -e "$WORK/drive.done" ] && break; kill -0 "$DPID" 2>/dev/null || break; sleep 0.1; done
[ -e "$WORK/drive.done" ] || { cat "$WORK/drive.log" >&2; fail "msh/fetch session did not finish"; }
sleep 0.2
sed -n '/msh:\/\$ fetch/,$p' "$LOG" | tr -d '\r'
grep -q "hello from the Mac's loopback" "$LOG" || fail "fetch from host helper"
[ "$(grep -c 'fetch: 200 OK' "$LOG")" -ge 2 ] || fail "fetch: expected two 200 OK"
grep -q "fetch: connect 10.0.2.2:$P_refused: connection refused" "$LOG" || fail "fetch: refused port"

echo "---- host curl -> guest :80 (via 127.0.0.1:$HTTP_PORT) ----"
curl -sS -i --max-time 5 "http://127.0.0.1:$HTTP_PORT/" >"$WORK/index.txt" || fail "curl /"
cat "$WORK/index.txt"
grep -q "Hello from milkOS" "$WORK/index.txt" || fail "index page content"

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
echo "GET /nope: 404 ok"
echo "---- host helper log ----"
cat "$WORK/servers.log"
cleanup
trap - EXIT
rm -rf "$WORK"
echo "net-test: PASS"
