//! milkfs: a copy-on-write file system with instant snapshots, writable clones, send /
//! receive streams, encryption at rest and a background scrub. See `docs/filesystem.md`
//! ("milkfs") for the format and the design.
//!
//! `no_std` + `alloc`, built for the kernel and for the Mac (host tests). What it needs from
//! its host: a disk ([`blockdev::BlockDevice`]) and a [`platform::Platform`] (time, random
//! bytes, scrypt, a log, locks). The kernel's `fs/milkfs` is the VFS glue on top.
//!
//! - `errno`, `abi`: Linux errno values and the file-type bits (shared with the kernel).
//! - `blockdev`, `bcache`: the disk and the write-back block cache over it.
//! - `platform`, `sync`: the host's services, and locks built on them.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod abi;
pub mod bcache;
pub mod blockdev;
pub mod errno;
pub mod platform;
pub mod sync;
