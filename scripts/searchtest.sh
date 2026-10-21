#!/usr/bin/env bash
# The search index on a private image (fresh /tmp directory; only qemus started here are
# killed). The image gets N files (default 5000) under /home/milk/gen with known words
# (mkmilkfs.py populate: "uniqNNNNN" once each, FRUITS[i % 7], .md/.txt/no extension).
#   boot 1: the index builds; queries return exactly the expected files (unique word,
#           fruit counts, kind:/in: filters, name prefix, a structured pipe); a new file
#           is found within 1 s, a rename shows the new path within 1 s, a delete is
#           gone within 1 s; the index is saved to ~/.cache/milk/index.
#   Mac:    add a file while milkOS is off.
#   boot 2: the index is loaded from disk and verified (nothing re-read but the new
#           file), queries work at once, the offline file is found.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
N="${1:-5000}"
T="$(mktemp -d /tmp/milk-searchtest.XXXXXX)"
M="python3 scripts/mkmilkfs.py"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/s.img"
$M mkfs "$IMG" --size 256M >/dev/null
$M mkdir "$IMG" /home/milk >/dev/null
$M populate "$IMG" /home/milk/gen "$N" >/dev/null
expect() { python3 -c "n=$N; print(sum(1 for i in range(n) if $1))"; }
QPID=""
trap '[ -n "$QPID" ] && kill -9 "$QPID" 2>/dev/null || true' EXIT
boot() { # name commands...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  qemu-system-aarch64 -M virt -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
  QPID=$!
  local P='msh:[^\n]*\$ '
  local args=("wait:$P")
  for c in "$@"; do args+=("send:$c" "wait:$P"); done
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "${args[@]}" "send:echo SEARCH""-DONE" "wait:SEARCH-DONE" "done:$T/$name.done" &
  local DPID=$!
  for _ in $(seq 3000); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true; QPID=""
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
}
# Output of the command typed as $2 in log $1, up to the next prompt.
out() { tr -d '\r' < "$1" | awk -v c="$2" 'on && /^msh:/ {on=0; done=1} !done && $0 ~ /^msh:/ && substr($0, index($0, "$ ") + 2) == c {on=1; next} on'; }
G=/home/milk/gen
boot b1 "sleep 4" "spot --status" "sleep 2" "spot --status" \
  "spot uniq01234" "head -1 /sys/fs/search/query" "spot -n 10000 mango" "head -1 /sys/fs/search/query" "spot -n 10000 mango kind:md" "spot -n 10000 in:d042" \
  "spot f0499" "spot -n 3 kind:txt cherry | where kind == txt | count" \
  "echo quokka sighting near the dam > /home/milk/new-note.txt" "sleep 1" "spot quokka" \
  "mv /home/milk/new-note.txt $G/d007/quokka.md" "sleep 1" "spot -l quokka" \
  "rm $G/d007/quokka.md" "sleep 1" "spot quokka" \
  "echo now > /sys/fs/milkfs/sync" "sleep 3" "spot --status"
L1="$T/b1.log"
st="$(out "$L1" "spot --status" | grep "origin" | head -1 | sed 's/^origin: *//')"
echo "    first boot: $st"
tr -d '\r' < "$L1" | grep -a "^# .* result(s) for" | sed 's/^/    query: /'
out "$L1" "spot --status" | grep -q "state: *ready" && ok "index ready" || bad "index not ready"
[ "$(out "$L1" "spot uniq01234")" = "$G/d012/f01234.txt" ] && ok "a unique word finds exactly its file" || bad "uniq01234: $(out "$L1" "spot uniq01234" | head -3)"
c=$(out "$L1" "spot -n 10000 mango" | grep -c "^$G" || true); e=$(expect "i % 7 == 5")
[ "$c" = "$e" ] && ok "mango: $c files, as expected" || bad "mango: $c, want $e"
c=$(out "$L1" "spot -n 10000 mango kind:md" | grep -c "\.md$" || true); e=$(expect "i % 7 == 5 and i % 3 == 0")
[ "$c" = "$e" ] && [ "$(out "$L1" "spot -n 10000 mango kind:md" | grep -vc "\.md$" || true)" = 0 ] && ok "mango kind:md: $c, as expected" || bad "mango kind:md: $c, want $e"
c=$(out "$L1" "spot -n 10000 in:d042" | grep -c "^$G/d042/" || true)
[ "$c" = 100 ] && ok "in:d042: the folder's 100 files" || bad "in:d042: $c"
c=$(out "$L1" "spot f0499" | grep -c "^$G" || true)
[ "$c" = 10 ] && [ "$(out "$L1" "spot f0499" | head -1)" = "$G/d049/f04990.txt" ] && ok "name prefix f0499: 10 files" || bad "f0499: $c"
[ "$(out "$L1" "spot -n 3 kind:txt cherry | where kind == txt | count" | tail -1 | tr -d ' ')" = 3 ] && ok "structured pipe: spot | where kind == txt | count" || bad "structured pipe: $(out "$L1" "spot -n 3 kind:txt cherry | where kind == txt | count")"
[ "$(out "$L1" "spot quokka" | head -1)" = /home/milk/new-note.txt ] && ok "a new file is found within 1 s" || bad "new file: $(out "$L1" "spot quokka")"
out "$L1" "spot -l quokka" | grep -q "md .*content.* $G/d007/quokka.md$" && ok "a rename shows the new path (and kind) within 1 s" || bad "rename: $(out "$L1" "spot -l quokka")"
tr -d '\r' < "$L1" | grep -aq 'spot: nothing found for "quokka"' && ok "a delete is gone within 1 s" || bad "delete still found"
out "$L1" "spot --status" | tail -6 | grep -q "saves" && ok "saved: $(out "$L1" "spot --status" | tail -5 | grep events | sed 's/^events: *//')" || bad "no save"

echo "a narwhal surfaced while milkOS was off" > "$T/offline.txt"
$M put "$IMG" "$G/d000/offline.txt" "$T/offline.txt" >/dev/null
boot b2 "sleep 3" "spot --status" "spot uniq04321" "spot narwhal"
L2="$T/b2.log"
st="$(out "$L2" "spot --status" | grep origin | sed 's/^origin: *//')"
echo "    second boot: $st"
echo "$st" | grep -q "^loaded .* verified" || bad "index not loaded: $st"
[ "$(out "$L2" "spot uniq04321")" = "$G/d043/f04321.txt" ] && ok "queries work right away" || bad "uniq04321: $(out "$L2" "spot uniq04321")"
[ "$(out "$L2" "spot narwhal")" = "$G/d000/offline.txt" ] && ok "a file added while off is found (verify scan)" || bad "offline file: $(out "$L2" "spot narwhal")"
$M check "$IMG" > "$T/check.txt" 2>&1 && ok "host check clean" || { bad "host check"; tail -3 "$T/check.txt"; }
echo
[ $FAIL = 0 ] && echo "searchtest: PASS ($T)" || echo "searchtest: FAIL ($T)"
exit $FAIL
