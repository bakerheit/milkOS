#!/usr/bin/env bash
# statfs/fstatfs and `disks list` against the Mac's own numbers (private images in a fresh /tmp
# directory; the Mac share read-only; only the qemu started here is killed, by PID).
#   milkfs (64 MiB image): size = the image; used grows by what's written (text show /Commands/*); a
#          snapshot reports read-only; disks list /Users/milk = the volume (bind mounts not repeated)
#   FAT32 (64 MiB, mkfat.sh): size and free space = the image's BPB and FSInfo
#   9p (/Volumes/Mac): size = what the Mac says about the share's disk (os.statvfs)
#   tmpfs has a size; proc, sysfs, devfs show only with --all, all zero
#   MILK_TABLE=1 disks list renders a table; disks list of a missing path fails with a message
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d /tmp/milk-dftest.XXXXXX)"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"

scripts/mkfat.sh "$T/fat.img" 64 DFFAT >/dev/null
python3 scripts/mkmilkfs.py mkfs "$T/fs.img" --size 64M --label Milk >/dev/null
export MILK_FS="$T/fs.img" MILK_DISK="$T/disk.img" MILK_FAT="$T/fat.img"
unset MILK_SHARE_RW
source scripts/disk.sh; milk_disk_args
source scripts/share.sh; milk_share_args
SHARE_DIR="$(cd "$(git rev-parse --git-common-dir)/.." && pwd)/share"

SOCK="$T/serial.sock" LOG="$T/serial.log"
qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -global virtio-mmio.force-legacy=false "${DISK_ARGS[@]}" "${SHARE_ARGS[@]}" 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
P='msh:[^\n]*\$ '
# A first command that only lets the boot finish: services still starting print their
# banners over the first prompt, so a real command typed there wouldn't sit on its own line.
args=("wait:$P" "send:pause 1" "wait:$P")
for c in \
  "disks list" "disks list --all" "disks list --human" "MILK_TABLE=1 disks list" \
  "disks list /Volumes/Milk /Users/milk /Volumes/DFFAT /Volumes/Mac /tmp" \
  "text show /Commands/* > /Volumes/Milk/big.bin" "print now > /System/Kernel/fs/milkfs/sync" "text count --bytes /Volumes/Milk/big.bin" "disks list /Volumes/Milk" \
  "disks snapshot dfsnap" "disks list /Volumes/Milk/.snapshots/dfsnap" \
  "disks list /no/such/path"; do
  args+=("send:$c" "wait:$P")
done
python3 scripts/serial_drive.py "$SOCK" "$LOG" "${args[@]}" "send:print DF""-DONE" "wait:DF-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 900); do [ -e "$T/done" ] && break; sleep 0.2; done
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
L="$(tr -d '\r' < "$LOG")"
# The output of one command: the lines after "$ CMD" up to the next prompt.
out() { echo "$L" | awk -v c="\$ $1" 'index($0, c) && substr($0, index($0, c)) == c {on=1; next} on && /^msh:/ {exit} on'; }
col() { awk -v m="$1" -v n="$2" '$NF == m || ($(NF-1) == m && $NF == "(ro)") {print $n; exit}'; }

