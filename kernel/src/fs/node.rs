//! The object model: a [`FileSystem`] hands out [`Inode`]s, shared as `Arc<dyn Inode>`.
//!
//! An inode is one file-system object (regular file, directory, symlink, device). Every
//! operation is a method with a sensible default error, so a file system only implements
//! what it supports. Names passed to directory methods are single path components: never
//! empty, never `.` or `..` (the VFS resolves those), never containing `/`, at most
//! [`NAME_MAX`] bytes. The one exception is `lookup("..")`, which every directory must
//! answer (the root of a file system returns itself).
//!
//! Locking rule: an inode may take its own locks inside a method but must never hold one
//! while calling into another inode it does not own, or into the VFS (single-core
//! `SpinLock`s panic on re-entry rather than deadlock).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::errno::{Errno, Result};

pub type InodeRef = Arc<dyn Inode>;
pub type FsRef = Arc<dyn FileSystem>;

/// Longest single path component, in bytes.
pub const NAME_MAX: usize = 255;
/// Longest path the VFS accepts, in bytes (including the terminating NUL in Linux terms).
pub const PATH_MAX: usize = 4096;

// The file types and `Timespec` are pure ABI, shared with the milkfs crate (which stores
// them): defined there.
#[allow(unused_imports)]
pub use ::milkfs::abi::{
    DT_BLK, DT_CHR, DT_DIR, DT_FIFO, DT_LNK, DT_REG, DT_SOCK, DT_UNKNOWN, InodeType, S_IFBLK,
    S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK, S_IFMT, S_IFREG, S_IFSOCK, Timespec,
};

/// File metadata, in kernel form. [`Stat::to_linux`] gives the userspace layout.
#[derive(Clone, Debug)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    pub kind: InodeType,
    /// Permission bits only (`0o7777`); the type is in `kind`.
    pub perm: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    /// Device number for character/block devices (Linux `makedev` encoding).
    pub rdev: u64,
    pub size: u64,
    pub blksize: u32,
    /// 512-byte blocks actually allocated.
    pub blocks: u64,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
}

impl Stat {
    /// A stat with everything zero except identity, type and permissions.
    pub fn new(dev: u64, ino: u64, kind: InodeType, perm: u32) -> Stat {
        Stat {
            dev,
            ino,
            kind,
            perm,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            size: 0,
            blksize: 4096,
            blocks: 0,
            atime: Timespec::default(),
            mtime: Timespec::default(),
            ctime: Timespec::default(),
        }
    }

    /// Full `st_mode`: type bits | permission bits.
    pub fn mode(&self) -> u32 {
        self.kind.mode_bits() | (self.perm & 0o7777)
    }
}

/// One directory entry, as returned by [`Inode::readdir`].
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: InodeType,
}

/// Fields to change in [`Inode::setattr`] (chmod, chown, utimensat). `None` = leave alone.
#[derive(Clone, Copy, Debug, Default)]
pub struct SetAttr {
    pub perm: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<Timespec>,
    pub mtime: Option<Timespec>,
}

/// Lets the VFS and file systems downcast `dyn Inode` to a concrete type (tmpfs rename and
/// link need to know the other inode is theirs). Implemented for every `'static` type.
pub trait AsAny {
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `inode` as a `&T`, if it is one.
pub fn downcast<T: 'static>(inode: &InodeRef) -> Option<&T> {
    let i: &dyn Inode = &**inode;
    <dyn Inode as AsAny>::as_any(i).downcast_ref::<T>()
}

/// The error for a file operation on the wrong kind of object.
fn not_file(kind: InodeType) -> Errno {
    if kind == InodeType::Dir {
        Errno::EISDIR
    } else {
        Errno::EINVAL
    }
}

/// The error for a directory operation on a non-directory, or on a directory whose file
/// system does not support changing it.
fn not_dir(kind: InodeType) -> Errno {
    if kind == InodeType::Dir {
        Errno::EROFS
    } else {
        Errno::ENOTDIR
    }
}

/// One file-system object. See the module docs for the rules on names and locking.
pub trait Inode: AsAny + Send + Sync {
    /// Inode number, unique within its file system.
    fn ino(&self) -> u64;
    /// Device number of the file system it lives on (same as `FileSystem::dev`).
    fn dev(&self) -> u64;
    fn kind(&self) -> InodeType;
    fn stat(&self) -> Stat;

