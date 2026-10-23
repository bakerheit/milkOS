# Security

Two parts: a **capability sandbox** (what a process is allowed to do) and the **audit log**
(a tamper-evident record of what it did). The sandbox is below; the log follows.

## Users and credentials

Two accounts by default (`rootfs/etc/passwd`, `group`, `shadow`):

| user | uid | primary group | groups | home |
|---|---|---|---|---|
| `root` | 0 | `wheel` (0) | wheel, admin | `/Users/root` |
| `milk` | 501 | `staff` (20) | staff, **admin** (80) | `/Users/milk` |

The default password of both is `milk`. It is a **well-known default for a hobby OS (and
the test suite's password), not a secret**: change it with `users password`.

* **Processes** carry real / effective / saved uid and gid plus supplementary groups
  (`proc/cred.rs`), Linux rules: setuid, setgid, setreuid, setregid, setresuid,
  setresgid, getresuid, getresgid, getgroups, setgroups (Linux numbers 143-150, 158,
  159; get*id 174-177). Root (euid 0) may set anything; others only shuffle between
  their real and saved ids. Ids are inherited across fork and kept across exec, except
  set-user-ID / set-group-ID programs (mode 4xxx / 2xxx) set the effective id to the
  file's owner. Set-ID bits are ignored under `no_new_privs` (any chrooted or sandboxed
  process) and on FAT and the 9p share. Every change is audited (`cred`, `setid.exec`).
  `/proc/<pid>/status` shows `Uid:` / `Gid:` / `Groups:`.
* **Who runs as whom.** The kernel and init are root. The desktop starts everything
  (Terminal, apps, the Store) as the *session user*, `milk` at boot. The **serial
  console** is the maintenance console: a root shell (HOME stays /Users/milk, so the test
  scripts that drive it keep working on host-built disk images, whose files belong to
  root). `users switch milk` gives a milk shell there; `utest perms` / `utest creds` switch to milk
  themselves.
* **Passwords** are never stored in plain text: `/System/Settings/shadow` (0600, root) holds
  `$5m$<rounds>$<salt hex>$<digest hex>`, h = SHA-256(salt ‖ password) then rounds-1 times
  h = SHA-256(h ‖ salt ‖ password), 5000 rounds, 16-byte random salt. Same code in the
  kernel (`proc/login.rs`) and user space (`rt::users`); `scripts/mkpasswd.py` makes one
  on the host. `users password` also writes `/Volumes/Milk/.milkos/shadow`, which the kernel
  restores into `/System/Settings/shadow` at boot, so changes survive a reboot.
* **Tools** (`user/systools`; they replaced whoami, id, su, passwd, sudo and auth):
  `users me` (name, id, groups), `users list`, `users switch [NAME] [--command CMD]` (asks
  the *target's* password; default root), `users password [NAME]` (your own after the
  current one; root sets anyone's), `admin [--user NAME] [--stdin] COMMAND...` (admins
  only, asks *your* password, three tries; `--stdin` says the password comes from stdin,
  for scripts; no credential caching), `admin ticket COMMAND...` (below) and `admin
  status`. `users` and `admin` are set-user-ID root (initramfs.list mode 4755): `users
  password`, `users switch` and `admin COMMAND` use it, and every other subcommand gives
  root up first, for good (`setresuid` to the real uid), so it runs with the caller's own
  rights (`users me` therefore reports the real uid). A wrong password costs a second and
  is written to the audit log as `user.password.fail via=users-switch|users-password|admin
  user=<whose password> uid=<caller>` (through `/dev/audit`, so the kernel adds the pid and
  program). The command `admin` runs as root gets a clean environment made from the target
  account (PATH=/Commands, HOME, USER, LOGNAME, the caller's TERM if it is a plain name)
  plus `ADMIN_USER=<you>`, not the caller's.

## Permissions

Classic Unix rwx bits are enforced for user processes (`kernel/src/fs/perm.rs`, called
from the VFS path walk and `fs/ops.rs`); kernel threads are root. The capability sandbox
stays on top: an operation needs **both** the permission bits and the sandbox.

* **Path walk:** search (x) on every directory passed through, including the last
  component's parent.
* **open:** r / w by the access mode (O_TRUNC needs w); creating needs w on the
  directory, and the creator may open what it just made whatever its mode.
  **exec** needs x (not r); root can only exec files with some x bit.
* **unlink / rmdir / rename:** w + x on the directory; in a **sticky** directory (`/tmp`
  is 1777) only the file's owner, the directory's owner or root. Moving a directory to
  another parent also needs w on it (its `..`). **mkdir / symlink / link** need w on the
  directory. **truncate** needs w.
* **chmod** (`files access`): owner or root (a non-root owner outside the file's group loses
  set-group-ID). **chown** (`files owner`): root; the owner may change the group to one of
  its own. A non-root chown
  clears set-ID bits. **utimes:** owner, root, or anyone who may write. Syscalls fchmodat,
  fchmod, fchownat, fchown; faccessat checks with the effective ids.
* **Ownership of new files:** the caller's euid and egid (or the directory's group if it
  is set-group-ID). A kernel thread (the desktop saving into ~) gives a new file the
  parent directory's owner, so the user's files stay the user's. On milkfs that is one
  extra small transaction per create.
