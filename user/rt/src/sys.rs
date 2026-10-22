//! Syscall wrappers (Linux aarch64 numbers; milkOS implements a subset).

use alloc::ffi::CString;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::fmt;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Errno(pub i32);

impl Errno {
    pub fn name(self) -> &'static str {
        match self.0 {
            1 => "EPERM",
            2 => "ENOENT",
            3 => "ESRCH",
            7 => "E2BIG",
            8 => "ENOEXEC",
            9 => "EBADF",
            10 => "ECHILD",
            11 => "EAGAIN",
            12 => "ENOMEM",
            13 => "EACCES",
            14 => "EFAULT",
            17 => "EEXIST",
            20 => "ENOTDIR",
            21 => "EISDIR",
            22 => "EINVAL",
            25 => "ENOTTY",
            36 => "ENAMETOOLONG",
            38 => "ENOSYS",
            16 => "EBUSY",
            18 => "EXDEV",
            19 => "ENODEV",
            23 => "ENFILE",
            24 => "EMFILE",
            27 => "EFBIG",
            28 => "ENOSPC",
            29 => "ESPIPE",
            30 => "EROFS",
            32 => "EPIPE",
            40 => "ELOOP",
            39 => "ENOTEMPTY",
            4 => "EINTR",
            88 => "ENOTSOCK",
            95 => "EOPNOTSUPP",
            97 => "EAFNOSUPPORT",
            98 => "EADDRINUSE",
            99 => "EADDRNOTAVAIL",
            101 => "ENETUNREACH",
            104 => "ECONNRESET",
            107 => "ENOTCONN",
            110 => "ETIMEDOUT",
            111 => "ECONNREFUSED",
            113 => "EHOSTUNREACH",
            125 => "ECANCELED",
            _ => "E?",
        }
    }
    pub fn msg(self) -> &'static str {
        match self.0 {
            2 => "no such file or directory",
            8 => "exec format error",
            13 => "permission denied",
            20 => "not a directory",
            21 => "is a directory",
            38 => "function not implemented",
            101 => "network is unreachable",
            104 => "connection reset by peer",
            110 => "connection timed out",
            111 => "connection refused",
            _ => self.name(),
        }
    }
}

impl fmt::Debug for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.name(), self.0)
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.msg())
    }
}

pub type Result<T> = core::result::Result<T, Errno>;

pub mod nr {
    pub const GETCWD: usize = 17;
    pub const MKDIRAT: usize = 34;
    pub const UNLINKAT: usize = 35;
    pub const SYMLINKAT: usize = 36;
    pub const LINKAT: usize = 37;
    pub const NEWFSTATAT: usize = 79;
    pub const STATFS: usize = 43;
    pub const FSTATFS: usize = 44;
    pub const UTIMENSAT: usize = 88;
    pub const RENAMEAT2: usize = 276;
    pub const CHDIR: usize = 49;
    pub const CHROOT: usize = 51;
    pub const DUP: usize = 23;
    pub const DUP3: usize = 24;
    pub const FCNTL: usize = 25;
    pub const OPENAT: usize = 56;
    pub const PIPE2: usize = 59;
    pub const CLOSE: usize = 57;
    pub const GETDENTS64: usize = 61;
    pub const LSEEK: usize = 62;
    pub const READ: usize = 63;
    pub const WRITE: usize = 64;
    pub const READLINKAT: usize = 78;
    pub const FSTAT: usize = 80;
    pub const EXIT: usize = 93;
    pub const NANOSLEEP: usize = 101;
    pub const CLOCK_GETTIME: usize = 113;
    pub const SCHED_YIELD: usize = 124;
    pub const UNAME: usize = 160;
    pub const SETHOSTNAME: usize = 161;
    pub const GETPID: usize = 172;
    pub const GETPPID: usize = 173;
    pub const BRK: usize = 214;
    pub const MUNMAP: usize = 215;
    pub const CLONE: usize = 220;
    pub const FUTEX: usize = 98;
    pub const MILK_SANDBOX: usize = 0x1000;
    pub const MILK_POWERBOX: usize = 0x1001;
    pub const EXECVE: usize = 221;
    pub const MMAP: usize = 222;
    pub const WAIT4: usize = 260;
    pub const IOCTL: usize = 29;
    pub const KILL: usize = 129;
    pub const RT_SIGACTION: usize = 134;
    pub const RT_SIGPROCMASK: usize = 135;
    pub const RT_SIGRETURN: usize = 139;
    pub const SETPGID: usize = 154;
    pub const GETPGID: usize = 155;
}

/// Raw syscall with up to six arguments.
///
/// # Safety
/// The arguments must be valid for syscall `n`.
#[inline(always)]
pub unsafe fn syscall(n: usize, a: [usize; 6]) -> isize {
    let r: isize;
    // SAFETY: caller's contract; svc clobbers only x0.
    unsafe {
        asm!("svc #0",
            inlateout("x0") a[0] => r,
            in("x1") a[1], in("x2") a[2], in("x3") a[3], in("x4") a[4], in("x5") a[5],
            in("x8") n,
            options(nostack));
    }
    r
}

fn sc(n: usize, a: [usize; 6]) -> Result<usize> {
    // SAFETY: every wrapper below passes valid pointers/lengths.
    let r = unsafe { syscall(n, a) };
    if (-4095..0).contains(&r) {
        Err(Errno(-r as i32))
    } else {
        Ok(r as usize)
    }
}