    // ---- data (regular files and devices) --------------------------------------------

    /// Read up to `buf.len()` bytes at `off`. Returns 0 at end of file.
    fn read_at(&self, _off: u64, _buf: &mut [u8]) -> Result<usize> {
        Err(not_file(self.kind()))
    }
    /// Write `buf` at `off`, growing the file if needed. Returns bytes written.
    fn write_at(&self, _off: u64, _buf: &[u8]) -> Result<usize> {
        Err(not_file(self.kind()))
    }
    /// Set the size to `len`, dropping or zero-filling data.
    fn truncate(&self, _len: u64) -> Result<()> {
        Err(not_file(self.kind()))
    }
    /// Device-specific control (ioctl). Default: not a tty / no such control.
    fn ioctl(&self, _cmd: u32, _arg: usize) -> Result<usize> {
        Err(Errno::ENOTTY)
    }
    /// poll(2) readiness as Linux `POLL*` bits. Default: always readable and writable
    /// (right for regular files); devices and sockets that can block override it.
    fn poll(&self) -> u32 {
        0x1 | 0x4 // POLLIN | POLLOUT
    }
    /// A counter that changes whenever this object's readiness may have changed (new
    /// data, space, hangup), for edge-triggered epoll (`crate::poll`). Default: the global
    /// readiness generation, which changes on any event: never misses an edge, may report
    /// extra ones. Objects that can do better (pipes, AF_UNIX sockets) count their own.
    fn poll_seq(&self) -> u64 {
        crate::poll::generation()
    }

    // ---- directories -----------------------------------------------------------------

    /// Find `name` in this directory. Must handle `..`.
    fn lookup(&self, _name: &str) -> Result<InodeRef> {
        Err(not_dir(self.kind()))
    }
    /// List the entries, without `.` and `..` (the VFS adds those).
    fn readdir(&self) -> Result<Vec<DirEntry>> {
        Err(not_dir(self.kind()))
    }
    /// Create a regular file. EEXIST if `name` exists.
    fn create(&self, _name: &str, _perm: u32) -> Result<InodeRef> {
        Err(not_dir(self.kind()))
    }
    fn mkdir(&self, _name: &str, _perm: u32) -> Result<InodeRef> {
        Err(not_dir(self.kind()))
    }
    fn symlink(&self, _name: &str, _target: &str) -> Result<InodeRef> {
        Err(not_dir(self.kind()))
    }
    /// Create a special node (`Socket` or `Fifo`) that carries no data: a name for an
    /// AF_UNIX socket (proc/unix.rs) or a FIFO. Default: not supported.
    fn mknod(&self, _name: &str, _kind: InodeType, _perm: u32) -> Result<InodeRef> {
        Err(Errno::EPERM)
    }
    /// Make `name` another hard link to `target` (never a directory; same fs, the VFS checks).
    fn link(&self, _name: &str, _target: &InodeRef) -> Result<()> {
        Err(not_dir(self.kind()))
    }
    /// Remove a non-directory entry. EISDIR for a directory.
    fn unlink(&self, _name: &str) -> Result<()> {
        Err(not_dir(self.kind()))
    }
    /// Remove an empty directory. ENOTDIR for a non-directory, ENOTEMPTY if not empty.
    fn rmdir(&self, _name: &str) -> Result<()> {
        Err(not_dir(self.kind()))
    }
    /// Move `old` in this directory to `new` in `new_dir` (same file system, the VFS
    /// checks), replacing a compatible existing `new` (POSIX rename rules).
    fn rename(&self, _old: &str, _new_dir: &InodeRef, _new: &str) -> Result<()> {
        Err(not_dir(self.kind()))
    }

    // ---- misc ------------------------------------------------------------------------

    fn readlink(&self) -> Result<String> {
        Err(Errno::EINVAL)
    }
    fn setattr(&self, _attr: &SetAttr) -> Result<()> {
        Err(Errno::EPERM)
    }
    /// `fsync`: make this file's changes durable. Default: nothing to do (RAM file systems).
    fn sync(&self) -> Result<()> {
        Ok(())
    }
}

