#!/usr/bin/env bash
# Build the toy packages (packages/) into a milk repo, and/or serve it to the guest.
#   scripts/milk-repo.sh build [DIR]          build + package + index into DIR (/tmp/milk-repo)
#   scripts/milk-repo.sh serve [PORT] [DIR]   serve DIR on 127.0.0.1:PORT (8765) until ^C
# The guest reaches it as http://10.0.2.2:PORT/ (qemu user networking maps that to the
# Mac's 127.0.0.1); put that URL in the guest's /System/Settings/milk/repos (the default already is
# http://10.0.2.2:8765/). The server binds 127.0.0.1 only and is killed by its exact PID.
# `run.sh` restricts the guest network (no host access), so the repo is reachable only from
# boots with an unrestricted user netdev, like scripts/pkg-test.sh's.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

build() {
  local dir="${1:-/tmp/milk-repo}"
  (cd packages && cargo build --release -q --target-dir "$REPO_ROOT/target/packages")
  mkdir -p "$dir"
  rm -f "$dir"/*.milkpkg "$dir"/index.txt
  # Every spec: packages/<name>/package.txt, plus extra versions as package-<ver>.txt.
  for spec in packages/*/package*.txt; do
    local name
    name=$(awk '$1 == "name" { print $2 }' "$spec")
    local ver
    ver=$(awk '$1 == "version" { print $2 }' "$spec")
    python3 -I scripts/mkpkg.py build "$spec" --root "$REPO_ROOT" -o "$dir/$name-$ver.milkpkg"
  done
  # Signed with the TEST-ONLY toy key (scripts/milk-test-key.secret): index.txt.sig and
  # <pkg>.milkpkg.sig, checked by `apps` against /System/Settings/milk/keys/milk-test.pub.
  python3 -I scripts/mkpkg.py index "$dir" --key "${MILK_SIGNING_KEY:-scripts/milk-test-key.secret}"
}

serve() {
  local port="${1:-8765}" dir="${2:-/tmp/milk-repo}"
  [ -s "$dir/index.txt" ] || build "$dir"
  python3 -I -m http.server --bind 127.0.0.1 --directory "$dir" "$port" &
  local pid=$!
  trap 'kill "$pid" 2>/dev/null || true' EXIT INT TERM
  echo "milk repo: http://127.0.0.1:$port/ (guest: http://10.0.2.2:$port/), pid $pid; ^C stops it"
  wait "$pid"
}

case "${1:-build}" in
  build) build "${2:-}" ;;
  serve) serve "${2:-}" "${3:-}" ;;
  *) echo "usage: $0 build [DIR] | serve [PORT] [DIR]" >&2; exit 2 ;;
esac
