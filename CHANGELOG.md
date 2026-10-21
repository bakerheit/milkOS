# Changelog

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

**Known limits**
- Kernel RAM is mapped RWX; processes have no user credentials yet, so file permission
  bits aren't checked (the capability sandbox is the access control).
- milkfs: one volume per disk, at most 30 snapshots + clones, sends only from the
  primary volume, scrub reports but doesn't repair.
- Needs QEMU on a Mac; Intel Macs only via slow, untested emulation.