/// A mounted (or mountable) file system instance.
pub trait FileSystem: Send + Sync {
    /// Type name as in `/proc/mounts` (`tmpfs`, `devfs`, `proc`, `sysfs`).
    fn name(&self) -> &'static str;
    /// Device number, `st_dev` of every inode in it.
    fn dev(&self) -> u64;
    fn root(&self) -> InodeRef;
    /// `sync`: make everything durable. Default: nothing to do.
    fn sync(&self) -> Result<()> {
        Ok(())
    }
    /// `statfs(2)`: size and free space. Default: no storage of its own (all sizes 0),
    /// the type from [`magic_of`] (devfs, proc, sysfs).
    fn statfs(&self) -> Result<StatFs> {
        Ok(StatFs::empty(magic_of(self.name()), self.dev()))
    }
}

/// What `statfs(2)` reports about a file system (sizes in `bsize` units). See
/// docs/filesystem.md "statfs" for the Linux `struct statfs` it becomes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatFs {
    /// `f_type`: the file system's magic number (`*_MAGIC` below).
    pub fs_type: u64,
    pub bsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    /// Free blocks an unprivileged user may use.
    pub bavail: u64,
    /// Inodes (0 when the file system has no fixed inode table).
    pub files: u64,
    pub ffree: u64,
    /// `f_fsid` (two 32-bit words, low first).
    pub fsid: u64,
    pub namelen: u64,
    /// `ST_*` mount flags.
    pub flags: u64,
}

pub const ST_RDONLY: u64 = 1;
pub const ST_NOSUID: u64 = 2;
pub const ST_NODEV: u64 = 4;
pub const ST_NOEXEC: u64 = 8;
/// Linux sets this on every statfs answer (the flags are valid).
pub const ST_VALID: u64 = 0x20;

/// "MILK": milkOS's own (no Linux file system uses it).
pub const MILKFS_MAGIC: u64 = 0x4d49_4c4b;
pub const TMPFS_MAGIC: u64 = 0x0102_1994;
pub const MSDOS_MAGIC: u64 = 0x4d44;
pub const V9FS_MAGIC: u64 = 0x0102_1997;
pub const PROC_MAGIC: u64 = 0x9fa0;
pub const SYSFS_MAGIC: u64 = 0x6265_6572;
/// The old Linux devfs's number (devtmpfs reuses tmpfs's; ours has no storage).
pub const DEVFS_MAGIC: u64 = 0x1373;

/// `f_type` for a file system type name (`FileSystem::name`); 0 if unknown.
pub fn magic_of(name: &str) -> u64 {
    match name {
        "milkfs" => MILKFS_MAGIC,
        "tmpfs" => TMPFS_MAGIC,
        "vfat" => MSDOS_MAGIC,
        "9p" => V9FS_MAGIC,
        "proc" => PROC_MAGIC,
        "sysfs" => SYSFS_MAGIC,
        "devfs" => DEVFS_MAGIC,
        _ => 0,
    }
}

impl StatFs {
    /// No storage: every size 0, 4 KiB blocks, 255-byte names.
    pub fn empty(fs_type: u64, fsid: u64) -> StatFs {
        StatFs {
            fs_type,
            bsize: 4096,
            fsid,
            namelen: 255,
            flags: ST_VALID,
            ..StatFs::default()
        }
    }
}

/// Linux `makedev`.
pub const fn makedev(major: u32, minor: u32) -> u64 {
    let (ma, mi) = (major as u64, minor as u64);
    (mi & 0xff) | ((ma & 0xfff) << 8) | ((mi & !0xff) << 12) | ((ma & !0xfff) << 32)
}

static NEXT_ANON_MINOR: AtomicU64 = AtomicU64::new(1);

/// A fresh anonymous device number (major 0, like Linux tmpfs/proc) for a new fs instance.
pub fn alloc_dev() -> u64 {
    makedev(0, NEXT_ANON_MINOR.fetch_add(1, Ordering::Relaxed) as u32)
}

/// Is `name` an acceptable single component for creating something?
pub fn check_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Errno::ENOENT);
    }
    if name.len() > NAME_MAX {
        return Err(Errno::ENAMETOOLONG);
    }
    if name.contains('/') || name.contains('\0') {
        return Err(Errno::EINVAL);
    }
    Ok(())
}