DF="$(out "disks list")"
echo "$DF" | sed 's/^/    /'
fs_kb=$(( $(stat -f %z "$T/fs.img") / 1024 ))
[ "$(echo "$DF" | col /Volumes/Milk 2)" = "$fs_kb" ] && ok "milkfs size = the image ($fs_kb K)" || bad "milkfs size: $(echo "$DF" | col /Volumes/Milk 2), want $fs_kb"
[ "$(echo "$DF" | col /Volumes/Milk 1)" = milkfs ] && ok "milkfs type" || bad "milkfs type"
echo "$DF" | grep -q " /Users/milk$" && bad "disks list repeats the /Users/milk bind" || ok "the /Users/milk bind isn't repeated"
echo "$DF" | grep -qE " /(proc|System/Kernel|dev)$" && bad "proc/sys/dev listed without -a" || ok "proc, sysfs, devfs hidden without -a"
read -r fat_kb fat_free_kb < <(python3 - "$T/fat.img" <<'EOF'
import struct, sys
b = open(sys.argv[1], 'rb').read(1024)
bps, spc, rsvd, nfat = struct.unpack_from('<HBHB', b, 11)
tot = struct.unpack_from('<I', b, 32)[0]; fsz = struct.unpack_from('<I', b, 36)[0]
ncl = (tot - rsvd - nfat * fsz) // spc
free = struct.unpack_from('<I', b, bps + 488)[0]
print(ncl * spc * bps // 1024, free * spc * bps // 1024)
EOF
)
[ "$(echo "$DF" | col /Volumes/DFFAT 2)" = "$fat_kb" ] && ok "FAT size = the BPB's data clusters ($fat_kb K)" || bad "FAT size: $(echo "$DF" | col /Volumes/DFFAT 2), want $fat_kb"
[ "$(echo "$DF" | col /Volumes/DFFAT 4)" = "$fat_free_kb" ] && ok "FAT free = FSInfo ($fat_free_kb K)" || bad "FAT free: $(echo "$DF" | col /Volumes/DFFAT 4), want $fat_free_kb"
mac_kb=$(python3 -c "import os,sys; s=os.statvfs(sys.argv[1]); print(s.f_blocks*s.f_frsize//1024)" "$SHARE_DIR")
got=$(echo "$DF" | col /Volumes/Mac 2)
[ -n "$got" ] && python3 -c "import sys; a,b=int(sys.argv[1]),int(sys.argv[2]); sys.exit(0 if abs(a-b) <= b//100 else 1)" "$got" "$mac_kb" \
  && ok "9p size = the Mac's disk ($got K)" || bad "9p size: $got, Mac says $mac_kb"
echo "$DF" | grep -q "^9p .* /Volumes/Mac (ro)$" && ok "the read-only share says (ro)" || bad "share not shown read-only"
P1="$(out "disks list /Volumes/Milk /Users/milk /Volumes/DFFAT /Volumes/Mac /tmp")"
tmp=$(echo "$P1" | col /tmp 2)
[ -n "$tmp" ] && [ "$tmp" -gt 0 ] && ok "tmpfs /tmp has a size ($tmp K)" || bad "tmpfs size"
A="$(out "disks list --all")"
echo "$A" | grep -qE "^proc +0 +0 +0 +- +/proc$" && echo "$A" | grep -qE "^sysfs +0 .* /System/Kernel$" && echo "$A" | grep -qE "^devfs +0 .* /dev$" \
  && ok "disks list --all: proc, sysfs, devfs with zero sizes" || bad "disks list --all zero-size file systems"
echo "$A" | grep -q "^milkfs .* /Users/milk$" && ok "disks list --all shows the bind too" || bad "disks list --all bind"
out "disks list --human" | grep -q "^milkfs .*M .* /Volumes/Milk$" && ok "disks list --human: human sizes" || bad "disks list --human"
T1="$(out "MILK_TABLE=1 disks list")"
echo "$T1" | grep -q "use_pct" && echo "$T1" | grep -q "/Volumes/Milk" && ok "MILK_TABLE=1: a table with use_pct" || bad "table output: $(echo "$T1" | head -3)"
# Same file system: same size; used may move a little between the two calls (the
# search index or Time Machine writing meanwhile).
hu=$(echo "$P1" | col /Users/milk 3); vu=$(echo "$P1" | col /Volumes/Milk 3)
[ "$(echo "$P1" | col /Users/milk 2)" = "$(echo "$P1" | col /Volumes/Milk 2)" ] && [ -n "$hu" ] && [ -n "$vu" ] && [ $((hu > vu ? hu - vu : vu - hu)) -le 256 ] \
  && ok "disks list /Users/milk = disks list /Volumes/Milk (fstatfs: same size, used $vu / $hu K)" || bad "home vs volume"
u0=$(echo "$DF" | col /Volumes/Milk 3); u1=$(out "disks list /Volumes/Milk" | col /Volumes/Milk 3)
big=$(( $(out "text count --bytes /Volumes/Milk/big.bin" | awk '{print $1; exit}') / 1024 ))
[ -n "$u0" ] && [ -n "$u1" ] && [ "$big" -gt 1024 ] && [ $((u1 - u0)) -ge "$big" ] && [ $((u1 - u0)) -le $((big + big / 10 + 64)) ] \
  && ok "$big K written: used $u0 -> $u1 K" || bad "used: $u0 -> $u1 K after writing $big K"
out "disks list /Volumes/Milk/.snapshots/dfsnap" | grep -q "^milkfs .* /Volumes/Milk/.snapshots/dfsnap (ro)$" && ok "a snapshot: milkfs, read-only" || bad "snapshot statfs"
out "disks list /no/such/path" | grep -q "^disks list: /no/such/path doesn't exist" && ok "a missing path: an error" || bad "missing path"
echo "$L" | grep -q "^encryption:  off$" && ok "info (plain volume): encryption: off" || bad "info encryption line"
echo "$L" | grep -q "fs selftest: PASS" && ok "self-test: $(echo "$L" | grep -o 'fs selftest: PASS ([0-9]* checks)')" || bad "self-test: $(echo "$L" | grep -a 'selftest: FAIL' | head -3)"
echo
[ $FAIL = 0 ] && echo "dftest: PASS ($T)" || echo "dftest: FAIL ($T)"
exit $FAIL
