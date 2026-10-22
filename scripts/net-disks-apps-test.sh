#!/usr/bin/env bash
# `network`, `disks`, `apps` and `search` end to end, every subcommand, on private images (a
# milkfs volume, a second milkfs volume "Spare" and a blank disk; qemu lists them as vdc,
# vdb and vda), one boot with a restricted user netdev (DHCP, no host, no internet):
#   network: help, status (a link, a missing link, --keys, a table), connections (--tcp
#            --listening, a table), counters (a table), ping (IPv4 and IPv6 loopback, a bad
#            count), get (the guest's own httpd, --headers, --save, refusing to overwrite,
#            --replace, https refused), listen + connect over loopback, name (set, show,
#            a bad name, --local), hide, show, a misspelled option and subcommand;
#   disks:   help, list (--human, --all, a table, a missing path), devices (a table), info,
#            snapshot, snapshots (a table), changes (two snapshots, one and now), clone,
#            clones (a table), discard, keep, rollback, delete, auto, check, verify (and
#            status, a setting), dedupe (--dry-run, then for real), send (to a file and to
#            stdout), receive (from a file into Spare, from stdin refused: already there),
#            encrypt Spare in place (--passphrase-file, --wait), lock, unlock (the
#            passphrase from a pipe), decrypt (--wait), format the blank disk and refuse
#            the mounted one, passphrase on a plain volume refused, usage and unknown words;
#   apps:    help, list (plain `apps` too), keys, repos, selftest, search / info / install
#            without an index, update with no reachable repository, remove (a built-in app
#            refused, --unused), upgrade, try / run / keep / discard of nothing, relink,
#            an unknown subcommand (pkg-test.sh does the real installs);
#   search:  help, --wait, --wait-index, --status, words, --long, --limit, a table, nothing
#            found, --rebuild, a misspelled option;
#   and the old names (ifconfig netstat ping nc netctl fetch df snap dedupe milk spot) are
#   "command not found". Fresh /tmp directory; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d "${TMPDIR:-/tmp}/milk-nda.XXXXXX")"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/fs.img"; SPARE="$T/spare.img"; BLANK="$T/blank.img"
python3 scripts/mkmilkfs.py mkfs "$IMG" --size 64M --label Milk >/dev/null
python3 scripts/mkmilkfs.py mkfs "$SPARE" --size 24M --label Spare >/dev/null
head -c $((16 * 1024 * 1024)) /dev/zero > "$BLANK"
P='msh:[^\n]*\$ '
SOCK="$T/s.sock" LOG="$T/serial.log"

# Steps: "send:CMD" "wait:PROMPT" for each command.
S=()
s() { local c; for c in "$@"; do S+=("send:$c" "wait:$P"); done; }
s "sleep 1" \
  "ifconfig" "netstat" "ping" "nc" "netctl" "fetch" "df" "snap" "dedupe" "milk" "spot" \
  "network help" \
  "network" \
  "network status eth0" \
  "network status wlan0" \
  "network status --keys" \
  "MILK_TABLE=1 network status" \
  "network connections" \
  "network connections --tcp --listening" \
  "MILK_TABLE=1 network connections" \
  "network counters" \
  "MILK_TABLE=1 network counters | count" \
  "network ping --count 2 --interval 100 127.0.0.1" \
  "network ping -6 -c 1 ::1" \
  "network ping --count lots 127.0.0.1" \
  "network get http://127.0.0.1/" \
  "network get --headers http://127.0.0.1/" \
  "network get --save /tmp/page.html http://127.0.0.1/" \
  "network get --save /tmp/page.html http://127.0.0.1/" \
  "network get --save /tmp/page.html --replace http://127.0.0.1/" \
  "cat /tmp/page.html" \
  "network get https://127.0.0.1/" \
  "sleep 3 | network listen 7070 &" \
  "sleep 1" \
  "echo hello over loopback | network connect 127.0.0.1 7070" \
  "wait" \
  "network name testbox" \
  "network name" \
  "network name bad_name!" \
  "network name --local" \
  "network hide" \
  "network status" \
  "network show" \
  "network ping --fast 127.0.0.1" \
  "network teleport"
