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
            39 => "ENOTEMPTY",
            32 => "EPIPE",
            88 => "ENOTSOCK",
            97 => "EAFNOSUPPORT",
            98 => "EADDRINUSE",
            99 => "EADDRNOTAVAIL",
            101 => "ENETUNREACH",
            104 => "ECONNRESET",
            107 => "ENOTCONN",
            110 => "ETIMEDOUT",
            111 => "ECONNREFUSED",
            113 => "EHOSTUNREACH",
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
    pub const CHDIR: usize = 49;
    pub const OPENAT: usize = 56;
    pub const CLOSE: usize = 57;
    pub const GETDENTS64: usize = 61;
    pub const LSEEK: usize = 62;
    pub const READ: usize = 63;
    pub const WRITE: usize = 64;
    pub const FSTAT: usize = 80;
    pub const EXIT: usize = 93;
    pub const NANOSLEEP: usize = 101;
    pub const CLOCK_GETTIME: usize = 113;
    pub const SCHED_YIELD: usize = 124;
    pub const UNAME: usize = 160;
    pub const GETPID: usize = 172;
    pub const GETPPID: usize = 173;
    pub const BRK: usize = 214;
    pub const MUNMAP: usize = 215;
    pub const CLONE: usize = 220;
    pub const EXECVE: usize = 221;
    pub const MMAP: usize = 222;
    pub const WAIT4: usize = 260;
    pub const MILK_PS: usize = 4096;
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

pub fn getcwd() -> Result<String> {
    let mut b = [0u8; 512];
    let n = sc(nr::GETCWD, [b.as_mut_ptr() as usize, b.len(), 0, 0, 0, 0])?;
    Ok(String::from_utf8_lossy(&b[..n.saturating_sub(1)]).into_owned())
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

pub fn brk(addr: usize) -> usize {
    sc(nr::BRK, [addr, 0, 0, 0, 0, 0]).unwrap_or(0)
}

pub fn mmap_anon(len: usize) -> Result<*mut u8> {
    const PROT_RW: usize = 3;
    const MAP_PRIVATE_ANON: usize = 0x22;
    sc(nr::MMAP, [0, len, PROT_RW, MAP_PRIVATE_ANON, usize::MAX, 0]).map(|a| a as *mut u8)
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
        format!("killed by {name} ({sig})")
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

/// One process/thread from the kernel's MILK_PS call.
pub struct PsEntry {
    pub pid: u32,
    pub ppid: u32,
    pub tid: u32,
    pub state: char,
    pub kernel: bool,
    pub cpu_ns: u64,
    pub rss: u64,
    pub name: String,
}

pub fn ps() -> Result<Vec<PsEntry>> {
    let mut buf = alloc::vec![0u8; 48 * 256];
    let n = sc(
        nr::MILK_PS,
        [buf.as_mut_ptr() as usize, buf.len(), 0, 0, 0, 0],
    )?;
    Ok(buf[..n * 48]
        .chunks(48)
        .map(|r| {
            let u32_at = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
            let u64_at = |o: usize| u64::from_le_bytes(r[o..o + 8].try_into().unwrap());
            let name = &r[32..48];
            let len = name.iter().position(|&b| b == 0).unwrap_or(16);
            PsEntry {
                pid: u32_at(0),
                ppid: u32_at(4),
                tid: u32_at(8),
                state: r[12] as char,
                kernel: r[13] != 0,
                cpu_ns: u64_at(16),
                rss: u64_at(24),
                name: String::from_utf8_lossy(&name[..len]).into_owned(),
            }
        })
        .collect())
}
