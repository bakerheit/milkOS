#!/usr/bin/env bash
# Compares the JavaScript module conformance cases (user/jsengine/tests/modules) with Node, a
# second implementation of the spec. A development check, not part of check-all (the guest has no
# Node); it says so and exits 0 when Node is missing.
cd "$(dirname "$0")/.." || exit 1
command -v node >/dev/null 2>&1 || { echo "jsmodules-oracle: node not found, skipped"; exit 0; }
exec node scripts/jsmodules_oracle.mjs "$@"
