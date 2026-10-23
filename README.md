# milkOS

A hobby operating system for 64-bit ARM, written in Rust and run in qemu on a Mac. The kernel and
userland are inspired by Linux (Linux syscall numbers, `/proc`, `/dev`, a POSIX-ish shell); the desktop
is inspired by macOS (menu bar, dock, glass, Spaces, a launcher).

![The milkOS desktop](docs/screenshots/desktop.png)

## Run it
```
scripts/run.sh                 # desktop window + serial console in this terminal
scripts/run.sh headless        # serial console only
MILK_OFFLINE=1 scripts/run.sh  # no internet (tests always run like this)
MILK_SOUND=1 scripts/run.sh    # with sound through the Mac's speakers (off by default)
MILK_SHARE_RW=1 scripts/run.sh # make the Mac shared folder writable (read-only by default)
MILK_SMP=1 scripts/run.sh      # one CPU instead of four
```
Needs Rust nightly via rustup (`brew install rustup`; the toolchain is pinned in `rust-toolchain.toml`)
and `qemu-system-aarch64`. Boots with HVF acceleration on Apple Silicon.

On first boot a Welcome tour shows the main ideas with buttons that really do them.

Handy keys on the desktop: Ctrl/Cmd+Space opens Skim (the launcher), F3 opens Spaces, F4 opens Today.
The dock has Files, Terminal, Browser, Notes, Paint, Music and Settings; installed packages add their own
tiles, and the Trash sits at the right end. Ctrl+Cmd+Q locks the screen, and Ctrl+Alt+arrows snap windows to halves and quarters.

## What's in it
**Kernel**
- Boot on qemu `virt`, MMU with 4 KiB pages, frame allocator, kernel heap, GICv2, generic timer.
- SMP on 4 CPUs (PSCI bring-up, real spinlocks, IPIs, TLB shootdown), preemptive scheduler, wait queues.
- User processes at EL0 with their own address spaces, copy-on-write `fork`, threads and futexes, signals,
  job control, pipes, `chroot`. Over 80 Linux aarch64 syscalls (Linux numbering), plus a few milkOS ones.
- Capability sandbox: a process only gets the files, network and devices it is granted (`sandbox`). Apps
  installed as packages run sandboxed, and get extra files only through the Powerbox, an Open/Save panel
  drawn by milkOS itself.
- Tamper-evident audit log: every exec, connection, mount, snapshot and sandbox denial in a SHA-256 hash
  chain (`audit`, `audit verify`).

**Storage**
- VFS with tmpfs root, initramfs, devfs, procfs, sysfs.
- milkfs, our own copy-on-write filesystem: snapshots, rollback, writable clones, per-file versions,
  `disks changes`, automatic Time Machine snapshots, crash-tested (kill, power cut, torn superblock), `check --repair`.
