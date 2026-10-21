//! System calls: Linux aarch64 ABI and numbers. `svc #0`, number in x8, arguments in
//! x0..x5, result in x0 (negative errno on failure).
//!
//! Syscalls run on the calling thread's kernel stack with IRQs enabled (so they can be
//! preempted and can block); a pending reschedule is honoured on the way back to EL0.
//! User memory is only touched through `UserSpace::copy_in/copy_out`, which validate it.
//! Unknown numbers return ENOSYS and are logged once per number.
//! There are no milkOS-specific syscalls: process information comes from /proc.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::current;
use super::errno::*;
use super::signal;
use super::uspace::{Perm, UserSpace};
use crate::exceptions::{self, TrapFrame};
use crate::fs::{self, InodeRef, InodeType, OpenFile, abi};
use crate::sched;
use crate::{kprintln, sync, timer};

pub mod nr {
    pub const GETCWD: u64 = 17;
    pub const DUP: u64 = 23;
    pub const DUP3: u64 = 24;
    pub const FCNTL: u64 = 25;
    pub const IOCTL: u64 = 29;
    pub const MKDIRAT: u64 = 34;
    pub const UNLINKAT: u64 = 35;
    pub const SYMLINKAT: u64 = 36;
    pub const LINKAT: u64 = 37;
    pub const FTRUNCATE: u64 = 46;
    pub const FCHMODAT: u64 = 53;
    pub const FCHDIR: u64 = 50;
    pub const UTIMENSAT: u64 = 88;
    pub const RENAMEAT2: u64 = 276;
    pub const CHDIR: u64 = 49;
    pub const OPENAT: u64 = 56;
    pub const PIPE2: u64 = 59;
    pub const CLOSE: u64 = 57;
    pub const GETDENTS64: u64 = 61;
    pub const LSEEK: u64 = 62;
    pub const READ: u64 = 63;
    pub const WRITE: u64 = 64;
    pub const READV: u64 = 65;
    pub const WRITEV: u64 = 66;
    pub const PREAD64: u64 = 67;
    pub const PWRITE64: u64 = 68;
    pub const READLINKAT: u64 = 78;
    pub const NEWFSTATAT: u64 = 79;
    pub const FSTAT: u64 = 80;
    pub const EXIT: u64 = 93;
    pub const EXIT_GROUP: u64 = 94;
    pub const SET_TID_ADDRESS: u64 = 96;
    pub const NANOSLEEP: u64 = 101;
    pub const CLOCK_GETTIME: u64 = 113;
    pub const CLOCK_GETRES: u64 = 114;
    pub const CLOCK_NANOSLEEP: u64 = 115;
    pub const SCHED_YIELD: u64 = 124;
    pub const KILL: u64 = 129;
    pub const TKILL: u64 = 130;
    pub const TGKILL: u64 = 131;
    pub const RT_SIGACTION: u64 = 134;
    pub const RT_SIGPROCMASK: u64 = 135;
    pub const RT_SIGPENDING: u64 = 136;
    pub const RT_SIGRETURN: u64 = 139;
    pub const SETPGID: u64 = 154;
    pub const GETPGID: u64 = 155;
    pub const GETSID: u64 = 156;
    pub const SETSID: u64 = 157;
    pub const UNAME: u64 = 160;
    pub const GETTIMEOFDAY: u64 = 169;
    pub const GETPID: u64 = 172;
    pub const GETPPID: u64 = 173;
    pub const GETUID: u64 = 174;
    pub const GETEUID: u64 = 175;
    pub const GETGID: u64 = 176;
    pub const GETEGID: u64 = 177;
    pub const GETTID: u64 = 178;
    pub const BRK: u64 = 214;
    pub const MUNMAP: u64 = 215;
    pub const CLONE: u64 = 220;
    pub const EXECVE: u64 = 221;
    pub const MMAP: u64 = 222;
    pub const MPROTECT: u64 = 226;
    pub const WAIT4: u64 = 260;
    pub const GETRANDOM: u64 = 278;
}

pub fn init() {
    exceptions::set_syscall_handler(handle);
}