fn cs(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

pub fn read(fd: i32, buf: &mut [u8]) -> Result<usize> {
    sc(
        nr::READ,
        [fd as usize, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0],
    )
}

pub fn write(fd: i32, buf: &[u8]) -> Result<usize> {
    sc(
        nr::WRITE,
        [fd as usize, buf.as_ptr() as usize, buf.len(), 0, 0, 0],
    )
}

pub fn write_all(fd: i32, mut buf: &[u8]) -> Result<()> {
    while !buf.is_empty() {
        let n = write(fd, buf)?;
        if n == 0 {
            break;
        }
        buf = &buf[n..];
    }
    Ok(())
}

pub const O_RDONLY: u32 = 0;
pub const O_DIRECTORY: u32 = 0o40000;
pub const O_CLOEXEC: u32 = 0o2000000;
const AT_FDCWD: isize = -100;

pub fn open(path: &str, flags: u32) -> Result<i32> {
    let p = cs(path);
    sc(
        nr::OPENAT,
        [
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            flags as usize,
            0,
            0,
            0,
        ],
    )
    .map(|f| f as i32)
}

pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;

/// openat(AT_FDCWD, path, flags, mode).
pub fn open_mode(path: &str, flags: u32, mode: u32) -> Result<i32> {
    let p = cs(path);
    sc(
        nr::OPENAT,
        [
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            flags as usize,
            mode as usize,
            0,
            0,
        ],
    )
    .map(|f| f as i32)
}

/// The fields of `struct stat` the tools use.
#[derive(Clone, Copy, Default)]
pub struct Stat {
    /// The file system it is on (one number per mounted volume or snapshot).
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    /// Last read, last written, last changed (written, renamed, chmod...): seconds.
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

impl Stat {
    pub fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
    pub fn is_link(&self) -> bool {
        self.mode & 0o170000 == 0o120000
    }
    /// 'd', '-', 'l', 'c', 'b', 'p', 's'
    pub fn type_char(&self) -> char {
        match self.mode & 0o170000 {
            0o040000 => 'd',
            0o120000 => 'l',
            0o020000 => 'c',
            0o060000 => 'b',
            0o010000 => 'p',
            0o140000 => 's',
            _ => '-',
        }
    }
}

fn parse_stat(st: &[u8; 128]) -> Stat {
    let u32_at = |o: usize| u32::from_le_bytes(st[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(st[o..o + 8].try_into().unwrap());
    Stat {
        dev: u64_at(0),
        ino: u64_at(8),
        mode: u32_at(16),
        nlink: u32_at(20),
        uid: u32_at(24),
        gid: u32_at(28),
        rdev: u64_at(32),
        size: u64_at(48),
        atime: u64_at(72) as i64,
        mtime: u64_at(88) as i64,
        ctime: u64_at(104) as i64,
    }
}

/// stat (follow = true) or lstat (false).
pub fn stat_path(path: &str, follow: bool) -> Result<Stat> {
    let p = cs(path);
    let mut st = [0u8; 128];
    let flags = if follow { 0 } else { 0x100 };
    sc(
        nr::NEWFSTATAT,
        [
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            st.as_mut_ptr() as usize,
            flags,
            0,
            0,
        ],
    )?;
    Ok(parse_stat(&st))
}

pub fn stat_fd(fd: i32) -> Result<Stat> {
    let mut st = [0u8; 128];
    sc(
        nr::FSTAT,
        [fd as usize, st.as_mut_ptr() as usize, 0, 0, 0, 0],
    )?;
    Ok(parse_stat(&st))
}

/// What `statfs`/`fstatfs` say about a file system (Linux `struct statfs`, 120 bytes;
/// sizes in `bsize` units). See docs/filesystem.md "statfs".
#[derive(Clone, Copy, Debug, Default)]
pub struct StatFs {
    /// Magic number: 0x4d494c4b milkfs, 0x01021994 tmpfs, 0x4d44 FAT, 0x01021997 9p,
    /// 0x9fa0 proc, 0x62656572 sysfs, 0x1373 devfs.
    pub fs_type: u64,
    pub bsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub fsid: u64,
    pub namelen: u64,
    /// ST_RDONLY 1, ST_NOSUID 2, ST_NODEV 4, ST_NOEXEC 8, ST_VALID 0x20.
    pub flags: u64,
}

impl StatFs {
    pub fn read_only(&self) -> bool {
        self.flags & 1 != 0
    }
    /// The type's name, from the magic number.
    pub fn type_name(&self) -> &'static str {
        match self.fs_type {
            0x4d49_4c4b => "milkfs",
            0x0102_1994 => "tmpfs",
            0x4d44 => "vfat",
            0x0102_1997 => "9p",
            0x9fa0 => "proc",
            0x6265_6572 => "sysfs",
            0x1373 => "devfs",
            _ => "?",
        }
    }
}

fn parse_statfs(b: &[u8; 120]) -> StatFs {
    let w = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    StatFs {
        fs_type: w(0),
        bsize: w(1),
        blocks: w(2),
        bfree: w(3),
        bavail: w(4),
        files: w(5),
        ffree: w(6),
        fsid: w(7),
        namelen: w(8),
        flags: w(10),
    }
}

/// `statfs(2)`: the file system `path` is on.
pub fn statfs(path: &str) -> Result<StatFs> {
    let p = cs(path);
    let mut b = [0u8; 120];
    sc(
        nr::STATFS,
        [p.as_ptr() as usize, b.as_mut_ptr() as usize, 0, 0, 0, 0],
    )?;
    Ok(parse_statfs(&b))
}

/// `fstatfs(2)`.
pub fn fstatfs(fd: i32) -> Result<StatFs> {
    let mut b = [0u8; 120];
    sc(
        nr::FSTATFS,
        [fd as usize, b.as_mut_ptr() as usize, 0, 0, 0, 0],
    )?;
    Ok(parse_statfs(&b))
}

pub fn unlink(path: &str) -> Result<()> {
    let p = cs(path);
    sc(
        nr::UNLINKAT,
        [AT_FDCWD as usize, p.as_ptr() as usize, 0, 0, 0, 0],
    )
    .map(|_| ())
}

pub fn rmdir(path: &str) -> Result<()> {
    let p = cs(path);
    sc(
        nr::UNLINKAT,
        [AT_FDCWD as usize, p.as_ptr() as usize, 0x200, 0, 0, 0],
    )
    .map(|_| ())
}

pub fn rename(old: &str, new: &str) -> Result<()> {
    rename_flags(old, new, 0)
}

/// `renameat2(..., RENAME_NOREPLACE)`: like `rename`, but EEXIST instead of replacing
/// something already at `new`.
pub fn rename_noreplace(old: &str, new: &str) -> Result<()> {
    rename_flags(old, new, 1)
}

fn rename_flags(old: &str, new: &str, flags: usize) -> Result<()> {
    let (o, n) = (cs(old), cs(new));
    sc(
        nr::RENAMEAT2,
        [
            AT_FDCWD as usize,
            o.as_ptr() as usize,
            AT_FDCWD as usize,
            n.as_ptr() as usize,
            flags,
            0,
        ],
    )
    .map(|_| ())
}

pub fn symlink(target: &str, path: &str) -> Result<()> {
    let (t, p) = (cs(target), cs(path));
    sc(
        nr::SYMLINKAT,
        [
            t.as_ptr() as usize,
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            0,
            0,
            0,
        ],
    )
    .map(|_| ())
}

pub fn link(old: &str, new: &str) -> Result<()> {
    let (o, n) = (cs(old), cs(new));
    sc(
        nr::LINKAT,
        [
            AT_FDCWD as usize,
            o.as_ptr() as usize,
            AT_FDCWD as usize,
            n.as_ptr() as usize,
            0,
            0,
        ],
    )
    .map(|_| ())
}

/// Set atime and mtime to now (touch).
pub fn touch_now(path: &str) -> Result<()> {
    let p = cs(path);
    sc(
        nr::UTIMENSAT,
        [AT_FDCWD as usize, p.as_ptr() as usize, 0, 0, 0, 0],
    )
    .map(|_| ())
}

/// (rows, cols) of the terminal on `fd`, if it is one.
pub fn winsize(fd: i32) -> Option<(u16, u16)> {
    let mut ws = [0u16; 4];
    sc(
        nr::IOCTL,
        [fd as usize, 0x5413, ws.as_mut_ptr() as usize, 0, 0, 0],
    )
    .ok()?;
    (ws[1] > 0).then_some((ws[0], ws[1]))
}

/// `ioctl(dst, FICLONE, src)`: `dst` (open for writing) becomes a copy-on-write copy of
/// `src` (open for reading), sharing its blocks. milkfs only (EXDEV across volumes,
/// EOPNOTSUPP elsewhere).
pub fn ficlone(src: i32, dst: i32) -> Result<()> {
    sc(
        nr::IOCTL,
        [dst as usize, 0x4004_9409, src as usize, 0, 0, 0],
    )
    .map(|_| ())
}

/// Terminal width for laying out columns: TIOCGWINSZ on stdout, else 80.
pub fn term_cols() -> usize {
    winsize(1).map_or(80, |(_, c)| c as usize)
}

pub fn mkdir(path: &str, mode: u32) -> Result<()> {
    let p = cs(path);
    sc(
        nr::MKDIRAT,
        [
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            mode as usize,
            0,
            0,
            0,
        ],
    )
    .map(|_| ())
}

/// pipe2(2): (read end, write end).
pub fn pipe2(flags: u32) -> Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    sc(
        nr::PIPE2,
        [fds.as_mut_ptr() as usize, flags as usize, 0, 0, 0, 0],
    )?;
    Ok((fds[0], fds[1]))
}

pub fn pipe() -> Result<(i32, i32)> {
    pipe2(O_CLOEXEC)
}

pub fn dup(fd: i32) -> Result<i32> {
    sc(nr::DUP, [fd as usize, 0, 0, 0, 0, 0]).map(|f| f as i32)
}

/// dup2 via dup3 (no flags: the new fd is not close-on-exec). Same fd: no-op.
pub fn dup2(old: i32, new: i32) -> Result<i32> {
    if old == new {
        return Ok(new);
    }
    sc(nr::DUP3, [old as usize, new as usize, 0, 0, 0, 0]).map(|f| f as i32)
}

/// fcntl(F_DUPFD_CLOEXEC): a close-on-exec copy of `fd` at the lowest fd >= `min`.
pub fn fcntl_dupfd(fd: i32, min: i32) -> Result<i32> {
    sc(nr::FCNTL, [fd as usize, 1030, min as usize, 0, 0, 0]).map(|f| f as i32)
}

pub fn close(fd: i32) -> Result<()> {
    sc(nr::CLOSE, [fd as usize, 0, 0, 0, 0, 0]).map(|_| ())
}

pub fn lseek(fd: i32, off: i64, whence: u32) -> Result<u64> {
    sc(
        nr::LSEEK,
        [fd as usize, off as usize, whence as usize, 0, 0, 0],
    )
    .map(|v| v as u64)
}

/// (st_mode, st_size) from fstat.
pub fn fstat(fd: i32) -> Result<(u32, u64)> {
    let mut st = [0u8; 128];
    sc(
        nr::FSTAT,
        [fd as usize, st.as_mut_ptr() as usize, 0, 0, 0, 0],
    )?;
    Ok((
        u32::from_le_bytes(st[16..20].try_into().unwrap()),
        u64::from_le_bytes(st[48..56].try_into().unwrap()),
    ))
}

/// One directory entry from getdents64: (name, d_type).
pub fn read_dir(path: &str) -> Result<Vec<(String, u8)>> {
    let fd = open(path, O_RDONLY | O_DIRECTORY | O_CLOEXEC)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = match sc(
            nr::GETDENTS64,
            [fd as usize, buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0],
        ) {
            Ok(n) => n,
            Err(e) => {
                let _ = close(fd);
                return Err(e);
            }
        };
        if n == 0 {
            break;
        }
        let mut off = 0;
        while off < n {
            let reclen = u16::from_le_bytes([buf[off + 16], buf[off + 17]]) as usize;
            let dtype = buf[off + 18];
            let name = &buf[off + 19..off + reclen];
            let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            out.push((String::from_utf8_lossy(&name[..len]).into_owned(), dtype));
            off += reclen;
        }
    }
    let _ = close(fd);
    Ok(out)
}

pub fn readlink(path: &str) -> Result<String> {
    let p = cs(path);
    let mut b = [0u8; 512];
    let n = sc(
        nr::READLINKAT,
        [
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            b.as_mut_ptr() as usize,
            b.len(),
            0,
            0,
        ],
    )?;
    Ok(String::from_utf8_lossy(&b[..n]).into_owned())
}

pub fn getcwd() -> Result<String> {
    let mut b = [0u8; 512];
    let n = sc(nr::GETCWD, [b.as_mut_ptr() as usize, b.len(), 0, 0, 0, 0])?;
    Ok(String::from_utf8_lossy(&b[..n.saturating_sub(1)]).into_owned())
}

/// Shrink this process's capabilities from a spec string (see /System/Developer/sandbox). Only ever
/// drops capabilities; takes effect for this process and everything it execs/forks.
pub fn sandbox(spec: &str) -> Result<()> {
    let s = cs(spec);
    sc(nr::MILK_SANDBOX, [s.as_ptr() as usize, 0, 0, 0, 0, 0]).map(|_| ())
}

/// Powerbox: ask the user, through the desktop's own Open panel, for a file to read. Only
/// the chosen file is granted to this process; returns it opened read-only. ECANCELED if
/// the user cancels. Blocks until they decide.
pub fn powerbox_open() -> Result<i32> {
    sc(nr::MILK_POWERBOX, [0, 0, 0, 0, 0, 0]).map(|fd| fd as i32)
}

/// Powerbox Save panel (`name` is a suggestion the user can change): the chosen file is
/// granted read+write, created or truncated, and returned opened read-write.
pub fn powerbox_save(name: &str) -> Result<i32> {
    sc(
        nr::MILK_POWERBOX,
        [1, name.as_ptr() as usize, name.len(), 0, 0, 0],
    )
    .map(|fd| fd as i32)
}

pub fn chroot(path: &str) -> Result<()> {
    let p = cs(path);
    sc(nr::CHROOT, [p.as_ptr() as usize, 0, 0, 0, 0, 0]).map(|_| ())
}

pub fn chdir(path: &str) -> Result<()> {
    let p = cs(path);
    sc(nr::CHDIR, [p.as_ptr() as usize, 0, 0, 0, 0, 0]).map(|_| ())
}

pub fn exit(code: i32) -> ! {
    // SAFETY: exit never returns.
    unsafe { syscall(nr::EXIT, [code as usize, 0, 0, 0, 0, 0]) };
    unreachable!()
}

pub fn getpid() -> u32 {
    sc(nr::GETPID, [0; 6]).unwrap_or(0) as u32
}

pub fn getppid() -> u32 {
    sc(nr::GETPPID, [0; 6]).unwrap_or(0) as u32
}

pub fn sched_yield() {
    let _ = sc(nr::SCHED_YIELD, [0; 6]);
}

pub fn nanosleep(ns: u64) -> Result<()> {
    let ts = [ns / 1_000_000_000, ns % 1_000_000_000];
    sc(nr::NANOSLEEP, [ts.as_ptr() as usize, 0, 0, 0, 0, 0]).map(|_| ())
}

pub fn sleep_ms(ms: u64) {
    let _ = nanosleep(ms * 1_000_000);
}

pub const CLOCK_REALTIME: u32 = 0;
pub const CLOCK_MONOTONIC: u32 = 1;

pub fn clock_ns(clock: u32) -> u64 {
    let mut ts = [0u64; 2];
    let _ = sc(
        nr::CLOCK_GETTIME,
        [clock as usize, ts.as_mut_ptr() as usize, 0, 0, 0, 0],
    );
    ts[0] * 1_000_000_000 + ts[1]
}

/// uname(2): [sysname, nodename, release, version, machine].
pub fn uname() -> Result<[String; 5]> {
    let mut u = [0u8; 65 * 6];
    sc(nr::UNAME, [u.as_mut_ptr() as usize, 0, 0, 0, 0, 0])?;
    let f = |i: usize| {
        let s = &u[i * 65..(i + 1) * 65];
        let n = s.iter().position(|&b| b == 0).unwrap_or(65);
        String::from_utf8_lossy(&s[..n]).into_owned()
    };
    Ok([f(0), f(1), f(2), f(3), f(4)])
}

/// sethostname(2) (root only; milkOS: one DNS label, 1-63 letters, digits and '-').
pub fn sethostname(name: &str) -> Result<()> {
    sc(
        nr::SETHOSTNAME,
        [name.as_ptr() as usize, name.len(), 0, 0, 0, 0],
    )
    .map(|_| ())
}

pub fn brk(addr: usize) -> usize {
    sc(nr::BRK, [addr, 0, 0, 0, 0, 0]).unwrap_or(0)
}

pub const FUTEX_WAIT: u32 = 0;
pub const FUTEX_WAKE: u32 = 1;
pub const FUTEX_PRIVATE: u32 = 128;

/// futex(uaddr, FUTEX_WAIT_PRIVATE, val, timeout): sleep while *uaddr == val.
pub fn futex_wait(uaddr: usize, val: u32, timeout_ns: Option<u64>) -> Result<()> {
    let ts = timeout_ns.map(|ns| [ns / 1_000_000_000, ns % 1_000_000_000]);
    let tp = ts.as_ref().map_or(0, |t| t.as_ptr() as usize);
    sc(
        nr::FUTEX,
        [
            uaddr,
            (FUTEX_WAIT | FUTEX_PRIVATE) as usize,
            val as usize,
            tp,
            0,
            0,
        ],
    )
    .map(|_| ())
}

/// futex(uaddr, FUTEX_WAKE_PRIVATE, n): wake up to `n` waiters. Returns the number woken.
pub fn futex_wake(uaddr: usize, n: usize) -> Result<usize> {
    sc(
        nr::FUTEX,
        [uaddr, (FUTEX_WAKE | FUTEX_PRIVATE) as usize, n, 0, 0, 0],
    )
}

pub fn mmap_anon(len: usize) -> Result<*mut u8> {
    const PROT_RW: usize = 3;
    const MAP_PRIVATE_ANON: usize = 0x22;
    sc(nr::MMAP, [0, len, PROT_RW, MAP_PRIVATE_ANON, usize::MAX, 0]).map(|a| a as *mut u8)
}

/// Anonymous private mapping with protection `prot` (PROT_READ 1 | WRITE 2 | EXEC 4).
pub fn mmap_prot(len: usize, prot: u32) -> Result<*mut u8> {
    const MAP_PRIVATE_ANON: usize = 0x22;
    sc(
        nr::MMAP,
        [0, len, prot as usize, MAP_PRIVATE_ANON, usize::MAX, 0],
    )
    .map(|a| a as *mut u8)
}

/// mprotect(2) on `[addr, addr+len)`.
pub fn mprotect(addr: usize, len: usize, prot: u32) -> Result<()> {
    sc(226, [addr, len, prot as usize, 0, 0, 0]).map(|_| ())
}

pub fn munmap(p: *mut u8, len: usize) -> Result<()> {
    sc(nr::MUNMAP, [p as usize, len, 0, 0, 0, 0]).map(|_| ())
}

/// fork(): Ok(0) in the child, Ok(child pid) in the parent.
pub fn fork() -> Result<u32> {
    const SIGCHLD: usize = 17;
    sc(nr::CLONE, [SIGCHLD, 0, 0, 0, 0, 0]).map(|p| p as u32)
}

/// execve(2). Only returns on failure.
pub fn execve(path: &str, argv: &[&str], envp: &[&str]) -> Errno {
    let p = cs(path);
    let a: Vec<CString> = argv.iter().map(|s| cs(s)).collect();
    let e: Vec<CString> = envp.iter().map(|s| cs(s)).collect();
    let mut ap: Vec<usize> = a.iter().map(|c| c.as_ptr() as usize).collect();
    ap.push(0);
    let mut ep: Vec<usize> = e.iter().map(|c| c.as_ptr() as usize).collect();
    ep.push(0);
    match sc(
        nr::EXECVE,
        [
            p.as_ptr() as usize,
            ap.as_ptr() as usize,
            ep.as_ptr() as usize,
            0,
            0,
            0,
        ],
    ) {
        Err(e) => e,
        Ok(_) => Errno(0),
    }
}

pub const WNOHANG: usize = 1;

/// wait4(pid, &status, options): Ok((pid, wait status)); pid 0 with WNOHANG = none yet.
pub fn wait4(pid: i32, options: usize) -> Result<(u32, i32)> {
    let mut st = 0i32;
    let p = sc(
        nr::WAIT4,
        [
            pid as isize as usize,
            &mut st as *mut i32 as usize,
            options,
            0,
            0,
            0,
        ],
    )?;
    Ok((p as u32, st))
}

/// Describe a wait status like a shell would.
pub fn describe_status(st: i32) -> String {
    use alloc::format;
    if st & 0x7f == 0 {
        format!("exit {}", (st >> 8) & 0xff)
    } else {
        let sig = st & 0x7f;
        let name = match sig {
            4 => "SIGILL",
            5 => "SIGTRAP",
            7 => "SIGBUS",
            8 => "SIGFPE",
            9 => "SIGKILL",
            11 => "SIGSEGV",
            _ => "signal",
        };
        let core = if st & 0x80 != 0 { ", core dumped" } else { "" };
        format!("killed by {name} ({sig}{core})")
    }
}

/// fork + execve. The child exits 127 if exec fails.
pub fn spawn(path: &str, argv: &[&str], envp: &[&str]) -> Result<u32> {
    match fork()? {
        0 => {
            let e = execve(path, argv, envp);
            crate::eprintln!("{}: {}", argv.first().copied().unwrap_or(path), e);
            exit(127)
        }
        pid => Ok(pid),
    }
}

/// Read a whole (small) file, e.g. under /proc.
pub fn read_file(path: &str) -> Result<Vec<u8>> {
    let fd = open(path, O_RDONLY | O_CLOEXEC)?;
    let mut out = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => {
                let _ = close(fd);
                return Err(e);
            }
        }
    }
    let _ = close(fd);
    Ok(out)
}

