#!/usr/bin/env bash
# `files owner` and `files access` end to end (they replaced chown and chmod), one boot on a
# private milkfs image, on the serial console (root) and as milk and nobody:
#   help; owner: show, USER, USER:GROUP, :GROUP, numbers, an unknown user and group, a bad
#   spec, a missing path, a link (follows), --all (links left alone, a link and / refused);
#   milk (not root) refused another owner and a group it isn't in, allowed a group it is in,
#   nobody refused the group and access of milk's file, milk through admin gives a file
#   away; access: every word on files and folders (several paths at once), the sentences
#   (folders, merged classes, set-ID, sticky), runnable / not-runnable (a runnable file keeps
#   running under every word), runnable on a folder refused, --all runnable (files only),
#   runnable with nobody able to read, --mode NNNN and --mode=NNNN, set-user-ID through
#   --mode and a word dropping it (with a note), the kernel's set-group-ID rule (left off,
#   with a note), a non-admin group change clearing set-ID, bad modes, a word and --mode
#   together, a bad word, usage; --all with a word (runnable stays, links left alone), a
#   mode that locks the owner out (children first) and back (folder first); files info and
#   the files list table; chmod and chown are "command not found".
# Fresh /tmp directory; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d "${TMPDIR:-/tmp}/milk-access.XXXXXX")"
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
    -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
    -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" "$@" "send:print ACCESS""-DONE" "wait:ACCESS-DONE" "done:$T/$name.done" &
  DPID=$!
  for _ in $(seq 1200); do [ -e "$T/$name.done" ] && break; sleep 0.2; done
  sleep 1
  kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
  kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
  [ -e "$T/$name.done" ] || bad "$name: the session did not finish"
  python3 -I scripts/console_clean.py "$LOG"
}
trap 'kill -9 "${QPID:-}" 2>/dev/null || true' EXIT

