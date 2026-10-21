# Security

Two parts: a **capability sandbox** (what a process is allowed to do) and the **audit log**
(a tamper-evident record of what it did). The sandbox is below; the log follows.

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
filesystem it auto-grants read on the command's own binary (so it can still be exec'd).
Example:

```sh
sandbox --fs ~/Documents:rw --fs /bin:r --net none --no-audio weird-tool
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

**Packaged window apps run sandboxed** (desktop dock / Skim launches of `milk` packages
whose program links `user/gui`): `proc::spawn_with_caps` starts them with `fs` =
`/Applications/<name>:r` + `~/Library/<name>:rw` (created first), `net none`, no clipboard,
no spawning, windows and audio on — from the first instruction (no window in which they run
unrestricted). Audited as `sandbox.enter pid= exe= spec=`. Anything else they need comes
through the powerbox. Terminal programs from packages run in the Terminal as before.

**Demo**: `/bin/peek` sandboxes itself (`fs-none net=none spawn=0 clipboard=0`), shows that a
direct open of `~/Documents/todo.txt` fails (EACCES, `sandbox.deny`), then opens and saves
files only through the panel (wm demos `powerbox`, `powerboxpick`, `powerboxaudit`).

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
* **Rewinds**: a milkfs rollback or clone promote (e.g. `milk keep`) rewinds the file too.
  The persister detaches *before* the rewind (so no flush lands in the wrong tree), and on
  re-attach, if the rewound file is a prefix of the chain in RAM, it re-appends what the file
  lost, unchanged: the chain stays one chain (`audit verify` OK after `milk keep`). Only if
  the ring no longer reaches back that far do the lost records stay with the old snapshot.

## Writing records from user space: `/dev/audit`

Write-only. Each line written is `<event> [detail...]` (`event`: 1-32 of `[a-z0-9._-]`);
the kernel records it as `user.<event>` with the writer's pid, exe and the time. So a tool
can add records but can't pose as the kernel (`user.` prefix), can't choose its pid or exe,
and can't touch existing records. Today any process may write (everything is uid 0); the
exe column says who did. `milk` uses it for `user.pkg.*`.

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