/// Bitmap of syscall numbers already reported as unimplemented (0..512).
static REPORTED: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

fn handle(frame: &mut TrapFrame, imm: u16) {
    if !frame.is_user() {
        // Kernel SVCs (the boot self-test) are not syscalls.
        kprintln!(
            "[svc] from EL1 imm={:#x} x8={:#x}: ignored",
            imm,
            frame.x[8]
        );
        frame.x[0] = u64::MAX;
        return;
    }
    sync::irq_enable();
    let nr = frame.x[8];
    let r = dispatch(frame, nr);
    #[cfg(feature = "strace")]
    kprintln!("[strace] pid {} nr {} -> {:?}", sched::current_pid(), nr, r);
    frame.x[0] = match r {
        Ok(v) => v as u64,
        Err(e) => e.neg() as u64,
    };
    sync::irq_disable();
    // Rescheduling and signal delivery happen in proc::on_user_return.
}

fn dispatch(frame: &mut TrapFrame, nr: u64) -> KResult<usize> {
    let a = [
        frame.x[0], frame.x[1], frame.x[2], frame.x[3], frame.x[4], frame.x[5],
    ];
    let (a0, a1, a2, a3) = (a[0] as usize, a[1] as usize, a[2] as usize, a[3] as usize);
    match nr {
        n if super::sock::handles(n) => super::sock::dispatch(n, a),
        nr::READ => sys_read(a[0] as i64, a1, a2),
        nr::WRITE => sys_write(a[0] as i64, a1, a2),
        nr::READV => sys_readv(a[0] as i64, a1, a2),
        nr::WRITEV => sys_writev(a[0] as i64, a1, a2),
        nr::PREAD64 => {
            let f = file(a[0] as i64)?;
            let mut buf = alloc::vec![0u8; a2.min(MAX_IO)];
            let n = f.pread(&mut buf, a3 as u64)?;
            copy_out(a1, &buf[..n])?;
            Ok(n)
        }
        nr::PWRITE64 => {
            let f = file(a[0] as i64)?;
            let buf = copy_in(a1, a2.min(MAX_IO))?;
            f.pwrite(&buf, a3 as u64)
        }
        nr::OPENAT => sys_openat(a[0] as i32 as i64, a1, a[2] as u32, a[3] as u32),
        nr::CLOSE => {
            let e = current().inner.lock().fds.close(a[0] as i64)?;
            drop(e);
            Ok(0)
        }
        nr::LSEEK => {
            let f = file(a[0] as i64)?;
            if f.inode().kind() == InodeType::Fifo {
                return Err(ESPIPE);
            }
            Ok(f.seek(a[1] as i64, a[2] as u32)? as usize)
        }
        nr::PIPE2 => sys_pipe2(a0, a[1] as u32),
        nr::FSTAT => {
            let st = file(a[0] as i64)?.stat().to_linux();
            copy_out(a1, st.as_bytes())?;
            Ok(0)
        }
        nr::READLINKAT => {
            let path = user_path(a1)?;
            let base = base(a[0] as i32 as i64)?;
            let t = fs::readlink_at(base.as_ref(), &path)?;
            let n = t.len().min(a3);
            copy_out(a2, &t.as_bytes()[..n])?;
            Ok(n)
        }
        nr::NEWFSTATAT => sys_newfstatat(a[0] as i32 as i64, a1, a2, a[3] as u32),
        nr::GETDENTS64 => {
            let f = file(a[0] as i64)?;
            let mut buf = alloc::vec![0u8; a2.min(MAX_IO)];
            let n = f.getdents64(&mut buf)?;
            copy_out(a1, &buf[..n])?;
            Ok(n)
        }
        nr::GETCWD => {
            let cwd = cwd();
            let mut p = fs::path_of(&cwd)?.into_bytes();
            p.push(0);
            if p.len() > a1 {
                return Err(ERANGE);
            }
            copy_out(a0, &p)?;
            Ok(p.len())
        }
        nr::CHDIR => {
            let path = user_path(a0)?;
            let ino = fs::lookup_at(Some(&cwd()), &path, true)?;
            if ino.kind() != InodeType::Dir {
                return Err(ENOTDIR);
            }
            current().inner.lock().cwd = ino;
            Ok(0)
        }
        nr::MKDIRAT => {
            let path = user_path(a1)?;
            let base = base(a[0] as i32 as i64)?;
            fs::mkdir_at(base.as_ref(), &path, a[2] as u32 & 0o7777 & !UMASK)?;
            Ok(0)
        }
        nr::UNLINKAT => {
            let path = user_path(a1)?;
            let base = base(a[0] as i32 as i64)?;
            if a[2] as u32 & abi::AT_REMOVEDIR != 0 {
                fs::rmdir_at(base.as_ref(), &path)?;
            } else {
                fs::unlink_at(base.as_ref(), &path)?;
            }
            Ok(0)
        }
        nr::SYMLINKAT => {
            let target = user_path(a0)?;
            let path = user_path(a2)?;
            let base = base(a[1] as i32 as i64)?;
            fs::symlink_at(&target, base.as_ref(), &path)?;
            Ok(0)
        }
        nr::LINKAT => {
            let (old, new) = (user_path(a1)?, user_path(a3)?);
            let (ob, nb) = (base(a[0] as i32 as i64)?, base(a[2] as i32 as i64)?);
            let follow = a[4] as u32 & abi::AT_SYMLINK_FOLLOW != 0;
            fs::link_at(ob.as_ref(), &old, nb.as_ref(), &new, follow)?;
            Ok(0)
        }
        nr::RENAMEAT2 => {
            if a[4] != 0 {
                return Err(EINVAL); // RENAME_NOREPLACE / EXCHANGE: not yet
            }
            let (old, new) = (user_path(a1)?, user_path(a3)?);
            let (ob, nb) = (base(a[0] as i32 as i64)?, base(a[2] as i32 as i64)?);
            fs::rename_at(ob.as_ref(), &old, nb.as_ref(), &new)?;
            Ok(0)
        }
        nr::FTRUNCATE => {
            file(a[0] as i64)?.truncate(a1 as u64)?;
            Ok(0)
        }
        nr::FCHMODAT => {
            let path = user_path(a1)?;
            let base = base(a[0] as i32 as i64)?;
            let attr = fs::SetAttr {
                perm: Some(a[2] as u32 & 0o7777),
                ..Default::default()
            };
            fs::setattr_at(base.as_ref(), &path, true, &attr)?;
            Ok(0)
        }
        nr::FCHDIR => {
            let ino = file(a[0] as i64)?.inode().clone();
            if ino.kind() != InodeType::Dir {
                return Err(ENOTDIR);
            }
            current().inner.lock().cwd = ino;
            Ok(0)
        }
        nr::UTIMENSAT => sys_utimensat(a[0] as i32 as i64, a1, a2, a[3] as u32),
        nr::DUP => {
            let p = current();
            let mut i = p.inner.lock();
            let f = i.fds.get(a[0] as i64)?;
            Ok(i.fds.alloc(f, false)? as usize)
        }
        nr::DUP3 => {
            if a[0] == a[1] {
                return Err(EINVAL);
            }
            let p = current();
            let old = {
                let mut i = p.inner.lock();
                let f = i.fds.get(a[0] as i64)?;
                i.fds
                    .set(a[1] as i64, f, a[2] as u32 & abi::O_CLOEXEC != 0)?
            };
            drop(old);
            Ok(a1)
        }
        nr::FCNTL => sys_fcntl(a[0] as i64, a[1] as u32, a2),
        nr::IOCTL => sys_ioctl(a[0] as i64, a[1] as u32, a2),

        nr::EXIT | nr::EXIT_GROUP => super::exit_current(super::wstatus_exited(a[0] as i32)),
        nr::SET_TID_ADDRESS => Ok(sched::current_tid() as usize),
        nr::NANOSLEEP => sleep_interruptible(read_timespec(a0)?, a1),
        nr::CLOCK_NANOSLEEP => {
            const TIMER_ABSTIME: u64 = 1;
            let ns = read_timespec(a2)?;
            if a[1] & TIMER_ABSTIME != 0 {
                let now = clock_ns(a[0] as u32)?;
                sleep_interruptible(ns.saturating_sub(now), 0)
            } else {
                sleep_interruptible(ns, a3)
            }
        }
        nr::CLOCK_GETTIME => {
            let ns = clock_ns(a[0] as u32)?;
            write_timespec(a1, ns)?;
            Ok(0)
        }
        nr::CLOCK_GETRES => {
            clock_ns(a[0] as u32)?;
            if a1 != 0 {
                let res = 1_000_000_000 / timer::frequency().max(1);
                write_timespec(a1, res.max(1))?;
            }
            Ok(0)
        }
        nr::GETTIMEOFDAY => {
            if a0 != 0 {
                let ns = clock_ns(0)?;
                let tv = [ns / 1_000_000_000, (ns % 1_000_000_000) / 1000];
                copy_out(a0, &words(&tv))?;
            }
            Ok(0)
        }
        nr::SCHED_YIELD => {
            sched::yield_now();
            Ok(0)
        }
        nr::RT_SIGACTION => {
            let new = if a1 != 0 {
                let b = copy_in(a1, 32)?;
                let w = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
                Some(signal::SigAction {
                    handler: w(0),
                    flags: w(1),
                    restorer: w(2),
                    mask: w(3),
                })
            } else {
                None
            };
            let old = signal::sigaction(a[0] as u32, new)?;
            if a2 != 0 {
                copy_out(
                    a2,
                    &words(&[old.handler, old.flags, old.restorer, old.mask]),
                )?;
            }
            Ok(0)
        }
        nr::RT_SIGPROCMASK => {
            let set = if a1 != 0 {
                Some(u64::from_le_bytes(copy_in(a1, 8)?.try_into().unwrap()))
            } else {
                None
            };
            let old = signal::sigprocmask(a[0] as u32, set)?;
            if a2 != 0 {
                copy_out(a2, &old.to_le_bytes())?;
            }
            Ok(0)
        }
        nr::RT_SIGPENDING => {
            copy_out(a0, &signal::pending().to_le_bytes())?;
            Ok(0)
        }
        nr::RT_SIGRETURN => signal::sigreturn(frame),
        nr::KILL => signal::kill(a[0] as i32 as i64, a[1] as u32).map(|_| 0),
        // One thread per process: a tid names its process.
        nr::TKILL => signal::kill(tid_to_pid(a[0] as u32)? as i64, a[1] as u32).map(|_| 0),
        nr::TGKILL => signal::kill(a[0] as i32 as i64, a[2] as u32).map(|_| 0),
        nr::SETPGID => sys_setpgid(a[0] as u32, a[1] as u32),
        nr::GETPGID => {
            let p = if a[0] == 0 {
                current()
            } else {
                super::get(a[0] as u32).ok_or(ESRCH)?
            };
            Ok(p.inner.lock().pgid as usize)
        }
        nr::GETSID => {
            let p = if a[0] == 0 {
                current()
            } else {
                super::get(a[0] as u32).ok_or(ESRCH)?
            };
            Ok(p.inner.lock().sid as usize)
        }
        nr::SETSID => {
            let p = current();
            if super::all().iter().any(|q| q.inner.lock().pgid == p.pid) {
                return Err(EPERM); // already a group leader
            }
            let mut i = p.inner.lock();
            i.pgid = p.pid;
            i.sid = p.pid;
            Ok(p.pid as usize)
        }
        nr::UNAME => sys_uname(a0),
        nr::GETPID => Ok(sched::current_pid() as usize),
        nr::GETPPID => Ok(current().inner.lock().ppid as usize),
        nr::GETUID | nr::GETEUID | nr::GETGID | nr::GETEGID => Ok(0),
        nr::GETTID => Ok(sched::current_tid() as usize),

        nr::BRK => with_space(|s| Ok(s.set_brk(a0))),
        nr::MMAP => sys_mmap(a0, a1, a[2] as u32, a[3] as u32, a[4] as i64),
        nr::MUNMAP => with_space(|s| s.munmap(a0, a1).map(|_| 0)),
        nr::MPROTECT => {
            let perm = prot_to_perm(a[2] as u32);
            with_space(|s| s.protect(a0, a1, perm).map(|_| 0))
        }

        nr::CLONE => sys_clone(frame, a[0], a1),
        nr::EXECVE => {
            let path = user_path(a0)?;
            let (argv, envp) = {
                let p = current();
                let i = p.inner.lock();
                let s = i.space.as_ref().ok_or(ESRCH)?;
                (s.read_str_array(a1, 1024)?, s.read_str_array(a2, 1024)?)
            };
            super::exec(frame, &path, &argv, &envp)?;
            Ok(0)
        }
        nr::WAIT4 => {
            let (pid, status) = super::wait(a[0] as i32 as i64, a[2])?;
            if a1 != 0 && pid != 0 {
                copy_out(a1, &status.to_le_bytes())?;
            }
            if a3 != 0 {
                copy_out(a3, &[0u8; 144])?; // struct rusage: all zero
            }
            Ok(pid as usize)
        }
        nr::GETRANDOM => {
            let f = fs::open("/dev/urandom", abi::O_RDONLY, 0)?;
            let mut buf = alloc::vec![0u8; a1.min(MAX_IO)];
            let n = f.read(&mut buf)?;
            copy_out(a0, &buf[..n])?;
            Ok(n)
        }
        _ => {
            let (w, b) = ((nr as usize / 64) % 8, nr % 64);
            if nr < 512 && REPORTED[w].fetch_or(1 << b, Ordering::Relaxed) & (1 << b) == 0 {
                let p = current();
                let name = p.inner.lock().name;
                kprintln!(
                    "[syscall] pid {} ({}): syscall {} not implemented (ENOSYS)",
                    p.pid,
                    name,
                    nr
                );
            }
            Err(ENOSYS)
        }
    }
}