# Steps: "send:CMD" "wait:PROMPT" for each command, appended to the array named first.
s() { local a="$1" c; shift; for c in "$@"; do eval "$a+=(\"send:\$c\" \"wait:\$P\")"; done; }
M="users switch milk --command"
NB="admin --user nobody"
D=/tmp/acc
B=(); s B \
  "pause 1" \
  "files help" \
  "files create $D/ $D/notes.txt $D/sub/deep/ $D/sub/a.txt $D/sub/deep/b.txt $D/o1 $D/o2 $D/o3 $D/o4" \
  "files link $D/notes.txt $D/latest" \
  "files link $D/nothing $D/dangle" \
  "files owner $D/notes.txt" \
  "files owner $D/o1 milk" \
  "files owner $D/o2 milk:staff" \
  "files owner $D/o3 :admin" \
  "files owner $D/o4 501:20" \
  "files list -l $D" \
  "files owner $D/o1 bob" \
  "files owner $D/o1 :devs" \
  "files owner $D/o1 a:b:c" \
  "files owner $D/missing milk" \
  "files owner $D/latest" \
  "files owner $D/latest --all milk" \
  "files owner / --all root" \
  "files owner $D --all milk:staff" \
  "files owner $D/sub/deep/b.txt" \
  "files list -l $D" \
  "$M 'files owner $D/notes.txt nobody'" \
  "$M 'files owner $D/notes.txt :wheel'" \
  "$M 'files owner $D/notes.txt :admin'" \
  "files owner $D/notes.txt" \
  "$NB files owner $D/notes.txt :nogroup" \
  "$NB files access $D/notes.txt open" \
  "$M 'files access $D/notes.txt private'" \
  "$M 'print milk | admin --stdin files owner $D/o1 nobody'" \
  "files owner $D/o1" \
  "$M 'files create $D/w/ $D/w/f-private $D/w/f-shared $D/w/f-group $D/w/f-ro $D/w/f-open $D/w/f-640 $D/w/f-200 $D/w/d-private/ $D/w/d-shared/ $D/w/d-group/ $D/w/d-ro/ $D/w/d-open/'" \
  "files access $D/w/f-private $D/w/d-private private" \
  "files access $D/w/f-shared $D/w/d-shared shared" \
  "files access $D/w/f-group $D/w/d-group group" \
  "files access $D/w/f-ro $D/w/d-ro read-only" \
  "files access $D/w/f-open $D/w/d-open open" \
  "files access $D/w/f-640 --mode=0640" \
  "files access $D/w/f-200 --mode 200" \
  "files list -l $D/w" \
  "files access $D/w/f-group" \
  "files access $D/w/d-shared" \
  "files access $D/w/f-640" \
  "files access $D/w/f-ro" \
  "files access $D/w/d-open" \
  "files access /tmp" \
  "files access $D/w/f-200 runnable" \
  "$M 'files create $D/r/ $D/r/rs-shared $D/r/rs-private $D/r/rs-group $D/r/rs-ro $D/r/rs-open $D/r/nr $D/r/plain'" \
  "files access $D/r/rs-shared $D/r/rs-private $D/r/rs-group $D/r/rs-ro $D/r/rs-open $D/r/nr runnable" \
  "files access $D/r/rs-shared shared" \
  "files access $D/r/rs-private private" \
  "files access $D/r/rs-group group" \
  "files access $D/r/rs-ro read-only" \
  "files access $D/r/rs-open open" \
  "files access $D/r/nr not-runnable" \
  "files access $D/r/rs-shared" \
  "files list -l $D/r" \
  "files access $D/r runnable" \
  "files access $D/r --all runnable" \
  "files list -l $D/r" \
  "files create $D/s/ $D/s/suid $D/s/sgid" \
  "files access $D/s/suid --mode 4755" \
  "files access $D/s/suid" \
  "files info $D/s/suid" \
  "files access $D/s/suid shared" \
  "files info $D/s/suid" \
  "files owner $D/s/sgid milk:wheel" \
  "$M 'files access $D/s/sgid --mode 2755'" \
  "files info $D/s/sgid" \
  "files owner $D/s/suid milk:staff" \
  "files access $D/s/suid --mode 4755" \
  "$M 'files owner $D/s/suid :admin'" \
  "files info $D/s/suid" \
  "files access $D/s/suid --mode 9999" \
  "files access $D/s/suid --mode" \
  "files access $D/s/suid private --mode 644" \
  "files access $D/s/suid public" \
  "files access" \
  "files owner" \
  "$M 'files create $D/t/a/b/ $D/t/a/f.txt $D/t/a/b/g.txt $D/t/top.txt'" \
  "$M 'files access $D/t/top.txt runnable'" \
  "$M 'files link $D/t/top.txt $D/t/a/ln'" \
  "$M 'files access $D/t --all private'" \
  "files list -l $D/t $D/t/a $D/t/a/b" \
  "$M 'files access $D/t --all --mode 640'" \
  "files list -l $D/t/a/b" \
  "$M 'files access $D/t --all shared'" \
  "files list -l $D/t $D/t/a/b" \
  "files access $D/latest --all private" \
  "files access / --all open" \
  "files access $D/latest" \
  "files access $D/dangle" \
  "files info $D/notes.txt" \
  "MILK_TABLE=1 files list $D" \
  "chmod 644 $D/notes.txt" \
  "chown milk $D/notes.txt" \
  "files info /Commands/chmod /Commands/chown"
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
# A `files list -l` line: mode, owner, group, then the name at the end.
row() { # description command regex [nth]
  if out "$2" "${4:-1}" | grep -Eq -- "$3"; then ok "$1"; else bad "$1 (wanted /$3/ after '$2', got: $(out "$2" "${4:-1}" | tr '\n' '|'))"; fi
}

echo "== help"
check "help: owner" "files help" "owner PATH [WHO]        who owns it; WHO (USER, USER:GROUP or :GROUP) changes that"
check "help: access" "files help" "access PATH [HOW]       who may use it, in words; HOW (a word below) changes that"
check "help: the words" "files help" "Access words (file / folder): private 600/700, shared 644/755, group 660/770,"

