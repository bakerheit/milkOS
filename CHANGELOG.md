# Changelog

## Unreleased

**Network**
- https:// pages: a TLS 1.3 client written from scratch (`tls/`, shared by the kernel and
  user space; docs/tls.md), so the Browser and `network get` open secure sites. ChaCha20-
  Poly1305 and AES-128-GCM; X25519, P-256 and P-384; certificates always checked against the
  compiled-in Mozilla root store (121 roots, 126 KiB), with errors in plain words ("the
  certificate is for another site", "the certificate has expired", "the site doesn't speak
  TLS 1.3"). New in milkcrypto: AES-128 (bitsliced), GHASH/GCM, SHA-384, HKDF, RSA, P-256 and
  P-384. Tested against RFC 8448's trace and OpenSSL 3 (`scripts/tls-interop.sh`).

**Desktop**
- Real system fonts: UI text is anti-aliased Figtree (default), IBM Plex Sans, Fira Sans,
  Atkinson Hyperlegible Next or Ubuntu Sans, picked in Settings > Appearance > Font and
  switched live (saved as `font = <key>`). Baked at build time by `scripts/gen_sysfont.py`
  at 13, 26, 52 and 104 px (Regular, Medium, Bold): 451 KiB of glyphs, the kernel image
  grew 492 KiB. Terminal, Notes and the console stay on Unifont. Credits in Settings >
  About, licences in `/System/Fonts`.

**Files**
- `files`, one command for files and folders: `create` (a trailing slash makes a folder),
  `copy`, `move` (across disks too), `rename`, `delete`, `info`, `list`, `link`, `help`, with
  errors in plain English. It replaces `ls`, `cp`, `mv`, `rm`, `mkdir`, `touch` and `ln`,
  which are gone. A slash at the end of a path means a folder: on a file it is an error.
- `files owner` and `files access` replace `chown` and `chmod`, which are gone.
  `files owner notes.txt` says "milk (group: staff)"; `files owner PATH USER`, `USER:GROUP` or
  `:GROUP` changes it. `files access notes.txt` says who may use it in one sentence ("milk can
  read and change it, staff can read it, everyone else can't", with set-ID and sticky when
  they're on), and `files access PATH private | shared | group | read-only | open` sets it
  (600/700, 644/755, 660/770, 444/555, 666/777 for files/folders; a runnable file stays
  runnable), `runnable` / `not-runnable` turn running on and off, `--mode NNNN` sets an exact
  octal mode. `--all` on a folder changes everything in it without following links. The
  kernel's rules are the same: only an admin gives things away, an owner can change the group
  to one they're in. `files info` shows the sentence beside the mode; the `files list` table's
  `mode` column is now `access`.
- `go` and `where` instead of `cd` and `pwd`, which are gone: `go Documents`, `go ..`,
  `go /Apps`, `go` (home), `go back` (the folder before; a folder called back is
  `go ./back`); `where` prints the folder you're in, in pipes too, and takes no
  arguments. Errors in plain English ("Documents doesn't exist",
  "notes.txt is a file, not a folder", "you can't open ..."). In msh and the Terminal's
  built-in shell.
- `text`, one command for lines of text: `show`, `first [N]`, `last [N]`, `find`, `sort`,
  `unique`, `count`, `help`, reading what's piped in when there's no file. It replaces `cat`,
  `head`, `tail`, `grep`, `sort`, `uniq` and `wc`, which are gone; their flags are spelled out
  (`text find --ignore-case --invert --numbers --count --names`, `text sort --reverse
  --numbers --unique --field N`, `text unique --count --repeated`, `text count --lines
  --words --bytes`). `text count` and `text unique --count` give msh a table. `print`
  replaces `echo` (`--no-newline`, `--escapes`, and -n / -e still work), `pause` replaces
  `sleep` (`pause 5`, `5s`, `2m`, `500ms`). Errors in plain English.
- msh's `variables` built-in replaces `env` and the `export`, `set` and `unset` built-ins:
  `variables list` (a table too), `get NAME`, `set NAME=value` (exported), `remove NAME`,
  `run NAME=value COMMAND`. `NAME=value` and `NAME=value COMMAND` work as before.
- msh's table filter is now `filter` instead of `where`, same syntax and operators:
  `ps | filter rss > 100`, `files list | filter size > 1M`. `where` with arguments says
  "where takes no arguments; to filter a table use filter".
- A real Trash: `files delete` moves things to `~/.Trash` (or a disk's own `.Trash`),
  `files delete --now` skips it, `trash list`, `trash restore`, `trash empty`. The `trashd`
  service deletes things for good after 30 days, or what Settings > Trash says (or never).
  Files deletes to it too and shows it in its sidebar (Restore, Empty Trash); the dock has a
  Trash at its right end, empty or full.
- `renameat2` takes `RENAME_NOREPLACE`.
- Plain-English folders instead of `/bin`, `/etc`, `/home` and `/srv`: `/Apps` (apps you
  open, built-in and installed, one folder each), `/Commands` (what you type in Terminal;
  `PATH=/Commands`), `/Users/<name>` (homes, root's too), `~/Sites` (the web server's pages),
  and `/System` with `Services`, `Startup`, `Settings`, `Fonts`, `Sounds` and `Music`.
  A clean break: no links with the old names. `open <app>` finds apps by name; Files has
  Apps in its sidebar.
- The last Unix folders that only milkOS itself used moved under `/System`: the service
  sockets (`/run`) are in `/System/Services/Sockets`, the kernel controls (`/sys`) in
  `/System/Kernel` (`/System/Kernel/fs/milkfs/...`). `/dev`, `/proc` and `/tmp` stay for
  Linux and POSIX programs, hidden in Files and in `files list /` (`--all` shows them).
- Test and developer tools (`utest`, `fault`, `hello`, `fsstress`, `tcpbench`, `linux-hello`,
  `chroot`, `sandbox`) moved out of `/Commands` into `/System/Developer`, which is not on
  `PATH`.
- Existing disks move over once at boot (or when a login unlocks them): `home/<user>`,
  `Applications` and `etc/milk/{drop,net}` are renamed inside the volume, the home gets
  `~/Sites`, and old snapshots still open in Files and Versions.
- The toy packages put their program at the top of their folder (`/Apps/fortune/fortune`).

**Processes, services and accounts**
- Five commands with plain verbs replace fourteen, which are gone (no aliases):
  `processes` (`list`, `watch`, `stop [--now]`, `signal`, `signals`, `info`) replaces `ps`,
  `top` and `kill`; `services` (`list`, `status`, `start`, `stop`, `restart`, `signal`,
  `reload`, `logs [--follow] [--since 10m] [--last N] [--level L] [--this-boot]`) replaces
  `svc` and `log`; `system` (`info`, `memory`, `uptime`) replaces `uname`, `uptime` and
  `free`; `users` (`me`, `list`, `password`, `switch`) replaces `whoami`, `id`, `passwd` and
  `su`; `admin COMMAND` replaces `sudo`, and `admin ticket COMMAND` / `admin status`
  replace `auth`. Each has `help` in the format `files help` uses, errors in plain English
  ("there's no process called x", "only an administrator can stop services"), and a table
  for msh where it prints rows (`processes list | filter memory > 1M`, `users list`,
  `services list`, `system memory`). `processes stop NAME` refuses a name two processes
  share and lists their PIDs (`--all` for every one). `system memory` shows the new
  `/proc/meminfo` breakdown in Activity Monitor's groups. There is no `system restart` or
  `shutdown` yet: only the desktop's menu can.
- `users` and `admin` are set-user-ID root like `su`, `sudo` and `passwd` were, with the same
  checks; their other subcommands give root up first. `admin` now gives the root command a
  clean environment from the target account (PATH, HOME, USER, LOGNAME, TERM, plus
  `ADMIN_USER`) instead of the caller's, and wrong passwords for `users switch`,
  `users password` and `admin` are in the audit log (`user.password.fail`).
- Fixed: asking for a password on a terminal (`rt::users::read_password`, used by all of
  them) wrote 24 bytes past its termios buffer on the stack; the kernel's termios is 60
  bytes, the buffer was 36.
- The rules are in `user/syskit` (host tests: `scripts/syskit-test.sh`, smoke set), the
  commands in `user/systools`; `scripts/systools-test.sh` (default set) runs every
  subcommand in a VM and checks the old names are gone.

**File system**
- milkfs is its own crate (`<repo>/milkfs`, `no_std`, shared with the kernel like the crypto
  crate), with the kernel's VFS glue in `kernel/src/fs/milkfs`. Nothing changes on disk or in
  behaviour. It builds for the Mac too: `scripts/milkfs-test.sh` runs the file system there in
  a few seconds (memory disks, a fake clock, a seeded RNG, power cuts at many points, a drive
  that reorders unflushed writes), covering what the qemu milkfs tests check below the VFS;
  check-all runs it in the smoke set.

**Network, disks, apps and search**
- `network` replaces `ifconfig`, `netstat`, `ping`, `nc`, `netctl` and `fetch`, which are gone:
  `network status` (links, addresses, counters, the hostname and Milk Drop; `--keys` for
  the Settings > Network panel), `connections`, `counters`, `ping HOST`, `get URL` (`--save
  FILE`, never overwriting without `--replace`), `connect HOST PORT`, `listen PORT`,
  `name [NAME]` (the hostname; `--local` the .local name), `hide` and `show` (Milk Drop
  visibility), `help`. Options are spelled out (`--count 3`, `--ipv6`), short forms kept.
- `disks` replaces `df`, `snap` and `dedupe`, which are gone: `list` (free space), `devices`,
  `info`, `snapshots`, `snapshot NAME`, `delete`, `rollback`, `changes A [B]` (was diff),
  `auto`, `clones`, `clone`, `keep` (was promote), `discard`, `check`, `verify` (was scrub),
  `format` (was mkfs), `encrypt`, `decrypt`, `lock`, `unlock`, `passphrase` (was passwd),
  `send`, `receive`, `dedupe PATH`, `help`. Same checks, prompts and secrets handling; the
  stream temp files for `send` to stdout and `receive` from stdin are now made O_EXCL,
  mode 0600 (security review SR-25, user-space half).
- `apps` replaces the `milk` package manager, which is gone (the user, the folders and the
  package format keep the milk name): `list`, `search`, `info`, `install`, `remove`
  (`--unused` was autoremove), `update`, `upgrade` (`--yes`), `try`, `run` (was run-try),
  `keep`, `discard`, `relink`, `repos` (new), `keys`, `selftest`, `help`. Signatures,
  checksums and audit records are unchanged. The Store and init use it.
- `search` replaces `spot`, which is gone, with the same options spelled out: `--long`,
  `--limit N`, `--status`, `--wait`, `--wait-index`, `--rebuild`.
- `network status`, `connections`, `counters`, `disks list`, `devices`, `snapshots`,
  `clones`, `apps list`, `apps search` and `search` give msh tables. The parsing and wording
  are in `user/cmdkit` (host tests, smoke set); `scripts/net-disks-apps-test.sh` tests every
  subcommand in a VM.

**Internet**
- The internet is on for `scripts/run.sh` and the release `milkOS.command` (slirp without
  `restrict=on`; `MILK_OFFLINE=1` or `--offline` for the old restricted network), and every
  test stays offline: check-all puts a qemu wrapper first in each test's `PATH` that refuses
  an `internet=on` boot, and fails the run if one is tried. The kernel is told in fw_cfg
  `opt/milkos/net`; Settings > Network, `network status` and `/proc/net/internet` say
  "Internet: on" or "off (offline mode)" (docs/networking.md, "Internet").
- Real DNS when online: the DHCP server (slirp's 10.0.2.3), A and AAAA, CNAMEs, TCP for
  truncated answers, a TTL cache. Offline keeps the `.test`-only policy. User space asks the
  kernel (`MILK_RESOLVE`); `network lookup NAME` is new.
- `milkweb`, a shared no_std crate (kernel and user space, host-tested): URLs, an HTTP/1.1
  client over a small Transport trait (chunked, Content-Length, redirects with a loop guard
  and a limit of 10, gzip/deflate with imgdec's inflate, `User-Agent: milkOS/0.1`, time and
  size limits, an `https://` hook for TLS: until then "secure pages aren't supported yet"),
  DNS messages and the resolver's logic, the Browser's HTML subset.
- The Browser visits host names (offline: IP literals and localhost, as before), shows
  where redirects ended, says what went wrong (no internet, not found, can't connect, secure
  pages), searches DuckDuckGo's lite page for a bare word, and reads more of the web:
  entities, script/style dropped, div/span/table/blockquote/hr, `<img alt>`, UTF-8 and
  Latin-1. `network get` uses the same client (host names, redirects, gzip).
- Tests: `scripts/milkweb-test.sh` (host) and `scripts/web-test.sh` (a VM against a DNS and
  HTTP server on the Mac's 127.0.0.1; lookups, a redirect chain, a chunked gzip page, the
  Browser by host name).

**Memory**
- A real heap for every program (`user/heapkit`, the global allocator in `rt::heap`). The old
  one was a bump allocator that only took back the most recent block, so long-running
  processes grew without end (Activity Monitor, top and milkd most of all). Now: size
  classes for small blocks, coalescing free lists for the rest, realloc in place, and free
  memory goes back to the kernel (the break shrinks; free pages inside the heap are
  munmapped and taken back with `mmap(MAP_FIXED)` when reused). A lock makes it safe with
  `rt::thread`. In 3 minutes of a busy desktop Activity Monitor went from 2.6 to 18.5 MB
  before and stays at about 2.3 MB now; top went from 0.5 to 5.3 MB and stays at 0.3 MB.
  `scripts/heapkit-test.sh` (host, smoke set) and `scripts/leak-test.sh` (a desktop for a
  minute, default set) check it.

## 0.1.0 (2026-10-21)

The first release: everything below exists and is tested, on qemu `virt` (aarch64) with
HVF on an Apple Silicon Mac. Details are in `docs/` (one file per area).

**Release**
- `scripts/release.sh` builds `dist/milkOS-0.1.0/` and a zip: a release kernel, a ready
  milkfs disk, and `milkOS.command`, which runs on double-click (needs only QEMU).
  `scripts/releasetest.sh` runs the unzipped bundle from outside the repo.
- One version number, in `VERSION` (uname, `/proc/version`, motd, About, Settings).

**Kernel** (`docs/memory.md`, `docs/processes.md`, `docs/smp.md`)
- Boot, MMU with 4 KiB pages, frame allocator and heap, GICv2, generic timer, UART.
- SMP on 4 CPUs: PSCI bring-up, real spinlocks, IPIs, TLB shootdown, per-CPU stats.
- Preemptive scheduler with priorities, wait queues, sleeping mutexes.
- EL0 processes with their own address spaces (ASIDs), ELF loader, copy-on-write `fork`,
  threads and futexes, signals, job control, pipes, `chroot`; over 80 Linux-numbered
  syscalls.

**Security** (`docs/security.md`)
- Per-process capability sandbox (files, network, devices), enforced in the syscall layer;
  packaged apps run sandboxed and get files only through the Powerbox Open/Save panel.
- Tamper-evident audit log (SHA-256 hash chain) of execs, connections, mounts,
  snapshots, sandbox denials, scrub errors; `audit`, `audit verify`.

**Storage** (`docs/filesystem.md`)
- VFS with tmpfs root, initramfs, devfs, procfs, sysfs; virtio-blk; block cache; a page
  cache with readahead.
- milkfs, a copy-on-write filesystem with checksums on everything: snapshots, rollback,
  writable clones (try-before-install), Time Machine (automatic snapshots), `snap diff`,
  per-file version history, reflink copies (`cp --reflink`) and `dedupe`, `check
  --repair`, background scrub, `snap send` / `snap receive` (full and incremental,
  atomic) with Mac-side tools (`mkmilkfs.py`). Persistent space map (mount in under
  1 ms), multi-block extents and inline small files. Crash-tested: SIGKILL, power cuts,
  torn superblock writes.
- The home folder lives on milkfs; FAT32 with long file names; the Mac's `share/` folder
  as `/Volumes/Mac` (virtio-9p, read-only by default).

**Network and sound** (`docs/networking.md`, `docs/audio.md`)
- virtio-net and our own stack: Ethernet, ARP, IPv4, ICMP, UDP, DHCP, DNS, TCP; sockets;
  `ifconfig`, `ping`, `netstat`, `nc`, `fetch`, `httpd`. qemu's network is restricted.
- virtio-sound with a mixer, `/dev/audio`, `play`, a boot chime and system sounds.

**Desktop** (`docs/ui-concepts/`)
- Compositor with per-window buffers, glass, shadows, animations; menu bar, dock, Skim
  (launcher and command bar), Today, Spaces, notifications, control centre, light and dark
  themes, window snapping, lock screen, screenshots, live resolution switching.
- Apps: Files (with Time Machine and Browse All Versions), Terminal (tabs, colours),
  Notes, Browser, Paint, Music, Settings, About; user programs can open windows.

**Userland** (`docs/shell.md`, `docs/packages.md`)
- `msh`, a shell with pipelines, redirection, job control and structured pipes
  (`ps | where rss > 100 | sort-by name`); coreutils; `top`; `snap`, `versions`,
  `dedupe`.
- `milk`, a package manager with Ed25519-signed packages and dependencies; `milk try`
  installs into a throwaway clone, kept or discarded.

**Fixed before release**
- Three intermittent hangs/crashes, each root-caused and covered by `scripts/serial-soak.sh`: a
  lost PL011 RX interrupt (console stopped mid-line), a stale-waiter lost wakeup in `WaitQueue`,
  and a CPU taking interrupts before joining the scheduler during SMP bring-up (boot crash).

**Known limits**
- Kernel RAM is mapped RWX; processes have no user credentials yet, so file permission
  bits aren't checked (the capability sandbox is the access control).
- milkfs: one volume per disk, at most 30 snapshots + clones, sends only from the
  primary volume, scrub reports but doesn't repair.
- Needs QEMU on a Mac; Intel Macs only via slow, untested emulation.