// ---- helpers --------------------------------------------------------------------------

/// Largest single read/write we buffer in the kernel; bigger requests return short counts.
const MAX_IO: usize = 1 << 20;
const UMASK: u32 = 0o022;

fn with_space<R>(f: impl FnOnce(&mut UserSpace) -> KResult<R>) -> KResult<R> {
    let p = current();
    let mut i = p.inner.lock();
    f(i.space.as_mut().ok_or(ESRCH)?)
}

pub(super) fn copy_in(va: usize, len: usize) -> KResult<Vec<u8>> {
    let mut v = alloc::vec![0u8; len];
    with_space(|s| s.copy_in(va, &mut v))?;
    Ok(v)
}

pub(super) fn copy_out(va: usize, data: &[u8]) -> KResult<()> {
    with_space(|s| s.copy_out(va, data))
}

fn user_path(va: usize) -> KResult<String> {
    with_space(|s| s.read_cstr(va, 4096))
}

fn words(w: &[u64]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub(super) fn file(fd: i64) -> KResult<Arc<OpenFile>> {
    current().inner.lock().fds.get(fd)
}

fn cwd() -> InodeRef {
    current().inner.lock().cwd.clone()
}

/// The base directory for an `*at` call.
fn base(dirfd: i64) -> KResult<Option<InodeRef>> {
    if dirfd == abi::AT_FDCWD as i64 {
        Ok(Some(cwd()))
    } else {
        Ok(Some(file(dirfd)?.inode().clone()))
    }
}

fn read_timespec(va: usize) -> KResult<u64> {
    let b = copy_in(va, 16)?;
    let sec = i64::from_le_bytes(b[0..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(b[8..16].try_into().unwrap());
    if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
        return Err(EINVAL);
    }
    Ok((sec as u64).saturating_mul(1_000_000_000) + nsec as u64)
}

fn write_timespec(va: usize, ns: u64) -> KResult<()> {
    copy_out(va, &words(&[ns / 1_000_000_000, ns % 1_000_000_000]))
}

/// Time on clock `id` in ns.
fn clock_ns(id: u32) -> KResult<u64> {
    match id {
        // CLOCK_REALTIME, _COARSE, _ALARM, TAI
        0 | 5 | 8 | 11 => {
            let t = fs::clock::now();
            Ok(t.sec as u64 * 1_000_000_000 + t.nsec as u64)
        }
        // CLOCK_MONOTONIC, _RAW, _COARSE, BOOTTIME, BOOTTIME_ALARM
        1 | 4 | 6 | 7 | 9 => Ok(timer::now_ns()),
        // CLOCK_PROCESS_CPUTIME_ID, CLOCK_THREAD_CPUTIME_ID
        2 | 3 => Ok(sched::current_cpu_ns()),
        _ => Err(EINVAL),
    }
}

/// nanosleep that a signal can cut short (EINTR, remaining time in `rem`).
fn sleep_interruptible(ns: u64, rem: usize) -> KResult<usize> {
    static NOBODY: sched::WaitQueue = sched::WaitQueue::new();
    let start = timer::now_ns();
    if NOBODY.wait_until_timeout(signal::interrupted, ns) {
        if rem != 0 {
            let left = ns.saturating_sub(timer::now_ns() - start);
            write_timespec(rem, left)?;
        }
        return Err(EINTR);
    }
    Ok(0)
}

fn tid_to_pid(tid: u32) -> KResult<u32> {
    sched::threads()
        .into_iter()
        .find(|t| t.tid == tid && t.pid != 0)
        .map(|t| t.pid)
        .ok_or(ESRCH)
}

/// setpgid(pid, pgid): only on yourself or your children, within your session.
fn sys_setpgid(pid: u32, pgid: u32) -> KResult<usize> {
    let me = current();
    let target = if pid == 0 || pid == me.pid {
        me.clone()
    } else {
        let t = super::get(pid).ok_or(ESRCH)?;
        if t.inner.lock().ppid != me.pid {
            return Err(ESRCH);
        }
        t
    };
    let pgid = if pgid == 0 { target.pid } else { pgid };
    let my_sid = me.inner.lock().sid;
    // Joining an existing group: it must be in our session.
    if pgid != target.pid
        && !super::all().iter().any(|q| {
            let i = q.inner.lock();
            i.pgid == pgid && i.sid == my_sid
        })
    {
        return Err(EPERM);
    }
    let mut i = target.inner.lock();
    if i.sid != my_sid || i.sid == target.pid {
        return Err(EPERM); // other session, or a session leader
    }
    i.pgid = pgid;
    Ok(0)
}

fn prot_to_perm(prot: u32) -> Option<Perm> {
    const PROT_WRITE: u32 = 2;
    const PROT_EXEC: u32 = 4;
    (prot != 0).then_some(Perm {
        write: prot & PROT_WRITE != 0,
        exec: prot & PROT_EXEC != 0,
    })
}

// ---- files ----------------------------------------------------------------------------

fn sys_read(fd: i64, buf: usize, count: usize) -> KResult<usize> {
    let f = file(fd)?;
    let mut kbuf = alloc::vec![0u8; count.min(MAX_IO)];
    let n = f.read(&mut kbuf)?;
    copy_out(buf, &kbuf[..n])?;
    Ok(n)
}

fn sys_write(fd: i64, buf: usize, count: usize) -> KResult<usize> {
    let f = file(fd)?;
    let data = copy_in(buf, count.min(MAX_IO))?;
    f.write(&data)
}

fn iovecs(iov: usize, cnt: usize) -> KResult<Vec<(usize, usize)>> {
    if cnt > 1024 {
        return Err(EINVAL);
    }
    let raw = copy_in(iov, cnt * 16)?;
    Ok(raw
        .chunks(16)
        .map(|c| {
            (
                u64::from_le_bytes(c[0..8].try_into().unwrap()) as usize,
                u64::from_le_bytes(c[8..16].try_into().unwrap()) as usize,
            )
        })
        .collect())
}

fn sys_writev(fd: i64, iov: usize, cnt: usize) -> KResult<usize> {
    let f = file(fd)?;
    let mut total = 0;
    for (base, len) in iovecs(iov, cnt)? {
        if len == 0 {
            continue;
        }
        let data = copy_in(base, len.min(MAX_IO))?;
        let n = f.write(&data)?;
        total += n;
        if n < len {
            break;
        }
    }
    Ok(total)
}

fn sys_readv(fd: i64, iov: usize, cnt: usize) -> KResult<usize> {
    let f = file(fd)?;
    let mut total = 0;
    for (base, len) in iovecs(iov, cnt)? {
        if len == 0 {
            continue;
        }
        let mut kbuf = alloc::vec![0u8; len.min(MAX_IO)];
        let n = f.read(&mut kbuf)?;
        copy_out(base, &kbuf[..n])?;
        total += n;
        if n < len {
            break;
        }
    }
    Ok(total)
}

fn sys_openat(dirfd: i64, path: usize, flags: u32, mode: u32) -> KResult<usize> {
    let path = user_path(path)?;
    let base = base(dirfd)?;
    let f = Arc::new(fs::open_at(
        base.as_ref(),
        &path,
        flags,
        mode & 0o7777 & !UMASK,
    )?);
    let fd = current()
        .inner
        .lock()
        .fds
        .alloc(f, flags & abi::O_CLOEXEC != 0)?;
    Ok(fd as usize)
}

fn sys_pipe2(fds: usize, flags: u32) -> KResult<usize> {
    if flags & !(abi::O_CLOEXEC | abi::O_NONBLOCK) != 0 {
        return Err(EINVAL);
    }
    let nb = flags & abi::O_NONBLOCK;
    let (r, w) = super::pipe::new(nb != 0);
    let r = Arc::new(OpenFile::new(r, abi::O_RDONLY | nb));
    let w = Arc::new(OpenFile::new(w, abi::O_WRONLY | nb));
    let cloexec = flags & abi::O_CLOEXEC != 0;
    let (rfd, wfd) = {
        let p = current();
        let mut i = p.inner.lock();
        let rfd = i.fds.alloc(r, cloexec)?;
        match i.fds.alloc(w, cloexec) {
            Ok(wfd) => (rfd, wfd),
            Err(e) => {
                let _ = i.fds.close(rfd);
                return Err(e);
            }
        }
    };
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&(rfd as i32).to_le_bytes());
    out[4..].copy_from_slice(&(wfd as i32).to_le_bytes());
    if let Err(e) = copy_out(fds, &out) {
        let p = current();
        let mut i = p.inner.lock();
        let _ = i.fds.close(rfd);
        let _ = i.fds.close(wfd);
        return Err(e);
    }
    Ok(0)
}

fn sys_newfstatat(dirfd: i64, path: usize, statbuf: usize, flags: u32) -> KResult<usize> {
    let path = user_path(path)?;
    let st = if path.is_empty() && flags & abi::AT_EMPTY_PATH != 0 {
        file(dirfd)?.stat()
    } else {
        let base = base(dirfd)?;
        fs::stat_at(base.as_ref(), &path, flags & abi::AT_SYMLINK_NOFOLLOW == 0)?
    };
    copy_out(statbuf, st.to_linux().as_bytes())?;
    Ok(0)
}

fn sys_utimensat(dirfd: i64, path: usize, times: usize, flags: u32) -> KResult<usize> {
    const UTIME_NOW: i64 = (1 << 30) - 1;
    const UTIME_OMIT: i64 = (1 << 30) - 2;
    let now = fs::clock::now();
    let (atime, mtime) = if times == 0 {
        (Some(now), Some(now))
    } else {
        let b = copy_in(times, 32)?;
        let ts = |o: usize| {
            let sec = i64::from_le_bytes(b[o..o + 8].try_into().unwrap());
            let nsec = i64::from_le_bytes(b[o + 8..o + 16].try_into().unwrap());
            match nsec {
                UTIME_OMIT => None,
                UTIME_NOW => Some(now),
                _ => Some(fs::Timespec { sec, nsec }),
            }
        };
        (ts(0), ts(16))
    };
    let attr = fs::SetAttr {
        atime,
        mtime,
        ..Default::default()
    };
    let follow = flags & abi::AT_SYMLINK_NOFOLLOW == 0;
    if path == 0 {
        // futimens(fd, ...)
        return file(dirfd)?.inode().setattr(&attr).map(|_| 0);
    }
    let p = user_path(path)?;
    let base = base(dirfd)?;
    fs::setattr_at(base.as_ref(), &p, follow, &attr)?;
    Ok(0)
}

fn sys_fcntl(fd: i64, cmd: u32, arg: usize) -> KResult<usize> {
    const F_DUPFD: u32 = 0;
    const F_GETFD: u32 = 1;
    const F_SETFD: u32 = 2;
    const F_GETFL: u32 = 3;
    const F_SETFL: u32 = 4;
    const F_DUPFD_CLOEXEC: u32 = 1030;
    const FD_CLOEXEC: usize = 1;
    let p = current();
    let mut i = p.inner.lock();
    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let f = i.fds.get(fd)?;
            Ok(i.fds.alloc_from(f, cmd == F_DUPFD_CLOEXEC, arg)? as usize)
        }
        F_GETFD => Ok(if i.fds.cloexec(fd)? { FD_CLOEXEC } else { 0 }),
        F_SETFD => {
            i.fds.set_cloexec(fd, arg & FD_CLOEXEC != 0)?;
            Ok(0)
        }
        F_GETFL => Ok(i.fds.get(fd)?.flags() as usize),
        F_SETFL => {
            i.fds.get(fd)?.set_flags(arg as u32);
            Ok(0)
        }
        _ => Err(EINVAL),
    }
}

