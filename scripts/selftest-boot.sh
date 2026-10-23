#!/usr/bin/env bash
# The kernel's boot self-tests, all of them, in one boot: the one test that runs them (every
# other test boots without them, which is the default: kernel/src/selftest.rs). Boots headless
# with `opt/milkos/selftest=full`, a milkfs disk, the Mac share (read-only) and an offline NIC
# (restricted slirp: DHCP and the gateway only), and checks every self-test said it passed:
#   timer/exception ([selftest] ok), the shared crypto crate's vectors ([crypto] ... ok),
#   the CSPRNG's own check, memtest, the fs self-test (fs selftest: PASS), the scheduler's
#   ([sched-test] PASS), the process layer's ([proc-test] PASS) and the network stack's
#   ([net] selftest ok), with no FAIL line and no panic.
# Private images in a fresh mktemp directory; only the qemu started here is killed (by PID).
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-selftest.XXXXXX)"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
python3 scripts/mkmilkfs.py mkfs "$T/fs.img" --size 64M --label Milk >/dev/null
export MILK_FS="$T/fs.img" MILK_DISK="$T/disk.img"
unset MILK_SHARE_RW
source scripts/disk.sh; milk_disk_args
source scripts/share.sh; milk_share_args
source scripts/netdev.sh; MILK_HTTP_PORT= milk_net_args
SOCK="$T/serial.sock" LOG="$T/serial.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
  -global virtio-mmio.force-legacy=false "${DISK_ARGS[@]}" "${SHARE_ARGS[@]}" "${NET_ARGS[@]}" \
  -fw_cfg name=opt/milkos/selftest,string=full 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait@240:$P" "send:pause 1" "wait:$P" \
  "send:print SELFTEST""-DONE" "wait:SELFTEST-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 1500); do [ -e "$T/done" ] && break; kill -0 "$QPID" 2>/dev/null || break; sleep 0.2; done
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
[ -e "$T/done" ] || bad "the boot didn't reach a shell prompt"
L="$(tr -d '\r' < "$LOG")"
has() { echo "$L" | grep -Eaq -- "$1"; }
check() { # pattern what
  if has "$1"; then ok "$2: $(echo "$L" | grep -Ea -m1 -- "$1" | cut -c1-110)"; else bad "$2 (no line matching: $1)"; fi
}
has '^\[selftest\] full boot' && ok "the full self-tests were asked for" || bad "the kernel didn't see opt/milkos/selftest=full"
check '^\[selftest\] ok' "timer, exceptions and interrupts"
check '^\[crypto\] test vectors: .*x25519 8/8.*: ok$' "the crypto crate's vectors"
check '^\[rng\] ChaCha20 CSPRNG seeded from virtio-rng .*: ok$' "the CSPRNG"
check '^selftest: mmu ok' "memtest: the MMU"
check '^selftest: pmm ok' "memtest: the page allocator"
check '^selftest: heap ok' "memtest: the kernel heap"
check 'fs selftest: PASS \([0-9]+ checks\)' "the fs self-test"
check '^\[sched-test\] PASS' "the scheduler self-test"
check '^\[proc-test\] PASS' "the process-layer self-test"
check '^\[net\] dhcp: 10\.0\.2\.15 ' "DHCP on eth0"
check '^\[net\] selftest ok' "the network self-test"
has 'FAIL' && bad "a FAIL line: $(echo "$L" | grep -a -m3 'FAIL')" || ok "no FAIL line"
has 'KERNEL PANIC|kernel panic' && bad "a kernel panic" || ok "no panic"
if [ "$FAIL" = 0 ]; then echo "selftest-boot: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; else echo "selftest-boot: FAIL (log $LOG)"; exit 1; fi
