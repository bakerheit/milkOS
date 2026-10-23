#!/usr/bin/env bash
# `text`, `print`, `pause` and msh's `variables` end to end, on a private milkfs image, one boot:
#   print: words, --no-newline and -n, --escapes and -e (\t, \c), -- and --help, no escapes
#          without the flag;
#   text:  show (several files, --numbers, a pipe, -, a missing file, a folder), first and last
#          (N, the default 10, several files, a pipe), find (plain, --ignore-case, --invert,
#          --numbers, --count, --names, several files, a pipe from processes list, exit codes 0 / 1 / 2, a
#          missing TEXT, a short flag refused), sort (--reverse, --unique, --numbers, --field),
#          unique (--count, --repeated, as a table for filter / get), count (the table, one
#          count as a bare number, a pipe, a total row, as a table for msh), help, an unknown
#          verb or option;
#   pause: 1s, 500ms, 0.2, a bad length, none, help;
#   variables: set (exported: a child shell sees it), get, NAME=value (shell only), list (text
#          and as a table with its exported column), set NAME (export one that is set),
#          remove, run NAME=value CMD (alone and in a pipe, not left behind), the errors, help,
#          `variables` alone;
#   the old names (cat echo env grep head tail sort uniq wc sleep, and msh's export set unset)
#   are "command not found"; msh's help names the new ones.
# Fresh /tmp directory; only the qemus started here are killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d "${TMPDIR:-/tmp}/milk-text.XXXXXX")"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/fs.img"
python3 scripts/mkmilkfs.py mkfs "$IMG" --size 64M --label Milk >/dev/null
P='msh:[^\n]*\$ '

boot() { # name steps...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" "$@" "send:print TEXT""-DONE" "wait:TEXT-DONE" "done:$T/$name.done" &
  DPID=$!
  for _ in $(seq 900); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
  [ -e "$T/$name.done" ] || bad "$name: the session did not finish"
  python3 -I scripts/console_clean.py "$LOG"
}
trap 'kill -9 "${QPID:-}" 2>/dev/null || true' EXIT

# Steps: "send:CMD" "wait:PROMPT" for each command, appended to the array named first.
s() { local a="$1" c; shift; for c in "$@"; do eval "$a+=(\"send:\$c\" \"wait:\$P\")"; done; }
B=(); s B \
  "go" \
  "print one two three > a.txt" \
  "print --escapes 'b\nc\nb\nb\na' > b.txt" \
  "print --escapes '1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12' > n.txt" \
  "print --escapes 'Milk\nhoney\nmilkshake\ncocoa' > m.txt" \
  "print --escapes 'x 3\ny 1\nz 2' > f.txt" \
  "print hello   world" \
  "print --no-newline abc > nn.txt" \
  "text count --bytes nn.txt" \
  "print -n abcd > nn.txt" \
  "text count --bytes nn.txt" \
  "print --escapes 'x\ty' > tab.txt" \
  "text count --bytes tab.txt" \
  "print -e 'stop\cgone' > c.txt" \
  "text show c.txt" \
  "text count --bytes c.txt" \
  "print 'a\nb' > lit.txt" \
  "text count --lines lit.txt" \
  "print -- --escapes" \
  "print --help" \
  "text show a.txt b.txt" \
  "text show --numbers b.txt" \
  "print piped | text show" \
  "print dash | text show -" \
  "text show missing.txt" \
  "text show /Users" \
  "text first 3 n.txt" \
  "text first n.txt" \
  "text last 2 n.txt" \
  "text last n.txt" \
  "text first 1 a.txt b.txt" \
  "text show n.txt | text last 1" \
  "text find milk m.txt" \
  "text find --ignore-case milk m.txt" \
  "text find --invert milk m.txt" \
  "text find --numbers honey m.txt" \
  "text find --count milk m.txt" \
  "text find --names milk m.txt a.txt" \
  "text find milk m.txt a.txt" \
  "processes list | text find msh" \
  'text find nothing-here m.txt; print rc=$?' \
  'text find milk m.txt > /dev/null; print rc=$?' \
  'text find milk nowhere.txt; print rc=$?' \
  "text find" \
  "text find -i milk m.txt" \
  "text sort b.txt" \
  "text sort --reverse b.txt" \
  "text sort --unique b.txt" \
  "text sort n.txt | text last 1" \
  "text sort --numbers n.txt | text last 1" \
  "text sort --numbers --field 2 f.txt" \
  "text unique b.txt" \
  "text unique --count b.txt" \
  "text unique --repeated b.txt" \
  "text sort b.txt | text unique --count | filter count > 1 | get line" \
  "text count a.txt" \
  "text count --lines n.txt" \
  "text show n.txt | text count --lines" \
  "text count --words a.txt" \
  "text count a.txt b.txt" \
  "text count a.txt b.txt | filter file == total | get lines" \
  "MILK_TABLE=1 text count a.txt" \
  "text" \
  "text help" \
  "text frobnicate" \
  "text show --invert a.txt" \
  'pause 1s; print rc=$?' \
  "pause 500ms && print half-ok" \
  "pause 0.2 && print fifth-ok" \
  "pause 5x" \
  "pause" \
  "pause help" \
  "variables set GREETING=hello" \
  'print got $GREETING' \
  "variables get GREETING" \
  "msh -c 'print child sees \$GREETING'" \
  "LOCAL=1" \
  "variables list" \
  "MILK_TABLE=1 variables list" \
  "variables list | filter name == LOCAL | get exported" \
  "variables list | filter name == GREETING | get exported" \
  "msh -c 'variables get LOCAL'" \
  "variables set LOCAL" \
  "msh -c 'variables get LOCAL'" \
  "variables remove GREETING" \
  "variables get GREETING" \
  "variables remove GREETING" \
  "variables run WHO=cow msh -c 'print hi \$WHO'" \
  "variables get WHO" \
  "variables run WHO=pipe msh -c 'print through a \$WHO' | text show" \
  "variables run A=1" \
  "variables set 1X=2" \
  "variables frob" \
  "variables help" \
  "variables" \
  "cat a.txt" "echo hi" "env" "grep milk m.txt" "head n.txt" "tail n.txt" "sort b.txt" \
  "uniq b.txt" "wc a.txt" "sleep 1" "export X=1" "set" "unset LOCAL" \
  "help"