fn tty_of(f: &OpenFile) -> Option<&'static super::tty::Tty> {
    let st = f.stat();
    (st.kind == InodeType::CharDev)
        .then(|| super::tty::by_rdev(st.rdev))
        .flatten()
}

fn sys_ioctl(fd: i64, cmd: u32, arg: usize) -> KResult<usize> {
    let f = file(fd)?;
    if let Some(tty) = tty_of(&f) {
        let input = match super::tty::ioctl_in_size(cmd) {
            0 => Vec::new(),
            n => copy_in(arg, n)?,
        };
        if let Some(out) = tty.ioctl(cmd, &input)?
            && arg != 0
        {
            copy_out(arg, &out)?;
        }
        return Ok(0);
    }
    f.ioctl(cmd, arg)
}

// ---- memory ---------------------------------------------------------------------------

fn sys_mmap(addr: usize, len: usize, prot: u32, flags: u32, fd: i64) -> KResult<usize> {
    const MAP_PRIVATE: u32 = 0x02;
    const MAP_FIXED: u32 = 0x10;
    const MAP_ANONYMOUS: u32 = 0x20;
    if flags & MAP_ANONYMOUS == 0 || fd != -1 {
        return Err(Errno::ENODEV); // file mappings: not yet
    }
    if flags & MAP_PRIVATE == 0 {
        return Err(EINVAL); // shared anonymous needs shared frames across fork: not yet
    }
    with_space(|s| s.mmap_anon(addr, len, prot_to_perm(prot), flags & MAP_FIXED != 0))
}

// ---- processes ------------------------------------------------------------------------

fn sys_clone(frame: &TrapFrame, flags: u64, newsp: usize) -> KResult<usize> {
    const SIGCHLD: u64 = 17;
    // Plain fork() only: exit signal SIGCHLD (or none), no sharing flags.
    if flags & !0xff != 0 || !(flags == SIGCHLD || flags == 0) {
        return Err(ENOSYS);
    }
    let mut f = *frame;
    if newsp != 0 {
        f.sp_el0 = newsp as u64;
    }
    Ok(super::fork(&f)? as usize)
}

fn sys_uname(buf: usize) -> KResult<usize> {
    let mut u = [0u8; 65 * 6];
    let fields = [
        "milkOS",
        "milk",
        "0.0.1",
        "#1 milkOS 0.0.1 (aarch64, Rust)",
        "aarch64",
        "(none)",
    ];
    for (i, f) in fields.iter().enumerate() {
        u[i * 65..i * 65 + f.len()].copy_from_slice(f.as_bytes());
    }
    copy_out(buf, &u)?;
    Ok(0)
}
