#!/usr/bin/env bash
# Run the unit tests of one pure userland crate on the Mac and say how many ran:
#   scripts/hosttest.sh CRATE NAME      e.g. scripts/hosttest.sh timekit timekit-unit
# Only the library's tests are run (`--lib`: the empty doc-test pass printed "0 passed" after the
# real one and made a log look like nothing ran). It fails if any test fails, if the output has no
# test result, or if zero tests ran, and ends with "NAME: N passed".
# Built into <repo>/target/host-tests, not user/target: kernel/build.rs watches all of user/, so
# writing there made the next `cargo build` rebuild the kernel (and, with check-all -j, replace
# it under the tests already running).
set -uo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
CRATE="${1:?usage: hosttest.sh CRATE NAME}"; NAME="${2:-$CRATE-unit}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO/user"
OUT="$(cargo test -p "$CRATE" --lib --target aarch64-apple-darwin --target-dir "$REPO/target/host-tests" 2>&1)"; RC=$?
echo "$OUT"
[ "$RC" = 0 ] || { echo "$NAME: FAIL (cargo test exited $RC)"; exit 1; }
PASSED="$(echo "$OUT" | sed -n 's/^test result: ok\. \([0-9]*\) passed.*/\1/p' | awk '{s += $1} END {print s + 0}')"
FAILED="$(echo "$OUT" | sed -n 's/.* \([0-9]*\) failed.*/\1/p' | awk '{s += $1} END {print s + 0}')"
[ "$FAILED" = 0 ] || { echo "$NAME: FAIL ($FAILED failed)"; exit 1; }
[ "$PASSED" -gt 0 ] || { echo "$NAME: FAIL (no tests ran)"; exit 1; }
echo "$NAME: $PASSED passed"