* **Root (euid 0)** passes everything except the exec rule above.
* **File systems without owners:** everything on FAT and the 9p Mac share shows as owned
  by the session user (the "mounting user"), set-ID bits ignored. Terminals (`/dev/console`,
  `/dev/ttyN`) belong to the session user. `/proc` and `/System/Kernel` control files (owner
  writable) are also writable by group **admin**; what a control write triggers (mounting a
  clone, a snapshot) runs as the kernel (`perm::as_kernel`), the check is at open.
* `chroot` is allowed to anyone (it only narrows the view; `apps run` needs it) but
  sets `no_new_privs`, as does the capability sandbox: set-ID programs don't work in there.

**Other users' processes** (Linux's default, `hidepid=0`): everyone can see every
process in /proc and read its `status` (ids, threads, memory, and a `Caps:` line with its
capability set in sandbox syntax, `all` if unrestricted), `stat` and `cmdline`, and
`/proc/audit`, so `ps`, `top` and an Activity Monitor work for any user. Private to the
process's user and root: `/proc/<pid>/maps` (0400, owned by its euid) and
`/proc/<pid>/fd/` (0500). Signals follow Linux: root, or a sender whose real or effective
uid is the target's real or saved uid; otherwise `kill` is EPERM (signal 0 included).

**Admin group decision (apps install, the Store):** like macOS, the administrators group
`admin` (80) owns the shared, installable places: `/Apps` and `/Commands` are
`root:admin 0775`, `/System/Settings/milk/repos` (package sources) `root:admin 0664`. `milk` is in
`admin`, so `apps install`, the Store and `apps relink` work without `admin`; a non-admin user
would need `admin apps install`. The package signing keys (`/System/Settings/milk/keys`) and the
accounts (`/System/Settings/passwd`, `/System/Settings/group`, `/System/Settings/shadow`) stay root's.

**Boot layout** (`proc::login::init`): /Users/root 0700, /tmp 1777, the above, a milkfs volume
root `root:admin 0775` (if it was plain root), and a one-time migration giving the session
user's home to them when it still belongs to root (disks made before users existed).

### Login and encrypted disks (milkfs)