s "disks help" \
  "disks" \
  "disks list --human" \
  "disks list --all" \
  "MILK_TABLE=1 disks list" \
  "disks list /no/such/path" \
  "disks devices" \
  "MILK_TABLE=1 disks devices" \
  "disks info" \
  "echo one > /Users/milk/a.txt" \
  "disks snapshot s1" \
  "echo two > /Users/milk/a.txt" \
  "disks snapshot s2" \
  "disks snapshots" \
  "MILK_TABLE=1 disks snapshots" \
  "disks changes s1 s2" \
  "disks changes s1" \
  "disks clone s1 c1" \
  "disks clones" \
  "MILK_TABLE=1 disks clones" \
  "cat /Volumes/Milk-c1/Users/milk/a.txt" \
  "disks discard c1" \
  "disks clone s1 c2" \
  "disks keep c2" \
  "cat /Users/milk/a.txt" \
  "disks rollback before-c2" \
  "cat /Users/milk/a.txt" \
  "disks delete s2" \
  "disks snapshots" \
  "disks auto" \
  "disks auto interval=1h" \
  "disks check" \
  "disks verify" \
  "disks verify status" \
  "disks verify rate=64" \
  "files create /Users/milk/dd/" \
  "files copy /Commands/network /Users/milk/dd/n1" \
  "files copy /Commands/network /Users/milk/dd/n2" \
  "disks dedupe --dry-run /Users/milk/dd" \
  "disks dedupe /Users/milk/dd" \
  "disks dedupe /tmp" \
  "disks send s1 --output /tmp/s1.stream" \
  "disks send s1 > /tmp/s1b.stream" \
  "disks receive /tmp/s1.stream --to /Volumes/Spare" \
  "files list /Volumes/Spare/.snapshots" \
  "cat /tmp/s1b.stream | disks receive --to /Volumes/Spare" \
  "echo fake-spare-passphrase > /tmp/sp" \
  "disks encrypt vdb --passphrase-file /tmp/sp --no-recovery --wait" \
  "disks lock Spare" \
  "disks devices" \
  "echo fake-spare-passphrase | disks unlock vdb" \
  "cat /Volumes/Spare/.snapshots/s1/Users/milk/a.txt" \
  "disks decrypt vdb --passphrase-file /tmp/sp --wait" \
  "disks format vda Blank" \
  "disks format vdc" \
  "disks devices" \
  "echo old-fake > /tmp/pp" "echo new-fake-pass >> /tmp/pp" "echo new-fake-pass >> /tmp/pp" \
  "disks passphrase < /tmp/pp" \
  "disks snapshot" \
  "disks repair" \
  "disks list --fast"
s "apps help" \
  "apps" \
  "apps list" \
  "apps keys" \
  "apps repos" \
  "echo http://127.0.0.1:1/ > /System/Settings/milk/repos" \
  "apps repos" \
  "apps selftest" \
  "apps search" \
  "apps info fortune" \
  "apps install fortune" \
  "apps update" \
  "apps remove store" \
  "apps remove --unused" \
  "apps upgrade" \
  "apps try" \
  "apps run nothing" \
  "apps keep nothing" \
  "apps discard nothing" \
  "apps relink --quiet" \
  "apps frobnicate"
s "search help" \
  "search --wait" \
  "echo zebracorn lives here > /Users/milk/zebra.txt" \
  "sleep 1" \
  "search zebracorn" \
  "search --long zebracorn" \
  "search --limit 1 zebracorn lives" \
  "MILK_TABLE=1 search zebracorn" \
  "search nosuchwordanywhere" \
  "search --status" \
  "search --rebuild" \
  "search --wait-index" \
  "search --fuzzy x" \
  "echo now > /System/Kernel/fs/milkfs/sync"

qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
  -display none -monitor none -serial "unix:$SOCK,server,nowait" \
  -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
  -global virtio-mmio.force-legacy=false \
  -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 \
  -drive "file=$SPARE,if=none,format=raw,id=hd1" -device virtio-blk-device,drive=hd1 \
  -drive "file=$BLANK,if=none,format=raw,id=hd2" -device virtio-blk-device,drive=hd2 \
  -netdev user,id=n0,restrict=on -device virtio-net-device,netdev=n0 2>/dev/null &
QPID=$!
trap 'kill -9 "$QPID" 2>/dev/null || true' EXIT
python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait@240:$P" "${S[@]}" \
  "send:echo NDA""-DONE" "wait:NDA-DONE" "done:$T/done" &
DPID=$!
for _ in $(seq 1500); do [ -e "$T/done" ] && break; kill -0 "$DPID" 2>/dev/null || break; sleep 0.2; done
sleep 1
kill -9 "$QPID" 2>/dev/null || true; wait "$QPID" 2>/dev/null || true
kill "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true
[ -e "$T/done" ] || bad "the session did not finish (last lines: $(tail -3 "$LOG" | tr '\r\n' '  '))"
L="$(python3 -I scripts/console_clean.py "$LOG")"
echo "$L" > "$T/session.txt"

