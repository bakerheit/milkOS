//! Errno for the process layer: the VFS's `fs::Errno` (Linux values) plus the few
//! process-related codes it doesn't name. Syscalls return `e.neg()`.

#![allow(dead_code)]

pub use crate::fs::errno::Errno;

pub const EPERM: Errno = Errno::EPERM;
pub const ENOENT: Errno = Errno::ENOENT;
pub const ESRCH: Errno = Errno(3);
pub const EINTR: Errno = Errno(4);
pub const EIO: Errno = Errno::EIO;
pub const E2BIG: Errno = Errno(7);
pub const ENOEXEC: Errno = Errno(8);
pub const EBADF: Errno = Errno::EBADF;
pub const ECHILD: Errno = Errno(10);
pub const EAGAIN: Errno = Errno::EAGAIN;
pub const ENOMEM: Errno = Errno::ENOMEM;
pub const EACCES: Errno = Errno::EACCES;
pub const EFAULT: Errno = Errno::EFAULT;
pub const EEXIST: Errno = Errno::EEXIST;
pub const ENOTDIR: Errno = Errno::ENOTDIR;
pub const EISDIR: Errno = Errno::EISDIR;
pub const EINVAL: Errno = Errno::EINVAL;
pub const EMFILE: Errno = Errno::EMFILE;
pub const ENOTTY: Errno = Errno::ENOTTY;
pub const ESPIPE: Errno = Errno::ESPIPE;
pub const ERANGE: Errno = Errno::ERANGE;
pub const ENAMETOOLONG: Errno = Errno::ENAMETOOLONG;
pub const ENOSYS: Errno = Errno::ENOSYS;
pub const ECANCELED: Errno = Errno(125);

pub type KResult<T> = Result<T, Errno>;
