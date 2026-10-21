#!/usr/bin/env bash
# Unit tests for user/timekit on the Mac itself (pure logic, no VM). Prints "timekit-unit: N passed" and
# fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" timekit timekit-unit