/// One process (from /proc/<pid>/status) or kernel thread (from /proc/threads).
pub struct PsEntry {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub tid: u32,
    pub state: char,
    pub kernel: bool,
    pub cpu_ns: u64,
    pub rss: u64,
    pub name: String,
    /// CPU it runs on / last ran on.
    pub psr: u32,
}

/// Processes from /proc; with `kernel_threads`, also the kernel's own threads.
pub fn ps(kernel_threads: bool) -> Result<Vec<PsEntry>> {
    let mut out = Vec::new();
    let mut pids: Vec<u32> = read_dir("/proc")?
        .into_iter()
        .filter_map(|(n, _)| n.parse().ok())
        .collect();
    pids.sort_unstable();
    for pid in pids {
        let Ok(st) = read_file(&alloc::format!("/proc/{pid}/status")) else {
            continue; // exited meanwhile
        };
        let st = String::from_utf8_lossy(&st);
        let field = |k: &str| {
            st.lines()
                .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix(':')))
                .map(|v| v.trim())
                .unwrap_or("")
        };
        let num = |k: &str| {
            field(k)
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0u64)
        };
        out.push(PsEntry {
            pid,
            ppid: num("PPid") as u32,
            pgid: num("Pgid") as u32,
            tid: num("Tid") as u32,
            state: field("State").chars().next().unwrap_or('?'),
            kernel: false,
            cpu_ns: num("CpuNs"),
            rss: num("VmRSS") * 1024,
            name: String::from(field("Name")),
            psr: num("Cpu") as u32,
        });
    }
    if kernel_threads {
        let t = read_file("/proc/threads")?;
        for l in String::from_utf8_lossy(&t).lines().skip(1) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 7 || f[1] != "0" {
                continue;
            }
            out.push(PsEntry {
                pid: 0,
                ppid: 0,
                pgid: 0,
                tid: f[0].parse().unwrap_or(0),
                state: f[2].chars().next().unwrap_or('?'),
                kernel: true,
                cpu_ns: f[5].parse().unwrap_or(0),
                rss: 0,
                name: String::from(f[6]),
                psr: f[4].parse().unwrap_or(0),
            });
        }
    }
    Ok(out)
}

