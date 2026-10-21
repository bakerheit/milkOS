#!/usr/bin/env bash
# Unit tests for user/sysview on the Mac itself (pure logic, no VM). Prints "sysview-unit: N passed" and
# fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" sysview sysview-unit