# The output of the first (or Nth) run of a command typed at the prompt.
out() { echo "$L" | awk -v c="$1" -v n="${2:-1}" '
  index($0, "$ " c) && substr($0, index($0, "$ " c) + length(c) + 2) == "" { k++; on = (k == n); next }
  /^msh:/ { on = 0 } on'; }
check() { # description command text [nth]
  if out "$2" "${4:-1}" | grep -Eq -- "$3"; then ok "$1"; else bad "$1 (wanted /$3/ after '$2', got: $(out "$2" "${4:-1}" | head -4 | tr '\n' '|'))"; fi
}
# (out stops at a line starting "msh:", so the old names look at the line after the command.)
gone() { echo "$L" | grep -A1 -F "\$ $1" | grep -Fq "msh: $1: command not found" && ok "$1 is gone" || bad "$1 still runs: $(echo "$L" | grep -A1 -F "\$ $1" | tail -1)"; }

echo "== the old names"
for c in ifconfig netstat ping nc netctl fetch df snap dedupe milk spot; do gone "$c"; done

echo "== network"
check "help lists every subcommand" "network help" "^  listen PORT +wait for one connection"
for v in status connections counters ping get connect listen name hide show help; do
  out "network help" | grep -Eq "^  $v( |$)" || bad "network help is missing $v"
done
check "network alone: the names and the links" "network" "^hostname +milk$"
check "  eth0 with its DHCP address and counters" "network" "^  ipv4 +10\\.0\\.2\\.15/24 +netmask 255\\.255\\.255\\.0"
check "  loopback IPv6 is host scope" "network" "^  ipv6 +::1/128 +host$"
check "  packet counters" "network" "^  received +[0-9]+ packets, [0-9]+ bytes"
check "status LINK: just that link" "network status eth0" "^eth0 +up "
out "network status eth0" | grep -q "^hostname" && bad "status eth0 printed the names too" || ok "  and not the names"
check "a missing link says which there are" "network status wlan0" "there's no network link called wlan0 \\(there are: lo, eth0"
check "status --keys: the panel's key=value lines" "network status --keys" "^iface\\.eth0\\.ipv4=10\\.0\\.2\\.15/24$"
check "status as a table" "MILK_TABLE=1 network status" "link:str	state:str	mac:str	mtu:int"
check "connections: httpd listens on [::]:80" "network connections" "^tcp6 .* \\[::\\]:80 .* LISTEN"
check "connections --tcp --listening" "network connections --tcp --listening" "LISTEN"
out "network connections --tcp --listening" | grep -q "^udp" && bad "--tcp showed udp" || ok "  and no udp"
check "connections as a table" "MILK_TABLE=1 network connections" "proto:str	local:str	remote:str	state:str"
check "counters" "network counters" "^Tcp:$"
check "counters as a table (count)" "MILK_TABLE=1 network counters | count" "[1-9]"
check "ping 127.0.0.1" "network ping --count 2 --interval 100 127.0.0.1" "2 packets transmitted, 2 received, 0% packet loss"
check "ping -6 ::1" "network ping -6 -c 1 ::1" "1 packets transmitted, 1 received"
check "a count that isn't a number" "network ping --count lots 127.0.0.1" "network ping: --count wants a whole number \\(got lots\\)"
check "get: the guest's own httpd" "network get http://127.0.0.1/" "network get: 200 OK, [0-9]+ bytes from 127\\.0\\.0\\.1:80"
check "get --headers" "network get --headers http://127.0.0.1/" "^HTTP/1\\.[01] 200"
check "get --save" "network get --save /tmp/page.html http://127.0.0.1/" "saved to /tmp/page\\.html"
check "get --save won't overwrite" "network get --save /tmp/page.html http://127.0.0.1/" "/tmp/page\\.html already exists \\(add --replace to overwrite it\\)" 2
check "get --save --replace does" "network get --save /tmp/page.html --replace http://127.0.0.1/" "saved to /tmp/page\\.html"
check "the saved page" "cat /tmp/page.html" "<"
check "https is refused in words" "network get https://127.0.0.1/" "only http:// addresses work"
echo "$L" | grep -q "network listen: listening on tcp \\[::\\]:7070 (IPv6 and IPv4)" && ok "listen" || bad "listen: no 'listening on' line"
echo "$L" | grep -q "network listen: connection from 127\\.0\\.0\\.1:" && ok "connect reached listen" || bad "no connection from 127.0.0.1"
[ "$(echo "$L" | grep -c "^hello over loopback")" -ge 1 ] && ok "listen printed what connect sent" || bad "listen didn't print the line"
check "name sets the hostname" "network name testbox" "network: the hostname is now testbox \\(was milk\\)"
check "name shows it" "network name" "^testbox$"
check "a bad hostname is refused" "network name bad_name!" "bad_name! can't be a hostname \\(1-63 letters, digits and '-'\\)"
check "name --local: the .local name, or why there is none" "network name --local" "\\.local$|no Milk Drop identity"
check "hide" "network hide" "network: Milk Drop is now hidden on the VM link"
check "status says hidden" "network status" "^milk drop +hidden"
check "show" "network show" "network: Milk Drop is now visible on the VM link"
check "a misspelled option" "network ping --fast 127.0.0.1" "network ping: there's no option --fast \\(try network help\\)"
check "an unknown subcommand" "network teleport" "network: there's no network teleport \\(try network help\\)"

echo "== disks"
for v in list devices info snapshots snapshot delete rollback changes auto clones clone keep discard check verify format encrypt decrypt lock unlock passphrase send receive dedupe help; do
  out "disks help" | grep -Eq "^  $v( |$)" || bad "disks help is missing $v"
done
ok "disks help lists every subcommand"
check "disks alone: free space" "disks" "^milkfs .* /Volumes/Milk$"
check "list --human" "disks list --human" "^milkfs .*M .* /Volumes/Milk$"
check "list --all shows proc" "disks list --all" "^proc +0 "
check "list as a table" "MILK_TABLE=1 disks list" "mount:str	type:str	size:size"
check "a missing path in plain words" "disks list /no/such/path" "disks list: /no/such/path doesn't exist"
check "devices" "disks devices" "^vdb [0-9]+ rw milkfs plain mounted=/Volumes/Spare Spare$"
check "devices as a table" "MILK_TABLE=1 disks devices" "device:str	size:size	ro:str	content:str"
check "info" "disks info" "^encryption: +off$"
check "snapshot" "disks snapshot s1" "disks: took snapshot s1"
check "snapshots" "disks snapshots" "^s2 gen="
check "snapshots as a table" "MILK_TABLE=1 disks snapshots" "name:str	generation:int	created:str	kind:str"
check "changes between two snapshots" "disks changes s1 s2" "a\\.txt"
check "changes since a snapshot" "disks changes s1" "a\\.txt"
check "clone" "disks clone s1 c1" "disks: made clone c1 of snapshot s1, at /Volumes/Milk-c1"
check "clones" "disks clones" "^c1 of=s1 at=/Volumes/Milk-c1"
check "clones as a table" "MILK_TABLE=1 disks clones" "name:str	snapshot:str	mount:str"
check "the clone has s1's file" "cat /Volumes/Milk-c1/Users/milk/a.txt" "^one$"
check "discard" "disks discard c1" "disks: discarded clone c1"
check "keep" "disks keep c2" "disks: clone c2 is now the volume \\(the old state is snapshot before-c2\\)"
check "after keep the volume is the clone" "cat /Users/milk/a.txt" "^one$"
check "rollback" "disks rollback before-c2" "disks: the volume is back to snapshot before-c2"
check "after rollback the file is back" "cat /Users/milk/a.txt" "^two$" 2
check "delete" "disks delete s2" "disks: deleted snapshot s2"
out "disks snapshots" 2 | grep -q "^s2 " && bad "s2 is still there" || ok "  and it's gone"
check "auto shows the settings" "disks auto" "interval"
check "auto interval=1h" "disks auto interval=1h" "1h"
check "check" "disks check" "checksummed: clean"
check "verify reads every block" "disks verify" "scrub: \\[run [0-9]+\\] done.* 0 error"
check "verify status" "disks verify status" "scrub: \\[run"
check "verify rate=64" "disks verify rate=64" "scrub:"
check "dedupe --dry-run" "disks dedupe --dry-run /Users/milk/dd" "1 could share"
check "dedupe" "disks dedupe /Users/milk/dd" "1 now share"
check "dedupe off milkfs says so" "disks dedupe /tmp" "disks dedupe: /tmp isn't on a milkfs volume"
check "send to a file" "disks send s1 --output /tmp/s1.stream" "disks: sent s1"
check "receive into Spare" "disks receive /tmp/s1.stream --to /Volumes/Spare" "disks: received"
check "  the snapshot is there" "files list /Volumes/Spare/.snapshots" "s1/"
check "receive from stdin (sent to stdout) refused: already there" "cat /tmp/s1b.stream | disks receive --to /Volumes/Spare" "disks receive: .*\\(EEXIST"
check "encrypt Spare in place" "disks encrypt vdb --passphrase-file /tmp/sp --no-recovery --wait" "encrypted in place, done"
check "lock" "disks lock Spare" "^disks: vdb Spare locked$"
check "  devices says locked" "disks devices" "^vdb [0-9]+ rw milkfs encrypted locked Spare$" 2
check "unlock (passphrase from a pipe)" "echo fake-spare-passphrase | disks unlock vdb" "disks: Spare unlocked"
check "  the received snapshot survived" "cat /Volumes/Spare/.snapshots/s1/Users/milk/a.txt" "^one$"
check "decrypt" "disks decrypt vdb --passphrase-file /tmp/sp --wait" "decrypted in place, done"
check "format the blank disk" "disks format vda Blank" "disks: vda formatted \\(milkfs, mounted at the next boot\\)"
check "format refuses the mounted volume" "disks format vdc" "disks format vdc: vdc is mounted.*\\(EBUSY"
check "  devices: vda is milkfs now" "disks devices" "^vda [0-9]+ rw milkfs plain unmounted Blank$" 3
check "passphrase on a plain volume is refused" "disks passphrase < /tmp/pp" "disks passphrase: "
check "usage for a missing name" "disks snapshot" "usage: disks snapshot NAME"
check "an unknown subcommand" "disks repair" "disks: there's no disks repair \\(try disks help\\)"
check "a misspelled option" "disks list --fast" "disks list: there's no option --fast \\(try disks help\\)"

echo "== apps"
for v in list search info install remove update upgrade try run keep discard relink repos keys selftest help; do
  out "apps help" | grep -Eq "^  $v( |$)" || bad "apps help is missing $v"
done
ok "apps help lists every subcommand"
check "apps alone lists the installed packages" "apps" "apps: no packages installed"
check "list" "apps list" "apps: no packages installed"
check "keys: the test key" "apps keys" "^milk-test +ed25519 [0-9a-f]{64}$"
check "repos" "apps repos" "^http://10\\.0\\.2\\.2:8765/$"
check "selftest" "apps selftest" "apps: selftest ok"
check "search with no index" "apps search" "no package index; run \`apps update\` first"
check "info with no index" "apps info fortune" "no package index; run \`apps update\` first"
check "install with no index" "apps install fortune" "no package index; run \`apps update\` first"
check "repos after a change" "apps repos" "^http://127\\.0\\.0\\.1:1/$" 2
check "update with nothing listening there" "apps update" "apps: http://127\\.0\\.0\\.1:1/: connect 127\\.0\\.0\\.1:1: "
check "a built-in app can't be removed" "apps remove store" "store is a built-in app"
check "remove --unused" "apps remove --unused" "apps: removed 0 unused package\\(s\\)"
check "upgrade: nothing to do" "apps upgrade" "apps: everything is up to date"
check "try needs a name" "apps try" "usage: apps try NAME"
check "run something not being tried" "apps run nothing" "not trying nothing: \`apps try nothing\` first"
check "keep something not being tried" "apps keep nothing" "not trying nothing"
check "discard something not being tried" "apps discard nothing" "not trying nothing"
out "apps relink --quiet" | grep -q . && bad "relink --quiet printed something" || ok "relink --quiet"
check "an unknown subcommand" "apps frobnicate" "apps: there's no apps frobnicate \\(try apps help\\)"

echo "== search"
check "help" "search help" "^  --limit N \\(-n\\) "
check "--wait" "search --wait" "search: the index is ready"
check "words" "search zebracorn" "^/Users/milk/zebra\\.txt$"
check "--long" "search --long zebracorn" "^txt .* /Users/milk/zebra\\.txt$"
[ "$(out "search --limit 1 zebracorn lives" | grep -c "^/")" = 1 ] && ok "--limit 1" || bad "--limit 1: $(out "search --limit 1 zebracorn lives")"
check "a table" "MILK_TABLE=1 search zebracorn" "path:str	name:str	kind:str	size:size"
check "nothing found" "search nosuchwordanywhere" "search: nothing found for \"nosuchwordanywhere\""
check "--status" "search --status" "state: +ready"
check "--rebuild" "search --rebuild" "search: rebuilding the index in the background"
check "--wait-index" "search --wait-index" "search: the index is (ready|loaded)"
check "a misspelled option" "search --fuzzy x" "search: there's no option --fuzzy \\(try search help\\)"

[ "$FAIL" = 0 ] && { echo "net-disks-apps-test: PASS"; rm -rf "$T"; } || { echo "net-disks-apps-test: FAIL (logs in $T)"; exit 1; }
