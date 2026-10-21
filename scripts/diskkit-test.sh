#!/usr/bin/env bash
# Unit tests for user/diskkit on the Mac itself (pure logic, no VM). Prints "diskkit-unit: N passed" and
# fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" diskkit diskkit-unit
