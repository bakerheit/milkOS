#!/usr/bin/env bash
# Unit tests for user/textkit on the Mac itself (the rules behind text, print, pause and msh's
# variables: options, matching, sorting, counting, lengths of time, escapes, variable names,
# the help lists). Prints "textkit-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" textkit textkit-unit
