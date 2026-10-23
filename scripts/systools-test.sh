#!/usr/bin/env bash
# `processes`, `services`, `system`, `users` and `admin` end to end (they replaced ps top kill,
# svc log, uname uptime free, whoami id passwd su, sudo auth), one boot on a private milkfs
# image, on the serial console (root) and as milk and nobody:
#   help for each (the files help format); processes list (text, a table, --kernel, filter),
#   watch --plain, stop (TERM; a name two processes have is refused; --now --all), signal
#   (by name, a bad signal), signals, info (by PID and name), --group, a PID that isn't
#   there, a process milk may not stop; services list / status (text, a table, one, a
#   missing one), restart, signal, reload, stop refused for nobody, logs (a marker, by
#   name, --last, --since, --level, --follow, refused for nobody, a bad --since), ten
#   restart + signal pairs back to back (milkd keeps answering); system
#   info / memory (Activity Monitor's groups, a table) / uptime, restart refused with the
#   hint; users me (root, milk), list (and a table), switch (to milk with and without a
#   password, a wrong password, a missing user), password (milk's own, a wrong current one,
#   the two not matching, someone else's refused, root sets one); admin COMMAND (as root;
#   as milk with the password: root, a clean environment with ADMIN_USER; three wrong
#   passwords; nobody refused), --user, run, ticket (a fresh ticket in admin status, a
#   wrong password), status; wrong passwords in the audit log (user.password.fail); the
#   set-ID modes; every old name is "command not found".
# Fresh /tmp directory; only the qemu started here is killed.
set -euo pipefail
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$(dirname "$0")/.."
T="$(mktemp -d "${TMPDIR:-/tmp}/milk-systools.XXXXXX")"
FAIL=0
ok() { echo "  ok: $*"; }
bad() { echo "  FAIL: $*"; FAIL=1; }
cargo build -q
ELF="target/aarch64-unknown-none-softfloat/debug/milkos-kernel"
"$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy)" -O binary "$ELF" "$ELF.bin"
IMG="$T/fs.img"
python3 scripts/mkmilkfs.py mkfs "$IMG" --size 64M --label Milk >/dev/null
P='msh:[^\n]*\$ '
MARK="systools-$$-$RANDOM"

