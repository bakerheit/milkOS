#!/usr/bin/env bash
# Unit tests for user/syskit on the Mac itself (the rules behind processes, services, system,
# users and admin: their help, how their words are read, signal names, picking a process,
# the journal filters, milkd replies, uptimes, the memory groups). Prints
# "syskit-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" syskit syskit-unit