echo "== files owner"
check "show: owner (group: group)" "files owner $D/notes.txt" "root (group: wheel)"
row "USER" "files list -l $D" "milk +wheel .* o1$"
row "USER:GROUP" "files list -l $D" "milk +staff .* o2$"
row ":GROUP" "files list -l $D" "root +admin .* o3$"
row "numbers" "files list -l $D" "milk +staff .* o4$"
check "an unknown user" "files owner $D/o1 bob" "files: there's no user called bob (users list shows them)"
check "an unknown group" "files owner $D/o1 :devs" "files: there's no group called devs"
check "a bad spec" "files owner $D/o1 a:b:c" "a:b:c isn't an owner: use USER, USER:GROUP or :GROUP"
check "a missing path" "files owner $D/missing milk" "$D/missing doesn't exist"
check "a link means what it links to" "files owner $D/latest" "$D/latest -> $D/notes.txt: root (group: wheel)"
check "--all never follows a link" "files owner $D/latest --all milk" "$D/latest is a link, and --all never follows links: name what it links to ($D/notes.txt)"
check "--all on / is refused" "files owner / --all root" "won't change everything on the disk with --all: name a folder"
check "--all: everything inside, links left alone" "files owner $D --all milk:staff" "$D: changed the owner of 10 items, left 2 links alone"
check "--all reached the deepest file" "files owner $D/sub/deep/b.txt" "milk (group: staff)"
row "--all didn't change the link itself" "files list -l $D" "link +rwxrwxrwx +root +wheel .* latest -> $D/notes.txt" 2
check "milk can't give a file away" "$M 'files owner $D/notes.txt nobody'" "files: only an admin can change who owns $D/notes.txt (admin files owner $D/notes.txt ... does it)"
check "milk can't use a group it isn't in" "$M 'files owner $D/notes.txt :wheel'" "files: you're not in the group wheel, so you can't give $D/notes.txt to it"
check "milk can use a group it is in" "files owner $D/notes.txt" "milk (group: admin)" 2
check "nobody can't change the group of milk's file" "$NB files owner $D/notes.txt :nogroup" "files: you can't change the group of $D/notes.txt: only its owner (milk) or an admin can"
check "nobody can't change the access of milk's file" "$NB files access $D/notes.txt open" "files: you can't change who may use $D/notes.txt: only its owner (milk) or an admin can"
check "milk through admin gives a file away" "files owner $D/o1" "nobody (group: staff)"

echo "== files access: the words"
row "private file 600" "files list -l $D/w" "rw------- +milk +staff .* f-private$"
row "shared file 644" "files list -l $D/w" "rw-r--r-- +milk +staff .* f-shared$"
row "group file 660" "files list -l $D/w" "rw-rw---- +milk +staff .* f-group$"
row "read-only file 444" "files list -l $D/w" "r--r--r-- +milk +staff .* f-ro$"
row "open file 666" "files list -l $D/w" "rw-rw-rw- +milk +staff .* f-open$"
row "private folder 700" "files list -l $D/w" "rwx------ +milk +staff .* d-private$"
row "shared folder 755" "files list -l $D/w" "rwxr-xr-x +milk +staff .* d-shared$"
row "group folder 770" "files list -l $D/w" "rwxrwx--- +milk +staff .* d-group$"
row "read-only folder 555" "files list -l $D/w" "r-xr-xr-x +milk +staff .* d-ro$"
row "open folder 777" "files list -l $D/w" "rwxrwxrwx +milk +staff .* d-open$"
row "--mode=0640" "files list -l $D/w" "rw-r----- +milk +staff .* f-640$"
check "sentence: owner and group together" "files access $D/w/f-group" "milk and staff can read and change it, everyone else can't"
check "sentence: a folder" "files access $D/w/d-shared" "milk can open, list and change it, everyone else can open and list it"
check "sentence: three classes" "files access $D/w/f-640" "milk can read and change it, staff can read it, everyone else can't"
check "sentence: everyone" "files access $D/w/f-ro" "everyone can read it"
check "sentence: an open folder" "files access $D/w/d-open" "everyone can open, list and change it"
check "sentence: sticky" "files access /tmp" "everyone can open, list and change it; only their owners can delete or rename things in it (sticky)"
check "runnable when nobody can read" "files access $D/w/f-200 runnable" "files: nobody can read $D/w/f-200, so it can't be made runnable (first: files access $D/w/f-200 private)"

echo "== runnable"
check "sentence: runnable" "files access $D/r/rs-shared" "milk can read, change and run it, everyone else can read and run it"
row "runnable then shared: 755" "files list -l $D/r" "rwxr-xr-x .* rs-shared$"
row "runnable then private: 700" "files list -l $D/r" "rwx------ .* rs-private$"
row "runnable then group: 770" "files list -l $D/r" "rwxrwx--- .* rs-group$"
row "runnable then read-only: 555" "files list -l $D/r" "r-xr-xr-x .* rs-ro$"
row "runnable then open: 777" "files list -l $D/r" "rwxrwxrwx .* rs-open$"
row "not-runnable: 644" "files list -l $D/r" "rw-r--r-- .* nr$"
check "runnable on a folder is refused" "files access $D/r runnable" "files: $D/r is a folder: folders are opened, not run (for the files in it: files access $D/r --all runnable)"
check "--all runnable: the files in it" "files access $D/r --all runnable" "$D/r: set access on 7 items"
row "--all runnable made nr runnable" "files list -l $D/r" "rwxr-xr-x .* nr$" 2
row "--all runnable made plain runnable" "files list -l $D/r" "rwxr-xr-x .* plain$" 2

