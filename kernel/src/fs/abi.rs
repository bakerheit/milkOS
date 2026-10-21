//! Linux (aarch64) userspace ABI pieces the VFS produces or consumes: open flags, `lseek`
//! whence, `*at` flags, `struct stat` and `linux_dirent64`. Layouts are documented in
//! `docs/filesystem.md`; the size/offset asserts below pin them.

use core::mem::{offset_of, size_of};

use super::node::{Stat, Timespec};

// ---- open(2) flags: arch/arm64/include/uapi/asm/fcntl.h + asm-generic --------------
// arm64 differs from asm-generic for DIRECTORY / NOFOLLOW / DIRECT / LARGEFILE.
pub const O_RDONLY: u32 = 0o0;
pub const O_WRONLY: u32 = 0o1;
pub const O_RDWR: u32 = 0o2;
pub const O_ACCMODE: u32 = 0o3;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_NOCTTY: u32 = 0o400;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DSYNC: u32 = 0o10000;
pub const O_DIRECTORY: u32 = 0o40000;
pub const O_NOFOLLOW: u32 = 0o100000;
pub const O_DIRECT: u32 = 0o200000;
pub const O_LARGEFILE: u32 = 0o400000;
pub const O_NOATIME: u32 = 0o1000000;
pub const O_CLOEXEC: u32 = 0o2000000;
pub const O_PATH: u32 = 0o10000000;

/// Flags `fcntl(F_SETFL)` may change on an open file.
pub const SETFL_MASK: u32 = O_APPEND | O_NONBLOCK | O_DIRECT | O_NOATIME;

// ---- lseek(2) ----------------------------------------------------------------------
pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

// ---- *at(2) ------------------------------------------------------------------------
pub const AT_FDCWD: i32 = -100;
pub const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
pub const AT_REMOVEDIR: u32 = 0x200;
pub const AT_SYMLINK_FOLLOW: u32 = 0x400;
pub const AT_EMPTY_PATH: u32 = 0x1000;

/// `struct stat` for aarch64 Linux (asm-generic/stat.h), 128 bytes. This is what
/// `fstat`/`newfstatat` (syscalls 80/79) copy out.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxStat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub __pad1: u64,
    pub st_size: i64,
    pub st_blksize: i32,
    pub __pad2: i32,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_atime_nsec: u64,
    pub st_mtime: i64,
    pub st_mtime_nsec: u64,
    pub st_ctime: i64,
    pub st_ctime_nsec: u64,
    pub __unused4: u32,
    pub __unused5: u32,
}

const _: () = {
    assert!(size_of::<LinuxStat>() == 128);
    assert!(offset_of!(LinuxStat, st_mode) == 16);
    assert!(offset_of!(LinuxStat, st_rdev) == 32);
    assert!(offset_of!(LinuxStat, st_size) == 48);
    assert!(offset_of!(LinuxStat, st_blksize) == 56);
    assert!(offset_of!(LinuxStat, st_blocks) == 64);
    assert!(offset_of!(LinuxStat, st_atime) == 72);
    assert!(offset_of!(LinuxStat, st_mtime) == 88);
    assert!(offset_of!(LinuxStat, st_ctime) == 104);
};

impl LinuxStat {
    /// The raw bytes, ready to copy to userspace.
    pub fn as_bytes(&self) -> &[u8; 128] {
        // SAFETY: repr(C), 128 bytes, no padding holes (all explicit), plain integers.
        unsafe { &*(self as *const LinuxStat as *const [u8; 128]) }
    }
}

/// `struct statfs` for aarch64 Linux (asm-generic/statfs.h, 64-bit: every field a
/// 64-bit word except the two-word fsid), 120 bytes. What `statfs`/`fstatfs` (syscalls
/// 43/44) copy out.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LinuxStatFs {
    pub f_type: i64,
    pub f_bsize: i64,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_fsid: [i32; 2],
    pub f_namelen: i64,
    pub f_frsize: i64,
    pub f_flags: i64,
    pub f_spare: [i64; 4],
}