- Your home folder lives on milkfs, so files survive reboots. Deleted things go to a real Trash
  (`~/.Trash`, or a disk's own), and are deleted for good after 30 days (Settings > Trash).
- Folders in plain English: `/Apps` (apps you open), `/Commands` (what you type in Terminal),
  `/Users/milk` (your home), `/Volumes` (disks) and `/System` (milkOS itself: its services and
  their sockets, settings, kernel controls, and the test tools in `/System/Developer`). `/dev`,
  `/proc` and `/tmp` stay for Linux and POSIX programs, hidden in listings. Details in
  `docs/filesystem.md`.
- FAT32 with long names, a page cache, and a shared folder with the Mac (`share/` here is `/Volumes/Mac` there).

**Network and sound**
- virtio-net with our own TCP/IP stack (Ethernet, ARP, IPv4, ICMP, UDP, DHCP, DNS, TCP, and TLS 1.3
  for https:// with the Mozilla root store, docs/tls.md), sockets,
  `network` (status, connections, ping, get, connect, listen, names; `network help`), `httpd`.
  Traffic stays inside qemu's private network and the Mac's loopback.
- virtio-sound with a mixer, `/dev/audio`, `play`, a boot chime and system sounds.

**Desktop**
- A compositor where every window has its own buffer, glass panels, shadows, animations, dirty-rect redraws.
- Menu bar, dock, Skim launcher, Today widgets, Spaces, notifications, control centre, a light and a dark
  ("Chocolate milk") theme.
- Skim is a system-wide command bar: apps, app actions, files, settings, a calculator (`= 2+2`) and
  shell commands (`> files list`). Windows snap to edges and corners. Lock screen. Browse All Versions for any
  file on milkfs. Screenshots with Cmd+Shift+3/4 (PNG). Live resolution switching (1024x768 to 1440x900).
- Apps: Terminal (real shell, tabs, colours), Music, Files (with Time Machine), Notes, Browser, Paint, Settings,
  About, Text Editor (`docs/text-editor.md`), Preview for PNG, PPM and BMP pictures and a peek into PDFs
  (`docs/preview.md`). User programs can open their own windows (`user/gui`).

**Userland**
- `msh`, a shell with pipelines, redirection, job control and structured pipes:
  `processes list | filter memory > 1M | sort-by name`. `go Documents` changes folder (`go` alone: home,
  `go back`: the one before) and `where` says where you are; there is no `cd` or `pwd`.
- `files`, one command for files and folders in plain English (`files create notes.txt`,
  `files copy`, `files move`, `files delete` to the Trash, `files list`, `files owner`,
  `files access notes.txt private`, `files help`), and `trash` (list, restore, empty).
- `processes` (list, watch, stop, signal, info), `services` (list, status, start, stop, restart,
  logs: the journal), `system` (info, memory, uptime), `users` (me, list, password, switch) and
  `admin` (run a command as root, or with an administrator's ticket), each with `help`.
- `text` for lines of text (`text show`, `text find`, `text first`, `text last`, `text sort`,
  `text unique`, `text count`, `text help`), `print` and `pause`; msh's `variables` built-in
  (`list`, `get`, `set`, `remove`, `run`).
- Coreutils, `disks` (free space, snapshots, clones, encryption, backups), `search` (files
  by name and text), and `apps`, a package manager with signed packages, where `apps try`
  installs into a throwaway clone of the filesystem that you then keep or discard. Each one's
  `help` lists what it does.

## Layout
- `kernel/`: the kernel crate (`no_std`). Entry is `kernel/src/boot.S`, then `kmain` in `main.rs`.
- `user/`: user programs and the `rt` runtime library, packed into the initramfs (`initramfs.list`).
- `packages/`: toy packages for `apps` (`fortune`, `snake`, `moo2048`, `games`).
- `rootfs/`: files copied into the initramfs (`/Apps` manifests, `/System/Settings`, system sounds,
  the home skeleton with the web root `~/Sites`).
- `scripts/`: run and test scripts. The main tests: `net-test.sh`, `pkg-test.sh`, `audio-test.sh`,
  `crashtest.sh`, `fattest.sh`, `wm-demo.sh <demo>`, `input-test.sh`, `screenshot.sh`.
  `check-all.sh` runs all of them, eight at a time (`-j N`), and prints one table (`--smoke` for a quick
  one-per-area check, `--full` for the long ones).
- `docs/`: design notes, one per area: `memory`, `processes`, `smp`, `filesystem`, `networking`, `audio`,
  `packages`, `security`, `shell`, plus `AGENTS.md` (how to work on milkOS) and `ui-concepts/` (the UI spec
  and mockups). Screenshots are in `docs/screenshots/`.

## Licence
milkOS is MIT licensed, see `LICENSE`. Third-party pieces keep their own licences (below).

## Credits
The console font is [GNU Unifont](https://unifoundry.com/unifont/) 13.0.06 (8x16 glyphs, U+0020..U+00FF),
used under the SIL Open Font License 1.1. Licence text: `assets/fonts/UNIFONT-LICENSE.txt`. Terminal,
Notes and the other monospace grids use it too.

The desktop's UI text is one of five typefaces, picked in Settings > Appearance (Figtree by default):
[Figtree](https://github.com/erikdkennedy/figtree), [IBM Plex Sans](https://github.com/IBM/plex),
[Fira Sans](https://github.com/mozilla/Fira) and
[Atkinson Hyperlegible Next](https://github.com/googlefonts/atkinson-hyperlegible-next) under the
SIL Open Font License 1.1, and [Ubuntu Sans](https://github.com/canonical/Ubuntu-Sans-fonts) under the
Ubuntu Font Licence 1.0 (licences and copyright lines in `assets/fonts/`, and in the image at
`/System/Fonts/`). `scripts/gen_sysfont.py` renders them into the glyph bitmaps in
`kernel/src/sysfont/`; it names the exact files it uses (from the Google Fonts repository) and their
hashes. The .ttf files are not in this repository.

The TLS root store (`assets/ca/mozilla-roots.der`) is the root certificates of Mozilla's CA program
(NSS's certdata.txt, via curl's cacert.pem), under the Mozilla Public License 2.0: `assets/ca/NOTICE.txt`
and `assets/ca/MPL-2.0.txt`. `scripts/gen_roots.py` records the bundle's date and SHA-256.