An encrypted milkfs volume (docs/filesystem.md) stays locked at boot. **At login** the
password is tried on it: `proc::login::login` calls `fs::milkfs::unlock(password, None)`
after a successful authentication, so a disk whose passphrase *is* the login password
opens as the user logs in (FileVault style), binds its `/Users/milk` and `/Apps`,
and gets the ownership/layout fix-up. ENODEV (nothing locked) is quiet; EACCES (a
different passphrase) is logged and the disk stays locked for `disks unlock`.
**On `users password`** (a user changing their own password, so the old one is known), the same
change is tried on the primary volume (`/System/Kernel/fs/milkfs/passwd`, `OLD\nNEW`): if the disk
was unlocked with the old login password, its passphrase follows the new one. It is skipped
when the new password is under 8 characters (milkfs's minimum), when root sets someone's
password, and quietly does nothing for an unencrypted or differently protected disk.

### Login API for the desktop (`kernel/src/proc/login.rs`)

```rust
proc::login::authenticate(name, password) -> Result<Account, LoginError>
proc::login::login(name, password) -> Result<Account, LoginError> // + becomes the session user
proc::login::session() -> Account          // who is logged in (boot: milk)
proc::login::set_session(account)          // no password check (e.g. after authenticate)
proc::login::lookup(name) / lookup_uid(uid) -> Option<Account>
Account { name, uid, gid, groups, gecos, home, shell } .cred() .env() .is_admin()
```

After `login`, everything the desktop starts (`proc::spawn`, `spawn_with_caps`,
`spawn_on`) runs as that account; processes already running keep their ids. A failed
try costs 500 ms and is audited (`login.fail`). The lock screen can become a login
screen by calling `authenticate` (unlock) or `login` (switch user).

## The capability sandbox

Files: `kernel/src/proc/caps.rs` (the model), enforcement in `proc/syscall.rs` (openat,
execve, fork), `proc/sock.rs` (socket/connect/sendto) and `proc/mod.rs` (device opens);
launcher `user/coreutils/src/bin/sandbox.rs`, wrapper `rt::sys::sandbox`.

Every process carries a capability set:

| cap | values | gates |
|---|---|---|
| `fs` | unrestricted (default), or a list of `path:r` / `path:rw` grants | openat, execve, and every *at path syscall through the same check |
| `net` | `all` (default) / `local` (loopback + link-local only) / `none` | socket, connect, sendto |
| `audio` | on (default) / off | open `/dev/audio` |
| `clipboard` | on / off | open `/dev/clipboard` (wm wires the device) |
| `windows` | on / off | open the wm device (`/dev/wm`/fb); the wm also checks it per request |
| `spawn` | on / off | fork / clone without `CLONE_THREAD` (threads are always allowed) |

**Inherited and shrink-only.** The set is shared by a thread group, copied into a child at
fork, kept across execve, and can only ever lose capabilities. `sandbox(spec)` (the
`MILK_SANDBOX` syscall) intersects the caller's set with the requested one: a tighter
sandbox nested inside another can never regain what the outer dropped, and an fs grant is
clamped to what the parent already allowed (and write only if the parent had write).

**Default is everything**, so a normal process is unaffected — nothing is enforced until a
process calls `sandbox`. Enforcement is also skipped entirely while `fs` is unrestricted,
so the common path adds no cost.

**Launcher**: `sandbox [--fs P:r|rw ...] [--net none|local|all] [--no-audio|--no-clipboard|
--no-windows|--no-spawn] [--no-fs] CMD...`. It builds the spec, and when it restricts the
filesystem it auto-grants read on the command's own program file (so it can still be exec'd).
Example:

```sh
sandbox --fs ~/Documents:rw --fs /Commands:r --net none --no-audio weird-tool
```

**Every denial is audited** as `sandbox.deny` (`op=`, the path/addr, what was needed);
entering a sandbox logs `sandbox.enter` with the spec. So a denied open, exec, socket or
fork leaves a chain-verified record of who tried what.

**Matching is string based** on the normalised absolute path a request resolves to. A
symlink that points out of a granted subtree is followed by the VFS and would escape the
grant — a known gap (an early-LSM-style limitation); grants should name real directories,
and a future version resolves to the inode. `..` is collapsed before matching, so it can't
walk out. chroot and the sandbox compose: a jailed, sandboxed process is bounded by both.

### Powerbox: granting one file by asking the user

A sandboxed process with no broad fs grant asks the window server to let the user pick a
file; on confirmation the kernel adds *just that path* to the process's grants.

