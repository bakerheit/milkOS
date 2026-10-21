#!/usr/bin/env bash
# Unit tests for user/sysview (the logic behind Activity Monitor: /proc parsing, CPU percent,
# sorting, charts' history, audit records in plain English) on the Mac itself; no VM needed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/../user"
exec cargo test -q -p sysview --target aarch64-apple-darwin
