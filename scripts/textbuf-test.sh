#!/usr/bin/env bash
# Unit tests for user/textbuf on the Mac itself (the Text Editor's buffer: lines, selection, undo
# and redo, find and replace, UTF-8, CRLF and BOM kept, a random-edit model check). Prints
# "textbuf-unit: N passed" and fails if none ran. See scripts/hosttest.sh.
exec "$(dirname "$0")/hosttest.sh" textbuf textbuf-unit
