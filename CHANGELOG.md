# Changelog

## Unreleased

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
- `go` and `where` instead of `cd` and `pwd`, which are gone: `go Documents`, `go ..`,
  `go /Apps`, `go` (home), `go back` (the folder before; a folder called back is
  `go ./back`); `where` prints the folder you're in, in pipes too, and takes no
  arguments. Errors in plain English ("Documents doesn't exist",
  "notes.txt is a file, not a folder", "you can't open ..."). In msh and the Terminal's
  built-in shell.
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

**File system**
- milkfs is its own crate (`<repo>/milkfs`, `no_std`, shared with the kernel like the crypto
  crate), with the kernel's VFS glue in `kernel/src/fs/milkfs`. Nothing changes on disk or in
  behaviour. It builds for the Mac too: `scripts/milkfs-test.sh` runs the file system there in
  a few seconds (memory disks, a fake clock, a seeded RNG, power cuts at many points, a drive
  that reorders unflushed writes), covering what the qemu milkfs tests check below the VFS;
  check-all runs it in the smoke set.

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