// ---- signals -------------------------------------------------------------------------

pub const SIGHUP: i32 = 1;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGKILL: i32 = 9;
pub const SIGUSR1: i32 = 10;
pub const SIGSEGV: i32 = 11;
pub const SIGUSR2: i32 = 12;
pub const SIGPIPE: i32 = 13;
pub const SIGTERM: i32 = 15;
pub const SIGCHLD: i32 = 17;
pub const SIG_DFL: usize = 0;
pub const SIG_IGN: usize = 1;
const SA_RESTORER: usize = 0x0400_0000;
const SA_SIGINFO: usize = 4;

/// Signal names for kill -l and friends.
pub fn signal_name(sig: i32) -> &'static str {
    match sig {
        1 => "HUP",
        2 => "INT",
        3 => "QUIT",
        4 => "ILL",
        5 => "TRAP",
        6 => "ABRT",
        7 => "BUS",
        8 => "FPE",
        9 => "KILL",
        10 => "USR1",
        11 => "SEGV",
        12 => "USR2",
        13 => "PIPE",
        14 => "ALRM",
        15 => "TERM",
        17 => "CHLD",
        18 => "CONT",
        19 => "STOP",
        20 => "TSTP",
        28 => "WINCH",
        _ => "?",
    }
}

pub fn signal_number(name: &str) -> Option<i32> {
    let n = name.trim_start_matches("SIG");
    if let Ok(v) = n.parse() {
        return Some(v);
    }
    (1..32).find(|&s| signal_name(s) == n)
}

