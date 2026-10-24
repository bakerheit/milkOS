#!/usr/bin/env bash
# Run the unit tests of one pure crate on the Mac and say how many ran:
#   scripts/hosttest.sh CRATE NAME      e.g. scripts/hosttest.sh timekit timekit-unit
# CRATE is a userland crate (user/CRATE, the user workspace) or one of the root workspace's
# (<repo>/CRATE, e.g. milkfs; its package name is read from CRATE/Cargo.toml, so crypto
# runs milkcrypto).
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
PKG="$CRATE"
if [ -f "$REPO/$CRATE/Cargo.toml" ]; then
  cd "$REPO"
  PKG="$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$CRATE/Cargo.toml" | head -1)"
else
  cd "$REPO/user"
fi
# The host the compiler runs on (aarch64-apple-darwin on the Macs this is made on;
# MILK_HOST_TARGET overrides it).
HOST="${MILK_HOST_TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
OUT="$(cargo test -p "$PKG" --lib --target "$HOST" --target-dir "$REPO/target/host-tests" 2>&1)"; RC=$?
echo "$OUT"
[ "$RC" = 0 ] || { echo "$NAME: FAIL (cargo test exited $RC)"; exit 1; }
PASSED="$(echo "$OUT" | sed -n 's/^test result: ok\. \([0-9]*\) passed.*/\1/p' | awk '{s += $1} END {print s + 0}')"
FAILED="$(echo "$OUT" | sed -n 's/.* \([0-9]*\) failed.*/\1/p' | awk '{s += $1} END {print s + 0}')"
[ "$FAILED" = 0 ] || { echo "$NAME: FAIL ($FAILED failed)"; exit 1; }
[ "$PASSED" -gt 0 ] || { echo "$NAME: FAIL (no tests ran)"; exit 1; }
echo "$NAME: $PASSED passed"
