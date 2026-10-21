//! Linux errno values (asm-generic), returned negated from syscalls.

#![allow(dead_code)]

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Errno(pub i32);

pub const EPERM: Errno = Errno(1);
pub const ENOENT: Errno = Errno(2);
pub const ESRCH: Errno = Errno(3);
pub const EINTR: Errno = Errno(4);
pub const EIO: Errno = Errno(5);
pub const E2BIG: Errno = Errno(7);
pub const ENOEXEC: Errno = Errno(8);
pub const EBADF: Errno = Errno(9);
pub const ECHILD: Errno = Errno(10);
pub const EAGAIN: Errno = Errno(11);
pub const ENOMEM: Errno = Errno(12);
pub const EACCES: Errno = Errno(13);
pub const EFAULT: Errno = Errno(14);
pub const EEXIST: Errno = Errno(17);
pub const ENOTDIR: Errno = Errno(20);
pub const EISDIR: Errno = Errno(21);
pub const EINVAL: Errno = Errno(22);
pub const EMFILE: Errno = Errno(24);
pub const ENOTTY: Errno = Errno(25);
pub const ESPIPE: Errno = Errno(29);
pub const EROFS: Errno = Errno(30);
pub const ERANGE: Errno = Errno(34);
pub const ENAMETOOLONG: Errno = Errno(36);
pub const ENOSYS: Errno = Errno(38);

pub type KResult<T> = Result<T, Errno>;
