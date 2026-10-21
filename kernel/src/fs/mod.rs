//! Virtual file system. See `docs/filesystem.md`.
//!
//! - `node`: the `FileSystem` / `Inode` traits and metadata types.
//! - `path`: mount table and path resolution.
//! - `file`: `OpenFile` (inode + offset + flags), what fd tables hold.
//! - `ops`: the path-based kernel API (`open`, `stat`, `mkdir`, ...), re-exported here.
//! - `abi`: Linux userspace layouts (`struct stat`, `linux_dirent64`, `O_*` flags).
//! - file systems: `tmpfs` (root), `devfs` (/dev), `procfs` (/proc), `sysfs` (/sys), all but
//!   tmpfs built on `synth`; `initramfs` unpacks the built-in archive into the root.

// Public API surface for the syscall layer; not all of it is used inside the kernel yet.
#![allow(dead_code)]

pub mod abi;
pub mod bcache;
pub mod blockdev;
pub mod clock;
pub mod devfs;
pub mod errno;
pub mod fat;
pub mod file;
pub mod initramfs;
pub mod milkfs;
pub mod ninep;
pub mod node;
pub mod notify;
pub mod ops;
pub mod pagecache;
pub mod path;
pub mod perm;
pub mod procfs;
pub mod ramdisk;
pub mod search;
pub mod selftest;
pub mod synth;
pub mod sysfs;
pub mod tmpfs;

// Re-exports: the API the syscall layer uses (unused inside the kernel so far).
#[allow(unused_imports)]
pub use errno::{Errno, Result};
#[allow(unused_imports)]
pub use file::OpenFile;
#[allow(unused_imports)]
pub use node::{DirEntry, FileSystem, FsRef, Inode, InodeRef, InodeType, SetAttr, Stat, Timespec};
pub use ops::*;
#[allow(unused_imports)]
pub use path::{bind, current_root, mount, path_of, root, set_root_resolver, sync_all, unmount};

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

/// Start the file systems' kernel threads (call after `sched::init`).
pub fn start_threads() {
    milkfs::start_threads();
    search::start_thread();
}

/// Mount the root tmpfs and build the standard layout. Call once, after the heap is up.
pub fn init() {
    clock::init();
    let root = tmpfs::TmpFs::new(0);
    mount("/", root.clone()).expect("fs: mount root tmpfs");
    for &(dir, perm) in SKELETON {
        mkdir(dir, perm).expect("fs: create skeleton");
    }
    // macOS spelling of the home directory parent.
    symlink("home", "/Users").expect("fs: /Users symlink");
    kprintln!("fs: tmpfs mounted at /");
    initramfs::load();
    devfs::init().expect("fs: mount /dev");
    procfs::init(root).expect("fs: mount /proc");
    sysfs::init().expect("fs: mount /sys");
    milkfs::crashtest::configure();
    milkfs::automount();
    milkfs::setup_home();
    milkfs::setup_applications();
    milkfs::setup_persistent_etc("/etc/milk/drop");
    milkfs::setup_persistent_etc("/etc/milk/net");
    ninep::automount();
    fat::automount();
}
