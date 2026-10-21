//! Linux errno values the network stack returns that `fs::errno` does not define (yet).
//! Same `Errno` type, so a syscall layer returns `-(e.0)` for all of them alike.

#![allow(dead_code)]

use crate::fs::errno::Errno;

pub const EAGAIN: Errno = Errno::EAGAIN;
pub const EINVAL: Errno = Errno::EINVAL;
pub const EPERM: Errno = Errno::EPERM;
pub const ENOENT: Errno = Errno::ENOENT;
pub const EBADF: Errno = Errno::EBADF;
pub const EOPNOTSUPP: Errno = Errno::EOPNOTSUPP;
pub const EPIPE: Errno = Errno(32);
pub const ENOTSOCK: Errno = Errno(88);
pub const EDESTADDRREQ: Errno = Errno(89);
pub const EMSGSIZE: Errno = Errno(90);
pub const ENOPROTOOPT: Errno = Errno(92);
pub const EPROTONOSUPPORT: Errno = Errno(93);
pub const EAFNOSUPPORT: Errno = Errno(97);
pub const EADDRINUSE: Errno = Errno(98);
pub const EADDRNOTAVAIL: Errno = Errno(99);
pub const ENETDOWN: Errno = Errno(100);
pub const ENETUNREACH: Errno = Errno(101);
pub const ECONNABORTED: Errno = Errno(103);
pub const ECONNRESET: Errno = Errno(104);
pub const ENOBUFS: Errno = Errno(105);
pub const EISCONN: Errno = Errno(106);
pub const ENOTCONN: Errno = Errno(107);
pub const ETIMEDOUT: Errno = Errno(110);
pub const ECONNREFUSED: Errno = Errno(111);
pub const EHOSTUNREACH: Errno = Errno(113);
pub const EALREADY: Errno = Errno(114);
pub const EINPROGRESS: Errno = Errno(115);

/// Short name for logs.
pub fn name(e: Errno) -> &'static str {
    match e.0 {
        11 => "EAGAIN",
        22 => "EINVAL",
        1 => "EPERM",
        2 => "ENOENT",
        9 => "EBADF",
        95 => "EOPNOTSUPP",
        32 => "EPIPE",
        88 => "ENOTSOCK",
        89 => "EDESTADDRREQ",
        90 => "EMSGSIZE",
        93 => "EPROTONOSUPPORT",
        97 => "EAFNOSUPPORT",
        98 => "EADDRINUSE",
        99 => "EADDRNOTAVAIL",
        100 => "ENETDOWN",
        101 => "ENETUNREACH",
        103 => "ECONNABORTED",
        104 => "ECONNRESET",
        105 => "ENOBUFS",
        106 => "EISCONN",
        107 => "ENOTCONN",
        110 => "ETIMEDOUT",
        111 => "ECONNREFUSED",
        113 => "EHOSTUNREACH",
        114 => "EALREADY",
        115 => "EINPROGRESS",
        _ => "E?",
    }
}