L="$(boot b1 "${B[@]}")"
echo "$L" > "$T/b1.txt"

# The output of the first (or Nth) run of a command typed at the prompt.
out() { echo "$L" | awk -v c="$1" -v n="${2:-1}" '
  index($0, "$ " c) && substr($0, index($0, "$ " c) + length(c) + 2) == "" { k++; on = (k == n); next }
  /^msh:/ { on = 0 } on'; }
has() { out "$1" "${3:-1}" | grep -Fq -- "$2"; }
check() { # description command text [nth]
  if has "$2" "$3" "${4:-1}"; then ok "$1"; else bad "$1 (wanted '$3' after '$2', got: $(out "$2" "${4:-1}" | head -3 | tr '\n' '|'))"; fi
}
is() { # description command exact-output [nth]
  local got; got="$(out "$2" "${4:-1}")"
  if [ "$got" = "$3" ]; then ok "$1"; else bad "$1 (wanted '$(printf '%s' "$3" | tr '\n' '|')' after '$2', got: '$(printf '%s' "$got" | tr '\n' '|')')"; fi
}
lines() { out "$1" "${2:-1}" | grep -c . || true; }

echo "== print"
is "print: words with one space between" "print hello   world" "hello world"
is "--no-newline: no newline at the end" "text count --bytes nn.txt" "3"
is "-n still works" "text count --bytes nn.txt" "4" 2
is "--escapes: \\t is one character (x, tab, y, newline)" "text count --bytes tab.txt" "4"
# (No newline after it, so the next prompt follows on the same line.)
out "text show c.txt" | grep -q "^stopmsh:" && ok "-e: \\c stops there, with no newline" || bad "-e and \\c: $(out "text show c.txt" | head -2 | tr '\n' '|')"
is "and wrote 4 bytes" "text count --bytes c.txt" "4"
is "no escapes without the flag" "text count --lines lit.txt" "1"
is "-- prints what comes after it" "print -- --escapes" "--escapes"
check "print --help" "print --help" "usage: print [--no-newline] [--escapes] WORDS..."

echo "== text show, first, last"
is "show several files" "text show a.txt b.txt" "$(printf 'one two three\nb\nc\nb\nb\na')"
check "show --numbers" "text show --numbers b.txt" "     5  a"
is "show reads a pipe" "print piped | text show" "piped"
is "show - is stdin" "print dash | text show -" "dash"
check "a missing file, in plain English" "text show missing.txt" "text: missing.txt doesn't exist"
check "a folder" "text show /Users" "text: /Users is a folder, not a file"
is "first 3" "text first 3 n.txt" "$(printf '1\n2\n3')"
[ "$(lines "text first n.txt")" = 10 ] && [ "$(out "text first n.txt" | tail -1)" = 10 ] && ok "first: 10 without N" || bad "first default: $(out "text first n.txt" | tr '\n' '|')"
is "last 2" "text last 2 n.txt" "$(printf '11\n12')"
[ "$(lines "text last n.txt")" = 10 ] && [ "$(out "text last n.txt" | head -1)" = 3 ] && ok "last: 10 without N" || bad "last default: $(out "text last n.txt" | tr '\n' '|')"
is "first of several files: headings" "text first 1 a.txt b.txt" "$(printf '==> a.txt <==\none two three\n==> b.txt <==\nb')"
is "last reads a pipe" "text show n.txt | text last 1" "12"

echo "== text find"
is "plain text, case matters" "text find milk m.txt" "milkshake"
is "--ignore-case" "text find --ignore-case milk m.txt" "$(printf 'Milk\nmilkshake')"
is "--invert" "text find --invert milk m.txt" "$(printf 'Milk\nhoney\ncocoa')"
is "--numbers" "text find --numbers honey m.txt" "2:honey"
is "--count" "text find --count milk m.txt" "1"
is "--names" "text find --names milk m.txt a.txt" "m.txt"
is "several files: name:line" "text find milk m.txt a.txt" "m.txt:milkshake"
check "in a pipe from processes list" "processes list | text find msh" "msh"
is "exits 1 when nothing matches" 'text find nothing-here m.txt; print rc=$?' "rc=1"
is "exits 0 when something does" 'text find milk m.txt > /dev/null; print rc=$?' "rc=0"
is "exits 2 when a file can't be read" 'text find milk nowhere.txt; print rc=$?' "$(printf 'text: nowhere.txt doesn'"'"'t exist\nrc=2')"
check "find with no TEXT" "text find" "usage: text find TEXT [FILE...]"
check "short flags are spelled out now" "text find -i milk m.txt" "text find: there's no option -i (text help lists them)"

echo "== text sort, unique"
is "sort" "text sort b.txt" "$(printf 'a\nb\nb\nb\nc')"
is "sort --reverse" "text sort --reverse b.txt" "$(printf 'c\nb\nb\nb\na')"
is "sort --unique" "text sort --unique b.txt" "$(printf 'a\nb\nc')"
is "sort as text: 9 last" "text sort n.txt | text last 1" "9"
is "sort --numbers: 12 last" "text sort --numbers n.txt | text last 1" "12"
is "sort --numbers --field 2" "text sort --numbers --field 2 f.txt" "$(printf 'y 1\nz 2\nx 3')"
is "unique drops repeats next to each other" "text unique b.txt" "$(printf 'b\nc\nb\na')"
is "unique --count" "text unique --count b.txt" "$(printf '      1 b\n      1 c\n      2 b\n      1 a')"
is "unique --repeated" "text unique --repeated b.txt" "b"
is "unique --count is a table for filter and get" "text sort b.txt | text unique --count | filter count > 1 | get line" "b"

echo "== text count"
check "count: a table with a header" "text count a.txt" "lines  words  bytes  file"
check "count: the numbers" "text count a.txt" "    1      3     14  a.txt"
is "count --lines on one file: just the number" "text count --lines n.txt" "12"
is "count --lines on a pipe" "text show n.txt | text count --lines" "12"
is "count --words" "text count --words a.txt" "3"
check "several files: a total row" "text count a.txt b.txt" "total"
is "the table works with filter and get" "text count a.txt b.txt | filter file == total | get lines" "6"
check "a table for msh" "MILK_TABLE=1 text count a.txt" "lines:int	words:int	bytes:int	file:str"

echo "== text help and mistakes"
check "text alone shows the help" "text" "usage: text COMMAND ..."
check "text help: show" "text help" "  show FILE...            print files one after another (--numbers: number the lines)"
check "text help: find" "text help" "  find TEXT [FILE...]     the lines with TEXT in them: --ignore-case --invert --numbers"
check "text help: count" "text help" "  count [FILE...]         lines, words and bytes, as a table: --lines --words --bytes"
check "an unknown verb" "text frobnicate" "text: there's no text frobnicate (text help lists them)"
check "an option another verb has" "text show --invert a.txt" "text show: there's no option --invert (text help lists them)"

echo "== pause"
is "pause 1s" 'pause 1s; print rc=$?' "rc=0"
is "pause 500ms" "pause 500ms && print half-ok" "half-ok"
is "pause 0.2" "pause 0.2 && print fifth-ok" "fifth-ok"
check "a bad length" "pause 5x" "pause: 5x isn't a length of time (try 5, 5s, 2m or 500ms)"
check "no length" "pause" "pause: how long? pause 5, pause 5s, pause 2m or pause 500ms"
check "pause help" "pause help" "  pause 500ms             half a second (0.5 works too)"
# The seconds a pause took, from the serial log's own timing is too coarse; utest covers
# nanosleep. Here: it returns, with 0.

echo "== variables"
is "set, then \$NAME" 'print got $GREETING' "got hello"
is "get" "variables get GREETING" "hello"
is "set exports: a child shell sees it" "msh -c 'print child sees \$GREETING'" "child sees hello"
check "list: NAME=value lines" "variables list" "LOCAL=1"
check "list: the exported ones too" "variables list" "GREETING=hello"
check "list as a table" "MILK_TABLE=1 variables list" "name:str	value:str	exported:str"
is "NAME=value alone: not exported" "variables list | filter name == LOCAL | get exported" "no"
is "variables set: exported" "variables list | filter name == GREETING | get exported" "yes"
check "a child doesn't see a shell-only variable" "msh -c 'variables get LOCAL'" "variables get: LOCAL isn't set"
is "set NAME exports one that is set" "msh -c 'variables get LOCAL'" "1" 2
check "remove, then get" "variables get GREETING" "variables get: GREETING isn't set" 2
check "removing one that isn't set says so" "variables remove GREETING" "variables remove: GREETING isn't set" 2
is "run NAME=value CMD" "variables run WHO=cow msh -c 'print hi \$WHO'" "hi cow"
check "and it isn't left behind" "variables get WHO" "variables get: WHO isn't set"
is "run in a pipe" "variables run WHO=pipe msh -c 'print through a \$WHO' | text show" "through a pipe"
check "run without a command" "variables run A=1" "variables run: a command is needed (variables run NAME=value... COMMAND)"
check "a bad name" "variables set 1X=2" "variables set: 1X isn't a variable name"
check "an unknown verb" "variables frob" "variables: there's no variables frob (variables help lists them)"
check "variables help" "variables help" "  run NAME=value... CMD   run CMD with those set, for CMD only"
check "variables alone lists them" "variables" "PATH=/Commands"

echo "== the old names are gone"
# (out stops at a line starting "msh:", so these look at the line after the command.)
gone() { echo "$L" | grep -A1 -F "\$ $1" | grep -Fq "msh: ${1%% *}: command not found" && ok "${1%% *} is gone" || bad "${1%% *} still runs: $(echo "$L" | grep -A1 -F "\$ $1" | tail -1)"; }
gone "cat a.txt"; gone "echo hi"; gone "env"; gone "grep milk m.txt"; gone "head n.txt"; gone "tail n.txt"
gone "sort b.txt"; gone "uniq b.txt"; gone "wc a.txt"; gone "sleep 1"; gone "export X=1"; gone "set"
gone "unset LOCAL"
check "msh help: variables" "help" "  variables        list, get NAME, remove NAME, run NAME=v CMD   (variables help)"
check "msh help: text, print, pause" "help" "programs (/Commands): files trash text print pause"

[ "$FAIL" = 0 ] && { echo "text-test: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; } || { echo "text-test: FAIL (logs in $T)"; exit 1; }
