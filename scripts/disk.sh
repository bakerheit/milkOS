# Sourced by run.sh / screenshot.sh: a scratch virtio-blk disk in /tmp.
#   milk_disk_args          -> DISK_ARGS writable, persistent (qemu locks the image)
#   milk_disk_args shared   -> DISK_ARGS with snapshot=on + no locking: writes are thrown
#                              away, and any number of qemus can use the image at once
# The image (64 MiB, sparse) is created on first use, with a text label in sector 0 that
# the kernel's fs self-test reads back. Override the path with MILK_DISK=...
milk_disk_args() {
  local disk="${MILK_DISK:-/tmp/milk-disk.img}"
  if [ ! -e "$disk" ]; then
    dd if=/dev/zero of="$disk" bs=1m count=0 seek=64 2>/dev/null
    printf 'milkOS scratch disk (created by scripts/disk.sh)\n' |
      dd of="$disk" conv=notrunc 2>/dev/null
  fi
  local opts="file=$disk,if=none,format=raw,id=hd0"
  [ "${1:-}" = shared ] && opts="$opts,snapshot=on,file.locking=off"
  DISK_ARGS=(-drive "$opts" -device virtio-blk-device,drive=hd0)
}