/// Every handler returns here; the kernel's rt_sigreturn restores the interrupted state.
#[unsafe(naked)]
extern "C" fn __rt_sigreturn() {
    core::arch::naked_asm!("mov x8, #139", "svc #0", "brk #1");
}

/// sigaction with a plain handler address (or SIG_DFL / SIG_IGN). Returns the old one.
pub fn sigaction(sig: i32, handler: usize) -> Result<usize> {
    let act = [
        handler,
        SA_SIGINFO | SA_RESTORER,
        __rt_sigreturn as *const () as usize,
        0usize,
    ];
    let mut old = [0usize; 4];
    sc(
        nr::RT_SIGACTION,
        [
            sig as usize,
            act.as_ptr() as usize,
            old.as_mut_ptr() as usize,
            8,
            0,
            0,
        ],
    )?;
    Ok(old[0])
}

/// Install `h` as the handler for `sig`.
pub fn signal(sig: i32, h: extern "C" fn(i32)) -> Result<usize> {
    sigaction(sig, h as *const () as usize)
}

pub const SIG_BLOCK: usize = 0;
pub const SIG_UNBLOCK: usize = 1;
pub const SIG_SETMASK: usize = 2;

pub fn sigprocmask(how: usize, set: u64) -> Result<u64> {
    let mut old = 0u64;
    sc(
        nr::RT_SIGPROCMASK,
        [
            how,
            &set as *const u64 as usize,
            &mut old as *mut u64 as usize,
            8,
            0,
            0,
        ],
    )?;
    Ok(old)
}

