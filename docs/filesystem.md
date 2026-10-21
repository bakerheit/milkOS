# File systems

Files: `kernel/src/fs/` (milkfs in `fs/milkfs/`), `kernel/src/virtio/blk.rs`,
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
| `selftest.rs` | boot self-test (247 checks with both disks), then prints `/proc/*`, appends to `/Volumes/Milk/boot.log`, `cat /etc/motd` |

Boot: `kmain` calls `fs::init()` after `memtest::run()`, then `fs::selftest::run()`.
`fs::init` mounts tmpfs on `/`, creates the skeleton, unpacks the initramfs, mounts devfs,
procfs and sysfs, then mounts the first disk that holds milkfs at `/Volumes/<label>`
(never formats anything).

## The tree

macOS-flavoured on top, Linux underneath, so both kinds of software find their way.

| path | fs | what |
|---|---|---|
| `/` | tmpfs | root, everything in RAM |
| `/Applications` | milkfs (bind of `/Volumes/Milk/Applications`), tmpfs without a disk | packages (`milk install`, docs/packages.md), persistent |
| `/System` | tmpfs | OS files; `/System/Version.txt` |
| `/home/milk` | milkfs (bind of `/Volumes/Milk/home/milk`), tmpfs without a disk | the user's home (`$HOME`), persistent |
| `/Users` | symlink → `home` | macOS spelling: `/Users/milk` == `/home/milk` |
| `/Volumes/Mac` | 9p | the Mac's `share/` folder (read-only unless `MILK_SHARE_RW=1`) |
| `/Volumes` | tmpfs | disks: `/Volumes/Milk` is the milkfs volume, `/Volumes/Milk/.snapshots/<name>` its snapshots, `/Volumes/Milk-<name>` its clones |
| `/bin` | tmpfs | user programs from the initramfs |
| `/etc` | tmpfs | `motd`, `hostname` (= `milk`) |
| `/tmp` | tmpfs | mode `1777` |
| `/dev` | devfs | `null zero random urandom console`, `vda`... (one per disk) |
| `/proc` | proc | `meminfo uptime version interrupts mounts` |
| `/sys` | sysfs | `devices/platform/*`, `kernel/version`, `fs/milkfs/*` (snapshot control) |

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
pub fn symlink(target: &str, linkpath: &str) -> Result<()>;      // + symlink_at(target, base, linkpath)
pub fn readlink(path: &str) -> Result<String>;                    // + readlink_at
pub fn link(old: &str, new: &str) -> Result<()>;                  // + link_at(.., follow)
pub fn truncate(path: &str, len: u64) -> Result<()>;
pub fn chmod(path: &str, perm: u32) -> Result<()>;
pub fn setattr_at(base: Option<&InodeRef>, path: &str, follow: bool, a: &SetAttr) -> Result<()>;

// whole files (loading ELF binaries etc.)
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
- `/dev/random` and `/dev/urandom`: xorshift64* seeded from the counter. **Not
  cryptographic.** Writes stir the state.
- Timestamps: Unix time from the PL031 RTC read once at `fs::init`, plus uptime. No atime
  updates.
- Locking: tmpfs, devfs, procfs and sysfs use `SpinLock`s and never block (tmpfs copies
  file data with IRQs masked). milkfs, the bcache and virtio-blk **sleep** (`sched::Mutex`,
  wait queues): so VFS calls are thread context only, never from IRQ context and never
  while holding a `SpinLock` (that's also true because the VFS allocates).

## Writing a file system

Implement `FileSystem` (`name`, `dev` from `node::alloc_dev()`, `root`) and `Inode` for
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
skeleton, so it can add to `/bin`, `/etc`, ...). Entries are sorted, uid/gid 0, mtime 0:
the archive is reproducible. Two ways in:

1. **Files checked into git**: put them under `rootfs/` at the same path they should have
   at runtime (`rootfs/etc/motd` → `/etc/motd`). Permissions come from the host file;
   symlinks are kept as symlinks.
