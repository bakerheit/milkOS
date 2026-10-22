#!/usr/bin/env bash
# `files` and `trash` end to end, on private images (a milkfs volume and a FAT disk), two boots:
#   boot 1: create (file, folder/ with parents, touch an existing file), copy (a folder, into
#           a folder, --replace), move (rename, across disks: copy + remove), rename, link,
#           list (columns, --long, --all, a table for msh), info; the trailing-slash rules
#           (a slash on a file is an error, a folder works with or without one); delete to
#           the Trash, a name collision in the Trash (nothing overwritten), trash list (and
#           as a table), restore (and the refusal when something is back in the way), an
#           item with no record, --now, deleting from the Trash, /tmp and the FAT disk
#           refused with the --now hint, a disk's own Trash (a milkfs clone), trash empty
#           (asks; --yes doesn't); auto-empty: trashd --once with Settings' days at 1 and a
#           pretend clock (--later), and "never"; then an item whose record says it was
#           deleted in 2001 is left in the Trash; msh's go and where: relative, .., absolute,
#           go back, ./back, home, in a pipe and a redirection, the plain-English errors (and
#           "you can't open" as another user), cd and pwd gone, `where` with arguments is
#           an error (no stray file from its `>`), the table filter is `filter COL OP V`;
#   boot 2: trashd (a milkd service) deleted it at boot, says so in the journal, and the
#           Trash's other item is still there.
# Fresh /tmp directory; only the qemus started here are killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d "${TMPDIR:-/tmp}/milk-files.XXXXXX")"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/fs.img"
python3 scripts/mkmilkfs.py mkfs "$IMG" --size 64M --label Milk >/dev/null
FAT="$T/fat.img"
scripts/mkfat.sh "$FAT" 40 FATT >/dev/null
P='msh:[^\n]*\$ '