const _: () = assert!(core::mem::size_of::<LinuxStatFs>() == 120);

impl LinuxStatFs {
    pub fn as_bytes(&self) -> &[u8; 120] {
        // SAFETY: repr(C), 120 bytes (asserted), no padding (8-byte fields, the fsid is
        // two 4-byte words at an 8-byte offset), plain integers.
        unsafe { &*(self as *const LinuxStatFs as *const [u8; 120]) }
    }
}

impl super::node::StatFs {
    pub fn to_linux(self) -> LinuxStatFs {
        LinuxStatFs {
            f_type: self.fs_type as i64,
            f_bsize: self.bsize as i64,
            f_blocks: self.blocks,
            f_bfree: self.bfree,
            f_bavail: self.bavail,
            f_files: self.files,
            f_ffree: self.ffree,
            f_fsid: [self.fsid as u32 as i32, (self.fsid >> 32) as u32 as i32],
            f_namelen: self.namelen as i64,
            f_frsize: self.bsize as i64,
            f_flags: self.flags as i64,
            f_spare: [0; 4],
        }
    }
}

impl Stat {
    pub fn to_linux(&self) -> LinuxStat {
        let t = |t: Timespec| (t.sec, t.nsec as u64);
        let (a, an) = t(self.atime);
        let (m, mn) = t(self.mtime);
        let (c, cn) = t(self.ctime);
        LinuxStat {
            st_dev: self.dev,
            st_ino: self.ino,
            st_mode: self.mode(),
            st_nlink: self.nlink,
            st_uid: self.uid,
            st_gid: self.gid,
            st_rdev: self.rdev,
            st_size: self.size as i64,
            st_blksize: self.blksize as i32,
            st_blocks: self.blocks as i64,
            st_atime: a,
            st_atime_nsec: an,
            st_mtime: m,
            st_mtime_nsec: mn,
            st_ctime: c,
            st_ctime_nsec: cn,
            ..LinuxStat::default()
        }
    }
}

/// Fixed part of `struct linux_dirent64`: `d_ino` u64 @0, `d_off` i64 @8, `d_reclen` u16
/// @16, `d_type` u8 @18, then the NUL-terminated name @19. Records are padded to 8 bytes.
pub const DIRENT64_HEADER: usize = 19;

/// Size of the `linux_dirent64` record for a name of `name_len` bytes.
pub const fn dirent64_reclen(name_len: usize) -> usize {
    (DIRENT64_HEADER + name_len + 1).next_multiple_of(8)
}

/// Encode one `linux_dirent64` at the start of `buf`. Returns the record length, or `None`
/// if it does not fit. `d_off` is the cookie to `lseek` to for the *next* entry.
pub fn put_dirent64(buf: &mut [u8], ino: u64, d_off: i64, d_type: u8, name: &str) -> Option<usize> {
    let reclen = dirent64_reclen(name.len());
    if reclen > buf.len() || reclen > u16::MAX as usize {
        return None;
    }
    let rec = &mut buf[..reclen];
    rec[0..8].copy_from_slice(&ino.to_ne_bytes());
    rec[8..16].copy_from_slice(&d_off.to_ne_bytes());
    rec[16..18].copy_from_slice(&(reclen as u16).to_ne_bytes());
    rec[18] = d_type;
    rec[19..19 + name.len()].copy_from_slice(name.as_bytes());
    rec[19 + name.len()..].fill(0); // NUL terminator + padding
    Some(reclen)
}

/// `ioctl(dst_fd, FICLONE, src_fd)`: make `dst`'s contents a copy-on-write copy of
/// `src`'s (Linux `_IOW(0x94, 9, int)`; milkfs only, see `fs::ops::ficlone`).
pub const FICLONE: u32 = 0x4004_9409;
