#!/usr/bin/env bash
# Build a release bundle anyone with a Mac + QEMU can run:
#   dist/milkOS-<VERSION>/            (gitignored)
#     milkOS.command                  double-click to run (windowed; --headless, --sound)
#     README.txt                      for non-developers
#     kernel.bin                      release kernel (flat arm64 Image, initramfs inside)
#     disks/milk.img                  milkfs volume "Milk", made here with mkmilkfs.py:
#                                     /Users/milk (the home skeleton) and /Apps
#     disks/scratch.img               scratch disk (the boot self-test's)
#     share/README.txt                the Mac shared folder, empty, read-only in milkOS
#   dist/milkOS-<VERSION>.zip         the same folder, zipped (ditto: modes kept)
#   dist/SHA256SUMS                   the zip's SHA-256, in `shasum -c` form
# The kernel is built with MILKOS_RELEASE=1, so it says plain "0.2.0", not "0.2.0-dev (abc1234)"
# (docs/versioning.md). The name comes from <repo>/VERSION. Needs macOS (ditto, dd). Releases are
# built from a clean checkout of the tag: scripts/publish-release.sh checks that.
# Nothing in the bundle refers to the repo, cargo or rustup.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
V="$(tr -d '[:space:]' < VERSION)"
NAME="milkOS-$V"
OUT="dist/$NAME"
M="python3 scripts/mkmilkfs.py"
MILK_SIZE="${MILK_SIZE:-512M}"

echo "== building the release kernel ($V)"
MILKOS_RELEASE=1 cargo build --release -q
ELF="target/aarch64-unknown-none-softfloat/release/milkos-kernel"
rm -rf "$OUT" "dist/$NAME.zip"
mkdir -p "$OUT/disks" "$OUT/share"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$OUT/kernel.bin"
grep -aq "milkOS $V (aarch64" "$OUT/kernel.bin" || { echo "release: kernel.bin isn't version $V" >&2; exit 1; }
if grep -aq "milkOS $V-dev (" "$OUT/kernel.bin"; then echo "release: kernel.bin is a dev build (MILKOS_RELEASE=1 didn't reach build.rs)" >&2; exit 1; fi

echo "== disks"
$M mkfs "$OUT/disks/milk.img" --size "$MILK_SIZE" --label Milk >/dev/null
$M mkdir "$OUT/disks/milk.img" /Users/milk >/dev/null
$M mkdir "$OUT/disks/milk.img" /Apps >/dev/null
(cd rootfs/Users/milk && find . -type d | sort) | while read -r d; do
  [ "$d" = . ] || $M mkdir "$OUT/disks/milk.img" "/Users/milk/${d#./}" >/dev/null
done
(cd rootfs/Users/milk && find . -type f ! -name .DS_Store | sort) | while read -r f; do
  $M put "$OUT/disks/milk.img" "/Users/milk/${f#./}" "rootfs/Users/milk/$f" >/dev/null
done
$M check "$OUT/disks/milk.img" --quiet >/dev/null
dd if=/dev/zero of="$OUT/disks/scratch.img" bs=1m count=0 seek=64 2>/dev/null
printf 'milkOS scratch disk (created by scripts/release.sh)\n' | dd of="$OUT/disks/scratch.img" conv=notrunc 2>/dev/null

echo "== files"
for f in milkOS.command README.txt; do
  sed "s/@VERSION@/$V/g" "scripts/release/$f" > "$OUT/$f"
done
cp scripts/release/share-README.txt "$OUT/share/README.txt"
chmod 755 "$OUT/milkOS.command"
# The bundle must stand alone.
if grep -rIl -e "$(pwd)" -e rustup -e cargo "$OUT/milkOS.command" "$OUT/README.txt" "$OUT/share"; then
  echo "release: the bundle refers to the build machine (above)" >&2; exit 1
fi

echo "== zip"
(cd dist && ditto -c -k --keepParent "$NAME" "$NAME.zip")
(cd dist && shasum -a 256 "$NAME.zip" > SHA256SUMS && shasum -a 256 -c SHA256SUMS >/dev/null) || { echo "release: SHA256SUMS failed" >&2; exit 1; }
du -sh "$OUT" | sed 's/^/   /'
ls -l "dist/$NAME.zip" | awk '{print "   " $5 " bytes  " $9}'
echo "release: $OUT, dist/$NAME.zip and dist/SHA256SUMS"
