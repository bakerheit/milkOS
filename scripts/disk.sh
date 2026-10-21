# Sourced by run.sh / screenshot.sh: two virtio-blk disks in /tmp.
#   /tmp/milk-disk.img  scratch disk for the block-layer self-test (64 MiB, sparse,
#                       text label in sector 0)
#   /tmp/milk-fs.img    milkfs volume, mounted at /Volumes/Milk (64 MiB, made with
#                       scripts/mkmilkfs.py on first use; never reformatted here)
#   milk_disk_args          -> DISK_ARGS writable, persistent (qemu locks the images)
#   milk_disk_args shared   -> DISK_ARGS with snapshot=on + no locking: writes are thrown
#                              away, and any number of qemus can use the images at once
# Override the paths with MILK_DISK=... / MILK_FS=...
milk_disk_args() {
  local disk="${MILK_DISK:-/tmp/milk-disk.img}"
  local fsimg="${MILK_FS:-/tmp/milk-fs.img}"
  local here
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  if [ ! -e "$disk" ]; then
    dd if=/dev/zero of="$disk" bs=1m count=0 seek=64 2>/dev/null
    printf 'milkOS scratch disk (created by scripts/disk.sh)\n' |
      dd of="$disk" conv=notrunc 2>/dev/null
  fi
  if [ ! -e "$fsimg" ]; then
    python3 "$here/mkmilkfs.py" mkfs "$fsimg" --size 64M --label Milk >/dev/null
  fi
  local extra=""
  [ "${1:-}" = shared ] && extra=",snapshot=on,file.locking=off"
  DISK_ARGS=(-drive "file=$disk,if=none,format=raw,id=hd0$extra" -device virtio-blk-device,drive=hd0
    -drive "file=$fsimg,if=none,format=raw,id=hd1$extra" -device virtio-blk-device,drive=hd1)
}
