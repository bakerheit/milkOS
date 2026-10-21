//! Virtual file system. See `docs/filesystem.md`.
//!
//! - `node`: the `FileSystem` / `Inode` traits and metadata types.
//! - `path`: mount table and path resolution.
//! - `file`: `OpenFile` (inode + offset + flags), what fd tables hold.
//! - `ops`: the path-based kernel API (`open`, `stat`, `mkdir`, ...), re-exported here.
//! - `abi`: Linux userspace layouts (`struct stat`, `linux_dirent64`, `O_*` flags).
//! - file systems: `tmpfs`.

// Public API surface for the syscall layer; not all of it is used inside the kernel yet.
#![allow(dead_code)]
#![allow(unused_imports)]

pub mod abi;
pub mod clock;
pub mod errno;
pub mod file;
pub mod node;
pub mod ops;
pub mod path;
pub mod tmpfs;

pub use errno::{Errno, Result};
pub use file::OpenFile;
pub use node::{DirEntry, FileSystem, FsRef, Inode, InodeRef, InodeType, SetAttr, Stat, Timespec};
pub use ops::*;
pub use path::{mount, path_of, root};

use crate::kprintln;

/// Directories created on the root tmpfs before anything else is unpacked.
/// (path, permissions)
const SKELETON: &[(&str, u32)] = &[
    ("/Applications", 0o755),
    ("/System", 0o755),
    ("/bin", 0o755),
    ("/dev", 0o755),
    ("/etc", 0o755),
    ("/home", 0o755),
    ("/home/milk", 0o755),
    ("/proc", 0o555),
    ("/sys", 0o555),
    ("/tmp", 0o1777),
    ("/Volumes", 0o755),
];

/// Mount the root tmpfs and build the standard layout. Call once, after the heap is up.
pub fn init() {
    clock::init();
    let root = tmpfs::TmpFs::new(0);
    mount("/", root).expect("fs: mount root tmpfs");
    for &(dir, perm) in SKELETON {
        mkdir(dir, perm).expect("fs: create skeleton");
    }
    // macOS spelling of the home directory parent.
    symlink("home", "/Users").expect("fs: /Users symlink");
    kprintln!("fs: tmpfs mounted at /");
}