**Syscall** `MILK_POWERBOX = 0x1001` (`proc/syscall.rs`, `sys_powerbox`), args
`(mode, name, name_len)`: mode 0 = open (read), 1 = save (read+write, created/truncated);
`name` is a suggested file name for save (`/` stripped, 255 bytes max). Userland:
`rt::sys::powerbox_open()`, `rt::sys::powerbox_save(name)`, both returning an open fd.
Needs the `windows` capability (else EACCES, audited). Errors: `ECANCELED` (125) when the
user cancels, `EINTR` when a signal arrives while the panel is up (the panel then closes).

**Kernel ↔ wm message** (`kernel/src/wm/powerbox.rs`): the syscall calls
`wm::powerbox::ask(pid, save, suggested)` on the requesting thread. That queues a request
(pid, the process name as the kernel knows it, open/save, the suggested name — nothing else
from the process), posts `Request::Powerbox` to the desktop and sleeps on a wait queue
(100 ms ticks, checking `signal::interrupted()`). The desktop shows one panel at a time
(`App::Powerbox`), which answers with an absolute path or `None`; closing it is a cancel.
Without a running desktop `ask` answers `None` at once.

**The panel** is the wm's own window: header "*app* (pid N) wants to open/save a file" and
"Only the file you choose is shared with it", places (Home, Documents, ...), the folder's
contents, a name field for save, Cancel / Open (Save). The app can't draw on it, pre-fill a
path, or send it input.

**The grant**: `caps.with_fs_grant(FsGrant { prefix: <exact path>, write: save })` on the
process (one file: grants match the path itself or below it, and a file has nothing below),
then the kernel opens it and returns the fd. Audited as `powerbox.grant pid= path= mode=r|rw`,
cancels as `powerbox.cancel pid= mode=`. The process never widens its own authority; the user
does, per file, through trusted UI.