boot() { # name steps...
  local name="$1"; shift
  local SOCK="$T/$name.sock" LOG="$T/$name.log"
  qemu-system-aarch64 -M virt -accel hvf -cpu host -smp "${MILK_SMP:-4}" -m 512M -kernel "$ELF.bin" \
    -display none -monitor none -serial "unix:$SOCK,server=on,wait=on" \
    -object rng-random,id=rng0,filename=/dev/urandom -device virtio-rng-device,rng=rng0 \
    -global virtio-mmio.force-legacy=false \
    -drive "file=$IMG,if=none,format=raw,id=hd0" -device virtio-blk-device,drive=hd0 2>/dev/null &
  QPID=$!
  python3 scripts/serial_drive.py "$SOCK" "$LOG" "wait:$P" "$@" "send:print SYSTOOLS""-DONE" "wait:SYSTOOLS-DONE" "done:$T/$name.done" &
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
# A command that asks for passwords: ask CMD (PROMPT ANSWER)...
ask() { local a="$1" c="$2"; shift 2; eval "$a+=(\"send:\$c\")"
  while [ $# -gt 0 ]; do eval "$a+=(\"wait:\$1\" \"send:\$2\")"; shift 2; done; eval "$a+=(\"wait:\$P\")"; }
AS_MILK="users switch milk --command"
# Ten restart + signal pairs on one line, back to back (msh has no loops).
PAIRS="services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null; services restart alarmd > /dev/null; services signal alarmd > /dev/null"
B=(); s B \
  "pause 1" \
  "processes help" "services help" "system help" "users help" "admin help" \
  "processes" \
  "MILK_TABLE=1 processes list" \
  "processes list --kernel" \
  "processes list | filter name == init | select pid" \
  "processes watch --plain --frames 2 --every 1" \
  "pause 300 &" \
  "processes stop pause" \
  "pause 1" \
  "processes list | text find --count ' pause'" \
  "pause 301 &" "pause 302 &" \
  "processes stop pause" \
  "processes stop --now --all pause" \
  "pause 303 &" \
  "processes signal pause usr1" \
  "processes signal pause NOPE" \
  "processes signals" \
  "processes info 1" \
  "processes info init" \
  "processes info nope" \
  "processes stop 99999" \
  "processes stop --group 99999" \
  "$AS_MILK 'processes stop 1'" \
  "processes kill 1" \
  "services" \
  "services status alarmd" \
  "MILK_TABLE=1 services list" \
  "services status nope" \
  "services restart alarmd" \
  "pause 1" \
  "services signal alarmd" \
  "pause 1" \
  "services reload" \
  "$PAIRS" \
  "pause 1" \
  "services status alarmd" \
  "services start alarmd" \
  "pause 1" \
  "services status alarmd" \
  "admin --user nobody services stop alarmd" \
  "print $MARK > /dev/journal" \
  "services logs | text find --count $MARK" \
  "services logs user/print --last 1" \
  "services logs --since 10m --level info user/print | text find --count $MARK" \
  "services logs --level err user/print | text find --count $MARK" \
  "services logs --since soon" \
  "admin --user nobody services logs" \
  "services logs --follow user/print > /tmp/follow.txt &" \
  "pause 1" \
  "print followed-$MARK > /dev/journal" \
  "pause 1" \
  "processes stop --all services" \
  "text show /tmp/follow.txt" \
  "system" \
  "system uptime" \
  "system memory" \
  "MILK_TABLE=1 system memory" \
  "system restart" \
  "users" \
  "$AS_MILK 'users me'" \
  "users list" \
  "MILK_TABLE=1 users list" \
  "users switch nope" \
  "files info /Commands/users /Commands/admin"
ask B "admin --user nobody users switch milk --command 'users me'" "Password for milk" "milk"
ask B "admin --user nobody users switch milk" "Password for milk" "wrong"
ask B "$AS_MILK 'users password'" "Current password for milk" "milk" "New password" "newpass123" "Type it again" "newpass123"
ask B "$AS_MILK 'users password'" "Current password for milk" "milk"
ask B "users password milk" "New password" "milk" "Type it again" "milk"
ask B "users password milk" "New password" "one1" "Type it again" "two2"
s B \
  "$AS_MILK 'users password root'" \
  "admin users me" \
  "$AS_MILK 'print milk | FOO=bar admin --stdin msh -c \"variables list\"'" \
  "$AS_MILK 'print --escapes \"no\\nnope\\nnah\" | admin --stdin users me'" \
  "admin --user nobody admin users me" \
  "admin run status" \
  "admin status" \
  "$AS_MILK 'print milk | admin ticket --stdin admin status'" \
  "$AS_MILK 'print wrong | admin ticket --stdin admin status'" \
  "admin" \
  "audit --event user.password.fail" \
  "ps" "top" "kill 1" "svc list" "log" "uname" "uptime" "free" "whoami" "id" "passwd" "su" "sudo id" "auth -s"
L1="$(boot b1 "${B[@]}")"
echo "$L1" > "$T/b1.txt"

# The output of the first (or Nth) run of a command typed at the prompt.
# (The command goes in through the environment: awk -v would turn its \n into a newline.)
out() { echo "$L1" | C="$1" awk -v n="${2:-1}" 'BEGIN { c = ENVIRON["C"] }
  index($0, "$ " c) && substr($0, index($0, "$ " c) + length(c) + 2) == "" { k++; on = (k == n); next }
  /^msh:/ { on = 0 } on'; }
has() { out "$1" "${3:-1}" | grep -Fq -- "$2"; }
hasre() { out "$1" "${3:-1}" | grep -Eq -- "$2"; }
check() { # description command text [nth]
  if has "$2" "$3" "${4:-1}"; then ok "$1"; else bad "$1 (wanted '$3' after '$2', got: $(out "$2" "${4:-1}" | head -3 | tr '\n' '|'))"; fi
}
checkre() { # description command regex [nth]
  if hasre "$2" "$3" "${4:-1}"; then ok "$1"; else bad "$1 (wanted /$3/ after '$2', got: $(out "$2" "${4:-1}" | head -3 | tr '\n' '|'))"; fi
}
count() { # description command number [nth]
  [ "$(out "$2" "${4:-1}" | tr -d ' \r' | head -1)" = "$3" ] && ok "$1" || bad "$1 (wanted $3 after '$2', got: $(out "$2" "${4:-1}" | head -2 | tr '\n' '|'))"; }

echo "== help"
for n in processes services system users admin; do
  check "$n help: the usage line" "$n help" "usage: $n COMMAND ...    "
  check "$n help: ends with help" "$n help" "  help                    this list"
done
check "processes help lists stop" "processes help" "  stop NAME|PID           ask a process to stop (TERM); --now stops it at once (KILL)"
check "admin help: ticket" "admin help" "  ticket COMMAND...       run it as you, with a 5-minute administrator's ticket"

echo "== processes"
check "processes with no verb lists" "processes" "  PID PARENT GROUP THREAD STATE    CORE   CPU TIME   MEMORY  NAME"
checkre "init is PID 1, parent 0" "processes" "^ +1 +0 +1 +[0-9]+ (sleeping|running) .* init$"
check "a table for msh" "MILK_TABLE=1 processes list" "pid:int	parent:int	group:int	thread:int	state:str	core:int	cpu_ms:int	memory:size	name:str"
checkre "--kernel: the kernel's threads, no PID" "processes list --kernel" "^    -      -     - +[0-9]+ .*\\[[a-z]"
[ "$(out "processes list | filter name == init | select pid" | tr -d ' \r' | grep -cx 1)" = 1 ] && ok "filter and select on the table" || bad "filter: $(out "processes list | filter name == init | select pid" | head -3 | tr '\n' '|')"
[ "$(out "processes watch --plain --frames 2 --every 1" | grep -c '^processes watch - .*load ')" = 2 ] && ok "watch --plain --frames 2: two frames" || bad "watch frames"
checkre "watch: CPU bars" "processes watch --plain --frames 2 --every 1" "^cpu0 +\\[[| ]+\\] +[0-9]+%"
checkre "stop asks politely" "processes stop pause" "^asked pause \\(PID [0-9]+\\) to stop$"
count "and it stopped" "processes list | text find --count ' pause'" 0
checkre "a name two processes have is refused, with their PIDs" "processes stop pause" "^processes stop: 2 processes are called pause \\(PIDs [0-9]+, [0-9]+\\): say which with processes stop PID, or add --all for every one$" 2
[ "$(out "processes stop --now --all pause" | grep -Ec '^stopped pause \(PID [0-9]+\)$')" = 2 ] && ok "--now --all stops both at once" || bad "--now --all: $(out "processes stop --now --all pause" | tr '\n' '|')"
checkre "signal by name" "processes signal pause usr1" "^sent USR1 to pause \\(PID [0-9]+\\)$"
check "a signal that doesn't exist" "processes signal pause NOPE" "processes signal: there's no signal NOPE (processes signals lists them)"
check "signals lists them" "processes signals" "15  TERM   please stop (the polite one)"
checkre "info by PID: name" "processes info 1" "^name +init$"
checkre "info: the user" "processes info 1" "^user +root \\(0\\)$"
checkre "info: the sandbox" "processes info 1" "^sandbox +none"
checkre "info by name" "processes info init" "^pid +1$"
check "info: a name nothing has" "processes info nope" "processes info: there's no process called nope (processes list shows them all)"
check "a PID that isn't there" "processes stop 99999" "processes stop: there's no process 99999"
check "a group that isn't there" "processes stop --group 99999" "processes stop: there's no process group 99999"
check "milk may not stop init" "$AS_MILK 'processes stop 1'" "processes stop: you don't have permission to stop init (PID 1)"
check "an unknown subcommand" "processes kill 1" "processes: there's no processes kill (try processes help)"

echo "== services"
check "services with no verb lists" "services" "NAME           STATE         PID RESTARTS  DESCRIPTION"
checkre "alarmd is running" "services" "^alarmd +running +[0-9]+ "
checkre "status NAME in detail" "services status alarmd" "^  state +running$"
checkre "status: the process" "services status alarmd" "^  process +PID [0-9]+, up "
check "a table for msh" "MILK_TABLE=1 services list" "name:str	state:str	pid:int	restarts:int"
check "a service that isn't there" "services status nope" "services status: there's no service called nope (services list shows them all)"
check "restart" "services restart alarmd" "restarted alarmd"
checkre "signal (KILL), the restart policy applies" "services signal alarmd" "^sent KILL to alarmd \\(PID [0-9]+\\); its restart policy applies$"
check "reload" "services reload" "read the service files again"
checkre "ten restarts and kills back to back: milkd still answers (and gave up on the quick failures)" "services status alarmd" "^  state +(running|restarting|failed)" 2
checkre "services start after that: running again" "services status alarmd" "^  state +running$" 3
check "stop is for administrators" "admin --user nobody services stop alarmd" "services stop: only an administrator can stop services (try admin services stop ...)"
count "logs: the marker is in the journal" "services logs | text find --count $MARK" 1
checkre "logs by name, --last 1" "services logs user/print --last 1" "^[A-Z][a-z]{2} +[0-9]+ [0-9:]{8} user/print\\[[0-9]+\\]: $MARK$"
count "--since 10m --level info" "services logs --since 10m --level info user/print | text find --count $MARK" 1
count "--level err leaves it out" "services logs --level err user/print | text find --count $MARK" 0
check "a bad --since" "services logs --since soon" "services logs: --since wants 30s, 10m, 2h, 1d or a Unix time"
check "the journal is for administrators" "admin --user nobody services logs" "services logs: only root and administrators can read the journal (try admin services logs)"
checkre "--follow saw a new entry" "text show /tmp/follow.txt" "user/print\\[[0-9]+\\]: followed-$MARK$"
checkre "the follow was stopped by name" "processes stop --all services" "^asked services \\(PID [0-9]+\\) to stop$"

echo "== system"
checkre "system with no verb: info, the name" "system" "^name +milk$"
checkre "info: the version" "system" "^system +milkOS [0-9]+\\.[0-9]+\\.[0-9]+"
checkre "info: CPUs" "system" "^CPUs +[0-9]+$"
checkre "info: memory" "system" "^memory +[0-9]+ MB, [0-9.]+ MB free$"
checkre "uptime" "system uptime" "^up .* \\(since [A-Z][a-z]{2} +[0-9]+ [0-9]{2}:[0-9]{2}\\), [0-9]+ processes, load [0-9.]+ [0-9.]+ [0-9.]+ \\(1, 5, 15 minutes\\)$"
checkre "memory: the headline" "system memory" "^[0-9]+ MB of memory: [0-9.]+ [KM]B in use, [0-9.]+ [KM]B free \\(pressure (low|medium|high)\\)$"
for g in "Apps" "Kernel heap" "Kernel image" "Files in RAM" "Screen" "Windows" "Other kernel" "Page cache" "Free"; do
  checkre "memory: $g" "system memory" "^  $g +[0-9.]+ [KMG]?B +[0-9]+\\.[0-9]%$"
done
check "memory: what Other kernel is" "system memory" "Other kernel is "
check "memory as a table" "MILK_TABLE=1 system memory" "group:str	size:size	percent:str"
check "restart: not from the shell yet" "system restart" "system: restart isn't something the shell can do yet: use the milk menu on the desktop"

echo "== users"
checkre "users with no verb: me (root)" "users" "^name +root$"
checkre "me: administrator" "users" "^administrator +yes$"
checkre "me as milk" "$AS_MILK 'users me'" "^name +milk$"
checkre "me as milk: id" "$AS_MILK 'users me'" "^id +501$"
checkre "me as milk: groups" "$AS_MILK 'users me'" "^groups +staff \\(20\\), admin \\(80\\)$"
checkre "list: milk" "users list" "^milk +501  yes +/Users/milk +staff, admin$"
checkre "list: nobody" "users list" "^nobody +65534  no "
check "list as a table" "MILK_TABLE=1 users list" "name:str	id:int	group:str	groups:str	admin:str	home:str	shell:str	full_name:str"
check "switch to someone who isn't there" "users switch nope" "users switch: there's no user called nope"
checkre "users is set-user-ID root" "files info /Commands/users /Commands/admin" "rwsr-xr-x"
[ "$(out "files info /Commands/users /Commands/admin" | grep -c 'rwsr-xr-x')" = 2 ] && ok "both users and admin are 4755" || bad "set-ID modes: $(out "files info /Commands/users /Commands/admin" | grep -i perm | tr '\n' '|')"
checkre "switch with milk's password (from nobody)" "admin --user nobody users switch milk --command 'users me'" "^name +milk$"
check "switch: a wrong password" "admin --user nobody users switch milk" "users switch: that's not the password for milk"
check "password: milk changes her own" "$AS_MILK 'users password'" "users password: the password for milk is changed"
check "password: a wrong current one" "$AS_MILK 'users password'" "users password: that's not the password for milk" 2
check "password: root sets one" "users password milk" "users password: the password for milk is changed"
check "password: the two don't match" "users password milk" "users password: the two passwords don't match; nothing changed" 2
check "password: only your own" "$AS_MILK 'users password root'" "users password: you can only change your own password"

echo "== admin"
checkre "admin COMMAND as root" "admin users me" "^id +0$"
E="$AS_MILK 'print milk | FOO=bar admin --stdin msh -c \"variables list\"'"
check "as milk with the password: root's home" "$E" "HOME=/Users/root"
check "a clean PATH" "$E" "PATH=/Commands"
check "ADMIN_USER says who" "$E" "ADMIN_USER=milk"
out "$E" | grep -q "FOO=bar" && bad "the caller's environment reached the root command" || ok "the caller's environment stays behind"
W="$AS_MILK 'print --escapes \"no\\nnope\\nnah\" | admin --stdin users me'"
[ "$(out "$W" | grep -c "That's not the password. Try again.")" = 3 ] && ok "three tries" || bad "three tries: $(out "$W" | tr '\n' '|')"
check "then nothing runs" "$W" "admin: that's not the password for milk; nothing was run"
check "nobody isn't an administrator" "admin --user nobody admin users me" "admin: nobody isn't an administrator (only group admin can use admin)"
check "run: a command with one of admin's own names" "admin run status" "admin: there's no command status"
checkre "status as root" "admin status" "^administrator +yes \\(root\\)$"
checkre "status: no ticket" "admin status" "^ticket +none"
checkre "ticket: a fresh ticket for the command" "$AS_MILK 'print milk | admin ticket --stdin admin status'" "^ticket +[0-9]+ s left \\(this process only\\)$"
checkre "ticket: the command runs as you" "$AS_MILK 'print milk | admin ticket --stdin admin status'" "you +milk \\(501\\)$"
check "ticket: a wrong password" "$AS_MILK 'print wrong | admin ticket --stdin admin status'" "admin ticket: that's not the password for milk"
check "admin with nothing: the help" "admin" "usage: admin COMMAND ...    run a command as an administrator"
for via in users-switch users-password admin; do
  check "the audit log has the wrong password ($via)" "audit --event user.password.fail" "via=$via"
done

echo "== the old names are gone"
gone() { echo "$L1" | grep -A1 -F "\$ $1" | grep -Fq "msh: ${1%% *}: command not found" && ok "${1%% *} is gone" || bad "${1%% *} still runs: $(echo "$L1" | grep -A1 -F "\$ $1" | tail -1)"; }
for c in ps top "kill 1" "svc list" log uname uptime free whoami id passwd su "sudo id" "auth -s"; do gone "$c"; done

[ "$FAIL" = 0 ] && { echo "systools-test: PASS"; [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T" || true; } || { echo "systools-test: FAIL (logs in $T)"; exit 1; }
