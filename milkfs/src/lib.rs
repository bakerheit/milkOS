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
//! - `format`: on-disk structures (superblock, nodes, dead lists, items), xxh64.
//! - `store`: block allocation (free map from the space map, or rebuilt at mount),
//!   birth-time release rules, checksummed (and on encrypted volumes, sealed) block I/O.
//! - `btree`: the CoW B+tree.
//! - `volume`: transaction / commit, mount (mark + check + orphan reclaim), mkfs.
//! - `items`: inodes, directory entries, extents, file data, shared blocks (reflink).
//! - `snap`, `clone`: snapshots (create / delete / rollback) and writable clones.
//! - `diff`, `versions`, `stream`: what changed between trees, a file's versions,
//!   send / receive streams.
//! - `crypt`, `convert`: encryption at rest, encrypting or decrypting in place.
//! - `fsck`, `scrub`: the full check (and repair), reading every block back.
//! - `timemachine`: automatic snapshots' policy.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod abi;
pub mod bcache;
pub mod blockdev;
pub mod btree;
pub mod clone;
pub mod convert;
pub mod crypt;
pub mod diff;
pub mod errno;
pub mod format;
pub mod fsck;
pub mod items;
pub mod platform;
pub mod scrub;
pub mod snap;
pub mod store;
pub mod stream;
pub mod sync;
pub mod timemachine;
pub mod versions;
pub mod volume;

pub(crate) use platform::fail;
