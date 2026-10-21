//! An open file: an inode plus a file offset and the open flags. This is what a process fd
//! table points at (share it with `Arc<OpenFile>` for `dup`/`fork`, which share the offset
//! like Linux's `struct file`).

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::abi::{
    self, O_ACCMODE, O_APPEND, O_PATH, O_RDONLY, O_WRONLY, SEEK_CUR, SEEK_END, SEEK_SET, SETFL_MASK,
};
use super::errno::{Errno, Result};
use super::node::{InodeRef, InodeType, Stat};
use super::path;

pub struct OpenFile {
    inode: InodeRef,
    flags: AtomicU32,
    /// File offset. For directories: index of the next entry for `getdents64`.
    pos: AtomicU64,
}

impl OpenFile {
    pub fn new(inode: InodeRef, flags: u32) -> OpenFile {
        OpenFile {
            inode,
            flags: AtomicU32::new(flags),
            pos: AtomicU64::new(0),
        }
    }

    pub fn inode(&self) -> &InodeRef {
        &self.inode
    }

    /// Open flags (access mode + O_APPEND/O_NONBLOCK/...). O_CREAT/O_EXCL/O_TRUNC are
    /// consumed by `open` and not kept. O_CLOEXEC is kept but is the fd table's business.
    pub fn flags(&self) -> u32 {
        self.flags.load(Ordering::Relaxed)
    }

    /// `fcntl(F_SETFL)`: only the [`SETFL_MASK`] bits change.
    pub fn set_flags(&self, flags: u32) {
        let old = self.flags();
        self.flags.store(
            (old & !SETFL_MASK) | (flags & SETFL_MASK),
            Ordering::Relaxed,
        );
    }

    pub fn readable(&self) -> bool {
        let f = self.flags();
        f & O_PATH == 0 && f & O_ACCMODE != O_WRONLY
    }

    pub fn writable(&self) -> bool {
        let f = self.flags();
        f & O_PATH == 0 && f & O_ACCMODE != O_RDONLY
    }

    pub fn pos(&self) -> u64 {
        self.pos.load(Ordering::Relaxed)
    }

    pub fn stat(&self) -> Stat {
        self.inode.stat()
    }

    /// `read(2)`: read at the current offset and advance it.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize> {
        let off = self.pos();
        let n = self.pread(buf, off)?;
        self.pos.store(off + n as u64, Ordering::Relaxed);
        Ok(n)
    }

    /// `write(2)`: write at the current offset (or the end, with O_APPEND) and advance it.
    pub fn write(&self, buf: &[u8]) -> Result<usize> {
        if !self.writable() {
            return Err(Errno::EBADF);
        }
        let off = if self.flags() & O_APPEND != 0 {
            self.inode.stat().size
        } else {
            self.pos()
        };
        let n = self.pwrite(buf, off)?;
        self.pos.store(off + n as u64, Ordering::Relaxed);
        Ok(n)
    }

    /// `pread64(2)`: read at `off`; the file offset is untouched.
    pub fn pread(&self, buf: &mut [u8], off: u64) -> Result<usize> {
        if !self.readable() {
            return Err(Errno::EBADF);
        }
        if self.inode.kind() == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        self.inode.read_at(off, buf)
    }

    /// `pwrite64(2)`: write at `off`; the file offset is untouched.
    pub fn pwrite(&self, buf: &[u8], off: u64) -> Result<usize> {
        if !self.writable() {
            return Err(Errno::EBADF);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        self.inode.write_at(off, buf)
    }

    /// `lseek(2)`. Returns the new offset.
    pub fn seek(&self, off: i64, whence: u32) -> Result<u64> {
        if self.flags() & O_PATH != 0 {
            return Err(Errno::EBADF);
        }
        let base: i64 = match whence {
            SEEK_SET => 0,
            SEEK_CUR => self.pos() as i64,
            SEEK_END if self.inode.kind() != InodeType::Dir => self.inode.stat().size as i64,
            _ => return Err(Errno::EINVAL),
        };
        let new = base.checked_add(off).ok_or(Errno::EINVAL)?;
        if new < 0 {
            return Err(Errno::EINVAL);
        }
        self.pos.store(new as u64, Ordering::Relaxed);
        Ok(new as u64)
    }

    /// `ftruncate(2)`.
    pub fn truncate(&self, len: u64) -> Result<()> {
        if !self.writable() {
            return Err(Errno::EINVAL);
        }
        if self.inode.kind() == InodeType::Dir {
            return Err(Errno::EISDIR);
        }
        self.inode.truncate(len)
    }

    /// `fsync(2)` / `fdatasync(2)`.
    pub fn fsync(&self) -> Result<()> {
        self.inode.sync()
    }

    /// `ioctl(2)`, passed straight to the inode.
    pub fn ioctl(&self, cmd: u32, arg: usize) -> Result<usize> {
        if self.flags() & O_PATH != 0 {
            return Err(Errno::EBADF);
        }
        self.inode.ioctl(cmd, arg)
    }

    /// `getdents64(2)`: fill `buf` with `linux_dirent64` records starting at the current
    /// position, including `.` and `..`. Returns bytes written; 0 at the end. EINVAL if
    /// `buf` is too small for even one record.
    pub fn getdents64(&self, buf: &mut [u8]) -> Result<usize> {
        if self.flags() & O_PATH != 0 {
            return Err(Errno::EBADF);
        }
        if self.inode.kind() != InodeType::Dir {
            return Err(Errno::ENOTDIR);
        }
        let entries = self.inode.readdir()?;
        let start = self.pos() as usize;
        let total = entries.len() + 2;
        let mut idx = start;
        let mut used = 0;
        while idx < total {
            let (ino, kind, name): (u64, InodeType, &str) = match idx {
                0 => (self.inode.ino(), InodeType::Dir, "."),
                1 => {
                    let ino = path::step(&self.inode, "..")
                        .map(|p| p.ino())
                        .unwrap_or(self.inode.ino());
                    (ino, InodeType::Dir, "..")
                }
                i => {
                    let e = &entries[i - 2];
                    (e.ino, e.kind, e.name.as_str())
                }
            };
            match abi::put_dirent64(
                &mut buf[used..],
                ino,
                (idx + 1) as i64,
                kind.dirent_type(),
                name,
            ) {
                Some(n) => used += n,
                None => break,
            }
            idx += 1;
        }
        if used == 0 && idx < total {
            return Err(Errno::EINVAL);
        }
        self.pos.store(idx as u64, Ordering::Relaxed);
        Ok(used)
    }
}
