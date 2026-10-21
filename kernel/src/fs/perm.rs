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
    // Only cache devs that are mounted (a dev looked up before its mount must not stick).
    let Some(f) = super::path::mounts()
        .iter()
        .find(|m| m.dev == dev)
        .map(|m| matches!(m.fs_name, "fat" | "vfat" | "msdos" | "9p"))
    else {
        return false;
    };
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

// ---- the checks --------------------------------------------------------------------------

use super::errno::{Errno, Result};
use super::node::{InodeType, SetAttr, Stat};

const ADMIN_GID: u32 = 80;

/// The ownership and mode the checks use: [`stat`] plus two mappings for synthetic file
/// systems: terminals (`/dev/console`, `/dev/ttyN`, mode 0620) belong to the session
/// user, and /proc and /sys control files (owner-writable) are writable by the `admin`
/// group, so administrators can use them without sudo.
pub fn view(node: &InodeRef) -> Stat {
    let mut st = stat(node);
    if let Some(kind) = synthetic(st.dev) {
        match kind {
            Synth::Dev if st.kind == InodeType::CharDev && st.perm == 0o620 => {
                let o = MOUNT_OWNER.load(Ordering::Relaxed);
                st.uid = (o >> 32) as u32;
                st.gid = o as u32;
            }
            Synth::ProcSys if st.kind == InodeType::File && st.perm & 0o200 != 0 => {
                st.gid = ADMIN_GID;
                st.perm |= 0o020;
            }
            _ => {}
        }
    }
    st
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Synth {
    Dev,
    ProcSys,
}

static SYNTH: SpinLock<Vec<(u64, Option<Synth>)>> = SpinLock::new(Vec::new());

fn synthetic(dev: u64) -> Option<Synth> {
    if let Some(&(_, s)) = SYNTH.lock().iter().find(|(d, _)| *d == dev) {
        return s;
    }
    let m = super::path::mounts().into_iter().find(|m| m.dev == dev)?;
    let s = match m.fs_name {
        "devfs" => Some(Synth::Dev),
        "proc" | "sysfs" => Some(Synth::ProcSys),
        _ => None,
    };
    SYNTH.lock().push((dev, s));
    s
}

/// May `w` access a file with `st` for `mask` (R | W | X)? Root may do anything, except
/// execute a non-directory with no x bit at all.
pub fn allowed(w: &Who, st: &Stat, mask: u32) -> bool {
    if w.is_root() {
        return mask & X == 0 || st.kind == InodeType::Dir || st.perm & 0o111 != 0;
    }
    let bits = if st.uid == w.uid {
        st.perm >> 6
    } else if w.in_group(st.gid) {
        st.perm >> 3
    } else {
        st.perm
    } & 7;
    bits & mask == mask
}

/// Check `mask` on `node` for the current caller: EACCES if not allowed.
pub fn may(node: &InodeRef, mask: u32) -> Result<()> {
    let w = who();
    if w.kernel {
        return Ok(());
    }
    if allowed(&w, &view(node), mask) {
        Ok(())
    } else {
        Err(Errno::EACCES)
    }
}

/// Removing or renaming `victim` out of `dir`: write + search on `dir`, and in a sticky
/// directory (`/tmp`) only the owner of the file or of the directory (or root).
pub fn may_delete(dir: &InodeRef, victim: &InodeRef) -> Result<()> {
    let w = who();
    if w.kernel {
        return Ok(());
    }
    let d = view(dir);
    if !allowed(&w, &d, W | X) {
        return Err(Errno::EACCES);
    }
    if d.perm & 0o1000 != 0 && !w.is_root() {
        let v = view(victim);
        if v.uid != w.uid && d.uid != w.uid {
            return Err(Errno::EPERM);
        }
    }
    Ok(())
}

/// chmod / chown / utimes on `node`: the change `a` after the rules (EPERM if refused).
///  * chmod: owner or root; a non-root owner not in the file's group loses set-group-ID.
///  * chown: root; the owner may change the group to one it is in.
///  * times: owner, root, or anyone who may write (touch).
pub fn check_setattr(node: &InodeRef, a: &SetAttr) -> Result<SetAttr> {
    let w = who();
    let mut a = *a;
    if w.kernel || w.is_root() {
        return Ok(a);
    }
    let st = view(node);
    let owner = st.uid == w.uid;
    if let Some(p) = a.perm {
        if !owner {
            return Err(Errno::EPERM);
        }
        let gid = a.gid.unwrap_or(st.gid);
        if !w.in_group(gid) {
            a.perm = Some(p & !0o2000);
        }
    }
    if a.uid.is_some_and(|u| u != st.uid) {
        return Err(Errno::EPERM);
    }
    if let Some(g) = a.gid
        && g != st.gid
        && !(owner && w.in_group(g))
    {
        return Err(Errno::EPERM);
    }
    if (a.uid.is_some() || a.gid.is_some()) && a.perm.is_none() {
        // A non-root chown clears set-ID bits (Linux).
        if st.perm & 0o6000 != 0 {
            a.perm = Some(st.perm & !0o6000);
        }
    }
    if (a.atime.is_some() || a.mtime.is_some()) && !owner && !allowed(&w, &st, W) {
        return Err(Errno::EACCES);
    }
    Ok(a)
}

/// Give a node just created in `dir` its owner: the caller's effective uid, and its
/// effective gid (or `dir`'s group if `dir` is set-group-ID). For the kernel (the desktop
/// saving into a user's home, say) the owner of `dir`, so files stay the user's.
pub fn own_new(node: &InodeRef, dir: &InodeRef) {
    let w = who();
    let d = view(dir);
    let (uid, gid) = if w.kernel {
        (d.uid, d.gid)
    } else if d.perm & 0o2000 != 0 {
        (w.uid, d.gid)
    } else {
        (w.uid, w.gid)
    };
    if foreign_dev(node.dev()) {
        return;
    }
    let st = node.stat();
    let mut a = SetAttr::default();
    if st.uid != uid {
        a.uid = Some(uid);
    }
    if st.gid != gid {
        a.gid = Some(gid);
    }
    if st.kind == InodeType::Dir && d.perm & 0o2000 != 0 && st.perm & 0o2000 == 0 {
        a.perm = Some(st.perm | 0o2000); // set-group-ID directories propagate
    }
    if a.uid.is_some() || a.gid.is_some() || a.perm.is_some() {
        let _ = node.setattr(&a);
    }
}

/// Kernel helper: chown a whole tree (one-time migrations at boot). Doesn't follow
/// symlinks or cross into other file systems.
pub fn chown_tree(path: &str, uid: u32, gid: u32) -> usize {
    let Ok(top) = super::ops::lookup(path) else {
        return 0;
    };
    let dev = top.dev();
    let mut n = 0;
    let mut stack = alloc::vec![top];
    while let Some(node) = stack.pop() {
        let st = node.stat();
        if st.uid != uid || st.gid != gid {
            let _ = node.setattr(&SetAttr {
                uid: Some(uid),
                gid: Some(gid),
                ..SetAttr::default()
            });
            n += 1;
        }
        if st.kind == InodeType::Dir {
            for e in node.readdir().unwrap_or_default() {
                if let Ok(c) = node.lookup(&e.name)
                    && c.dev() == dev
                    && c.kind() != InodeType::Symlink
                {
                    stack.push(c);
                }
            }
        }
    }
    n
}