boot() { # name steps...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server,nowait" \
    -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
    -drive "file=$FAT,if=none,format=raw,id=hd1" -device virtio-blk-device,drive=hd1 2>/dev/null &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" "$@" "send:print FILES""-DONE" "wait:FILES-DONE" "done:$T/$name.done" &
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
B1=(); s B1 \
  "go" \
  "files create notes.txt projects/a/b/" \
  "print hello > notes.txt" \
  "files create notes.txt" \
  "text show notes.txt" \
  "files list" \
  "files list -l" \
  "MILK_TABLE=1 files list" \
  "files info notes.txt" \
  "files copy notes.txt projects" \
  "files copy projects backup" \
  "files list backup backup/a" \
  "files copy notes.txt backup/notes.txt" \
  "print replaced > other.txt" \
  "files copy --replace other.txt backup/notes.txt" \
  "text show backup/notes.txt" \
  "files move backup/notes.txt moved.txt" \
  "files rename moved.txt renamed.txt" \
  "files rename renamed.txt a/b" \
  "files link renamed.txt latest" \
  "text show latest" \
  "files copy backup backup/inner" \
  "files delete notes.txt/" \
  "files create renamed.txt/" \
  "files create projects" \
  "files info projects/" \
  "files delete nothing.txt" \
  "files delete notes.txt" \
  "files delete projects" \
  "files delete backup/" \
  "files create notes.txt" \
  "files delete notes.txt" \
  "files list -a ~/.Trash" \
  "trash list" \
  "MILK_TABLE=1 trash list" \
  "trash restore notes.txt" \
  "text show notes.txt" \
  "trash restore notes.txt" \
  "trash restore ~/projects" \
  "files list projects/a" \
  "print stray > ~/.Trash/stray.txt" \
  "trash restore stray.txt" \
  "files delete ~/.Trash/stray.txt" \
  "files delete --now renamed.txt" \
  "files list -a" \
  "files move projects /tmp/projects" \
  "files list /tmp/projects/a" \
  "files list -a ~" \
  "files delete /tmp/projects" \
  "files delete --now /tmp/projects" \
  "files create /Volumes/FATT/x.txt" \
  "files delete /Volumes/FATT/x.txt" \
  "files delete --now /Volumes/FATT/x.txt" \
  "files list /Volumes/FATT" \
  "snap create files1" \
  "snap clone files1 ftry" \
  "files create /Volumes/Milk-ftry/vol.txt" \
  "files delete /Volumes/Milk-ftry/vol.txt" \
  "files list -a /Volumes/Milk-ftry/.Trash" \
  "trash list" \
  "trash restore vol.txt" \
  "files list /Volumes/Milk-ftry" \
  "snap discard ftry" \
  "trash list"
B1+=("send:trash empty" "wait:\[y/N\]" "send:n" "wait:$P")
s B1 \
  "trash list" \
  "files create e1.txt e2.txt" \
  "files delete e1.txt e2.txt" \
  "trash empty --yes" \
  "trash list" \
  "files create ~/.config/milk/" \
  "print 'trash = 1' > ~/.config/milk/settings" \
  "files create old.txt" \
  "files delete old.txt" \
  "/System/Services/trashd --once" \
  "trash list" \
  "/System/Services/trashd --once --later 2" \
  "trash list" \
  "print 'trash = never' > ~/.config/milk/settings" \
  "files create keep.txt" \
  "files delete keep.txt" \
  "/System/Services/trashd --once --later 400" \
  "trash list" \
  "print 'trash = 1' > ~/.config/milk/settings" \
  "files create boot.txt" \
  "files delete boot.txt" \
  "print path = /Users/milk/boot.txt > ~/.Trash/.trashinfo/boot.txt" \
  "print deleted = 1000000000 >> ~/.Trash/.trashinfo/boot.txt" \
  "trash list" \
  "files create gotest/back/ gotest/inner/ gotest/notes.txt" \
  "go gotest/inner" \
  "where | text show" \
  "go .." \
  "where" \
  "go back" \
  "where > /tmp/where.txt" \
  "text show /tmp/where.txt" \
  "go /Apps" \
  "where" \
  "go /Users/milk/gotest" \
  "go ./back" \
  "where" \
  "go" \
  "where" \
  "go gotest/missing" \
  "go gotest/notes.txt" \
  "go gotest/notes.txt/x" \
  "go gotest inner" \
  "OLDPWD= msh -c 'go back'" \
  "files create /tmp/locked/" \
  "chmod 700 /tmp/locked" \
  "users switch milk --command 'go /tmp/locked'" \
  "cd /tmp" \
  "pwd" \
  "where" \
  "where cpu > 5" \
  "files info 5" \
  "processes list | filter name == msh | select name" \
  "help" \
  "print now > /System/Kernel/fs/milkfs/sync"
L1="$(boot b1 "${B1[@]}")"
echo "$L1" > "$T/b1.txt"

# The output of the first (or Nth) run of a command typed at the prompt.
out() { echo "$L1" | awk -v c="$1" -v n="${2:-1}" '
  index($0, "$ " c) && substr($0, index($0, "$ " c) + length(c) + 2) == "" { k++; on = (k == n); next }
  /^msh:/ { on = 0 } on'; }
has() { out "$1" "${3:-1}" | grep -Fq -- "$2"; }
check() { # description command text [nth]
  if has "$2" "$3" "${4:-1}"; then ok "$1"; else bad "$1 (wanted '$3' after '$2', got: $(out "$2" "${4:-1}" | head -3 | tr '\n' '|'))"; fi
}

echo "== create, copy, move, rename, link, list, info"
check "create makes a file and a folder with its parents; create on a file sets its time" "files list" "projects/"
[ "$(out "text show notes.txt" | tr -d ' ')" = "hello" ] && ok "create on an existing file leaves it be" || bad "create on an existing file"
check "files list --long: kind, permissions, owner, size, date" "files list -l" "file    rw-r--r--  root  wheel        6"
check "files list gives msh a table" "MILK_TABLE=1 files list" "name:str	kind:str	size:size"
check "files info: kind and size" "files info notes.txt" "kind         file"
check "files info: where" "files info notes.txt" "where        /Users/milk/notes.txt"
check "files info: owner" "files info notes.txt" "owner        root, group wheel"
check "copy into a folder, a folder with everything in it" "files list backup backup/a" "notes.txt"
check "copy refuses to overwrite" "files copy notes.txt backup/notes.txt" "backup/notes.txt already exists (add --replace"
check "copy --replace overwrites" "text show backup/notes.txt" "replaced"
check "move, rename, link" "text show latest" "replaced"
check "rename wants a name, not a path" "files rename renamed.txt a/b" "a/b is a path, not a name"
check "a folder can't be copied into itself" "files copy backup backup/inner" "can't copy backup into itself"

echo "== the trailing slash"
check "a slash on a file is an error" "files delete notes.txt/" "notes.txt is a file, not a folder"
check "create renamed.txt/ when it is a file" "files create renamed.txt/" "renamed.txt is a file, not a folder"
check "create projects (a file) when it is a folder" "files create projects" "projects is a folder, not a file"
check "a folder with a slash is fine" "files info projects/" "kind         folder"
check "plain English for a missing file" "files delete nothing.txt" "nothing.txt doesn't exist"

echo "== the Trash"
check "deleted things are in ~/.Trash, with their records" "files list -a ~/.Trash" ".trashinfo/"
check "a second notes.txt in the Trash gets a free name" "files list -a ~/.Trash" "notes 2.txt"
check "a folder deleted without its slash, and one with" "files list -a ~/.Trash" "backup/"
check "trash list: name, where from, when, days left" "trash list" "notes 2.txt  ~/notes.txt"
has "trash list" "today" && has "trash list" "30" && ok "trash list: deleted today, 30 days left" || bad "trash list dates"
check "trash list as a table" "MILK_TABLE=1 trash list" "name:str	from:str	deleted:str	days_left:int"
check "restore puts it back" "trash restore notes.txt" "put notes.txt back: /Users/milk/notes.txt"
[ "$(out "text show notes.txt" 2 | tr -d ' ')" = "hello" ] && ok "the restored file is the first one" || bad "restored content: $(out "text show notes.txt" 2)"
check "restore refuses when something is there" "trash restore notes.txt" "/Users/milk/notes.txt already exists, so notes 2.txt can't go back there" 2
check "restore by the path it had" "files list projects/a" "b/"
check "an item with no record says so" "trash restore stray.txt" "stray.txt has no record of where it came from"
check "deleting from the Trash is for good" "files delete ~/.Trash/stray.txt" "was in the Trash: deleted for good"
out "files list -a" | grep -q "renamed.txt" && bad "--now left renamed.txt" || ok "delete --now deletes for good"
out "trash list" 2 | grep -q "renamed" && bad "--now went to the Trash" || ok "delete --now skipped the Trash"
check "move across disks (copy, then remove)" "files list /tmp/projects/a" "b/"
out "files list -a ~" | grep -q "projects" && bad "the moved folder is still in ~" || ok "the original is gone after the move"
check "/tmp has no Trash: says so, offers --now" "files delete /tmp/projects" "To delete it for good: files delete --now /tmp/projects"
check "a FAT disk has no Trash: says so" "files delete /Volumes/FATT/x.txt" "/Volumes/FATT is a FAT disk, which has no Trash"
out "files list /Volumes/FATT" | grep -q "x.txt" && bad "--now on FAT" || ok "--now works on the FAT disk"
check "a disk's own Trash: /Volumes/<disk>/.Trash" "files list -a /Volumes/Milk-ftry/.Trash" "vol.txt"
check "trash list shows the disk's Trash too" "trash list" "/Volumes/Milk-ftry/vol.txt" 2
check "restore from a disk's Trash" "trash restore vol.txt" "put vol.txt back: /Volumes/Milk-ftry/vol.txt"
check "trash empty asks first" "trash empty" "Delete 2 items in the Trash for good? This can't be undone."
check "and answering no deletes nothing" "trash list" "notes 2.txt" 4
check "trash empty --yes" "trash empty --yes" "Deleted 4 items for good. The Trash is empty."
check "empty" "trash list" "The Trash is empty." 5

echo "== auto-empty (trashd)"
check "trashd: nothing is a day old yet" "trash list" "old.txt" 6
check "trashd --later 2 with trash = 1: deleted for good" "/System/Services/trashd --once --later 2" "/Users/milk/.Trash: deleted 1 item for good (after 1 day)"
check "and it is gone" "trash list" "The Trash is empty." 7
check "trash = never: kept 400 days later" "trash list" "keep.txt" 8
check "days left: never" "trash list" "never" 8
check "an item from 2001 is due" "trash list" "boot.txt" 9

echo "== go and where"
check "go into a folder two deep; where in a pipe" "where | text show" "/Users/milk/gotest/inner"
check "go .." "where" "/Users/milk/gotest"
check "go back, where > file" "text show /tmp/where.txt" "/Users/milk/gotest/inner"
check "go /Apps (absolute)" "where" "/Apps" 2
check "go ./back reaches a folder called back" "where" "/Users/milk/gotest/back" 3
[ "$(out "where" 4 | tr -d ' \r')" = "/Users/milk" ] && ok "go with no folder goes home" || bad "go home: $(out "where" 4)"
check "a missing folder" "go gotest/missing" "go: gotest/missing doesn't exist"
check "a file" "go gotest/notes.txt" "go: gotest/notes.txt is a file, not a folder"
check "a file in the middle of the path" "go gotest/notes.txt/x" "go: gotest/notes.txt/x doesn't exist (part of its path is a file, not a folder)"
check "one folder at a time" "go gotest inner" "go takes one folder (put quotes around a name with spaces)"
check "go back with nowhere to go back to" "OLDPWD= msh -c 'go back'" "go: there's no folder to go back to yet"
check "a folder you can't open" "users switch milk --command 'go /tmp/locked'" "go: you can't open /tmp/locked"
# (out stops at a line starting "msh:", so these look at the line after the command.)
gone() { echo "$L1" | grep -A1 -F "\$ $1" | grep -Fq "msh: $1: command not found" && ok "$1 is gone" || bad "$1 still runs: $(echo "$L1" | grep -A1 -F "\$ $1" | tail -1)"; }
gone cd; gone pwd
check "a failed go stays put" "where" "/Users/milk" 5
check "where with arguments is an error" "where cpu > 5" "where takes no arguments; to filter a table use filter"
check "and its > made no file" "files info 5" "5 doesn't exist"
check "filter COL OP V is the table filter" "processes list | filter name == msh | select name" "msh"
check "help: go" "help" "go FOLDER        change folder: go Documents, go .., go /Apps"
check "help: where" "help" "where            print the folder you're in"
check "help: filter" "help" "structured pipes (docs/shell.md): filter COL OP V, sort-by COL [-r]"

echo "== boot 2: trashd ran at boot"
L2="$(boot b2 \
  "send:pause 2" "wait:$P" \
  "send:trash list" "wait:$P" \
  "send:services logs trashd" "wait:$P" \
  "send:services status trashd" "wait:$P")"
echo "$L2" > "$T/b2.txt"
L1="$L2"
out "trash list" | grep -q "boot.txt" && bad "boot.txt still in the Trash after boot" || ok "trashd deleted the 2001 item at boot"
check "the other item is still there" "trash list" "keep.txt"
check "trashd says so in the journal" "services logs trashd" "/Users/milk/.Trash: deleted 1 item for good (after 1 day)"
check "trashd is a running milkd service" "services status trashd" "running"

[ "$FAIL" = 0 ] && { echo "files-test: PASS"; rm -rf "$T"; } || { echo "files-test: FAIL (logs in $T)"; exit 1; }
