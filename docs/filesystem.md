# File systems

Files: `kernel/src/fs/` (milkfs: the `milkfs` crate in `<repo>/milkfs`, its VFS glue in
`fs/milkfs/`), `kernel/src/virtio/blk.rs`,
`kernel/build.rs` (initramfs packer), `rootfs/`, `initramfs.list`, `scripts/disk.sh`,
`scripts/mkmilkfs.py`, `scripts/crashtest.sh`, `user/snap`.
One small change outside: `gic.rs` counts interrupts per INTID for `/proc/interrupts`.

| module | what |
|---|---|
| `node.rs` | `FileSystem` and `Inode` traits, `Stat`, `DirEntry`, `SetAttr`, `InodeType`, `makedev` |
| `path.rs` | mount table, path walker (`.`, `..`, symlinks, mount crossing), `path_of` |
| `file.rs` | `OpenFile`: inode + offset + flags. What an fd table holds |
| `ops.rs` | the path API: `open`, `stat`, `mkdir`, `rename`, ... (re-exported as `fs::*`) |
| `abi.rs` | Linux aarch64 ABI: `O_*`, `SEEK_*`, `AT_*`, `struct stat`, `linux_dirent64` |
| `errno.rs` | `Errno` with Linux values |
| `tmpfs.rs` | RAM fs, mounted at `/` |
| `synth.rs` | building blocks for kernel-generated fs (dirs, generated files, symlinks) |
| `devfs.rs`, `procfs.rs`, `sysfs.rs` | `/dev`, `/proc`, `/sys` |
| `initramfs.rs` | cpio newc reader, unpacks the built-in archive into `/` |
| `blockdev.rs` | `BlockDevice` trait, disk registry, `/dev/vdX` byte-offset inode |
| `../virtio/blk.rs` | virtio-blk driver implementing `BlockDevice` |
| `bcache.rs` | block cache: fixed-size LRU of 4 KiB buffers, write-back, `flush` = durable |
| `ramdisk.rs` | RAM block device (the self-test's throwaway milkfs) |
| `ninep.rs` | 9P2000.L client: the Mac shared folder at `/Volumes/Mac` |
| `fat.rs` | FAT32 with VFAT long names, at `/Volumes/<LABEL>` |
| `pagecache.rs` | page cache for milkfs and FAT reads, `/sys/fs/pagecache` |
| `milkfs/` | milkfs v0, the copy-on-write snapshot file system (below), at `/Volumes/Milk` |
| `clock.rs` | wall-clock for timestamps (PL031 RTC read once + uptime) |
| `selftest.rs` | boot self-test (247 checks with both disks), then prints `/proc/*`, appends to `/Volumes/Milk/boot.log`, `cat /System/Settings/motd` |

Boot: `kmain` calls `fs::init()` after `memtest::run()`, then `fs::selftest::run()`.
`fs::init` mounts tmpfs on `/`, creates the skeleton, unpacks the initramfs, mounts devfs,
procfs and sysfs, then mounts the first disk that holds milkfs at `/Volumes/<label>`
(never formats anything).

## The tree

Plain English on top: three kinds of program, each with its own folder. An **app** has a
window (`/Apps`), a **command** is what you type in Terminal (`/Commands`, the shell's
`PATH`), a **service** runs in the background (`/System/Services`, started by milkd).
Constants and the two boot jobs below are in `fs/layout.rs`.

```text
/Apps            apps you open: the built-in ones and the packages milk installs
/Commands        what you type in Terminal (PATH=/Commands)
/Users/milk      the user's home ($HOME); /Users/root is root's
  Sites          the built-in web server's pages (httpd serves ~/Sites on :80)
/Volumes         disks, USB sticks, the Mac's shared folder
/System          the OS's own files
  Services       background helpers: milkd, alarmd, httpd, rngd, soundd, trashd
  Startup        init, the first program (PID 1)
  Settings       system settings: passwd, group, shadow, hostname, motd, milk/...
  Fonts          font licences
  Sounds         the system sounds (chime, notify, error, shutter)
  Music          the tunes ~/Music is seeded with
  Version.txt
/dev /proc /sys /tmp /run   (Linux-style, for programs that expect them)
```

| path | fs | what |
|---|---|---|
| `/` | tmpfs | root, everything in RAM |
| `/Apps` | milkfs (bind of `/Volumes/Milk/Apps`), tmpfs without a disk | one folder per app: its `manifest` and program. Built-in apps come from the initramfs and are copied into the volume's folder at boot (below); `milk install` adds packages (docs/packages.md), persistent |
| `/Commands` | tmpfs | command-line programs from the initramfs, plus `milk`'s links to installed packages (remade at boot) |
| `/Users` | tmpfs | homes: `/Users/milk`, `/Users/root` (0700) |
| `/Users/milk` | milkfs (bind of `/Volumes/Milk/Users/milk`), tmpfs without a disk | the user's home (`$HOME`), persistent |
| `/Users/milk/Sites` | (in the home) | the web root `httpd` serves (`/System/Settings/milk/services/httpd.service`) |
| `/Volumes` | tmpfs | disks: `/Volumes/Milk` is the milkfs volume, `/Volumes/Milk/.snapshots/<name>` its snapshots, `/Volumes/Milk-<name>` its clones |
| `/Volumes/Mac` | 9p | the Mac's `share/` folder (read-only unless `MILK_SHARE_RW=1`) |
| `/System` | tmpfs | the OS's own files; `/System/Version.txt` |
| `/System/Services` | tmpfs | background programs, started and supervised by milkd (docs/processes.md, "Services") |
| `/System/Startup` | tmpfs | `init`, the first program the kernel starts |
| `/System/Settings` | tmpfs | `passwd`, `group`, `shadow`, `hostname` (= `milk`), `motd`, `milk/` (`repos`, `keys/`, `services/`) |
| `/System/Settings/milk/drop`, `.../milk/net` | milkfs (binds of `/Volumes/Milk/System/Settings/milk/...`, `milkfs::setup_persistent_etc`), tmpfs without a disk | Milk Drop's identity and trusted peers (docs/networking.md), the hostname; persistent |
| `/System/Fonts`, `/System/Sounds`, `/System/Music` | tmpfs | font licences, system sounds, the seed tunes for ~/Music |
| `/tmp` | tmpfs | mode `1777` |
| `/run` | tmpfs | service sockets (`/run/milkd.sock`) |
| `/dev` | devfs | `null zero random urandom console`, `vda`... (one per disk) |
| `/proc` | proc | `meminfo uptime version interrupts mounts` |
| `/sys` | sysfs | `devices/platform/*`, `kernel/version`, `fs/milkfs/*` (snapshot control) |

There are no `/bin`, `/etc`, `/home`, `/srv` or `/Applications` folders, and no links
with those names: a clean break (the self-test checks they are gone).

**Built-in apps on the disk.** `/Apps` is one folder for both kinds of app, but the
built-in ones arrive in RAM (the initramfs) and the installed ones live on the volume. So
before the volume's `Apps` folder is bound onto `/Apps`, `layout::sync_builtin_apps`
copies each built-in app into it, only when the copy differs (a new milkOS version), and
removes built-in apps an older milkOS left that this one no longer ships (the list is
in `/Volumes/Milk/.milkos/builtin-apps`). Built-in apps have no `.origin` file; `milk`
never lists, replaces or removes them, and the desktop runs only those unsandboxed
(`layout::is_builtin_app`: shipped in this initramfs and no `.origin`).

**Older disks.** A volume made before this layout has `home/milk`, `Applications` and
`etc/milk/{drop,net}`. `layout::migrate` (run by `milkfs::setup_home`, at boot or when a
login unlocks the volume, before anything is bound) moves them once, by renames inside the
volume: `home/<user>` to `Users/<user>`, `Applications` to `Apps`,
`etc/milk/drop` and `etc/milk/net` to `System/Settings/milk/`. The moved home gets the
shipped `~/Sites` (the web pages used to be in RAM, at `/srv/www`) and loses its search
index (a cache with the old paths; it is rebuilt). It logs `layout: moved ...` and audits
`fs.layout.migrate`. Snapshots taken before keep the old names (they are read-only):
Files' Time Machine view and Versions look for `home/milk` there too. A clone made before
the move gets moved the boot after it is kept (promoted).

## Files and the Trash

One command for files and folders, `files` (`user/filetools`; it replaced `ls`, `cp`, `mv`,
`rm`, `mkdir`, `touch` and `ln`, which are gone), and a real Trash shared by `files`, the
`trash` command, the `trashd` service, Files and the dock. The rules live in `user/filekit`
(pure, host-tested with `scripts/filekit-test.sh`); the kernel's Files app uses the same
crate (`kernel/src/wm/trash.rs`).

```text
files create notes.txt        an empty file (an existing file just gets its time set to now)
files create projects/a/      a folder: the trailing slash decides; parents too (mkdir -p)
files copy SRC... DST         folders with everything in them; --reflink[=auto] shares blocks
files move SRC... DST         rename or move, folders too; across disks: copy, then delete
files rename OLD NEW          in place; NEW is a name, not a path
files delete PATH...          to the Trash
files delete --now PATH...    for good
files info PATH...            kind, size (or items), where, which disk, dates, owner, permissions
files list [-l] [-a] [PATH]   folders first, "name/" for folders; --long, --all (dot files)
files link TARGET NAME        a symbolic link
trash [list]                  name, where it was, when deleted, days left
trash restore NAME|PATH...    back where it was; refused if something is there now
trash empty [--yes]           for good (asks first)
```

* **The trailing slash.** On `create` it decides file or folder. Everywhere else what is on
  disk decides, and a slash only says "a folder": `files delete projects` and
  `files delete projects/` both delete the folder, `files delete notes.txt/` on a file is an
  error ("notes.txt is a file, not a folder"), never a guess. A slash on a link to a folder
  means the folder for `list` and `info`, and is an error for the commands that change things.
* **Destinations.** When DST is a folder, `copy` and `move` put SRC inside it; a slash on DST
  means it must be a folder that is there. Nothing is overwritten unless `--replace` (a file
  over a file only). `move` renames with `RENAME_NOREPLACE`; on `EXDEV` it copies, and deletes
  the original only once the copy is whole (a failed copy is removed again).
* **Errors** are sentences: "notes.txt doesn't exist", "projects is a folder, not a file",
  "backup/notes.txt already exists (add --replace to overwrite it)" (`filekit::words`).
* **Tables.** `files list` and `trash list` give msh a table when it asks (docs/shell.md).

**Where things go.** `files delete` (and Files, and a drop on the dock's Trash) renames the
item into a Trash on its own disk, never copying it:

| the item is on | its Trash |
|---|---|
| the same file system as your home (`/Users/milk` is on the Milk disk) | `~/.Trash` (0700, yours) |
| another milkfs (or tmpfs) disk mounted at `/Volumes/<disk>` | `/Volumes/<disk>/.Trash` (1777, sticky; each item's record says whose it is) |
| a FAT disk, the Mac's shared folder, the RAM root (`/tmp`), `/proc`... | none: `files delete` says so and offers `files delete --now` |
| a read-only disk (a snapshot, the Mac folder without `MILK_SHARE_RW`) | none: it can't be deleted at all |

**Layout and records.** Items sit in the Trash folder under their own name; a second
`notes.txt` becomes `notes 2.txt` (`filekit::trash::free_name`), so nothing in the Trash is
ever overwritten. Each item has a small text file in `.trashinfo/` with the same name:

```text
# milkOS Trash: where this item came from (trash restore puts it back)
path = /Users/milk/Documents/notes.txt
deleted = 1792537265
uid = 501
```

The record is written first, create-exclusive (`O_EXCL`: two deletes never pick one name),
then the item is renamed in with `RENAME_NOREPLACE`; restoring renames it back (refusing if
something is at the old path now, making missing parent folders) and then removes the record.
So a crash leaves at worst a record with no item (ignored; `trashd` removes it after an hour)
or an item with no record (listed as from "(unknown)", aged by its change time, which a
rename sets). Everything is renames and small files on the same disk, so it survives reboots
like any other file. Root deleting into someone's home Trash keeps the Trash and the records
theirs (chown to the home's owner). Deleting something that is already in a Trash deletes it
for good.

**How long things stay.** `trash = 30` (days, 1..3650) or `trash = never` in
`~/.config/milk/settings`, the desktop settings file; Settings > Trash sets it (1, 7, 30 or
90 days, or Never; another number typed into the file shows as its own choice). The default
is 30. **`trashd`** (`/System/Services/trashd`, a milkd service,
`/System/Settings/milk/services/trashd.service`) goes through every Trash at boot and then
about once a day: each home's (`/Users/*/.Trash`, with that home's setting) and each disk's
(each person's items with their own setting), deleting for good what has been there longer.
It runs as root but does each Trash in a child that has become that Trash's owner, so it
never deletes with more rights than they have (links are removed, never followed). It logs
what it deleted to the journal (`log -u trashd`). `trashd --once [--later DAYS]` does one
pass now, pretending it is DAYS later (for tests).

**Desktop.** Files: Cmd+Backspace, Delete, or Move to Trash in the right-click menu; the
sidebar's Trash lists what is there (where from, when, days left) with Restore for the
selected item, Restore / Delete for Good on right-click, and Empty Trash (a second click on
"Delete N items for good" confirms). The dock's last tile is the Trash, empty or full (it
looks once a second); clicking it opens the Trash in Files, dropping a file on it deletes the
file to the Trash.

**Tests:** `scripts/filekit-test.sh` (the rules, on the Mac) and `scripts/files-test.sh` (both
commands end to end in qemu, including a disk's own Trash, the FAT refusal and `trashd`
deleting at boot).

## Kernel API (for the syscall layer)

All in `crate::fs`. `Result<T> = core::result::Result<T, Errno>`. A syscall returns
`e.neg()` (= `-errno` as `isize`) on error.

```rust
pub type InodeRef = Arc<dyn Inode>;

// open / files
pub fn open(path: &str, flags: u32, mode: u32) -> Result<OpenFile>;
pub fn open_at(base: Option<&InodeRef>, path: &str, flags: u32, mode: u32) -> Result<OpenFile>;

impl OpenFile {
    pub fn read(&self, buf: &mut [u8]) -> Result<usize>;          // read(2), advances offset
    pub fn write(&self, buf: &[u8]) -> Result<usize>;             // write(2), O_APPEND aware
    pub fn pread(&self, buf: &mut [u8], off: u64) -> Result<usize>;
    pub fn pwrite(&self, buf: &[u8], off: u64) -> Result<usize>;
    pub fn seek(&self, off: i64, whence: u32) -> Result<u64>;     // lseek(2)
    pub fn stat(&self) -> Stat;                                   // fstat: .to_linux()
    pub fn getdents64(&self, buf: &mut [u8]) -> Result<usize>;    // fills linux_dirent64s
    pub fn truncate(&self, len: u64) -> Result<()>;               // ftruncate(2)
    pub fn ioctl(&self, cmd: u32, arg: usize) -> Result<usize>;   // ENOTTY by default
    pub fn flags(&self) -> u32;  pub fn set_flags(&self, f: u32); // fcntl F_GETFL / F_SETFL
    pub fn readable(&self) -> bool;  pub fn writable(&self) -> bool;
    pub fn inode(&self) -> &InodeRef;                             // fchdir, *at with a dirfd
    pub fn pos(&self) -> u64;
}

// metadata
pub fn stat(path: &str) -> Result<Stat>;
pub fn lstat(path: &str) -> Result<Stat>;
pub fn stat_at(base: Option<&InodeRef>, path: &str, follow: bool) -> Result<Stat>;
pub fn lookup(path: &str) -> Result<InodeRef>;                    // chdir: check kind == Dir
pub fn lookup_at(base: Option<&InodeRef>, path: &str, follow: bool) -> Result<InodeRef>;
pub fn path_of(inode: &InodeRef) -> Result<String>;               // getcwd
pub fn readdir(path: &str) -> Result<Vec<DirEntry>>;              // kernel convenience, no . / ..

// namespace
pub fn mkdir(path: &str, mode: u32) -> Result<()>;
pub fn mkdir_at(base: Option<&InodeRef>, path: &str, mode: u32) -> Result<()>;
pub fn mkdir_all(path: &str, mode: u32) -> Result<()>;
pub fn unlink(path: &str) -> Result<()>;                          // + unlink_at
pub fn rmdir(path: &str) -> Result<()>;                           // + rmdir_at (AT_REMOVEDIR)
pub fn rename(old: &str, new: &str) -> Result<()>;
pub fn rename_at(ob: Option<&InodeRef>, old: &str, nb: Option<&InodeRef>, new: &str) -> Result<()>;
pub fn rename_noreplace(old: &str, new: &str) -> Result<()>;      // EEXIST instead of replacing
pub fn symlink(target: &str, linkpath: &str) -> Result<()>;      // + symlink_at(target, base, linkpath)
pub fn readlink(path: &str) -> Result<String>;                    // + readlink_at
pub fn link(old: &str, new: &str) -> Result<()>;                  // + link_at(.., follow)
pub fn truncate(path: &str, len: u64) -> Result<()>;
pub fn chmod(path: &str, perm: u32) -> Result<()>;
pub fn setattr_at(base: Option<&InodeRef>, path: &str, follow: bool, a: &SetAttr) -> Result<()>;

// whole files (loading ELF programs etc.)
pub fn read_file(path: &str) -> Result<Vec<u8>>;
pub fn read_to_string(path: &str) -> Result<String>;
pub fn write_file(path: &str, data: &[u8]) -> Result<()>;

// mounts
pub fn mount(path: &str, fs: Arc<dyn FileSystem>) -> Result<()>;
pub fn root() -> InodeRef;
```

How the syscall layer is expected to use it:

- The fd table holds `Arc<OpenFile>`. `dup`/`fork` share the `Arc` (and so the offset, like
  Linux). `O_CLOEXEC` stays in `flags()`; acting on it is the fd table's job.
- cwd is an `InodeRef` (from `lookup`, checked to be a directory). `AT_FDCWD` → `Some(&cwd)`;
  a dirfd → `Some(file.inode())`. Absolute paths ignore the base.
- `AT_SYMLINK_NOFOLLOW` → `follow = false`. `AT_REMOVEDIR` → `rmdir_at` instead of `unlink_at`.
  `AT_EMPTY_PATH` with an empty path → use the fd's inode directly (the VFS gives `ENOENT`
  for `""`).
- Apply the umask to `mode` before calling `open`/`mkdir`. There are no permission checks
  in the VFS yet: everything behaves as root.
- Copying to/from user memory is the syscall layer's job; the VFS only sees kernel slices.
  `getdents64` writes into a kernel buffer: allocate one of the user's size, call, copy out.

## Userspace layouts

### `struct stat` (aarch64 = asm-generic, 128 bytes)

`Stat::to_linux()` returns `abi::LinuxStat` (`#[repr(C)]`, size/offsets checked at compile
time); `.as_bytes()` gives the 128 bytes to copy out for `fstat` (80) / `newfstatat` (79).

| off | size | field | milkOS value |
|---|---|---|---|
| 0 | 8 | `st_dev` u64 | fs device number (`makedev(0, n)`, unique per mount) |
| 8 | 8 | `st_ino` u64 | inode number, unique within the fs |
| 16 | 4 | `st_mode` u32 | `S_IF*` type bits \| permission bits |
| 20 | 4 | `st_nlink` u32 | links (dirs: 2 + subdirs) |
| 24 | 4 | `st_uid` u32 | 0 |
| 28 | 4 | `st_gid` u32 | 0 |
| 32 | 8 | `st_rdev` u64 | device number for char/block devices |
| 40 | 8 | `__pad1` | 0 |
| 48 | 8 | `st_size` i64 | bytes (0 for procfs/sysfs files: read to EOF) |
| 56 | 4 | `st_blksize` i32 | 4096 (1024 for devices) |
| 60 | 4 | `__pad2` | 0 |
| 64 | 8 | `st_blocks` i64 | 512-byte blocks allocated |
| 72 | 8 | `st_atime` i64 | Unix seconds |
| 80 | 8 | `st_atime_nsec` u64 | |
| 88 | 8 | `st_mtime` i64 | |
| 96 | 8 | `st_mtime_nsec` u64 | |
| 104 | 8 | `st_ctime` i64 | |
| 112 | 8 | `st_ctime_nsec` u64 | |
| 120 | 8 | `__unused4`, `__unused5` u32 | 0 |

`st_mode` type bits: `S_IFREG 0o100000`, `S_IFDIR 0o040000`, `S_IFLNK 0o120000`,
`S_IFCHR 0o020000`, `S_IFBLK 0o060000`, `S_IFIFO 0o010000`, `S_IFSOCK 0o140000`.

### `struct linux_dirent64` (getdents64, syscall 61)

| off | size | field |
|---|---|---|
| 0 | 8 | `d_ino` u64 |
| 8 | 8 | `d_off` i64: cookie for the *next* entry (an index; `lseek(fd, d_off, SEEK_SET)` resumes there) |
| 16 | 2 | `d_reclen` u16: record length, multiple of 8 |
| 18 | 1 | `d_type` u8: `DT_REG 8`, `DT_DIR 4`, `DT_LNK 10`, `DT_CHR 2`, `DT_BLK 6`, `DT_FIFO 1`, `DT_SOCK 12` |
| 19 | n+1 | `d_name`: name + NUL, then zero padding to `d_reclen` |

`d_reclen = (19 + len + 1)` rounded up to 8. `.` and `..` come first. Returns 0 at the end,
`EINVAL` if the buffer can't hold even one record.

### open flags (arm64 values; note the four that differ from asm-generic)

`O_RDONLY 0`, `O_WRONLY 1`, `O_RDWR 2`, `O_CREAT 0o100`, `O_EXCL 0o200`, `O_NOCTTY 0o400`,
`O_TRUNC 0o1000`, `O_APPEND 0o2000`, `O_NONBLOCK 0o4000`, **`O_DIRECTORY 0o40000`**,
**`O_NOFOLLOW 0o100000`**, **`O_DIRECT 0o200000`**, **`O_LARGEFILE 0o400000`**,
`O_CLOEXEC 0o2000000`, `O_PATH 0o10000000`. `AT_FDCWD -100`, `AT_SYMLINK_NOFOLLOW 0x100`,
`AT_REMOVEDIR 0x200`, `AT_SYMLINK_FOLLOW 0x400`, `AT_EMPTY_PATH 0x1000`.

### errno (Linux values, `fs::Errno::*`)

EPERM 1, ENOENT 2, EIO 5, ENXIO 6, EBADF 9, EAGAIN 11, ENOMEM 12, EACCES 13, EFAULT 14,
EBUSY 16, EEXIST 17, EXDEV 18, ENODEV 19, ENOTDIR 20, EISDIR 21, EINVAL 22, EMFILE 24,
ENOTTY 25, EFBIG 27, ENOSPC 28, ESPIPE 29, EROFS 30, EMLINK 31, ERANGE 34,
ENAMETOOLONG 36, ENOSYS 38, ENOTEMPTY 39, ELOOP 40, EOPNOTSUPP 95.

## Semantics worth knowing

- Paths: max 4095 bytes, components max 255. Repeated slashes are fine; a trailing slash
  means "must be a directory". Symlinks: 40 per lookup, 8 nested, `ELOOP` beyond.
  `O_CREAT` through a dangling symlink creates its target, as on Linux.
- Mounts: walking into a covered directory lands on the mounted root; `..` from a mounted
  root goes to the covered directory's parent. Mount points and mount roots can't be removed
  or renamed (`EBUSY`); renames and links across file systems give `EXDEV`.
- POSIX rename rules: replaces a file or an *empty* directory, `EINVAL` for moving a
  directory into its own subtree, no-op when both names are the same inode.
  `renameat2(..., RENAME_NOREPLACE)` (`fs::rename_noreplace`) gives `EEXIST` instead of
  replacing (what `files move` and the Trash use); `RENAME_EXCHANGE` is `EINVAL`.
- Unlinked files stay readable through open `OpenFile`s; tmpfs frees the pages when the
  last reference goes (checked by the self-test against the PMM free count).
- tmpfs: data lives in whole PMM frames (page-aligned: `mmap` can map them directly later),
  sparse files are supported, max file size 1 GiB, `ENOSPC` when the PMM runs dry. A size cap
  per mount is supported (`TmpFs::new(max_bytes)`), unused for `/`.
- procfs/sysfs files are generated on every `read_at`, so a file read in several chunks
  could see content change between chunks. `st_size` is 0, read until a 0-byte read.
- `/dev/console`: writes go to the `kprintln!` sink (UART + screen), invalid UTF-8 becomes
  U+FFFD. Reads take whatever `uart::read_byte` has, mapping CR to LF, and return `EAGAIN`
  when nothing is waiting (there are no wait queues yet). Note `kmain`'s idle loop
  (`uart::echo_pending`) consumes the same bytes.
- `/dev/random` and `/dev/urandom`: the kernel CSPRNG (ChaCha20 with fast key erasure,
  seeded from virtio-rng + timer jitter, docs/crypto.md). Reads block only until the boot
  seed; writes are mixed into its pool.
- Timestamps: Unix time from the PL031 RTC read once at `fs::init`, plus uptime. No atime
  updates.
- Locking: tmpfs, devfs, procfs and sysfs use `SpinLock`s and never block (tmpfs copies
  file data with IRQs masked). milkfs, the bcache and virtio-blk **sleep** (`sched::Mutex`,
  wait queues): so VFS calls are thread context only, never from IRQ context and never
  while holding a `SpinLock` (that's also true because the VFS allocates).

## statfs (`df`, Disk Utility)

`statfs(2)` (syscall 43, a path, symlinks followed) and `fstatfs(2)` (44, an fd) answer
for every file system; kernel side `fs::statfs(path)` / `fs::statfs_of(&inode)` ->
`node::StatFs`, each file system's `FileSystem::statfs`. The file system is found by the
inode's device number among the mounts (a bind mount like `/Users/milk` answers for what
it binds); a milkfs snapshot browsed under `.snapshots` (its own device number) answers
through its volume; pipes and sockets get type 0 and zero sizes.

Copied out as Linux's aarch64 `struct statfs` (asm-generic, 64-bit), 120 bytes, all
little-endian:

| offset | field | type | milkOS |
|---|---|---|---|
| 0 | `f_type` | i64 | magic number, below |
| 8 | `f_bsize` | i64 | block size (bytes) |
| 16 | `f_blocks` | u64 | size in blocks |
| 24 | `f_bfree` | u64 | free blocks |
| 32 | `f_bavail` | u64 | free blocks for unprivileged users (= `f_bfree`, nothing is reserved) |
| 40 | `f_files` | u64 | inodes (0: no fixed inode table) |
| 48 | `f_ffree` | u64 | free inodes |
| 56 | `f_fsid` | i32[2] | file system id (low word first) |
| 64 | `f_namelen` | i64 | longest name (255) |
| 72 | `f_frsize` | i64 | = `f_bsize` |
| 80 | `f_flags` | i64 | `ST_RDONLY` 1, `ST_NOSUID` 2, `ST_NODEV` 4, `ST_NOEXEC` 8, `ST_VALID` 0x20 (always) |
| 88 | `f_spare` | i64[4] | 0 |

| file system | `f_type` | block | size / free | flags, fsid |
|---|---|---|---|---|
| milkfs | `0x4d494c4b` ("MILK") | 4096 | the device's blocks / free in the space map (copy-on-write: a write needs free blocks even when it overwrites) | snapshots and a broken volume `ST_RDONLY`; fsid from the UUID (same for the live tree, snapshots and clones) |
| tmpfs | `0x01021994` | 4096 | the cap, or without one used + free RAM / what's left of that | |
| FAT32 | `0x4d44` (msdos) | cluster size | data clusters / free clusters (FSInfo, kept exact) | `ST_NOSUID ST_NODEV`, `ST_RDONLY` if mounted read-only |
| 9p (`/Volumes/Mac`) | `0x01021997` | the Mac's | the Mac's numbers for the shared folder's disk (Tstatfs), so "free" is the Mac's free space | `ST_NOSUID ST_NODEV`, `ST_RDONLY` unless the share is writable |
| proc, sysfs, devfs | `0x9fa0`, `0x62656572`, `0x1373` | 4096 | 0 / 0 | |

User side: `rt::sys::statfs(path)`, `rt::sys::fstatfs(fd)` -> `rt::sys::StatFs` (with
`type_name()`, `read_only()`). Tool: `df [-h] [-a] [PATH...]` (coreutils): without paths
every mount in `/proc/mounts`, minus the ones with no storage (proc, sysfs, devfs) and
repeats of one file system (binds), `-a` for all; 1K blocks, `-h` human; `(ro)` after a
read-only mount. With `MILK_TABLE=1` a table: `mount type size used avail use_pct bsize
blocks bfree bavail files ffree ro` (sizes in bytes; `use_pct` like GNU df: used /
(used + avail), rounded up; empty for no storage).

```text
Type      1K-blocks       Used      Avail  Use%  Mounted on
tmpfs        492356      10168     482188    3%  /
milkfs        65536         40      65496    1%  /Volumes/Milk
9p        482746440  381604980  101141460   80%  /Volumes/Mac (ro)
vfat          64511          0      64510    1%  /Volumes/DFFAT
```

Tests: the self-test (`statfs_basics`: RAM file systems, the struct layout; on the
milkfs test volume: sizes, a write and a delete moving the free count, a snapshot
read-only) and `scripts/dftest.sh` (`df` against the Mac's numbers: the milkfs image's
size, the FAT image's BPB and FSInfo, `os.statvfs` of the share, growth by a written
file).

## Writing a file system

Implement `FileSystem` (`name`, `dev` from `node::alloc_dev()`, `root`, and `statfs` if it
has storage; the default reports zero sizes) and `Inode` for
your objects. Every `Inode` method has a default error, so only implement what you support.
Names passed in are single components, never `.`/`..` (except `lookup("..")`, which every
directory must answer; the root of a fs returns itself). Don't hold your own lock while
calling into another inode. Use `node::downcast::<YourInode>(&inode_ref)` when an operation
gets another inode (rename's `new_dir`, link's `target`). Then `fs::mount("/somewhere", fs)`.

For kernel-generated trees use `synth::SynthFs`: `root_dir().add_dir(..)`,
`add_file(name, || String)`, `add_text`, `add_symlink`, `add(name, custom_inode)`.
Drivers publish device nodes with `devfs::add_device(name, inode)` (and `devfs::ids()` for
inode numbers). `procfs::root()` gives the `/proc` root for `/proc/<pid>` and `/proc/self`.

## Adding files to the initramfs

`kernel/build.rs` packs a cpio newc archive into `$OUT_DIR/initramfs.cpio`, which
`fs/initramfs.rs` embeds with `include_bytes!` and unpacks into `/` at boot (after the
skeleton, so it can add to `/Commands`, `/System/Settings`, ...). Entries are sorted, uid/gid 0, mtime 0:
the archive is reproducible. Two ways in:

1. **Files checked into git**: put them under `rootfs/` at the same path they should have
   at runtime (`rootfs/System/Settings/motd` → `/System/Settings/motd`, `rootfs/Apps/<name>/manifest`). Permissions come from the host file;
   symlinks are kept as symlinks.
2. **Build outputs** (user programs): list them in `initramfs.list` at the repo root, one
   per line, `<path in image> <host path relative to repo root> [octal mode]`. A command
   goes in `/Commands`, an app in `/Apps/<name>/` (next to its manifest in rootfs/), a
   service in `/System/Services`:

   ```
   /System/Startup/init   target/user/aarch64-unknown-none/release/init   755
   /Commands/msh          target/user/aarch64-unknown-none/release/msh    755
   /Apps/paint/paint      target/user/aarch64-unknown-none/release/paint  755
   ```

   Parent directories are created as needed. A missing host file is skipped with a
   `cargo:warning` (so the kernel still builds before the user programs are), and cargo
   re-runs the packer when any listed file changes. Build the user programs first, then
   `cargo build` the kernel. `fs::read_file("/System/Startup/init")` then hands you the ELF.

## Block devices

```rust
pub trait BlockDevice: Send + Sync {
    fn name(&self) -> &str;                 // "vda"
    fn block_size(&self) -> usize;          // 512 for virtio-blk
    fn num_blocks(&self) -> u64;
    fn read_only(&self) -> bool;
    fn read_blocks(&self, lba: u64, buf: &mut [u8]) -> Result<()>;   // whole blocks
    fn write_blocks(&self, lba: u64, buf: &[u8]) -> Result<()>;
    fn flush(&self) -> Result<()>;
}
fs::blockdev::register(dev: Arc<dyn BlockDevice>);   // drivers; any time
fs::blockdev::devices() -> Vec<Arc<dyn BlockDevice>>; fs::blockdev::get("vda")
```

Each registered disk appears as `/dev/<name>` (block special, `rdev` 254:16n), readable and
writable at any byte offset (read-modify-write at unaligned edges, 64 KiB per call).

virtio-blk (`kernel/src/virtio/blk.rs`, probed by `virtio::init`): one queue, one request
in flight, header/status page + 64 KiB bounce buffer from `pmm::alloc_contiguous_zeroed`
(so callers pass any kernel buffer; bigger transfers are split). Negotiates `RO` and
`FLUSH`. The device lock is a `sched::Mutex`; a request sleeps on a `WaitQueue` until its
used-ring entry appears (5 s timeout) and the IRQ handler acks and wakes it. Before
`sched::init` that wait is a `wfi` loop, so the boot self-test uses the same path.

Block cache (`fs/bcache.rs`): `BCache::new(dev, nbufs)` over 4 KiB blocks, `read`, `write`
(write-back), `write_through`, `flush` (all dirty buffers in block order, then the device
flush: after it returns, everything written before is durable), `stats`. LRU eviction
writes dirty buffers back early. Lock: `sched::Mutex` (held across I/O).

Scripts: `scripts/disk.sh` attaches two disks: `/tmp/milk-disk.img` (scratch: 64 MiB,
sparse, a text label in sector 0) and `/tmp/milk-fs.img` (milkfs, made once with
`mkmilkfs.py`). Which one becomes `vda` depends on qemu's slot order, so the kernel goes by
content: milkfs mounts whichever disk has a milkfs superblock, the scratch test uses the
first that doesn't. `run.sh` attaches them writable (persistent; qemu takes a write lock,
so one `run.sh` at a time). `screenshot.sh` attaches them with
`snapshot=on,file.locking=off`: writes are discarded and any number of qemus can share them.
The scratch test reads sector 0, then writes/reads/flushes the last 80 KiB directly, through
`/dev/vdX` and through a 4-buffer bcache, and restores what was there.

## Mac shared folder (virtio-9p, `/Volumes/Mac`)

`kernel/src/virtio/p9.rs` (transport) + `kernel/src/fs/ninep.rs` (9P2000.L client and VFS
objects), `scripts/share.sh` (qemu arguments, sourced by run.sh / screenshot.sh /
input-test.sh).

- **Exactly one host folder** is shared: `<main checkout>/share/` (`~/workspace/milkOS/share`),
  found through `git rev-parse --git-common-dir` so every worktree uses the same one. There
  is no option to share any other path. The folder is gitignored except its `README.txt`.
- **Read-only by default**: qemu gets `readonly=on`, and the kernel refuses writes itself
  (EROFS) unless the boot sets `-fw_cfg name=opt/milkos/share-rw,string=1`, which
  `MILK_SHARE_RW=1 scripts/run.sh` does (and drops `readonly=on`).
- qemu: `-fsdev local,id=fs0,path=<share>,security_model=mapped-xattr[,readonly=on]
  -device virtio-9p-device,fsdev=fs0,mount_tag=mac`. `mapped-xattr` works on macOS; files
  milkOS creates are mode 0600 on the Mac (owned by you), guest permissions live in xattrs.
- Supported: lookup, readdir, stat, read; with rw: write, create, mkdir, symlink, unlink,
  rmdir, rename, truncate, chmod/utimens, fsync. Inode numbers are qid paths. Each inode
  keeps a walked path fid plus lazily opened read/write fids; fids no longer needed are
  clunked on the next request (never from `Drop`). msize 64 KiB, one request in flight,
  sleeping on a wait queue the virtio IRQ wakes.
- Dev loop: copy a built program into `share/` and run `/Volumes/Mac/<name>` from msh.

## FAT32 (`/Volumes/<LABEL>`)

`kernel/src/fs/fat.rs`, `scripts/mkfat.sh`, `scripts/fattest.sh`.

- Read/write FAT32 with VFAT long file names (UTF-16 LFN entries, checksummed, in front of
  a generated `BASIS~N.EXT` alias; plain upper-case 8.3 names get no LFN; NT lower-case
  flags honoured when reading). Names compare case-insensitively and keep their case.
- Mounted automatically (`fat::automount` in `fs::init`) for every disk with a FAT32 boot
  sector (512-byte sectors), at `/Volumes/<volume label>`. The scratch-disk self-test skips
  FAT disks.
- The whole FAT is cached in RAM; changed FAT sectors are written through to every copy at
  the end of each operation; the FSInfo hint is set to "unknown" on the first write (valid
  per the spec, fsck_msdos agrees). Data moves a cluster at a time.
- FAT has no inodes: directories use their first cluster as inode number, files the
  position of their 8.3 entry (updated on rename). No holes (gaps are written as zeros), no
  symlinks/hard links (EPERM), permissions = the read-only bit, no uid/gid. Unlinking an
  open file frees its data immediately.
- Times: FAT stores local time; milkOS has no time zone and writes UTC (the RTC).
- Make an image: `scripts/mkfat.sh [/tmp/IMAGE] [MiB] [LABEL]` (hdiutil attach -nomount +
  newfs_msdos -F 32; /tmp only). Boot with it: `MILK_FAT=/tmp/milk-fat.img scripts/run.sh`.
- `scripts/fattest.sh`: the Mac writes files (long names, spaces, non-ASCII, nested, 300 KB
  random, 8.3 upper/lower, empty), milkOS reads them back out through `/Volumes/Mac` for
  byte-exact compares, writes (long-name dir and file, 200 KB non-text file, rename to a long name,
  delete, dir move), then the Mac attaches the image read-only and checks it all;
  `fsck_msdos -n` before and after.

## Page cache

`kernel/src/fs/pagecache.rs`. File reads on milkfs and FAT go through one global, bounded
LRU of 4 KiB pages (2048 pages = 8 MiB, PMM frames), keyed per inode by `(dev, ino, page)`.
The file system's read path fills a missing page outside the cache lock.

- Invalidation: milkfs drops an inode's pages on write and truncate, and a whole view's on
  rollback, promote and discard (contents change without a write). FAT also on create,
  unlink and rename, because its inode numbers are directory-entry slots that get reused.
  Every invalidation bumps an epoch; a page read while an invalidation happened is not
  inserted, so a racing write can't leave stale data behind.
- Not cached: 9p (the Mac can change files underneath), tmpfs (already RAM), devices.
- No `mmap` of files yet (that's proc/uspace's side).
- `/sys/fs/pagecache/`: `stats`, `enabled` (write 0/1), `drop`, `bench` (write a path: it
  is read twice, cold then warm, and the timings are printed and readable from `bench`).

Measured with `scripts/cachebench.sh` (4 MiB random file, real virtio disks, HVF):

| | cache off, 2nd read | cache on, 1st read | cache on, 2nd read |
|---|---|---|---|
| milkfs (`/Users/milk`) | 38 ms (104 MiB/s) | 48 ms (83 MiB/s) | 0.66 ms (6.0 GiB/s) |
| FAT32 (512 B clusters) | 34 ms (118 MiB/s) | 39 ms (103 MiB/s) | 0.39 ms (10 GiB/s) |

Filling the cache costs a frame allocation and a copy per page, so a first read with the
cache on is slightly slower. FAT reads also walk the cluster chain from a per-file cursor
and read runs of contiguous clusters in one request (before that change, a cold 4 MiB FAT
read took 289 ms).

## SMP

With 4 CPUs, file-system calls really run in parallel: user processes on different CPUs,
the `milkfs-flush` and `timemachine` threads, virtio completions (IRQs on CPU 0, waiters
anywhere). Locking that makes it safe: one `sched::Mutex` per milkfs volume and per FAT
volume (and per bcache and virtio device), spinlocks only for short map updates and never
held across I/O, always taken in one order (inode map -> node / fid / snapshot-dev locks,
never back); page-cache fills run unlocked and are guarded by the invalidation epoch.

`user/fsstress DIR [WORKERS] [ROUNDS] [READFILE]` forks workers (one per CPU and more)
that write / read-verify / rename / delete files of varying size in parallel, make
reflink copies on milkfs (verify, write into the copy, check the original is unchanged),
optionally re-reading a shared file every round. `scripts/stressloop.sh [BOOTS]` runs it
on fresh private milkfs + FAT32 images with Time Machine snapshotting every second, then
`dedupe`, `snap check`, and on the Mac `mkmilkfs.py check` and `fsck_msdos -n`. Run on `-smp 4` for 8 boots, each with Time Machine
snapshotting every second meanwhile: 8 workers x 20 rounds on milkfs and 4 x 8 on FAT32
per boot, 0 errors, `mkmilkfs.py check` clean, `fsck_msdos` with nothing to fix. The test
scripts (`crashtest.sh`, `fattest.sh`, `cachebench.sh`) boot `-smp ${MILK_SMP:-4}`.

## Change notifications (`fs::notify`)

`kernel/src/fs/notify.rs`, an inotify-lite for kernel code: every namespace change and
every close of a written file, in order, to whoever watches.

```rust
let w: Arc<notify::Watcher> = notify::watch(4096); // queue capacity; unwatch = drop it
while let Some(ev) = w.next(200_000_000) {          // wait up to 200 ms
    match ev {
        Event::Created { dir, name, obj, kind } => {} // name appeared in dir (create,
                                                      // mkdir, symlink, link)
        Event::Removed { dir, name, obj } => {}      // unlink, rmdir
        Event::Renamed { from_dir, from, to_dir, to, obj } => {} // whatever `to` was is gone
        Event::Modified { obj } => {}                 // last close after a write, truncate,
                                                      // O_TRUNC, FICLONE into it
        Event::Attr { obj } => {}                     // chmod, chown, utimensat
        Event::Rescan { dev } => {}                   // rollback, receive, promote on dev
        Event::Overflow => {}                         // this watcher fell behind: rescan
    }
}
```

- Objects are `(dev, ino)` ids (`notify::id(&inode)`), not paths: a path costs a walk up
  `..`, depends on the caller's root (chroot) and has several spellings with bind mounts.
  A watcher that wants paths maps directory ids to paths itself (the search indexer does;
  `/Users/milk` and `/Volumes/Milk/Users/milk` are then the same directory).
- Emitted by `fs::ops` (`open_at` with O_CREAT/O_TRUNC, `mkdir_at`, `symlink_at`,
  `link_at`, `unlink_at`, `rmdir_at`, `rename_at`, `truncate`, `setattr_at`, `ficlone`)
  and by `OpenFile` (`Modified` when the last reference to an open file that was written
  goes, like inotify's IN_CLOSE_WRITE). So every syscall and every kernel caller of
  `fs::ops` is covered; a file system that changes contents behind the VFS calls
  `notify::rescan(dev)` (milkfs: rollback, receive, promote).
- Cost with no watcher: one atomic load per operation. With watchers: a small allocation
  and a spinlock per event and watcher. A full queue drops events and the watcher's next
  `next()` returns `Overflow` (once).
- Events are reported after the change succeeded; by the time a watcher handles one the
  tree may have moved on (a file created and renamed): check that the path still names
  `obj` before trusting it.

## Search (`fs::search`)

`kernel/src/fs/search/` (`mod.rs`: the index and its thread; `query.rs`: queries and
`/sys/fs/search`). A Spotlight-style index of everything under its roots (default
`/Users/milk` and `/Apps`; `/sys/fs/search/roots`): path, name, kind, size, mtime,
and the words of text files.

- **Kinds**: the extension for known ones (`md`, `txt`, `rs`, `toml`, `html`, `json`, `py`,
  `sh`, `csv`, ... are text; `png`, `wav`, `pdf`, `zip`, `img`, ...), otherwise sniffed from
  the first 512 bytes (`png`, `wav`, `app` (ELF), `pdf`, `stream` (a `snap send` stream),
  `text`, `data`); `folder`, `link`.
- **Words**: text files up to 512 KiB are read; words are lowercase alphanumeric runs of
  2+ characters, at most 4000 distinct per file. Names are split the same way (plus the
  whole name), single characters included.
- **Skipped**: anything with a component starting with `.` (so `~/.cache`, where the
  index lives, never shows up).
- **Kept up to date** by the `search-index` thread (priority `Low`, content reading
  limited to 16 MB/s) from `fs::notify` events: a create, rename or delete is in the index
  within ~0.2 s, a written file once it is closed. If an event can't be applied as is (the
  tree moved on), the thread re-verifies once the queue is empty.
- **Stored** at `~/.cache/milk/index` (on milkfs: survives reboots), written 2 s after
  things go quiet (at most every 30 s while busy), via a temp file + rename. Format:
  `"MKSX" | version u32 | roots | entries (path, kind, size, mtime, folder, words) | xxh64`.
  A `sync` (`sync(2)`, `fs::sync_all`, the milkfs `sync` control) saves it first when it
  changed since the last save (`search::flush`), so a machine turned off right after a
  sync comes back to the index it had. At boot it is loaded and **verified**: the roots
  are walked comparing size and mtime, and only changed or new files are read again;
  queries are answered from the loaded index meanwhile (state `verifying`); without a
  usable file (missing, damaged, other roots or version) the index is built from scratch
  (queries wait for it: `spot: the index isn't ready yet`). `spot --wait` waits for
  `ready`, `spot --wait-index` for any index to query.

Numbers (`scripts/searchtest.sh`, 5051 entries, `-smp 4`): built from scratch in 170 ms;
after a reboot loaded in 9 ms and verified in ~590 ms (cold caches: the walk reads every
tree node once); a query for a unique word 0.3 ms, one with 714 hits 3.2 ms.

### Queries

Words and filters, separated by spaces:

- Every word must match the file's **name** or **content**, as a whole word or the start
  of one (`proj` finds `Project Plan.md`, `milk` finds "milkshake").
- `kind:K`: `md`, `txt`, `png`, ... or a group: `text` (any text kind), `image`, `audio`,
  `folder`. Several `kind:` = any of them.
- `in:NAME`: under a folder called NAME (any level, case-insensitive); `in:/path`: under
  that path.
- `modified:today|yesterday|week|month|Nd|Nh|Nm` (N days, hours, minutes).
- Filters alone list everything that passes them, newest first.

Ranking, per word: name word 100, name prefix 60, content word 20, content prefix 10
(name and content add up), +50 if the whole name is the query, + recency (8 if modified
today, 4 this week, 2 this month); ties newest first, then by path.

### Kernel API (for the wm's Skim)

```rust
use crate::fs::search;
let hits: Vec<search::Hit> = search::query("plan kind:md", 20)?;  // limit 0 = 50
for h in hits {
    // h.path: "/Users/milk/Documents/Project Plan.md"
    // h.kind: "md" | "txt" | "folder" | "png" | "text" | "data" | ...
    // h.size (bytes), h.mtime (unix seconds), h.score (higher is better),
    // h.matched: "name" | "content" | "name+content" | "" (filters only)
}
let s: search::Status = search::status(); // s.state: "starting" | "building" |
                                           // "verifying" | "ready"; files, folders, ...
```

- Errors: `EAGAIN` while the index isn't built yet (shortly after boot), `EINVAL` for a
  malformed filter (`modified:soon`).
- Cheap enough to call per keystroke from a kernel thread: it takes the index lock
  (a `sched::Mutex`) for the length of the query, no I/O. The results are sorted.
- `search::parse(q, now)` splits a query without running it (e.g. to show filter chips).

### `/sys/fs/search` and `spot`

- `query`: write `[limit=N] QUERY`, read back `# N result(s) for "QUERY" in T us`, then one
  line per hit: `score<TAB>kind<TAB>size<TAB>mtime<TAB>matched<TAB>path` (the last query
  written, whoever wrote it).
- `status` (read): state, counts, roots, how this boot's index came about, events
  handled, saves. `roots` (read/write: space-separated paths; writing rebuilds),
  `rebuild` (write anything).
- `spot WORDS...` (like macOS `mdfind`): paths, best first; `-l` adds kind, size, date and
  where it matched; `-n N`; `--status`, `--rebuild`. Under msh structured pipes
  (`MILK_TABLE=1`) it emits a table (`path name kind size modified score matched`):
  `spot -n 100 kind:md cow | where size > 1K | sort-by modified -r`.

## Next steps

- Pipes (`InodeType::Fifo`) and `poll` on the wait queues; `O_NONBLOCK`.
- Permission checks once processes have credentials (`Inode::stat` already has
  uid/gid/perm), `umask` in the syscall layer.
- `mmap` of tmpfs files (expose the frame for a page index), `statfs`, `sync`.

## milkfs v0 (implemented)

The `milkfs` crate (`<repo>/milkfs`, `no_std` + `alloc`, see "The milkfs crate" below) with
the kernel's VFS glue in `kernel/src/fs/milkfs/`; host tool `scripts/mkmilkfs.py`, crash test
`scripts/crashtest.sh`, host tests `scripts/milkfs-test.sh`.
Mounted at `/Volumes/<label>` (`/Volumes/Milk`). Every disk with milkfs is mounted, at
`/Volumes/<label>` (or `/Volumes/<label>-<dev>` if taken); the **primary** volume (home,
Time Machine, the snapshot API, `/sys/fs/milkfs`) is the one labelled `Milk`, else the first
found (qemu's disk order isn't the command line's). Others are plain volumes, e.g. backup
targets for `snap receive`. The design it follows is the sketch below;
the differences are listed at the end of this section.

| file | what |
|---|---|
| `milkfs/src/format.rs` | on-disk structures, xxh64, FNV-1a name hash (mirrored in `mkmilkfs.py`) |
| `milkfs/src/store.rs` | allocator and its persistent space map, birth-time release rules, checksummed block I/O, dead-list chains, decoded node cache |
| `milkfs/src/bcache.rs` | the 4 KiB write-back block cache over the disk |
| `milkfs/src/btree.rs` | copy-on-write B+tree: dirty nodes in memory, written bottom-up at commit |
| `milkfs/src/volume.rs` | `State`, commit, mount (space map, or mark + namespace check + orphan reclaim), mkfs, space check |
| `milkfs/src/items.rs` | inodes, directory entries, extents, symlinks, file data, shared blocks |
| `milkfs/src/ops.rs` | create, link, unlink, rmdir, rename, write, truncate, reflink, dedupe (what the VFS objects call) |
| `milkfs/src/snap.rs` | snapshot create / delete / rollback |
| `milkfs/src/clone.rs` | clones: create / discard / promote |
| `milkfs/src/diff.rs`, `versions.rs` | `snap diff`, a file's versions |
| `milkfs/src/stream.rs` | send / receive streams and the drivers that write and apply them |
| `milkfs/src/crypt.rs`, `convert.rs` | encryption at rest, encrypting / decrypting in place |
| `milkfs/src/fsck.rs`, `scrub.rs` | `check [--repair]`, the scrub walk |
| `milkfs/src/timemachine.rs` | automatic snapshots: settings, retention, the snapshot |
| `kernel/src/fs/milkfs/inode.rs` | VFS objects: `Volume` (the `FileSystem`), `MilkInode`, `.snapshots` |
| `kernel/src/fs/milkfs/scrub.rs`, `timemachine.rs` | their threads, rate limit, status, `/sys/fs/milkfs/scrub` and `/auto` |
| `kernel/src/fs/milkfs/platform.rs` | the kernel as the crate's `Platform` |
| `kernel/src/fs/milkfs/crashtest.rs` | fw_cfg-selected crash workloads and checker (inert on a normal boot) |
| `kernel/src/fs/milkfs/mod.rs` | kernel API, automount, unlock / lock, `/sys/fs/milkfs` |

### The milkfs crate

The file system builds for the kernel and for the Mac, so most of it is tested on the Mac in
seconds (`scripts/milkfs-test.sh`) instead of in qemu. The kernel depends on it like it does
on `milkcrypto` and re-exports its modules under the old paths (`fs::milkfs::format`,
`fs::milkfs::volume::State`, ...); `fs::errno`, the file-type ABI in `fs::node` (`InodeType`,
`S_IF*`, `DT_*`, `Timespec`), `fs::blockdev::BlockDevice` and `fs::bcache` are the crate's
too. What the crate gets from its host:

- a disk: `blockdev::BlockDevice` (the kernel's drivers; on the Mac, memory or a file);
- `platform::Platform`, installed once (`set_platform`; the kernel's in
  `kernel/src/fs/milkfs/platform.rs`, first thing in `fs::init`): wall-clock time,
  monotonic time, a counter (UUID seed), random bytes, scrypt (the kernel's in page
  frames), a log line, the reason for an error (`/sys/fs/milkfs/error`), and locks
  (`RawLock`: a sleeping one, the kernel's `sched::Mutex`, for the block cache, which is
  held across disk I/O; a spinning one, its IRQ-masking `SpinLock`, for the node cache);
- `volume::StateAccess`: how the drivers that must not hold the volume lock across slow
  work (scrub, send, receive) take it, a step at a time (the kernel's `Volume`);
- `scrub::ScrubRunner` (stop, progress, rate limit) and `stream::StreamSink` (the output).

`LAST_COMMIT` (the last generation whose commit completed, for the crash test's power-cut
report) is the crate's. The kernel keeps the VFS (`inode.rs`, the page cache), the
threads, `/sys/fs/milkfs`, unlock and lock, the audit log, and the crash-test modes.

### On-disk format (v0, v1, v2)

All integers little endian, 4 KiB blocks. The current kernel writes **v1** = v0 plus two
item kinds: multi-block runs (kind 5) and inline data (kind 6), and **v2** = v1 plus shared
blocks (kind 8, reflink copies) on a volume's first reflink: a kernel without reflinks
would free a shared block while another file still uses it, so v1 kernels refuse a v2
volume ("no valid superblock"). It mounts v0, v1 and v2. A v0
volume mounts with the message `milkfs: format v0 volume: it is upgraded to v1 (...) by its
first write; kernels older than that won't mount it then`: the first commit writes version 1
into the superblock (v0 kernels refuse it: the version doesn't match). Old one-block extents
(kind 3) stay readable forever and are rewritten as runs only when that part of the file is
written. `mkmilkfs.py` reads both; `mkmilkfs.py mkfs --v0` and `put --v0` make v0 images
(the upgrade test, `scripts/upgradetest.sh`, uses them).

```text
block 0, 1   superblock slots: generation g is written to slot g % 2; mount takes the valid
             (magic + checksum + version) slot with the higher generation
block 2..    tree nodes, data blocks, dead-list blocks, anywhere; never overwritten while the
             last committed superblock can reach them
```

**Block pointer** (24 B): `block u64 | birth u64 (generation it was written in) | xxh64 u64`
of the 4 KiB block. `block == 0` = none. Checksums live in pointers (Merkle style), so every
tree node, data block and dead-list block is verified when read; the superblock checksums
itself.

**Superblock** (one block):

| off | field |
|---|---|
| 0 | magic `MILKFS01` |
| 8 | version u32 (0 or 1), 12: block size u32 = 4096 |
| 16 | total blocks u64, 24: generation u64 |
| 32 | live tree root: block pointer |
| 56 | live dead list head: block pointer |
| 80 | next inode number u64, 88: created (unix s) u64 |
| 96 | uuid [16], 112: label [32] (NUL padded) |
| 144 | snapshot count u32 (max 30) |
| 152 | 30 records x 128 B (snapshots and clones): name [48], generation u64 (clone: its origin's), created u64, root pointer, dead list pointer, next_ino u64, flags u32 (1 = clone), id u32 (clones) |
| 3992 | automatic snapshots: state u32 (0 default/1 on/2 off), interval s u32, keep u32, daily u32 (0 = default) |
| 4008 | flags u32: 1 = `SB_BRANCHED` (a clone was promoted; see below) |
| 4016 | space map index: block pointer (zero = none; kernels before the map write zeros here) |
| 4040 | last full scrub u64 (unix s, 0 = never), 4048: scrub interval u32 (s; 0 = default 7 days, 2^32-1 = off) |
| 4088 | xxh64 of bytes 0..4088 |

**Tree node**: header `"MKND" | level u8 (0 = leaf) | pad u8 | count u16 | birth u64`, then
leaf items `key (17 B) | value length u16 | value` or internal entries
`key (17 B) | child pointer (24 B)` (up to 99 per node). A key is
`(object id u64, kind u8, offset u64)`, sorted in that order; child `i` of an internal node
covers keys from its separator to the next one. Values are at most 1500 bytes.

Tree shape (in memory, nothing on disk changes): a leaf that overflows is split evenly,
except when the item that overflowed it starts an object newer than everything else in
the leaf (inode numbers only grow, so that is file creation): then it splits at that
object boundary and the left leaf stays full. On delete, a node left under a quarter full
is merged with a neighbour if the two fit in one block, otherwise the two share their
entries evenly; a root with one child is replaced by it. `scripts/treetest.sh [N]`
(`/sys/fs/milkfs/treetest`: `DIR N` creates N empty files in DIR, deletes 99 of every 100,
then the rest, printing the live tree's shape after each step), 50 000 files:

| | before | even splits only + merge on delete | + object-boundary splits |
|---|---|---|---|
| 50 000 files | 3 levels, 2925 nodes, 52% full | same | 3 levels, 1818 nodes, 83% full |
| 500 left (99% deleted) | 3 levels, 995 nodes, 2% full, 983 underfull | 2 levels, 51 nodes, 31% full, 0 underfull | 2 levels, 52 nodes, 30% full, 0 underfull |
| all deleted | 1 leaf | 1 leaf | 1 leaf |
| create / delete time | 671 / 1164 ms | 660 / 950 ms | 672 / 800 ms |

The self-test runs 6000 random inserts, replaces and deletes against a `BTreeMap` model
(commits in between, then 95% deleted: no underfull node, contents equal, space back to
where it was) and the same create/delete test with 3000 files.

| kind | key | value |
|---|---|---|
| 1 inode | `(ino, 1, 0)` | 68 B: mode u32 (`S_IF*` + perm), nlink u32, uid u32, gid u32, size u64, parent u64 (directories), atime/mtime/ctime as (i64 s, u32 ns) |
| 2 dirent | `(dir ino, 2, fnv1a64(name))` | every name in that dir with that hash: `ino u64 | d_type u8 | len u8 | name` repeated |
| 3 extent | `(ino, 3, block index)` | block pointer of that 4 KiB of the file (absent = hole) |
| 4 symlink | `(ino, 4, 0)` | target bytes (max 1024) |
| 5 run (v1) | `(ino, 5, first block index)` | `block u64 | birth u64 | count u32 | count x xxh64 u64`: `count` (1..128) consecutive file blocks stored in consecutive disk blocks from `block`, one checksum per block |
| 6 inline (v1) | `(ino, 6, 0)` | the whole file content, files up to 1024 bytes (no data block at all) |
| 8 shared blocks (v2) | `(2^64-1, 8, first disk block)` | `len u32 | extra u32`: the `len` (at most 128) disk blocks from there are referenced `1 + extra` times by this tree; blocks without an entry once. Entries never overlap. |
| 7 orphan | `(2^64-1, 7, ino)` | empty: `ino` has no links left but is still open; whoever deletes it removes the entry, mount finishes the delete if the machine went down first |

The root directory is inode 1 (its parent is itself). Inode numbers only grow.

**Dead-list block**: `"MKDL" | count u32 | next pointer (24 B) | count x (block u64, birth u64)`
(254 per block). A chain is only ever prepended to or rewritten whole.

**Space map**: an index block `"MKSM" | pad u32 | generation u64 | total blocks u64 |
used u64 | chunk count u32 | pad u32 | chunk count x block pointer`, and the chunks: plain
bitmaps, bit `b` of chunk `i` = block `i * 32768 + b` is in use. One index holds 169 chunks
(volumes up to 22 GiB; bigger ones simply have no map and mount by scanning). Neither item
kind 7 nor the map needs a format bump: kernels that predate them ignore kind 7 and write
zeros at 4016, which drops a map they would have made stale.

### Transactions and crash consistency

Mutating VFS calls (write, create, mkdir, unlink, rmdir, rename, link, truncate, setattr)
join the volume's open transaction. It is committed when it is **1 s old** (checked at the
end of each operation and by the `milkfs-flush` thread every 250 ms), after **512
operations**, when the bcache is 3/4 dirty, on `fsync`/`fdatasync`/`sync` (syscalls 82/83/81,
`OpenFile::fsync`, `fs::sync_all`), and before any snapshot or clone operation. So a crash
loses at most about a second of work, and always lands on a state between whole
operations. 200 x 1 KiB files: 155 ms / 601 commits before batching, 1-8 ms / 2 commits
after. A commit:

1. The B+tree path to every changed item was copied into memory (old blocks released by the
   rules below); new data blocks went to freshly allocated blocks through the bcache.
2. Commit writes the dirty nodes bottom-up into new blocks, prepends this transaction's
   dead-list entries as a new chain block, writes the space map (last, so it sees every
   allocation), `bcache.flush()` (everything durable),
3. writes the new superblock to slot `generation % 2` (**the commit point**), flushes again.

A crash at any moment leaves the previous superblock valid and everything it references
untouched; a torn superblock fails its checksum and the other slot wins. Blocks the commit
freed only become reusable after it is durable.

**Branched history.** Dead lists rely on history being linear (a block that leaves the
live tree never comes back). Promoting a clone breaks that, so it sets `SB_BRANCHED`:
dead lists are dropped and snapshot delete, rollback and promote reclaim space with an
exact **sweep** (free every allocated block the committed state can't reach, the same walk
mount does). When no snapshots are left the history is linear again and the flag clears.

**Free space.** While a transaction is open a dropped block is: born in this transaction →
reused at once; born after the newest snapshot → freed when the commit is durable;
otherwise a snapshot still has it → live dead list.

Every commit writes the **space map** of the generation it commits, into fresh blocks, as
part of the commit: the allocator's bitmap minus what the commit frees (`pending_free`,
the previous map among it) plus the map's own blocks. Because it is written before the
superblock and the superblock points at it, it can't be out of date with the generation
that wins at mount, crash or not. Mount loads it when it is intact: index and chunk
checksums, generation stamp = the superblock's, size matches, bit count = the stored
count, and the blocks the superblock points at (roots, dead-list heads, the map itself)
marked used. Then it finishes the deletes on the orphan list and is done:

```text
milkfs: vda mounted at /Volumes/Milk in 0.37 ms: generation 29, space map current, ...
```

Otherwise (no map: made by `mkmilkfs.py`, last written by the Mac tool or an older kernel;
damaged; stale) it says why and **rebuilds** it the old way: mark every block reachable
from the superblock (live tree, every snapshot tree, shared subtrees once, extents,
dead-list chains, the rest is free), check the live namespace (every entry points at an
inode, directory parents and link counts match) and reclaim orphans (inodes with no entry
and no links). The next commit writes a fresh map.

```text
milkfs: vda: space map absent (last written by the Mac tool or an older kernel): rebuilding it from the trees
milkfs: vda mounted at /Volumes/Milk in 100.70 ms: generation 2, 20202 inodes, 3578/65536 blocks used, 0 snapshot(s), check clean
```

So a mapped mount does no namespace check: that is what `snap check` is for. On a branched
volume the map also counts blocks waiting for the next sweep (as the in-memory map does);
before the map, a reboot reclaimed those, now the next sweep (snapshot delete) does.

Mount time, `scripts/mounttime.sh N` (N files made on the Mac, 100 per directory, every
50th 8 KiB, the rest inline; virtio, `-smp 4`):

| files | scan (no map: before, or after a Mac-side write) | with the space map |
|---|---|---|
| 20 000 (3 578 blocks) | 100-113 ms | 0.3-0.5 ms |
| 100 000 (18 604 blocks) | 1 065-1 163 ms | 0.3-0.5 ms |

### Snapshots

- **Create** (`fs::milkfs::snapshot(name)`): commit, then record `{name, generation, root,
  dead list}` in the superblock and start the live head with an empty dead list; one more
  commit. O(1), no data copied. Names: `[A-Za-z0-9._:-]`, max 47, max 30 snapshots.
- **Browse**: `/Volumes/Milk/.snapshots/<name>/` is the snapshot's tree, read-only (EROFS),
  with its own device number. `.snapshots` is a virtual directory in the volume root.
- **Delete**: the dead-list algorithm (ZFS): entries of the next dead list (next snapshot's,
  or the head's) born after the previous snapshot were only visible to the deleted one and
  are freed; the rest, plus the deleted snapshot's own list, become the next one's list.
- **Rollback** (`rollback(name)`): deletes newer snapshots (like `zfs rollback -r`), frees
  every live block born after the snapshot (subtrees born before it are skipped, CoW makes
  them entirely old), drops the head's dead list (those blocks are live again), sets the
  live root to the snapshot's. The snapshot is kept. Inode objects hold only `(view, ino)`
  and inode numbers are never reused, so open files see ENOENT instead of stale data.

### Time Machine (automatic snapshots)

The `timemachine` kernel thread (started by `fs::start_threads()` after `proc::start_init`)
snapshots the primary volume at boot and then every interval, as `auto-YYYY-MM-DDTHH:MM:SS`
(RTC time), unless nothing changed since the newest snapshot (same committed root). Retention
applies to `auto-` snapshots only: the newest `keep`, plus the newest of each day for the last
`daily` days; manual snapshots and clone origins are never deleted; if the 30-record table
is full the oldest auto snapshot makes room. Defaults: on, 15 min, keep 8, daily 14. Settings
live in the superblock, so they persist: `/sys/fs/milkfs/auto` reads e.g. `on interval=15m
keep=8 daily=14` / `next in 12m`; write `on`, `off`, `interval=30s|15m|2h`, `keep=K`,
`daily=D`, `now` (`snap auto ...`). A snapshot is a root pointer plus one superblock commit.

### Clones (try before install)

A clone is a writable copy of a snapshot (O(1)), mounted as its own file system at
`/Volumes/<label>-<name>` (and remounted at boot). It shares the origin's blocks until it
writes; inside a clone, a dropped block born after the origin is the clone's own and is
freed, an older one stays with the origin. The origin snapshot can't be deleted while it
has clones (EBUSY); clones can't be snapshotted (v0).

- `clone(snap, name) -> path`: make and mount it. Install into it, try it.
- `discard_clone(name)`: unmount, free what the clone wrote.
- `promote_clone(name) -> "before-<name>"`: the clone becomes the live volume; the live state
  it replaces is kept as snapshot `before-<name>`, so `rollback before-<name>` undoes the
  install. Sets `SB_BRANCHED` (above).

The intended UI flow: snapshot (or take the newest auto snapshot) → clone → installer
writes into `/Volumes/Milk-<name>` → user tries the app (run with that root once proc has
per-process roots; today, from that path) → Keep = promote, Cancel = discard.

### Diff (`snap diff`)

`fs::milkfs::diff(a, b: Option<&str>) -> DiffResult { changes: Vec<Change>, nodes_read,
subtrees_skipped }`, `Change::{Added, Removed, Modified, Renamed(old, new)}` with paths from
the volume root. `b = None` or `"live"` = the live volume; the order of `a` and `b` doesn't
matter. Shell: write `A [B]` to `/sys/fs/milkfs/diff`, read the result back;
`snap diff A [B|live]` prints e.g.

```
diff before..live: 5 change(s) (2 tree nodes read, 0 unchanged subtrees skipped)
A /Users/milk/Documents/Plans 2026
D /Users/milk/Documents/todo.txt
M /Users/milk/README.txt
R /dd/a -> /dd/sub/a2
```

Only changed nodes are read: walking the newer tree, a subtree whose pointer was born at or
before the older snapshot is skipped (CoW: it's the same subtree in both), and those
pointers are skipped in the older tree too; the items of the remaining leaves are compared.
After a promote (branched history) shared subtrees are found by block-pointer equality
against the older tree instead. One change among 300 files reads at most a handful of
nodes (self-test checks <= 8). Files now record their parent directory (as directories
always did), so paths are found without scanning; older files fall back to a scan.

### Versions ("Browse All Versions")

For a Versions panel (Notes, Files): the history of one file across the volume's snapshots.

```rust
// crate::fs::milkfs
pub struct Version {
    pub snapshot: Option<String>, // the (oldest) snapshot holding this content; None = current file
    pub taken: u64,                // when that snapshot was taken (unix s); current: now
    pub mtime: i64,                // the file's own mtime in that version
    pub size: u64,
    pub hash: u64,                 // content hash: equal = identical contents
}
pub fn versions(path: &str) -> Result<Vec<Version>>;            // oldest first, current last
pub fn restore_version(path: &str, snapshot: &str) -> Result<()>;
```

- `path` is any VFS path to a regular file on the milkfs volume (`/Users/milk/...`,
  `/Volumes/Milk/...`, `/Apps/...`); elsewhere EXDEV, a directory EISDIR.
- Distinct versions only: consecutive snapshots with the same content collapse into the
  oldest one; if the current content equals the newest snapshot's, that entry is shown as
  the current one. Hashes come from the extent checksums (no file data is read), so listing
  is cheap even for big files.
- The file is followed by inode number, so its history survives renames; where that inode
  doesn't exist in a snapshot, the current path is tried (for editors that save via
  write-temp + rename).
- `restore_version` writes the snapshot's content into the live file (same inode, so open
  editors see it on their next read). If the current content isn't in any snapshot, it first
  takes snapshot `pre-restore-<time>`, so a restore never loses anything (and that version
  shows up in the list). Fails with ENOSPC if the 30-record snapshot table is full.
- To show a version's content, read `<volume>/.snapshots/<snapshot><path in volume>`, e.g.
  `/Volumes/Milk/.snapshots/monday/Users/milk/Documents/ideas.md` (read-only).
- Shell: write a path to `/sys/fs/milkfs/versions` and read back tab-separated lines
  `snapshot|current, taken, mtime, size, hash` (after a `# path` header); write
  `SNAPSHOT PATH` to `/sys/fs/milkfs/restore`. Tool: `versions FILE`,
  `versions restore FILE SNAPSHOT`:

```
/Users/milk/Documents/ideas.md: 4 version(s)
   1  2026-10-21 05:04      12 B  109ff1d8  in snapshot monday
   2  2026-10-21 05:04      21 B  cf46a6a6  in snapshot tuesday
   3  2026-10-21 05:04       6 B  a3f8b57b  in snapshot pre-restore-2026-10-21T05:04:10
   *  2026-10-21 05:04      12 B  109ff1d8  (current)
```

### Check and repair (fsck)

Never automatic: mount only finishes deferred deletes (inodes with no links left, i.e.
files unlinked while open when the machine went down, like Linux's orphan list) and
*reports* anything else (`check FOUND PROBLEMS`). Repairs happen only on request:

- kernel: `fs::milkfs::check(repair) -> CheckReport`, `/sys/fs/milkfs/check` (write `check`
  or `repair`, read the report), `snap check [--repair]`;
- host: `scripts/mkmilkfs.py check IMAGE [--repair]` (exit 0 clean/repaired, 1 problems,
  2 damage that can't be repaired).

| damage | check finds | repair |
|---|---|---|
| a superblock slot is garbage | yes, as a **note** (not a problem: a commit torn mid-superblock-write leaves exactly this, so crashed images still check clean) | rewritten from the good one (the next commit lands there; in the kernel any commit heals it) |
| entry pointing at a missing inode | yes | entry removed |
| inode with links but no entry | yes | linked into `/lost+found` as `#<ino>` |
| inode with no links and no entry | yes | deleted |
| wrong link count / directory parent | yes | fixed |
| allocated but unreachable blocks | yes (a note on a branched volume: they wait for the next sweep) | swept, and a corrected space map committed |
| reachable blocks marked free | yes | marked used again (before anything else allocates) |
| space map unreadable or wrong (host check compares it with the reachable blocks) | yes | kernel: mount already rebuilt it; host: dropped (the next mount rebuilds it) |
| orphan-list entry for an inode that is gone or has links | yes | entry dropped |
| shared-block counts that don't match the references (too low = deleting one copy would free a block another uses) | yes (not at mount: `check` counts every reference) | table rebuilt from the references |
| a snapshot taken while the live table was wrong (it keeps the wrong counts) | host check: a note | none (snapshots are read-only); harmless unless rolled back to: then `check --repair` |
| data block fails its checksum | yes, with the file's path | **no**: recover with `versions restore` / `snap rollback` |
| tree node fails its checksum | yes (mount also refuses, EIO) | **no**: the image is left untouched |

The host repair rewrites the live tree from the fixed item set into free blocks and commits a
new superblock (snapshots untouched; if there are any, the volume switches to exact sweeps
since the dead lists no longer describe the new tree). `mkmilkfs.py corrupt IMAGE
{orphan,dangling,nlink,parent,sb,smap,bref,data,node}` damages a test image on purpose, and
`scripts/fscktest.sh` runs every kind through both repairs (base image = a few seconds of
the crash workload; after a repair the kernel's crash-check verifies every file).

### Reflink copies and dedupe

A file can share data blocks with another (copy-on-write): `files copy --reflink SRC DST` (or
`--reflink=auto`: copy if sharing isn't possible), the Linux `FICLONE` ioctl
(`ioctl(dst_fd, 0x40049409, src_fd)`, `rt::sys::ficlone(src, dst)`, kernel
`fs::ops::ficlone(&src, &dst)` / `fs::milkfs::inode::reflink`), and `dedupe`.

- The copy gets the source's extent items with the same block pointers, and each shared
  run gets a **shared-block entry** (kind 8) counting its extra references. Writing either
  file is ordinary copy-on-write: the touched blocks get new ones, the old run is trimmed,
  and dropping a reference to a shared block only lowers its count. Only the last
  reference goes through the normal release rules (birth time, dead lists), which are
  about which *trees* (live, snapshots) still have a block; the counts are per tree, so
  snapshots and clones carry their own. Space accounting stays exact through snapshots,
  deletes, rollbacks, clones and promotes (self-test and crash test).
- Same view of the same volume only: EXDEV otherwise (other file system, other volume,
  a snapshot or clone); EISDIR for directories, EINVAL for other non-files or a file onto
  itself; EBADF unless the source is open for reading and the destination for writing.
  Inline files (up to 1 KiB) are simply copied. The destination's times are updated.
- 2 MB with `files copy --reflink`: a handful of metadata blocks instead of 489 data blocks
  (`scripts/reflinktest.sh`).

**dedupe** (`dedupe [-n] DIR`, `/sys/fs/milkfs/dedupe` (`DIR` or `DIR dry`),
`fs::milkfs::dedupe(dir, dry_run)`): every regular file with data blocks under `DIR` is
grouped by size and content hash (from the block checksums, no data read); files in a
group are compared byte by byte with the group's first file and, if equal, made to share
its blocks. File times stay as they were. Blocks a snapshot still has stay used until
that snapshot is deleted (the report says how many were freed now):

```text
/Volumes/Milk/d: 4 file(s) with data, 1 group(s) of identical files, 2 now share, 1 already shared, 0 same hash but different
978 block(s) (3912 KiB) shared, 980 block(s) freed now
```

### Encryption at rest

A milkfs volume can be encrypted ("FileVault"): every block except the two superblocks
and the keyring is encrypted and authenticated; without the passphrase (or the recovery
key) the volume doesn't mount. Crypto: the shared `milkcrypto` crate (`crypto/`: XChaCha20-Poly1305,
HMAC-SHA256, scrypt; from scratch, published test
vectors at boot; see docs/crypto.md); milkfs part: `milkfs/src/crypt.rs`.

**Keys.** A random 256-bit volume key VK (made on the Mac with `os.urandom` at `mkfs
--encrypt`). Never stored in the clear: the keyring block (plaintext, pointed at by the
superblock) holds key slots, each VK wrapped with XChaCha20-Poly1305 (random nonce,
aad = "milkfs slot" | uuid | kind):

- passphrase slots: KEK = scrypt(passphrase, salt, N = 2^16, r = 8, p = 4): 64 MiB, about
  0.35 s per try here (parameters are per slot, so they can grow later);
- the recovery slot: KEK = HMAC-SHA256(salt, recovery key); the recovery key is 128 random
  bits shown once at creation as 8 groups of 4 hex digits.

Changing a passphrase rewraps VK into a new slot (a new keyring block, one commit): no
data block is rewritten. Salts and nonces made in the kernel come from the CSPRNG
(`crypto::rng::fill`). VK gives `enc = HMAC(VK, "milkfs v1 enc")` and
`mac = HMAC(VK, "milkfs v1 mac")`.

**Blocks: XChaCha20 with a synthetic IV, checksums over the plaintext.**
`tag = HMAC-SHA256(mac, "MKB1" | block | birth | plaintext)[..8]` is stored where the
checksum always was (in the pointer to the block, or per block in an extent), and the
block is `plaintext XOR XChaCha20(enc, nonce = tag | block | birth)`. Reading rebuilds
the nonce from the pointer, decrypts, and checks the tag in constant time. Why this and
not XChaCha20-Poly1305 per block: a block is exactly 4 KiB and a pointer has 8 spare
bytes, so there is nowhere to keep a 24-byte nonce and a 16-byte tag; and copy-on-write
can rewrite the same (block, birth) within one transaction, so a counter or random nonce
couldn't be reconstructed. With SIV the nonce depends on the content: a repeated nonce
means the identical plaintext and ciphertext again (it leaks only "this block was
written twice with the same bytes"), never a reused keystream, under CoW, snapshots and
clones alike. Block number and birth are inside the tag, so blocks can't be swapped or
replayed; a forgery succeeds with probability 2^-64 per try. The price: verifying any
block (check, scrub) needs the key; dedupe and versions hash file contents instead of
checksums on encrypted volumes (the tags differ for equal data in different places).

**Superblock** (plaintext, readable before unlocking: label, generation, sizes, snapshot
names and times are visible) carries `SB_ENCRYPTED` (flag 2), the keyring pointer (offset
4052), and `sb_mac` (offset 4076) = HMAC-SHA256(mac, "MKSB" | bytes 0..4076) over the
bytes on disk, checked after unlocking: an altered superblock (an old root, a different
keyring) refuses to mount. Encrypted volumes are format v3: older kernels refuse them.

**Unlocking.** At boot an encrypted volume is found but stays unmounted, "locked":

```text
milkfs: vda (Milk) is encrypted and locked: `snap unlock` (or write the passphrase to /sys/fs/milkfs/unlock)
```

- `snap unlock [DEV]` asks for the passphrase (or the recovery key) without echoing it,
  three tries; `snap passwd` changes the passphrase (asks for the current one or the
  recovery key, then the new one twice).
- `/sys/fs/milkfs/unlock`: read: the locked volumes (`vda Milk locked`); write
  `[dev=NAME ]SECRET`. `/sys/fs/milkfs/passwd`: write `OLD\nNEW`. Wrong: EACCES.
- Kernel: `fs::milkfs::unlock(secret, dev) -> Result<Vec<String>>` (mount paths),
  `fs::milkfs::change_passphrase(old, new)`, `fs::milkfs::locked()`. If the volume is the
  primary one (labelled Milk), unlocking it binds `/Users/milk` and `/Apps` onto
  it, mounts its clones and tells the search indexer to rescan, as boot would have; until
  then home is the RAM copy, and no other disk takes the primary role.
- For proc's login: after a successful login, `fs::milkfs::unlock(password, None)` opens
  a volume whose passphrase is the login password (ENODEV if nothing is locked, EACCES if
  it isn't that password); a password change can call `change_passphrase`.
- Test only: `-fw_cfg name=opt/milkos/passphrase,string=...` unlocks at boot (anyone who
  can start qemu sees it; for unattended test boots).

**Making one in milkOS** (no Mac needed): `snap mkfs DEV [LABEL] [--force] --encrypt
[--recovery-out FILE | --no-recovery] [--passphrase-file F]` asks for the passphrase twice
(not echoed) and shows the recovery key once (`snap: recovery key: XXXX-...`), or writes
it to FILE. Underneath: `/sys/fs/milkfs/mkfs` takes `DEV [LABEL] [force] encrypt
[recovery=FILE]` and then, on a second line of the same write, the passphrase (8
characters or more; wiped after use, never logged or audited); kernel
`fs::milkfs::mkfs_encrypted(dev, label, force, passphrase, recovery_out)`. A random
volume key (CSPRNG) goes into a passphrase slot (scrypt 2^16/8/4) and, with `recovery=`,
a recovery slot whose 128-bit key is written to FILE (created with O_EXCL, mode 0600,
owned like its folder; snap's temporary one in the home folder is read and removed). The
layout is what `mkmilkfs.py mkfs --encrypt` makes (root leaf sealed, keyring in block 3,
format v3, `sb_mac`). The new volume is left locked (listed by `/sys/fs/milkfs/unlock`):
`snap unlock DEV` mounts it. `mkfs` refuses a mounted device (EBUSY, any mounted volume,
not just the primary one).

**Labels** (`fs::milkfs::label_ok`, the same rule in `mkmilkfs.py mkfs`): 1 to 31 bytes of
UTF-8 (a multi-byte character counts as several), no `/`, no control characters, not
starting with `.`; spaces and other punctuation are fine. The label names the mount
point, `/Volumes/<label>` (`/Volumes/<label>-<device>` if that is taken; a disk whose
label breaks the rule, made by an older tool, mounts as `Milk`). **Erasing**: `mkfs` of a
disk that holds milkfs needs `force` (EEXIST otherwise), and works on any disk that isn't
mounted, a locked encrypted one included; a mounted one (the home volume always is) is
refused with EBUSY.

`/sys/fs/milkfs/disks` lists every block device, for choosing what to format:

```text
vda 25165824 rw milkfs encrypted mounted=/Volumes/Fresh Fresh
vdb 67108864 rw milkfs plain mounted=/Volumes/Milk Milk
vdc 67108864 rw other
```

(`NAME BYTES rw|ro milkfs plain|encrypted mounted=PATH|locked|unmounted LABEL`, the label
last since it may contain spaces, or `NAME BYTES rw|ro other` for anything that isn't
milkfs: FAT, blank, a scratch disk.) `snap disks` prints it.

**State in `/sys/fs/milkfs/info`** (the primary volume; Disk Utility's diskkit parses it):

```text
encryption:  on
locked:      no
cipher:      xchacha20-siv (per block; tag HMAC-SHA256/64)
key wrap:    xchacha20-poly1305
kdf:         scrypt N=2^16 r=8 p=4
key slots:   1 passphrase, 1 recovery
```

`kdf` is the first passphrase slot's parameters (each slot has its own). A plain volume
has just `encryption:  off`. `info` only describes a mounted volume, so `locked:` is
always `no` there; locked volumes (not mounted, no `info`) are listed by
`/sys/fs/milkfs/unlock` (`vda Milk locked`) and `/sys/fs/milkfs/lock`.

**Encrypting in place** (and back). `snap encrypt [DEV] [--recovery-out FILE |
--no-recovery] [--rate MB] [--wait]` encrypts a plain volume (default the primary one) in
the background, while it is in use; `snap decrypt [DEV] [--wait]` (asks for the
passphrase or the recovery key) makes an encrypted one plain again. Underneath:
`/sys/fs/milkfs/encrypt` (`[dev=NAME] [recovery=FILE] [rate=MB]`, the new passphrase on
a second line; `rate=MB` alone changes the speed, 0 = unlimited) and
`/sys/fs/milkfs/decrypt` (`[dev=NAME]`, the secret on a second line); kernel
`fs::milkfs::encrypt_in_place(dev, passphrase, recovery_out)`,
`decrypt_in_place(dev, secret)`; `milkfs/src/convert.rs`.

How (crash-safe because it is copy-on-write all the way):

- From the start the superblock has `SB_CONVERTING` (flag 4; `SB_TO_PLAIN`, 8, when
  decrypting) and the keyring: the volume needs its key to mount. A block may be in
  either form; its pointer's checksum says which (the plaintext xxh64, or the SIV tag),
  and reads try the xxh64 first (a sealed block matching its tag as an xxh64: 2^-64).
  Every new block is written in the target form.
- Each tree (live, then snapshots oldest first, then clones) is walked a leaf at a time
  from a cursor. A leaf in the old form is rewritten (copy-on-write: it and its path to
  the root are written anew at the next commit); every data block it points to in the
  old form is copied to a new block in the new form, keeping its birth (so the
  birth-based release rules keep meaning the same). Each batch (64 blocks) ends in an
  ordinary commit that also records the cursor (item `K_CONVERT`): a crash or power cut
  leaves a committed generation, consistent, partly converted, and the work resumes from
  the cursor. Nothing is ever overwritten in place.
- Data shared between trees (snapshots, clones, reflink copies) stays shared: a copied
  block goes into the remap table (items `K_REMAP`: old block and checksum -> new block
  and checksum, committed with the batch), and every other tree pointing at the old block
  is pointed at the same copy. Tree nodes are not shared afterwards (each snapshot gets
  its own copies of its nodes: a few percent of its size). Shared-block counts move with
  the blocks; a snapshot's are recounted when it is done. The volume is in branched mode
  for the duration (no dead lists); a reachability sweep after each tree, and whenever
  free space runs low, reclaims old blocks no tree needs. Needs some free space (about
  two batches plus a tenth of the volume when a sweep can't help); without it the
  conversion pauses ("paused: needs N more free KiB") and goes on by itself once there
  is room.
- Then a pass checks that every block of every tree is in the new form (another pass if
  not), then (encrypting) every free block is overwritten with zeros, so no plaintext of
  old data is left in free space; then the superblock flips (`SB_ENCRYPTED` set or
  cleared, the keyring freed when decrypting) and the remap table and `K_CONVERT` go.

**Resuming is automatic**: a volume found converting at mount (boot, or `unlock` for an
encrypted one) carries on in the background (thread `milkfs-convert`, low priority) from
its cursor; there is nothing to press. **While converting**: reads and writes work,
snapshots can be taken and deleted, `send` works (from an encrypted-or-converting volume
the stream is encrypted by default), Time Machine keeps going. Refused with EBUSY (they
could add a reference to an old-form block, or would race with it): reflink copies
(`files copy --reflink`, FICLONE), `dedupe`, `rollback`, `clone`, `promote`, `receive`, `lock`,
`passwd`, `scrub` (and a scrub running refuses the start), a second `encrypt`/`decrypt`.

`info` while converting:

```text
encryption:  converting
conversion:  encrypt 24% (385/1486 blocks), live tree
```

(`encrypt|decrypt PCT% (DONE/TOTAL blocks), PHASE[, paused: REASON]`, PHASE `live tree`,
`snapshot NAME`, `clone NAME`, `verifying` or `clearing free space`; TOTAL is the blocks
in use at the start, DONE can pass it, PCT stays below 100 until it is done.)
`/sys/fs/milkfs/encrypt` reads `vda (Milk) at /Volumes/Milk: converting: encrypt 24% ...`
per converting volume, then the last result (`... encrypted in place, done`).

Tests: `scripts/converttest.sh` (a volume filled by the crash writer: files, reflink and
deduped copies, snapshots, a promoted clone; SIGKILL and power-cut rounds at random
points of encrypting and of decrypting; after each crash the Mac's check is clean, every
tree is exactly as before, the crash-check boot passes, and a resumed copy finishes:
encrypted with no plaintext left on the disk, or plain and readable without a
passphrase) and enctest boots 5-6 (the home volume encrypted while in use, what is
refused meanwhile, locked at the next boot, decrypted back).

**Locking again.** `snap lock [DEV|PATH|LABEL]` (no argument: every encrypted volume that
can be locked), `/sys/fs/milkfs/lock` (write `vdb`, `/Volumes/Vault`, `Vault` or `all`),
kernel `fs::milkfs::lock(Some(target) | None) -> Result<Vec<String>>` (devices locked):

1. refuse if a send, receive or scrub is using the volume (EBUSY);
2. commit the open transaction (an I/O error: that errno, nothing unmounted);
3. unmount the live tree, so nothing new can be opened by path;
4. look for anything still holding a file on it (any view: live, snapshots): every
   process's open files, working directory and root directory are scanned, and the
   volume's inode table tells about the rest (mappings, kernel threads). If anything is
   found the volume is mounted back where it was and the lock is refused with EBUSY; the
   reason names the processes:
   `vdb (Vault) is in use: pid 16 (sleep): 1 open file; pid 10 (msh): working directory`;
5. drop the key and the decoded tree nodes and mark the in-memory state unwritable (a
   block written without the key would land in plaintext), invalidate the page cache for
   every view's device number (no plaintext pages left), forget the volume and put the
   device back on the locked list, where `unlock` finds it.

`VolumeKey` wipes itself when its last holder drops it; the log line says whether that
happened at the lock (`... locked, /Volumes/Vault unmounted, key wiped`) or will when a
last reader lets go (`key wiped once its last reader finishes`). Freed heap memory is
not zeroed beyond the key itself.

**The home volume can't be locked.** `/Users/milk`, `/Apps` and `/System/Settings/milk/drop`
are bound from the primary volume, and the logged-in session, the desktop and the
indexer live on it, so locking it is refused (EBUSY: "you can't lock the volume you're
logged into"); it locks at shutdown (the key is only ever in RAM). A secondary encrypted
volume (a backup disk, a vault) locks and unlocks any number of times.
`/sys/fs/milkfs/lock` reads as the encrypted volumes and their state:

```text
vdc Milk unlocked at=/Volumes/Milk (home: can't be locked)
vdb Vault unlocked at=/Volumes/Vault
vdd Old locked
```

**Send/receive**: streams from an encrypted volume are **encrypted by default** under a
passphrase of their own (`snap send` asks for it; `--plaintext` opts out); see "Encrypted
streams" under Send and receive. Inside, a stream carries plaintext and xxh64 of
plaintext blocks (never the volume's keyed tags), so any milkfs (or the Mac tool) with
the stream passphrase can check and receive it, and an encrypted target re-encrypts
under its own key.

**Mac**: `mkmilkfs.py mkfs IMAGE --encrypt [--recovery-out FILE]` (asks for a passphrase
twice, prints the recovery key; or `--passphrase-file`), `mkmilkfs.py change-passphrase
IMAGE`, and every other command opens an encrypted image with `--passphrase-file FILE`,
`--recovery KEY`, `$MILKFS_PASSPHRASE` or a prompt. `mkmilkfs.py selftest-crypto` runs
the published vectors on the tool's own crypto (stdlib only; ChaCha20 and scrypt use the
`cryptography` package or OpenSSL's scrypt for speed when available). A volume being
converted in place is read in either form (`check` says how many blocks it read sealed
and plain); writing to one (`put`, `mkdir`, `receive`, `check --repair`) is refused until
milkOS has finished.

### Scrub

`snap scrub` (start and wait), `snap scrub start|stop|status|rate=MB/s|auto=off|on|7d|12h`,
`/sys/fs/milkfs/scrub` (read: status; write: the same words), kernel
`fs::milkfs::scrub::scrub_volume`. A low-priority kernel thread (`milkfs-scrub`) reads
every block reachable from each mounted volume's committed state (live tree, snapshots,
clones, each shared subtree and shared data block once, dead-list chains, the space map,
and on an encrypted volume the keyring) **straight from the disk** (no bcache, no node
cache, no page cache) and checks its checksum. The keyring block is plaintext: it is
checked by its xxh64 and by decoding it (magic, the volume's uuid, at least one slot), and
reported as `keyring (the wrapped keys)` (keep the recovery key: a damaged keyring means
the next unlock fails). A volume being encrypted or decrypted in place is skipped (and a
scrub can't start meanwhile). It runs on request and on a schedule (default every 7 days, never earlier than
5 minutes after boot; the last full scrub's time and the interval live in the superblock).

- Smooth: priority `Low` (every normal thread runs first), rate limited (default 32 MB/s,
  token bucket; `rate=0` = unlimited), at most 128 KiB per read, the volume lock only to
  start, to finish and to name a damaged file. fsstress (1600 ops) takes 141 ms alone and
  148 ms with a 16 MB/s scrub running alongside (`scripts/scrubtest.sh`).
- Consistent: it checks the state committed when it started. Blocks freed while it runs
  are held back from reuse (`Store::hold_frees`; they count as free in the space map and in
  space checks) and released at the end, so nothing it is about to read can be rewritten.
- Reports: each checksum error with what the block is: data of a file with its path, the
  offset and every tree that has that block (live, snapshots, clones), a tree node, a
  dead-list or space-map block. Printed, in the status, and audit-logged
  (`milkfs.scrub.error`). Nothing is repaired; restore a damaged file from a snapshot with
  a good copy (`versions restore`), if any.

```text
$ snap scrub
scrub: [run 1] done on /Volumes/Milk (requested): 9907 blocks (38 MiB) in 1228 ms, 32.0 MB/s, 2 error(s)
rate: 32 MB/s, auto: every 7d, last full scrub of the primary volume: 2026-10-21T06:58:30
error: block 3: data of /Volumes/Milk/docs/b.bin at offset 0 (live, snapshot s1)
error: block 2: data of /Volumes/Milk/docs/old.bin at offset 0 (snapshot s1)
```

`scripts/scrubtest.sh`: a clean volume scrubs clean; the rate limit holds (8.0 MB/s at
`rate=8`); with one bit flipped (on the Mac, `mkmilkfs.py corrupt IMAGE flip PATH
[--snapshot S]`) in a live file and in a file only a snapshot still has, the scrub finds
exactly those two, with paths and snapshots, and both are in the audit log; the self-test
does the same on its RAM disk and checks the hold-back of frees.

### Send and receive (backups)

`snap send SNAP [--from OLDER] [-o FILE] [--encrypt | --plaintext] [--passphrase-file F]`
writes snapshot SNAP of the primary volume as a stream (full, or incremental from the
older snapshot OLDER) to FILE or stdout; `snap receive [FILE] [--to PATH] [--force]
[--passphrase-file F]` recreates it on the milkfs volume PATH is on (stdin if no FILE;
default the primary volume). Kernel: `fs::milkfs::send(snap, from, out_path, crypt)`
(`StreamCrypt::Passphrase(p)`, `Plain` or `Default`), `fs::milkfs::receive(path, target,
force, passphrase)`; `/sys/fs/milkfs/send` (`SNAP [FROM] PATH [plaintext]`, then
optionally a newline and the stream passphrase) and `/sys/fs/milkfs/receive` (`PATH
[TARGET] [force]`, then optionally a newline and the passphrase), each readable for the
last report (`sent s1, encrypted to ...`). Mac: `mkmilkfs.py verify-stream FILE`,
`mkmilkfs.py receive IMAGE FILE [--force]`, `mkmilkfs.py seal-stream FILE OUT` (encrypt
a plaintext stream), `mkmilkfs.py tree IMAGE [--snapshot S]` (paths, modes, sizes,
mtimes, content hashes: diff two to compare); encrypted streams take
`--stream-passphrase-file F` or `$MILKFS_STREAM_PASSPHRASE` (else they ask). So a backup
can live on the Mac:

```text
snap send monday -o /Volumes/Mac/backups/monday.stream          # (rw share)
snap send tuesday --from monday -o /Volumes/Mac/backups/tuesday.stream
mkmilkfs.py verify-stream share/backups/tuesday.stream
mkmilkfs.py receive /tmp/restore.img share/backups/monday.stream  # then tuesday's
```

**Stream format** (version 1, little endian). A sequence of records, each
`type u8 | 0 0 0 | length u32 | payload | xxh64 u64` (the checksum covers type, length and
payload; payloads at most 64 KiB):

| type | payload |
|---|---|
| 1 header | `"MILKSTRM"`, version u32 = 1, flags u32 (1 = incremental), source uuid [16], label [32], snapshot name [48], its creation time u64, its generation u64, base name [48], base creation time u64, base generation u64, next inode u64 |
| 2 item | key (17) + value: an inode, directory-entry bucket, symlink target or inline data |
| 3 delete | key (17): remove that item |
| 4 drop | ino, from, to (u64): remove file blocks `from..to` |
| 5 data | the sender's block number u64 + its 4096 bytes (each block once, before its first use) |
| 6 extent | ino, start, old_end, sender's first block (u64), count u32, count x xxh64: file blocks `start..start+count` are those blocks; whatever the receiver had in `start..max(start+count, old_end)` goes first |
| 7 end | records u64, data records u64, chained hash u64 (`h = xxh64(h ‖ record checksum)` over every record before it) |

A full stream is every item of the snapshot; an incremental one only what differs from
the base (found like `snap diff`, reading only changed tree nodes), removals first, then
additions and changes in key order. Data travels per run: a changed run is sent whole;
blocks shared by reflink copies are sent once and stay shared on the receiver (it
recounts its shared-block table). Not sent: files with no links left (unlinked while
open: if the receiver has them from the base, they are removed), the orphan list, the
shared-block table. Data removal is by file range, so the receiver may lay blocks out
differently from the sender.

**Encrypted streams** (`snap send --encrypt`; the default from an encrypted volume,
`--plaintext` to opt out; from a plain volume the default is plaintext). The plaintext
stream above is wrapped record by record:

```text
envelope (128 bytes, plaintext, authenticated):
   0 "MILKSTRX"   8 version u32 = 1   12 log_n u8, r u8, p u8, kind u8 = 1 (passphrase)
  16 salt [16]   32 record nonce prefix [16]   48 wrap nonce [24]   72 wrapped stream key [32]
 104 wrap tag [16]   120 zero [8]
then for record i = 0, 1, ...:  len u32 | flags u32 (1 = last) | ciphertext [len] | tag [16]

KEK         = scrypt(passphrase, salt, N = 2^log_n, r, p)          (2^16, 8, 4 for new streams)
wrapped|tag = XChaCha20-Poly1305(KEK, wrap nonce, aad = "milkfs stream key" | envelope[0..48], SK)
record i    = XChaCha20-Poly1305(SK, nonce = prefix | i u64,
                                 aad = "MKSR" | i u64 | len u32 | flags u32, plaintext record)
```

SK is a random 256-bit key per stream. The envelope's fields that matter (format, KDF
parameters, salt, nonce prefix) are authenticated by the key unwrap, the rest is the
wrap itself: an altered envelope reads as a wrong passphrase (EACCES). Each sealed record
is exactly one plaintext record; the counter in nonce and aad catches reordered, dropped
or replayed records, the "last" flag (set on END and only END) a stream cut at a record
boundary, and nothing may follow it (EIO for all of these, before anything changes: the
receive is atomic as below). The inner checksums and chained hash are still checked.
Nothing is in the clear but the envelope: snapshot and volume names, times and sizes are
inside. New stream passphrases need 8 characters. Receive needs the passphrase: EACCES
"the stream is encrypted: give its passphrase" / "wrong passphrase for the stream"; KDF
parameters above 128 MiB of scrypt memory are refused (EINVAL). Cost: about 0.35 s of
scrypt per send and per receive, then XChaCha20-Poly1305 at about 45 MB/s here.

**Receive** is atomic. It applies the records to a hidden clone (`.recv`) of the base
snapshot (for a full stream: of a `.recv-base` snapshot of the empty target, deleted
afterwards); only when the `END` record checks out (counts and chained hash: a missing,
reordered or replaced record is caught) does the clone become the live tree, without
branching history (`clone_fast_forward`: the origin blocks the clone dropped become the
live dead list, as if the live tree had made those changes), and the snapshot is recorded
with the sender's name and creation time. The base of an incremental stream is found by
name + creation time. Refused: a full stream into a non-empty volume (ENOTEMPTY), an
incremental stream whose base isn't there (ENOENT), a target whose live tree changed since
the base (EBUSY; `--force` rolls it back to the base first, deleting newer snapshots, like
`zfs receive -F`), a snapshot name the target has (EEXIST), any damaged or truncated
stream (EIO). A crash mid-receive leaves the target as it was plus the hidden clone (and
`.recv-base`), which the next receive discards. A send keeps the snapshots it reads from
being deleted (EBUSY) and doesn't hold the volume lock while it writes, so the stream can
go onto the volume itself.

`scripts/sendtest.sh`: full + incremental + full-to-stdout sends into the rw Mac share,
plus an `--encrypt` one (verified and received on the Mac, a wrong passphrase refused on
both sides); with `MILK_ENCRYPT=1` everything runs on encrypted streams (the default from
an encrypted volume; `--plaintext` checked to opt out);
`verify-stream` on the Mac; receives in milkOS (onto a second disk) and on the Mac; the
s1 / s2 trees identical on source, received volume and Mac-received images; damaged and
truncated streams refused with the target byte-for-byte untouched; a missing base
refused; qemu killed in the middle of a 20 MB receive: target untouched, and receiving
again works. 21.7 MB full stream: sent in 53 ms, received in 434 ms (from the 9p share).

Limits: one receive at a time per volume; the target should be idle (a write to its live
tree during a receive makes the receive fail with EBUSY at the end); changed runs are
resent whole, so on the receiver they no longer share blocks with unchanged reflink
copies of the old data; sends come from the primary volume.

### Persistent home

With a milkfs volume, `/Users/milk` is a **bind mount** of `<volume>/Users/milk`
(`fs::bind`): same inodes, `path_of`/getcwd still say `/Users/milk`.
`milkfs::setup_home()` runs in `fs::init` right after automount, before the desktop creates
its home folders; it first moves a volume with the old layout over (`layout::migrate`, "The tree"), and the first time it seeds the directory from the initramfs copy (with `~/Sites`). Without a
milkfs disk home stays on tmpfs. Snapshots and rollbacks of the volume include home.

### Interfaces

Kernel API (`crate::fs::milkfs`): `probe(dev)`, `mkfs(dev, label, force)`, `mount(dev, path)`
(first becomes primary), `mount_secondary`, `automount()`, `setup_home()`, `start_threads()`,
`snapshot(name) -> SnapInfo`, `snapshots()`, `delete_snapshot(name)`, `rollback(name)`,
`clone(snap, name)`, `clones()`, `discard_clone(name)`, `promote_clone(name)`, `sync()`,
`info()`, `volume()`, `user_volume()`, `mount_path()`. VFS: `fs::bind`, `fs::unmount`,
`fs::sync_all`, `Inode::sync`, `FileSystem::sync`, `OpenFile::fsync`.

Shell: `/sys/fs/milkfs/info`, `snapshots`, `clones`, `auto` (read); write a name to
`snapshot`, `delete`, `rollback`, `discard`, `promote`; `SNAP NAME` to `clone`; settings to
`auto`; anything to `sync`; `vdX [label] [force]` to `mkfs`. msh has no redirection yet, so
`user/snap` wraps them: `snap`, `snap info`, `snap create|delete|rollback NAME`,
`snap clones`, `snap clone SNAP NAME`, `snap promote|discard NAME`, `snap auto [SETTINGS]`.

### `/sys/fs/milkfs` reference: every file and its errors (for Disk Utility)

**How errors come back.** A control is written with one `write(2)`; failure is that
write's errno. Then `/sys/fs/milkfs/error` says why, one line:
`CONTROL: ERRNO: reason` (e.g. `lock: EBUSY: vdb (Vault) is in use: pid 16 (sleep): 1
open file`); a write that works empties it. It is the last failure on the system (not per
process), so read it right after the failed write and check the control name. `snap`
prints that reason. Writing nothing (a 0-byte write) does nothing at all.

**Who may write.** At `open`: control files are root's, writable by root and the
**admin** group (docs/security.md); anyone else gets EACCES from `open` itself (no
`error` line, the control never ran). Then the destructive ones (marked **D** below,
`fs::milkfs::DESTRUCTIVE`) pass `fs::milkfs::authorize(op, arg)` with the writer's
credentials before running as the kernel: the one place that decides, by default "yes"
(the open check is the rule); `fs::milkfs::set_authorizer(Some(rule))` installs a stricter
one (proc's admin ticket), whose EACCES/EPERM and `set_reason` text show in `error`.
`check` is only authorised for `repair`.

Errors every control can give: ENODEV "no milkfs volume is mounted" / "the volume is
encrypted and locked: unlock it first", EIO "disk or checksum error, or the volume went
read-only after a failed write" (see the kernel log; `snap check`), ENOSPC "the volume is
full", ENOMEM. Arguments are whitespace-trimmed; an empty one is EINVAL with the usage.

| file | read | write | errors (besides the common ones) |
|---|---|---|---|
| `info` | volume status, `key: value` lines (`label`, `device`, `generation`, `blocks`, `snapshots`, `state`, ..., `encryption: on\|off`; when on also `locked`, `cipher`, `key wrap`, `kdf`, `key slots`, see "Encryption at rest"); `no milkfs volume mounted` without one | - | - |
| `snapshots` | `NAME gen=G created=T` per line | - | - |
| `clones` | `NAME of=SNAP at=PATH` | - | - |
| `error` | last failure, `CONTROL: ERRNO: reason` | - | - |
| `snapshot` | = `snapshots` | `NAME` | EINVAL bad name (letters, digits, `. _ - :`), ENAMETOOLONG over 47, EEXIST name taken, ENOSPC "30 snapshots and clones at most: delete one first" |
| `delete` **D** | = `snapshots` | `NAME` | ENOENT no such snapshot, EBUSY a clone is built on it or a send is reading it |
| `rollback` **D** | = `snapshots` | `NAME` | ENOENT, EBUSY a newer snapshot that would go has a clone |
| `clone` | = `clones` | `SNAP NAME` | as `snapshot`, plus ENOENT no such snapshot |
| `discard` **D** | = `clones` | `NAME` | ENOENT no such clone |
| `promote` **D** | = `clones` | `NAME` | ENOENT no such clone, EEXIST snapshot `before-NAME` exists, ENOSPC no record left for `before-NAME` |
| `restore` **D** | - | `SNAPSHOT PATH` | ENOENT no such snapshot or the file isn't in it, EINVAL usage, EXDEV not on the primary volume, EISDIR a folder |
| `versions` | last result | `PATH` | ENOENT no such file, EXDEV, EISDIR |
| `diff` | last result | `A [B\|live]` | ENOENT no such snapshot, EINVAL usage |
| `sync` | - | anything | (common) |
| `auto` **D** | Time Machine settings | `on`, `off`, `now`, `interval=15m`, `keep=N`, `daily=N` | EINVAL anything else |
| `scrub` | progress, errors, last result | `start`, `stop`, `rate=MB/s`, `auto=off\|on\|7d\|12h` | EBUSY already running, EINVAL |
| `check` **D** (repair) | last report | `check` or `repair` | EINVAL |
| `send` **D** | last result | `SNAP [FROM] PATH [plaintext]` [`\nPASSPHRASE`] | ENOENT no such snapshot (or the output's folder), EINVAL usage, FROM not older, passphrase under 8, or no passphrase from an encrypted volume without `plaintext`, ENOSPC no room for the stream |
| `receive` **D** | last result | `PATH [TARGET] [force]` [`\nPASSPHRASE`] | ENOENT base snapshot not on the target (or no stream file), EBUSY target changed since the base (`force` rolls back) or a receive running, ENOTEMPTY full stream needs an empty volume, EEXIST snapshot exists, EINVAL not a stream / newer format / usage, EIO damaged or cut short (nothing changed), EACCES an encrypted stream without or with the wrong passphrase |
| `dedupe` **D** | last result | `DIR` or `DIR dry` | ENOENT, ENOTDIR, EOPNOTSUPP not on milkfs, EINVAL |
| `unlock` | locked volumes, `vdb Vault locked` | `[dev=NAME ]SECRET` | ENODEV nothing (of that name) locked, EACCES wrong passphrase or recovery key, EINVAL usage, EIO keyring unreadable |
| `lock` **D** | encrypted volumes and state | `DEV`, `PATH`, `LABEL` or `all` | EBUSY in use (names the processes) / home volume / send, receive or scrub running (the reason says which), EINVAL not encrypted or already locked, ENODEV no such mounted volume |
| `encrypt` **D** | progress / last result | `[dev=NAME] [recovery=FILE] [rate=MB]` + `\nPASSPHRASE`, or `rate=MB` alone | EBUSY converting already, a send/receive/scrub running, EINVAL encrypted already, passphrase under 8, usage, ENOSPC |
| `decrypt` **D** | = `encrypt` | `[dev=NAME]` + `\nSECRET` | EACCES wrong passphrase or recovery key, EINVAL not encrypted, EBUSY as `encrypt` |
| `passwd` **D** | - | `OLD\nNEW` | EACCES OLD is wrong, EINVAL NEW under 8 characters or the volume isn't encrypted, EIO |
| `mkfs` **D** | usage | `DEVICE [LABEL] [force] [encrypt [recovery=FILE]]` (encrypt: `\nPASSPHRASE`) | ENODEV no such device, EEXIST holds milkfs (add `force`), EBUSY mounted, EINVAL label (1-31 bytes of UTF-8, no `/` or control characters, not starting with `.`), passphrase missing or under 8, EEXIST/EACCES the recovery file can't be created (nothing formatted) |
| `disks` | every block device: `NAME BYTES rw\|ro milkfs plain\|encrypted mounted=PATH\|locked\|unmounted LABEL` or `NAME BYTES rw\|ro other` | - | - |
| `bench` **D** | last result | `DIR N SIZE` | EINVAL usage |
| `treetest` **D** | last result | `DIR N` | EINVAL usage |

The reasons are in `explain()` in `kernel/src/fs/milkfs/mod.rs` (a control's own code can
give a more exact one with `fail(errno, reason)`, as `lock` does); the self-test checks a
sample of them and the authorisation hook (`milkfs_control_errors`).

**mkfs** never runs on its own. In the kernel it is the explicit `mkfs()` call (or the
`/sys/fs/milkfs/mkfs` file); it refuses a disk that already holds milkfs (EEXIST) unless
`force`, and always refuses the mounted volume (EBUSY). On the Mac:
`scripts/mkmilkfs.py mkfs IMAGE [--size 64M] [--label Milk] [--force]`, same refusal.
`mkmilkfs.py check IMAGE` is a host fsck (every checksum, key order, namespace),
`ls IMAGE [PATH] [--snapshot NAME]` and `cat IMAGE PATH [--snapshot NAME]` read images.

### Tests

- Host tests, `scripts/milkfs-test.sh` (`milkfs-unit` in check-all, smoke set): the crate on
  the Mac (`milkfs/src/tests/`), a few seconds. The harness has a memory disk, a disk that
  loses power (`CrashDisk`: after N write requests, after N sectors with the request in
  flight torn, inside the K-th superblock write after T of its 8 sectors, or with a
  volatile write cache that keeps any subset of the unflushed writes, in any order, some
  torn), a fake clock and a seeded RNG per test thread, and a small file API over `State`
  doing what the VFS objects do. Tests by the guest test they stand in for: `basic`
  (format, mount, read, write, namespace rules, batching, orphans, the space map), `btree`
  (a model test, splits and merges, `treetest`), `fsck` (`fscktest`'s damage kinds),
  `reflink` (`reflinktest`), `snapshots` (snapshots, clones, diff, versions, Time Machine),
  `send` (`sendtest`), `scrub` (`scrubtest`), `crypt` (`enctest`), `convert`
  (`converttest`), `upgrade` (`upgradetest`'s v0 image), `crash` (`crashtest`: kill, power
  cut and superblock rounds, a sweep of 48 cut points over one stretch, and the write-cache
  rounds qemu can't do), `compat` (mkfs byte-identical to an image main's kernel wrote,
  with its clock and UUID seed replayed; every structure re-encodes to the same bytes;
  `MILKFS_COMPAT_IMAGE=FILE` checks an image another milkfs wrote, `MILKFS_MKFS_IMAGE=FILE`
  another kernel's fresh mkfs, `MILKFS_EXPORT_IMAGE=FILE` writes one for the other way).
  `MILKFS_TEST_LOG=1` shows the file system's log lines.
- Boot self-test, milkfs part. **The user's volume only gets read-only checks** (mounted,
  info consistent, exact space accounting, home bound); verified byte-identical after a boot.
  Everything destructive runs on a throwaway volume: a 16 MiB RAM disk (`fs/ramdisk.rs`),
  mkfs'd, mounted at `/Volumes/.milk-selftest` (never the primary volume), with the snapshot
  API and `/sys/fs/milkfs` pointed at it for the duration (`set_test_override`, before the
  scheduler starts), then unmounted and its RAM checked to be back. It covers file I/O,
  300 KiB file, truncate, sparse, 60 files, rename/link/symlink/unlink-while-open,
  getdents64, bind mounts, snapshots via the API and `/sys`, browsing, EROFS, rollback,
  delete, Time Machine naming/retention/settings, clones (create, isolation, origin pinned,
  discard, persistence across a re-open, promote, undo), a small-file benchmark, and
  **exact space accounting** (allocated == reachable, no leak, nothing live marked free)
  after every phase. Failures print a loud `*** fs selftest: FAIL ***` line, never a panic.
- `scripts/crashtest.sh [kills] [powercuts] [superblock-cuts]` (default 4 4 4) on a fresh
  `/tmp/milk-crash.img`. A writer workload (self-describing files via tmp + rename,
  snapshots taken, deleted and rolled back, clones made, written into, promoted or
  discarded, so branched history is covered too) is interrupted by SIGKILL of qemu at a random
  time, by an in-kernel power cut after N random sectors (the request in flight is torn,
  the machine halts), or by a cut during the K-th superblock write after T of its 8
  sectors. After each: host `mkmilkfs.py check`, then a check boot that mounts, verifies
  every file in the live tree and all snapshots against its own header, checks the mounted
  generation is the last committed one (or exactly the in-flight one when its superblock
  fully landed), discards the clones, deletes the snapshots and checks space accounting is
  exact. The host check also compares each crashed image's space map with the blocks
  actually reachable (24/24 rounds exact).
- `scripts/mounttime.sh [N]`: mount time with N files, with and without a space map.
- `scripts/treetest.sh [N]`: B+tree shape after creating and deleting N files.
- `scripts/reflinktest.sh`: `files copy --reflink`, `--reflink=auto` / refusal on tmpfs, writing a
  copy, `dedupe -n` / `dedupe` from the shell; byte-exact and clean on the Mac.
- `scripts/stressloop.sh [BOOTS]`: parallel stress (see SMP).
- Self-test, space map (own RAM disk, `State` re-opens as reboots): a fresh volume scans;
  after commits a re-open loads the map; an unlinked-while-open file left at a "crash" is
  deleted from the orphan list; a damaged chunk is noticed and rebuilt; a block in use
  marked free is found by `check` and marked again by `check --repair`.

### Performance (v1)

What v1 changed, all measured with `scripts/cachebench.sh` (virtio disk, HVF, `-smp 4`;
the create benchmark is `/sys/fs/milkfs/bench`: write N files of a size into a fresh
directory and fsync, then read them all back with the page cache dropped):

| | v0 | v1 |
|---|---|---|
| 300 x 1000 B: create | 30.6 ms, 323 blocks | 11.3 ms, 157 blocks |
| 300 x 1000 B: read | 21.0 ms | 5.0 ms |
| 100 x 20 KB: create / read | 29.3 / 50.0 ms | 8.5 / 10.2 ms |
| 4 x 1 MiB: create / read | 48.0 / 91.2 ms | 5.1 / 9.0 ms |
| 4 MiB read, page cache off | 46.6 ms | 21.4 ms |
| 4 MiB read, page cache on, cold / warm | 64.8 / ~0.7 ms | 25.5 / ~0.7 ms |
| FAT32 4 MiB read, page cache off | 43.8 ms | 3.0 ms |

Where it comes from:
- **Runs** (`K_EXT`): the allocator hands out consecutive blocks (`alloc_run`), one item
  describes up to 128 of them, and a run is read with one device request (`bcache.read_run`:
  one request for the missing blocks, cached ones overlaid). Writes inside a run trim or
  split it (`drop_range`); every block keeps its own checksum, so partial reads still verify.
- **Inline data** (`K_INLINE`): files up to 1 KiB live in the tree leaf, no data block; a
  file that grows past that moves to runs.
- **Decoded node cache** in `store.rs`: 512 decoded tree nodes keyed by (block, checksum),
  so lookups don't re-read and re-parse the same nodes (inline data makes leaves fuller and
  the tree a bit deeper; without this, small-file reads got slower). Keyed by checksum, so
  it can never return a stale node.
- **bcache flush coalescing**: up to 64 consecutive dirty blocks per write request.
- **Page cache readahead**: a miss fills up to 16 pages with one file-system read.
- FAT gains too: with the page cache off a read now reaches the file system in one call
  (it used to be one 4 KiB page per call), so FAT's existing cluster-run reads cover all of it.

### Not in v0 (vs the design below)

- Snapshot and clone records live in the superblock (30 in total) instead of a volume
  tree; one volume per disk, no per-app volumes; clones can't be snapshotted.
- Batched commits (about 1 s). Reads are page-cached (see Page cache).
- After a promote, deletes and rollbacks use a full reachability sweep instead of dead lists
  (fine at these sizes; proportional to metadata).
- One global lock per volume (`sched::Mutex<State>`), so operations on a volume serialise.
- No permission checks (like the rest of the VFS), no xattrs.
- Rollback brings back files that were unlinked-but-open when the snapshot was taken; they
  are reclaimed right after.

## milkfs: design sketch (the plan v0 follows)

Goal: a disk file system where snapshots are free, so the OS can offer per-app undo and
try-before-install as ordinary features. Copy-on-write everywhere, like ZFS/btrfs/APFS, but
kept small.

### Principles

- **Never overwrite live data.** Every change writes new blocks; a transaction commits by
  writing a new superblock that points at the new tree. A crash leaves either the old or
  the new state, never a mix. No fsck, no journal.
- **A snapshot is a saved root pointer.** Taking one is O(1): record the current root and
  generation. Blocks are shared until something changes them.
- **Checksums on every block pointer**, so corruption is detected on read.

### On-disk layout (4 KiB blocks)

```
block 0        superblock A  ┐ written alternately; mount takes the valid one
block 1        superblock B  ┘ with the higher generation
block 2..      everything else: tree nodes and file data, allocated anywhere
```

Superblock: magic `MILKFS01`, version, block size, total blocks, `generation` (u64,
+1 per commit), pointer to the **volume tree** root, pointer to the **space map** root,
UUID, label, checksum of the superblock itself.

A **block pointer** is `{ block: u64, birth: u64 (generation written), csum: u64 (xxh64) }`.

Trees are B+trees of 4 KiB nodes (header: level, item count, owner, generation, checksum;
items sorted by key). Leaves hold items keyed by `(object id: u64, type: u8, offset: u64)`:

| tree | key | value |
|---|---|---|
| volume tree | `(volume id, VOLUME, 0)` | name, root pointer of its object tree, parent volume / origin snapshot, created, flags (read-only, snapshot, clone) |
| object tree (one per volume) | `(ino, INODE, 0)` | mode, uid, gid, nlink, size, times |
| | `(dir ino, DIRENT, name hash)` | child ino, type, name (hash collisions: a small list) |
| | `(ino, EXTENT, file offset)` | block pointer + length (contiguous run), or inline data for small files |
| | `(ino, XATTR, name hash)` | extended attributes (app tags, quarantine bits) |
| space map | `(block range)` | free extents (B+tree of free ranges, itself CoW) |

### Freeing with snapshots: birth times + dead lists (ZFS style)

No reference counts. Each block pointer remembers the generation it was born in. When the
live volume drops a block (overwritten or deleted):

- born **after** the volume's newest snapshot → nobody else can see it, free it now;
- otherwise → append it to the volume's **dead list** (an object of its own) tagged with
  the generation it died in.

Deleting a snapshot walks the next-newer snapshot's dead list: blocks born after the
previous-older snapshot and dead before the next one are freed; the rest move to the
merged list. This keeps snapshot creation O(1) and deletion proportional to what changed.

### Transactions

Changes collect in memory (dirty inodes, dirty tree nodes, dirty data blocks). A commit
(every ~5 s, on `fsync`, on snapshot) writes dirty leaves, then parents up to the roots,
issues a virtio-blk `FLUSH`, writes the superblock for `generation + 1`, flushes again.
Old tree nodes go to the dead list / free list as above.

### What the OS builds on it

- **Volumes**: `/` (System), `/Users/milk` (user data), one per app's data
  (`~/Library/<App>`), mounted from the same disk. Shown under `/Volumes`.
- **Per-app undo**: the system snapshots an app's data volume when the app starts and at
  intervals (a ring of N). "Undo what this app did" rolls the volume back to a snapshot
  (swap its root pointer; the dead list of the abandoned state is freed). Snapshots are
  browsable read-only at `<volume>/.snapshots/<time>/`.
- **Try before install**: installing creates a writable *clone* (a snapshot with its own
  dead list) of the System volume and installs into the clone. The app runs with the clone
  as its root. "Keep" makes the clone the System volume (pointer swap at the next commit);
  "Discard" deletes the clone and frees what only it used.
- Cheap copies: `files copy --reflink` shares extents instead of copying (done: see Reflink copies).

### How it plugs into this VFS

- `fs/blockdev.rs` already has the `BlockDevice` trait and virtio-blk implements it
  (`fs::blockdev::get("vda")`). milkfs needs 4 KiB blocks on top: a block cache that reads
  and writes 8 sectors at a time.
- `fs/milkfs/`: `MilkFs::mount(dev: Arc<dyn BlockDevice>, volume: &str) ->
  Result<Arc<MilkFs>>` implements `FileSystem`; `MilkInode` implements `Inode`
  (`lookup`/`readdir` from DIRENT items, `read_at`/`write_at` through EXTENT items and a
  block cache, CoW on write). An inode cache (`BTreeMap<ino, Weak<MilkInode>>`) keeps one
  object per inode, which the VFS's `(dev, ino)` identity relies on for mount points.
- Mounted with `fs::mount("/Volumes/<label>", fs)`, or as `/` with tmpfs kept for `/tmp`
  and `/dev`, `/proc`, `/sys` on top.
- Snapshot control needs a small kernel API (`fs::snapshot(path, name)`,
  `fs::rollback(path, name)`, `fs::clone_volume`) exposed to userspace via `ioctl` on the
  volume root (`Inode::ioctl` exists for this) or a milkOS-specific syscall.
- Needs from other subsystems first: blocking I/O (virtio-blk completion IRQ waking a
  waiter instead of today's polling: proc's wait queues; can't hold `SpinLock`s across that), and for try-before-install
  a per-process root (mount namespace or `chroot`) in proc.
- The VFS will grow `FileSystem::sync()` and `statfs()` for this.