pub fn sigmask(sig: i32) -> u64 {
    1 << (sig - 1)
}

pub fn kill(pid: i32, sig: i32) -> Result<()> {
    sc(nr::KILL, [pid as isize as usize, sig as usize, 0, 0, 0, 0]).map(|_| ())
}

pub fn setpgid(pid: u32, pgid: u32) -> Result<()> {
    sc(nr::SETPGID, [pid as usize, pgid as usize, 0, 0, 0, 0]).map(|_| ())
}

pub fn getpgid(pid: u32) -> Result<u32> {
    sc(nr::GETPGID, [pid as usize, 0, 0, 0, 0, 0]).map(|p| p as u32)
}

const TIOCGPGRP: usize = 0x540f;
const TIOCSPGRP: usize = 0x5410;
const TIOCGWINSZ: usize = 0x5413;

/// Make process group `pg` the foreground group of the terminal on `fd`.
pub fn tcsetpgrp(fd: i32, pg: u32) -> Result<()> {
    sc(
        nr::IOCTL,
        [fd as usize, TIOCSPGRP, &pg as *const u32 as usize, 0, 0, 0],
    )
    .map(|_| ())
}

pub fn tcgetpgrp(fd: i32) -> Result<u32> {
    let mut pg = 0u32;
    sc(
        nr::IOCTL,
        [
            fd as usize,
            TIOCGPGRP,
            &mut pg as *mut u32 as usize,
            0,
            0,
            0,
        ],
    )?;
    Ok(pg)
}

