# Sourced by run.sh / screenshot.sh / input-test.sh: the Mac shared folder (virtio-9p).
#   milk_share_args   -> SHARE_ARGS for qemu
# Shares exactly ONE host folder: <main checkout>/share/ (~/workspace/milkOS/share), found
# from git so every worktree uses the same one. There is deliberately no way to pass a
# different path. Read-only (qemu readonly=on, and the kernel refuses writes) unless
# MILK_SHARE_RW=1. Inside milkOS it is /Volumes/Mac.
milk_share_args() {
  local here common repo dir
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  # Absolute: in the main checkout git prints a relative path here, in worktrees an absolute one.
  common="$(git -C "$here" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || { SHARE_ARGS=(); return; }
  repo="$(cd "$common/.." && pwd)"
  dir="$repo/share"
  mkdir -p "$dir"
  [ -e "$dir/README.txt" ] || cp "$here/../share/README.txt" "$dir/README.txt" 2>/dev/null || true
  local ro=",readonly=on" rw=0
  if [ "${MILK_SHARE_RW:-0}" = 1 ]; then ro=""; rw=1; fi
  SHARE_ARGS=(-fsdev "local,id=fs0,path=$dir,security_model=mapped-xattr$ro"
    -device virtio-9p-device,fsdev=fs0,mount_tag=mac
    -fw_cfg "name=opt/milkos/share-rw,string=$rw")
}
