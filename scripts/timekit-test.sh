#!/usr/bin/env bash
# Unit tests for user/timekit (dates, repeating events, alarms, timers) on the Mac itself:
# the crate is pure logic, so no VM is needed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/../user"
exec cargo test -q -p timekit --target aarch64-apple-darwin
