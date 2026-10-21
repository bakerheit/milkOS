#!/usr/bin/env bash
# Unit tests for user/diskkit (the logic behind Disk Utility: /sys/fs/milkfs and /proc/mounts
# parsing, names, error words, backup file names) on the Mac itself; no VM needed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/../user"
exec cargo test -q -p diskkit --target aarch64-apple-darwin
