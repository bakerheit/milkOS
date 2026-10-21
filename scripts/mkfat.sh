#!/usr/bin/env bash
# Make a FAT32 disk image with macOS's own tools (hdiutil + newfs_msdos), in /tmp only.
#   scripts/mkfat.sh [IMAGE] [SIZE_MB] [LABEL]      default /tmp/milk-fat.img 64 MILKFAT
# 512-byte clusters (FAT32 needs >= 65525 clusters, so small images need small clusters).
# Boot it with MILK_FAT=IMAGE scripts/run.sh: milkOS mounts it at /Volumes/<LABEL>.
set -euo pipefail
IMG="${1:-/tmp/milk-fat.img}"
SIZE="${2:-64}"
LABEL="${3:-MILKFAT}"
dir="$(cd "$(dirname "$IMG")" && pwd -P)"
case "$dir/" in
  /private/tmp/*|/tmp/*) ;;
  *) echo "mkfat: disk images go in /tmp only (got $IMG)" >&2; exit 1 ;;
esac
[ -e "$IMG" ] && { echo "mkfat: $IMG exists, not overwriting" >&2; exit 1; }
dd if=/dev/zero of="$IMG" bs=1m count=0 seek="$SIZE" 2>/dev/null
DEV="$(hdiutil attach -imagekey diskimage-class=CRawDiskImage -nomount "$IMG" | awk 'NR==1{print $1}')"
trap 'hdiutil detach "$DEV" >/dev/null 2>&1 || true' EXIT
newfs_msdos -F 32 -S 512 -c 1 -v "$LABEL" "$DEV" >/dev/null
echo "$IMG: FAT32, $SIZE MiB, label $LABEL"