/// Is `fd` a terminal?
pub fn isatty(fd: i32) -> bool {
    let mut ws = [0u16; 4];
    sc(
        nr::IOCTL,
        [fd as usize, TIOCGWINSZ, ws.as_mut_ptr() as usize, 0, 0, 0],
    )
    .is_ok()
}

/// ioctl(2) with an integer (or pointer) argument.
pub fn ioctl(fd: i32, cmd: u32, arg: usize) -> Result<usize> {
    sc(29, [fd as usize, cmd as usize, arg, 0, 0, 0])
}

// ---- users and groups (kernel/src/proc/cred.rs) ----------------------------------------

pub fn getuid() -> u32 {
    sc(174, [0; 6]).unwrap_or(0) as u32
}
pub fn geteuid() -> u32 {
    sc(175, [0; 6]).unwrap_or(0) as u32
}
pub fn getgid() -> u32 {
    sc(176, [0; 6]).unwrap_or(0) as u32
}
pub fn getegid() -> u32 {
    sc(177, [0; 6]).unwrap_or(0) as u32
}
pub fn setuid(uid: u32) -> Result<()> {
    sc(146, [uid as usize, 0, 0, 0, 0, 0]).map(|_| ())
}
pub fn setgid(gid: u32) -> Result<()> {
    sc(144, [gid as usize, 0, 0, 0, 0, 0]).map(|_| ())
}
/// `u32::MAX` leaves a field unchanged.
pub fn setresuid(r: u32, e: u32, s: u32) -> Result<()> {
    sc(147, [r as usize, e as usize, s as usize, 0, 0, 0]).map(|_| ())
}
pub fn setresgid(r: u32, e: u32, s: u32) -> Result<()> {
    sc(149, [r as usize, e as usize, s as usize, 0, 0, 0]).map(|_| ())
}
/// (real, effective, saved) user ids.
pub fn getresuid() -> (u32, u32, u32) {
    let mut v = [0u32; 3];
    let p = v.as_mut_ptr() as usize;
    let _ = sc(148, [p, p + 4, p + 8, 0, 0, 0]);
    (v[0], v[1], v[2])
}
pub fn getgroups() -> Vec<u32> {
    let mut g = [0u32; 64];
    let n = sc(158, [g.len(), g.as_mut_ptr() as usize, 0, 0, 0, 0]).unwrap_or(0);
    g[..n.min(64)].to_vec()
}
pub fn setgroups(groups: &[u32]) -> Result<()> {
    sc(159, [groups.len(), groups.as_ptr() as usize, 0, 0, 0, 0]).map(|_| ())
}

