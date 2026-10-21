#!/usr/bin/env bash
# Desktop + packages demos: build the toy repo (packages/), serve it on a free 127.0.0.1 port
# (python3 http.server, killed by its exact PID), and run wm demos that install from it in
# the Terminal (the guest reaches the Mac as 10.0.2.2 through WM_HOSTNET's open netdev).
#   scripts/wm-pkg-demo.sh OUTDIR [DEMO ...]     (default demos: pkgdock pkgrun pkgfortune)
# Disks are the usual shared snapshot ones (writes discarded), so nothing stays installed.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:?usage: wm-pkg-demo.sh OUTDIR [DEMO ...]}"
shift
DEMOS=("$@")
[ ${#DEMOS[@]} -gt 0 ] || DEMOS=(pkgdock pkgrun pkgfortune)
mkdir -p "$OUT"
WORK="$(mktemp -d /tmp/milk-wm-pkg.XXXXXX)"
HPID=""
cleanup() { [ -n "$HPID" ] && kill "$HPID" 2>/dev/null || true; }
trap cleanup EXIT
scripts/milk-repo.sh build "$WORK/repo" >"$WORK/repo.log"
PORT=$(python3 -I -c "import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1])")
(cd "$WORK" && exec python3 -I -m http.server --bind 127.0.0.1 --directory "$WORK/repo" "$PORT" >"$WORK/http.log" 2>&1) &
HPID=$!
for _ in $(seq 50); do curl -sf "http://127.0.0.1:$PORT/index.txt" >/dev/null 2>&1 && break; sleep 0.1; done
echo "repo at http://127.0.0.1:$PORT/ (pid $HPID)"
for d in "${DEMOS[@]}"; do
  WM_HOSTNET=1 scripts/wm-demo.sh "$d" "$OUT/$d.png" -fw_cfg "name=opt/milkos/testport,string=$PORT" |
    grep screenshot || echo "$d: failed"
done
