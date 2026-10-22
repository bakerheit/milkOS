#!/usr/bin/env bash
# Unit tests for user/filekit on the Mac itself (the rules behind files, trash and the Files
# app's Trash: the trailing-slash rule, where copies land, Trash metadata and names, how long
# things stay). Prints "filekit-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" filekit filekit-unit