/// chmod(2) (fchmodat, AT_FDCWD).
pub fn chmod(path: &str, mode: u32) -> Result<()> {
    let p = cs(path);
    sc(
        53,
        [
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            mode as usize,
            0,
            0,
            0,
        ],
    )
    .map(|_| ())
}

/// chown(2) (fchownat, AT_FDCWD); `u32::MAX` leaves uid or gid unchanged.
pub fn chown(path: &str, uid: u32, gid: u32) -> Result<()> {
    let p = cs(path);
    sc(
        54,
        [
            AT_FDCWD as usize,
            p.as_ptr() as usize,
            uid as usize,
            gid as usize,
            0,
            0,
        ],
    )
    .map(|_| ())
}

/// Fill `buf` from the kernel's random source (getrandom(2)).
pub fn getrandom(buf: &mut [u8]) -> Result<usize> {
    sc(278, [buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0, 0])
}

/// Turn terminal echo on `fd` on or off. Returns the previous state (None: not a tty).
pub fn set_echo(fd: i32, on: bool) -> Option<bool> {
    const TCGETS: u32 = 0x5401;
    const TCSETS: u32 = 0x5402;
    const ECHO: u32 = 0o10;
    let mut t = [0u8; 36];
    ioctl(fd, TCGETS, t.as_mut_ptr() as usize).ok()?;
    let lflag = u32::from_le_bytes([t[12], t[13], t[14], t[15]]);
    let was = lflag & ECHO != 0;
    let new = if on { lflag | ECHO } else { lflag & !ECHO };
    t[12..16].copy_from_slice(&new.to_le_bytes());
    ioctl(fd, TCSETS, t.as_mut_ptr() as usize).ok()?;
    Some(was)
}

// ---- admin authorisation tickets (kernel/src/proc/auth.rs) ------------------------------

const MILK_AUTH: usize = 0x1003;

/// Ask the desktop's trusted password panel to authorise this process (5 minutes).
/// Ok(ms the ticket lasts); ECANCELED if the user cancelled, EOPNOTSUPP without a desktop.
pub fn authorize(reason: &str) -> Result<u64> {
    sc(
        MILK_AUTH,
        [0, reason.as_ptr() as usize, reason.len(), 0, 0, 0],
    )
    .map(|ms| ms as u64)
}

/// Authorise this process with a password it read itself (a terminal tool); `user` empty =
/// the caller's own account. EACCES wrong password, EPERM not an administrator, EAGAIN
/// locked out after too many wrong passwords.
pub fn authorize_password(password: &str, user: &str) -> Result<u64> {
    sc(
        MILK_AUTH,
        [
            1,
            password.as_ptr() as usize,
            password.len(),
            user.as_ptr() as usize,
            user.len(),
            0,
        ],
    )
    .map(|ms| ms as u64)
}

/// Drop this process's ticket.
pub fn auth_revoke() {
    let _ = sc(MILK_AUTH, [2, 0, 0, 0, 0, 0]);
}

/// Milliseconds left on this process's ticket (0: none).
pub fn auth_left() -> u64 {
    sc(MILK_AUTH, [3, 0, 0, 0, 0, 0]).unwrap_or(0) as u64
}

// ---- user-space drivers (docs/drivers.md) -----------------------------------------------

const MILK_DRIVER: usize = 0x1010;

/// Claim the first virtio device with this id (4 rng, 25 sound) as a user-space driver
/// (root + the `driver` capability). Returns the device fd: read 8 bytes to wait for its
/// interrupt, poll for POLLIN.
pub fn driver_claim(virtio_id: u32) -> Result<i32> {
    sc(MILK_DRIVER, [0, virtio_id as usize, 0, 0, 0, 0]).map(|fd| fd as i32)
}

/// Read a register (`width` 1, 2 or 4 bytes) in the device's virtio-mmio window.
pub fn driver_read(fd: i32, off: usize, width: usize) -> Result<u32> {
    sc(MILK_DRIVER, [1, fd as usize, off, width, 0, 0]).map(|v| v as u32)
}

pub fn driver_write(fd: i32, off: usize, v: u32, width: usize) -> Result<()> {
    sc(MILK_DRIVER, [2, fd as usize, off, v as usize, width, 0]).map(|_| ())
}

/// Zeroed, physically contiguous DMA memory: (virtual address, physical address).
pub fn driver_dma(fd: i32, bytes: usize) -> Result<(usize, u64)> {
    let mut pa = 0u64;
    let va = sc(
        MILK_DRIVER,
        [3, fd as usize, bytes, &mut pa as *mut u64 as usize, 0, 0],
    )?;
    Ok((va, pa))
}

/// Unmask the device's interrupt (after acknowledging it at the device).
pub fn driver_arm(fd: i32) -> Result<()> {
    sc(MILK_DRIVER, [4, fd as usize, 0, 0, 0, 0]).map(|_| ())
}

/// `slot=N irq=N base=0x... id=N`.
pub fn driver_info(fd: i32) -> String {
    let mut b = [0u8; 128];
    match sc(
        MILK_DRIVER,
        [5, fd as usize, b.as_mut_ptr() as usize, b.len(), 0, 0],
    ) {
        Ok(n) => String::from_utf8_lossy(&b[..n]).into_owned(),
        Err(_) => String::new(),
    }
}
