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

/// File type, as in the `S_IFMT` bits of `st_mode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InodeType {
    File,
    Dir,
    Symlink,
    CharDev,
    BlockDev,
    Fifo,
    Socket,
}

pub const S_IFMT: u32 = 0o170000;
pub const S_IFSOCK: u32 = 0o140000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

/// `linux_dirent64.d_type` values.
pub const DT_UNKNOWN: u8 = 0;
pub const DT_FIFO: u8 = 1;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_BLK: u8 = 6;
pub const DT_REG: u8 = 8;
pub const DT_LNK: u8 = 10;
pub const DT_SOCK: u8 = 12;

impl InodeType {
    /// The `S_IF*` bits for `st_mode`.
    pub const fn mode_bits(self) -> u32 {
        match self {
            InodeType::File => S_IFREG,
            InodeType::Dir => S_IFDIR,
            InodeType::Symlink => S_IFLNK,
            InodeType::CharDev => S_IFCHR,
            InodeType::BlockDev => S_IFBLK,
            InodeType::Fifo => S_IFIFO,
            InodeType::Socket => S_IFSOCK,
        }
    }

    pub const fn from_mode(mode: u32) -> Option<InodeType> {
        Some(match mode & S_IFMT {
            S_IFREG => InodeType::File,
            S_IFDIR => InodeType::Dir,
            S_IFLNK => InodeType::Symlink,
            S_IFCHR => InodeType::CharDev,
            S_IFBLK => InodeType::BlockDev,
            S_IFIFO => InodeType::Fifo,
            S_IFSOCK => InodeType::Socket,
            _ => return None,
        })
    }

    /// The `d_type` byte for `linux_dirent64`.
    pub const fn dirent_type(self) -> u8 {
        match self {
            InodeType::File => DT_REG,
            InodeType::Dir => DT_DIR,
            InodeType::Symlink => DT_LNK,
            InodeType::CharDev => DT_CHR,
            InodeType::BlockDev => DT_BLK,
            InodeType::Fifo => DT_FIFO,
            InodeType::Socket => DT_SOCK,
        }
    }

    /// One letter, as in the first column of `ls -l`.
    pub const fn letter(self) -> char {
        match self {
            InodeType::File => '-',
            InodeType::Dir => 'd',
            InodeType::Symlink => 'l',
            InodeType::CharDev => 'c',
            InodeType::BlockDev => 'b',
            InodeType::Fifo => 'p',
            InodeType::Socket => 's',
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}

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
}

/// A mounted (or mountable) file system instance.
pub trait FileSystem: Send + Sync {
    /// Type name as in `/proc/mounts` (`tmpfs`, `devfs`, `proc`, `sysfs`).
    fn name(&self) -> &'static str;
    /// Device number, `st_dev` of every inode in it.
    fn dev(&self) -> u64;
    fn root(&self) -> InodeRef;
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
