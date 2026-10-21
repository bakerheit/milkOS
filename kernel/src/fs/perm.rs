//! Permission checks and file ownership for the VFS (Unix rwx bits, sticky directories,
//! set-ID handling), on behalf of whoever is calling: a user process's credentials, or the
//! kernel (root). See docs/security.md "Users and permissions".
//!
//! The file systems only store `perm` / `uid` / `gid`; this module decides. It learns who
//! is calling through a resolver the process layer installs ([`set_cred_resolver`]);
//! without one (early boot, kernel threads) the caller is the kernel, which is root.
//!
//! FAT and the 9p host share have no Unix owners: everything on them is shown as owned by
//! the user they were mounted for ([`set_mount_owner`], the session user), and set-ID bits
//! there are ignored ([`nosuid`]).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use super::node::InodeRef;
use crate::sync::{FnSlot, SpinLock};

pub const R: u32 = 4;
pub const W: u32 = 2;
pub const X: u32 = 1;

/// Who is asking. Copy, no allocation (checked on every path step).
#[derive(Clone, Copy, Debug)]
pub struct Who {
    pub uid: u32,
    pub gid: u32,
    pub groups: [u32; 16],
    pub ngroups: u8,
    /// A kernel thread (no process): root, and new files take the parent's owner.
    pub kernel: bool,
}

impl Who {
    pub const KERNEL: Who = Who {
        uid: 0,
        gid: 0,
        groups: [0; 16],
        ngroups: 0,
        kernel: true,
    };

    pub fn is_root(&self) -> bool {
        self.uid == 0
    }

    pub fn in_group(&self, gid: u32) -> bool {
        self.gid == gid || self.groups[..self.ngroups as usize].contains(&gid)
    }
}

pub type CredResolver = fn() -> Who;
static RESOLVER: FnSlot = FnSlot::new();

/// The process layer tells the VFS who the current caller is.
pub fn set_cred_resolver(f: CredResolver) {
    RESOLVER.set(f);
}

pub fn who() -> Who {
    RESOLVER.get::<CredResolver>().map_or(Who::KERNEL, |f| f())
}

// ---- file systems without Unix owners ---------------------------------------------------

/// uid << 32 | gid of the user FAT / 9p files appear to belong to.
static MOUNT_OWNER: AtomicU64 = AtomicU64::new(0);

pub fn set_mount_owner(uid: u32, gid: u32) {
    MOUNT_OWNER.store((uid as u64) << 32 | gid as u64, Ordering::Relaxed);
}

/// dev -> "has no Unix owners" (FAT, 9p), cached from the mount table.
static FOREIGN: SpinLock<Vec<(u64, bool)>> = SpinLock::new(Vec::new());

fn foreign_dev(dev: u64) -> bool {
    if let Some(&(_, f)) = FOREIGN.lock().iter().find(|(d, _)| *d == dev) {
        return f;
    }
    let f = super::path::mounts()
        .iter()
        .find(|m| m.dev == dev)
        .is_some_and(|m| matches!(m.fs_name, "fat" | "vfat" | "msdos" | "9p"));
    FOREIGN.lock().push((dev, f));
    f
}

/// Set-ID bits are ignored on this inode's file system.
pub fn nosuid(node: &InodeRef) -> bool {
    foreign_dev(node.dev())
}

/// `node.stat()` with FAT / 9p ownership mapped to the mount owner.
pub fn stat(node: &InodeRef) -> super::node::Stat {
    let mut st = node.stat();
    if foreign_dev(st.dev) {
        let o = MOUNT_OWNER.load(Ordering::Relaxed);
        st.uid = (o >> 32) as u32;
        st.gid = o as u32;
        st.perm &= !0o6000;
    }
    st
}