**Drag and drop is a grant too.** Dropping a file from Files onto a userland window (or a
packaged window app's dock tile) is the user choosing that one file for that app, so it
goes through the same path: `proc::grant_file(pid, path, write=false, via="drop")` adds
`caps.with_fs_grant(path, read)`, opens the file into the process and audits
`powerbox.grant pid= path= mode=r via=drop`; the window gets a Drop event carrying the fd
(docs/processes.md). A dock tile drop on an app that isn't running starts it sandboxed with
the file in its grants and as its argument. Peek shows dropped files (wm demo `dragpeek`).

**Packaged window apps run sandboxed** (desktop dock / Skim launches of installed packages
whose program links `user/gui`): `proc::spawn_with_caps` starts them with `fs` =
`/Apps/<name>:r` + `~/Library/<name>:rw` (created first), `net none`, no clipboard,
no spawning, windows and audio on — from the first instruction (no window in which they run
unrestricted). Audited as `sandbox.enter pid= exe= spec=`. Anything else they need comes
through the powerbox. Terminal programs from packages run in the Terminal as before.

**Demo**: `/Apps/peek/peek` sandboxes itself (`fs-none net=none spawn=0 clipboard=0`), shows that a
direct open of `~/Documents/todo.txt` fails (EACCES, `sandbox.deny`), then opens and saves
files only through the panel (wm demos `powerbox`, `powerboxpick`, `powerboxaudit`).

## Authorisation tickets: admin rights for one process, for 5 minutes

Destructive storage operations (Disk Utility, `disks`, `apps keep`) should not be open to
every program a member of `admin` happens to run. Like macOS's authorisation panel, a
process *asks* to be authorised, the user proves they are an administrator, and the kernel
attaches a short-lived **admin ticket** to that one process. Files: `kernel/src/proc/auth.rs`
(tickets, rate limit, audit), `sys_auth` in `proc/syscall.rs`, `admin ticket`
(`user/systools/src/bin/admin.rs`), `rt::sys::authorize*`.

**The ticket.** Held by the kernel, never by the process: (pid, the process's start time,
the administrator's uid, expiry). Keying on the start time means a recycled pid gets
nothing. A ticket

* lasts 5 minutes (`auth::TICKET_NS`) from the grant; asking again renews it;
* belongs to that process only: **not inherited** by fork (the child has another pid);
  **kept across exec** (same process), which is how `admin ticket CMD` works;
* is dropped when the process exits, and on request (`rt::sys::auth_revoke`);
* gives nothing but the check below: the process's uid, groups and capability sandbox are
  unchanged (a sandboxed process with a ticket is still sandboxed).

`/proc/<pid>/status` shows `Auth:\tadmin ticket from uid N, S s left` while one is valid.

**What it unlocks.** `proc::auth::require(op)` passes for euid 0, the kernel itself, or a
valid ticket, and otherwise fails with EPERM and an `auth.required` audit record. The
filesystem decides in one place, `fs::milkfs::authorize(op, arg)`, run with the writer's
credentials before every control in `fs::milkfs::DESTRUCTIVE` (docs/filesystem.md):
`delete`, `rollback`, `restore`, `discard`, `promote`, `receive`, `mkfs`, `check` (repair
only), `auto` (its `keep=` prunes snapshots), `lock`, `passwd`, and `send`, `bench`,
`treetest`, `dedupe` (they write and delete files anywhere as the kernel). `proc::auth::init`
installs the ticket rule with `fs::milkfs::set_authorizer`; a refusal is EPERM, and
`set_reason` puts "needs an administrator's authorisation ..." in `/System/Kernel/fs/milkfs/error`,
which `disks` prints. Opening the control files still needs the usual admin-group write
access; the ticket is an extra requirement on top (a non-admin's process with a ticket
still can't open them). Harmless controls (`snapshot`, `clone`, `scrub`, `sync`, listings)
are unchanged. `users password`, `users switch` and `admin COMMAND` run set-user-ID
root, so they pass as euid 0 after their own password check. Kernel threads (the desktop, Time Machine) are the kernel and
pass.

**Until the panel exists (transitional).** The wm's trusted panel isn't built yet, and
without it a desktop app has no way to ask for a ticket. So while no prompt is
registered, apps the desktop started (`proc::spawn_with_caps`, e.g. Disk Utility) keep
the old rule, the open-time admin-group check, and each such write is audited as
`auth.legacy pid= op=milkfs.<control> (no trusted panel yet)`. Their children are not
covered. Everything run from a terminal (console, the Terminal window, ssh-style
sessions) needs a ticket already. The moment the wm calls `set_prompt` the allowance
ends, and Disk Utility's control writes (`user/diskutil/src/data.rs`, `ctl`) ask the
panel on EPERM and retry. disk-test then has to type the test password into the panel.

**Asking: the syscall.** `MILK_AUTH = 0x1003` (0x1002 is `MILK_OPENPTY`), `(op, a, b, c, d)`:

| op | args | does | returns |
|---|---|---|---|
| 0 | reason, reason_len | ask the desktop's trusted panel | ms on the ticket; `EOPNOTSUPP` (95) without a desktop, `ECANCELED` (125) on Cancel; needs the `windows` capability |
| 1 | password, len, user, user_len | check an administrator's password (the terminal path; empty user = the caller's own account) | ms on the ticket |
| 2 | | revoke the caller's ticket | 0 |
| 3 | | time left | ms (0 = none) |

Errors for 0 and 1: `EACCES` wrong password (or unknown user), `EPERM` the account is not an
administrator (root or group `admin`), `EAGAIN` rate-limited. The kernel copies the
password in, checks it with `login::authenticate` (the same code as login, 500 ms per
failure, reading `/System/Settings/shadow` as the kernel) and zeroes its copy. Userland:
`rt::sys::authorize(reason)`, `authorize_password(pw, user)`, `auth_revoke()`, `auth_left()`.

**Kernel ↔ wm request.** The window manager is a kernel thread, so the request stays in
the kernel, like the Powerbox: the wm registers its panel with
`proc::auth::set_prompt(fn(pid, reason, default_user) -> Option<(user, password)>)`, and op
0 calls it on the requesting thread, which blocks until the user answers. The panel is
drawn by the wm, never the app: header "*exe* (pid N) wants to make changes", the reason
string (256 bytes max, shown as text), user name (pre-filled with the session user),
password, Cancel / OK. The password goes from the panel to `auth::grant` and never reaches
the requesting process. With no prompt registered (no desktop) op 0 fails with
`EOPNOTSUPP` at once and the program should point at `admin ticket`. If the panel ever moves into a
user-space window server, the same hook becomes an AF_UNIX service under milkd
(`/System/Services/Sockets/authd.sock`, SO_PEERCRED-checked so only that service can answer); the syscall
side doesn't change.

**Terminal fallback: `admin ticket`.** `admin ticket [--user ADMIN] [--stdin] COMMAND
[ARGS...]` (it replaced `auth`) asks for an administrator's password (your own account by
default, `--user` for another; `--stdin` for scripts), gets a ticket for *itself*, then
execs COMMAND, which keeps it: `admin ticket disks rollback before-upgrade`, `admin ticket
apps keep moo2048`. `admin status` prints the ticket of the process it runs in, so `admin
ticket admin status` shows a fresh one; there is nothing to revoke afterwards (no `admin
lock`), because the ticket ends with COMMAND, and nothing to unlock for a whole session
(no `admin unlock`), because a ticket is never inherited. `admin` is set-user-ID root, but
`ticket` gives root up for good before it asks, so COMMAND runs exactly as you with only
the ticket added, unlike `admin COMMAND` (runs it as root). `ticket`, `status`, `run` and
`help` are `admin`'s own words: a command with one of those names runs with `admin run
NAME` (or its path). `disks` and `apps` print that hint on EPERM;
`apps` first asks the panel itself (op 0), so the Store and Disk Utility get the panel
without extra code.

**Rate limit.** Per requesting real uid: after 3 wrong passwords in a row, every further
request from that uid is refused with `EAGAIN` for 15 s, doubling with each further miss
up to 10 minutes; a correct password resets the count. Each wrong try also costs the
500 ms of `authenticate`. Lockouts are kept in RAM only, so a reboot clears them, which is
acceptable for a single-user machine with console access.

**Audit records** (`proc/auth.rs`): `auth.grant pid= exe= user= uid= via=panel|password
for=300 s reason=`, `auth.deny pid= exe= user= via= reason=BadPassword|NoSuchUser|
not-an-administrator|rate-limited (N s left)|cancelled`, `auth.revoke pid=`,
`auth.required op=milkfs.<control>` (a privileged write refused for want of a ticket),
`auth.legacy` (the transitional desktop-app allowance above).
`login.ok` / `login.fail` from `authenticate` appear alongside.

**Effect on existing flows.** Anything that wrote those controls as a non-root admin now
needs a ticket: `disks rollback/delete/send/...`, `disks dedupe`, and `apps try`'s keep/discard
(and its auto-discard after a failed try) when run as `milk` rather than root. Boot-time and kernel
callers (the fs self-test, `apps` under the root console in the tests) are unaffected.

**Tests**: `utest auth` (as milk): admin group alone is refused, a wrong password is
EACCES, the right one gives 5 minutes and the write gets past the check, `/proc/self/status`
shows the ticket, a forked child has none, revoke works, `admin ticket` keeps it across exec, the
rate limit kicks in after three misses, and the records are in `/proc/audit`. The test
password is the documented default `milk` (see "Users and credentials"), not a secret.

## Milk Drop: one machine identity, per-user clients

Milk Drop (docs/networking.md) authenticates *machines*: peers trust a machine's Ed25519
key and see its name. When the desktop started running as milk instead of root, the old
layout broke: `/System/Settings/milk/drop` was root's, so the desktop user's `drop name` and `drop
recv` failed writing the identity. Two models were possible:

* **a per-user identity** in `~/.config/milk/drop`: each account would show up as its own
  "machine" with its own key and name, two users on one Mac would look like two Macs, and
  root's existing identity (the one peers already trust) would be orphaned;
* **one machine identity used through a root service**: chosen. It matches what peers are
  told (a machine), keeps the key peers already trust, and keeps the secret key out of
  every user's reach.

**The identity** lives in `/System/Settings/milk/drop` (persistent, on the Milk volume):
`identity.secret` 0600 root (the seed); `identity.pub`, `name` and `rotation` 0644 in a
0755 directory (older machines made it 0700; the root side opens it up). Only root
processes read the secret: the `drop` service (milkd, `user = root`) and root's own
`drop`.

**The daemon's request socket** `/System/Services/Sockets/drop.sock` (AF_UNIX stream, 0666, one request line per
connection, a client gets 2 s), checked with SO_PEERCRED:

| request | who | does |
|---|---|---|
| `sign HEX` | root and people's accounts (uid 500..59999), not service accounts like nobody | signs a Milk Drop hello transcript (`milkdrop-hello-v2 ...`, which must contain this machine's public key) or an offer (`milkdrop-offer-v1 ...`, bound to the session's nonces); anything else is refused |
| `name NEW` | administrators (root or group admin) | renames the machine; `user.drop.rename old= new= by uid= user=` |
| `rotate` | administrators | `drop id --rotate`; `user.drop.rotate ... by uid= user=` |

Refusals of `name` / `rotate` are audited as `user.drop.refused uid= user= request=
reason=not-an-administrator`. A user who can sign hellos can act as this machine toward
peers, which is exactly what being allowed to use Milk Drop on it means; the oracle only
signs the two Milk Drop contexts, so the key can't be used to sign anything else, and it
never leaves the daemon.

**The client** (`drop send/recv/list/peers/forget`) runs as the user and does the network
and file work itself: it reads the file it sends with its own permissions (so the daemon
can't be used to read someone else's files), and writes what it receives into the user's
`~/Downloads` as that user. When root receives into a user's home (the console's HOME is
`/Users/milk`), the new folder and file are chowned to the home's owner. `/tmp/drop` (the
daemon's peer cache and the "receiving" marker) is 1777, like /tmp.

**Trust is per user.** Root's trusted peers are the machine's
(`/System/Settings/milk/drop/known_peers`); every other user has `~/.config/milk/drop/known_peers`,
which starts as a copy of the machine's list the first time they trust or forget someone.
So one user can't pin, or forget, a peer key for another. Without the daemon (eth1 down),
non-root `drop` says the service isn't running; root's still works directly.

**Tests**: `scripts/drop-test.sh` sends and receives as milk on both machines, checks
the ownership in `files list -l`, that milk can't read the secret, an admin rename through the
daemon and nobody's refusal.

# The audit log

Files: `kernel/src/audit/` (log, SHA-256, `/dev/audit`), hooks in `proc/mod.rs` (exec),
`fs/path.rs` (mount/unmount), `fs/milkfs/mod.rs` (snapshot, rollback, clones), `fs/ops.rs`
(the sealed log file), `proc/sock.rs` (network), `audio/mod.rs`; userland `user/audit`,
`user/rt/src/sha256.rs`; `scripts/tamper_audit.py`.

(The per-app firewall is parked; nothing here filters traffic.)

## Records

One line per record, space-separated, `exe` / `event` / `detail` %-escaped (no spaces or
newlines inside a field):

```text
<seq> <time_ns> <pid> <exe> <event> <detail> <prev_hash> <hash>
```

`time_ns` is wall-clock (RTC) nanoseconds, `pid 0` / `[kernel]` are kernel threads,
`hash = SHA-256(the line up to and including prev_hash)` in lowercase hex, and `prev_hash`
is the previous record's hash (64 zeros for the very first). Editing, inserting, deleting
or reordering a record breaks the chain at that point.

| event | from | detail |
|---|---|---|
| `audit.boot` | kernel, every boot | kernel version |
| `audit.tamper` | kernel, when the persisted chain doesn't verify at attach | first bad seq |
| `audit.reattach` | kernel, after a rollback or clone promote | |
| `exec` | proc::exec | ppid, argv |
| `fs.mount` / `fs.unmount` | fs::path | path, fs |
| `milkfs.snapshot` / `.rollback` / `.clone` / `.clone.discard` / `.clone.promote` | milkfs (user volume only) | names, result |
| `net.connect` / `net.listen` / `net.accept` / `net.sendto` | socket syscalls | proto, address, result (sendto: once per destination per socket) |
| `audio.open` | /dev/audio (first write of a stream) | |
| `user.*` | user tools through `/dev/audit` (e.g. `user.pkg.install`) | whatever the tool says |
| `clipboard.read` | for the wm side to add (`audit::record`) | |
| `auth.grant` / `.deny` / `.revoke` / `.required` | proc::auth (admin tickets) | pid, exe, user, via, reason / op |

## Where it lives

* **RAM**: the newest 2048 records, `/proc/audit`; `/proc/audit_status` has counts and
  the current head (seq + hash).
* **Disk**: `/Volumes/Milk/var/log/audit.log`, appended by the `audit` kernel thread
  (about once a second, then a milkfs sync) once the volume is mounted. At attach the file
  is verified; records made earlier in the boot are chained onto its end. A broken chain is
  logged as `audit.tamper` (and the new records are chained onto the last good one).
* **Sealed**: user processes get `EPERM` opening the file for writing, with `O_TRUNC`,
  truncating, renaming or replacing it, or unlinking it (`fs::ops` checks the inode). Only
  the kernel appends.
* **Rewinds**: a milkfs rollback or clone promote (e.g. `apps keep`) rewinds the file too.
  The persister detaches *before* the rewind (so no flush lands in the wrong tree), and on
  re-attach, if the rewound file is a prefix of the chain in RAM, it re-appends what the file
  lost, unchanged: the chain stays one chain (`audit verify` OK after `apps keep`). Only if
  the ring no longer reaches back that far do the lost records stay with the old snapshot.

## Writing records from user space: `/dev/audit`

Write-only. Each line written is `<event> [detail...]` (`event`: 1-32 of `[a-z0-9._-]`);
the kernel records it as `user.<event>` with the writer's pid, exe and the time. So a tool
can add records but can't pose as the kernel (`user.` prefix), can't choose its pid or exe,
and can't touch existing records. Today any process may write (everything is uid 0); the
exe column says who did. `apps` uses it for `user.pkg.*`.

## The `audit` tool

```text
audit [--file | FILE] [--app EXE] [--since 30s|10m|2h|1d] [--event PREFIX]
audit verify [--file | FILE]       # both /proc/audit and the file by default; exit 1 if broken
audit status                       # /proc/audit_status
audit selftest                     # userland SHA-256 against the FIPS 180-4 vectors
```

`audit verify` uses userland's own SHA-256 (`rt::sha256`, a copy of the kernel's), so it
doesn't trust kernel code to check kernel output. The kernel runs the same 5 FIPS 180-4
vectors at boot (`[audit] sha256 self-test: 5/5`).

## Tamper test

`scripts/tamper_audit.py IMAGE OLD NEW` plays a careful attacker on a private image: it
rewrites text inside the log (e.g. `dst=127.0.0.1:80` → `dst=6.6.6.6:8080`) and fixes every
milkfs checksum up to the superblock, so `mkmilkfs.py check` says the volume is clean. On
the next boot:

```text
[audit] attached: /Volumes/Milk/var/log/audit.log has 20 records (last seq 20), chain BROKEN at seq 21
audit verify: /Volumes/Milk/var/log/audit.log: BROKEN at seq 21: hash does not match the record
```

## Limits

* Cutting records off the **end** of the file leaves a valid (shorter) chain. Detecting that
  needs the head kept somewhere the attacker can't reach (another machine, a TPM); the head
  is in `/proc/audit_status` to copy out.
* Anyone with the disk image and the will to recompute every hash can rewrite the whole
  chain from the first change on; the chain proves *consistency*, not authorship (no key).
* The fs self-test's throwaway mounts show up as `fs.mount` records at every boot.