echo "== --mode and set-ID"
check "sentence: set-user-ID" "files access $D/s/suid" "root can read, change and run it, everyone else can read and run it; it runs as root (set-user-ID)"
check "--mode 4755" "files info $D/s/suid" "mode         rwsr-xr-x (4755)"
check "a word drops set-ID, and says so" "files access $D/s/suid shared" "files: note: $D/s/suid no longer runs as its owner or group (set-ID is only set with --mode)"
check "shared on a set-ID program: 755" "files info $D/s/suid" "mode         rwxr-xr-x (755)" 2
check "set-group-ID outside your group is left off, and says so" "$M 'files access $D/s/sgid --mode 2755'" "files: note: set-group-ID was left off $D/s/sgid: only members of its group (wheel) can set it"
check "so it is 755" "files info $D/s/sgid" "mode         rwxr-xr-x (755)"
check "a non-admin group change clears set-ID" "files info $D/s/suid" "mode         rwxr-xr-x (755)" 3
check "and the group changed" "files info $D/s/suid" "owner        milk (group: admin)" 3
check "a bad mode" "files access $D/s/suid --mode 9999" "files: --mode wants an octal number like 644, 755 or 4755 (up to four digits 0-7), not 9999"
check "--mode with nothing after it" "files access $D/s/suid --mode" "files access: --mode without a mode (--mode 644)"
check "a word and --mode" "files access $D/s/suid private --mode 644" "files: use a word or --mode, not both"
check "a bad word" "files access $D/s/suid public" "files: public isn't a way to set access: use private, shared, group, read-only, open, runnable or not-runnable (or --mode NNNN)"
check "usage: access" "files access" "usage: files access PATH"
check "usage: owner" "files owner" "usage: files owner PATH"

echo "== --all"
check "--all private: everything, links left alone" "$M 'files access $D/t --all private'" "$D/t: set access on 6 items, left 1 link alone"
row "folders get 700" "files list -l $D/t $D/t/a $D/t/a/b" "rwx------ .* a$"
row "a runnable file stays runnable (700)" "files list -l $D/t $D/t/a $D/t/a/b" "rwx------ .* top.txt$"
row "files get 600" "files list -l $D/t $D/t/a $D/t/a/b" "rw------- .* g.txt$"
row "the link is unchanged" "files list -l $D/t $D/t/a $D/t/a/b" "link +rwxrwxrwx .* ln -> $D/t/top.txt"
check "--mode 640 locks the owner out of the folders: children first" "$M 'files access $D/t --all --mode 640'" "$D/t: set access on 6 items, left 1 link alone"
row "the deepest file got 640" "files list -l $D/t/a/b" "rw-r----- .* g.txt$"
check "and back to shared: folders first" "$M 'files access $D/t --all shared'" "$D/t: set access on 6 items, left 1 link alone"
row "the folders are 755 again" "files list -l $D/t $D/t/a/b" "rwxr-xr-x .* a$"
row "the deepest file is 644" "files list -l $D/t $D/t/a/b" "rw-r--r-- .* g.txt$"
check "--all never follows a link" "files access $D/latest --all private" "files: $D/latest is a link, and --all never follows links: name what it links to ($D/notes.txt)"
check "--all on / is refused" "files access / --all open" "files: won't change everything on the disk with --all: name a folder"
check "a link means what it links to" "files access $D/latest" "$D/latest -> $D/notes.txt: milk can read and change it, everyone else can't"
check "a link to nothing" "files access $D/dangle" "files: $D/dangle is a link to $D/nothing, which doesn't exist"

echo "== files info, files list, the old names"
check "info: owner" "files info $D/notes.txt" "owner        milk (group: admin)"
check "info: access in words" "files info $D/notes.txt" "access       milk can read and change it, everyone else can't"
check "info: the mode beside it" "files info $D/notes.txt" "mode         rw------- (600)"
check "the files list table has an access column" "MILK_TABLE=1 files list $D" "name:str	kind:str	size:size	access:str	owner:str"
gone() { echo "$L" | grep -A1 -F "\$ $1 " | grep -Fq "msh: $1: command not found" && ok "$1 is gone" || bad "$1 still runs: $(echo "$L" | grep -A1 -F "\$ $1 " | tail -1)"; }
gone chmod; gone chown
check "and not in /Commands" "files info /Commands/chmod /Commands/chown" "/Commands/chmod doesn't exist"
check "chown too" "files info /Commands/chmod /Commands/chown" "/Commands/chown doesn't exist"

[ "$FAIL" = 0 ] && { echo "files-access-test: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; } || { echo "files-access-test: FAIL (logs in $T)"; exit 1; }
