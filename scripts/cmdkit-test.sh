#!/usr/bin/env bash
# Unit tests for user/cmdkit on the Mac itself (the parsing and wording behind network, disks,
# apps and search: options, /proc/net text, URLs, milkfs's errors and progress lines, search
# hits). Prints "cmdkit-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" cmdkit cmdkit-unit
