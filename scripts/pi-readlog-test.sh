#!/usr/bin/env bash
# Host tests of the Mac-side log reader (scripts/pi_readlog.py, run through scripts/pi-readlog.sh) on made-up files: healthy
# logs of one and many boots (--all, --last, --boot N, --list, --tail N), a wrapped ring, torn and lost headers, a header that
# lies, damaged sectors, ghost sectors behind a gap, old format 1 files, blank, truncated and garbage files, bad options.
# No VM. See scripts/pi_readlog_test.py. Prints "pi-readlog-test: N passed".
set -uo pipefail
cd "$(dirname "$0")/.."
OUT="$(python3 -I scripts/pi_readlog_test.py 2>&1)" || { echo "$OUT"; echo "pi-readlog-test: FAIL"; exit 1; }
N="$(echo "$OUT" | sed -n 's/^Ran \([0-9]*\) tests.*/\1/p')"
[ "${N:-0}" -gt 0 ] || { echo "$OUT"; echo "pi-readlog-test: FAIL (no tests ran)"; exit 1; }
echo "pi-readlog-test: $N passed"
