#!/usr/bin/env bash
# The repo is public: fail on names (from a local list kept outside the repo), e-mail addresses,
# home paths, key material and ticket ids. Checks the working tree and the commits not yet on origin/main.
# Rules and exemptions: scripts/public_check.py. Run it before asking for a merge.
set -euo pipefail
exec python3 -I "$(dirname "$0")/public_check.py" "$@"