2. **Build outputs** (user programs): list them in `initramfs.list` at the repo root, one
   per line, `<path in image> <host path relative to repo root> [octal mode]`:

   ```
   /bin/init   target/user/aarch64-unknown-none/release/init   755
   /bin/sh     target/user/aarch64-unknown-none/release/sh     755
   ```

   Parent directories are created as needed. A missing host file is skipped with a
   `cargo:warning` (so the kernel still builds before the user programs are), and cargo
   re-runs the packer when any listed file changes. Build the user programs first, then
   `cargo build` the kernel. `fs::read_file("/bin/init")` then hands you the ELF.

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
  byte-exact compares, writes (long-name dir and file, 200 KB binary, rename to a long name,
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
| milkfs (`/home/milk`) | 38 ms (104 MiB/s) | 48 ms (83 MiB/s) | 0.66 ms (6.0 GiB/s) |
| FAT32 (512 B clusters) | 34 ms (118 MiB/s) | 39 ms (103 MiB/s) | 0.39 ms (10 GiB/s) |

Filling the cache costs a frame allocation and a copy per page, so a first read with the
cache on is slightly slower. FAT reads also walk the cluster chain from a per-file cursor
and read runs of contiguous clusters in one request (before that change, a cold 4 MiB FAT
read took 289 ms).

## Next steps

- Pipes (`InodeType::Fifo`) and `poll` on the wait queues; `O_NONBLOCK`.
- Permission checks once processes have credentials (`Inode::stat` already has
  uid/gid/perm), `umask` in the syscall layer.
- `mmap` of tmpfs files (expose the frame for a page index), `statfs`, `sync`.

## milkfs v0 (implemented)

`kernel/src/fs/milkfs/`, host tool `scripts/mkmilkfs.py`, crash test `scripts/crashtest.sh`.
Mounted at `/Volumes/<label>` (`/Volumes/Milk`). The design it follows is the sketch below;
the differences are listed at the end of this section.

| file | what |
|---|---|
| `format.rs` | on-disk structures, xxh64, FNV-1a name hash (mirrored in `mkmilkfs.py`) |
| `store.rs` | allocator (free map rebuilt at mount), birth-time release rules, checksummed block I/O, dead-list chains |
| `btree.rs` | copy-on-write B+tree: dirty nodes in memory, written bottom-up at commit |
| `volume.rs` | `State`, commit, mount (mark + namespace check + orphan reclaim), mkfs, space check |
| `items.rs` | inodes, directory entries, extents, symlinks, file data |
| `snap.rs` | snapshot create / delete / rollback |
| `clone.rs` | clones: create / discard / promote |
| `timemachine.rs` | automatic snapshots thread, retention, `/sys/fs/milkfs/auto` |
| `inode.rs` | VFS objects: `Volume` (the `FileSystem`), `MilkInode`, `.snapshots` |
| `crashtest.rs` | fw_cfg-selected crash workloads and checker (inert on a normal boot) |
| `mod.rs` | kernel API, automount, `/sys/fs/milkfs` |

### On-disk format v0

All integers little endian, 4 KiB blocks.

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
| 8 | version u32 = 0, 12: block size u32 = 4096 |
| 16 | total blocks u64, 24: generation u64 |
| 32 | live tree root: block pointer |
| 56 | live dead list head: block pointer |
| 80 | next inode number u64, 88: created (unix s) u64 |
| 96 | uuid [16], 112: label [32] (NUL padded) |
| 144 | snapshot count u32 (max 30) |
| 152 | 30 records x 128 B (snapshots and clones): name [48], generation u64 (clone: its origin's), created u64, root pointer, dead list pointer, next_ino u64, flags u32 (1 = clone), id u32 (clones) |
| 3992 | automatic snapshots: state u32 (0 default/1 on/2 off), interval s u32, keep u32, daily u32 (0 = default) |
| 4008 | flags u32: 1 = `SB_BRANCHED` (a clone was promoted; see below) |
| 4088 | xxh64 of bytes 0..4088 |

**Tree node**: header `"MKND" | level u8 (0 = leaf) | pad u8 | count u16 | birth u64`, then
leaf items `key (17 B) | value length u16 | value` or internal entries
`key (17 B) | child pointer (24 B)` (up to 99 per node). A key is
`(object id u64, kind u8, offset u64)`, sorted in that order; child `i` of an internal node
covers keys from its separator to the next one. Values are at most 1500 bytes.

| kind | key | value |
|---|---|---|
| 1 inode | `(ino, 1, 0)` | 68 B: mode u32 (`S_IF*` + perm), nlink u32, uid u32, gid u32, size u64, parent u64 (directories), atime/mtime/ctime as (i64 s, u32 ns) |
| 2 dirent | `(dir ino, 2, fnv1a64(name))` | every name in that dir with that hash: `ino u64 | d_type u8 | len u8 | name` repeated |
| 3 extent | `(ino, 3, block index)` | block pointer of that 4 KiB of the file (absent = hole) |
| 4 symlink | `(ino, 4, 0)` | target bytes (max 1024) |

The root directory is inode 1 (its parent is itself). Inode numbers only grow.

**Dead-list block**: `"MKDL" | count u32 | next pointer (24 B) | count x (block u64, birth u64)`
(254 per block). A chain is only ever prepended to or rewritten whole.

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
   dead-list entries as a new chain block, `bcache.flush()` (everything durable),
3. writes the new superblock to slot `generation % 2` (**the commit point**), flushes again.

A crash at any moment leaves the previous superblock valid and everything it references
untouched; a torn superblock fails its checksum and the other slot wins. Blocks the commit
freed only become reusable after it is durable.

**Branched history.** Dead lists rely on history being linear (a block that leaves the
live tree never comes back). Promoting a clone breaks that, so it sets `SB_BRANCHED`:
dead lists are dropped and snapshot delete, rollback and promote reclaim space with an
exact **sweep** (free every allocated block the committed state can't reach, the same walk
mount does). When no snapshots are left the history is linear again and the flag clears.

**Free space is not stored.** Mount marks every block reachable from the chosen superblock
(live tree, every snapshot tree, shared subtrees once, extents, dead-list chains); the rest is
free. So the allocator can't be wrong after a crash. While a transaction is open a dropped
block is: born in this transaction → reused at once; born after the newest snapshot → freed
when the commit is durable; otherwise a snapshot still has it → live dead list.

Mount also checks the live namespace (every entry points at an inode, directory parents
and link counts match) and reclaims orphans (inodes with no entry: unlinked while open
before a crash). It prints e.g. `milkfs: vda mounted at /Volumes/Milk: generation 1430,
2 inodes, 4/16384 blocks used, 0 snapshot(s), check clean`.

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
A /home/milk/Documents/Plans 2026
D /home/milk/Documents/todo.txt
M /home/milk/README.txt
R /dd/a -> /dd/sub/a2
```

Only changed nodes are read: walking the newer tree, a subtree whose pointer was born at or
before the older snapshot is skipped (CoW: it's the same subtree in both), and those
pointers are skipped in the older tree too; the items of the remaining leaves are compared.
After a promote (branched history) shared subtrees are found by block-pointer equality
against the older tree instead. One change among 300 files reads at most a handful of
nodes (self-test checks <= 8). Files now record their parent directory (as directories
always did), so paths are found without scanning; older files fall back to a scan.

### Persistent home

With a milkfs volume, `/home/milk` is a **bind mount** of `<volume>/home/milk`
(`fs::bind`): same inodes, `path_of`/getcwd still say `/home/milk`, `/Users/milk` follows.
`milkfs::setup_home()` runs in `fs::init` right after automount, before the desktop creates
its home folders; the first time it seeds the directory from the initramfs copy. Without a
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

**mkfs** never runs on its own. In the kernel it is the explicit `mkfs()` call (or the
`/sys/fs/milkfs/mkfs` file); it refuses a disk that already holds milkfs (EEXIST) unless
`force`, and always refuses the mounted volume (EBUSY). On the Mac:
`scripts/mkmilkfs.py mkfs IMAGE [--size 64M] [--label Milk] [--force]`, same refusal.
`mkmilkfs.py check IMAGE` is a host fsck (every checksum, key order, namespace),
`ls IMAGE [PATH] [--snapshot NAME]` and `cat IMAGE PATH [--snapshot NAME]` read images.

### Tests

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
  exact.

### Not in v0 (vs the design below)

- Snapshot and clone records live in the superblock (30 in total) instead of a volume
  tree; one volume per disk, no per-app volumes; clones can't be snapshotted.
- No persistent space map (rebuilt at mount instead; mount time grows with metadata size).
- One extent per 4 KiB block (no multi-block extents, no inline data, no reflink copies).
- B+tree deletion doesn't merge underfull nodes.
- Batched commits (about 1 s). Reads are page-cached (see Page cache).
- After a promote, deletes and rollbacks use a full reachability sweep instead of dead lists
  (fine at these sizes; proportional to metadata).
- One global lock per volume (`sched::Mutex<State>`), so operations on a volume serialise.
- No permission checks (like the rest of the VFS), no xattrs, no fsck repair (check only).
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

- **Volumes**: `/` (System), `/home/milk` (user data), one per app's data
  (`~/Library/<App>`), mounted from the same disk. Shown under `/Volumes`.
- **Per-app undo**: the system snapshots an app's data volume when the app starts and at
  intervals (a ring of N). "Undo what this app did" rolls the volume back to a snapshot
  (swap its root pointer; the dead list of the abandoned state is freed). Snapshots are
  browsable read-only at `<volume>/.snapshots/<time>/`.
- **Try before install**: installing creates a writable *clone* (a snapshot with its own
  dead list) of the System volume and installs into the clone. The app runs with the clone
  as its root. "Keep" makes the clone the System volume (pointer swap at the next commit);
  "Discard" deletes the clone and frees what only it used.
- Cheap copies: `cp` of a large file can clone extents (reflink) instead of copying.

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
