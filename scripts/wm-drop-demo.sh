#!/usr/bin/env bash
# Two milkOS desktops with their eth1s linked, each running a demo script (kernel/src/wm/
# script.rs), for the Milk Drop sheet and incoming panel:
#   scripts/wm-drop-demo.sh ALICE_DEMO BOB_DEMO OUTDIR
# e.g. scripts/wm-drop-demo.sh drop-alice-pick drop-bob-wait /tmp/drop-shots
# Each VM gets fresh, private milkfs and scratch images in a mktemp dir (so the two have
# different Milk Drop identities), a restricted eth0 (wm-demo.sh), and eth1 on
# qemu `-netdev dgram` between two free UDP ports on 127.0.0.1 (MACs as in drop-test.sh, so
# 169.254.10.10 / 169.254.11.11). Screenshots: OUTDIR/ALICE_DEMO.png and OUTDIR/BOB_DEMO.png.
# Sound goes to WAV files (never the speakers). Only the qemus started here are killed.
set -euo pipefail
cd "$(dirname "$0")/.."
A="${1:?usage: scripts/wm-drop-demo.sh ALICE_DEMO BOB_DEMO OUTDIR}"
B="${2:?usage}"
OUT="${3:?usage}"
mkdir -p "$OUT"
WORK=$(mktemp -d /tmp/milk-drop-demo.XXXXXX)
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build -q
read -r PA PB < <(python3 -I -c '
import socket
s = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(2)]
for x in s: x.bind(("127.0.0.1", 0))
print(*[x.getsockname()[1] for x in s])')
for who in alice bob; do
  python3 -I scripts/mkmilkfs.py mkfs "$WORK/$who-fs.img" --size 64M --label Milk >/dev/null
  dd if=/dev/zero of="$WORK/$who-disk.img" bs=1m count=0 seek=64 2>/dev/null
done
# run NAME DEMO MAC LOCAL REMOTE
run() {
  DEMO_WAIT=90 MILK_FS="$WORK/$1-fs.img" MILK_DISK="$WORK/$1-disk.img" MILK_SOUND_WAV="$WORK/$1.wav" \
    scripts/wm-demo.sh "$2" "$OUT/$2.png" \
    -netdev "dgram,id=d0,local.type=inet,local.host=127.0.0.1,local.port=$4,remote.type=inet,remote.host=127.0.0.1,remote.port=$5" \
    -device "virtio-net-device,netdev=d0,mac=$3" >"$WORK/$1.out" 2>&1
}
run bob "$B" 02:6d:6b:64:0b:0b "$PB" "$PA" &
BP=$!
sleep 1
run alice "$A" 02:6d:6b:64:0a:0a "$PA" "$PB" &
AP=$!
ok=1
wait "$AP" || { echo "alice ($A) failed: $(tail -3 "$WORK/alice.out")"; ok=0; }
wait "$BP" || { echo "bob ($B) failed: $(tail -3 "$WORK/bob.out")"; ok=0; }
echo "screenshots: $OUT/$A.png $OUT/$B.png (work dir $WORK)"
[ "$ok" = 1 ]
